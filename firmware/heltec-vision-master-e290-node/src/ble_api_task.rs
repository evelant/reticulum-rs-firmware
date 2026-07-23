//! Opt-in E290 BLE GATT and authenticated RDA1 device-API owner.
//!
//! This proof exposes one write-with-response RX characteristic and one
//! indication-only TX characteristic. Characteristic values are fragments of
//! the existing ordered RDA1 byte stream, not a second framing layer. The
//! board must already own an Active credential provisioned by the ordinary USB
//! profile; BLE pairing and credential initialization are intentionally absent.

#![allow(
    clippy::needless_borrows_for_generic_args,
    reason = "trouble 0.6 GATT derive expansion borrows generic characteristic values"
)]

use core::{num::NonZeroU64, str};

use ::embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_futures::{
    join::join,
    select::{Either, select},
};
// Trouble 0.6 intentionally remains on embassy-sync 0.7. Keep its macro
// expansion bound to that exact version while the product handoffs use 0.8.
use embassy_sync_07 as embassy_sync;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::{peripherals::BT, rng::Trng};
use esp_radio::ble::controller::BleConnector;
use log::{error, info, warn};
use reticulum_device_api_ble as gatt_profile;
use reticulum_device_api_framing::{DecodeEvent, StreamDecoder};
use reticulum_device_api_handoff::{BearerHandoff, DeviceApiHandoff};
use reticulum_device_api_pairing_policy::{ConnectionId, MonotonicMillis as PairingMillis};
use reticulum_device_api_session::{AuthenticatedGrant, ServerParameters};
#[cfg(reticulum_e290_ble_startup_diagnostic)]
use reticulum_heltec_vision_master_e290_node::config as product_config;
use reticulum_heltec_vision_master_e290_node::{
    ble_api_profile::{
        self as profile, IndicationGate, PreAuthenticationDeadline, PreAuthenticationDeadlineStatus,
    },
    live_pairing_handoff::BearerLivePairingHandoff,
    pairing_control_handoff::{
        LifecycleAcknowledgement, PairingControlCommand, PairingControlReplyKind, UsbPairingHandoff,
    },
    session_admission_handoff::BearerSessionAdmissionHandoff,
    usb_authenticated_session::{
        UsbAuthenticatedSession, UsbAuthenticatedSessionPhase, UsbSessionRxDisposition,
    },
};
use static_cell::StaticCell;
use trouble_host::{
    att::{AttCfm, AttClient},
    prelude::*,
    types::gatt_traits::AsGatt,
};

static BLE_RESOURCES: StaticCell<
    HostResources<DefaultPacketPool, { profile::CONNECTIONS_MAX }, { profile::L2CAP_CHANNELS_MAX }>,
> = StaticCell::new();
static BLE_LOCAL_NAME: StaticCell<[u8; gatt_profile::LOCAL_NAME_BYTES]> = StaticCell::new();
static BLE_SERVER: StaticCell<Server<'static>> = StaticCell::new();

const SERVICE_UUIDS: [[u8; 16]; 1] = [gatt_profile::SERVICE_UUID_LE];

#[cfg(reticulum_e290_ble_startup_diagnostic)]
fn diagnostic_heap_marker(point: &'static str) {
    let heap = esp_alloc::HEAP.stats();
    let region0 = heap.region_stats[0].as_ref();
    let region_values = |region: Option<&esp_alloc::RegionStats>| {
        region.map_or((false, 0, 0, 0), |region| {
            (true, region.size, region.used, region.free)
        })
    };
    let (region0_present, region0_bytes, region0_used, region0_free) = region_values(region0);
    let (internal_bytes, internal_used, internal_free) = heap
        .region_stats
        .iter()
        .flatten()
        .filter(|region| {
            region
                .capabilities
                .contains(esp_alloc::MemoryCapability::Internal)
        })
        .fold((0_usize, 0_usize, 0_usize), |totals, region| {
            (
                totals.0.saturating_add(region.size),
                totals.1.saturating_add(region.used),
                totals.2.saturating_add(region.free),
            )
        });
    esp_println::println!(
        "e290-ble-startup-diagnostic stage=heap point={} configured_internal_bytes={} total_heap_bytes={} total_heap_used_bytes={} internal_bytes={} internal_used_bytes={} internal_free_bytes={} region0_present={} region0_bytes={} region0_used_bytes={} region0_free_bytes={}",
        point,
        product_config::INTERNAL_HEAP_BYTES,
        heap.size,
        heap.current_usage,
        internal_bytes,
        internal_used,
        internal_free,
        region0_present,
        region0_bytes,
        region0_used,
        region0_free,
    );
}

