//! Product-owned lifecycle for a native PRNS client node.
//!
//! This module composes unmodified PRNS public APIs. PRNS owns Reticulum
//! protocol state, persistence, and Bluetooth Auto. Product code owns only the
//! app-private storage location and the lifecycle exposed to Expo.

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use log::{LevelFilter, Log, Metadata, Record};
use personal_rns::engine::{EstablishLinkFailure, EstablishLinkRejection, RatchetPolicy};
use personal_rns::identity::in_memory::InMemoryNodeIdentity;
use personal_rns::identity::{IDENTITY_SECRET_KEY_LEN, IdentityHash, IdentitySigner, Zeroizing};
use personal_rns::routing::announce::{AnnounceObservation, derive_destination_hash, expand_name};
use personal_rns::routing::links::LinkId;
use personal_rns::routing::links::resources::ResourceStrategy;
use personal_rns::routing::request_handlers::RequestPathHash;
use personal_rns::routing::{LinkRequestPolicy, ProofStrategy};
use personal_rns::runtime::{
    Diagnostic, ManuallyAttached, NodePersistence, PreConfiguredDestination, PrnsEvent, PrnsNode,
    PrnsNodeHandle, PrnsNodeRecipe, SegmentCompression, SendError, ServeMyRequestEndpoints,
    load_or_create_ble_identity, load_or_create_identity_secret,
};
use personal_rns::storage::GrowableHeap;
use personal_rns::wire::DestinationHash;
use reticulum_appliance_runtime::{MAX_NEARBY_PEERS, NearbyPeerObservation, NearbyPeerView};
use reticulum_device_api::{
    ApiVersion, DeviceRequest, DeviceResponse, ESP_IMAGE_MAGIC, IdentitySummary,
    MANAGEMENT_APPLICATION_NAME, MANAGEMENT_ENROLLMENT_PATH, MANAGEMENT_ENROLLMENT_REQUEST_VALUE,
    MANAGEMENT_ENROLLMENT_SUCCESS_RESPONSE, MANAGEMENT_PUBLIC_PATH, MANAGEMENT_REQUEST_PATH,
    MAX_MESSAGE_BYTES, OTA_IMAGE_CHUNK_BYTES, OTA_NEXT_PATH, OTA_REBOOT_PATH,
    OTA_REBOOT_REQUEST_VALUE, OTA_START_PATH, OTA_STATUS_PATH, OTA_STATUS_REQUEST_VALUE,
    OtaChunkMetadata, OtaFailure, OtaManifest, OtaPhase, OtaSessionId, OtaSlot, OtaStatus,
    OtaVersion, RequestEnvelope, RequestId, decode_response, decode_status_response,
    encode_next_request, encode_request, encode_start_request,
};
use sha2::{Digest, Sha256};

#[cfg(any(target_os = "ios", target_os = "macos"))]
use personal_rns::bluetooth_auto::AutoBle;
#[cfg(target_os = "android")]
use personal_rns::bluetooth_auto::BluetoothAuto;
#[cfg(target_os = "android")]
use personal_rns::interfaces::bluetooth_auto::{
    AndroidHost, BLE_HW_MTU, Endpoint, LinkCapabilities,
};
#[cfg(target_os = "android")]
use prns_ffi::bluetooth_auto::android::{AndroidBleBackend, AndroidBleBridge};

const APP_IDENTITY_FILE: &str = "identity";
const BLE_IDENTITY_FILE: &str = "bluetooth-auto-identity";
const PRNS_PERSISTENCE_DIRECTORY: &str = "runtime";
const MOBILE_PRNS_DIRECTORY: &str = "prns";
const CLIENT_APP_NAME: &str = MANAGEMENT_APPLICATION_NAME;
const CLIENT_ASPECTS: &[&str] = &["appliance", "client"];
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const MANAGEMENT_CANDIDATE_CAPACITY: usize = 32;
const OTA_PROGRESS_POLL_INTERVAL: Duration = Duration::from_millis(25);
const OTA_PROGRESS_POLL_ATTEMPTS: usize = 400;

static NODE_CLAIMED: AtomicBool = AtomicBool::new(false);
static PRNS_DIAGNOSTIC_LOGGER: PrnsDiagnosticLogger = PrnsDiagnosticLogger;

struct PrnsDiagnosticLogger;

impl Log for PrnsDiagnosticLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target().starts_with("personal_rns")
            || metadata.target().starts_with("prns_")
            || metadata.target().starts_with("reticulum_appliance_native")
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "appliance-prns {} {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

fn requested_diagnostic_level() -> Option<LevelFilter> {
    let requested = std::env::var("RETICULUM_PRNS_LOG").ok()?;
    match requested.trim().to_ascii_lowercase().as_str() {
        "error" => Some(LevelFilter::Error),
        "warn" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

fn initialize_prns_diagnostics() {
    let Some(level) = requested_diagnostic_level() else {
        return;
    };
    if log::set_logger(&PRNS_DIAGNOSTIC_LOGGER).is_ok() {
        log::set_max_level(level);
    }
}

/// Lifecycle state of the one native PRNS node owned by this process.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum NativePrnsNodeState {
    /// The worker is loading identities and PRNS persistence.
    Starting,
    /// PRNS owns the live Reticulum node and Bluetooth Auto interface.
    Running,
    /// The node stopped because startup, persistence, or runtime work failed.
    Failed,
    /// The owner completed its final PRNS persistence flush and stopped.
    Stopped,
}

impl NativePrnsNodeState {
    const fn code(self) -> u8 {
        match self {
            Self::Starting => 0,
            Self::Running => 1,
            Self::Failed => 2,
            Self::Stopped => 3,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Starting,
            1 => Self::Running,
            2 => Self::Failed,
            _ => Self::Stopped,
        }
    }
}

/// Secret-free facts about the app's PRNS node.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativePrnsNodeSnapshot {
    /// Current lifecycle state.
    pub state: NativePrnsNodeState,
    /// App-owned Reticulum identity hash as lowercase hexadecimal.
    pub identity_hash: Option<String>,
    /// A local Single destination that holds the app identity in PRNS.
    pub client_destination: Option<String>,
    /// Stable PRNS Bluetooth Auto identity as lowercase hexadecimal.
    pub bluetooth_identity: Option<String>,
    /// Last lifecycle failure without filesystem paths or secret material.
    pub failure: Option<String>,
}

