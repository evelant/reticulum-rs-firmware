//! E290 secure BLE onboarding and authenticated RDA1 device-API owner.
//!
//! This proof exposes one write-with-response RX characteristic, one
//! indication-only TX characteristic, and one public retained-link readiness
//! marker. RX and TX characteristic values are fragments of the existing
//! ordered RDA1 byte stream, not a second framing layer. The GPIO21 physical
//! presence opens a connection-bound DisplayOnly SMP window; only an
//! authenticated, durable bond may then carry the BLE-bound live pairing
//! protocol. A restored authenticated bond enters the ordinary RDA1 session
//! path without reopening physical pairing.

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
use esp_hal::{gpio::Input, peripherals::BT, rng::Trng};
use esp_radio::ble::controller::{BleConnector, BleInitError};
use log::{error, info, warn};
use reticulum_appliance_display_model::{
    DisplayCommand, DisplayHomeSnapshot, DisplaySetupState, DisplayViewKind, PairingPasskey,
    PairingSecretClearReason, PairingWindowSeconds,
};
use reticulum_ble_bond_store::BleBond;
use reticulum_device_api_ble as gatt_profile;
use reticulum_device_api_framing::{DecodeEvent, FramedRecord, Record, StreamDecoder};
use reticulum_device_api_handoff::{BearerHandoff, DeviceApiHandoff};
use reticulum_device_api_pairing::{ActivateResult, BearerBinding, PairingResponse};
use reticulum_device_api_pairing_control::ControlResponse;
use reticulum_device_api_pairing_policy::{
    ActiveLowButton, ConnectionId, MonotonicMillis as PairingMillis,
};
use reticulum_device_api_session::{
    AuthenticatedGrant, RECORD_KIND_CLIENT_HELLO, ServerParameters,
};
#[cfg(reticulum_e290_ble_startup_diagnostic)]
use reticulum_heltec_vision_master_e290_node::config as product_config;
use reticulum_heltec_vision_master_e290_node::{
    ble_api_profile::{
        self as profile, ApplicationPairingIdleDeadline, BleBondExchangeResult, IndicationGate,
        PreAuthenticationDeadline, PreAuthenticationDeadlineStatus,
    },
    ble_bond_handoff::{BearerBleBondHandoff, BleBondCommitCommand, BleBondCommitOutcome},
    display_handoff::{DisplayPublisher, DisplayRenderOutcome, DisplayRequestId},
    live_pairing_handoff::{BearerLivePairingHandoff, LivePairingCommand, LivePairingReply},
    pairing_control_handoff::{
        ButtonObservationFlight, ButtonObservationFlightProgress, ExclusiveAcquisitionReply,
        LifecycleAcknowledgement, PairingControlCommand, PairingControlReplyKind,
        UsbPairingHandoff,
    },
    session_admission_handoff::BearerSessionAdmissionHandoff,
    usb_authenticated_session::{
        PairingExclusiveCloseDisposition, UsbAuthenticatedSession, UsbAuthenticatedSessionPhase,
        UsbSessionRxDisposition,
    },
    usb_pairing_policy::{
        ActiveLowButtonDebouncer, ExactNextSequenceGate, PhysicalPresencePublicationGuard,
    },
    usb_pairing_records::{
        UsbPreAuthenticationRequest, UsbPreAuthenticationRequestKind,
        decode_pre_authentication_request,
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
const PAIRING_DISPLAY_WINDOW_SECONDS: u16 = 30;

#[cfg(reticulum_e290_ble_startup_diagnostic)]
macro_rules! pairing_diagnostic {
    ($($argument:tt)*) => {
        esp_println::println!($($argument)*)
    };
}

#[cfg(not(reticulum_e290_ble_startup_diagnostic))]
macro_rules! pairing_diagnostic {
    ($($argument:tt)*) => {};
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BleLinkMode {
    AwaitingPresence,
    SecurityRequested,
    PairingExclusive,
    Ordinary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairingDisplayState {
    Idle,
    Pending(DisplayRequestId),
    Rendered,
    Cleared,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndicationOwner {
    Pairing,
    Ordinary,
}

struct PairingTransmission {
    frame: FramedRecord,
    activation_succeeded: bool,
}

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
    #[characteristic(
        uuid = gatt_profile::RX_UUID_U128,
        write,
        permissions(write = authenticated)
    )]
    rx: GattFragment,
    /// Ordered device-to-phone RDA1 bytes.
    #[characteristic(uuid = gatt_profile::TX_UUID_U128, indicate)]
    tx: GattFragment,
    /// Public readiness marker changed only after authenticated bond durability.
    ///
    /// The read itself is intentionally public: polling `WAIT` must not start
    /// platform security before firmware observes GPIO21 and requests SMP.
    #[characteristic(
        uuid = gatt_profile::SECURITY_CONFIRMATION_UUID_U128,
        read,
        value = gatt_profile::SECURITY_CONFIRMATION_PENDING_VALUE
    )]
    security_confirmation: [u8; 4],
}

/// Sole bearer-side handoffs moved into the BLE API task.
pub(crate) struct BleHandoffs {
    pairing: UsbPairingHandoff<CriticalSectionRawMutex>,
    live_pairing: BearerLivePairingHandoff<CriticalSectionRawMutex>,
    bond: BearerBleBondHandoff<CriticalSectionRawMutex>,
    admission: BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
}

impl BleHandoffs {
    pub(crate) const fn new(
        pairing: UsbPairingHandoff<CriticalSectionRawMutex>,
        live_pairing: BearerLivePairingHandoff<CriticalSectionRawMutex>,
        bond: BearerBleBondHandoff<CriticalSectionRawMutex>,
        admission: BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
        authenticated_api: BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    ) -> Self {
        Self {
            pairing,
            live_pairing,
            bond,
            admission,
            authenticated_api,
        }
    }
}

/// Physical resources retained by secure BLE onboarding.
pub(crate) struct BlePhysicalOwners {
    pairing_button: Input<'static>,
    display_publisher: Option<DisplayPublisher<CriticalSectionRawMutex>>,
    display_home: DisplayHomeSnapshot,
    restored_bond: Option<BleBond>,
    bond_store_available: bool,
}

impl BlePhysicalOwners {
    pub(crate) fn new(
        pairing_button: Input<'static>,
        display_publisher: Option<DisplayPublisher<CriticalSectionRawMutex>>,
        display_home: DisplayHomeSnapshot,
        restored_bond: Option<BleBond>,
        bond_store_available: bool,
    ) -> Self {
        Self {
            pairing_button,
            display_publisher,
            display_home,
            restored_bond,
            bond_store_available,
        }
    }
}

struct BleConnectionOwners {
    pairing_button: Input<'static>,
    display_publisher: Option<DisplayPublisher<CriticalSectionRawMutex>>,
    display_home: DisplayHomeSnapshot,
}

