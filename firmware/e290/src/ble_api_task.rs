//! E290 secure BLE onboarding and authenticated RDA1 device-API owner.
//!
//! This bearer exposes one write-with-response RX characteristic, one
//! indication-only TX characteristic, and one public retained-link readiness
//! marker. RX and TX characteristic values are fragments of the existing
//! ordered RDA1 byte stream, not a second framing layer. The GPIO21 physical
//! presence opens a connection-bound DisplayOnly SMP window; only an
//! authenticated, durable bond may then carry the BLE-bound live pairing
//! protocol. A restored authenticated bond enters the ordinary RDA1 session
//! path without reopening physical pairing.

use core::{num::NonZeroU64, str};

use ::embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use bt_hci::{cmd::le::LeReadLocalSupportedFeatures, controller::ControllerCmdSync};
use embassy_futures::{
    join::join,
    select::{Either, select},
};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::{gpio::Input, peripherals::BT, ram, rng::Trng, system::software_reset};
use esp_radio::ble::controller::{BleConnector, BleInitError};
use log::{error, info, warn};
use reticulum_appliance_display_model::{
    DisplayCommand, DisplayHomeSnapshot, DisplaySetupState, DisplayViewKind, PairingPasskey,
    PairingSecretClearReason, PairingWindowSeconds,
};
use reticulum_ble_bond_store::{BleAddressKind, BleBond};
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
use reticulum_e290_firmware::{
    authenticated_session::{
        AuthenticatedSession, AuthenticatedSessionPhase, PairingExclusiveCloseDisposition,
        SessionRxDisposition,
    },
    ble_api_profile::{
        self as profile, ApplicationPairingIdleDeadline, BleAdvertisingIdentityState,
        BleBondExchangeResult, IndicationGate, PreAuthenticationDeadline,
        PreAuthenticationDeadlineStatus,
    },
    ble_bond_handoff::{BearerBleBondHandoff, BleBondCommitCommand, BleBondCommitOutcome},
    display_handoff::{DisplayPublisher, DisplayRenderOutcome, DisplayRequestId},
    live_pairing_handoff::{BearerLivePairingHandoff, LivePairingCommand, LivePairingReply},
    pairing_control_handoff::{
        BearerPairingHandoff, ButtonObservationFlight, ButtonObservationFlightProgress,
        ExclusiveAcquisitionReply, LifecycleAcknowledgement, PairingControlCommand,
        PairingControlReplyKind,
    },
    pairing_policy::{
        ActiveLowButtonDebouncer, ExactNextSequenceGate, PhysicalPresencePublicationGuard,
    },
    pairing_records::{
        PreAuthenticationRequest, PreAuthenticationRequestKind, decode_pre_authentication_request,
    },
    session_admission_handoff::BearerSessionAdmissionHandoff,
};
use static_cell::StaticCell;
use trouble_host::{prelude::*, types::gatt_traits::AsGatt};

static BLE_RESOURCES: StaticCell<
    HostResources<DefaultPacketPool, { profile::CONNECTIONS_MAX }, { profile::L2CAP_CHANNELS_MAX }>,
> = StaticCell::new();
static BLE_LOCAL_NAME: StaticCell<[u8; gatt_profile::LOCAL_NAME_BYTES]> = StaticCell::new();
static BLE_RECOVERY_LOCAL_NAME: StaticCell<[u8; gatt_profile::RECOVERY_LOCAL_NAME_BYTES]> =
    StaticCell::new();
static BLE_SERVER: StaticCell<Server<'static>> = StaticCell::new();

// A nonzero marker survives software reset and rate-limits the only recovery
// path available when Trouble's private connection manager and the controller
// disagree. Zero is restored by a power-on reset. Treat torn writes as armed,
// so a reset during this update cannot create a reboot loop.
#[ram(unstable(rtc_fast, persistent))]
static mut BLE_RECOVERY_RESET_MARKER: [u32; 2] = [0; 2];

// These handles are part of the deployed BLE database ABI even though clients
// address characteristics by UUID. Bonded Apple platforms cache the resolved
// handles across reconnects, so moving them without a Service Changed path
// makes an otherwise valid firmware upgrade write to stale attributes.
const DEPLOYED_RX_HANDLE: u16 = 9;
const DEPLOYED_TX_HANDLE: u16 = 11;
const DEPLOYED_TX_CCCD_HANDLE: u16 = 12;
const DEPLOYED_SECURITY_CONFIRMATION_HANDLE: u16 = 14;

const SERVICE_UUIDS: [[u8; 16]; 1] = [gatt_profile::SERVICE_UUID_LE];
const PAIRING_DISPLAY_WINDOW_SECONDS: u16 = 30;

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

#[derive(Clone, Copy)]
struct GattFragment {
    bytes: [u8; gatt_profile::MAXIMUM_ATT_VALUE_BYTES],
    len: usize,
}

impl Default for GattFragment {
    fn default() -> Self {
        Self {
            bytes: [0; gatt_profile::MAXIMUM_ATT_VALUE_BYTES],
            len: 0,
        }
    }
}

impl GattFragment {
    fn copy_from(source: &[u8]) -> Option<Self> {
        if source.is_empty() || source.len() > gatt_profile::MAXIMUM_ATT_VALUE_BYTES {
            return None;
        }
        let mut fragment = Self::default();
        fragment.bytes[..source.len()].copy_from_slice(source);
        fragment.len = source.len();
        Some(fragment)
    }

    const fn len(&self) -> usize {
        self.len
    }
}