#[derive(Clone, Copy, Default)]
struct GattFragment {
    bytes: [u8; gatt_profile::INITIAL_ATT_VALUE_BYTES],
    len: usize,
}

impl GattFragment {
    fn copy_from(source: &[u8]) -> Option<Self> {
        if source.is_empty() || source.len() > gatt_profile::INITIAL_ATT_VALUE_BYTES {
            return None;
        }
        let mut fragment = Self::default();
        fragment.bytes[..source.len()].copy_from_slice(source);
        fragment.len = source.len();
        Some(fragment)
    }
}

impl AsGatt for GattFragment {
    const MIN_SIZE: usize = 0;
    const MAX_SIZE: usize = gatt_profile::INITIAL_ATT_VALUE_BYTES;

    fn as_gatt(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[gatt_server]
struct Server {
    api: ApiService,
}

#[gatt_service(uuid = gatt_profile::SERVICE_UUID_U128)]
struct ApiService {
    /// Ordered phone-to-device RDA1 bytes.
    #[characteristic(uuid = gatt_profile::RX_UUID_U128, write)]
    rx: GattFragment,
    /// Ordered device-to-phone RDA1 bytes.
    #[characteristic(uuid = gatt_profile::TX_UUID_U128, indicate)]
    tx: GattFragment,
}

/// Sole bearer-side handoffs moved into the BLE API task.
pub(crate) struct BleHandoffs {
    pairing: UsbPairingHandoff<CriticalSectionRawMutex>,
    _live_pairing: BearerLivePairingHandoff<CriticalSectionRawMutex>,
    admission: BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
}

impl BleHandoffs {
    pub(crate) const fn new(
        pairing: UsbPairingHandoff<CriticalSectionRawMutex>,
        live_pairing: BearerLivePairingHandoff<CriticalSectionRawMutex>,
        admission: BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
        authenticated_api: BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    ) -> Self {
        Self {
            pairing,
            _live_pairing: live_pairing,
            admission,
            authenticated_api,
        }
    }
}

/// Initialize and retain the complete opt-in BLE bearer.
///
/// Initialization occurs after the autonomous LoRa and node tasks have been
/// constructed. Returned configuration errors and handled host/GATT failures
/// therefore close this API bearer without stopping Reticulum routing. The
/// pinned esp-radio controller still asserts on some scheduler, allocation and
/// controller-init failures; making that boundary fallible is tracked as a
/// known defect and must not be described as isolated yet.
#[embassy_executor::task]
pub async fn run(
    bluetooth: BT<'static>,
    base_mac: [u8; 6],
    handoffs: BleHandoffs,
    session_parameters: ServerParameters,
    session_rng: Trng,
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    diagnostic_usb_serial_jtag_owner: crate::DiagnosticUsbSerialJtagOwner,
) {
    let controller_config = esp_radio::ble::Config::default()
        // esp-radio 0.18 writes this value directly to Espressif's
        // `ble_max_act`: it counts the advertiser and ACL link as distinct
        // controller activities, despite the API's connection-shaped name.
        .with_max_connections(profile::CONTROLLER_ACTIVITY_MAX as u8)
        .with_scan(false);
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    {
        const _: () = assert!(product_config::INTERNAL_HEAP_BYTES == 73_728);
        diagnostic_heap_marker("before-ble-controller-init");
        esp_println::println!(
            "e290-ble-startup-diagnostic stage=ble-controller-init ENTER internal_heap_configured_bytes=73728 controller_activity_max=2 application_connection_max=1"
        );
    }
    let connector = match BleConnector::new(bluetooth, controller_config) {
        Ok(connector) => {
            #[cfg(reticulum_e290_ble_startup_diagnostic)]
            esp_println::println!(
                "e290-ble-startup-diagnostic stage=ble-controller-init RETURNED OK"
            );
            #[cfg(reticulum_e290_ble_startup_diagnostic)]
            diagnostic_heap_marker("after-ble-controller-init");
            connector
        }
        Err(reason) => {
            #[cfg(reticulum_e290_ble_startup_diagnostic)]
            esp_println::println!(
                "e290-ble-startup-diagnostic stage=ble-controller-init RETURNED ERR reason={reason:?}"
            );
            error!("e290-node stage=ble-api status=DISABLED reason=controller-init-{reason:?}");
            core::future::pending().await
        }
    };
    let controller: ExternalController<_, 1> = ExternalController::new(connector);
    let host_result = run_host(
        controller,
        base_mac,
        handoffs,
        session_parameters,
        session_rng,
    )
    .await;
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    #[allow(
        clippy::drop_non_drop,
        reason = "the peripheral token is intentionally retained across the complete async host lifetime"
    )]
    core::mem::drop(diagnostic_usb_serial_jtag_owner);
    host_result
}