/// One verified announce to probe through the public management request path.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativePrnsManagementCandidate {
    /// Announced destination hash as lowercase hexadecimal.
    pub destination_hash: String,
    /// Complete PRNS interface identity on which the announce arrived.
    pub interface_id: String,
    /// Reticulum hop count carried by the accepted announce.
    pub hops: u8,
}

/// Public destination facts returned through the management application.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativePrnsManagementIdentity {
    /// Primary management destination confirmed by the appliance.
    pub management_destination: String,
    /// Local LXMF delivery destination when the app is active.
    pub lxmf_destination: Option<String>,
}

/// Public phase of one product-owned OTA transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum NativePrnsOtaPhase {
    /// The board has not started an update in this boot.
    Idle,
    /// The board is receiving and verifying ordered image chunks.
    Receiving,
    /// The verified inactive slot is selected for the next boot.
    Activated,
    /// The update ended without selecting a new boot slot.
    Failed,
}

/// ESP boot slot selected by the board OTA coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum NativePrnsOtaSlot {
    /// ESP-IDF `ota_0` application partition.
    Ota0,
    /// ESP-IDF `ota_1` application partition.
    Ota1,
}

/// Secret-free board projection returned during an OTA transfer.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativePrnsOtaStatus {
    /// Current transfer phase.
    pub phase: NativePrnsOtaPhase,
    /// Boot-scoped transfer identity as lowercase hexadecimal.
    pub session: Option<String>,
    /// Selected inactive slot, when known.
    pub slot: Option<NativePrnsOtaSlot>,
    /// Validated target release label.
    pub version: Option<String>,
    /// Complete manifest byte count.
    pub image_bytes: u32,
    /// Bytes written and verified by the board.
    pub verified_bytes: u32,
    /// Zero-based application chunk expected next.
    pub next_chunk: u32,
    /// Whether the board has admitted exactly that next Resource.
    pub resource_armed: bool,
    /// Stable terminal failure label, when the transfer failed.
    pub failure: Option<String>,
}

/// Failure to start or stop the mobile PRNS node.
#[derive(Debug, Eq, PartialEq, uniffi::Error)]
#[allow(missing_docs)]
pub enum NativePrnsNodeError {
    InvalidStorage { reason: String },
    AlreadyRunning,
    WorkerSpawn,
    StartupTimeout,
    Startup { reason: String },
    Stopped { reason: String },
}

/// Failure of one typed appliance-management operation over PRNS.
#[derive(Debug, Eq, PartialEq, uniffi::Error)]
#[allow(missing_docs)]
pub enum NativePrnsManagementError {
    NodeNotRunning,
    InvalidDestination { reason: String },
    Path { reason: String },
    Link { reason: String },
    Identify { reason: String },
    RequestEncoding,
    Request { reason: String },
    Response { reason: String },
    UnexpectedResponse,
    EnrollmentRejected,
}

/// Failure of one complete OTA staging operation over an identified PRNS Link.
#[derive(Debug, Eq, PartialEq, uniffi::Error)]
#[allow(missing_docs)]
pub enum NativePrnsOtaError {
    InvalidDestination { reason: String },
    InvalidImage { reason: String },
    Reticulum { reason: String },
    Control { reason: String },
    Resource { reason: String },
    TargetRejected { reason: String },
    ProgressTimeout,
}

impl fmt::Display for NativePrnsManagementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NodeNotRunning => formatter.write_str("the client PRNS node is not running"),
            Self::InvalidDestination { reason } => {
                write!(formatter, "invalid management destination: {reason}")
            }
            Self::Path { reason } => {
                write!(formatter, "management path discovery failed: {reason}")
            }
            Self::Link { reason } => write!(formatter, "management Link failed: {reason}"),
            Self::Identify { reason } => write!(formatter, "Link identification failed: {reason}"),
            Self::RequestEncoding => formatter.write_str("management request could not be encoded"),
            Self::Request { reason } => write!(formatter, "management request failed: {reason}"),
            Self::Response { reason } => write!(formatter, "management response failed: {reason}"),
            Self::UnexpectedResponse => {
                formatter.write_str("management response had the wrong operation")
            }
            Self::EnrollmentRejected => formatter.write_str("the appliance rejected enrollment"),
        }
    }
}

impl std::error::Error for NativePrnsManagementError {}

impl fmt::Display for NativePrnsOtaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDestination { reason } => {
                write!(formatter, "invalid OTA destination: {reason}")
            }
            Self::InvalidImage { reason } => write!(formatter, "invalid OTA image: {reason}"),
            Self::Reticulum { reason } => write!(formatter, "OTA Link failed: {reason}"),
            Self::Control { reason } => write!(formatter, "OTA control request failed: {reason}"),
            Self::Resource { reason } => write!(formatter, "OTA Resource failed: {reason}"),
            Self::TargetRejected { reason } => {
                write!(formatter, "the board rejected the OTA transfer: {reason}")
            }
            Self::ProgressTimeout => {
                formatter.write_str("the board did not commit OTA progress in time")
            }
        }
    }
}

impl std::error::Error for NativePrnsOtaError {}

impl fmt::Display for NativePrnsNodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStorage { reason } => write!(formatter, "invalid PRNS storage: {reason}"),
            Self::AlreadyRunning => formatter.write_str("a client PRNS node is already running"),
            Self::WorkerSpawn => formatter.write_str("could not start the client PRNS worker"),
            Self::StartupTimeout => {
                formatter.write_str("the client PRNS node did not start in time")
            }
            Self::Startup { reason } => write!(formatter, "client PRNS startup failed: {reason}"),
            Self::Stopped { reason } => write!(formatter, "client PRNS shutdown failed: {reason}"),
        }
    }
}

