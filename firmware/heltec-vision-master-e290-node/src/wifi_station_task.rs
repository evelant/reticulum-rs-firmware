//! Opt-in Wi-Fi station owner for the additive LoRa/Wi-Fi coexistence proof.
//!
//! The actor applies one immutable durable profile selected during boot, drives
//! DHCPv4, and publishes only a passphrase-free latest-value status. It owns no
//! Reticulum sockets. Synchronous composition returns a copy of the initialized
//! network-stack handle to main; the optional border-interface task receives
//! that handle while keeping all TCP and interface-fabric capabilities in its
//! own actor.

#![cfg(all(target_arch = "xtensa", feature = "wifi-station-proof"))]

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_futures::select::{Either, select};
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Timer, with_timeout};
use esp_hal::peripherals::WIFI;
use esp_radio::wifi::{
    AuthenticationMethod, Config as WifiConfig, ConnectionError, ControllerConfig, CountryInfo,
    Interface, WifiController, WifiError, sta::StationConfig,
};
use log::{error, info, warn};
use reticulum_heltec_vision_master_e290_node::wifi_driver_metrics::{
    InstrumentedWifiDriver, WIFI_DRIVER_METRICS,
};
use reticulum_heltec_vision_master_e290_node::wifi_station_profile::{
    ASSOCIATION_TIMEOUT_SECONDS, INITIAL_RECONNECT_BACKOFF_SECONDS,
    MAXIMUM_RECONNECT_BACKOFF_SECONDS, NETWORK_STACK_RESOURCES, WIFI_AMPDU_RX_ENABLED,
    WIFI_AMPDU_TX_ENABLED, WIFI_DYNAMIC_RX_BUFFERS, WIFI_DYNAMIC_TX_BUFFERS,
    WIFI_MAX_TX_POWER_QUARTER_DBM, WIFI_RX_BA_WINDOW, WIFI_RX_QUEUE_SIZE, WIFI_STATIC_RX_BUFFERS,
    WIFI_STATIC_TX_BUFFERS, WIFI_TX_QUEUE_SIZE, WifiStationBootPlan, WifiStationBootstrap,
    WifiStationPhase, WifiStationStatus, WifiStationStatusCell,
};
use static_cell::StaticCell;

static NETWORK_RESOURCES: StaticCell<StackResources<NETWORK_STACK_RESOURCES>> = StaticCell::new();

#[cfg(reticulum_e290_ble_startup_diagnostic)]
macro_rules! wireless_diagnostic {
    ($($argument:tt)*) => {
        esp_println::println!($($argument)*)
    };
}

#[cfg(not(reticulum_e290_ble_startup_diagnostic))]
macro_rules! wireless_diagnostic {
    ($($argument:tt)*) => {
        log::info!($($argument)*)
    };
}

