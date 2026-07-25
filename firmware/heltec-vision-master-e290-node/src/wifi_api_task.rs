//! Opt-in E290 SoftAP and authenticated raw-TCP device-API owner.
//!
//! This proof profile reuses the existing single-flight RDA1 session machine
//! with an explicitly Wi-Fi-bound suite. It does not expose initialization or
//! live pairing: the board must already own an Active credential provisioned
//! by the ordinary USB profile. Only one TCP connection is accepted at a time.
//! The bearer is a local administration edge, not a Reticulum interface.

use core::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    num::NonZeroU64,
};

use edge_dhcp::{
    io::{self, DEFAULT_SERVER_PORT},
    server::{Server, ServerOptions},
};
use edge_nal::UdpBind;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_executor::Spawner;
use embassy_net::{
    IpListenEndpoint, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4, tcp::TcpSocket,
};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::{peripherals::WIFI, rng::Trng};
use esp_radio::wifi::{
    AuthenticationMethod, Config as WifiConfig, ControllerConfig, Interface, WifiController,
    WifiError, ap::AccessPointConfig,
};
use log::{error, info, warn};
use reticulum_device_api_framing::{DecodeEvent, StreamDecoder};
use reticulum_device_api_handoff::{BearerHandoff, DeviceApiHandoff};
use reticulum_device_api_pairing_policy::{ConnectionId, MonotonicMillis as PairingMillis};
use reticulum_device_api_session::{AuthenticatedGrant, ServerParameters};
use reticulum_heltec_vision_master_e290_node::{
    live_pairing_handoff::BearerLivePairingHandoff,
    pairing_control_handoff::{
        LifecycleAcknowledgement, PairingControlCommand, PairingControlReplyKind, UsbPairingHandoff,
    },
    session_admission_handoff::BearerSessionAdmissionHandoff,
    usb_authenticated_session::{
        UsbAuthenticatedSession, UsbAuthenticatedSessionPhase, UsbSessionRxDisposition,
    },
    wifi_api_profile as profile,
};
use static_cell::StaticCell;

static NETWORK_RESOURCES: StaticCell<StackResources<{ profile::NETWORK_STACK_RESOURCES }>> =
    StaticCell::new();

/// Sole bearer-side handoffs moved into the Wi-Fi API task.
pub(crate) struct WifiHandoffs {
    pairing: UsbPairingHandoff<CriticalSectionRawMutex>,
    _live_pairing: BearerLivePairingHandoff<CriticalSectionRawMutex>,
    admission: BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
}

impl WifiHandoffs {
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

/// Concrete objects produced after the radio driver and IPv4 stack initialize.
pub(crate) struct WifiComposition {
    controller: WifiController<'static>,
    stack: Stack<'static>,
    runner: Runner<'static, Interface<'static>>,
}

impl WifiComposition {
    pub(crate) fn into_parts(
        self,
    ) -> (
        WifiController<'static>,
        Stack<'static>,
        Runner<'static, Interface<'static>>,
    ) {
        (self.controller, self.stack, self.runner)
    }
}

/// Initialize the exact Espressif SoftAP and Embassy static-IPv4 boundary.
pub(crate) fn compose(
    wifi: WIFI<'static>,
    base_mac: [u8; 6],
    random_seed: u64,
) -> Result<WifiComposition, WifiError> {
    let ssid = profile::softap_ssid(base_mac);
    let access_point = AccessPointConfig::default()
        .with_ssid(ssid.as_slice())
        .with_channel(profile::SOFTAP_CHANNEL)
        .with_auth_method(AuthenticationMethod::Wpa2Personal)
        .with_password(profile::SOFTAP_DEVELOPMENT_PASSPHRASE.into())
        .with_max_connections(profile::SOFTAP_MAX_STATIONS);
    let (controller, interfaces) = esp_radio::wifi::new(
        wifi,
        ControllerConfig::default().with_initial_config(WifiConfig::AccessPoint(access_point)),
    )?;

    let gateway = Ipv4Addr::from(profile::GATEWAY_IPV4);
    let network = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(gateway, profile::GATEWAY_PREFIX_LEN),
        gateway: Some(gateway),
        dns_servers: Default::default(),
    });
    let resources =
        NETWORK_RESOURCES.init(StackResources::<{ profile::NETWORK_STACK_RESOURCES }>::new());
    let (stack, runner) =
        embassy_net::new(interfaces.access_point, network, resources, random_seed);

    Ok(WifiComposition {
        controller,
        stack,
        runner,
    })
}