async fn run_host<C>(
    controller: C,
    base_mac: [u8; 6],
    handoffs: BleHandoffs,
    session_parameters: ServerParameters,
    session_rng: Trng,
) where
    C: Controller,
{
    let resources = BLE_RESOURCES.init(HostResources::new());
    let address = Address::random(profile::static_random_address(base_mac));
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    esp_println::println!("e290-ble-startup-diagnostic stage=trouble-host-new ENTER");
    let stack = trouble_host::new(controller, resources).set_random_address(address);
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    esp_println::println!("e290-ble-startup-diagnostic stage=trouble-host-new RETURNED OK");
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    esp_println::println!("e290-ble-startup-diagnostic stage=trouble-host-build ENTER");
    let Host {
        mut peripheral,
        runner,
        ..
    } = stack.build();
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    esp_println::println!("e290-ble-startup-diagnostic stage=trouble-host-build RETURNED OK");

    let local_name_bytes = BLE_LOCAL_NAME.init(gatt_profile::local_name(base_mac));
    let local_name = match str::from_utf8(local_name_bytes) {
        Ok(name) => name,
        Err(_) => {
            error!("e290-node stage=ble-api status=DISABLED reason=local-name-encoding");
            core::future::pending().await
        }
    };
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    esp_println::println!("e290-ble-startup-diagnostic stage=gatt-server-construction ENTER");
    let server = match Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: local_name,
        appearance: &appearance::computer::GENERIC_COMPUTER,
    })) {
        Ok(server) => {
            let server = BLE_SERVER.init(server);
            #[cfg(reticulum_e290_ble_startup_diagnostic)]
            esp_println::println!(
                "e290-ble-startup-diagnostic stage=gatt-server-construction RETURNED OK"
            );
            server
        }
        Err(reason) => {
            #[cfg(reticulum_e290_ble_startup_diagnostic)]
            esp_println::println!(
                "e290-ble-startup-diagnostic stage=gatt-server-construction RETURNED ERR reason={reason}"
            );
            error!("e290-node stage=ble-api status=DISABLED reason=gatt-server-{reason}");
            core::future::pending().await
        }
    };

    info!(
        "e290-node stage=ble-api status=READY address={} local_name={} service={} profile={}.{} central_limit=1 att_fragment_bytes={} tx=indicate rx=write-with-response pairing=usb-profile-required",
        address,
        local_name,
        gatt_profile::SERVICE_UUID,
        gatt_profile::GATT_PROFILE_MAJOR,
        gatt_profile::GATT_PROFILE_MINOR,
        gatt_profile::INITIAL_ATT_VALUE_BYTES,
    );

    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    esp_println::println!("e290-ble-startup-diagnostic stage=host-join ENTER");
    join(
        run_controller(runner),
        serve_advertising(
            &mut peripheral,
            server,
            local_name.as_bytes(),
            handoffs,
            session_parameters,
            session_rng,
        ),
    )
    .await;
}

async fn run_controller<C: Controller, P: PacketPool>(mut runner: Runner<'_, C, P>) {
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    esp_println::println!("e290-ble-startup-diagnostic stage=controller-runner START");
    loop {
        #[cfg(reticulum_e290_ble_startup_diagnostic)]
        esp_println::println!("e290-ble-startup-diagnostic stage=controller-runner-run ENTER");
        match runner.run().await {
            Ok(()) => {
                #[cfg(reticulum_e290_ble_startup_diagnostic)]
                esp_println::println!(
                    "e290-ble-startup-diagnostic stage=controller-runner-run RETURNED OK"
                );
            }
            Err(reason) => {
                #[cfg(reticulum_e290_ble_startup_diagnostic)]
                esp_println::println!(
                    "e290-ble-startup-diagnostic stage=controller-runner-run RETURNED ERR reason={reason:?}"
                );
                error!("e290-node stage=ble-controller status=RETRY reason={reason:?}");
                Timer::after_millis(100).await;
            }
        }
    }
}