/// Compose and spawn the Wi-Fi controller and Embassy network runner.
///
/// A missing profile or a controller-initialization failure closes only this
/// additive station actor. BLE management and LoRa routing continue in their
/// independent owners. A returned stack handle is valid only when the runner
/// task was successfully retained by the executor.
pub fn start(
    spawner: Spawner,
    wifi: WIFI<'static>,
    random_seed: u64,
    plan: Option<WifiStationBootPlan>,
    status: &'static WifiStationStatusCell,
) -> Option<Stack<'static>> {
    let plan = match plan {
        Some(plan) => plan,
        None => {
            status.publish(WifiStationStatus::DISABLED);
            error!("e290-node stage=wifi-station status=DISABLED reason=config-unavailable");
            return None;
        }
    };
    let bootstrap = match plan {
        WifiStationBootPlan::Disabled { applied_revision } => {
            status.publish(WifiStationStatus::disabled_at(applied_revision));
            info!(
                "e290-node stage=wifi-station status=DISABLED reason=no-enabled-profile applied_revision={applied_revision}"
            );
            return None;
        }
        WifiStationBootPlan::Connect(bootstrap) => bootstrap,
    };

    let station_config = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(bootstrap.ssid())
            .with_auth_method(AuthenticationMethod::Wpa2Personal)
            .with_password(bootstrap.password().into()),
    );
    let controller_config = ControllerConfig::default()
        .with_country_info(CountryInfo::from(*b"US"))
        .with_rx_queue_size(WIFI_RX_QUEUE_SIZE)
        .with_tx_queue_size(WIFI_TX_QUEUE_SIZE)
        .with_static_rx_buf_num(WIFI_STATIC_RX_BUFFERS)
        .with_dynamic_rx_buf_num(WIFI_DYNAMIC_RX_BUFFERS)
        .with_static_tx_buf_num(WIFI_STATIC_TX_BUFFERS)
        .with_dynamic_tx_buf_num(WIFI_DYNAMIC_TX_BUFFERS)
        .with_ampdu_rx_enable(WIFI_AMPDU_RX_ENABLED)
        .with_ampdu_tx_enable(WIFI_AMPDU_TX_ENABLED)
        .with_rx_ba_win(WIFI_RX_BA_WINDOW)
        .with_initial_config(station_config);
    let station_interface = Interface::station();
    wireless_diagnostic!(
        "e290-wireless-diagnostic stage=wifi-controller status=START internal_free={} rx_queue={} tx_queue={} static_rx={} dynamic_rx={} dynamic_tx={} ampdu_rx={} ampdu_tx={} ba_window={}",
        esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into()),
        WIFI_RX_QUEUE_SIZE,
        WIFI_TX_QUEUE_SIZE,
        WIFI_STATIC_RX_BUFFERS,
        WIFI_DYNAMIC_RX_BUFFERS,
        WIFI_DYNAMIC_TX_BUFFERS,
        WIFI_AMPDU_RX_ENABLED,
        WIFI_AMPDU_TX_ENABLED,
        WIFI_RX_BA_WINDOW,
    );
    let mut controller = match WifiController::new(wifi, controller_config) {
        Ok(controller) => {
            wireless_diagnostic!(
                "e290-wireless-diagnostic stage=wifi-controller status=PASS internal_free={}",
                esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into()),
            );
            controller
        }
        Err(reason) => {
            wireless_diagnostic!(
                "e290-wireless-diagnostic stage=wifi-controller status=FAIL reason={} internal_free={}",
                wifi_error_label(&reason),
                esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into()),
            );
            publish(status, &bootstrap, WifiStationPhase::Faulted, None, None);
            error!(
                "e290-node stage=wifi-station status=DISABLED operation=initialize reason={}",
                wifi_error_label(&reason)
            );
            return None;
        }
    };
    if let Err(reason) = controller.set_max_tx_power(WIFI_MAX_TX_POWER_QUARTER_DBM) {
        publish(status, &bootstrap, WifiStationPhase::Faulted, None, None);
        error!(
            "e290-node stage=wifi-station status=DISABLED operation=set-tx-power quarter_dbm={} reason={}",
            WIFI_MAX_TX_POWER_QUARTER_DBM,
            wifi_error_label(&reason),
        );
        return None;
    }

    let network_config = embassy_net::Config::dhcpv4(Default::default());
    let resources = NETWORK_RESOURCES.init(StackResources::<NETWORK_STACK_RESOURCES>::new());
    let station_driver = InstrumentedWifiDriver::new(station_interface, &WIFI_DRIVER_METRICS);
    let (stack, runner) = embassy_net::new(station_driver, network_config, resources, random_seed);
    let task_pool_fault_status =
        WifiStationStatus::for_bootstrap(&bootstrap, WifiStationPhase::Faulted);
    let task = match run_station(controller, runner, stack, bootstrap, status) {
        Ok(task) => task,
        Err(_) => {
            status.publish(task_pool_fault_status);
            error!("e290-node stage=wifi-station status=DISABLED operation=spawn reason=task-pool");
            return None;
        }
    };
    spawner.spawn(task);

    info!(
        "e290-node stage=wifi-station status=READY security=wpa2-personal country=US tx_power_quarter_dbm={} tx_power_dbm=15 dhcp=enabled apply=boot-time-only",
        WIFI_MAX_TX_POWER_QUARTER_DBM,
    );
    Some(stack)
}