/// Initialize the BLE controller before optional peripheral actors start.
///
/// The ESP32-S3 PHY calibration is order-sensitive in the powered product
/// graph. Keeping construction outside the task makes the modem/PHY ownership
/// boundary explicit and lets startup establish it before SPI3 display work.
pub(crate) fn initialize_controller(
    bluetooth: BT<'static>,
) -> Result<BleConnector<'static>, BleInitError> {
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
    let connector = BleConnector::new(bluetooth, controller_config);
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    match &connector {
        Ok(_) => {
            esp_println::println!(
                "e290-ble-startup-diagnostic stage=ble-controller-init RETURNED OK"
            );
            diagnostic_heap_marker("after-ble-controller-init");
        }
        Err(reason) => {
            esp_println::println!(
                "e290-ble-startup-diagnostic stage=ble-controller-init RETURNED ERR reason={reason:?}"
            );
        }
    }
    connector
}

/// Retain the complete opt-in BLE bearer after controller initialization.
///
/// Returned controller configuration errors are handled by the caller before
/// spawning this task. Host/GATT failures close only this API bearer without
/// stopping Reticulum routing. The pinned esp-radio controller still asserts
/// on some scheduler, allocation and vendor-init failures; making that boundary
/// fallible is tracked as a known defect and must not be described as isolated.
#[embassy_executor::task]
pub async fn run(
    connector: BleConnector<'static>,
    advertising: profile::BleAdvertisingParameters,
    handoffs: BleHandoffs,
    session_parameters: ServerParameters,
    session_rng: Trng,
    physical: BlePhysicalOwners,
    alpha_usb_serial_jtag_owner: crate::AlphaUsbSerialJtagOwner,
) {
    let controller: ExternalController<_, 1> = ExternalController::new(connector);
    let host_result = run_host(
        controller,
        advertising,
        handoffs,
        session_parameters,
        session_rng,
        physical,
    )
    .await;
    #[allow(
        clippy::drop_non_drop,
        reason = "the alpha diagnostics peripheral token is retained across the complete async host lifetime"
    )]
    core::mem::drop(alpha_usb_serial_jtag_owner);
    host_result
}