async fn serve_advertising<C: Controller>(
    peripheral: &mut Peripheral<'_, C, DefaultPacketPool>,
    server: &'static Server<'static>,
    local_name: &'static [u8],
    handoffs: BleHandoffs,
    session_parameters: ServerParameters,
    mut session_rng: Trng,
) {
    let BleHandoffs {
        pairing: mut pairing_handoff,
        _live_pairing,
        admission: mut session_admission,
        mut authenticated_api,
    } = handoffs;
    let mut authenticated_session = UsbAuthenticatedSession::new(session_parameters);
    let mut next_connection = NonZeroU64::MIN;

    loop {
        drain_stale_replies(
            &mut authenticated_session,
            &mut session_admission,
            &mut authenticated_api,
            &mut session_rng,
        );
        let connection = ConnectionId::new(next_connection.get())
            .expect("the nonzero BLE connection counter is valid");
        let Some(successor) = next_connection
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
        else {
            error!("e290-node stage=ble-api status=DISABLED reason=connection-epoch-exhausted");
            core::future::pending().await
        };
        next_connection = successor;

        #[cfg(reticulum_e290_ble_startup_diagnostic)]
        esp_println::println!(
            "e290-ble-startup-diagnostic stage=advertise-wrapper ENTER connection={}",
            connection.get()
        );
        let gatt_connection = match advertise(peripheral, server, local_name).await {
            Ok(gatt_connection) => {
                #[cfg(reticulum_e290_ble_startup_diagnostic)]
                esp_println::println!(
                    "e290-ble-startup-diagnostic stage=advertise-wrapper RETURNED OK connection={}",
                    connection.get()
                );
                gatt_connection
            }
            Err(reason) => {
                #[cfg(reticulum_e290_ble_startup_diagnostic)]
                esp_println::println!(
                    "e290-ble-startup-diagnostic stage=advertise-wrapper RETURNED ERR connection={} reason={reason:?}",
                    connection.get()
                );
                warn!("e290-node stage=ble-api status=ADVERTISE-RETRY reason={reason:?}");
                Timer::after_millis(100).await;
                continue;
            }
        };

        let connection_outcome = serve_connection(
            server,
            &gatt_connection,
            connection,
            &mut pairing_handoff,
            &mut authenticated_session,
            &mut session_admission,
            &mut authenticated_api,
            &mut session_rng,
        )
        .await;
        // Release logical session owners before a fail-closed controller drain
        // which intentionally has no success timeout.
        authenticated_session.reset();
        drain_connection(
            &gatt_connection,
            connection,
            connection_outcome.disconnected_event_observed,
        )
        .await;
        // Trouble will not reuse the sole HostResources connection slot until
        // its state is Disconnected and the final Connection reference is gone.
        // Keep this drop visibly ahead of every path to the next advertiser.
        drop(gatt_connection);
        info!(
            "e290-node stage=ble-api status=LINK-RESOURCES-RELEASED connection={}",
            connection.get()
        );
        #[cfg(reticulum_e290_ble_startup_diagnostic)]
        esp_println::println!(
            "e290-ble-startup-diagnostic stage=disconnect-drain status=RESOURCES-RELEASED connection={}",
            connection.get()
        );

        if connection_outcome.lifecycle_started {
            if !announce_lifecycle(
                &mut pairing_handoff,
                PairingControlCommand::Disconnected {
                    at: pairing_time(),
                    connection,
                },
                connection,
                LifecycleAcknowledgement::Disconnected,
            )
            .await
            {
                error!(
                    "e290-node stage=ble-api status=DISABLED reason=disconnected-lifecycle-mismatch connection={}",
                    connection.get()
                );
                core::future::pending().await
            }
            info!(
                "e290-node stage=ble-api status=DISCONNECTED connection={}",
                connection.get()
            );
        }
    }
}