#[embassy_executor::task]
async fn run_station(
    controller: WifiController<'static>,
    runner: Runner<'static, InstrumentedWifiDriver<Interface>>,
    stack: Stack<'static>,
    bootstrap: WifiStationBootstrap,
    status: &'static WifiStationStatusCell,
) {
    join(
        run_network(runner),
        maintain_connection(controller, stack, &bootstrap, status),
    )
    .await;
}

async fn run_network(mut runner: Runner<'static, InstrumentedWifiDriver<Interface>>) -> ! {
    runner.run().await
}

async fn maintain_connection(
    mut controller: WifiController<'static>,
    stack: Stack<'static>,
    bootstrap: &WifiStationBootstrap,
    status: &WifiStationStatusCell,
) -> ! {
    let mut reconnect_backoff_seconds = INITIAL_RECONNECT_BACKOFF_SECONDS;

    loop {
        if !controller.is_connected() {
            wireless_diagnostic!(
                "e290-wireless-diagnostic stage=wifi-association status=START internal_free={}",
                esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into()),
            );
            publish(status, bootstrap, WifiStationPhase::Associating, None, None);
            match with_timeout(
                Duration::from_secs(ASSOCIATION_TIMEOUT_SECONDS),
                controller.connect_async(),
            )
            .await
            {
                Ok(Ok(_)) => {
                    wireless_diagnostic!(
                        "e290-wireless-diagnostic stage=wifi-association status=PASS rssi={:?} internal_free={}",
                        current_rssi(&controller),
                        esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into()),
                    );
                    info!("e290-node stage=wifi-station status=ASSOCIATED");
                }
                Ok(Err(reason)) => {
                    wireless_diagnostic!(
                        "e290-wireless-diagnostic stage=wifi-association status=RETRY reason={} rssi={:?}",
                        connection_error_label(&reason),
                        connection_error_rssi(&reason),
                    );
                    warn!(
                        "e290-node stage=wifi-station status=RETRY operation=associate reason={}",
                        connection_error_label(&reason)
                    );
                    backoff(
                        status,
                        bootstrap,
                        &mut reconnect_backoff_seconds,
                        connection_error_rssi(&reason),
                    )
                    .await;
                    continue;
                }
                Err(_) if controller.is_connected() => {
                    wireless_diagnostic!(
                        "e290-wireless-diagnostic stage=wifi-association status=PASS timing=after-deadline rssi={:?}",
                        current_rssi(&controller),
                    );
                    info!("e290-node stage=wifi-station status=ASSOCIATED timing=after-deadline");
                }
                Err(_) => {
                    wireless_diagnostic!(
                        "e290-wireless-diagnostic stage=wifi-association status=RETRY reason=timeout"
                    );
                    warn!(
                        "e290-node stage=wifi-station status=RETRY operation=associate reason=timeout"
                    );
                    backoff(status, bootstrap, &mut reconnect_backoff_seconds, None).await;
                    continue;
                }
            }
        }

        wireless_diagnostic!("e290-wireless-diagnostic stage=wifi-dhcp status=START");
        publish(
            status,
            bootstrap,
            WifiStationPhase::Dhcp,
            None,
            current_rssi(&controller),
        );
        match with_timeout(
            Duration::from_secs(ASSOCIATION_TIMEOUT_SECONDS),
            stack.wait_config_up(),
        )
        .await
        {
            Ok(()) => {}
            Err(_) => {
                warn!("e290-node stage=wifi-station status=RETRY operation=dhcp reason=timeout");
                backoff(
                    status,
                    bootstrap,
                    &mut reconnect_backoff_seconds,
                    current_rssi(&controller),
                )
                .await;
                continue;
            }
        }

        let Some(ipv4) = stack
            .config_v4()
            .map(|config| config.address.address().octets())
        else {
            warn!("e290-node stage=wifi-station status=RETRY operation=dhcp reason=config-lost");
            backoff(
                status,
                bootstrap,
                &mut reconnect_backoff_seconds,
                current_rssi(&controller),
            )
            .await;
            continue;
        };
        let rssi = current_rssi(&controller);
        wireless_diagnostic!(
            "e290-wireless-diagnostic stage=wifi-dhcp status=PASS ipv4={}.{}.{}.{} rssi={:?} internal_free={}",
            ipv4[0],
            ipv4[1],
            ipv4[2],
            ipv4[3],
            rssi,
            esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into()),
        );
        publish(
            status,
            bootstrap,
            WifiStationPhase::Online,
            Some(ipv4),
            rssi,
        );
        info!(
            "e290-node stage=wifi-station status=ONLINE ipv4={}.{}.{}.{}",
            ipv4[0], ipv4[1], ipv4[2], ipv4[3]
        );
        reconnect_backoff_seconds = INITIAL_RECONNECT_BACKOFF_SECONDS;

        let disconnected_rssi = match select(
            controller.wait_for_disconnect_async(),
            stack.wait_config_down(),
        )
        .await
        {
            Either::First(Ok(disconnected)) => {
                warn!(
                    "e290-node stage=wifi-station status=RETRY operation=link reason={:?}",
                    disconnected.reason
                );
                Some(disconnected.rssi)
            }
            Either::First(Err(reason)) => {
                warn!(
                    "e290-node stage=wifi-station status=RETRY operation=link reason={}",
                    wifi_error_label(&reason)
                );
                None
            }
            Either::Second(()) => {
                warn!(
                    "e290-node stage=wifi-station status=RETRY operation=dhcp reason=config-lost"
                );
                publish(
                    status,
                    bootstrap,
                    WifiStationPhase::Dhcp,
                    None,
                    current_rssi(&controller),
                );
                continue;
            }
        };
        backoff(
            status,
            bootstrap,
            &mut reconnect_backoff_seconds,
            disconnected_rssi,
        )
        .await;
    }
}