impl AsGatt for GattFragment {
    const MIN_SIZE: usize = 0;
    const MAX_SIZE: usize = gatt_profile::MAXIMUM_ATT_VALUE_BYTES;

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
    pairing: BearerPairingHandoff<CriticalSectionRawMutex>,
    live_pairing: BearerLivePairingHandoff<CriticalSectionRawMutex>,
    bond: BearerBleBondHandoff<CriticalSectionRawMutex>,
    admission: BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
}

impl BleHandoffs {
    pub(crate) const fn new(
        pairing: BearerPairingHandoff<CriticalSectionRawMutex>,
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
        // The pinned esp-radio controller writes this value directly to
        // Espressif's `ble_max_act`: it counts the advertiser and ACL link as distinct
        // controller activities, despite the API's connection-shaped name.
        .with_max_connections(profile::CONTROLLER_ACTIVITY_MAX as u8)
        .with_scan(false);
    BleConnector::new(bluetooth, controller_config)
}

/// Retain the complete BLE bearer after controller initialization.
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
    usb_serial_jtag_diagnostics_owner: crate::UsbSerialJtagDiagnosticsOwner,
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
        reason = "the USB Serial/JTAG diagnostics peripheral token is retained across the complete async host lifetime"
    )]
    core::mem::drop(usb_serial_jtag_diagnostics_owner);
    host_result
}