impl std::error::Error for NativePrnsNodeError {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NodeFacts {
    identity_hash: String,
    client_destination: String,
    bluetooth_identity: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ManagementCandidateFacts {
    destination: DestinationHash,
    interface: personal_rns::interfaces::InterfaceId,
    hops: u8,
}

#[derive(Clone, Debug)]
struct NearbyPeerFacts {
    destination: DestinationHash,
    identity: IdentityHash,
    app_data: Vec<u8>,
    hops: u8,
    interface: personal_rns::interfaces::InterfaceId,
    observed_at: Instant,
}

#[derive(Clone)]
struct RuntimeAccess {
    handle: PrnsNodeHandle,
    identity: IdentityHash,
    executor: tokio::runtime::Handle,
}

struct SharedState {
    state: AtomicU8,
    facts: Mutex<Option<NodeFacts>>,
    failure: Mutex<Option<String>>,
    runtime: Mutex<Option<RuntimeAccess>>,
    management_candidates: Mutex<Vec<ManagementCandidateFacts>>,
    nearby_peers: Mutex<Vec<NearbyPeerFacts>>,
    next_request_id: AtomicU64,
    claim_released: AtomicBool,
}

impl SharedState {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(NativePrnsNodeState::Starting.code()),
            facts: Mutex::new(None),
            failure: Mutex::new(None),
            runtime: Mutex::new(None),
            management_candidates: Mutex::new(Vec::new()),
            nearby_peers: Mutex::new(Vec::new()),
            next_request_id: AtomicU64::new(1),
            claim_released: AtomicBool::new(false),
        }
    }

    fn set_state(&self, state: NativePrnsNodeState) {
        self.state.store(state.code(), Ordering::Release);
    }

    fn fail(&self, reason: String) {
        *lock_unpoisoned(&self.failure) = Some(reason);
        self.set_state(NativePrnsNodeState::Failed);
    }

    fn release_claim(&self) {
        if !self.claim_released.swap(true, Ordering::AcqRel) {
            NODE_CLAIMED.store(false, Ordering::Release);
        }
    }

    fn install_runtime(
        &self,
        handle: PrnsNodeHandle,
        identity: IdentityHash,
        executor: tokio::runtime::Handle,
    ) {
        *lock_unpoisoned(&self.runtime) = Some(RuntimeAccess {
            handle,
            identity,
            executor,
        });
    }

    fn clear_runtime(&self) {
        *lock_unpoisoned(&self.runtime) = None;
    }

    fn runtime(&self) -> Option<RuntimeAccess> {
        lock_unpoisoned(&self.runtime).clone()
    }

    fn next_request_id(&self) -> RequestId {
        RequestId(self.next_request_id.fetch_add(1, Ordering::Relaxed))
    }

    fn observe_event(&self, event: PrnsEvent<'_>) {
        let PrnsEvent::Diagnostic(Diagnostic::AnnounceHeard {
            destination,
            hops,
            source_interface,
        }) = event
        else {
            return;
        };
        let candidate = ManagementCandidateFacts {
            destination,
            interface: source_interface,
            hops,
        };
        let mut candidates = lock_unpoisoned(&self.management_candidates);
        if let Some(existing) = candidates
            .iter_mut()
            .find(|existing| existing.destination == destination)
        {
            *existing = candidate;
            return;
        }
        if candidates.len() == MANAGEMENT_CANDIDATE_CAPACITY {
            candidates.remove(0);
        }
        candidates.push(candidate);
    }

    fn observe_accepted_announce(&self, observation: AnnounceObservation<'_>) {
        let name = expand_name("lxmf", &["delivery"])
            .expect("the static LXMF delivery destination name is valid");
        if derive_destination_hash(&observation.announced_identity, &name)
            != observation.destination
        {
            return;
        }

        let peer = NearbyPeerFacts {
            destination: observation.destination,
            identity: observation.announced_identity,
            app_data: observation.app_data.to_vec(),
            hops: observation.hops.0,
            interface: observation.source_interface,
            observed_at: Instant::now(),
        };
        let mut peers = lock_unpoisoned(&self.nearby_peers);
        if let Some(existing) = peers
            .iter_mut()
            .find(|existing| existing.destination == peer.destination)
        {
            *existing = peer;
            return;
        }
        if peers.len() == MAX_NEARBY_PEERS
            && let Some((oldest, _)) = peers
                .iter()
                .enumerate()
                .max_by_key(|(_, peer)| peer.observed_at.elapsed())
        {
            peers.remove(oldest);
        }
        peers.push(peer);
    }
}