async fn drain_connection<P: PacketPool>(
    connection: &GattConnection<'_, '_, P>,
    connection_id: ConnectionId,
    disconnected_event_observed: bool,
) {
    let raw = connection.raw();

    // `ConnectionManager::disconnected` stores Disconnected before publishing
    // the event. If serve_connection consumed that exact event, waiting for a
    // second copy would stall forever.
    if disconnected_event_observed {
        if raw.is_connected() {
            error!(
                "e290-node stage=ble-api status=DISCONNECT-DRAIN-STALLED reason=observed-event-but-manager-connected connection={}",
                connection_id.get()
            );
            core::future::pending::<()>().await;
        }
        info!(
            "e290-node stage=ble-api status=DISCONNECT-DRAINED source=serve-event connection={}",
            connection_id.get()
        );
        #[cfg(reticulum_e290_ble_startup_diagnostic)]
        esp_println::println!(
            "e290-ble-startup-diagnostic stage=disconnect-drain status=COMPLETE source=serve-event connection={}",
            connection_id.get()
        );
        return;
    }

    // This call is idempotent when the controller has already disconnected.
    // Do not use the subsequent `is_connected() == false` as completion:
    // Trouble 0.6 reports false for its intermediate DisconnectRequest state.
    let connected_before_request = raw.is_connected();
    raw.disconnect();
    info!(
        "e290-node stage=ble-api status=DISCONNECT-DRAINING connected_before_request={} connection={}",
        connected_before_request,
        connection_id.get()
    );
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    esp_println::println!(
        "e290-ble-startup-diagnostic stage=disconnect-drain status=WAITING connected_before_request={} connection={}",
        connected_before_request,
        connection_id.get()
    );

    let drain_started_at = Instant::now();
    let mut last_prolonged_log_at = drain_started_at;
    loop {
        match select(
            raw.next(),
            Timer::after_millis(profile::DISCONNECT_DRAIN_RECHECK_INTERVAL_MS),
        )
        .await
        {
            Either::First(ConnectionEvent::Disconnected { reason }) => {
                info!(
                    "e290-node stage=ble-api status=DISCONNECT-DRAINED source=raw-event reason={reason:?} elapsed_ms={} connection={}",
                    drain_started_at.elapsed().as_millis(),
                    connection_id.get()
                );
                #[cfg(reticulum_e290_ble_startup_diagnostic)]
                esp_println::println!(
                    "e290-ble-startup-diagnostic stage=disconnect-drain status=COMPLETE source=raw-event reason={reason:?} elapsed_ms={} connection={}",
                    drain_started_at.elapsed().as_millis(),
                    connection_id.get()
                );
                return;
            }
            Either::First(_) => {}
            Either::Second(()) => {
                // Trouble's public predicate collapses DisconnectRequest and
                // Disconnected into false, so this short recheck is diagnostic
                // only. The raw Disconnected event remains the success gate.
                let manager_reports_connected = raw.is_connected();
                if last_prolonged_log_at.elapsed()
                    >= Duration::from_millis(profile::DISCONNECT_DRAIN_PROLONGED_LOG_MS)
                {
                    warn!(
                        "e290-node stage=ble-api status=DISCONNECT-DRAINING elapsed_ms={} manager_reports_connected={} completion=awaiting-disconnected-event connection={}",
                        drain_started_at.elapsed().as_millis(),
                        manager_reports_connected,
                        connection_id.get()
                    );
                    #[cfg(reticulum_e290_ble_startup_diagnostic)]
                    esp_println::println!(
                        "e290-ble-startup-diagnostic stage=disconnect-drain status=PROLONGED elapsed_ms={} manager_reports_connected={} completion=awaiting-disconnected-event connection={}",
                        drain_started_at.elapsed().as_millis(),
                        manager_reports_connected,
                        connection_id.get()
                    );
                    last_prolonged_log_at = Instant::now();
                }
            }
        }
    }
}