async fn run_host<C>(
    controller: C,
    advertising: profile::BleAdvertisingParameters,
    handoffs: BleHandoffs,
    session_parameters: ServerParameters,
    session_rng: Trng,
    physical: BlePhysicalOwners,
) where
    C: Controller + ControllerCmdSync<LeReadLocalSupportedFeatures>,
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
    let stack = trouble_host::new(controller, resources)
        .set_random_address(address)
        .set_io_capabilities(IoCapabilities::DisplayOnly)
        .build();
    let authoritative_bond_identity = restored_bond
        .as_ref()
        .map(|bond| trouble_bond(bond).identity);
    if let Some(bond) = restored_bond.as_ref()
        && stack.add_bond_information(trouble_bond(bond)).is_err()
    {
        error!("e290-node stage=ble-api status=DISABLED reason=restored-bond-rejected");
        core::future::pending::<()>().await;
    }
    drop(restored_bond);
    let runner = stack.runner();
    let mut peripheral = stack.peripheral();

    let local_name_bytes =
        BLE_LOCAL_NAME.init(gatt_profile::local_name(advertising.local_name_mac()));
    let local_name = match str::from_utf8(local_name_bytes) {
        Ok(name) => name,
        Err(_) => {
            error!("e290-node stage=ble-api status=DISABLED reason=local-name-encoding");
            core::future::pending().await
        }
    };
    let recovery_local_name_bytes = BLE_RECOVERY_LOCAL_NAME.init(
        gatt_profile::recovery_local_name(advertising.local_name_mac()),
    );
    let recovery_local_name = match str::from_utf8(recovery_local_name_bytes) {
        Ok(name) => name,
        Err(_) => {
            error!("e290-node stage=ble-api status=DISABLED reason=recovery-local-name-encoding");
            core::future::pending().await
        }
    };
    // Build the six-attribute peripheral GAP/GATT prefix explicitly. Trouble's
    // `central` compile configuration otherwise adds Central Address Resolution
    // to a peripheral GapConfig and shifts every product characteristic by two
    // handles, making the cached RX/TX handles resolve to read-only declarations.
    let mut table = AttributeTable::new();
    let mut gap = table.add_service(Service::new(service::GAP));
    gap.add_characteristic_ro(characteristic::DEVICE_NAME, local_name);
    gap.add_characteristic_ro(
        characteristic::APPEARANCE,
        &appearance::computer::GENERIC_COMPUTER,
    );
    gap.build();
    table.add_service(Service::new(service::GATT)).build();
    let server = BLE_SERVER.init(Server::new(table));
    let tx_cccd_handle = server
        .api
        .tx
        .cccd_handle()
        .map(|handle| handle.handle())
        .unwrap_or(0);
    let rx_write_permission = server
        .table()
        .permissions(server.api.rx.handle())
        .map(|permissions| permissions.write);
    let tx_cccd_write_permission = server
        .table()
        .permissions(tx_cccd_handle)
        .map(|permissions| permissions.write);
    let deployed_layout = server.api.rx.handle() == DEPLOYED_RX_HANDLE
        && server.api.tx.handle() == DEPLOYED_TX_HANDLE
        && tx_cccd_handle == DEPLOYED_TX_CCCD_HANDLE
        && server.api.security_confirmation.handle() == DEPLOYED_SECURITY_CONFIRMATION_HANDLE
        && rx_write_permission == Some(PermissionLevel::AuthenticationRequired)
        && tx_cccd_write_permission == Some(PermissionLevel::Allowed);
    if !deployed_layout {
        error!(
            "e290-node stage=ble-api status=DISABLED reason=gatt-database-layout rx={} tx={} tx_cccd={} security={} rx_write={:?} tx_cccd_write={:?}",
            server.api.rx.handle(),
            server.api.tx.handle(),
            tx_cccd_handle,
            server.api.security_confirmation.handle(),
            rx_write_permission,
            tx_cccd_write_permission,
        );
        core::future::pending().await
    }

    info!(
        "e290-node stage=ble-api status=READY address={} local_name={} recovery_local_name={} service={} profile={}.{} central_limit=1 att_fragment_min_bytes={} att_fragment_max_bytes={} tx=indicate rx=write-with-response pairing=display-passkey-physical-presence rx_handle={} tx_handle={} tx_cccd_handle={} security_handle={}",
        address,
        local_name,
        recovery_local_name,
        gatt_profile::SERVICE_UUID,
        gatt_profile::GATT_PROFILE_MAJOR,
        gatt_profile::GATT_PROFILE_MINOR,
        gatt_profile::MINIMUM_ATT_VALUE_BYTES,
        gatt_profile::MAXIMUM_ATT_VALUE_BYTES,
        server.api.rx.handle(),
        server.api.tx.handle(),
        tx_cccd_handle,
        server.api.security_confirmation.handle(),
    );

    join(
        run_controller(runner),
        serve_advertising(
            &stack,
            &mut peripheral,
            server,
            local_name.as_bytes(),
            recovery_local_name.as_bytes(),
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
    loop {
        match runner.run().await {
            Ok(()) => {}
            Err(reason) => {
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
async fn serve_advertising<C: Controller + ControllerCmdSync<LeReadLocalSupportedFeatures>>(
    stack: &Stack<'_, C, DefaultPacketPool>,
    peripheral: &mut Peripheral<'_, C, DefaultPacketPool>,
    server: &'static Server<'static>,
    local_name: &'static [u8],
    recovery_local_name: &'static [u8],
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
    let mut authenticated_session = AuthenticatedSession::new(session_parameters);
    let mut next_connection = NonZeroU64::MIN;
    let mut advertising_identity =
        BleAdvertisingIdentityState::from_restored_bond(authoritative_bond_identity.is_some());

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

        // A bondless board deliberately leaves the normal credential-derived
        // name unused. Saved profiles therefore cannot monopolize the sole
        // central slot with ordinary reconnect traffic while an operator is
        // trying to establish replacement platform security. The public
        // service UUID remains unchanged so first-run discovery still works.
        let advertised_name = if advertising_identity.uses_recovery_name() {
            recovery_local_name
        } else {
            local_name
        };
        let gatt_connection = match advertise(peripheral, server, advertised_name).await {
            Ok(gatt_connection) => gatt_connection,
            Err(reason) => {
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
            disconnect_observation,
            remove_volatile_bond,
            purge_non_authoritative_bonds,
            retain_only_bond,
            disable_bearer_until_reboot,
            ordinary_session_established,
            application_credential_activated,
        } = connection_outcome;
        // Release logical session owners before the bounded controller drain.
        // The final GATT reference stays live through that drain, then Trouble
        // itself gates slot reuse on Disconnected state plus refcount zero.
        authenticated_session.reset();
        let drain_result =
            drain_connection(&gatt_connection, connection, disconnect_observation).await;
        // Trouble will not reuse the sole HostResources connection slot until
        // its state is Disconnected and the final Connection reference is gone.
        // Keep this drop visibly ahead of every path to the next advertiser.
        drop(gatt_connection);
        let disable_ble_after_host_recovery = match drain_result {
            DisconnectDrainResult::Drained => false,
            DisconnectDrainResult::HostRecoveryRequired {
                manager_reports_connected,
            } => recover_ble_host_after_drain_timeout(connection, manager_reports_connected).await,
        };
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
            loop {
                let candidate = stack.with_bond_information(|bonds| {
                    bonds
                        .iter()
                        .find(|candidate| {
                            !authoritative_bond_identity.is_some_and(|identity| {
                                identity.match_identity(&candidate.identity)
                            })
                        })
                        .map(|candidate| candidate.identity)
                });
                let Some(candidate) = candidate else {
                    break;
                };
                if stack.remove_bond_information(candidate).is_err() {
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
            loop {
                let candidate = stack.with_bond_information(|bonds| {
                    bonds
                        .iter()
                        .find(|candidate| !candidate.identity.match_identity(&identity))
                        .map(|candidate| candidate.identity)
                });
                let Some(candidate) = candidate else {
                    break;
                };
                if stack.remove_bond_information(candidate).is_err() {
                    error!(
                        "e290-node stage=ble-api status=DISABLED reason=stale-bond-removal-fault connection={}",
                        connection.get()
                    );
                    core::future::pending::<()>().await;
                }
            }
            authoritative_bond_identity = Some(identity);
        }
        let previous_advertising_identity = advertising_identity;
        advertising_identity = advertising_identity.after_connection(
            authoritative_bond_identity.is_some(),
            ordinary_session_established,
            application_credential_activated,
        );
        if previous_advertising_identity != advertising_identity {
            info!(
                "e290-node stage=ble-api status=RECOVERY-COMPLETE proof={} action=advertise-normal-name-next",
                if ordinary_session_established {
                    "authenticated-session"
                } else {
                    "application-credential-activation"
                }
            );
        }
        info!(
            "e290-node stage=ble-api status=LINK-RESOURCES-RELEASED connection={}",
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
            error!(
                "e290-node stage=ble-api status=DISABLED reason=bond-commit-not-durable connection={} recovery=reboot-from-authoritative-bond",
                connection.get()
            );
            core::future::pending::<()>().await;
        }
        if disable_ble_after_host_recovery {
            error!(
                "e290-node stage=ble-api status=DISABLED reason=ble-host-recovery-loop-guard connection={} recovery=power-cycle",
                connection.get()
            );
            core::future::pending::<()>().await;
        }
    }
}

type DisconnectObservation = profile::BleDisconnectEvidence;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisconnectDrainResult {
    Drained,
    HostRecoveryRequired { manager_reports_connected: bool },
}

fn retained_ble_recovery_marker() -> profile::BleRecoveryResetMarkerState {
    // SAFETY: This task is the sole runtime owner of the marker. Volatile raw
    // access creates no reference to the mutable static and is required because
    // startup intentionally preserves this RTC region across software reset.
    let marker =
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(BLE_RECOVERY_RESET_MARKER)) };
    profile::classify_ble_recovery_reset_marker(marker)
}

fn arm_ble_recovery_reset_marker() {
    let marker = core::ptr::addr_of_mut!(BLE_RECOVERY_RESET_MARKER).cast::<u32>();
    // SAFETY: This task is the sole runtime writer. Write the nonzero checksum
    // first so interruption at either store is conservatively classified as a
    // previous recovery attempt rather than as a clean marker.
    unsafe {
        core::ptr::write_volatile(marker.add(1), profile::BLE_RECOVERY_RESET_MARKER_WORDS[1]);
        core::ptr::write_volatile(marker, profile::BLE_RECOVERY_RESET_MARKER_WORDS[0]);
    }
}

async fn recover_ble_host_after_drain_timeout(
    connection_id: ConnectionId,
    manager_reports_connected: bool,
) -> bool {
    let marker = retained_ble_recovery_marker();
    let boot_uptime_ms = Instant::now().as_millis();
    match profile::ble_host_recovery_disposition(marker.is_clean(), boot_uptime_ms) {
        profile::BleHostRecoveryDisposition::SoftwareReset => {
            arm_ble_recovery_reset_marker();
            error!(
                "e290-node stage=ble-api status=HOST-RECOVERY action=software-reset reason=disconnect-drain-timeout manager_reports_connected={} retained_marker={} boot_uptime_ms={} rearm_uptime_ms={} connection={}",
                manager_reports_connected,
                marker.label(),
                boot_uptime_ms,
                profile::BLE_RECOVERY_RESET_REARM_UPTIME_MS,
                connection_id.get()
            );
            // Preserve the terminal diagnostic on USB Serial/JTAG before the
            // ROM tears down the whole chip.
            Timer::after_millis(100).await;
            software_reset();
        }
        profile::BleHostRecoveryDisposition::DisableBleUntilPowerCycle => {
            error!(
                "e290-node stage=ble-api status=HOST-RECOVERY-SUPPRESSED reason=loop-guard manager_reports_connected={} retained_marker={} boot_uptime_ms={} rearm_uptime_ms={} action=finish-logical-disconnect-then-disable-ble connection={}",
                manager_reports_connected,
                marker.label(),
                boot_uptime_ms,
                profile::BLE_RECOVERY_RESET_REARM_UPTIME_MS,
                connection_id.get()
            );
            true
        }
    }
}

async fn drain_connection<P: PacketPool>(
    connection: &GattConnection<'_, '_, P>,
    connection_id: ConnectionId,
    disconnect_observation: DisconnectObservation,
) -> DisconnectDrainResult {
    let raw = connection.raw();

    // `ConnectionManager::disconnected` stores Disconnected before publishing
    // the event. An exact serve event or a manager-state fallback observed
    // before this task requests disconnect therefore proves completion when
    // the manager also reports false. Waiting for a duplicate event would
    // stall forever. Conversely, an event/state disagreement is not healthy
    // enough to release into the next advertiser.
    let connected_before_request = raw.is_connected();
    if profile::ble_disconnect_drain_entry_disposition(
        disconnect_observation,
        connected_before_request,
    ) == profile::BleDisconnectDrainEntryDisposition::Drained
    {
        let source = match disconnect_observation {
            DisconnectObservation::ServeEvent => "serve-event",
            DisconnectObservation::ManagerStateFallback => "manager-state-fallback",
            DisconnectObservation::None => "none",
        };
        info!(
            "e290-node stage=ble-api status=DISCONNECT-DRAINED source={} manager_reports_connected=false connection={}",
            source,
            connection_id.get()
        );
        return DisconnectDrainResult::Drained;
    }
    if disconnect_observation != DisconnectObservation::None && connected_before_request {
        warn!(
            "e290-node stage=ble-api status=DISCONNECT-DRAIN-INCONSISTENT observation={disconnect_observation:?} reason=disconnect-evidence-but-manager-connected action=request-and-bound-drain connection={}",
            connection_id.get()
        );
    }

    // This call is idempotent when the controller has already disconnected.
    // Do not use the subsequent `is_connected() == false` as completion:
    // the pinned Trouble reports false for its intermediate DisconnectRequest state.
    raw.disconnect();
    info!(
        "e290-node stage=ble-api status=DISCONNECT-DRAINING observation={disconnect_observation:?} connected_before_request={} timeout_ms={} connection={}",
        connected_before_request,
        profile::DISCONNECT_DRAIN_TIMEOUT_MS,
        connection_id.get()
    );

    let drain_started_at = Instant::now();
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
                return DisconnectDrainResult::Drained;
            }
            Either::First(_) => {}
            Either::Second(()) => {
                // Trouble's public predicate collapses DisconnectRequest and
                // Disconnected into false. It is diagnostic only; the exact
                // Disconnected event remains the sole success gate.
                let manager_reports_connected = raw.is_connected();
                if profile::ble_disconnect_drain_disposition(drain_started_at.elapsed().as_millis())
                    == profile::BleDisconnectDrainDisposition::RecoverHost
                {
                    error!(
                        "e290-node stage=ble-api status=DISCONNECT-DRAIN-TIMEOUT elapsed_ms={} manager_reports_connected={} action=bounded-host-recovery connection={}",
                        drain_started_at.elapsed().as_millis(),
                        manager_reports_connected,
                        connection_id.get()
                    );
                    return DisconnectDrainResult::HostRecoveryRequired {
                        manager_reports_connected,
                    };
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
            AdStructure::CompleteServiceUuids128(&SERVICE_UUIDS),
        ],
        &mut advertising_data,
    )?;
    let mut scan_data = [0_u8; 31];
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::CompleteLocalName(local_name)],
        &mut scan_data,
    )?;
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
        Ok(advertiser) => advertiser,
        Err(reason) => {
            return Err(reason);
        }
    };
    info!("e290-node stage=ble-api status=ADVERTISING");
    let connection = advertiser.accept().await?.with_attribute_server(server)?;
    let params = connection.raw().params();
    info!(
        "e290-node stage=ble-api status=LINK-CONNECTED session=pending-tx-cccd interval_us={} latency={} supervision_timeout_ms={} internal_free={}",
        params.conn_interval.as_micros(),
        params.peripheral_latency,
        params.supervision_timeout.as_millis(),
        esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into())
    );
    Ok(connection)
}

