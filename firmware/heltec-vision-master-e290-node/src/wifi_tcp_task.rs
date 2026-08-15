//! Outbound Reticulum TCP packet-interface actor.
//!
//! This actor shares the station-owned Embassy stack, but owns one TCP socket,
//! interface slot 2, and both of that slot's exact permit/completion lanes. It
//! never creates another Reticulum node. Native packets cross the existing
//! interface fabric and use Rete's standard TCP HDLC framing.

#![cfg(all(target_arch = "xtensa", feature = "wifi-tcp-proof"))]

use core::{future::poll_fn, mem};

use embassy_futures::select::{Either3, Either4, select3, select4};
use embassy_net::{
    IpAddress, IpEndpoint, Ipv4Address, Stack,
    dns::DnsQueryType,
    tcp::{ConnectError, TcpSocket},
    udp::{PacketMetadata, UdpSocket},
};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use log::{error, info, warn};
#[cfg(reticulum_e290_ble_startup_diagnostic)]
use rete_core::Packet;
use rete_core::{
    MTU,
    hdlc::{self, HdlcDecoder, MAX_ENCODED},
};
use reticulum_device_api::{
    MAX_RETICULUM_DNS_DHCP_SERVERS, MAX_RETICULUM_DNS_RAW_ATTEMPTS, ReticulumDnsDiagnostics,
    ReticulumDnsPrimaryOutcome, ReticulumDnsRawAttempt, ReticulumDnsRawOutcome,
    ReticulumDnsRawSetupState, ReticulumDnsRawSource, ReticulumDnsResolution,
    ReticulumDnsResolutionSource,
};
use reticulum_heltec_vision_master_e290_node::{
    config,
    dns_wire::{
        DnsResponseError, MAX_DNS_MESSAGE_BYTES, allows_public_fallback, encode_a_query,
        parse_a_response,
    },
    wifi_driver_metrics::{WIFI_DRIVER_METRICS, WifiDriverMetricsSnapshot},
    wifi_tcp_profile::{
        WifiTcpBootstrap, WifiTcpFailure, WifiTcpPeerAddress, WifiTcpPhase, WifiTcpStatus,
        WifiTcpStatusCell, tcp_network_ready, tcp_stream_requirements,
    },
};
use reticulum_interface_router::{
    ActorCompletionSendError, ActorIngressBindingError, ActorIngressSendError,
    AvailableIngressBuffer, InterfaceDescriptor, InterfaceIngressActorHandoff,
    InterfaceIngressAuthority, InterfaceLifecycleActorHandoff, InterfaceLifecycleState,
    InterfaceTxActorHandoff, InterfaceTxCompletion, InterfaceTxJob, SealedIngressPacket,
};
use reticulum_node_core::{
    MonotonicMillis, OrdinaryFrameError, OrdinaryPermitResolution, PermitResolution,
    TxCompletionCode, TxFrameError,
};
use reticulum_tx_handoff::{DispatcherPermitHandoff, OrdinaryDispatcherPermitHandoff};
use reticulum_tx_supervisor::NodeInterfaceActorPorts;
#[cfg(reticulum_e290_ble_startup_diagnostic)]
use sha2::{Digest, Sha256};
use static_cell::StaticCell;

#[cfg(reticulum_e290_ble_startup_diagnostic)]
macro_rules! wireless_diagnostic {
    ($($argument:tt)*) => {
        esp_println::println!($($argument)*)
    };
}

#[cfg(not(reticulum_e290_ble_startup_diagnostic))]
macro_rules! wireless_diagnostic {
    // Native-frame traffic can be frequent on a public Reticulum peer. Keep
    // these packet-level traces available to a Debug build without making the
    // synchronous USB logger part of every production TCP frame's hot path.
    ($($argument:tt)*) => {
        log::debug!($($argument)*)
    };
}

/// Socket receive capacity. One full worst-case native frame fits with slack.
const SOCKET_RX_BYTES: usize = 1_024;
/// Socket transmit capacity. One worst-case HDLC frame fits without splitting.
const SOCKET_TX_BYTES: usize = MAX_ENCODED + 22;
/// Bounded stream-read batch retained while waiting for an ingress owner.
const STREAM_READ_BYTES: usize = 256;
/// Outbound connect and complete-frame write deadline.
const IO_DEADLINE_SECONDS: u64 = 30;
/// TCP keepalive interval for an otherwise idle Reticulum stream.
const KEEPALIVE_SECONDS: u64 = 30;
/// Inactivity deadline after keepalives stop receiving acknowledgement.
const SOCKET_IDLE_TIMEOUT_SECONDS: u64 = 180;
/// Initial outbound-peer reconnect delay.
const INITIAL_BACKOFF_SECONDS: u64 = 1;
/// Maximum outbound-peer reconnect delay.
const MAXIMUM_BACKOFF_SECONDS: u64 = 30;
/// Allow smoltcp's DHCP resolver to complete one full per-server attempt.
const DHCP_DNS_DEADLINE_SECONDS: u64 = 11;
/// Per-resolver deadline for queueing one explicit raw UDP query.
const RAW_DNS_ENQUEUE_DEADLINE_SECONDS: u64 = 3;
/// Per-resolver deadline for smoltcp to hand one query to the Wi-Fi driver.
const RAW_DNS_EGRESS_DEADLINE_SECONDS: u64 = 3;
/// Per-resolver deadline after egress for one matching DNS response.
const RAW_DNS_RESPONSE_DEADLINE_SECONDS: u64 = 3;
const DNS_PORT: u16 = 53;
const MAX_DNS_QUERY_BYTES: usize = 272;
const PUBLIC_DNS_FALLBACKS: [[u8; 4]; 2] = [[1, 1, 1, 1], [9, 9, 9, 9]];
const FIRST_PUBLIC_DNS_ATTEMPT: usize = MAX_RETICULUM_DNS_DHCP_SERVERS;

const COMPLETION_TRANSMITTED: TxCompletionCode = TxCompletionCode::new(0xe221);
const COMPLETION_UNPERMITTED: TxCompletionCode = TxCompletionCode::new(0xe222);
const COMPLETION_EXPIRED: TxCompletionCode = TxCompletionCode::new(0xe223);
const COMPLETION_IO_RECOVERY: TxCompletionCode = TxCompletionCode::new(0xe224);
const COMPLETION_FRAME_RECOVERY: TxCompletionCode = TxCompletionCode::new(0xe225);

struct WifiTcpIoStorage {
    socket_rx: [u8; SOCKET_RX_BYTES],
    socket_tx: [u8; SOCKET_TX_BYTES],
    stream_read: [u8; STREAM_READ_BYTES],
    encoded: [u8; MAX_ENCODED],
    decoder: HdlcDecoder<MTU>,
}

impl WifiTcpIoStorage {
    const fn new() -> Self {
        Self {
            socket_rx: [0; SOCKET_RX_BYTES],
            socket_tx: [0; SOCKET_TX_BYTES],
            stream_read: [0; STREAM_READ_BYTES],
            encoded: [0; MAX_ENCODED],
            decoder: HdlcDecoder::new(),
        }
    }
}

static TCP_IO: StaticCell<WifiTcpIoStorage> = StaticCell::new();

struct DnsFallbackIoStorage {
    rx_metadata: [PacketMetadata; 1],
    rx: [u8; MAX_DNS_MESSAGE_BYTES],
    tx_metadata: [PacketMetadata; 1],
    tx: [u8; MAX_DNS_QUERY_BYTES],
    query: [u8; MAX_DNS_QUERY_BYTES],
}