async fn advertise<'stack, 'server, C: Controller>(
    peripheral: &mut Peripheral<'stack, C, DefaultPacketPool>,
    server: &'server Server<'static>,
    local_name: &[u8],
) -> Result<GattConnection<'stack, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    let mut advertising_data = [0_u8; 31];
    let advertising_len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::ServiceUuids128(&SERVICE_UUIDS),
        ],
        &mut advertising_data,
    )?;
    let mut scan_data = [0_u8; 31];
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::CompleteLocalName(local_name)],
        &mut scan_data,
    )?;
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    esp_println::println!(
        "e290-ble-startup-diagnostic stage=peripheral-advertise ENTER adv_bytes={} scan_bytes={}",
        advertising_len,
        scan_len,
    );
    let advertiser = match peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &advertising_data[..advertising_len],
                scan_data: &scan_data[..scan_len],
            },
        )
        .await
    {
        Ok(advertiser) => {
            #[cfg(reticulum_e290_ble_startup_diagnostic)]
            esp_println::println!(
                "e290-ble-startup-diagnostic stage=peripheral-advertise RETURNED OK"
            );
            #[cfg(reticulum_e290_ble_startup_diagnostic)]
            diagnostic_heap_marker("after-peripheral-advertise-ok");
            advertiser
        }
        Err(reason) => {
            #[cfg(reticulum_e290_ble_startup_diagnostic)]
            esp_println::println!(
                "e290-ble-startup-diagnostic stage=peripheral-advertise RETURNED ERR reason={reason:?}"
            );
            #[cfg(reticulum_e290_ble_startup_diagnostic)]
            diagnostic_heap_marker("after-peripheral-advertise-err");
            return Err(reason);
        }
    };
    info!("e290-node stage=ble-api status=ADVERTISING");
    let connection = advertiser.accept().await?.with_attribute_server(server)?;
    info!("e290-node stage=ble-api status=LINK-CONNECTED session=pending-tx-cccd");
    Ok(connection)
}

struct ServeConnectionOutcome {
    lifecycle_started: bool,
    disconnected_event_observed: bool,
}