struct ServeConnectionOutcome {
    lifecycle_started: bool,
    disconnect_observation: DisconnectObservation,
    remove_volatile_bond: Option<Identity>,
    purge_non_authoritative_bonds: bool,
    retain_only_bond: Option<Identity>,
    disable_bearer_until_reboot: bool,
    ordinary_session_established: bool,
    application_credential_activated: bool,
}

fn reconcile_disconnect_observation(
    observation: DisconnectObservation,
    manager_reports_connected: bool,
    connection_id: ConnectionId,
    observation_point: &'static str,
) -> DisconnectObservation {
    if observation == DisconnectObservation::None
        && profile::ble_manager_state_disposition(manager_reports_connected)
            == profile::BleManagerStateDisposition::ReconcileDisconnect
    {
        warn!(
            "e290-node stage=ble-api status=LINK-DROPPED source=manager-state-fallback reason=disconnected-event-not-observed observation={} action=reconcile-and-release-disconnected-slot connection={}",
            observation_point,
            connection_id.get()
        );
        DisconnectObservation::ManagerStateFallback
    } else {
        observation
    }
}

async fn request_safe_connection_parameters<
    C: Controller + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
>(
    stack: &Stack<'_, C, P>,
    connection: &GattConnection<'_, '_, P>,
    connection_id: ConnectionId,
) {
    let raw = connection.raw();
    let current = raw.params();
    let derived = match profile::derive_ble_connection_update_parameters(
        current.conn_interval.as_micros(),
        current.peripheral_latency,
        current.supervision_timeout.as_millis(),
    ) {
        Ok(derived) => derived,
        Err(reason) => {
            warn!(
                "e290-node stage=ble-api status=CONNECTION-PARAMS-PROACTIVE-SKIPPED reason={reason:?} interval_us={} latency={} current_supervision_timeout_ms={} action=retain-central-parameters connection={}",
                current.conn_interval.as_micros(),
                current.peripheral_latency,
                current.supervision_timeout.as_millis(),
                connection_id.get()
            );
            return;
        }
    };
    if derived.supervision_timeout_ms() == current.supervision_timeout.as_millis() {
        info!(
            "e290-node stage=ble-api status=CONNECTION-PARAMS-PROACTIVE-SKIPPED reason=already-safe interval_us={} latency={} supervision_timeout_ms={} connection={}",
            derived.interval_us(),
            derived.latency(),
            derived.supervision_timeout_ms(),
            connection_id.get()
        );
        return;
    }
    let desired = RequestedConnParams {
        min_connection_interval: Duration::from_micros(derived.interval_us()),
        max_connection_interval: Duration::from_micros(derived.interval_us()),
        max_latency: derived.latency(),
        min_event_length: Duration::from_ticks(0),
        max_event_length: Duration::from_ticks(0),
        supervision_timeout: Duration::from_millis(derived.supervision_timeout_ms()),
    };
    info!(
        "e290-node stage=ble-api status=CONNECTION-PARAMS-PROACTIVE-REQUEST requested_interval_us={} requested_latency={} current_supervision_timeout_ms={} effective_supervision_timeout_ms={} connection={}",
        derived.interval_us(),
        derived.latency(),
        current.supervision_timeout.as_millis(),
        derived.supervision_timeout_ms(),
        connection_id.get()
    );
    match select(
        raw.update_connection_params(stack, &desired),
        Timer::after_millis(profile::PROACTIVE_CONNECTION_PARAMETER_REQUEST_TIMEOUT_MS),
    )
    .await
    {
        Either::First(Ok(())) => {
            info!(
                "e290-node stage=ble-api status=CONNECTION-PARAMS-PROACTIVE-SUBMITTED effective_interval_us={} effective_latency={} effective_supervision_timeout_ms={} completion=await-negotiated-event connection={}",
                derived.interval_us(),
                derived.latency(),
                derived.supervision_timeout_ms(),
                connection_id.get()
            );
        }
        Either::First(Err(reason)) => {
            warn!(
                "e290-node stage=ble-api status=CONNECTION-PARAMS-PROACTIVE-FAILED reason={reason:?} action=retain-central-parameters connection={}",
                connection_id.get()
            );
        }
        Either::Second(()) => {
            warn!(
                "e290-node stage=ble-api status=CONNECTION-PARAMS-PROACTIVE-TIMEOUT timeout_ms={} action=continue-gatt-with-central-parameters connection={}",
                profile::PROACTIVE_CONNECTION_PARAMETER_REQUEST_TIMEOUT_MS,
                connection_id.get()
            );
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one BLE link retains each exact session and handoff owner explicitly"
)]
async fn serve_connection<
    C: Controller + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
>(
    stack: &Stack<'_, C, P>,
    server: &Server<'_>,
    connection: &GattConnection<'_, '_, P>,
    connection_id: ConnectionId,
    pairing_handoff: &mut BearerPairingHandoff<CriticalSectionRawMutex>,
    live_pairing_handoff: &mut BearerLivePairingHandoff<CriticalSectionRawMutex>,
    bond_handoff: &mut BearerBleBondHandoff<CriticalSectionRawMutex>,
    session: &mut AuthenticatedSession,
    admission: &mut BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: &mut BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    session_rng: &mut Trng,
    pairing_button: &Input<'static>,
    display_publisher: &mut Option<DisplayPublisher<CriticalSectionRawMutex>>,
    display_home: &mut DisplayHomeSnapshot,
    mut authoritative_bond_identity: Option<Identity>,
) -> ServeConnectionOutcome {
    let raw = connection.raw();
    request_safe_connection_parameters(stack, connection, connection_id).await;
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
            disconnect_observation: reconcile_disconnect_observation(
                DisconnectObservation::None,
                raw.is_connected(),
                connection_id,
                "security-confirmation-reset",
            ),
            remove_volatile_bond: None,
            purge_non_authoritative_bonds: false,
            retain_only_bond: None,
            disable_bearer_until_reboot: false,
            ordinary_session_established: false,
            application_credential_activated: false,
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
            disconnect_observation: reconcile_disconnect_observation(
                DisconnectObservation::None,
                raw.is_connected(),
                connection_id,
                "connected-lifecycle",
            ),
            remove_volatile_bond: None,
            purge_non_authoritative_bonds: false,
            retain_only_bond: None,
            disable_bearer_until_reboot: false,
            ordinary_session_established: false,
            application_credential_activated: false,
        };
    }
    if !session.begin_connection(connection_id) {
        warn!(
            "e290-node stage=ble-api status=LINK-RESET reason=session-owner-not-disconnected connection={}",
            connection_id.get()
        );
        return ServeConnectionOutcome {
            lifecycle_started: true,
            disconnect_observation: reconcile_disconnect_observation(
                DisconnectObservation::None,
                raw.is_connected(),
                connection_id,
                "session-admission",
            ),
            remove_volatile_bond: None,
            purge_non_authoritative_bonds: false,
            retain_only_bond: None,
            disable_bearer_until_reboot: false,
            ordinary_session_established: false,
            application_credential_activated: false,
        };
    }

    let rx = server.api.rx;
    let tx = server.api.tx;
    let tx_cccd = tx
        .cccd_handle
        .expect("the indication characteristic always owns a CCCD");
    let mut decoder = StreamDecoder::new();
    let mut sequence_gate = ExactNextSequenceGate::new(connection_id);
    let mut indication = IndicationGate::new();
    let mut pairing_transmission: Option<PairingTransmission> = None;
    let mut ordinary_session_established = false;
    let linked_at = Instant::now();
    let mut pre_authentication = PreAuthenticationDeadline::new();
    let mut pre_authentication_started_at: Option<Instant> = None;
    let mut cccd_enabled = false;
    let mut disconnect_observation = DisconnectObservation::None;
    let mut remove_volatile_bond: Option<Identity> = None;
    let mut retain_only_bond: Option<Identity> = None;
    let mut fresh_security_pending_durability = false;
    let mut bond_reboot_required = false;
    let mut mode = BleLinkMode::AwaitingPresence;
    let mut pending_pairing_exclusive = false;
    let mut security_requested_at: Option<Instant> = None;
    let mut application_pairing_idle = ApplicationPairingIdleDeadline::new();
    let mut display_state = PairingDisplayState::Idle;
    let mut display_clear_reason: Option<PairingSecretClearReason> = None;
    let mut display_home_after_pairing = false;
    let mut display_home_after_bond_recovery = false;
    let started_at = pairing_time().get();
    let mut debouncer =
        ActiveLowButtonDebouncer::new(started_at, active_low_level(pairing_button.is_low()));
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
            warn!(
                "e290-node stage=ble-api status=LINK-RESET reason=button-handoff-timeout connection={}",
                connection_id.get()
            );
            break;
        }

        // Sample once per owner iteration, not only when the idle timer wins
        // the select below. Embassy's select is first-biased and a stream of
        // ready GATT events can otherwise recreate the timer indefinitely,
        // starving GPIO21 observation during the exact repair window in which
        // the app polls the security-confirmation characteristic.
        let now_millis = pairing_time().get();
        let raw_button_level = active_low_level(pairing_button.is_low());
        let observation = match debouncer.observe(now_millis, raw_button_level) {
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
                let scheduled = button_observation.try_schedule_and_poll(
                    pairing_handoff,
                    PairingMillis::new(now_millis),
                    connection_id,
                    level,
                );
                match scheduled {
                    Ok(Some(progress)) => {
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
                    Ok(None) => {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=button-flight-owner-mismatch connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                    Err(_) => {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=button-reply-mismatch connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                }
                last_button_observation_ms = now_millis;
                button_publication.publication_queued();
            }
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
                AuthenticatedSessionPhase::TerminatedUntilReset
            ) {
                warn!(
                    "e290-node stage=ble-api status=LINK-RESET reason=session-terminated connection={}",
                    connection_id.get()
                );
                break;
            }
        }

        if cccd_enabled && !indication.is_pending() {
            let maximum_fragment_bytes = profile::negotiated_att_value_bytes(raw.att_mtu());
            let next = if let Some(transmission) = pairing_transmission.as_ref() {
                Some((
                    transmission.frame.next_chunk(maximum_fragment_bytes),
                    IndicationOwner::Pairing,
                ))
            } else if mode == BleLinkMode::Ordinary && session.tx_kind().is_some() {
                session
                    .next_tx_chunk(maximum_fragment_bytes)
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
                if indication.arm(fragment.len()).is_err() {
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=ambiguous-indication-owner connection={}",
                        connection_id.get()
                    );
                    break;
                }
                match select(
                    tx.indicate(connection, &fragment, true),
                    Timer::after_millis(profile::INDICATION_CONFIRM_TIMEOUT_MS),
                )
                .await
                {
                    Either::First(Ok(())) => {
                        let Ok(acknowledged) = indication.confirm() else {
                            warn!(
                                "e290-node stage=ble-api status=LINK-RESET reason=unexpected-indication-confirmation connection={}",
                                connection_id.get()
                            );
                            break;
                        };
                        match owner {
                            IndicationOwner::Ordinary => {
                                if session.advance_tx(acknowledged).is_err() {
                                    warn!(
                                        "e290-node stage=ble-api status=LINK-RESET reason=session-tx-advance-fault connection={}",
                                        connection_id.get()
                                    );
                                    break;
                                }
                            }
                            IndicationOwner::Pairing => {
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
                        }
                    }
                    Either::First(Err(_)) => {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=indication-send-fault connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                    Either::Second(()) => {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=indication-confirm-timeout connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                }
            }
        }

        match select(
            connection.next(),
            Timer::after_millis(profile::API_POLL_INTERVAL_MS),
        )
        .await
        {
            Either::Second(()) => {
                if profile::ble_manager_state_disposition(raw.is_connected())
                    == profile::BleManagerStateDisposition::ReconcileDisconnect
                {
                    disconnect_observation = DisconnectObservation::ManagerStateFallback;
                    warn!(
                        "e290-node stage=ble-api status=LINK-DROPPED source=manager-state-fallback reason=disconnected-event-not-observed action=reconcile-and-release-disconnected-slot connection={}",
                        connection_id.get()
                    );
                    break;
                }
            }
            Either::First(GattConnectionEvent::Disconnected { reason }) => {
                disconnect_observation = DisconnectObservation::ServeEvent;
                info!(
                    "e290-node stage=ble-api status=LINK-DROPPED source=serve-event reason={reason:?} connection={}",
                    connection_id.get()
                );
                break;
            }
            Either::First(GattConnectionEvent::ConnectionParamsUpdated {
                conn_interval,
                peripheral_latency,
                supervision_timeout,
            }) => {
                info!(
                    "e290-node stage=ble-api status=CONNECTION-PARAMS-NEGOTIATED interval_us={} latency={} supervision_timeout_ms={} connection={}",
                    conn_interval.as_micros(),
                    peripheral_latency,
                    supervision_timeout.as_millis(),
                    connection_id.get()
                );
            }
            Either::First(GattConnectionEvent::RequestConnectionParams(request)) => {
                let requested = request.params().clone();
                let requested_timeout_ms = requested.supervision_timeout.as_millis();
                let applied_timeout_ms = match profile::derive_ble_connection_update_parameters(
                    requested.max_connection_interval.as_micros(),
                    requested.max_latency,
                    requested_timeout_ms,
                ) {
                    Ok(derived) => derived.supervision_timeout_ms(),
                    Err(reason) => {
                        warn!(
                            "e290-node stage=ble-api status=CONNECTION-PARAMS-POLICY-DEGRADED reason={reason:?} action=apply-timeout-floor-then-let-trouble-validate-complete-request connection={}",
                            connection_id.get()
                        );
                        profile::safe_supervision_timeout_ms(requested_timeout_ms)
                    }
                };
                let mut applied = requested.clone();
                applied.supervision_timeout = Duration::from_millis(applied_timeout_ms);
                info!(
                    "e290-node stage=ble-api status=CONNECTION-PARAMS-REQUEST min_interval_us={} max_interval_us={} latency={} min_event_length_us={} max_event_length_us={} requested_supervision_timeout_ms={} applied_supervision_timeout_ms={} clamped={} connection={}",
                    requested.min_connection_interval.as_micros(),
                    requested.max_connection_interval.as_micros(),
                    requested.max_latency,
                    requested.min_event_length.as_micros(),
                    requested.max_event_length.as_micros(),
                    requested_timeout_ms,
                    applied_timeout_ms,
                    requested_timeout_ms != applied_timeout_ms,
                    connection_id.get()
                );
                if request.accept(Some(&applied), stack).await.is_err() {
                    warn!(
                        "e290-node stage=ble-api status=LINK-RESET reason=connection-params-response-fault connection={}",
                        connection_id.get()
                    );
                    break;
                }
                info!(
                    "e290-node stage=ble-api status=CONNECTION-PARAMS-ACCEPTED applied_supervision_timeout_ms={} connection={}",
                    applied_timeout_ms,
                    connection_id.get()
                );
            }
            Either::First(GattConnectionEvent::Gatt { event }) => match event {
                GattEvent::Write(write) if write.handle() == tx_cccd => {
                    let enabled =
                        write.with_data(|offset, data| offset == 0 && data == [0x02, 0x00]);
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
                    info!(
                        "e290-node stage=ble-api status=LINK-READY connection={} cccd=indicate pairing=press-gpio21 att_mtu={} att_payload_bytes={}",
                        connection_id.get(),
                        raw.att_mtu(),
                        profile::negotiated_att_value_bytes(raw.att_mtu())
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
                    let maximum_fragment_bytes = profile::negotiated_att_value_bytes(raw.att_mtu());
                    let mut bytes = [0_u8; gatt_profile::MAXIMUM_ATT_VALUE_BYTES];
                    let (valid, data_len) = write.with_data(|offset, data| {
                        let valid = offset == 0
                            && cccd_enabled
                            && !data.is_empty()
                            && data.len() <= maximum_fragment_bytes
                            && matches!(
                                mode,
                                BleLinkMode::PairingExclusive | BleLinkMode::Ordinary
                            )
                            && !pending_pairing_exclusive
                            && pairing_transmission.is_none();
                        if valid {
                            bytes[..data.len()].copy_from_slice(data);
                        }
                        (valid, data.len())
                    });
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
                                            == AuthenticatedSessionPhase::AwaitingClientHello
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
                                    Ok(SessionRxDisposition::Pending)
                                        | Ok(SessionRxDisposition::ClientHelloAccepted)
                                        | Ok(SessionRxDisposition::ClientHelloDroppedBusy)
                                        | Ok(SessionRxDisposition::SessionEstablished)
                                        | Ok(SessionRxDisposition::RequestAuthenticated)
                                ) {
                                    fault = true;
                                    break;
                                }
                                if matches!(
                                    disposition,
                                    Ok(SessionRxDisposition::SessionEstablished)
                                ) {
                                    if mode == BleLinkMode::Ordinary {
                                        ordinary_session_established = true;
                                        info!(
                                            "e290-node stage=ble-api status=APPLICATION-SESSION-READY connection={} bearer=authenticated-gatt internal_free={}",
                                            connection_id.get(),
                                            esp_alloc::HEAP.free_caps(
                                                esp_alloc::MemoryCapability::Internal.into()
                                            )
                                        );
                                    }
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
                GattEvent::Other(other) => match other.accept() {
                    Ok(reply) => reply.send().await,
                    Err(reason) => {
                        warn!(
                            "e290-node stage=ble-api status=LINK-RESET reason=gatt-other-reply-{reason:?} connection={}",
                            connection_id.get()
                        );
                        break;
                    }
                },
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
                GattEvent::NotAllowed(not_allowed) => {
                    let rejected_handle = not_allowed.handle();
                    let target = if rejected_handle == rx.handle {
                        "rx"
                    } else if rejected_handle == tx_cccd {
                        "cccd"
                    } else if rejected_handle == server.api.security_confirmation.handle {
                        "security-confirmation"
                    } else {
                        "other"
                    };
                    warn!(
                        "e290-node stage=ble-api status=GATT-NOT-ALLOWED requested_handle={} target={} current_rx_handle={} current_tx_cccd_handle={} current_security_handle={} mode={:?} security={:?} connection={}",
                        rejected_handle,
                        target,
                        rx.handle,
                        tx_cccd,
                        server.api.security_confirmation.handle,
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
                        if display_home.setup() == DisplaySetupState::BluetoothRecoveryRequired {
                            *display_home = (*display_home).with_setup(DisplaySetupState::Paired);
                            display_home_after_bond_recovery = true;
                        }
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
    if display_home_after_pairing || display_home_after_bond_recovery {
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
    // A disconnect can occur while this task is awaiting a bounded handoff or
    // indication confirmation rather than while it is polling GATT events.
    // Reconcile once more before returning so a dropped terminal event still
    // carries authoritative pre-drain manager evidence.
    disconnect_observation = reconcile_disconnect_observation(
        disconnect_observation,
        raw.is_connected(),
        connection_id,
        "serve-exit",
    );
    ServeConnectionOutcome {
        lifecycle_started: true,
        disconnect_observation,
        remove_volatile_bond,
        purge_non_authoritative_bonds: fresh_security_disposition.scrub_non_authoritative_bonds(),
        retain_only_bond,
        disable_bearer_until_reboot: fresh_security_disposition.disable_until_reboot(),
        ordinary_session_established,
        application_credential_activated: display_home_after_pairing,
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
            addr: Address::new(
                match bond.address_kind() {
                    BleAddressKind::Public => AddrKind::PUBLIC,
                    BleAddressKind::Random => AddrKind::RANDOM,
                },
                BdAddr::new(*bond.address()),
            ),
            irk: bond
                .irk()
                .copied()
                .and_then(IdentityResolvingKey::from_le_bytes),
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
    let address_kind = match bond.identity.addr.kind.as_raw() {
        0 | 2 => BleAddressKind::Public,
        1 | 3 => BleAddressKind::Random,
        _ => return None,
    };
    Some(BleBond::new(
        address_kind,
        bond.identity.addr.addr.into_inner(),
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
        return Ok(());
    }
    Ok(())
}

async fn exchange_pairing_control(
    handoff: &mut BearerPairingHandoff<CriticalSectionRawMutex>,
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
    control_handoff: &mut BearerPairingHandoff<CriticalSectionRawMutex>,
    live_handoff: &mut BearerLivePairingHandoff<CriticalSectionRawMutex>,
) -> Option<PairingTransmission> {
    let request = decode_pre_authentication_request(BearerBinding::BleGatt, record).ok()?;
    let sequence = request.sequence();
    let kind = request.kind();
    sequence_gate.accept(connection, sequence).ok()?;

    let (record, activation_succeeded) = match request {
        PreAuthenticationRequest::Control(request) => {
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
        PreAuthenticationRequest::Live(request) => {
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
    kind: PreAuthenticationRequestKind,
    response: &ControlResponse,
) -> bool {
    matches!(
        (kind, response),
        (
            PreAuthenticationRequestKind::Status,
            ControlResponse::Status { .. }
        ) | (
            PreAuthenticationRequestKind::Initialize,
            ControlResponse::Initialize { .. }
        )
    )
}

fn drain_stale_replies(
    session: &mut AuthenticatedSession,
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
    handoff: &mut BearerPairingHandoff<CriticalSectionRawMutex>,
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