impl DnsFallbackIoStorage {
    const fn new() -> Self {
        Self {
            rx_metadata: [PacketMetadata::EMPTY; 1],
            rx: [0; MAX_DNS_MESSAGE_BYTES],
            tx_metadata: [PacketMetadata::EMPTY; 1],
            tx: [0; MAX_DNS_QUERY_BYTES],
            query: [0; MAX_DNS_QUERY_BYTES],
        }
    }
}

static DNS_FALLBACK_IO: StaticCell<DnsFallbackIoStorage> = StaticCell::new();

/// Exact slot-2 capabilities consumed by the permanent TCP actor.
#[must_use = "dropping the TCP actor ports abandons exact interface ownership"]
pub struct WifiTcpActorPorts {
    tx: InterfaceTxActorHandoff<CriticalSectionRawMutex, { config::INTERFACE_QUEUE_DEPTH }>,
    ingress:
        InterfaceIngressActorHandoff<CriticalSectionRawMutex, { config::INTERFACE_QUEUE_DEPTH }>,
    lifecycle: InterfaceLifecycleActorHandoff<CriticalSectionRawMutex>,
    authority: InterfaceIngressAuthority,
    data_permit: DispatcherPermitHandoff<CriticalSectionRawMutex>,
    ordinary_permit: OrdinaryDispatcherPermitHandoff<CriticalSectionRawMutex>,
}