#[allow(
    clippy::too_many_arguments,
    reason = "one BLE link retains each exact session and handoff owner explicitly"
)]
async fn serve_connection<P: PacketPool>(
    server: &Server<'_>,
    connection: &GattConnection<'_, '_, P>,
    connection_id: ConnectionId,
    pairing_handoff: &mut UsbPairingHandoff<CriticalSectionRawMutex>,
    session: &mut UsbAuthenticatedSession,
    admission: &mut BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: &mut BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    session_rng: &mut Trng,
) -> ServeConnectionOutcome {
    let rx = server.api.rx;
    let tx = server.api.tx;
    let tx_cccd = tx
        .cccd_handle
        .expect("the indication characteristic always owns a CCCD");
    let mut decoder = StreamDecoder::new();
    let mut indication = IndicationGate::new();
    let linked_at = Instant::now();
    let mut indication_sent_at: Option<Instant> = None;
    let mut pre_authentication = PreAuthenticationDeadline::new();
    let mut pre_authentication_started_at: Option<Instant> = None;
    let mut lifecycle_started = false;
    let mut disconnected_event_observed = false;

    'connection: loop {
        let mut fault = false;

        while let Some(reply) = admission.try_receive_reply() {
            if session.accept_admission_reply(reply, session_rng).is_err() {
                fault = true;
                break;
            }
        }
        while !fault {
            let Some(reply) = authenticated_api.replies().try_receive() else {
                break;
            };
            if let Err(reason) = session.accept_node_reply(reply) {
                drop(reason.into_reply());
                fault = true;
            }
        }
        if fault {
            warn!(
                "e290-node stage=ble-api status=LINK-RESET reason=session-reply-fault connection={}",
                connection_id.get()
            );
            break;
        }

        if let Some(sent_at) = indication_sent_at
            && sent_at.elapsed() >= Duration::from_millis(profile::INDICATION_CONFIRM_TIMEOUT_MS)
        {
            warn!(
                "e290-node stage=ble-api status=LINK-RESET reason=indication-confirm-timeout connection={}",
                connection_id.get()
            );
            break;
        }
        if !lifecycle_started
            && linked_at.elapsed() >= Duration::from_millis(profile::CCCD_SUBSCRIBE_TIMEOUT_MS)
        {
            warn!(
                "e290-node stage=ble-api status=LINK-RESET reason=tx-cccd-timeout connection={}",
                connection_id.get()
            );
            break;
        }
        if let Some(started_at) = pre_authentication_started_at {
            let phase = session.phase();
            if pre_authentication.poll(started_at.elapsed().as_millis(), phase)
                == PreAuthenticationDeadlineStatus::Expired
            {
                warn!(
                    "e290-node stage=ble-api status=LINK-RESET reason=pre-authentication-timeout phase={phase:?} connection={}",
                    connection_id.get()
                );
                break;
            }
        }

        let _ = session.try_send_admission_command(admission);
        let _ = session.try_send_request(authenticated_api.requests());
        if matches!(
            session.phase(),
            UsbAuthenticatedSessionPhase::TerminatedUntilReset
        ) {
            warn!(
                "e290-node stage=ble-api status=LINK-RESET reason=session-terminated connection={}",
                connection_id.get()
            );
            break;
        }

        if lifecycle_started && !indication.is_pending() && session.tx_kind().is_some() {
            let Some(chunk) = session.next_tx_chunk(gatt_profile::INITIAL_ATT_VALUE_BYTES) else {
                warn!(
                    "e290-node stage=ble-api status=LINK-RESET reason=missing-session-tx-owner connection={}",
                    connection_id.get()
                );
                break;
            };
            let Some(fragment) = GattFragment::copy_from(chunk) else {
                warn!(
                    "e290-node stage=ble-api status=LINK-RESET reason=invalid-session-tx-fragment connection={}",
                    connection_id.get()
                );
                break;
            };
            if indication.arm(chunk.len()).is_err() {
                warn!(
                    "e290-node stage=ble-api status=LINK-RESET reason=ambiguous-indication-owner connection={}",
                    connection_id.get()
                );
                break;
            }
            if tx.indicate(connection, &fragment).await.is_err() {
                warn!(
                    "e290-node stage=ble-api status=LINK-RESET reason=indication-send-fault connection={}",
                    connection_id.get()
                );
                break;
            }
            indication_sent_at = Some(Instant::now());
        }

        match select(
            connection.next(),
            Timer::after_millis(profile::API_POLL_INTERVAL_MS),
        )
        .await
        {
            Either::Second(()) => {}
            Either::First(GattConnectionEvent::Disconnected { reason }) => {
                disconnected_event_observed = true;
                info!(
                    "e290-node stage=ble-api status=LINK-DROPPED reason={reason:?} connection={}",
                    connection_id.get()
                );
                break;
            }
            Either::First(GattConnectionEvent::Gatt { event }) => match event {
                GattEvent::Write(write) if write.handle() == tx_cccd => {
                    let enabled = write.data() == [0x02, 0x00];
                    match write.accept() {
                        Ok(reply) => reply.send().await,
                        Err(reason) => {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=cccd-reply-{reason:?} connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                    }
                    if !enabled {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=tx-indications-disabled connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                    if !lifecycle_started {
                        if !announce_lifecycle(
                            pairing_handoff,
                            PairingControlCommand::Connected {
                                at: pairing_time(),
                                connection: connection_id,
                            },
                            connection_id,
                            LifecycleAcknowledgement::Connected,
                        )
                        .await
                        {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=connected-lifecycle-mismatch connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                        lifecycle_started = true;
                        if !session.begin_connection(connection_id) {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=session-owner-not-disconnected connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                        pre_authentication = PreAuthenticationDeadline::new();
                        pre_authentication_started_at = Some(Instant::now());
                        info!(
                            "e290-node stage=ble-api status=SESSION-READY connection={} cccd=indicate pre_authentication_timeout_ms={}",
                            connection_id.get(),
                            profile::PRE_AUTHENTICATION_TIMEOUT_MS,
                        );
                    }
                }
                GattEvent::Write(write) if write.handle() == rx.handle => {
                    let data = write.data();
                    let mut bytes = [0_u8; gatt_profile::INITIAL_ATT_VALUE_BYTES];
                    let valid = lifecycle_started
                        && !data.is_empty()
                        && data.len() <= gatt_profile::INITIAL_ATT_VALUE_BYTES;
                    if valid {
                        bytes[..data.len()].copy_from_slice(data);
                    }
                    let data_len = data.len();
                    let reply = if valid {
                        write.accept()
                    } else if lifecycle_started {
                        write.reject(AttErrorCode::INVALID_ATTRIBUTE_VALUE_LENGTH)
                    } else {
                        write.reject(AttErrorCode::CCCD_IMPROPERLY_CONFIGURED)
                    };
                    match reply {
                        Ok(reply) => reply.send().await,
                        Err(reason) => {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=rx-reply-{reason:?} connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                    }
                    if !valid {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=invalid-rx-fragment connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                    for byte in &bytes[..data_len] {
                        let decode = decoder.push(*byte);
                        if matches!(decode, DecodeEvent::Pending) {
                            continue;
                        }
                        let disposition = session.accept_decode_event(decode, pairing_time());
                        if !matches!(
                            disposition,
                            Ok(UsbSessionRxDisposition::Pending)
                                | Ok(UsbSessionRxDisposition::ClientHelloAccepted)
                                | Ok(UsbSessionRxDisposition::ClientHelloDroppedBusy)
                                | Ok(UsbSessionRxDisposition::SessionEstablished)
                                | Ok(UsbSessionRxDisposition::RequestAuthenticated)
                        ) {
                            fault = true;
                            break;
                        }
                        if matches!(disposition, Ok(UsbSessionRxDisposition::SessionEstablished)) {
                            let Some(started_at) = pre_authentication_started_at else {
                                warn!(
                                    "e290-node stage=ble-api status=LINK-RESET reason=missing-pre-authentication-owner connection={}",
                                    connection_id.get()
                                );
                                break 'connection;
                            };
                            let phase = session.phase();
                            if pre_authentication.poll(started_at.elapsed().as_millis(), phase)
                                == PreAuthenticationDeadlineStatus::Expired
                            {
                                warn!(
                                    "e290-node stage=ble-api status=LINK-RESET reason=pre-authentication-timeout phase={phase:?} connection={}",
                                    connection_id.get()
                                );
                                break 'connection;
                            }
                        }
                    }
                    if fault {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=session-rx-fault connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                }
                GattEvent::Other(other) => {
                    let confirmation = matches!(
                        other.payload().incoming(),
                        AttClient::Confirmation(AttCfm::ConfirmIndication)
                    );
                    match other.accept() {
                        Ok(reply) => reply.send().await,
                        Err(reason) => {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=gatt-other-reply-{reason:?} connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                    }
                    if confirmation {
                        let Ok(acknowledged) = indication.confirm() else {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=unexpected-indication-confirmation connection={}",
                                connection_id.get()
                            );
                            break;
                        };
                        indication_sent_at = None;
                        if session.advance_tx(acknowledged).is_err() {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=session-tx-advance-fault connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                    }
                }
                GattEvent::Read(read) => match read.accept() {
                    Ok(reply) => reply.send().await,
                    Err(reason) => {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=gatt-read-reply-{reason:?} connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                },
                GattEvent::NotAllowed(not_allowed) => match not_allowed.accept() {
                    Ok(reply) => reply.send().await,
                    Err(reason) => {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=gatt-reject-reply-{reason:?} connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                },
                GattEvent::Write(write) => {
                    match write.reject(AttErrorCode::WRITE_REQUEST_REJECTED) {
                        Ok(reply) => reply.send().await,
                        Err(reason) => {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=unexpected-write-reply-{reason:?} connection={}",
                                connection_id.get()
                            );
                        }
                    }
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=unexpected-write-handle connection={}",
                        connection_id.get()
                    );
                    break;
                }
            },
            Either::First(_) => {}
        }
    }

    indication.reset();
    ServeConnectionOutcome {
        lifecycle_started,
        disconnected_event_observed,
    }
}

fn drain_stale_replies(
    session: &mut UsbAuthenticatedSession,
    admission: &mut BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: &mut BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    session_rng: &mut Trng,
) {
    while let Some(reply) = admission.try_receive_reply() {
        let _ = session.accept_admission_reply(reply, session_rng);
    }
    while let Some(reply) = authenticated_api.replies().try_receive() {
        if let Err(fault) = session.accept_node_reply(reply) {
            drop(fault.into_reply());
        }
    }
}

async fn announce_lifecycle(
    handoff: &mut UsbPairingHandoff<CriticalSectionRawMutex>,
    command: PairingControlCommand,
    connection: ConnectionId,
    expected: LifecycleAcknowledgement,
) -> bool {
    let mut pending = Some(command);
    loop {
        if let Some(command) = pending.take()
            && let Err(pressure) = handoff.try_send_command(command)
        {
            pending = Some(pressure.into_inner());
        }
        if pending.is_none() {
            while let Some(reply) = handoff.try_receive_reply() {
                if reply.connection() != connection {
                    continue;
                }
                return matches!(
                    reply.into_kind(),
                    PairingControlReplyKind::Lifecycle(observed) if observed == expected
                );
            }
        }
        Timer::after_millis(profile::API_POLL_INTERVAL_MS).await;
    }
}

fn pairing_time() -> PairingMillis {
    PairingMillis::new(Instant::now().as_millis())
}

// Keep the complete handoff storage type in this module's graph audit. Only
// the split bearer role crosses into this task.
const _: usize =
    core::mem::size_of::<DeviceApiHandoff<CriticalSectionRawMutex, AuthenticatedGrant>>();