/// Initialize and retain the complete opt-in Wi-Fi bearer.
///
/// Initialization occurs in this detached owner after the LoRa and node tasks
/// are already spawned. A Wi-Fi-only failure therefore closes this API bearer
/// without stopping autonomous Reticulum routing.
#[embassy_executor::task]
pub async fn run(
    spawner: Spawner,
    wifi: WIFI<'static>,
    base_mac: [u8; 6],
    random_seed: u64,
    handoffs: WifiHandoffs,
    session_parameters: ServerParameters,
    session_rng: Trng,
    alpha_usb_serial_jtag_owner: crate::AlphaUsbSerialJtagOwner,
) {
    let composition = match compose(wifi, base_mac, random_seed) {
        Ok(composition) => composition,
        Err(reason) => {
            error!("e290-node stage=wifi-api status=DISABLED reason={reason:?}");
            core::future::pending().await
        }
    };
    let (controller, stack, runner) = composition.into_parts();
    let network_task = match run_network(runner) {
        Ok(task) => task,
        Err(_) => {
            error!("e290-node stage=wifi-api status=DISABLED reason=network-task-pool");
            core::future::pending().await
        }
    };
    let dhcp_task = match run_dhcp(stack) {
        Ok(task) => task,
        Err(_) => {
            error!("e290-node stage=wifi-api status=DISABLED reason=dhcp-task-pool");
            core::future::pending().await
        }
    };
    spawner.spawn(network_task);
    spawner.spawn(dhcp_task);
    serve_api(controller, stack, handoffs, session_parameters, session_rng).await;
    #[allow(
        clippy::drop_non_drop,
        reason = "the alpha diagnostics peripheral token is retained across the complete async service lifetime"
    )]
    core::mem::drop(alpha_usb_serial_jtag_owner);
}

/// Drive the Embassy network stack forever.
#[embassy_executor::task]
pub async fn run_network(mut runner: Runner<'static, Interface<'static>>) {
    runner.run().await
}

/// Serve DHCPv4 for the fixed proof subnet.
#[embassy_executor::task]
pub async fn run_dhcp(stack: Stack<'static>) {
    let gateway = Ipv4Addr::from(profile::GATEWAY_IPV4);
    let mut packet = [0_u8; 1_500];
    let mut gateways = [Ipv4Addr::UNSPECIFIED];
    let buffers = UdpBuffers::<1, 1_024, 1_024, 10>::new();
    let unbound = Udp::new(stack, &buffers);
    let mut socket = match unbound
        .bind(SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            DEFAULT_SERVER_PORT,
        )))
        .await
    {
        Ok(socket) => socket,
        Err(reason) => {
            error!("e290-node stage=wifi-dhcp status=FAIL operation=bind reason={reason:?}");
            core::future::pending().await
        }
    };

    loop {
        if let Err(reason) = io::server::run(
            &mut Server::<_, { profile::DHCP_LEASES }>::new_with_et(gateway),
            &ServerOptions::new(gateway, Some(&mut gateways)),
            &mut socket,
            &mut packet,
        )
        .await
        {
            warn!("e290-node stage=wifi-dhcp status=RETRY reason={reason:?}");
        }
        Timer::after_millis(profile::DHCP_RETRY_INTERVAL_MS).await;
    }
}