impl WifiTcpActorPorts {
    /// Bind common-slot router and permit capabilities to one offline descriptor.
    pub fn new(
        actor: NodeInterfaceActorPorts<CriticalSectionRawMutex, { config::INTERFACE_QUEUE_DEPTH }>,
        descriptor: InterfaceDescriptor,
    ) -> Result<Self, ActorIngressBindingError> {
        let (interface, data_permit, ordinary_permit) = actor.into_parts();
        let (tx, ingress, lifecycle) = interface.into_parts();
        let authority = ingress.bind_ingress(descriptor)?;
        Ok(Self {
            tx,
            ingress,
            lifecycle,
            authority,
            data_permit,
            ordinary_permit,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionEnd {
    NetworkDown,
    SocketClosed,
    TransmitFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrameWriteFailure {
    Write,
    ZeroWrite,
    Flush,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteResolutionError {
    Timeout,
    Lookup,
    NoIpv4Result,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawDnsError {
    Bind,
    Send,
    EnqueueTimeout,
    EgressTimeout,
    Timeout,
    Response(DnsResponseError),
}

/// Run the immutable boot peer against the station-owned network stack.
///
/// `None` retains the coordinator-published disabled revision and every exact
/// slot capability offline for the life of the boot.
#[embassy_executor::task]
pub async fn run(
    stack: Option<Stack<'static>>,
    bootstrap: Option<WifiTcpBootstrap>,
    status: &'static WifiTcpStatusCell,
    mut ports: WifiTcpActorPorts,
) -> ! {
    let Some(bootstrap) = bootstrap else {
        info!("e290-node stage=wifi-tcp status=DISABLED reason=no-enabled-peer");
        core::future::pending::<()>().await;
        unreachable!()
    };
    let Some(stack) = stack else {
        info!("e290-node stage=wifi-tcp status=WAITING reason=station-unavailable");
        core::future::pending::<()>().await;
        unreachable!()
    };

    let storage = TCP_IO.init(WifiTcpIoStorage::new());
    let dns_fallback_storage = DNS_FALLBACK_IO.init(DnsFallbackIoStorage::new());
    let WifiTcpIoStorage {
        socket_rx,
        socket_tx,
        stream_read,
        encoded,
        decoder,
    } = storage;
    let mut available = None;
    let mut reconnect_backoff_seconds = INITIAL_BACKOFF_SECONDS;

    loop {
        while !tcp_network_ready(stack.is_link_up(), stack.is_config_up()) {
            publish(status, bootstrap, WifiTcpPhase::WaitingForNetwork);
            if !stack.is_link_up() {
                stack.wait_link_up().await;
                continue;
            }
            match select3(
                stack.wait_config_up(),
                stack.wait_link_down(),
                Timer::after(Duration::from_secs(1)),
            )
            .await
            {
                Either3::First(()) | Either3::Second(()) | Either3::Third(()) => {}
            }
        }

        publish_connecting(status, bootstrap);
        let remote_address =
            match resolve_remote_ipv4(stack, bootstrap, &mut *dns_fallback_storage, status).await {
                Ok(address) => address,
                Err(reason) => {
                    warn!(
                        "e290-node stage=wifi-tcp status=RETRY operation=resolve reason={}",
                        resolution_error_label(reason)
                    );
                    backoff(
                        status,
                        bootstrap,
                        &mut reconnect_backoff_seconds,
                        tcp_network_ready(stack.is_link_up(), stack.is_config_up()),
                        Some(resolution_failure(reason)),
                    )
                    .await;
                    continue;
                }
            };
        let mut socket = TcpSocket::new(stack, &mut *socket_rx, &mut *socket_tx);
        socket.set_nagle_enabled(false);
        socket.set_keep_alive(Some(Duration::from_secs(KEEPALIVE_SECONDS)));
        socket.set_timeout(Some(Duration::from_secs(SOCKET_IDLE_TIMEOUT_SECONDS)));
        let remote = IpEndpoint::new(remote_address.into(), bootstrap.port());
        wireless_diagnostic!(
            "e290-wireless-diagnostic stage=reticulum-tcp-connect status=START remote={remote:?} link_up={} config_up={}",
            stack.is_link_up(),
            stack.is_config_up(),
        );
        match with_timeout(
            Duration::from_secs(IO_DEADLINE_SECONDS),
            socket.connect(remote),
        )
        .await
        {
            Ok(Ok(())) => {
                wireless_diagnostic!(
                    "e290-wireless-diagnostic stage=reticulum-tcp-connect status=PASS remote={remote:?} local={:?} state={:?} link_up={} config_up={}",
                    socket.local_endpoint(),
                    socket.state(),
                    stack.is_link_up(),
                    stack.is_config_up(),
                );
            }
            Ok(Err(reason)) => {
                wireless_diagnostic!(
                    "e290-wireless-diagnostic stage=reticulum-tcp-connect status=RETRY remote={remote:?} reason={} local={:?} state={:?} link_up={} config_up={}",
                    connect_error_label(reason),
                    socket.local_endpoint(),
                    socket.state(),
                    stack.is_link_up(),
                    stack.is_config_up(),
                );
                warn!(
                    "e290-node stage=wifi-tcp status=RETRY operation=connect reason={}",
                    connect_error_label(reason)
                );
                abort_socket(&mut socket).await;
                backoff(
                    status,
                    bootstrap,
                    &mut reconnect_backoff_seconds,
                    tcp_network_ready(stack.is_link_up(), stack.is_config_up()),
                    Some(connect_failure(reason)),
                )
                .await;
                continue;
            }
            Err(_) => {
                wireless_diagnostic!(
                    "e290-wireless-diagnostic stage=reticulum-tcp-connect status=RETRY remote={remote:?} reason=timeout local={:?} state={:?} link_up={} config_up={}",
                    socket.local_endpoint(),
                    socket.state(),
                    stack.is_link_up(),
                    stack.is_config_up(),
                );
                warn!("e290-node stage=wifi-tcp status=RETRY operation=connect reason=timeout");
                abort_socket(&mut socket).await;
                backoff(
                    status,
                    bootstrap,
                    &mut reconnect_backoff_seconds,
                    tcp_network_ready(stack.is_link_up(), stack.is_config_up()),
                    Some(WifiTcpFailure::ConnectTimeout),
                )
                .await;
                continue;
            }
        }
        if !tcp_network_ready(stack.is_link_up(), stack.is_config_up()) {
            wireless_diagnostic!(
                "e290-wireless-diagnostic stage=reticulum-tcp-connect status=RETRY remote={remote:?} reason=network-down-after-connect local={:?} state={:?} link_up={} config_up={}",
                socket.local_endpoint(),
                socket.state(),
                stack.is_link_up(),
                stack.is_config_up(),
            );
            abort_socket(&mut socket).await;
            backoff(
                status,
                bootstrap,
                &mut reconnect_backoff_seconds,
                false,
                Some(WifiTcpFailure::ConnectNoRoute),
            )
            .await;
            continue;
        }

        match ports
            .lifecycle
            .request_state(ports.authority.lease(), InterfaceLifecycleState::Ready)
            .await
        {
            Ok(descriptor) => {
                publish(status, bootstrap, WifiTcpPhase::Connected);
                info!(
                    "e290-node stage=wifi-tcp status=ONLINE interface={} generation={} queue={}",
                    descriptor.lease().interface().get(),
                    descriptor.lease().generation().get(),
                    descriptor.lease().queue().get(),
                );
            }
            Err(reason) => {
                error!(
                    "e290-node stage=wifi-tcp status=FAIL operation=lifecycle-ready reason={reason:?}"
                );
                abort_socket(&mut socket).await;
                fault_forever(status, bootstrap, &mut ports, reason).await
            }
        }
        reconnect_backoff_seconds = INITIAL_BACKOFF_SECONDS;
        decoder.reset();

        let ended = service_connection(
            &mut socket,
            stack,
            &mut ports,
            &mut available,
            decoder,
            stream_read,
            encoded,
            status,
            bootstrap,
        )
        .await;
        wireless_diagnostic!(
            "e290-wireless-diagnostic stage=reticulum-tcp-service status=ENDED reason={ended:?} state={:?} local={:?} remote={:?} send_queue={} recv_queue={} link_up={} config_up={}",
            socket.state(),
            socket.local_endpoint(),
            socket.remote_endpoint(),
            socket.send_queue(),
            socket.recv_queue(),
            stack.is_link_up(),
            stack.is_config_up(),
        );

        if let Err(reason) = ports
            .lifecycle
            .request_state(ports.authority.lease(), InterfaceLifecycleState::Offline)
            .await
        {
            error!(
                "e290-node stage=wifi-tcp status=FAIL operation=lifecycle-offline reason={reason:?}"
            );
            abort_socket(&mut socket).await;
            fault_forever(status, bootstrap, &mut ports, reason).await
        }
        abort_socket(&mut socket).await;
        warn!("e290-node stage=wifi-tcp status=OFFLINE reason={ended:?}");
        backoff(
            status,
            bootstrap,
            &mut reconnect_backoff_seconds,
            tcp_network_ready(stack.is_link_up(), stack.is_config_up()),
            connection_end_failure(ended),
        )
        .await;
    }
}

async fn resolve_remote_ipv4(
    stack: Stack<'static>,
    bootstrap: WifiTcpBootstrap,
    fallback_storage: &mut DnsFallbackIoStorage,
    status: &'static WifiTcpStatusCell,
) -> Result<Ipv4Address, RemoteResolutionError> {
    match bootstrap.address() {
        WifiTcpPeerAddress::Ipv4(ipv4) => {
            info!(
                "e290-node stage=wifi-tcp-dns status=SKIPPED reason=literal-ipv4 remote={}.{}.{}.{}",
                ipv4[0], ipv4[1], ipv4[2], ipv4[3]
            );
            Ok(Ipv4Address::new(ipv4[0], ipv4[1], ipv4[2], ipv4[3]))
        }
        address @ WifiTcpPeerAddress::Dns { .. } => {
            let hostname = address
                .dns_hostname()
                .expect("the selected address variant contains a DNS hostname");
            let mut diagnostics = initial_dns_diagnostics(stack);
            publish_dns_diagnostics(status, bootstrap, diagnostics);
            info!(
                "e290-node stage=wifi-tcp-dns status=START hostname={} gateway={:?} dhcp_servers={:?} link_up={} config_up={}",
                hostname,
                diagnostics.gateway_ipv4,
                diagnostics.dhcp_servers,
                stack.is_link_up(),
                stack.is_config_up(),
            );

            if diagnostics.dhcp_servers.iter().all(Option::is_none) {
                diagnostics.primary_outcome = ReticulumDnsPrimaryOutcome::NoServers;
                publish_dns_diagnostics(status, bootstrap, diagnostics);
            } else {
                diagnostics.primary_outcome = ReticulumDnsPrimaryOutcome::Resolving;
                publish_dns_diagnostics(status, bootstrap, diagnostics);
            }

            let started = Instant::now();
            let primary = if diagnostics.primary_outcome == ReticulumDnsPrimaryOutcome::NoServers {
                RemoteResolutionError::Lookup
            } else {
                match with_timeout(
                    Duration::from_secs(DHCP_DNS_DEADLINE_SECONDS),
                    stack.dns_query(hostname, DnsQueryType::A),
                )
                .await
                {
                    Ok(Ok(results)) => {
                        if let Some(address) = results
                            .into_iter()
                            .next()
                            .map(|IpAddress::Ipv4(address)| address)
                        {
                            diagnostics.primary_outcome = ReticulumDnsPrimaryOutcome::Resolved;
                            diagnostics.resolution = Some(ReticulumDnsResolution::new(
                                address.octets(),
                                ReticulumDnsResolutionSource::SystemDns,
                                None,
                            ));
                            publish_dns_diagnostics(status, bootstrap, diagnostics);
                            info!(
                                "e290-node stage=wifi-tcp-dns resolver=dhcp status=RESOLVED elapsed_ms={}",
                                elapsed_millis(started)
                            );
                            return Ok(address);
                        }
                        diagnostics.primary_outcome = ReticulumDnsPrimaryOutcome::NoIpv4Result;
                        RemoteResolutionError::NoIpv4Result
                    }
                    Ok(Err(reason)) => {
                        diagnostics.primary_outcome = ReticulumDnsPrimaryOutcome::LookupFailed;
                        warn!(
                            "e290-node stage=wifi-tcp-dns resolver=dhcp status=FAILED detail={reason:?}"
                        );
                        RemoteResolutionError::Lookup
                    }
                    Err(_) => {
                        diagnostics.primary_outcome = ReticulumDnsPrimaryOutcome::Timeout;
                        RemoteResolutionError::Timeout
                    }
                }
            };
            publish_dns_diagnostics(status, bootstrap, diagnostics);
            warn!(
                "e290-node stage=wifi-tcp-dns resolver=dhcp status=FAILED reason={} elapsed_ms={}",
                resolution_error_label(primary),
                elapsed_millis(started)
            );

            resolve_with_raw_dns(
                stack,
                hostname,
                fallback_storage,
                status,
                bootstrap,
                &mut diagnostics,
                primary,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_with_raw_dns(
    stack: Stack<'static>,
    hostname: &str,
    storage: &mut DnsFallbackIoStorage,
    status: &'static WifiTcpStatusCell,
    bootstrap: WifiTcpBootstrap,
    diagnostics: &mut ReticulumDnsDiagnostics,
    primary_error: RemoteResolutionError,
) -> Result<Ipv4Address, RemoteResolutionError> {
    let cycle_metrics_started = WIFI_DRIVER_METRICS.snapshot();
    let public_fallback_allowed = allows_public_fallback(hostname);
    let has_dhcp_raw_attempt = diagnostics.raw_attempts[..FIRST_PUBLIC_DNS_ATTEMPT]
        .iter()
        .any(Option::is_some);
    if !has_dhcp_raw_attempt && !public_fallback_allowed {
        skip_public_for_local_name(status, bootstrap, diagnostics);
        return Err(primary_error);
    }

    let DnsFallbackIoStorage {
        rx_metadata,
        rx,
        tx_metadata,
        tx,
        query,
    } = storage;

    let mut final_error = primary_error;
    for index in 0..MAX_RETICULUM_DNS_RAW_ATTEMPTS {
        let Some(attempt) = diagnostics.raw_attempts[index] else {
            continue;
        };
        if attempt.source == ReticulumDnsRawSource::Public && !public_fallback_allowed {
            set_raw_attempt_outcome(diagnostics, index, ReticulumDnsRawOutcome::SkippedLocalName);
            publish_dns_diagnostics(status, bootstrap, *diagnostics);
            info!(
                "e290-node stage=wifi-tcp-dns resolver=public server={}.{}.{}.{} status=SKIPPED reason=local-name",
                attempt.server[0], attempt.server[1], attempt.server[2], attempt.server[3],
            );
            continue;
        }
        if raw_server_attempted_before(diagnostics, index, attempt.server) {
            set_raw_attempt_outcome(diagnostics, index, ReticulumDnsRawOutcome::SkippedDuplicate);
            publish_dns_diagnostics(status, bootstrap, *diagnostics);
            info!(
                "e290-node stage=wifi-tcp-dns resolver=raw server={}.{}.{}.{} status=SKIPPED reason=duplicate",
                attempt.server[0], attempt.server[1], attempt.server[2], attempt.server[3],
            );
            continue;
        }

        let server = Ipv4Address::new(
            attempt.server[0],
            attempt.server[1],
            attempt.server[2],
            attempt.server[3],
        );
        let transaction_id = fallback_transaction_id(server, index);
        let query_length = match encode_a_query(hostname, transaction_id, query) {
            Ok(length) => length,
            Err(_) => {
                diagnostics.raw_setup_state = ReticulumDnsRawSetupState::EncodeFailed;
                publish_dns_diagnostics(status, bootstrap, *diagnostics);
                warn!("e290-node stage=wifi-tcp-dns resolver=raw status=FAILED reason=encode");
                return Err(RemoteResolutionError::Lookup);
            }
        };
        let endpoint = IpEndpoint::new(server.into(), DNS_PORT);
        // Each resolver receives a fresh smoltcp socket. Cancelling a future
        // does not remove a datagram retained behind unresolved ARP or an
        // unavailable driver token; reusing that one-packet queue made later
        // resolver rows look attempted when their send was only blocked behind
        // the first query.
        let mut socket = UdpSocket::new(
            stack,
            &mut *rx_metadata,
            &mut *rx,
            &mut *tx_metadata,
            &mut *tx,
        );
        diagnostics.raw_setup_state = ReticulumDnsRawSetupState::Binding;
        publish_dns_diagnostics(status, bootstrap, *diagnostics);
        if socket.bind(0).is_err() {
            diagnostics.raw_setup_state = ReticulumDnsRawSetupState::BindFailed;
            publish_dns_diagnostics(status, bootstrap, *diagnostics);
            warn!(
                "e290-node stage=wifi-tcp-dns resolver=raw status=FAILED reason={}",
                raw_dns_error_label(RawDnsError::Bind)
            );
            return Err(RemoteResolutionError::Lookup);
        }
        diagnostics.raw_setup_state = ReticulumDnsRawSetupState::Ready;
        publish_dns_diagnostics(status, bootstrap, *diagnostics);

        let started = Instant::now();
        let attempt_metrics_started = WIFI_DRIVER_METRICS.snapshot();
        let outcome = query_raw_dns_server(
            &mut socket,
            &query[..query_length],
            endpoint,
            transaction_id,
            hostname,
            status,
            bootstrap,
            diagnostics,
            index,
        )
        .await;
        match outcome {
            Ok(address) => {
                set_raw_attempt_outcome(diagnostics, index, ReticulumDnsRawOutcome::Resolved);
                diagnostics.resolution = Some(ReticulumDnsResolution::new(
                    address.octets(),
                    match attempt.source {
                        ReticulumDnsRawSource::Dhcp => ReticulumDnsResolutionSource::RawDhcp,
                        ReticulumDnsRawSource::Public => ReticulumDnsResolutionSource::RawPublic,
                    },
                    Some(attempt.server),
                ));
                publish_dns_diagnostics(status, bootstrap, *diagnostics);
                info!(
                    "e290-node stage=wifi-tcp-dns resolver=raw server={} status=RESOLVED elapsed_ms={}",
                    server,
                    elapsed_millis(started)
                );
                log_driver_metrics_delta("raw-dns-attempt", "resolved", attempt_metrics_started);
                log_driver_metrics_delta("raw-dns-cycle", "resolved", cycle_metrics_started);
                return Ok(address);
            }
            Err(reason) => {
                set_raw_attempt_outcome(diagnostics, index, raw_dns_outcome(reason));
                publish_dns_diagnostics(status, bootstrap, *diagnostics);
                final_error = raw_resolution_error(reason);
                warn!(
                    "e290-node stage=wifi-tcp-dns resolver=raw server={} status=FAILED reason={} elapsed_ms={}",
                    server,
                    raw_dns_error_label(reason),
                    elapsed_millis(started)
                );
                log_driver_metrics_delta(
                    "raw-dns-attempt",
                    raw_dns_error_label(reason),
                    attempt_metrics_started,
                );
            }
        }
    }
    log_driver_metrics_delta("raw-dns-cycle", "exhausted", cycle_metrics_started);
    Err(final_error)
}

#[allow(clippy::too_many_arguments)]
async fn query_raw_dns_server(
    socket: &mut UdpSocket<'_>,
    query: &[u8],
    endpoint: IpEndpoint,
    transaction_id: u16,
    hostname: &str,
    status: &'static WifiTcpStatusCell,
    bootstrap: WifiTcpBootstrap,
    diagnostics: &mut ReticulumDnsDiagnostics,
    attempt_index: usize,
) -> Result<Ipv4Address, RawDnsError> {
    set_raw_attempt_outcome(diagnostics, attempt_index, ReticulumDnsRawOutcome::Sending);
    publish_dns_diagnostics(status, bootstrap, *diagnostics);
    match with_timeout(
        Duration::from_secs(RAW_DNS_ENQUEUE_DEADLINE_SECONDS),
        socket.send_to(query, endpoint),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return Err(RawDnsError::Send),
        Err(_) => return Err(RawDnsError::EnqueueTimeout),
    }
    if with_timeout(
        Duration::from_secs(RAW_DNS_EGRESS_DEADLINE_SECONDS),
        socket.flush(),
    )
    .await
    .is_err()
    {
        return Err(RawDnsError::EgressTimeout);
    }
    set_raw_attempt_outcome(
        diagnostics,
        attempt_index,
        ReticulumDnsRawOutcome::AwaitingResponse,
    );
    publish_dns_diagnostics(status, bootstrap, *diagnostics);
    with_timeout(
        Duration::from_secs(RAW_DNS_RESPONSE_DEADLINE_SECONDS),
        async {
            loop {
                let parsed = socket
                    .recv_from_with(|packet, metadata| {
                        if metadata.endpoint != endpoint {
                            return None;
                        }
                        match parse_a_response(packet, transaction_id, hostname) {
                            Ok(octets) => Some(Ok(Ipv4Address::new(
                                octets[0], octets[1], octets[2], octets[3],
                            ))),
                            Err(DnsResponseError::TransactionMismatch) => None,
                            Err(reason) => Some(Err(RawDnsError::Response(reason))),
                        }
                    })
                    .await;
                if let Some(parsed) = parsed {
                    return parsed;
                }
            }
        },
    )
    .await
    .map_err(|_| RawDnsError::Timeout)?
}

fn initial_dns_diagnostics(stack: Stack<'static>) -> ReticulumDnsDiagnostics {
    let mut gateway_ipv4 = None;
    let mut dhcp_servers = [None; MAX_RETICULUM_DNS_DHCP_SERVERS];
    let mut raw_attempts = [None; MAX_RETICULUM_DNS_RAW_ATTEMPTS];
    if let Some(config) = stack.config_v4() {
        gateway_ipv4 = config.gateway.map(|gateway| gateway.octets());
        for (index, server) in config.dns_servers.iter().enumerate() {
            let octets = server.octets();
            dhcp_servers[index] = Some(octets);
            raw_attempts[index] = Some(ReticulumDnsRawAttempt::new(
                ReticulumDnsRawSource::Dhcp,
                octets,
                ReticulumDnsRawOutcome::NotStarted,
            ));
        }
    }
    for (offset, server) in PUBLIC_DNS_FALLBACKS.into_iter().enumerate() {
        raw_attempts[FIRST_PUBLIC_DNS_ATTEMPT + offset] = Some(ReticulumDnsRawAttempt::new(
            ReticulumDnsRawSource::Public,
            server,
            ReticulumDnsRawOutcome::NotStarted,
        ));
    }
    ReticulumDnsDiagnostics::new(
        gateway_ipv4,
        dhcp_servers,
        ReticulumDnsPrimaryOutcome::NotStarted,
        ReticulumDnsRawSetupState::NotStarted,
        raw_attempts,
        None,
    )
}

fn set_raw_attempt_outcome(
    diagnostics: &mut ReticulumDnsDiagnostics,
    index: usize,
    outcome: ReticulumDnsRawOutcome,
) {
    if let Some(attempt) = diagnostics.raw_attempts[index].as_mut() {
        attempt.outcome = outcome;
    }
}

fn raw_server_attempted_before(
    diagnostics: &ReticulumDnsDiagnostics,
    index: usize,
    server: [u8; 4],
) -> bool {
    diagnostics.raw_attempts[..index]
        .iter()
        .flatten()
        .any(|attempt| {
            attempt.server == server
                && !matches!(
                    attempt.outcome,
                    ReticulumDnsRawOutcome::NotStarted
                        | ReticulumDnsRawOutcome::SkippedDuplicate
                        | ReticulumDnsRawOutcome::SkippedLocalName
                )
        })
}

fn skip_public_for_local_name(
    status: &'static WifiTcpStatusCell,
    bootstrap: WifiTcpBootstrap,
    diagnostics: &mut ReticulumDnsDiagnostics,
) {
    for index in FIRST_PUBLIC_DNS_ATTEMPT..MAX_RETICULUM_DNS_RAW_ATTEMPTS {
        set_raw_attempt_outcome(diagnostics, index, ReticulumDnsRawOutcome::SkippedLocalName);
        publish_dns_diagnostics(status, bootstrap, *diagnostics);
    }
}

fn fallback_transaction_id(server: Ipv4Address, index: usize) -> u16 {
    let octets = server.octets();
    let address_fold = u16::from_be_bytes([octets[0] ^ octets[2], octets[1] ^ octets[3]]);
    (Instant::now().as_millis() as u16).rotate_left((index as u32) & 15) ^ address_fold ^ 0xa126
}

fn elapsed_millis(started: Instant) -> u64 {
    Instant::now()
        .as_millis()
        .saturating_sub(started.as_millis())
}

fn log_driver_metrics_delta(operation: &str, outcome: &str, started: WifiDriverMetricsSnapshot) {
    let current = WIFI_DRIVER_METRICS.snapshot();
    let delta = current.wrapping_delta_since(started);
    info!(
        "e290-node stage=wifi-driver-metrics operation={} outcome={} tx_poll_some_delta={} tx_poll_none_delta={} tx_consumes_delta={} tx_bytes_delta={} tx_arp_delta={} tx_ipv4_delta={} tx_other_delta={} rx_poll_some_delta={} rx_poll_none_delta={} rx_consumes_delta={} rx_bytes_delta={} rx_arp_delta={} rx_ipv4_delta={} rx_other_delta={} link_up_delta={} link_down_delta={} tx_consumes_total={} rx_consumes_total={} internal_free={}",
        operation,
        outcome,
        delta.tx_poll_some,
        delta.tx_poll_none,
        delta.tx_token_consumes,
        delta.tx_bytes,
        delta.tx_arp_frames,
        delta.tx_ipv4_frames,
        delta.tx_other_frames,
        delta.rx_poll_some,
        delta.rx_poll_none,
        delta.rx_token_consumes,
        delta.rx_bytes,
        delta.rx_arp_frames,
        delta.rx_ipv4_frames,
        delta.rx_other_frames,
        delta.link_up_polls,
        delta.link_down_polls,
        current.tx_token_consumes,
        current.rx_token_consumes,
        esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into()),
    );
}

const fn frame_write_failure_label(reason: FrameWriteFailure) -> &'static str {
    match reason {
        FrameWriteFailure::Write => "write",
        FrameWriteFailure::ZeroWrite => "zero-write",
        FrameWriteFailure::Flush => "flush",
    }
}

const fn raw_dns_error_label(error: RawDnsError) -> &'static str {
    match error {
        RawDnsError::Bind => "bind",
        RawDnsError::Send => "send",
        RawDnsError::EnqueueTimeout => "enqueue-timeout",
        RawDnsError::EgressTimeout => "egress-timeout",
        RawDnsError::Timeout => "timeout",
        RawDnsError::Response(DnsResponseError::TransactionMismatch) => "transaction-mismatch",
        RawDnsError::Response(DnsResponseError::NotAResponse) => "not-a-response",
        RawDnsError::Response(DnsResponseError::Truncated) => "truncated",
        RawDnsError::Response(DnsResponseError::ResponseCode(_)) => "response-code",
        RawDnsError::Response(DnsResponseError::QuestionMismatch) => "question-mismatch",
        RawDnsError::Response(DnsResponseError::Malformed) => "malformed",
        RawDnsError::Response(DnsResponseError::NoIpv4Address) => "no-ipv4-address",
    }
}

const fn raw_dns_outcome(error: RawDnsError) -> ReticulumDnsRawOutcome {
    match error {
        RawDnsError::Bind => ReticulumDnsRawOutcome::Malformed,
        RawDnsError::Send | RawDnsError::EnqueueTimeout | RawDnsError::EgressTimeout => {
            ReticulumDnsRawOutcome::SendFailed
        }
        RawDnsError::Timeout => ReticulumDnsRawOutcome::Timeout,
        RawDnsError::Response(DnsResponseError::TransactionMismatch) => {
            ReticulumDnsRawOutcome::Malformed
        }
        RawDnsError::Response(DnsResponseError::NotAResponse) => {
            ReticulumDnsRawOutcome::NotAResponse
        }
        RawDnsError::Response(DnsResponseError::Truncated) => ReticulumDnsRawOutcome::Truncated,
        RawDnsError::Response(DnsResponseError::ResponseCode(code)) => {
            match ReticulumDnsRawOutcome::response_code_outcome(code) {
                Some(outcome) => outcome,
                None => ReticulumDnsRawOutcome::Malformed,
            }
        }
        RawDnsError::Response(DnsResponseError::QuestionMismatch) => {
            ReticulumDnsRawOutcome::QuestionMismatch
        }
        RawDnsError::Response(DnsResponseError::Malformed) => ReticulumDnsRawOutcome::Malformed,
        RawDnsError::Response(DnsResponseError::NoIpv4Address) => {
            ReticulumDnsRawOutcome::NoIpv4Result
        }
    }
}

const fn raw_resolution_error(error: RawDnsError) -> RemoteResolutionError {
    match error {
        RawDnsError::Timeout => RemoteResolutionError::Timeout,
        RawDnsError::Response(DnsResponseError::NoIpv4Address) => {
            RemoteResolutionError::NoIpv4Result
        }
        _ => RemoteResolutionError::Lookup,
    }
}

const fn resolution_error_label(error: RemoteResolutionError) -> &'static str {
    match error {
        RemoteResolutionError::Timeout => "timeout",
        RemoteResolutionError::Lookup => "lookup-failed",
        RemoteResolutionError::NoIpv4Result => "no-ipv4-result",
    }
}

#[allow(clippy::too_many_arguments)]
async fn service_connection(
    socket: &mut TcpSocket<'_>,
    stack: Stack<'static>,
    ports: &mut WifiTcpActorPorts,
    available: &mut Option<AvailableIngressBuffer>,
    decoder: &mut HdlcDecoder<MTU>,
    stream_read: &mut [u8; STREAM_READ_BYTES],
    encoded: &mut [u8; MAX_ENCODED],
    status: &'static WifiTcpStatusCell,
    bootstrap: WifiTcpBootstrap,
) -> ConnectionEnd {
    let mut read_position = 0;
    let mut read_length = 0;

    loop {
        if !tcp_network_ready(stack.is_link_up(), stack.is_config_up()) {
            return ConnectionEnd::NetworkDown;
        }
        if read_position < read_length {
            if available.is_none() {
                match select4(
                    stack.wait_link_down(),
                    stack.wait_config_down(),
                    ports.ingress.receive_buffer(),
                    ports.tx.receive_job(),
                )
                .await
                {
                    Either4::First(()) | Either4::Second(()) => {
                        return ConnectionEnd::NetworkDown;
                    }
                    Either4::Third(buffer) => *available = Some(buffer),
                    Either4::Fourth(job) => {
                        if !process_tx_job(socket, stack, ports, job, encoded, status, bootstrap)
                            .await
                        {
                            return ConnectionEnd::TransmitFailed;
                        }
                    }
                }
                continue;
            }

            let byte = stream_read[read_position];
            read_position += 1;
            if decoder.feed(byte) {
                let frame = decoder
                    .frame()
                    .expect("a completed HDLC frame has a nonempty decoded payload");
                diagnose_native_frame("ingress", frame);
                let mut buffer = available
                    .take()
                    .expect("frame processing requires one owner");
                let Some(destination) = buffer.capacity_mut().get_mut(..frame.len()) else {
                    warn!(
                        "e290-node stage=wifi-tcp-rx status=DROP reason=native-packet-capacity packet_len={}",
                        frame.len()
                    );
                    *available = Some(buffer);
                    continue;
                };
                destination.copy_from_slice(frame);
                match buffer.seal(frame.len()) {
                    Ok(packet) => {
                        submit_ingress(ports, packet, status, bootstrap).await;
                    }
                    Err(failure) => {
                        warn!(
                            "e290-node stage=wifi-tcp-rx status=DROP reason={:?} packet_len={}",
                            failure.reason(),
                            frame.len()
                        );
                        *available = Some(failure.into_buffer());
                    }
                }
            }
            continue;
        }

        if available.is_none() {
            match select4(
                stack.wait_link_down(),
                stack.wait_config_down(),
                ports.ingress.receive_buffer(),
                ports.tx.receive_job(),
            )
            .await
            {
                Either4::First(()) | Either4::Second(()) => return ConnectionEnd::NetworkDown,
                Either4::Third(buffer) => *available = Some(buffer),
                Either4::Fourth(job) => {
                    if !process_tx_job(socket, stack, ports, job, encoded, status, bootstrap).await
                    {
                        return ConnectionEnd::TransmitFailed;
                    }
                }
            }
            continue;
        }

        match select4(
            stack.wait_link_down(),
            stack.wait_config_down(),
            socket.read(stream_read),
            ports.tx.receive_job(),
        )
        .await
        {
            Either4::First(()) | Either4::Second(()) => return ConnectionEnd::NetworkDown,
            Either4::Third(Ok(0) | Err(_)) => return ConnectionEnd::SocketClosed,
            Either4::Third(Ok(length)) => {
                read_position = 0;
                read_length = length;
            }
            Either4::Fourth(job) => {
                if !process_tx_job(socket, stack, ports, job, encoded, status, bootstrap).await {
                    return ConnectionEnd::TransmitFailed;
                }
            }
        }
    }
}

async fn process_tx_job(
    socket: &mut TcpSocket<'_>,
    stack: Stack<'static>,
    ports: &mut WifiTcpActorPorts,
    job: InterfaceTxJob,
    encoded: &mut [u8; MAX_ENCODED],
    status: &'static WifiTcpStatusCell,
    bootstrap: WifiTcpBootstrap,
) -> bool {
    match job {
        InterfaceTxJob::Data(job) => {
            let (ticket, job) = job.into_parts();
            let requirements = tcp_stream_requirements(job.packet_len());
            let (pending, mut request) = job.begin_permit(requirements);
            loop {
                match ports.data_permit.requests().try_send(request) {
                    Ok(()) => break,
                    Err(full) => {
                        request = full.into_inner();
                        poll_fn(|context| ports.data_permit.requests().poll_ready_to_send(context))
                            .await;
                    }
                }
            }
            let reply = ports.data_permit.replies().receive().await;
            let (completion, socket_usable, actor_fault) = match pending.resolve(reply, now()) {
                Ok(PermitResolution::Authorized(mut owner)) => match owner.frame(now()) {
                    Ok(frame) => {
                        diagnose_native_frame("data", frame.bytes());
                        match hdlc::encode(frame.bytes(), encoded) {
                            Ok(length) if write_frame(socket, stack, &encoded[..length]).await => (
                                owner.complete_transmitted(COMPLETION_TRANSMITTED, now()),
                                true,
                                false,
                            ),
                            Ok(_) => (owner.complete(COMPLETION_IO_RECOVERY), false, false),
                            Err(_) => (owner.recovery_fault(COMPLETION_FRAME_RECOVERY), true, true),
                        }
                    }
                    Err(TxFrameError::DeadlineExpired { .. }) => {
                        (owner.complete(COMPLETION_EXPIRED), true, false)
                    }
                    Err(TxFrameError::AlreadyTaken | TxFrameError::Invariant) => {
                        (owner.recovery_fault(COMPLETION_FRAME_RECOVERY), true, true)
                    }
                },
                Ok(PermitResolution::Expired(owner)) => {
                    (owner.complete(COMPLETION_EXPIRED), true, false)
                }
                Ok(PermitResolution::Unpermitted(owner)) => {
                    (owner.complete(COMPLETION_UNPERMITTED), true, false)
                }
                Err(mismatch) => {
                    error!(
                        "e290-node stage=wifi-tcp-tx status=FAIL family=data reason=permit-reply-mismatch"
                    );
                    fault_forever(status, bootstrap, ports, mismatch).await
                }
            };
            let completion = match ticket.complete(completion) {
                Ok(completion) => completion,
                Err(mismatch) => {
                    error!(
                        "e290-node stage=wifi-tcp-tx status=FAIL family=data reason=completion-ticket-mismatch"
                    );
                    fault_forever(status, bootstrap, ports, mismatch).await
                }
            };
            return_completion(ports, completion, status, bootstrap).await;
            if actor_fault {
                error!(
                    "e290-node stage=wifi-tcp-tx status=FAIL family=data reason=frame-invariant"
                );
                fault_forever(status, bootstrap, ports, ()).await
            }
            socket_usable
        }
        InterfaceTxJob::Ordinary(job) => {
            let (ticket, job) = job.into_parts();
            let requirements = tcp_stream_requirements(job.packet_len());
            let (pending, mut request) = job.begin_permit(requirements);
            loop {
                match ports.ordinary_permit.requests().try_send(request) {
                    Ok(()) => break,
                    Err(full) => {
                        request = full.into_inner();
                        poll_fn(|context| {
                            ports.ordinary_permit.requests().poll_ready_to_send(context)
                        })
                        .await;
                    }
                }
            }
            let reply = ports.ordinary_permit.replies().receive().await;
            let (completion, socket_usable, actor_fault) = match pending.resolve(reply, now()) {
                Ok(OrdinaryPermitResolution::Authorized(mut owner)) => match owner.frame(now()) {
                    Ok(frame) => {
                        diagnose_native_frame("ordinary", frame.bytes());
                        match hdlc::encode(frame.bytes(), encoded) {
                            Ok(length) if write_frame(socket, stack, &encoded[..length]).await => (
                                owner.complete_transmitted(COMPLETION_TRANSMITTED),
                                true,
                                false,
                            ),
                            Ok(_) => (owner.cancel(COMPLETION_IO_RECOVERY), false, false),
                            Err(_) => (owner.recovery_fault(COMPLETION_FRAME_RECOVERY), true, true),
                        }
                    }
                    Err(OrdinaryFrameError::DeadlineExpired { .. }) => {
                        (owner.cancel(COMPLETION_EXPIRED), true, false)
                    }
                    Err(OrdinaryFrameError::AlreadyTaken | OrdinaryFrameError::Invariant) => {
                        (owner.recovery_fault(COMPLETION_FRAME_RECOVERY), true, true)
                    }
                },
                Ok(OrdinaryPermitResolution::Expired(owner)) => {
                    (owner.cancel(COMPLETION_EXPIRED), true, false)
                }
                Ok(OrdinaryPermitResolution::Unpermitted(owner)) => {
                    (owner.complete(COMPLETION_UNPERMITTED), true, false)
                }
                Err(mismatch) => {
                    error!(
                        "e290-node stage=wifi-tcp-tx status=FAIL family=ordinary reason=permit-reply-mismatch"
                    );
                    fault_forever(status, bootstrap, ports, mismatch).await
                }
            };
            let completion = match ticket.complete(completion) {
                Ok(completion) => completion,
                Err(mismatch) => {
                    error!(
                        "e290-node stage=wifi-tcp-tx status=FAIL family=ordinary reason=completion-ticket-mismatch"
                    );
                    fault_forever(status, bootstrap, ports, mismatch).await
                }
            };
            return_completion(ports, completion, status, bootstrap).await;
            if actor_fault {
                error!(
                    "e290-node stage=wifi-tcp-tx status=FAIL family=ordinary reason=frame-invariant"
                );
                fault_forever(status, bootstrap, ports, ()).await
            }
            socket_usable
        }
    }
}

async fn return_completion(
    ports: &mut WifiTcpActorPorts,
    mut completion: InterfaceTxCompletion,
    status: &'static WifiTcpStatusCell,
    bootstrap: WifiTcpBootstrap,
) {
    loop {
        match ports.tx.try_send_completion(completion) {
            Ok(()) => return,
            Err(failure) => match failure.reason() {
                ActorCompletionSendError::QueueFull(_) => {
                    completion = failure.into_completion();
                    ports.tx.wait_completion_capacity().await;
                }
                ActorCompletionSendError::ForeignQueue { .. } => {
                    error!(
                        "e290-node stage=wifi-tcp-tx status=FAIL reason=foreign-completion-queue"
                    );
                    fault_forever(status, bootstrap, ports, failure).await
                }
            },
        }
    }
}

async fn submit_ingress(
    ports: &mut WifiTcpActorPorts,
    mut packet: SealedIngressPacket,
    status: &'static WifiTcpStatusCell,
    bootstrap: WifiTcpBootstrap,
) {
    loop {
        match ports.ingress.try_send(ports.authority, packet) {
            Ok(()) => return,
            Err(failure) => match failure.reason() {
                ActorIngressSendError::QueueFull(_) => {
                    let (_, retained) = failure.into_parts();
                    packet = retained;
                    ports.ingress.wait_ready_to_send().await;
                }
                _ => {
                    error!("e290-node stage=wifi-tcp-rx status=FAIL reason=ingress-owner-mismatch");
                    fault_forever(status, bootstrap, ports, failure).await
                }
            },
        }
    }
}

async fn write_frame(socket: &mut TcpSocket<'_>, stack: Stack<'static>, encoded: &[u8]) -> bool {
    let metrics_started = WIFI_DRIVER_METRICS.snapshot();
    wireless_diagnostic!(
        "e290-wireless-diagnostic stage=reticulum-tcp-write status=START bytes={} state={:?} send_queue={} send_capacity={}",
        encoded.len(),
        socket.state(),
        socket.send_queue(),
        socket.send_capacity(),
    );
    let result = select3(
        stack.wait_link_down(),
        stack.wait_config_down(),
        with_timeout(Duration::from_secs(IO_DEADLINE_SECONDS), async {
            let mut offset = 0;
            while offset < encoded.len() {
                let written = socket
                    .write(&encoded[offset..])
                    .await
                    .map_err(|_| FrameWriteFailure::Write)?;
                if written == 0 {
                    return Err(FrameWriteFailure::ZeroWrite);
                }
                offset += written;
            }
            socket.flush().await.map_err(|_| FrameWriteFailure::Flush)
        }),
    )
    .await;
    match result {
        Either3::Third(Ok(Ok(()))) => {
            wireless_diagnostic!(
                "e290-wireless-diagnostic stage=reticulum-tcp-write status=PASS bytes={} state={:?} send_queue={}",
                encoded.len(),
                socket.state(),
                socket.send_queue(),
            );
            true
        }
        Either3::Third(Ok(Err(reason))) => {
            wireless_diagnostic!(
                "e290-wireless-diagnostic stage=reticulum-tcp-write status=FAIL bytes={} reason={reason:?} state={:?} send_queue={} recv_queue={}",
                encoded.len(),
                socket.state(),
                socket.send_queue(),
                socket.recv_queue(),
            );
            warn!(
                "e290-node stage=wifi-tcp-write status=FAIL bytes={} reason={reason:?} state={:?} send_queue={} recv_queue={}",
                encoded.len(),
                socket.state(),
                socket.send_queue(),
                socket.recv_queue(),
            );
            log_driver_metrics_delta(
                "tcp-write",
                frame_write_failure_label(reason),
                metrics_started,
            );
            false
        }
        Either3::Third(Err(_)) => {
            wireless_diagnostic!(
                "e290-wireless-diagnostic stage=reticulum-tcp-write status=FAIL bytes={} reason=deadline state={:?} send_queue={} recv_queue={}",
                encoded.len(),
                socket.state(),
                socket.send_queue(),
                socket.recv_queue(),
            );
            warn!(
                "e290-node stage=wifi-tcp-write status=FAIL bytes={} reason=deadline state={:?} send_queue={} recv_queue={}",
                encoded.len(),
                socket.state(),
                socket.send_queue(),
                socket.recv_queue(),
            );
            log_driver_metrics_delta("tcp-write", "deadline", metrics_started);
            false
        }
        Either3::First(()) | Either3::Second(()) => {
            wireless_diagnostic!(
                "e290-wireless-diagnostic stage=reticulum-tcp-write status=FAIL bytes={} reason=network-down state={:?} send_queue={} recv_queue={}",
                encoded.len(),
                socket.state(),
                socket.send_queue(),
                socket.recv_queue(),
            );
            warn!(
                "e290-node stage=wifi-tcp-write status=FAIL bytes={} reason=network-down state={:?} send_queue={} recv_queue={}",
                encoded.len(),
                socket.state(),
                socket.send_queue(),
                socket.recv_queue(),
            );
            log_driver_metrics_delta("tcp-write", "network-down", metrics_started);
            false
        }
    }
}

#[cfg(reticulum_e290_ble_startup_diagnostic)]
fn diagnose_native_frame(family: &str, bytes: &[u8]) {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    match Packet::parse(bytes) {
        Ok(packet) => wireless_diagnostic!(
            "e290-wireless-diagnostic stage=reticulum-tcp-native-frame family={} raw_bytes={} sha256_prefix={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} packet_type={:?} header_type={:?} dest_type={:?} context=0x{:02x} hops={} destination_prefix={:02x}{:02x}{:02x}{:02x}",
            family,
            bytes.len(),
            digest[0],
            digest[1],
            digest[2],
            digest[3],
            digest[4],
            digest[5],
            digest[6],
            digest[7],
            packet.packet_type,
            packet.header_type,
            packet.dest_type,
            packet.context,
            packet.hops,
            packet.destination_hash[0],
            packet.destination_hash[1],
            packet.destination_hash[2],
            packet.destination_hash[3],
        ),
        Err(_) => wireless_diagnostic!(
            "e290-wireless-diagnostic stage=reticulum-tcp-native-frame family={} raw_bytes={} sha256_prefix={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} parse=failed",
            family,
            bytes.len(),
            digest[0],
            digest[1],
            digest[2],
            digest[3],
            digest[4],
            digest[5],
            digest[6],
            digest[7],
        ),
    }
}

#[cfg(not(reticulum_e290_ble_startup_diagnostic))]
#[inline]
fn diagnose_native_frame(_: &str, _: &[u8]) {}

async fn abort_socket(socket: &mut TcpSocket<'_>) {
    socket.abort();
    let _ = with_timeout(Duration::from_secs(1), socket.flush()).await;
}

async fn backoff(
    status: &'static WifiTcpStatusCell,
    bootstrap: WifiTcpBootstrap,
    reconnect_backoff_seconds: &mut u64,
    network_up: bool,
    failure: Option<WifiTcpFailure>,
) {
    let phase = if network_up {
        WifiTcpPhase::Backoff
    } else {
        WifiTcpPhase::WaitingForNetwork
    };
    let snapshot = status.snapshot();
    status.publish(WifiTcpStatus::with_runtime_diagnostics(
        bootstrap,
        phase,
        failure,
        snapshot.dns_diagnostics,
    ));
    if network_up {
        Timer::after(Duration::from_secs(*reconnect_backoff_seconds)).await;
        *reconnect_backoff_seconds = reconnect_backoff_seconds
            .saturating_mul(2)
            .min(MAXIMUM_BACKOFF_SECONDS);
    }
}

async fn fault_forever<T>(
    status: &'static WifiTcpStatusCell,
    bootstrap: WifiTcpBootstrap,
    ports: &mut WifiTcpActorPorts,
    residue: T,
) -> ! {
    publish(status, bootstrap, WifiTcpPhase::Faulted);
    let _ = ports
        .lifecycle
        .request_state(ports.authority.lease(), InterfaceLifecycleState::Offline)
        .await;
    let _ = &residue;
    core::future::pending::<()>().await;
    unreachable!()
}

fn publish(status: &'static WifiTcpStatusCell, bootstrap: WifiTcpBootstrap, phase: WifiTcpPhase) {
    let snapshot = status.snapshot();
    status.publish(WifiTcpStatus::with_runtime_diagnostics(
        bootstrap,
        phase,
        None,
        snapshot.dns_diagnostics,
    ));
}

fn publish_connecting(status: &'static WifiTcpStatusCell, bootstrap: WifiTcpBootstrap) {
    let snapshot = status.snapshot();
    status.publish(WifiTcpStatus::with_runtime_diagnostics(
        bootstrap,
        WifiTcpPhase::Connecting,
        snapshot.last_failure,
        snapshot.dns_diagnostics,
    ));
}

fn publish_dns_diagnostics(
    status: &'static WifiTcpStatusCell,
    bootstrap: WifiTcpBootstrap,
    diagnostics: ReticulumDnsDiagnostics,
) {
    let snapshot = status.snapshot();
    status.publish(WifiTcpStatus::with_runtime_diagnostics(
        bootstrap,
        snapshot.phase,
        snapshot.last_failure,
        Some(diagnostics),
    ));
}

fn now() -> MonotonicMillis {
    MonotonicMillis::new(Instant::now().as_millis())
}

const fn resolution_failure(reason: RemoteResolutionError) -> WifiTcpFailure {
    match reason {
        RemoteResolutionError::Timeout => WifiTcpFailure::DnsTimeout,
        RemoteResolutionError::Lookup => WifiTcpFailure::DnsLookupFailed,
        RemoteResolutionError::NoIpv4Result => WifiTcpFailure::DnsNoIpv4Result,
    }
}

const fn connect_failure(reason: ConnectError) -> WifiTcpFailure {
    match reason {
        ConnectError::InvalidState => WifiTcpFailure::ConnectInvalidState,
        ConnectError::ConnectionReset => WifiTcpFailure::ConnectReset,
        ConnectError::TimedOut => WifiTcpFailure::ConnectTimeout,
        ConnectError::NoRoute => WifiTcpFailure::ConnectNoRoute,
    }
}

const fn connection_end_failure(reason: ConnectionEnd) -> Option<WifiTcpFailure> {
    match reason {
        ConnectionEnd::NetworkDown => None,
        ConnectionEnd::SocketClosed => Some(WifiTcpFailure::SocketClosed),
        ConnectionEnd::TransmitFailed => Some(WifiTcpFailure::TransmitFailed),
    }
}

const fn connect_error_label(reason: ConnectError) -> &'static str {
    match reason {
        ConnectError::InvalidState => "invalid-state",
        ConnectError::ConnectionReset => "connection-reset",
        ConnectError::TimedOut => "timed-out",
        ConnectError::NoRoute => "no-route",
    }
}

const _: () = assert!(SOCKET_TX_BYTES >= MAX_ENCODED);
const _: () = assert!(MAX_ENCODED == MTU * 2 + 2);
const _: () = assert!(STREAM_READ_BYTES <= SOCKET_RX_BYTES);
const _: () = assert!(mem::size_of::<WifiTcpIoStorage>() < 4 * 1_024);