async fn backoff(
    status: &WifiStationStatusCell,
    bootstrap: &WifiStationBootstrap,
    reconnect_backoff_seconds: &mut u64,
    rssi: Option<i8>,
) {
    publish(status, bootstrap, WifiStationPhase::Backoff, None, rssi);
    Timer::after(Duration::from_secs(*reconnect_backoff_seconds)).await;
    *reconnect_backoff_seconds = reconnect_backoff_seconds
        .saturating_mul(2)
        .min(MAXIMUM_RECONNECT_BACKOFF_SECONDS);
}

fn publish(
    status: &WifiStationStatusCell,
    bootstrap: &WifiStationBootstrap,
    phase: WifiStationPhase,
    ipv4: Option<[u8; 4]>,
    rssi_dbm: Option<i8>,
) {
    let mut next = WifiStationStatus::for_bootstrap(bootstrap, phase);
    next.ipv4 = ipv4;
    next.rssi_dbm = rssi_dbm;
    status.publish(next);
}

fn current_rssi(controller: &WifiController<'_>) -> Option<i8> {
    controller
        .rssi()
        .ok()
        .and_then(|rssi| i8::try_from(rssi).ok())
}

fn connection_error_rssi(reason: &ConnectionError) -> Option<i8> {
    match reason {
        ConnectionError::Failed(info) => Some(info.rssi),
        ConnectionError::WifiError(_) => None,
        _ => None,
    }
}

fn connection_error_label(reason: &ConnectionError) -> &'static str {
    match reason {
        ConnectionError::Failed(_) => "disconnected",
        ConnectionError::WifiError(reason) => wifi_error_label(reason),
        _ => "unknown",
    }
}

fn wifi_error_label(reason: &WifiError) -> &'static str {
    match reason {
        WifiError::Unsupported => "unsupported",
        WifiError::InvalidArguments => "invalid-arguments",
        WifiError::Failed => "driver-failure",
        WifiError::OutOfMemory => "out-of-memory",
        WifiError::InvalidSsid => "invalid-ssid",
        WifiError::InvalidPassword => "invalid-password",
        WifiError::NotConnected => "not-connected",
        _ => "unknown",
    }
}