/// Retain the Wi-Fi controller and serve one authenticated RDA1 TCP client.
async fn serve_api(
    controller: WifiController<'static>,
    stack: Stack<'static>,
    handoffs: WifiHandoffs,
    session_parameters: ServerParameters,
    mut session_rng: Trng,
) {
    let _controller = controller;
    let WifiHandoffs {
        pairing: mut pairing_handoff,
        _live_pairing,
        admission: mut session_admission,
        mut authenticated_api,
    } = handoffs;
    let mut authenticated_session = UsbAuthenticatedSession::new(session_parameters);
    let mut next_connection = NonZeroU64::MIN;
    let mut rx_buffer = [0_u8; profile::TCP_RX_BUFFER_BYTES];
    let mut tx_buffer = [0_u8; profile::TCP_TX_BUFFER_BYTES];
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(profile::TCP_IDLE_TIMEOUT_SECONDS)));
    socket.set_keep_alive(Some(Duration::from_secs(profile::TCP_KEEPALIVE_SECONDS)));
    socket.set_nagle_enabled(false);

    stack.wait_config_up().await;
    info!(
        "e290-node stage=wifi-api status=READY gateway={}.{}.{}.{} prefix={} tcp_port={} softap_security=wpa2-personal clients=1 session_suite=wifi-qualification integrity=authenticated confidentiality=none pairing=usb-profile-required",
        profile::GATEWAY_IPV4[0],
        profile::GATEWAY_IPV4[1],
        profile::GATEWAY_IPV4[2],
        profile::GATEWAY_IPV4[3],
        profile::GATEWAY_PREFIX_LEN,
        profile::RDA1_TCP_PORT,
    );

    loop {
        drain_stale_replies(
            &mut authenticated_session,
            &mut session_admission,
            &mut authenticated_api,
            &mut session_rng,
        );
        if let Err(reason) = socket
            .accept(IpListenEndpoint {
                addr: None,
                port: profile::RDA1_TCP_PORT,
            })
            .await
        {
            warn!("e290-node stage=wifi-api status=ACCEPT-RETRY reason={reason:?}");
            socket.abort();
            Timer::after_millis(profile::DHCP_RETRY_INTERVAL_MS).await;
            continue;
        }

        let connection = ConnectionId::new(next_connection.get())
            .expect("the nonzero Wi-Fi connection counter is valid");
        let Some(successor) = next_connection
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
        else {
            error!("e290-node stage=wifi-api status=FAIL reason=connection-epoch-exhausted");
            socket.abort();
            core::future::pending().await
        };
        next_connection = successor;

        if !announce_lifecycle(
            &mut pairing_handoff,
            PairingControlCommand::Connected {
                at: pairing_time(),
                connection,
            },
            connection,
            LifecycleAcknowledgement::Connected,
        )
        .await
        {
            error!(
                "e290-node stage=wifi-api status=FAIL reason=connected-lifecycle-mismatch connection={}",
                connection.get()
            );
            socket.abort();
            core::future::pending().await
        }

        debug_assert_eq!(
            authenticated_session.phase(),
            UsbAuthenticatedSessionPhase::Disconnected
        );
        if !authenticated_session.begin_connection(connection) {
            error!(
                "e290-node stage=wifi-api status=FAIL reason=session-owner-not-disconnected connection={}",
                connection.get()
            );
            socket.abort();
            core::future::pending().await
        }
        info!(
            "e290-node stage=wifi-api status=CONNECTED connection={} remote={:?}",
            connection.get(),
            socket.remote_endpoint(),
        );

        serve_connection(
            &mut socket,
            &mut authenticated_session,
            &mut session_admission,
            &mut authenticated_api,
            &mut session_rng,
        )
        .await;
        authenticated_session.reset();
        socket.abort();
        let _ = with_timeout(Duration::from_millis(100), socket.flush()).await;

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
                "e290-node stage=wifi-api status=FAIL reason=disconnected-lifecycle-mismatch connection={}",
                connection.get()
            );
            core::future::pending().await
        }
        info!(
            "e290-node stage=wifi-api status=DISCONNECTED connection={}",
            connection.get()
        );
    }
}

async fn serve_connection(
    socket: &mut TcpSocket<'_>,
    session: &mut UsbAuthenticatedSession,
    admission: &mut BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: &mut BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    session_rng: &mut Trng,
) {
    let mut decoder = StreamDecoder::new();
    loop {
        let mut progressed = false;

        while let Some(reply) = admission.try_receive_reply() {
            let _ = session.accept_admission_reply(reply, session_rng);
            progressed = true;
        }
        while let Some(reply) = authenticated_api.replies().try_receive() {
            if let Err(fault) = session.accept_node_reply(reply) {
                drop(fault.into_reply());
            }
            progressed = true;
        }

        if session.tx_kind().is_some() && socket.can_send() {
            let Some(chunk) = session.next_tx_chunk(profile::MAX_RDA1_BYTES_PER_POLL) else {
                break;
            };
            match socket.write(chunk).await {
                Ok(0) | Err(_) => break,
                Ok(acknowledged) => {
                    if session.advance_tx(acknowledged).is_err() {
                        break;
                    }
                    progressed = true;
                }
            }
        }

        let _ = session.try_send_admission_command(admission);
        let _ = session.try_send_request(authenticated_api.requests());

        if authenticated_rx_enabled(session.phase()) && socket.can_recv() {
            let mut byte = [0_u8; 1];
            match socket.read(&mut byte).await {
                Ok(1) => {
                    let event = decoder.push(byte[0]);
                    if !matches!(event, DecodeEvent::Pending) {
                        let result = session.accept_decode_event(event, pairing_time());
                        if !matches!(result, Ok(UsbSessionRxDisposition::Pending)) {
                            progressed = true;
                        }
                    }
                }
                Ok(_) | Err(_) => break,
            }
        }

        if !socket.may_recv()
            || matches!(
                session.phase(),
                UsbAuthenticatedSessionPhase::TerminatedUntilReset
            )
        {
            break;
        }
        if !progressed {
            Timer::after_millis(profile::API_POLL_INTERVAL_MS).await;
        }
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

const fn authenticated_rx_enabled(phase: UsbAuthenticatedSessionPhase) -> bool {
    matches!(
        phase,
        UsbAuthenticatedSessionPhase::AwaitingClientHello
            | UsbAuthenticatedSessionPhase::PendingClientProof
            | UsbAuthenticatedSessionPhase::Established
    )
}

fn pairing_time() -> PairingMillis {
    PairingMillis::new(Instant::now().as_millis())
}

// Keep the handoff storage type in this module's graph audit. Only the split
// bearer role crosses into this task.
const _: usize =
    core::mem::size_of::<DeviceApiHandoff<CriticalSectionRawMutex, AuthenticatedGrant>>();