async fn run_host<C>(
    controller: C,
    advertising: profile::BleAdvertisingParameters,
    handoffs: BleHandoffs,
    session_parameters: ServerParameters,
    mut session_rng: Trng,
    physical: BlePhysicalOwners,
) where
    C: Controller,
{
    let resources = BLE_RESOURCES.init(HostResources::new());
    let address = Address::random(advertising.static_random_address());
    let BlePhysicalOwners {
        pairing_button,
        display_publisher,
        display_home,
        restored_bond,
        bond_store_available,
    } = physical;
    if !bond_store_available {
        error!("e290-node stage=ble-api status=DISABLED reason=bond-store-unavailable");
        core::future::pending::<()>().await;
    }
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    esp_println::println!("e290-ble-startup-diagnostic stage=trouble-host-new ENTER");
    let stack = trouble_host::new(controller, resources)
        .set_random_address(address)
        .set_random_generator_seed(&mut session_rng);
    stack.set_io_capabilities(IoCapabilities::DisplayOnly);
    let authoritative_bond_identity = restored_bond
        .as_ref()
        .map(|bond| trouble_bond(bond).identity);
    pairing_diagnostic!(
        "e290-ble-pairing-diagnostic event=boot-bond restored={}",
        restored_bond.is_some()
    );
    if let Some(bond) = restored_bond.as_ref()
        && stack.add_bond_information(trouble_bond(bond)).is_err()
    {
        error!("e290-node stage=ble-api status=DISABLED reason=restored-bond-rejected");
        core::future::pending::<()>().await;
    }
    drop(restored_bond);
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

    let local_name_bytes =
        BLE_LOCAL_NAME.init(gatt_profile::local_name(advertising.local_name_mac()));
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
            &stack,
            &mut peripheral,
            server,
            local_name.as_bytes(),
            handoffs,
            session_parameters,
            session_rng,
            BleConnectionOwners {
                pairing_button,
                display_publisher,
                display_home,
            },
            authoritative_bond_identity,
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

#[allow(
    clippy::too_many_arguments,
    reason = "the one advertiser retains each sole host, protocol, and physical owner explicitly"
)]
async fn serve_advertising<C: Controller>(
    stack: &Stack<'_, C, DefaultPacketPool>,
    peripheral: &mut Peripheral<'_, C, DefaultPacketPool>,
    server: &'static Server<'static>,
    local_name: &'static [u8],
    handoffs: BleHandoffs,
    session_parameters: ServerParameters,
    mut session_rng: Trng,
    physical: BleConnectionOwners,
    mut authoritative_bond_identity: Option<Identity>,
) {
    let BleConnectionOwners {
        pairing_button,
        mut display_publisher,
        mut display_home,
    } = physical;
    let BleHandoffs {
        pairing: mut pairing_handoff,
        live_pairing: mut live_pairing_handoff,
        bond: mut bond_handoff,
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
            stack,
            server,
            &gatt_connection,
            connection,
            &mut pairing_handoff,
            &mut live_pairing_handoff,
            &mut bond_handoff,
            &mut authenticated_session,
            &mut session_admission,
            &mut authenticated_api,
            &mut session_rng,
            &pairing_button,
            &mut display_publisher,
            &mut display_home,
            authoritative_bond_identity,
        )
        .await;
        let ServeConnectionOutcome {
            lifecycle_started,
            disconnected_event_observed,
            remove_volatile_bond,
            purge_non_authoritative_bonds,
            retain_only_bond,
            disable_bearer_until_reboot,
        } = connection_outcome;
        // Release logical session owners before the bounded controller drain.
        // The final GATT reference stays live through that drain, then Trouble
        // itself gates slot reuse on Disconnected state plus refcount zero.
        authenticated_session.reset();
        drain_connection(&gatt_connection, connection, disconnected_event_observed).await;
        // Trouble will not reuse the sole HostResources connection slot until
        // its state is Disconnected and the final Connection reference is gone.
        // Keep this drop visibly ahead of every path to the next advertiser.
        drop(gatt_connection);
        if let Some(identity) = remove_volatile_bond
            && stack.remove_bond_information(identity).is_err()
        {
            error!(
                "e290-node stage=ble-api status=DISABLED reason=volatile-bond-removal-fault connection={}",
                connection.get()
            );
            core::future::pending::<()>().await;
        }
        if purge_non_authoritative_bonds {
            for candidate in stack.get_bond_information() {
                let authoritative = authoritative_bond_identity
                    .is_some_and(|identity| identity.match_identity(&candidate.identity));
                if !authoritative && stack.remove_bond_information(candidate.identity).is_err() {
                    error!(
                        "e290-node stage=ble-api status=DISABLED reason=volatile-bond-scrub-fault connection={}",
                        connection.get()
                    );
                    core::future::pending::<()>().await;
                }
            }
            info!(
                "e290-node stage=ble-api status=PAIRING-RETRY-READY connection={} action=non-authoritative-bonds-scrubbed",
                connection.get()
            );
        }
        if let Some(identity) = retain_only_bond {
            for candidate in stack.get_bond_information() {
                if !candidate.identity.match_identity(&identity)
                    && stack.remove_bond_information(candidate.identity).is_err()
                {
                    error!(
                        "e290-node stage=ble-api status=DISABLED reason=stale-bond-removal-fault connection={}",
                        connection.get()
                    );
                    core::future::pending::<()>().await;
                }
            }
            authoritative_bond_identity = Some(identity);
        }
        info!(
            "e290-node stage=ble-api status=LINK-RESOURCES-RELEASED connection={}",
            connection.get()
        );
        #[cfg(reticulum_e290_ble_startup_diagnostic)]
        esp_println::println!(
            "e290-ble-startup-diagnostic stage=disconnect-drain status=RESOURCES-RELEASED connection={}",
            connection.get()
        );

        if lifecycle_started {
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
        if disable_bearer_until_reboot {
            pairing_diagnostic!(
                "e290-ble-pairing-diagnostic event=fail-stop reason=bond-commit-not-durable connection={}",
                connection.get()
            );
            error!(
                "e290-node stage=ble-api status=DISABLED reason=bond-commit-not-durable connection={} recovery=reboot-from-authoritative-bond",
                connection.get()
            );
            core::future::pending::<()>().await;
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
            warn!(
                "e290-node stage=ble-api status=DISCONNECT-DRAIN-INCONSISTENT reason=observed-event-but-manager-connected action=release-final-reference connection={}",
                connection_id.get()
            );
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
                // Disconnected into false. It is diagnostic only; the exact
                // Disconnected event remains the sole success gate.
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
    remove_volatile_bond: Option<Identity>,
    purge_non_authoritative_bonds: bool,
    retain_only_bond: Option<Identity>,
    disable_bearer_until_reboot: bool,
}

#[allow(
    clippy::too_many_arguments,
    reason = "one BLE link retains each exact session and handoff owner explicitly"
)]
async fn serve_connection<C: Controller, P: PacketPool>(
    stack: &Stack<'_, C, P>,
    server: &Server<'_>,
    connection: &GattConnection<'_, '_, P>,
    connection_id: ConnectionId,
    pairing_handoff: &mut UsbPairingHandoff<CriticalSectionRawMutex>,
    live_pairing_handoff: &mut BearerLivePairingHandoff<CriticalSectionRawMutex>,
    bond_handoff: &mut BearerBleBondHandoff<CriticalSectionRawMutex>,
    session: &mut UsbAuthenticatedSession,
    admission: &mut BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: &mut BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    session_rng: &mut Trng,
    pairing_button: &Input<'static>,
    display_publisher: &mut Option<DisplayPublisher<CriticalSectionRawMutex>>,
    display_home: &mut DisplayHomeSnapshot,
    mut authoritative_bond_identity: Option<Identity>,
) -> ServeConnectionOutcome {
    if server
        .set(
            &server.api.security_confirmation,
            &gatt_profile::SECURITY_CONFIRMATION_PENDING_VALUE,
        )
        .is_err()
    {
        warn!(
            "e290-node stage=ble-api status=LINK-RESET reason=security-confirmation-reset-fault connection={}",
            connection_id.get()
        );
        return ServeConnectionOutcome {
            lifecycle_started: false,
            disconnected_event_observed: false,
            remove_volatile_bond: None,
            purge_non_authoritative_bonds: false,
            retain_only_bond: None,
            disable_bearer_until_reboot: false,
        };
    }
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
        return ServeConnectionOutcome {
            lifecycle_started: false,
            disconnected_event_observed: false,
            remove_volatile_bond: None,
            purge_non_authoritative_bonds: false,
            retain_only_bond: None,
            disable_bearer_until_reboot: false,
        };
    }
    if !session.begin_connection(connection_id) {
        warn!(
            "e290-node stage=ble-api status=LINK-RESET reason=session-owner-not-disconnected connection={}",
            connection_id.get()
        );
        return ServeConnectionOutcome {
            lifecycle_started: true,
            disconnected_event_observed: false,
            remove_volatile_bond: None,
            purge_non_authoritative_bonds: false,
            retain_only_bond: None,
            disable_bearer_until_reboot: false,
        };
    }

    let rx = server.api.rx;
    let tx = server.api.tx;
    let tx_cccd = tx
        .cccd_handle
        .expect("the indication characteristic always owns a CCCD");
    let raw = connection.raw();
    let mut decoder = StreamDecoder::new();
    let mut sequence_gate = ExactNextSequenceGate::new(connection_id);
    let mut indication = IndicationGate::new();
    let mut indication_owner: Option<IndicationOwner> = None;
    let mut pairing_transmission: Option<PairingTransmission> = None;
    let linked_at = Instant::now();
    let mut indication_sent_at: Option<Instant> = None;
    let mut pre_authentication = PreAuthenticationDeadline::new();
    let mut pre_authentication_started_at: Option<Instant> = None;
    let mut cccd_enabled = false;
    let mut disconnected_event_observed = false;
    let mut remove_volatile_bond: Option<Identity> = None;
    let mut retain_only_bond: Option<Identity> = None;
    let mut fresh_security_pending_durability = false;
    let mut bond_reboot_required = false;
    let mut mode = BleLinkMode::AwaitingPresence;
    let mut pending_pairing_exclusive = false;
    let mut security_requested_at: Option<Instant> = None;
    let mut application_pairing_idle = ApplicationPairingIdleDeadline::new();
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    let mut application_rx_observed = false;
    let mut display_state = PairingDisplayState::Idle;
    let mut display_clear_reason: Option<PairingSecretClearReason> = None;
    let mut display_home_after_pairing = false;
    let started_at = pairing_time().get();
    let mut debouncer =
        ActiveLowButtonDebouncer::new(started_at, active_low_level(pairing_button.is_low()));
    pairing_diagnostic!(
        "e290-ble-pairing-diagnostic event=button-initial level={:?} connection={}",
        debouncer.current(),
        connection_id.get()
    );
    let mut button_publication = PhysicalPresencePublicationGuard::new();
    let mut button_observation = ButtonObservationFlight::new();
    let mut last_button_observation_ms =
        started_at.saturating_sub(crate::config::PAIRING_BUTTON_OBSERVATION_INTERVAL_MS);

    'connection: loop {
        if poll_pairing_display(display_publisher, &mut display_state).is_err() {
            warn!(
                "e290-node stage=ble-api status=LINK-RESET reason=pairing-display-fault connection={}",
                connection_id.get()
            );
            display_clear_reason = Some(PairingSecretClearReason::Failed);
            break;
        }

        match button_observation.poll(pairing_handoff) {
            Ok(progress) => {
                if apply_button_observation_progress(
                    progress,
                    connection_id,
                    &mut pending_pairing_exclusive,
                )
                .is_err()
                {
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=duplicate-pairing-exclusive-request connection={}",
                        connection_id.get()
                    );
                    break;
                }
            }
            Err(_) => {
                warn!(
                    "e290-node stage=ble-api status=LINK-RESET reason=button-reply-mismatch connection={}",
                    connection_id.get()
                );
                break;
            }
        }
        if button_observation.started_at().is_some_and(|started_at| {
            pairing_time().get().saturating_sub(started_at.get())
                >= profile::BUTTON_OBSERVATION_HANDOFF_TIMEOUT_MS
        }) {
            pairing_diagnostic!(
                "e290-ble-pairing-diagnostic event=button-handoff-timeout mode={:?} pending_exclusive={} connection={}",
                mode,
                pending_pairing_exclusive,
                connection_id.get()
            );
            warn!(
                "e290-node stage=ble-api status=LINK-RESET reason=button-handoff-timeout connection={}",
                connection_id.get()
            );
            break;
        }

        if mode == BleLinkMode::AwaitingPresence
            && raw
                .security_level()
                .is_ok_and(|security| security.authenticated())
        {
            if !matches_authoritative_identity(authoritative_bond_identity, raw.peer_identity()) {
                warn!(
                    "e290-node stage=ble-api status=LINK-RESET reason=authenticated-peer-not-authoritative connection={}",
                    connection_id.get()
                );
                break;
            }
            mode = BleLinkMode::Ordinary;
            pre_authentication = PreAuthenticationDeadline::new();
            pre_authentication_started_at = Some(Instant::now());
            info!(
                "e290-node stage=ble-api status=BONDED-LINK-AUTHENTICATED connection={} application_session=awaiting-client-hello",
                connection_id.get()
            );
        }

        let mut fault = false;

        if pending_pairing_exclusive {
            match session.close_for_pairing_exclusivity(connection_id) {
                PairingExclusiveCloseDisposition::Closed
                | PairingExclusiveCloseDisposition::AlreadyExclusive => {
                    pending_pairing_exclusive = false;
                    let exclusive = exchange_pairing_control(
                        pairing_handoff,
                        PairingControlCommand::ExclusiveAcquired {
                            at: pairing_time(),
                            connection: connection_id,
                        },
                        connection_id,
                    )
                    .await;
                    if !matches!(
                        exclusive,
                        Some(PairingControlReplyKind::Exclusive(
                            ExclusiveAcquisitionReply::Opened
                        ))
                    ) {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=pairing-window-not-opened connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                    pairing_diagnostic!(
                        "e290-ble-pairing-diagnostic event=exclusive-opened connection={}",
                        connection_id.get()
                    );
                    if raw
                        .security_level()
                        .is_ok_and(|security| security.authenticated())
                    {
                        if !matches_authoritative_identity(
                            authoritative_bond_identity,
                            raw.peer_identity(),
                        ) {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=authenticated-peer-not-authoritative connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                        pre_authentication = PreAuthenticationDeadline::new();
                        pre_authentication_started_at = None;
                        mode = BleLinkMode::PairingExclusive;
                        application_pairing_idle.ensure_started(Instant::now().as_millis());
                        if let Some(publisher) = display_publisher.as_mut()
                            && publisher
                                .publish_latest(DisplayCommand::ShowHome {
                                    snapshot: *display_home,
                                })
                                .is_err()
                        {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=restored-pairing-display-home-fault connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                        if server
                            .set(
                                &server.api.security_confirmation,
                                &gatt_profile::SECURITY_CONFIRMATION_READY_VALUE,
                            )
                            .is_err()
                        {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=security-confirmation-ready-fault connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                        info!(
                            "e290-node stage=ble-api status=PAIRING-WINDOW-OPEN connection={} security=restored-authenticated-bond application_pairing=ready",
                            connection_id.get()
                        );
                        continue;
                    }
                    if display_publisher.is_none() {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=pairing-display-unavailable connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                    if raw.set_bondable(true).is_err() {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=ble-bondable-configuration-fault connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                    pairing_diagnostic!(
                        "e290-ble-pairing-diagnostic event=bondable-enabled connection={}",
                        connection_id.get()
                    );
                    // From this point Trouble may install or replace volatile
                    // key material. A failure before the durable-store handoff
                    // can scrub all non-authoritative host bonds and advertise
                    // again; an attempted store commit remains reboot-only
                    // because its late outcome may be ambiguous.
                    fresh_security_pending_durability = true;
                    if raw.request_security().is_err() {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=ble-security-request-fault connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                    pairing_diagnostic!(
                        "e290-ble-pairing-diagnostic event=security-requested connection={}",
                        connection_id.get()
                    );
                    mode = BleLinkMode::SecurityRequested;
                    security_requested_at = Some(Instant::now());
                    info!(
                        "e290-node stage=ble-api status=PAIRING-WINDOW-OPEN connection={} security=display-passkey-requested",
                        connection_id.get()
                    );
                    continue;
                }
                PairingExclusiveCloseDisposition::DrainBeforeClose { kind } => {
                    let _ = kind;
                }
                PairingExclusiveCloseDisposition::StaleConnection => {
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=pairing-exclusive-stale-connection connection={}",
                        connection_id.get()
                    );
                    break;
                }
            }
        }

        if mode == BleLinkMode::Ordinary && !pending_pairing_exclusive {
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
        }
        if fault {
            warn!(
                "e290-node stage=ble-api status=LINK-RESET reason=session-reply-fault connection={}",
                connection_id.get()
            );
            break;
        }

        if let Some(requested_at) = security_requested_at
            && requested_at.elapsed()
                >= Duration::from_millis(profile::BLE_SECURITY_PAIRING_TIMEOUT_MS)
        {
            warn!(
                "e290-node stage=ble-api status=LINK-RESET reason=ble-security-timeout connection={}",
                connection_id.get()
            );
            display_clear_reason = Some(PairingSecretClearReason::TimedOut);
            break;
        }
        if application_pairing_idle.poll(Instant::now().as_millis(), pairing_transmission.is_some())
        {
            warn!(
                "e290-node stage=ble-api status=LINK-RESET reason=application-pairing-timeout connection={}",
                connection_id.get()
            );
            display_clear_reason = Some(PairingSecretClearReason::TimedOut);
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
        if !cccd_enabled
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

        if mode == BleLinkMode::Ordinary && !pending_pairing_exclusive {
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
        }

        if cccd_enabled && !indication.is_pending() {
            let next = if let Some(transmission) = pairing_transmission.as_ref() {
                Some((
                    transmission
                        .frame
                        .next_chunk(gatt_profile::INITIAL_ATT_VALUE_BYTES),
                    IndicationOwner::Pairing,
                ))
            } else if mode == BleLinkMode::Ordinary && session.tx_kind().is_some() {
                session
                    .next_tx_chunk(gatt_profile::INITIAL_ATT_VALUE_BYTES)
                    .map(|chunk| (chunk, IndicationOwner::Ordinary))
            } else {
                None
            };
            if let Some((chunk, owner)) = next {
                let Some(fragment) = GattFragment::copy_from(chunk) else {
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=invalid-session-tx-fragment connection={}",
                        connection_id.get()
                    );
                    break;
                };
                if indication.arm(chunk.len()).is_err() || indication_owner.replace(owner).is_some()
                {
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
        }

        match select(
            connection.next(),
            Timer::after_millis(profile::API_POLL_INTERVAL_MS),
        )
        .await
        {
            Either::Second(()) => {
                let now_millis = pairing_time().get();
                let observation = match debouncer
                    .observe(now_millis, active_low_level(pairing_button.is_low()))
                {
                    Ok(observation) => observation,
                    Err(_) => {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=button-debounce-fault connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                };
                button_publication.observe(observation);

                if matches!(mode, BleLinkMode::AwaitingPresence | BleLinkMode::Ordinary)
                    && !pending_pairing_exclusive
                    && !button_observation.is_pending()
                {
                    let periodic_due = now_millis.saturating_sub(last_button_observation_ms)
                        >= crate::config::PAIRING_BUTTON_OBSERVATION_INTERVAL_MS;
                    if button_publication.publication_due(periodic_due) {
                        let level = button_publication.policy_level(debouncer.current());
                        if !button_observation.try_schedule(
                            PairingMillis::new(now_millis),
                            connection_id,
                            level,
                        ) {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=button-flight-owner-mismatch connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                        last_button_observation_ms = now_millis;
                        button_publication.publication_queued();
                    }
                }
            }
            Either::First(GattConnectionEvent::Disconnected { reason }) => {
                disconnected_event_observed = true;
                info!(
                    "e290-node stage=ble-api status=LINK-DROPPED reason={reason:?} connection={}",
                    connection_id.get()
                );
                break;
            }
            Either::First(GattConnectionEvent::RequestConnectionParams(request)) => {
                if request.accept(None, stack).await.is_err() {
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=connection-params-response-fault connection={}",
                        connection_id.get()
                    );
                    break;
                }
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
                    cccd_enabled = true;
                    pairing_diagnostic!(
                        "e290-ble-pairing-diagnostic event=cccd-enabled connection={}",
                        connection_id.get()
                    );
                    info!(
                        "e290-node stage=ble-api status=LINK-READY connection={} cccd=indicate pairing=press-gpio21",
                        connection_id.get()
                    );
                }
                GattEvent::Write(write) if write.handle() == rx.handle => {
                    if mode == BleLinkMode::AwaitingPresence
                        && raw
                            .security_level()
                            .is_ok_and(|security| security.authenticated())
                    {
                        if !matches_authoritative_identity(
                            authoritative_bond_identity,
                            raw.peer_identity(),
                        ) {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=authenticated-peer-not-authoritative connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                        mode = BleLinkMode::Ordinary;
                        pre_authentication = PreAuthenticationDeadline::new();
                        pre_authentication_started_at = Some(Instant::now());
                    }
                    let data = write.data();
                    let mut bytes = [0_u8; gatt_profile::INITIAL_ATT_VALUE_BYTES];
                    let valid = cccd_enabled
                        && !data.is_empty()
                        && data.len() <= gatt_profile::INITIAL_ATT_VALUE_BYTES
                        && matches!(mode, BleLinkMode::PairingExclusive | BleLinkMode::Ordinary)
                        && !pending_pairing_exclusive
                        && pairing_transmission.is_none();
                    if valid {
                        bytes[..data.len()].copy_from_slice(data);
                    }
                    let data_len = data.len();
                    #[cfg(reticulum_e290_ble_startup_diagnostic)]
                    if valid && !application_rx_observed {
                        pairing_diagnostic!(
                            "e290-ble-pairing-diagnostic event=application-rx mode={:?} bytes={} security={:?} connection={}",
                            mode,
                            data_len,
                            raw.security_level(),
                            connection_id.get()
                        );
                        application_rx_observed = true;
                    }
                    let reply = if valid {
                        write.accept()
                    } else if cccd_enabled {
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
                        match mode {
                            BleLinkMode::PairingExclusive => match decode {
                                DecodeEvent::Pending => {}
                                DecodeEvent::Record(record) => {
                                    let Some(transmission) = handle_pairing_record(
                                        record,
                                        connection_id,
                                        &mut sequence_gate,
                                        pairing_handoff,
                                        live_pairing_handoff,
                                    )
                                    .await
                                    else {
                                        fault = true;
                                        break;
                                    };
                                    retain_activated_application_home(
                                        &transmission,
                                        display_home,
                                        &mut display_home_after_pairing,
                                    );
                                    pairing_transmission = Some(transmission);
                                }
                                DecodeEvent::MalformedCobs
                                | DecodeEvent::MalformedRecord(_)
                                | DecodeEvent::Overflow => {
                                    fault = true;
                                    break;
                                }
                            },
                            BleLinkMode::Ordinary => {
                                let disposition = match decode {
                                    DecodeEvent::Record(record)
                                        if session.phase()
                                            == UsbAuthenticatedSessionPhase::AwaitingClientHello
                                            && record.kind() != RECORD_KIND_CLIENT_HELLO =>
                                    {
                                        let Some(transmission) = handle_pairing_record(
                                            record,
                                            connection_id,
                                            &mut sequence_gate,
                                            pairing_handoff,
                                            live_pairing_handoff,
                                        )
                                        .await
                                        else {
                                            fault = true;
                                            break;
                                        };
                                        retain_activated_application_home(
                                            &transmission,
                                            display_home,
                                            &mut display_home_after_pairing,
                                        );
                                        pre_authentication_started_at = None;
                                        application_pairing_idle
                                            .ensure_started(Instant::now().as_millis());
                                        pairing_transmission = Some(transmission);
                                        break;
                                    }
                                    event => session.accept_decode_event(event, pairing_time()),
                                };
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
                                if matches!(
                                    disposition,
                                    Ok(UsbSessionRxDisposition::SessionEstablished)
                                ) {
                                    let Some(started_at) = pre_authentication_started_at else {
                                        warn!(
                                            "e290-node stage=ble-api status=LINK-RESET reason=missing-pre-authentication-owner connection={}",
                                            connection_id.get()
                                        );
                                        break 'connection;
                                    };
                                    let phase = session.phase();
                                    if pre_authentication
                                        .poll(started_at.elapsed().as_millis(), phase)
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
                            BleLinkMode::AwaitingPresence | BleLinkMode::SecurityRequested => {
                                fault = true;
                                break;
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
                        match indication_owner.take() {
                            Some(IndicationOwner::Ordinary) => {
                                if session.advance_tx(acknowledged).is_err() {
                                    warn!(
                                        "e290-node stage=ble-api status=LINK-RESET reason=session-tx-advance-fault connection={}",
                                        connection_id.get()
                                    );
                                    break;
                                }
                            }
                            Some(IndicationOwner::Pairing) => {
                                let Some(transmission) = pairing_transmission.as_mut() else {
                                    warn!(
                                        "e290-node stage=ble-api status=LINK-RESET reason=missing-pairing-tx-owner connection={}",
                                        connection_id.get()
                                    );
                                    break;
                                };
                                if transmission.frame.advance(acknowledged).is_err() {
                                    warn!(
                                        "e290-node stage=ble-api status=LINK-RESET reason=pairing-tx-advance-fault connection={}",
                                        connection_id.get()
                                    );
                                    break;
                                }
                                if transmission.frame.is_complete() {
                                    let activated = transmission.activation_succeeded;
                                    pairing_transmission = None;
                                    if activated {
                                        info!(
                                            "e290-node stage=ble-api status=APPLICATION-PAIRING-COMPLETE connection={} action=reconnect-authenticated",
                                            connection_id.get()
                                        );
                                        break 'connection;
                                    }
                                }
                            }
                            None => {
                                warn!(
                                    "e290-node stage=ble-api status=LINK-RESET reason=missing-indication-owner connection={}",
                                    connection_id.get()
                                );
                                break;
                            }
                        }
                    }
                }
                GattEvent::Read(read) => {
                    let security_confirmation =
                        read.handle() == server.api.security_confirmation.handle;
                    match read.accept() {
                        Ok(reply) => reply.send().await,
                        Err(reason) => {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=gatt-read-reply-{reason:?} connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                    }
                    if security_confirmation {
                        pairing_diagnostic!(
                            "e290-ble-pairing-diagnostic event=security-confirmation-read mode={:?} security={:?} connection={}",
                            mode,
                            raw.security_level(),
                            connection_id.get()
                        );
                    }
                }
                GattEvent::NotAllowed(not_allowed) => {
                    pairing_diagnostic!(
                        "e290-ble-pairing-diagnostic event=gatt-not-allowed target={} mode={:?} security={:?} connection={}",
                        if not_allowed.handle() == rx.handle {
                            "rx"
                        } else if not_allowed.handle() == tx_cccd {
                            "cccd"
                        } else if not_allowed.handle() == server.api.security_confirmation.handle {
                            "security-confirmation"
                        } else {
                            "other"
                        },
                        mode,
                        raw.security_level(),
                        connection_id.get()
                    );
                    match not_allowed.accept() {
                        Ok(reply) => reply.send().await,
                        Err(reason) => {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=gatt-reject-reply-{reason:?} connection={}",
                                connection_id.get()
                            );
                            break;
                        }
                    }
                }
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
            Either::First(GattConnectionEvent::PassKeyDisplay(passkey)) => {
                pairing_diagnostic!(
                    "e290-ble-pairing-diagnostic event=passkey-display mode={:?} display={:?} connection={}",
                    mode,
                    display_state,
                    connection_id.get()
                );
                if mode != BleLinkMode::SecurityRequested
                    || display_state != PairingDisplayState::Idle
                {
                    fresh_security_pending_durability = true;
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=unbound-passkey-display connection={}",
                        connection_id.get()
                    );
                    break;
                }
                let Some(publisher) = display_publisher.as_mut() else {
                    break;
                };
                let Ok(passkey) = PairingPasskey::from_number(passkey.value()) else {
                    break;
                };
                let Ok(expires_after_seconds) =
                    PairingWindowSeconds::new(PAIRING_DISPLAY_WINDOW_SECONDS)
                else {
                    break;
                };
                let request = publisher.publish_latest(DisplayCommand::ShowPairing {
                    label: display_home.label(),
                    passkey,
                    expires_after_seconds,
                });
                let Ok(request) = request else {
                    break;
                };
                display_state = PairingDisplayState::Pending(request);
                info!(
                    "e290-node stage=ble-api status=PAIRING-PASSKEY-QUEUED request={} connection={}",
                    request.sequence(),
                    connection_id.get()
                );
            }
            Either::First(GattConnectionEvent::PassKeyConfirm(_))
            | Either::First(GattConnectionEvent::PassKeyInput) => {
                fresh_security_pending_durability = true;
                warn!(
                    "e290-node stage=ble-api status=LINK-RESET reason=unexpected-security-method connection={}",
                    connection_id.get()
                );
                display_clear_reason = Some(PairingSecretClearReason::Failed);
                break;
            }
            Either::First(GattConnectionEvent::PairingComplete {
                security_level,
                bond,
            }) => {
                pairing_diagnostic!(
                    "e290-ble-pairing-diagnostic event=pairing-complete security={:?} bond_present={} mode={:?} display={:?} peer_match={} connection={}",
                    security_level,
                    bond.is_some(),
                    mode,
                    display_state,
                    bond.as_ref().is_some_and(|candidate| candidate
                        .identity
                        .match_identity(&raw.peer_identity())),
                    connection_id.get()
                );
                if mode != BleLinkMode::SecurityRequested {
                    fresh_security_pending_durability = true;
                    if let Some(provisional_bond) = bond {
                        remove_volatile_bond = Some(provisional_bond.identity);
                    }
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=unexpected-fresh-pairing-complete security_level={security_level:?} connection={}",
                        connection_id.get()
                    );
                    display_clear_reason = Some(PairingSecretClearReason::Failed);
                    break;
                }
                let Some(fresh_bond) = bond else {
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=missing-bond-on-pairing-complete connection={}",
                        connection_id.get()
                    );
                    display_clear_reason = Some(PairingSecretClearReason::Failed);
                    break;
                };
                if poll_pairing_display(display_publisher, &mut display_state).is_err()
                    || !security_level.authenticated()
                    || !matches!(
                        display_state,
                        PairingDisplayState::Pending(_) | PairingDisplayState::Rendered
                    )
                    || !fresh_bond.identity.match_identity(&raw.peer_identity())
                {
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=unauthenticated-or-unbound-pairing-complete connection={}",
                        connection_id.get()
                    );
                    display_clear_reason = Some(PairingSecretClearReason::Failed);
                    break;
                }
                let identity = fresh_bond.identity;
                let Some(portable_bond) = durable_bond(security_level, &fresh_bond) else {
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=invalid-authenticated-bond connection={}",
                        connection_id.get()
                    );
                    remove_volatile_bond = Some(identity);
                    display_clear_reason = Some(PairingSecretClearReason::Failed);
                    break;
                };
                let outcome = exchange_ble_bond(
                    bond_handoff,
                    BleBondCommitCommand::new(connection_id, portable_bond),
                    connection_id,
                )
                .await;
                match outcome {
                    BleBondExchangeResult::Durable => {
                        retain_only_bond = Some(identity);
                        authoritative_bond_identity = Some(identity);
                        fresh_security_pending_durability = false;
                        pairing_diagnostic!(
                            "e290-ble-pairing-diagnostic event=bond-durable connection={}",
                            connection_id.get()
                        );
                        mode = BleLinkMode::PairingExclusive;
                        security_requested_at = None;
                        application_pairing_idle.ensure_started(Instant::now().as_millis());
                        if server
                            .set(
                                &server.api.security_confirmation,
                                &gatt_profile::SECURITY_CONFIRMATION_READY_VALUE,
                            )
                            .is_err()
                        {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=security-confirmation-ready-fault connection={}",
                                connection_id.get()
                            );
                            display_clear_reason = Some(PairingSecretClearReason::Failed);
                            break;
                        }
                        info!(
                            "e290-node stage=ble-api status=BLE-PAIRING-DURABLE connection={} application_pairing=ready",
                            connection_id.get()
                        );
                    }
                    BleBondExchangeResult::ExplicitFailure
                    | BleBondExchangeResult::TimedOutBeforeSend
                    | BleBondExchangeResult::TimedOutAfterSend => {
                        bond_reboot_required = outcome.bond_reboot_required();
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=ble-bond-not-durable connection={}",
                            connection_id.get()
                        );
                        remove_volatile_bond = Some(identity);
                        display_clear_reason = Some(PairingSecretClearReason::Failed);
                        break;
                    }
                }
            }
            Either::First(GattConnectionEvent::PairingFailed(_reason)) => {
                #[cfg(reticulum_e290_ble_startup_diagnostic)]
                match &_reason {
                    Error::Security(reason) => pairing_diagnostic!(
                        "e290-ble-pairing-diagnostic event=pairing-failed error_class=security reason={:?} mode={:?} display={:?} connection={}",
                        reason,
                        mode,
                        display_state,
                        connection_id.get()
                    ),
                    _ => pairing_diagnostic!(
                        "e290-ble-pairing-diagnostic event=pairing-failed error_class=non-security mode={:?} display={:?} connection={}",
                        mode,
                        display_state,
                        connection_id.get()
                    ),
                }
                fresh_security_pending_durability = true;
                warn!(
                    "e290-node stage=ble-api status=LINK-RESET reason=ble-pairing-failed connection={}",
                    connection_id.get()
                );
                display_clear_reason = Some(PairingSecretClearReason::Failed);
                break;
            }
            Either::First(_) => {}
        }
    }

    indication.reset();
    pairing_diagnostic!(
        "e290-ble-pairing-diagnostic event=connection-outcome mode={:?} display={:?} fresh_security_pending={} bond_reboot_required={} remove_volatile_bond={} retain_only_bond={} connection={}",
        mode,
        display_state,
        fresh_security_pending_durability,
        bond_reboot_required,
        remove_volatile_bond.is_some(),
        retain_only_bond.is_some(),
        connection_id.get()
    );
    if display_home_after_pairing {
        if let Some(publisher) = display_publisher.as_mut()
            && publisher
                .publish_latest(DisplayCommand::ShowHome {
                    snapshot: *display_home,
                })
                .is_ok()
        {
            display_state = PairingDisplayState::Cleared;
        }
    } else if let Some(reason) = display_clear_reason.or_else(|| {
        matches!(
            display_state,
            PairingDisplayState::Pending(_) | PairingDisplayState::Rendered
        )
        .then_some(PairingSecretClearReason::Failed)
    }) && let Some(publisher) = display_publisher.as_mut()
        && publisher
            .publish_latest(DisplayCommand::ClearPairingSecret {
                label: display_home.label(),
                reason,
            })
            .is_ok()
    {
        display_state = PairingDisplayState::Cleared;
    }
    let _ = display_state;
    let fresh_security_disposition = profile::FreshSecurityDisposition::classify(
        fresh_security_pending_durability,
        bond_reboot_required,
    );
    ServeConnectionOutcome {
        lifecycle_started: true,
        disconnected_event_observed,
        remove_volatile_bond,
        purge_non_authoritative_bonds: fresh_security_disposition.scrub_non_authoritative_bonds(),
        retain_only_bond,
        disable_bearer_until_reboot: fresh_security_disposition.disable_until_reboot(),
    }
}

fn active_low_level(is_low: bool) -> ActiveLowButton {
    if is_low {
        ActiveLowButton::Low
    } else {
        ActiveLowButton::High
    }
}

fn apply_button_observation_progress(
    progress: ButtonObservationFlightProgress,
    connection: ConnectionId,
    pending_pairing_exclusive: &mut bool,
) -> Result<(), ()> {
    if progress != ButtonObservationFlightProgress::AcquireExclusive {
        return Ok(());
    }
    if *pending_pairing_exclusive {
        return Err(());
    }
    pairing_diagnostic!(
        "e290-ble-pairing-diagnostic event=acquire-exclusive connection={}",
        connection.get()
    );
    // A started ordinary indication owns its exact remaining bytes. The top
    // of the loop retries closure until that flight is confirmed, then and
    // only then acknowledges exclusivity.
    *pending_pairing_exclusive = true;
    info!(
        "e290-node stage=ble-api status=PAIRING-EXCLUSIVE-PENDING connection={} action=drain-ordinary-indication-before-ack",
        connection.get()
    );
    Ok(())
}

fn trouble_bond(bond: &BleBond) -> BondInformation {
    BondInformation::new(
        Identity {
            bd_addr: BdAddr::new(*bond.address()),
            irk: bond.irk().copied().map(IdentityResolvingKey::from_le_bytes),
        },
        LongTermKey::from_le_bytes(*bond.ltk()),
        SecurityLevel::EncryptedAuthenticated,
        true,
    )
}

fn durable_bond(event_security_level: SecurityLevel, bond: &BondInformation) -> Option<BleBond> {
    if event_security_level != SecurityLevel::EncryptedAuthenticated
        || bond.security_level != event_security_level
        || !bond.is_bonded
    {
        return None;
    }
    Some(BleBond::new(
        bond.identity.bd_addr.into_inner(),
        bond.identity.irk.map(IdentityResolvingKey::to_le_bytes),
        bond.ltk.to_le_bytes(),
    ))
}

fn matches_authoritative_identity(authoritative: Option<Identity>, observed: Identity) -> bool {
    authoritative.is_some_and(|identity| identity.match_identity(&observed))
}

fn poll_pairing_display(
    publisher: &mut Option<DisplayPublisher<CriticalSectionRawMutex>>,
    state: &mut PairingDisplayState,
) -> Result<(), ()> {
    let PairingDisplayState::Pending(expected) = *state else {
        return Ok(());
    };
    let Some(publisher) = publisher.as_mut() else {
        return Err(());
    };
    while let Some(completion) = publisher.try_take_completion() {
        if completion.request_id() < expected {
            continue;
        }
        if completion.request_id() != expected
            || completion.view() != DisplayViewKind::Pairing
            || completion.outcome() != DisplayRenderOutcome::Rendered
        {
            return Err(());
        }
        *state = PairingDisplayState::Rendered;
        pairing_diagnostic!(
            "e290-ble-pairing-diagnostic event=pairing-display-rendered request={}",
            expected.sequence()
        );
        return Ok(());
    }
    Ok(())
}

async fn exchange_pairing_control(
    handoff: &mut UsbPairingHandoff<CriticalSectionRawMutex>,
    command: PairingControlCommand,
    connection: ConnectionId,
) -> Option<PairingControlReplyKind> {
    let mut pending = Some(command);
    let started_at = Instant::now();
    loop {
        if let Some(command) = pending.take()
            && let Err(pressure) = handoff.try_send_command(command)
        {
            pending = Some(pressure.into_inner());
        }
        if pending.is_none() {
            while let Some(reply) = handoff.try_receive_reply() {
                if reply.connection() == connection {
                    return Some(reply.into_kind());
                }
            }
        }
        if started_at.elapsed() >= Duration::from_millis(profile::HANDOFF_EXCHANGE_TIMEOUT_MS) {
            return None;
        }
        Timer::after_millis(profile::API_POLL_INTERVAL_MS).await;
    }
}

async fn exchange_live_pairing(
    handoff: &mut BearerLivePairingHandoff<CriticalSectionRawMutex>,
    command: LivePairingCommand,
    connection: ConnectionId,
) -> Option<LivePairingReply> {
    let mut pending = Some(command);
    let started_at = Instant::now();
    loop {
        if let Some(command) = pending.take()
            && let Err(pressure) = handoff.try_send_command(command)
        {
            pending = Some(pressure.into_inner());
        }
        if pending.is_none() {
            while let Some(reply) = handoff.try_receive_reply() {
                if reply.connection() == connection {
                    return Some(reply);
                }
            }
        }
        if started_at.elapsed() >= Duration::from_millis(profile::HANDOFF_EXCHANGE_TIMEOUT_MS) {
            return None;
        }
        Timer::after_millis(profile::API_POLL_INTERVAL_MS).await;
    }
}

async fn exchange_ble_bond(
    handoff: &mut BearerBleBondHandoff<CriticalSectionRawMutex>,
    command: BleBondCommitCommand,
    connection: ConnectionId,
) -> BleBondExchangeResult {
    let mut pending = Some(command);
    let started_at = Instant::now();
    loop {
        if let Some(command) = pending.take()
            && let Err(pressure) = handoff.try_send_command(command)
        {
            pending = Some(pressure.into_inner());
        }
        if pending.is_none() {
            while let Some(reply) = handoff.try_receive_reply() {
                if reply.connection() == connection {
                    return match reply.outcome() {
                        BleBondCommitOutcome::Durable => BleBondExchangeResult::Durable,
                        BleBondCommitOutcome::Failed => BleBondExchangeResult::ExplicitFailure,
                    };
                }
            }
        }
        if started_at.elapsed() >= Duration::from_millis(profile::HANDOFF_EXCHANGE_TIMEOUT_MS) {
            // Only a command that crossed the handoff can still complete late.
            // Pure queue pressure retains the exact command here and remains
            // immediately retryable after this connection is scrubbed.
            return BleBondExchangeResult::timed_out(pending.is_none());
        }
        Timer::after_millis(profile::API_POLL_INTERVAL_MS).await;
    }
}

async fn handle_pairing_record(
    record: Record,
    connection: ConnectionId,
    sequence_gate: &mut ExactNextSequenceGate,
    control_handoff: &mut UsbPairingHandoff<CriticalSectionRawMutex>,
    live_handoff: &mut BearerLivePairingHandoff<CriticalSectionRawMutex>,
) -> Option<PairingTransmission> {
    let request = decode_pre_authentication_request(BearerBinding::BleGatt, record).ok()?;
    let sequence = request.sequence();
    let kind = request.kind();
    sequence_gate.accept(connection, sequence).ok()?;

    let (record, activation_succeeded) = match request {
        UsbPreAuthenticationRequest::Control(request) => {
            let reply = exchange_pairing_control(
                control_handoff,
                PairingControlCommand::Control {
                    at: pairing_time(),
                    connection,
                    request,
                },
                connection,
            )
            .await?;
            let PairingControlReplyKind::Control(response) = reply else {
                return None;
            };
            if response.sequence() != sequence || !matches_control_response(kind, &response) {
                return None;
            }
            (response.into_record(), false)
        }
        UsbPreAuthenticationRequest::Live(request) => {
            let reply = exchange_live_pairing(
                live_handoff,
                LivePairingCommand::new(
                    pairing_time(),
                    BearerBinding::BleGatt,
                    connection,
                    request,
                ),
                connection,
            )
            .await?;
            let response = reply.into_response();
            if response.sequence() != sequence || !kind.matches_live_response(&response) {
                return None;
            }
            let activation_succeeded = matches!(
                &response,
                PairingResponse::Activate(response)
                    if response.result() == ActivateResult::Activated
            );
            (response.into_record(), activation_succeeded)
        }
    };
    let frame = FramedRecord::encode(&record).ok()?;
    Some(PairingTransmission {
        frame,
        activation_succeeded,
    })
}

const fn matches_control_response(
    kind: UsbPreAuthenticationRequestKind,
    response: &ControlResponse,
) -> bool {
    matches!(
        (kind, response),
        (
            UsbPreAuthenticationRequestKind::Status,
            ControlResponse::Status { .. }
        ) | (
            UsbPreAuthenticationRequestKind::Initialize,
            ControlResponse::Initialize { .. }
        )
    )
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
    let started_at = Instant::now();
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
                match reply.into_kind() {
                    PairingControlReplyKind::Lifecycle(observed) => {
                        return observed == expected;
                    }
                    // A link may retire after its nonblocking button command
                    // crossed the handoff but before the reply was observed.
                    // The following Disconnected command closes any retained
                    // acquisition capability, so discard that now-stale
                    // scalar reply and continue to the exact lifecycle ack.
                    PairingControlReplyKind::Button(_) => continue,
                    PairingControlReplyKind::Exclusive(_) | PairingControlReplyKind::Control(_) => {
                        return false;
                    }
                }
            }
        }
        if started_at.elapsed() >= Duration::from_millis(profile::HANDOFF_EXCHANGE_TIMEOUT_MS) {
            return false;
        }
        Timer::after_millis(profile::API_POLL_INTERVAL_MS).await;
    }
}

fn retain_activated_application_home(
    transmission: &PairingTransmission,
    display_home: &mut DisplayHomeSnapshot,
    display_home_after_pairing: &mut bool,
) {
    // `activation_succeeded` comes from the node's durable Activated response.
    // ATT indication confirmation proves only client delivery, so it must not
    // govern the board's authoritative setup truth.
    if transmission.activation_succeeded {
        *display_home = (*display_home).with_setup(DisplaySetupState::Paired);
        *display_home_after_pairing = true;
    }
}

fn pairing_time() -> PairingMillis {
    PairingMillis::new(Instant::now().as_millis())
}

// Keep the complete handoff storage type in this module's graph audit. Only
// the split bearer role crosses into this task.
const _: usize =
    core::mem::size_of::<DeviceApiHandoff<CriticalSectionRawMutex, AuthenticatedGrant>>();