struct Worker {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

/// Native owner of one app-wide PRNS node and Bluetooth Auto interface.
///
/// The app uses one node to reach any number of appliances and Reticulum
/// applications. Opening a separate node per appliance is rejected.
#[derive(uniffi::Object)]
pub struct NativePrnsNode {
    shared: Arc<SharedState>,
    worker: Mutex<Option<Worker>>,
}

#[uniffi::export]
impl NativePrnsNode {
    /// Start the process-wide PRNS node in an app-private absolute directory.
    #[uniffi::constructor]
    pub fn start(storage_directory: String) -> Result<Arc<Self>, NativePrnsNodeError> {
        initialize_prns_diagnostics();
        claim_node()?;
        match Self::start_claimed(storage_directory) {
            Ok(node) => Ok(node),
            Err(error) => {
                NODE_CLAIMED.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    /// Return secret-free identity and lifecycle facts.
    pub fn snapshot(&self) -> NativePrnsNodeSnapshot {
        let facts = lock_unpoisoned(&self.shared.facts).clone();
        NativePrnsNodeSnapshot {
            state: NativePrnsNodeState::from_code(self.shared.state.load(Ordering::Acquire)),
            identity_hash: facts.as_ref().map(|facts| facts.identity_hash.clone()),
            client_destination: facts.as_ref().map(|facts| facts.client_destination.clone()),
            bluetooth_identity: facts.map(|facts| facts.bluetooth_identity),
            failure: lock_unpoisoned(&self.shared.failure).clone(),
        }
    }

    /// Return verified management announces observed by this PRNS node.
    pub fn management_candidates(&self) -> Vec<NativePrnsManagementCandidate> {
        lock_unpoisoned(&self.shared.management_candidates)
            .iter()
            .map(|candidate| NativePrnsManagementCandidate {
                destination_hash: hex::encode(candidate.destination.as_bytes()),
                interface_id: hex::encode(candidate.interface.as_bytes()),
                hops: candidate.hops,
            })
            .collect()
    }

    /// Read the appliance's public destination summary over an identified Link.
    pub async fn public_management_identity(
        &self,
        destination_hash: String,
    ) -> Result<NativePrnsManagementIdentity, NativePrnsManagementError> {
        let destination = parse_destination_hash(&destination_hash)?;
        let request_id = self.shared.next_request_id();
        let response = self
            .request_device_api(
                destination,
                MANAGEMENT_PUBLIC_PATH,
                request_id,
                DeviceRequest::IdentitySummary,
            )
            .await?;
        let DeviceResponse::IdentitySummary(summary) = response else {
            return Err(NativePrnsManagementError::UnexpectedResponse);
        };
        Ok(native_management_identity(summary))
    }

    /// Read the destination summary only when this app identity is enrolled.
    pub async fn management_identity(
        &self,
        destination_hash: String,
    ) -> Result<NativePrnsManagementIdentity, NativePrnsManagementError> {
        let destination = parse_destination_hash(&destination_hash)?;
        let request_id = self.shared.next_request_id();
        let response = self
            .request_device_api(
                destination,
                MANAGEMENT_REQUEST_PATH,
                request_id,
                DeviceRequest::IdentitySummary,
            )
            .await?;
        let DeviceResponse::IdentitySummary(summary) = response else {
            return Err(NativePrnsManagementError::UnexpectedResponse);
        };
        Ok(native_management_identity(summary))
    }

    /// Enroll this app-owned Reticulum identity during the board's physical window.
    pub async fn enroll_management(
        &self,
        destination_hash: String,
    ) -> Result<(), NativePrnsManagementError> {
        let destination = parse_destination_hash(&destination_hash)?;
        let (handle, link_id) = self.identified_link(destination).await?;
        let response = handle
            .request(
                link_id,
                RequestPathHash::of(MANAGEMENT_ENROLLMENT_PATH),
                &MANAGEMENT_ENROLLMENT_REQUEST_VALUE,
            )
            .await
            .map(|(response, _)| response)
            .map_err(|error| NativePrnsManagementError::Request {
                reason: format!("{error:?}"),
            });
        let _ = handle.close_link(link_id);
        let response = response?;
        if response.as_slice() != MANAGEMENT_ENROLLMENT_SUCCESS_RESPONSE {
            return Err(NativePrnsManagementError::EnrollmentRejected);
        }
        Ok(())
    }

    /// Stage a complete ESP image through ordinary PRNS Resources.
    ///
    /// One identified Link remains open for the manifest, every ordered
    /// bounded Resource, and progress confirmation. The board selects the
    /// verified inactive slot for its next boot; this method does not request
    /// an immediate reboot.
    pub async fn stage_ota_image(
        &self,
        destination_hash: String,
        image: Vec<u8>,
        version: String,
    ) -> Result<NativePrnsOtaStatus, NativePrnsOtaError> {
        let destination = parse_destination_hash(&destination_hash).map_err(|error| {
            NativePrnsOtaError::InvalidDestination {
                reason: error.to_string(),
            }
        })?;
        let image_bytes =
            u32::try_from(image.len()).map_err(|_| NativePrnsOtaError::InvalidImage {
                reason: "image length exceeds the protocol range".to_owned(),
            })?;
        if image.first() != Some(&ESP_IMAGE_MAGIC) {
            return Err(NativePrnsOtaError::InvalidImage {
                reason: "image does not start with an ESP application header".to_owned(),
            });
        }
        let version = OtaVersion::try_from_bytes(version.as_bytes()).map_err(|error| {
            NativePrnsOtaError::InvalidImage {
                reason: format!("invalid release label: {error:?}"),
            }
        })?;
        let digest: [u8; 32] = Sha256::digest(&image).into();
        let manifest = OtaManifest::new(image_bytes, digest, version).map_err(|error| {
            NativePrnsOtaError::InvalidImage {
                reason: format!("invalid release manifest: {error:?}"),
            }
        })?;
        let (handle, link_id) = self.identified_link(destination).await.map_err(|error| {
            NativePrnsOtaError::Reticulum {
                reason: error.to_string(),
            }
        })?;
        let result = stage_ota_on_link(&handle, link_id, manifest, &image).await;
        let _ = handle.close_link(link_id);
        result.map(native_ota_status)
    }

    /// Ask the board to reboot into its completely verified staged slot.
    pub async fn reboot_into_staged_ota(
        &self,
        destination_hash: String,
    ) -> Result<NativePrnsOtaStatus, NativePrnsOtaError> {
        let destination = parse_destination_hash(&destination_hash).map_err(|error| {
            NativePrnsOtaError::InvalidDestination {
                reason: error.to_string(),
            }
        })?;
        let (handle, link_id) = self.identified_link(destination).await.map_err(|error| {
            NativePrnsOtaError::Reticulum {
                reason: error.to_string(),
            }
        })?;
        let result =
            request_ota_control(&handle, link_id, OTA_REBOOT_PATH, &OTA_REBOOT_REQUEST_VALUE).await;
        let _ = handle.close_link(link_id);
        let status = result?;
        if status.phase != OtaPhase::Activated {
            return Err(target_rejected(status));
        }
        Ok(native_ota_status(status))
    }

    /// Stop PRNS and wait for its recipe-managed persistence to flush.
    pub fn close(&self) -> Result<(), NativePrnsNodeError> {
        self.close_inner()
    }
}

impl NativePrnsNode {
    fn start_claimed(storage_directory: String) -> Result<Arc<Self>, NativePrnsNodeError> {
        let root = validate_storage_directory(&storage_directory)?;
        let shared = Arc::new(SharedState::new());
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let worker_shared = Arc::clone(&shared);
        let join = thread::Builder::new()
            .name("appliance-prns".to_owned())
            .spawn(move || run_worker(root, worker_shared, ready_tx, shutdown_rx))
            .map_err(|_| NativePrnsNodeError::WorkerSpawn)?;

        let worker = Worker {
            shutdown: Some(shutdown_tx),
            join: Some(join),
        };
        let node = Arc::new(Self {
            shared,
            worker: Mutex::new(Some(worker)),
        });
        match ready_rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(facts)) => {
                *lock_unpoisoned(&node.shared.facts) = Some(facts);
                let _ = node.shared.state.compare_exchange(
                    NativePrnsNodeState::Starting.code(),
                    NativePrnsNodeState::Running.code(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                Ok(node)
            }
            Ok(Err(reason)) => {
                node.shared.fail(reason.clone());
                let _ = node.close_inner();
                Err(NativePrnsNodeError::Startup { reason })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                node.shared.fail("worker readiness timed out".to_owned());
                let _ = node.close_inner();
                Err(NativePrnsNodeError::StartupTimeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let reason = "worker stopped before reporting readiness".to_owned();
                node.shared.fail(reason.clone());
                let _ = node.close_inner();
                Err(NativePrnsNodeError::Startup { reason })
            }
        }
    }

    pub(crate) fn nearby_peers(&self) -> Vec<NearbyPeerView> {
        let mut peers = lock_unpoisoned(&self.shared.nearby_peers).clone();
        peers.sort_by_key(|peer| peer.observed_at.elapsed());
        peers
            .into_iter()
            .map(|peer| {
                NearbyPeerView::from_observation(NearbyPeerObservation {
                    destination: *peer.destination.as_bytes(),
                    identity_hash: *peer.identity.as_bytes(),
                    app_data: peer.app_data,
                    hops: peer.hops,
                    interface_id: *peer.interface.as_bytes(),
                    observed_age_ms: u64::try_from(peer.observed_at.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                })
            })
            .collect()
    }

    fn close_inner(&self) -> Result<(), NativePrnsNodeError> {
        let mut worker = lock_unpoisoned(&self.worker).take();
        let Some(worker) = worker.as_mut() else {
            self.shared.release_claim();
            return Ok(());
        };
        if let Some(shutdown) = worker.shutdown.take() {
            let _ = shutdown.send(());
        }
        let joined = worker.join.take().is_none_or(|join| join.join().is_ok());
        self.shared.release_claim();
        if joined {
            if NativePrnsNodeState::from_code(self.shared.state.load(Ordering::Acquire))
                != NativePrnsNodeState::Failed
            {
                self.shared.set_state(NativePrnsNodeState::Stopped);
            }
            Ok(())
        } else {
            let reason = "worker panicked during shutdown".to_owned();
            self.shared.fail(reason.clone());
            Err(NativePrnsNodeError::Stopped { reason })
        }
    }

    async fn identified_link(
        &self,
        destination: DestinationHash,
    ) -> Result<(PrnsNodeHandle, LinkId), NativePrnsManagementError> {
        let runtime = self
            .shared
            .runtime()
            .ok_or(NativePrnsManagementError::NodeNotRunning)?;
        let handle = runtime.handle;
        let identity = runtime.identity;
        let link_id = match handle.establish_link(destination).await {
            Ok(link_id) => link_id,
            Err(SendError::Failed(EstablishLinkFailure::Rejected(
                EstablishLinkRejection::NoRouteToDestination,
            ))) => {
                handle.request_path(destination).await.map_err(|error| {
                    NativePrnsManagementError::Path {
                        reason: format!("{error:?}"),
                    }
                })?;
                handle.establish_link(destination).await.map_err(|error| {
                    NativePrnsManagementError::Link {
                        reason: format!("{error:?}"),
                    }
                })?
            }
            Err(error) => {
                return Err(NativePrnsManagementError::Link {
                    reason: format!("{error:?}"),
                });
            }
        };
        if let Err(error) = handle.identify(link_id, identity).await {
            let _ = handle.close_link(link_id);
            return Err(NativePrnsManagementError::Identify {
                reason: format!("{error:?}"),
            });
        }
        Ok((handle, link_id))
    }

    async fn request_device_api(
        &self,
        destination: DestinationHash,
        path: &str,
        request_id: RequestId,
        request: DeviceRequest<'_>,
    ) -> Result<DeviceResponse, NativePrnsManagementError> {
        let mut encoded = [0; MAX_MESSAGE_BYTES];
        let encoded_len = encode_request(
            &RequestEnvelope {
                version: ApiVersion::CURRENT,
                request_id,
                request,
            },
            &mut encoded,
        )
        .map_err(|_| NativePrnsManagementError::RequestEncoding)?;
        let (handle, link_id) = self.identified_link(destination).await?;
        let response = handle
            .request(link_id, RequestPathHash::of(path), &encoded[..encoded_len])
            .await
            .map(|(response, _)| response)
            .map_err(|error| NativePrnsManagementError::Request {
                reason: format!("{error:?}"),
            });
        let _ = handle.close_link(link_id);
        let response = response?;
        let response = decode_response(response.as_slice()).map_err(|error| {
            NativePrnsManagementError::Response {
                reason: format!("{error:?}"),
            }
        })?;
        if response.version != ApiVersion::CURRENT || response.request_id != request_id {
            return Err(NativePrnsManagementError::UnexpectedResponse);
        }
        Ok(response.response)
    }

    pub(crate) fn management_destination_hashes(&self) -> Vec<DestinationHash> {
        lock_unpoisoned(&self.shared.management_candidates)
            .iter()
            .map(|candidate| candidate.destination)
            .collect()
    }

    pub(crate) fn request_device_api_blocking(
        &self,
        destination: DestinationHash,
        path: &str,
        request: DeviceRequest<'_>,
    ) -> Result<DeviceResponse, NativePrnsManagementError> {
        let runtime = self
            .shared
            .runtime()
            .ok_or(NativePrnsManagementError::NodeNotRunning)?;
        let request_id = self.shared.next_request_id();
        runtime
            .executor
            .block_on(self.request_device_api(destination, path, request_id, request))
    }

    pub(crate) fn is_running(&self) -> bool {
        NativePrnsNodeState::from_code(self.shared.state.load(Ordering::Acquire))
            == NativePrnsNodeState::Running
            && self.shared.runtime().is_some()
    }
}

async fn stage_ota_on_link(
    handle: &PrnsNodeHandle,
    link_id: LinkId,
    manifest: OtaManifest,
    image: &[u8],
) -> Result<OtaStatus, NativePrnsOtaError> {
    let mut start = [0_u8; 75];
    let start_len = encode_start_request(manifest, &mut start).map_err(|error| {
        NativePrnsOtaError::InvalidImage {
            reason: format!("manifest could not be encoded: {error:?}"),
        }
    })?;
    let mut status =
        request_ota_control(handle, link_id, OTA_START_PATH, &start[..start_len]).await?;
    let session = require_ota_progress(status, None, 0, 0, true)?;

    for (index, chunk) in image.chunks(OTA_IMAGE_CHUNK_BYTES).enumerate() {
        let index = u32::try_from(index).map_err(|_| NativePrnsOtaError::InvalidImage {
            reason: "image needs more chunks than the protocol can represent".to_owned(),
        })?;
        let offset = index
            .checked_mul(OTA_IMAGE_CHUNK_BYTES as u32)
            .ok_or_else(|| NativePrnsOtaError::InvalidImage {
                reason: "image chunk offset exceeds the protocol range".to_owned(),
            })?;
        require_ota_progress(status, Some(session), index, offset, true)?;
        let metadata = OtaChunkMetadata::new(session, index, offset).encode();
        let (progress, _progress_receiver) = tokio::sync::mpsc::unbounded_channel();
        handle
            .send_resource_with_options(
                link_id,
                chunk.len() as u64,
                chunk,
                &metadata,
                SegmentCompression::Never,
                progress,
            )
            .await
            .map_err(|error| NativePrnsOtaError::Resource {
                reason: format!("{error:?}"),
            })?;

        let verified_bytes = offset + chunk.len() as u32;
        let final_chunk = verified_bytes == manifest.image_bytes();
        status = wait_for_ota_progress(
            handle,
            link_id,
            session,
            index + 1,
            verified_bytes,
            final_chunk,
        )
        .await?;
        if final_chunk {
            return Ok(status);
        }

        let next = encode_next_request(session, index + 1);
        status = request_ota_control(handle, link_id, OTA_NEXT_PATH, &next).await?;
        require_ota_progress(status, Some(session), index + 1, verified_bytes, true)?;
    }

    Err(NativePrnsOtaError::InvalidImage {
        reason: "image did not contain a transferable chunk".to_owned(),
    })
}

async fn request_ota_control(
    handle: &PrnsNodeHandle,
    link_id: LinkId,
    path: &str,
    data: &[u8],
) -> Result<OtaStatus, NativePrnsOtaError> {
    let response = handle
        .request(link_id, RequestPathHash::of(path), data)
        .await
        .map(|(response, _)| response)
        .map_err(|error| NativePrnsOtaError::Control {
            reason: format!("{error:?}"),
        })?;
    decode_status_response(&response).map_err(|error| NativePrnsOtaError::Control {
        reason: format!("malformed board status: {error:?}"),
    })
}

async fn wait_for_ota_progress(
    handle: &PrnsNodeHandle,
    link_id: LinkId,
    session: OtaSessionId,
    next_chunk: u32,
    verified_bytes: u32,
    final_chunk: bool,
) -> Result<OtaStatus, NativePrnsOtaError> {
    for _ in 0..OTA_PROGRESS_POLL_ATTEMPTS {
        let status =
            request_ota_control(handle, link_id, OTA_STATUS_PATH, &OTA_STATUS_REQUEST_VALUE)
                .await?;
        if status.phase == OtaPhase::Failed {
            return Err(target_rejected(status));
        }
        let expected_phase = if final_chunk {
            OtaPhase::Activated
        } else {
            OtaPhase::Receiving
        };
        if status.phase == expected_phase
            && status.session == Some(session)
            && status.next_chunk == next_chunk
            && status.verified_bytes == verified_bytes
            && !status.resource_armed
        {
            return Ok(status);
        }
        tokio::time::sleep(OTA_PROGRESS_POLL_INTERVAL).await;
    }
    Err(NativePrnsOtaError::ProgressTimeout)
}

fn require_ota_progress(
    status: OtaStatus,
    session: Option<OtaSessionId>,
    next_chunk: u32,
    verified_bytes: u32,
    resource_armed: bool,
) -> Result<OtaSessionId, NativePrnsOtaError> {
    if status.phase == OtaPhase::Failed {
        return Err(target_rejected(status));
    }
    let actual_session = status
        .session
        .ok_or_else(|| NativePrnsOtaError::TargetRejected {
            reason: "board status omitted the active session".to_owned(),
        })?;
    if status.phase != OtaPhase::Receiving
        || session.is_some_and(|session| session != actual_session)
        || status.next_chunk != next_chunk
        || status.verified_bytes != verified_bytes
        || status.resource_armed != resource_armed
    {
        return Err(NativePrnsOtaError::TargetRejected {
            reason: format!(
                "unexpected progress phase={:?} next_chunk={} verified_bytes={} armed={}",
                status.phase, status.next_chunk, status.verified_bytes, status.resource_armed,
            ),
        });
    }
    Ok(actual_session)
}

fn target_rejected(status: OtaStatus) -> NativePrnsOtaError {
    NativePrnsOtaError::TargetRejected {
        reason: status
            .failure
            .map(|failure| format!("{failure:?}"))
            .unwrap_or_else(|| format!("unexpected phase {:?}", status.phase)),
    }
}

fn native_ota_status(status: OtaStatus) -> NativePrnsOtaStatus {
    NativePrnsOtaStatus {
        phase: match status.phase {
            OtaPhase::Idle => NativePrnsOtaPhase::Idle,
            OtaPhase::Receiving => NativePrnsOtaPhase::Receiving,
            OtaPhase::Activated => NativePrnsOtaPhase::Activated,
            OtaPhase::Failed => NativePrnsOtaPhase::Failed,
        },
        session: status
            .session
            .map(|session| hex::encode(session.as_bytes())),
        slot: status.slot.map(|slot| match slot {
            OtaSlot::Ota0 => NativePrnsOtaSlot::Ota0,
            OtaSlot::Ota1 => NativePrnsOtaSlot::Ota1,
        }),
        version: status.version.map(|version| {
            String::from_utf8(version.as_bytes().to_vec())
                .expect("OTA versions are validated printable ASCII")
        }),
        image_bytes: status.image_bytes,
        verified_bytes: status.verified_bytes,
        next_chunk: status.next_chunk,
        resource_armed: status.resource_armed,
        failure: status.failure.map(|failure| {
            match failure {
                OtaFailure::Protocol => "protocol",
                OtaFailure::WrongLink => "wrong_link",
                OtaFailure::UnexpectedChunk => "unexpected_chunk",
                OtaFailure::InvalidEspImage => "invalid_esp_image",
                OtaFailure::DigestMismatch => "digest_mismatch",
                OtaFailure::Flash => "flash",
                OtaFailure::ImageValidation => "image_validation",
                OtaFailure::Activation => "activation",
                OtaFailure::Interrupted => "interrupted",
            }
            .to_owned()
        }),
    }
}

impl Drop for NativePrnsNode {
    fn drop(&mut self) {
        let _ = self.close_inner();
    }
}

fn claim_node() -> Result<(), NativePrnsNodeError> {
    NODE_CLAIMED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| NativePrnsNodeError::AlreadyRunning)
}

fn validate_storage_directory(path: &str) -> Result<PathBuf, NativePrnsNodeError> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(NativePrnsNodeError::InvalidStorage {
            reason: "path must be absolute".to_owned(),
        });
    }
    std::fs::create_dir_all(&path).map_err(|error| NativePrnsNodeError::InvalidStorage {
        reason: error.to_string(),
    })?;
    let root = path
        .canonicalize()
        .map_err(|error| NativePrnsNodeError::InvalidStorage {
            reason: error.to_string(),
        })?;
    if !root.is_dir() {
        return Err(NativePrnsNodeError::InvalidStorage {
            reason: "path is not a directory".to_owned(),
        });
    }
    Ok(root.join(MOBILE_PRNS_DIRECTORY))
}

fn parse_destination_hash(encoded: &str) -> Result<DestinationHash, NativePrnsManagementError> {
    let mut bytes = [0; 16];
    hex::decode_to_slice(encoded, &mut bytes).map_err(|error| {
        NativePrnsManagementError::InvalidDestination {
            reason: error.to_string(),
        }
    })?;
    Ok(DestinationHash::new(bytes))
}

fn native_management_identity(summary: IdentitySummary) -> NativePrnsManagementIdentity {
    NativePrnsManagementIdentity {
        management_destination: hex::encode(summary.primary_destination().0),
        lxmf_destination: summary
            .lxmf_delivery_destination()
            .map(|destination| hex::encode(destination.0)),
    }
}

fn run_worker(
    root: PathBuf,
    shared: Arc<SharedState>,
    ready: mpsc::SyncSender<Result<NodeFacts, String>>,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let reason = format!("could not build async runtime: {error}");
            let _ = ready.send(Err(reason.clone()));
            shared.fail(reason);
            return;
        }
    };
    let executor = runtime.handle().clone();
    runtime.block_on(run_node(root, shared, ready, shutdown, executor));
}

async fn run_node(
    root: PathBuf,
    shared: Arc<SharedState>,
    ready: mpsc::SyncSender<Result<NodeFacts, String>>,
    shutdown: tokio::sync::oneshot::Receiver<()>,
    executor: tokio::runtime::Handle,
) {
    let identity = match load_or_create_identity_secret(&root.join(APP_IDENTITY_FILE)) {
        Ok(identity) => identity,
        Err(error) => {
            fail_start(&shared, &ready, format!("Reticulum identity: {error}"));
            return;
        }
    };
    let ble_identity = match load_or_create_ble_identity(&root.join(BLE_IDENTITY_FILE)) {
        Ok(identity) => identity,
        Err(error) => {
            fail_start(&shared, &ready, format!("Bluetooth Auto identity: {error}"));
            return;
        }
    };
    let persistence = match NodePersistence::custom_dir(root.join(PRNS_PERSISTENCE_DIRECTORY)) {
        Ok(persistence) => persistence,
        Err(error) => {
            fail_start(&shared, &ready, format!("PRNS persistence: {error}"));
            return;
        }
    };

    let signer = InMemoryNodeIdentity::from_secret_key_bytes(&identity);
    let identity_hash = signer.identity_hash();
    let destination = client_destination(identity);
    let destination_hash = match destination.destination_hash() {
        Ok(destination) => destination,
        Err(error) => {
            fail_start(
                &shared,
                &ready,
                format!("client destination name is invalid: {error:?}"),
            );
            return;
        }
    };

    #[cfg(any(target_os = "ios", target_os = "macos"))]
    let prepared_ble = match AutoBle::prepare(ble_identity).await {
        Ok(prepared) => prepared,
        Err(_) => AutoBle::unavailable(ble_identity),
    };

    let event_shared = Arc::clone(&shared);
    let announce_shared = Arc::clone(&shared);
    let node = PrnsNode::new(PrnsNodeRecipe {
        transport_identity: None,
        pre_configured_destinations: [destination],
        app_state: (),
        storage: GrowableHeap,
        request_endpoints: personal_rns::request_endpoints![],
        interfaces: ManuallyAttached,
        persistence,
        on_event: move |event, _state: &()| event_shared.observe_event(event),
    })
    .with_accepted_announce_observer(move |observation| {
        announce_shared.observe_accepted_announce(observation);
    });
    let handle = node.handle();
    shared.install_runtime(handle.clone(), identity_hash, executor);

    #[cfg(any(target_os = "ios", target_os = "macos"))]
    let _attached_ble = handle.attach(prepared_ble);

    #[cfg(target_os = "android")]
    let _attached_ble = {
        let bridge = android_bridge();
        bridge.set_local_identity(ble_identity);
        let bluetooth = BluetoothAuto::<_, { AndroidBleBackend::MAX_PEERS }>::new(
            AndroidBleBackend::new(bridge),
            ble_identity,
            Endpoint::Android(AndroidHost::Android),
            LinkCapabilities {
                l2cap: None,
                link_mtu: BLE_HW_MTU as u16,
            },
        );
        let status = bluetooth.status();
        handle.supervise(bluetooth);
        status
    };

    #[cfg(not(any(target_os = "android", target_os = "ios", target_os = "macos")))]
    let _attached_ble = handle.attach(personal_rns::bluetooth_auto::AutoBle::new(ble_identity));

    let facts = NodeFacts {
        identity_hash: hex::encode(identity_hash.as_bytes()),
        client_destination: hex::encode(destination_hash.as_bytes()),
        bluetooth_identity: hex::encode(ble_identity.as_bytes()),
    };
    if ready.send(Ok(facts)).is_err() {
        shared.clear_runtime();
        shared.fail("node owner disappeared during startup".to_owned());
        return;
    }

    if let Err(error) = node
        .run_until(async {
            let _ = shutdown.await;
        })
        .await
    {
        shared.fail(format!("PRNS runtime stopped: {error}"));
    } else if NativePrnsNodeState::from_code(shared.state.load(Ordering::Acquire))
        != NativePrnsNodeState::Failed
    {
        shared.set_state(NativePrnsNodeState::Stopped);
    }
    shared.clear_runtime();
}

fn client_destination(
    identity: Zeroizing<[u8; IDENTITY_SECRET_KEY_LEN]>,
) -> PreConfiguredDestination<'static> {
    PreConfiguredDestination::Single {
        app_name: CLIENT_APP_NAME,
        aspects: CLIENT_ASPECTS,
        identity,
        announce_app_data: b"",
        proof: ProofStrategy::ProveNone,
        link_requests: LinkRequestPolicy::AcceptNone,
        ratchet: RatchetPolicy::NoRatchets,
        resource_strategy: ResourceStrategy::AcceptNone,
        maximum_request_bytes: Default::default(),
        request_endpoints: ServeMyRequestEndpoints::No,
    }
}

fn fail_start(
    shared: &SharedState,
    ready: &mpsc::SyncSender<Result<NodeFacts, String>>,
    reason: String,
) {
    let _ = ready.send(Err(reason.clone()));
    shared.fail(reason);
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(target_os = "android")]
static ANDROID_BRIDGE: std::sync::OnceLock<AndroidBleBridge> = std::sync::OnceLock::new();

#[cfg(target_os = "android")]
pub(crate) fn android_bridge() -> AndroidBleBridge {
    ANDROID_BRIDGE.get_or_init(AndroidBleBridge::new).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_identity_is_held_by_one_non_listening_destination() {
        let identity = Zeroizing::new([0x42; IDENTITY_SECRET_KEY_LEN]);
        let destination = client_destination(identity);
        assert_eq!(
            hex::encode(destination.destination_hash().unwrap().as_bytes()),
            "ffb378318a6b904d7c829e79f9456c30"
        );
        assert!(matches!(
            destination,
            PreConfiguredDestination::Single {
                app_name: CLIENT_APP_NAME,
                aspects: CLIENT_ASPECTS,
                proof: ProofStrategy::ProveNone,
                link_requests: LinkRequestPolicy::AcceptNone,
                ratchet: RatchetPolicy::NoRatchets,
                resource_strategy: ResourceStrategy::AcceptNone,
                request_endpoints: ServeMyRequestEndpoints::No,
                ..
            }
        ));
    }

    #[test]
    fn storage_must_be_absolute_before_any_node_is_claimed() {
        let error = validate_storage_directory("relative").unwrap_err();
        assert!(matches!(error, NativePrnsNodeError::InvalidStorage { .. }));
    }

    #[test]
    fn lifecycle_codes_are_stable_at_the_generated_boundary() {
        assert_eq!(
            NativePrnsNodeState::from_code(0),
            NativePrnsNodeState::Starting
        );
        assert_eq!(
            NativePrnsNodeState::from_code(1),
            NativePrnsNodeState::Running
        );
        assert_eq!(
            NativePrnsNodeState::from_code(2),
            NativePrnsNodeState::Failed
        );
        assert_eq!(
            NativePrnsNodeState::from_code(3),
            NativePrnsNodeState::Stopped
        );
    }

    #[test]
    fn ota_progress_requires_the_same_armed_session_and_exact_offset() {
        let session = OtaSessionId::new([0x44; 16]);
        let status = OtaStatus {
            phase: OtaPhase::Receiving,
            session: Some(session),
            slot: Some(OtaSlot::Ota1),
            version: Some(OtaVersion::try_from_bytes(b"0.2.0").unwrap()),
            image_bytes: 8_000,
            verified_bytes: 7_168,
            next_chunk: 1,
            resource_armed: true,
            failure: None,
        };
        assert_eq!(
            require_ota_progress(status, Some(session), 1, 7_168, true),
            Ok(session)
        );
        assert!(matches!(
            require_ota_progress(status, Some(session), 1, 0, true),
            Err(NativePrnsOtaError::TargetRejected { .. })
        ));
    }

    #[test]
    fn ota_status_projection_uses_stable_secret_free_labels() {
        let status = native_ota_status(OtaStatus {
            phase: OtaPhase::Failed,
            session: Some(OtaSessionId::new([0x55; 16])),
            slot: Some(OtaSlot::Ota0),
            version: Some(OtaVersion::try_from_bytes(b"1.0.0").unwrap()),
            image_bytes: 64,
            verified_bytes: 0,
            next_chunk: 0,
            resource_armed: false,
            failure: Some(OtaFailure::DigestMismatch),
        });
        assert_eq!(status.phase, NativePrnsOtaPhase::Failed);
        assert_eq!(status.session, Some("55".repeat(16)));
        assert_eq!(status.slot, Some(NativePrnsOtaSlot::Ota0));
        assert_eq!(status.version.as_deref(), Some("1.0.0"));
        assert_eq!(status.failure.as_deref(), Some("digest_mismatch"));
    }

    #[test]
    fn management_destination_parser_requires_one_complete_hash() {
        assert_eq!(
            parse_destination_hash("00112233445566778899aabbccddeeff").unwrap(),
            DestinationHash::new([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ])
        );
        assert!(matches!(
            parse_destination_hash("0011"),
            Err(NativePrnsManagementError::InvalidDestination { .. })
        ));
    }

    #[test]
    fn accepted_announces_become_bounded_probe_candidates_without_prns_extensions() {
        let shared = SharedState::new();
        let interface = personal_rns::interfaces::InterfaceId::new([7; 8]);
        for byte in 0..=MANAGEMENT_CANDIDATE_CAPACITY as u8 {
            shared.observe_event(PrnsEvent::Diagnostic(Diagnostic::AnnounceHeard {
                destination: DestinationHash::new([byte; 16]),
                hops: byte,
                source_interface: interface,
            }));
        }
        let candidates = lock_unpoisoned(&shared.management_candidates);
        assert_eq!(candidates.len(), MANAGEMENT_CANDIDATE_CAPACITY);
        assert_eq!(candidates[0].destination, DestinationHash::new([1; 16]));
        assert_eq!(candidates.last().unwrap().hops, 32);
    }

    #[test]
    fn public_identity_projection_contains_no_runtime_or_bearer_state() {
        let management = reticulum_device_api::DestinationHash([0x41; 16]);
        let lxmf = reticulum_device_api::DestinationHash([0x42; 16]);
        assert_eq!(
            native_management_identity(IdentitySummary::with_lxmf_delivery_destination(
                management, lxmf,
            )),
            NativePrnsManagementIdentity {
                management_destination: hex::encode(management.0),
                lxmf_destination: Some(hex::encode(lxmf.0)),
            }
        );
    }
}
