//! Scalar RNS identity, link, request, and transmit-target wrappers.

use super::*;

/// Scalar result of composing one signed basic LXMF carrier.
///
/// The caller retains the bytes in its supplied output buffer. The scalar
/// contains no private identity material and does not expose the underlying
/// Rete message, signing source, or destination. It privately preserves the
/// composer capability needed by the dedicated opportunistic DATA path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedBasicLxmf(pub(crate) RnsPreparedBasicLxmf);

impl PreparedBasicLxmf {
    /// Exact carrier prefix initialized in the caller's output buffer.
    pub const fn carrier_len(self) -> u16 {
        self.0.carrier_len()
    }

    /// Python-compatible authenticated LXMF message identifier.
    pub const fn message_id(self) -> [u8; 32] {
        self.0.message_id()
    }
}

/// Scalar result of composing one complete signed basic LXMF wire message.
///
/// The caller retains the bytes in its supplied output buffer. The private
/// native value preserves their binding for the later direct-Link transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedBasicDirectLxmf(pub(crate) RnsPreparedBasicDirectLxmf);

impl PreparedBasicDirectLxmf {
    /// Exact complete-wire prefix initialized in the caller's output buffer.
    pub const fn wire_len(self) -> u16 {
        self.0.wire_len()
    }

    /// Python-compatible authenticated LXMF message identifier.
    pub const fn message_id(self) -> [u8; 32] {
        self.0.message_id()
    }
}

/// Failure to compose the supported basic single-packet LXMF subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrepareBasicLxmfError {
    /// Timestamp zero is outside the firmware's useful message subset.
    InvalidTimestamp,
    /// Whole-millisecond timestamp exceeds the exact binary64 subset.
    TimestampTooLarge {
        /// Rejected Unix timestamp in milliseconds.
        actual: u64,
        /// Largest supported Unix timestamp in milliseconds.
        maximum: u64,
    },
    /// The node has no registered inbound Single `lxmf.delivery` source.
    DeliveryDestinationUnavailable,
    /// The node identity could not sign the canonical message.
    Signing,
    /// Canonical content exceeds the selected single-packet boundary.
    PayloadTooLarge {
        /// Computed Python LXMF `content_size`.
        actual: usize,
        /// Largest content size supported by the selected packet mode.
        maximum: usize,
    },
    /// Caller-owned message storage is too small.
    OutputTooSmall {
        /// Required message bytes.
        required: usize,
        /// Supplied output capacity.
        available: usize,
    },
    /// The pinned packer violated an internal reviewed invariant.
    Invariant,
}

impl From<RnsPrepareBasicLxmfError> for PrepareBasicLxmfError {
    fn from(error: RnsPrepareBasicLxmfError) -> Self {
        match error {
            RnsPrepareBasicLxmfError::InvalidTimestamp => Self::InvalidTimestamp,
            RnsPrepareBasicLxmfError::TimestampTooLarge { actual, maximum } => {
                Self::TimestampTooLarge { actual, maximum }
            }
            RnsPrepareBasicLxmfError::DeliveryDestinationUnavailable => {
                Self::DeliveryDestinationUnavailable
            }
            RnsPrepareBasicLxmfError::Signing => Self::Signing,
            RnsPrepareBasicLxmfError::PayloadTooLarge { actual, maximum } => {
                Self::PayloadTooLarge { actual, maximum }
            }
            RnsPrepareBasicLxmfError::OutputTooSmall {
                required,
                available,
            } => Self::OutputTooSmall {
                required,
                available,
            },
            RnsPrepareBasicLxmfError::Invariant => Self::Invariant,
        }
    }
}

/// Opaque Reticulum identity owned by the node core.
pub struct NodeIdentity(pub(crate) RnsIdentity);

impl NodeIdentity {
    /// Import Reticulum's combined 64-byte X25519-plus-Ed25519 private key.
    pub fn from_private_key(private_key: &[u8]) -> Result<Self, IdentityError> {
        reticulum_rns_rete::identity_from_private_key(private_key)
            .map(Self)
            .map_err(|_| IdentityError::InvalidPrivateKey)
    }

    /// Public X25519-plus-Ed25519 key bytes.
    pub fn public_key(&self) -> [u8; 64] {
        self.0.public_key()
    }

    /// Public 128-bit Reticulum identity hash.
    pub fn identity_hash(&self) -> [u8; 16] {
        *self.0.hash().as_bytes()
    }
}

/// Failure to import a node identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// The private key length or key material is invalid.
    InvalidPrivateKey,
}

/// Product-owned complete 128-bit Reticulum destination hash.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DestinationHash([u8; 16]);

impl DestinationHash {
    /// Construct a destination hash from its complete wire bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete destination hash bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub(crate) fn into_rns(self) -> RnsDestinationHash {
        RnsDestinationHash::from(self.0)
    }
}

impl AsRef<[u8]> for DestinationHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Opaque identifier for one Link retained by this exact protocol owner.
///
/// Callers normally receive handles from [`NodeCore::initiate_link`]. A handle
/// reconstructed from an application event remains only a lookup key: every
/// operation revalidates it against the node's retained Link table.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LinkHandle([u8; 16]);

impl LinkHandle {
    /// Construct a lookup handle from complete application-event Link bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub(crate) fn from_rns(link: RnsLinkId) -> Self {
        Self::new(*link.as_bytes())
    }

    pub(crate) fn into_rns(self) -> RnsLinkId {
        RnsLinkId::from(self.0)
    }

    /// Borrow the complete Link identifier for event correlation.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Read-only lifecycle state of one retained Link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkState {
    /// The Link request has not yet entered the handshake.
    Pending,
    /// The request/proof/RTT handshake is in progress.
    Handshake,
    /// Authenticated Link DATA may flow.
    Active,
    /// The established Link has exceeded its traffic watchdog.
    Stale,
    /// The Link is closing or closed but has not yet been purged.
    Closed,
}

impl From<RnsLinkState> for LinkState {
    fn from(state: RnsLinkState) -> Self {
        match state {
            RnsLinkState::Pending => Self::Pending,
            RnsLinkState::Handshake => Self::Handshake,
            RnsLinkState::Active => Self::Active,
            RnsLinkState::Stale => Self::Stale,
            RnsLinkState::Closed => Self::Closed,
        }
    }
}

/// Copyable exact correlation for one node-owned direct Link request.
///
/// The fields are private so application workers cannot forge a Link/request
/// association. [`NodeCore`] retains every move-only native preparation or
/// confirmed-dispatch authority; queues and protocol clients carry only this
/// scalar.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestHandle(RnsRequestHandle);

impl RequestHandle {
    pub(crate) fn from_rns(handle: RnsRequestHandle) -> Self {
        Self(handle)
    }

    pub(crate) fn into_rns(self) -> RnsRequestHandle {
        self.0
    }

    /// Complete Link identifier for application-event correlation.
    pub const fn link(&self) -> &[u8; 16] {
        self.0.link()
    }

    /// Complete request identifier for response and failure correlation.
    pub const fn request(&self) -> &[u8; 16] {
        self.0.request()
    }
}

/// Result of applying one router-observed dispatch decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a confirmed request must later complete or be canceled"]
pub enum RequestDispatchConfirmation {
    /// The router did not report this as the packet's first interface dispatch.
    NotFirstDispatch,
    /// The first dispatch started native response-timeout tracking.
    Confirmed,
}

impl From<RnsRequestDispatchConfirmation> for RequestDispatchConfirmation {
    fn from(confirmation: RnsRequestDispatchConfirmation) -> Self {
        match confirmation {
            RnsRequestDispatchConfirmation::NotFirstDispatch => Self::NotFirstDispatch,
            RnsRequestDispatchConfirmation::Confirmed => Self::Confirmed,
        }
    }
}

/// Definitive result of reclaiming one exact request from every native owner.
///
/// Every variant guarantees that the supplied request is absent from both the
/// integration adapter and the underlying Rete pending-request table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestDispatchReconciliation {
    /// Neither native table retained the supplied request.
    Absent,
    /// Both tables consistently retained and released a prepared request.
    ReclaimedPrepared,
    /// Both tables consistently retained and released a confirmed request.
    ReclaimedConfirmed,
    /// Native tables disagreed, but every exact residual owner was reclaimed.
    ReclaimedInconsistent,
}

impl From<RnsRequestDispatchReconciliation> for RequestDispatchReconciliation {
    fn from(reconciliation: RnsRequestDispatchReconciliation) -> Self {
        match reconciliation {
            RnsRequestDispatchReconciliation::Absent => Self::Absent,
            RnsRequestDispatchReconciliation::ReclaimedPrepared => Self::ReclaimedPrepared,
            RnsRequestDispatchReconciliation::ReclaimedConfirmed => Self::ReclaimedConfirmed,
            RnsRequestDispatchReconciliation::ReclaimedInconsistent => Self::ReclaimedInconsistent,
        }
    }
}

/// Failure to transition or cancel one exact request authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestDispatchError {
    /// No native authority is retained for the supplied handle.
    NotTracked,
    /// The request is no longer awaiting first dispatch.
    NotPrepared,
    /// The request has not entered confirmed-dispatch state.
    NotConfirmed,
    /// The request's Link was removed after preparation.
    LinkNotFound,
    /// The request's Link stopped being active after preparation.
    LinkNotActive,
    /// Native request state and the retained adapter authority disagree.
    NativeStateMismatch,
}

impl From<RnsRequestDispatchError> for RequestDispatchError {
    fn from(error: RnsRequestDispatchError) -> Self {
        match error {
            RnsRequestDispatchError::NotTracked => Self::NotTracked,
            RnsRequestDispatchError::NotPrepared => Self::NotPrepared,
            RnsRequestDispatchError::NotConfirmed => Self::NotConfirmed,
            RnsRequestDispatchError::LinkNotFound => Self::LinkNotFound,
            RnsRequestDispatchError::LinkNotActive => Self::LinkNotActive,
            RnsRequestDispatchError::NativeStateMismatch => Self::NativeStateMismatch,
        }
    }
}

/// Exact ordinary action envelope and correlation handle for one request.
///
/// Splitting this owner moves the complete packet action while returning only
/// a copyable [`RequestHandle`]. If ordinary routing cannot retain the action,
/// callers must preserve the unchanged envelope or cancel its handle through
/// the same node owner.
#[derive(Debug)]
#[must_use = "request actions must be durably admitted or their handle canceled"]
pub struct PreparedRequestActions {
    pub(crate) actions: NodeActions,
    pub(crate) handle: RequestHandle,
}

impl PreparedRequestActions {
    /// Borrow the exact ordinary action envelope without splitting ownership.
    pub const fn actions(&self) -> &NodeActions {
        &self.actions
    }

    /// Copy the exact Link/request correlation handle.
    pub const fn handle(&self) -> RequestHandle {
        self.handle
    }

    /// Split into the complete action owner and copyable correlation handle.
    pub fn into_parts(self) -> (NodeActions, RequestHandle) {
        (self.actions, self.handle)
    }
}

/// Failure to prepare one anonymous direct Link request action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrepareRequestError {
    /// The selected Link is not retained.
    LinkNotFound,
    /// The selected Link has not reached the active state.
    LinkNotActive,
    /// The active Link has no authenticated interface binding.
    LinkInterfaceUnknown,
    /// The canonical request exceeds the negotiated Link MDU.
    RequestTooLarge {
        /// Computed request payload size.
        actual: usize,
        /// Negotiated maximum Link data size.
        maximum: usize,
    },
    /// Every native request-dispatch authority slot is occupied.
    DispatchTableFull {
        /// Configured native authority limit.
        limit: usize,
    },
    /// Fresh entropy reproduced a still-live request identifier.
    RequestAlreadyTracked,
    /// Native bounded request storage could not be reserved.
    RequestAllocationFailed,
    /// Link encryption failed.
    Crypto,
    /// RNS could not build the direct request packet.
    PacketBuild,
    /// The native adapter reported an impossible request shape or state.
    Invariant,
    /// The outer ordinary action envelope could not reserve one packet entry.
    ActionAllocationFailed,
}

impl From<RnsPrepareDirectRequestError> for PrepareRequestError {
    fn from(error: RnsPrepareDirectRequestError) -> Self {
        match error {
            RnsPrepareDirectRequestError::LinkNotFound => Self::LinkNotFound,
            RnsPrepareDirectRequestError::LinkNotActive => Self::LinkNotActive,
            RnsPrepareDirectRequestError::LinkInterfaceUnknown => Self::LinkInterfaceUnknown,
            RnsPrepareDirectRequestError::RequestTooLarge { actual, maximum } => {
                Self::RequestTooLarge { actual, maximum }
            }
            // The anonymous API always supplies canonical MessagePack nil.
            RnsPrepareDirectRequestError::InvalidRequestValue => Self::Invariant,
            RnsPrepareDirectRequestError::DispatchTableFull { limit } => {
                Self::DispatchTableFull { limit }
            }
            RnsPrepareDirectRequestError::RequestAlreadyTracked => Self::RequestAlreadyTracked,
            RnsPrepareDirectRequestError::AllocationFailed => Self::RequestAllocationFailed,
            RnsPrepareDirectRequestError::Crypto => Self::Crypto,
            RnsPrepareDirectRequestError::PacketBuild => Self::PacketBuild,
            RnsPrepareDirectRequestError::Invariant => Self::Invariant,
        }
    }
}

/// Failure to prepare one direct response action for an event-carried request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrepareResponseError {
    /// The request binding's Link is no longer retained.
    LinkNotFound,
    /// Current retained Link destination or role differs from the binding.
    LinkBindingMismatch,
    /// The request binding's Link is no longer active.
    LinkNotActive,
    /// The active Link has no authenticated interface binding.
    LinkInterfaceUnknown,
    /// The response body cannot fit the Link's negotiated single-packet MDU.
    ResponseTooLarge {
        /// Supplied response-body bytes.
        actual: usize,
        /// Largest response body that fits the negotiated Link MDU.
        maximum: usize,
    },
    /// Native response packet storage could not be allocated.
    ResponseAllocationFailed,
    /// Link encryption failed.
    Crypto,
    /// RNS could not build the response packet.
    PacketBuild,
    /// The native adapter reported an impossible response state.
    Invariant,
    /// The outer ordinary action envelope could not reserve one packet entry.
    ActionAllocationFailed,
}

impl From<RnsPrepareResponseError> for PrepareResponseError {
    fn from(error: RnsPrepareResponseError) -> Self {
        match error {
            RnsPrepareResponseError::LinkNotFound => Self::LinkNotFound,
            RnsPrepareResponseError::LinkBindingMismatch => Self::LinkBindingMismatch,
            RnsPrepareResponseError::LinkNotActive => Self::LinkNotActive,
            RnsPrepareResponseError::LinkInterfaceUnknown => Self::LinkInterfaceUnknown,
            RnsPrepareResponseError::ResponseTooLarge { actual, maximum } => {
                Self::ResponseTooLarge { actual, maximum }
            }
            RnsPrepareResponseError::AllocationFailed => Self::ResponseAllocationFailed,
            RnsPrepareResponseError::Crypto => Self::Crypto,
            RnsPrepareResponseError::PacketBuild => Self::PacketBuild,
            RnsPrepareResponseError::Invariant => Self::Invariant,
        }
    }
}

/// Failure to construct and retain one outbound Link request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitiateLinkError {
    /// Every configured owned-Link slot is occupied.
    LinkTableFull {
        /// Configured owned-Link limit.
        limit: usize,
    },
    /// Fresh entropy reproduced an already-retained Link identifier.
    LinkIdCollision,
    /// Native initiation returned success without retaining its Link state.
    LinkStateNotRetained,
    /// Native protocol construction rejected the request.
    Protocol,
    /// The action envelope could not reserve one packet entry.
    ActionAllocationFailed,
    /// Action allocation failed and the unestablished Link could not be
    /// removed, so its state remains retained for diagnosis.
    RollbackFailed,
}

impl From<RnsLinkAdmissionError> for InitiateLinkError {
    fn from(error: RnsLinkAdmissionError) -> Self {
        match error {
            RnsLinkAdmissionError::LinkTableFull { limit } => Self::LinkTableFull { limit },
            RnsLinkAdmissionError::LinkIdCollision => Self::LinkIdCollision,
            RnsLinkAdmissionError::LinkStateNotRetained => Self::LinkStateNotRetained,
            RnsLinkAdmissionError::Rete(_) => Self::Protocol,
        }
    }
}

/// Caller-provided identity for one in-memory node-owner incarnation.
///
/// A value must not be reused when reconstructing the same Reticulum identity
/// while messages carrying old TX leases can still exist. Firmware should
/// derive this from a boot/session nonce or a persisted monotonic boot epoch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NodeInstanceId([u8; 16]);

impl NodeInstanceId {
    /// Construct an instance identifier from caller-owned epoch/nonce bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete instance identifier.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Opaque scalar binding for one Reticulum identity and in-memory node-owner
/// incarnation.
///
/// This is correlation metadata, not authorization. It lets a persistent
/// external owner machine reject accidental substitution of another
/// [`NodeCore`] before consuming a unique return.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TxOwnerScope {
    pub(crate) owner_identity: [u8; 16],
    pub(crate) instance: NodeInstanceId,
}

impl TxOwnerScope {
    /// Caller-supplied incarnation included in this exact owner binding.
    pub const fn instance_id(self) -> NodeInstanceId {
        self.instance
    }
}

/// Monotonic time in whole seconds for RNS path and receipt state.
///
/// This is deliberately distinct from executor ticks, milliseconds and Unix
/// wall time. Callers must convert at the platform boundary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicSeconds(u64);

impl MonotonicSeconds {
    /// Construct a monotonic-seconds sample.
    pub const fn new(seconds: u64) -> Self {
        Self(seconds)
    }

    /// Raw whole seconds supplied to RNS.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Reticulum's 40-bit announce-emission ordering value.
///
/// This is deliberately distinct from [`MonotonicSeconds`]. Reticulum embeds
/// the low 40 bits of this value in every locally generated announce and peers
/// use it to reject stale or replayed paths. A persistent identity therefore
/// needs a value that cannot move backwards across reboot. Unix seconds are
/// suitable when trustworthy wall time exists; an offline target may instead
/// use a durably reserved logical epoch.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AnnounceEmissionTime(u64);

impl AnnounceEmissionTime {
    /// Largest value representable in the five-byte Reticulum field.
    pub const MAX: u64 = (1_u64 << 40) - 1;

    /// Construct a checked announce-emission ordering value.
    pub const fn new(value: u64) -> Result<Self, AnnounceEmissionTimeError> {
        if value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(AnnounceEmissionTimeError::OutsideWireRange)
        }
    }

    /// Raw value supplied to the RNS implementation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Failure to construct a Reticulum announce-emission ordering value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnounceEmissionTimeError {
    /// The value would be truncated by Reticulum's five-byte wire field.
    OutsideWireRange,
}

/// Executor-independent monotonic time in whole milliseconds.
///
/// This clock scopes packet-owner deadlines. It is deliberately distinct
/// from RNS's whole-second protocol clock and from Unix wall time.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicMillis(u64);

impl MonotonicMillis {
    /// Construct a monotonic-millisecond sample.
    pub const fn new(milliseconds: u64) -> Self {
        Self(milliseconds)
    }

    /// Raw whole milliseconds supplied by the platform monotonic clock.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Deadline by which an externally owned packet buffer should return to the
/// node owner or enter retained recovery.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TxLeaseDeadline(MonotonicMillis);

impl TxLeaseDeadline {
    /// Construct a packet-owner deadline from a monotonic instant.
    pub const fn new(deadline: MonotonicMillis) -> Self {
        Self(deadline)
    }

    /// Monotonic instant represented by this deadline.
    pub const fn instant(self) -> MonotonicMillis {
        self.0
    }
}

/// Product-owned packet-interface identifier.
///
/// RNS interface identifiers are one byte. The initial compact
/// [`InterfaceSet`] can represent identifiers zero through 63; larger values
/// remain representable here so a native target is never truncated while a
/// later route-resolution step can reject an unsupported profile explicitly.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PacketInterfaceId(u8);

impl PacketInterfaceId {
    /// Construct a packet-interface identifier from its complete wire value.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Raw one-byte interface identifier.
    pub const fn get(self) -> u8 {
        self.0
    }

    pub(crate) const fn into_rns(self) -> RnsInterfaceId {
        RnsInterfaceId(self.0)
    }
}

/// Optional physical-link values observed for one complete ingress packet.
///
/// Radio interfaces supply whole-dB RSSI and SNR. Stream and other
/// interfaces leave this observation absent instead of fabricating values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketSignalObservation {
    rssi_dbm: i16,
    snr_db: i16,
}

impl PacketSignalObservation {
    /// Construct one complete physical-link observation.
    pub const fn new(rssi_dbm: i16, snr_db: i16) -> Self {
        Self { rssi_dbm, snr_db }
    }

    /// Received signal strength in whole dBm.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Signal-to-noise ratio in whole dB.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }

    pub(crate) const fn into_rns(self) -> RnsIngressSignalObservation {
        RnsIngressSignalObservation::new(self.rssi_dbm, self.snr_db)
    }

    pub(crate) const fn from_rns(signal: RnsIngressSignalObservation) -> Self {
        Self::new(signal.rssi_dbm(), signal.snr_db())
    }
}

/// Trust boundary for one packet entering RNS processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketIngressOrigin {
    /// Packet supplied by a trusted local client for routing by this node.
    LocalOrigin,
    /// Packet received from another Reticulum peer through an interface.
    RemoteInterface,
}

impl PacketIngressOrigin {
    pub(crate) const fn into_rns(self) -> RnsIngressOrigin {
        match self {
            Self::LocalOrigin => RnsIngressOrigin::LocalOrigin,
            Self::RemoteInterface => RnsIngressOrigin::RemoteInterface,
        }
    }

    pub(crate) const fn from_rns(origin: RnsIngressOrigin) -> Self {
        match origin {
            RnsIngressOrigin::LocalOrigin => Self::LocalOrigin,
            RnsIngressOrigin::RemoteInterface => Self::RemoteInterface,
        }
    }
}

/// Exact interface provenance and optional signal values for one ingress packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketIngressObservation {
    interface: PacketInterfaceId,
    signal: Option<PacketSignalObservation>,
    origin: PacketIngressOrigin,
}

impl PacketIngressObservation {
    /// Construct explicit remote-interface packet provenance.
    pub const fn remote(
        interface: PacketInterfaceId,
        signal: Option<PacketSignalObservation>,
    ) -> Self {
        Self {
            interface,
            signal,
            origin: PacketIngressOrigin::RemoteInterface,
        }
    }

    /// Construct one trusted local-origin injection.
    pub const fn local_origin(interface: PacketInterfaceId) -> Self {
        Self {
            interface,
            signal: None,
            origin: PacketIngressOrigin::LocalOrigin,
        }
    }

    /// Interface that supplied the packet.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Physical-link values, when supplied by the interface.
    pub const fn signal(self) -> Option<PacketSignalObservation> {
        self.signal
    }

    /// Whether the packet came from a local injector or a remote peer.
    pub const fn origin(self) -> PacketIngressOrigin {
        self.origin
    }

    pub(crate) const fn into_rns(self) -> RnsIngressObservation {
        let interface = self.interface.into_rns();
        match self.origin.into_rns() {
            RnsIngressOrigin::LocalOrigin => RnsIngressObservation::local_origin(interface),
            RnsIngressOrigin::RemoteInterface => RnsIngressObservation::remote(
                interface,
                match self.signal {
                    Some(signal) => Some(signal.into_rns()),
                    None => None,
                },
            ),
        }
    }

    pub(crate) const fn from_rns(observation: RnsIngressObservation) -> Self {
        Self {
            interface: PacketInterfaceId::new(observation.interface().0),
            signal: match observation.signal() {
                Some(signal) => Some(PacketSignalObservation::from_rns(signal)),
                None => None,
            },
            origin: PacketIngressOrigin::from_rns(observation.origin()),
        }
    }
}

/// Copy-only projection of one route currently retained by the RNS path table.
///
/// This record is routing state, not a reachability or peer-liveness claim.
/// The interface is the ingress identifier retained with the accepted path;
/// callers must resolve it against their current authoritative interface
/// registry before describing the route as usable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedRouteSnapshot {
    destination: DestinationHash,
    via: Option<[u8; 16]>,
    hops: u8,
    received_on: Option<PacketInterfaceId>,
    learned_at: MonotonicSeconds,
    last_accessed_at: MonotonicSeconds,
    expires_after_seconds: u64,
}

impl RetainedRouteSnapshot {
    pub(crate) fn from_rns(route: RnsRouteSnapshot) -> Self {
        Self {
            destination: DestinationHash::new(*route.destination.as_bytes()),
            via: route.via.map(|identity| *identity.as_bytes()),
            hops: route.hops,
            received_on: route
                .received_on
                .map(|interface| PacketInterfaceId::new(interface.0)),
            learned_at: MonotonicSeconds::new(route.learned_at_seconds),
            last_accessed_at: MonotonicSeconds::new(route.last_accessed_at_seconds),
            expires_after_seconds: route.expires_after_seconds,
        }
    }

    /// Destination addressed by this retained route.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Next-hop transport identity, or `None` for a direct route.
    pub const fn via(self) -> Option<[u8; 16]> {
        self.via
    }

    /// Reticulum hop count. A direct route has one hop.
    pub const fn hops(self) -> u8 {
        self.hops
    }

    /// Exact retained interface, or `None` for broadcast fallback.
    pub const fn received_on(self) -> Option<PacketInterfaceId> {
        self.received_on
    }

    /// Monotonic whole-second sample when this route candidate was installed.
    pub const fn learned_at(self) -> MonotonicSeconds {
        self.learned_at
    }

    /// Rete's monotonic LRU timestamp for local route access.
    ///
    /// This is not a last-heard or peer-activity timestamp.
    pub const fn last_accessed_at(self) -> MonotonicSeconds {
        self.last_accessed_at
    }

    /// Duration after learning before periodic RNS maintenance expires the route.
    pub const fn expires_after_seconds(self) -> u64 {
        self.expires_after_seconds
    }
}

/// Fixed set of the first 64 packet interfaces.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InterfaceSet(u64);

impl InterfaceSet {
    /// Empty interface set.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Construct a set from its complete bit representation.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Complete bit representation of this set.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether this set contains no interfaces.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether this compact set contains an interface.
    ///
    /// Identifiers above 63 are outside this profile and return `false`.
    pub const fn contains(self, interface: PacketInterfaceId) -> bool {
        let index = interface.get();
        index < 64 && self.0 & (1u64 << index) != 0
    }

    /// Add an interface when it fits the compact set.
    ///
    /// `None` reports an identifier above 63 without altering the set.
    pub const fn with(self, interface: PacketInterfaceId) -> Option<Self> {
        let index = interface.get();
        if index < 64 {
            Some(Self(self.0 | (1u64 << index)))
        } else {
            None
        }
    }

    /// Remove an interface when its identifier fits the compact set.
    ///
    /// `None` reports an identifier above 63 without altering the set.
    pub const fn without(self, interface: PacketInterfaceId) -> Option<Self> {
        let index = interface.get();
        if index < 64 {
            Some(Self(self.0 & !(1u64 << index)))
        } else {
            None
        }
    }

    /// Retain only interfaces present in both compact sets.
    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub(crate) fn take_first(&mut self) -> Option<PacketInterfaceId> {
        if self.0 == 0 {
            return None;
        }
        let index = self.0.trailing_zeros() as u8;
        self.0 &= self.0 - 1;
        Some(PacketInterfaceId::new(index))
    }
}

/// Interface authorization retained with one prepared RNS packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxTarget {
    /// Send on every eligible packet interface.
    All,
    /// Send only on one packet interface.
    Only(PacketInterfaceId),
    /// Send on every eligible interface except one packet interface.
    AllExcept(PacketInterfaceId),
    /// Send only on one explicit compact set of packet interfaces.
    Selected(InterfaceSet),
}

impl TxTarget {
    pub(crate) fn from_rns(target: RnsTxTarget) -> Self {
        match target {
            RnsTxTarget::All => Self::All,
            RnsTxTarget::Only(interface) => Self::Only(PacketInterfaceId::new(interface.0)),
            RnsTxTarget::AllExcept(interface) => {
                Self::AllExcept(PacketInterfaceId::new(interface.0))
            }
            RnsTxTarget::Selected(interfaces) => {
                Self::Selected(InterfaceSet::from_bits(interfaces.bits()))
            }
        }
    }
}

/// Failure to resolve an RNS target against the enabled packet interfaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxRouteError {
    /// A target named an interface outside the compact 0-through-63 profile.
    InterfaceOutsideProfile {
        /// Complete interface identifier that could not be represented.
        interface: PacketInterfaceId,
    },
    /// No enabled packet interface satisfies the target.
    NoEligibleInterface {
        /// Target that resolved to an empty route.
        target: TxTarget,
    },
}

/// Deterministic remaining interfaces for one serialized packet fan-out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxRoutePlan {
    remaining: InterfaceSet,
}

impl TxRoutePlan {
    /// Resolve a target against one synchronous enabled-interface snapshot.
    pub fn resolve(target: TxTarget, enabled: InterfaceSet) -> Result<Self, TxRouteError> {
        let remaining = match target {
            TxTarget::All => enabled,
            TxTarget::Only(interface) => {
                let singleton = InterfaceSet::empty()
                    .with(interface)
                    .ok_or(TxRouteError::InterfaceOutsideProfile { interface })?;
                enabled.intersect(singleton)
            }
            TxTarget::AllExcept(interface) => enabled
                .without(interface)
                .ok_or(TxRouteError::InterfaceOutsideProfile { interface })?,
            TxTarget::Selected(interfaces) => enabled.intersect(interfaces),
        };
        if remaining.is_empty() {
            Err(TxRouteError::NoEligibleInterface { target })
        } else {
            Ok(Self { remaining })
        }
    }

    /// Interfaces not yet selected for this serialized fan-out.
    pub const fn remaining(self) -> InterfaceSet {
        self.remaining
    }

    pub(crate) fn take_next(&mut self) -> Option<PacketInterfaceId> {
        self.remaining.take_first()
    }
}

/// Complete caller-owned inputs for one destination-DATA preparation.
///
/// The Reticulum clock and packet-owner clock are deliberately separate. The
/// owner deadline must be strictly later than `owner_now` when preparation
/// begins.
pub struct PrepareDataRequest<'a> {
    /// Complete destination hash for the encrypted DATA packet.
    pub destination: DestinationHash,
    /// Plaintext to encrypt into one base-MTU DATA packet.
    pub plaintext: &'a [u8],
    /// Current monotonic whole-second sample supplied to RNS.
    pub rns_now: MonotonicSeconds,
    /// Current monotonic millisecond sample for owner-lease validation.
    pub owner_now: MonotonicMillis,
    /// Deadline for returning the unique packet-buffer owner.
    pub deadline: TxLeaseDeadline,
    /// Synchronous snapshot of packet interfaces eligible for this dispatch.
    pub enabled_interfaces: InterfaceSet,
}

#[derive(Clone, Copy)]
pub(crate) enum DataPreparationMode {
    Generic,
    RehydratedOpportunisticLxmf,
    RehydratedDirectLxmf(RnsLinkId),
}

/// Whether this node terminates only local traffic or also forwards RNS
/// traffic for other nodes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeRole {
    /// Local endpoint only.
    Endpoint,
    /// Reticulum transport node.
    Transport,
}

/// Automatic delivery-proof behavior for DATA received at a local destination.
///
/// The default is fail-quiet: inbound DATA is delivered to the application but
/// does not cause an automatic transmission. Firmware must opt in explicitly
/// when delivery proofs are part of its configured Reticulum service policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InboundProofPolicy {
    /// Never generate delivery proofs automatically.
    #[default]
    Never,
    /// Generate a delivery proof for every successfully decrypted DATA packet.
    Always,
    /// Retain the generated proof with its application event until durable
    /// application policy explicitly releases it.
    Retain,
}

impl InboundProofPolicy {
    pub(crate) const fn into_rns(self) -> RnsInboundProofPolicy {
        match self {
            Self::Never => RnsInboundProofPolicy::Never,
            Self::Always => RnsInboundProofPolicy::Always,
            Self::Retain => RnsInboundProofPolicy::Retain,
        }
    }
}

/// Construction policy for the bounded node owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeConfig {
    /// Endpoint or transport behavior.
    pub role: NodeRole,
    /// Maximum destinations registered in addition to the primary one.
    pub max_additional_destinations: usize,
    /// Bitmask of interface indices treated as shared-medium.
    ///
    /// Shared-medium routes (for example LoRa) are reserved against eviction by
    /// point-to-point announce churn in the bounded native path table.
    pub shared_medium_interfaces: u64,
}

impl NodeConfig {
    /// Conservative endpoint profile.
    pub const fn endpoint() -> Self {
        Self {
            role: NodeRole::Endpoint,
            max_additional_destinations: 4,
            shared_medium_interfaces: 0,
        }
    }

    /// Transport profile with the same initial local-destination quota.
    pub const fn transport() -> Self {
        Self {
            role: NodeRole::Transport,
            max_additional_destinations: 4,
            shared_medium_interfaces: 0,
        }
    }

    /// Mark shared-medium interface indices, returning the profile.
    pub const fn with_shared_medium_interfaces(mut self, mask: u64) -> Self {
        self.shared_medium_interfaces = mask;
        self
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::endpoint()
    }
}

/// Failure to construct the concrete RNS owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeConstructionError {
    /// RNS rejected the application name, aspects, identity, or destination.
    InvalidRnsConfiguration,
}

/// Failure to register one additional inbound single-identity destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalDestinationRegistrationError {
    /// The construction-time additional-destination quota is exhausted.
    LimitReached {
        /// Configured maximum additional destinations.
        limit: usize,
    },
    /// RNS rejected the application name, aspects, or destination construction.
    InvalidRnsConfiguration,
}

impl From<RnsDestinationRegistrationError> for LocalDestinationRegistrationError {
    fn from(error: RnsDestinationRegistrationError) -> Self {
        match error {
            RnsDestinationRegistrationError::LimitReached { limit } => Self::LimitReached { limit },
            RnsDestinationRegistrationError::Rete(_) => Self::InvalidRnsConfiguration,
        }
    }
}

/// Failure to alias an authenticated peer identity under its canonical
/// `rnstransport.probe` destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProofProbeIdentityAliasError {
    /// No authenticated identity is retained for the supplied announced
    /// destination.
    SourceIdentityUnknown,
    /// The derived probe destination already retains different key material.
    DestinationIdentityConflict,
    /// The bounded native identity table could not retain the alias.
    IdentityCapacityUnavailable,
    /// Temporary identity-only snapshot storage could not be reserved.
    AllocationFailed,
}

impl From<RnsProofProbeIdentityAliasError> for ProofProbeIdentityAliasError {
    fn from(error: RnsProofProbeIdentityAliasError) -> Self {
        match error {
            RnsProofProbeIdentityAliasError::SourceIdentityUnknown => Self::SourceIdentityUnknown,
            RnsProofProbeIdentityAliasError::DestinationIdentityConflict => {
                Self::DestinationIdentityConflict
            }
            RnsProofProbeIdentityAliasError::IdentityCapacityUnavailable => {
                Self::IdentityCapacityUnavailable
            }
            RnsProofProbeIdentityAliasError::AllocationFailed => Self::AllocationFailed,
        }
    }
}

/// Failure to configure default announce application data for one local
/// destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalDestinationAnnounceAppDataError {
    /// The destination is not registered on this node.
    DestinationNotRegistered,
    /// Application data cannot fit in one base-MTU announce.
    AppDataTooLarge {
        /// Supplied application-data length.
        actual: usize,
        /// Maximum supported application-data length.
        maximum: usize,
    },
    /// Owned default application-data storage could not be reserved.
    AllocationFailed,
}

impl From<RnsAnnounceAppDataError> for LocalDestinationAnnounceAppDataError {
    fn from(error: RnsAnnounceAppDataError) -> Self {
        match error {
            RnsAnnounceAppDataError::DestinationNotRegistered => Self::DestinationNotRegistered,
            RnsAnnounceAppDataError::AppDataTooLarge { actual, maximum } => {
                Self::AppDataTooLarge { actual, maximum }
            }
            RnsAnnounceAppDataError::AllocationFailed => Self::AllocationFailed,
        }
    }
}

/// Failure to construct one tagged Reticulum path-request action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathRequestError {
    /// The fixed request could not be encoded into one base-MTU packet.
    PacketBuild,
    /// Owned packet storage could not be reserved.
    AllocationFailed,
}

impl From<RnsPathRequestBuildError> for PathRequestError {
    fn from(error: RnsPathRequestBuildError) -> Self {
        match error {
            RnsPathRequestBuildError::PacketBuild => Self::PacketBuild,
            RnsPathRequestBuildError::AllocationFailed => Self::AllocationFailed,
        }
    }
}

/// Failure to configure delivery-proof behavior for one local destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalDestinationProofPolicyError {
    /// The destination is not registered on this node.
    DestinationNotRegistered,
    /// Retained proofs require an inbound encrypted Single destination.
    RetainRequiresInboundSingle,
}

/// Failure to configure inbound Link admission for one local destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalDestinationLinkPolicyError {
    /// The destination is not registered on this node.
    DestinationNotRegistered,
}

/// Failure to bind a local announce destination to one exact packet interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalDestinationAnnounceTargetError {
    /// The named destination is not registered on this node.
    DestinationNotRegistered,
}

impl From<RnsLocalAnnounceTargetError> for LocalDestinationAnnounceTargetError {
    fn from(error: RnsLocalAnnounceTargetError) -> Self {
        match error {
            RnsLocalAnnounceTargetError::DestinationNotRegistered => Self::DestinationNotRegistered,
        }
    }
}

impl From<RnsInboundProofPolicyError> for LocalDestinationProofPolicyError {
    fn from(error: RnsInboundProofPolicyError) -> Self {
        match error {
            RnsInboundProofPolicyError::DestinationNotRegistered => Self::DestinationNotRegistered,
            RnsInboundProofPolicyError::RetainRequiresInboundSingle => {
                Self::RetainRequiresInboundSingle
            }
        }
    }
}

/// Failure to cache one peer identity for deterministic direct sending.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerRegistrationError {
    /// RNS rejected the peer name, aspects, or identity.
    InvalidPeer,
}

/// Failure to admit one local announce into the bounded protocol queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnounceAdmissionError {
    /// Optional application data cannot fit in a base-MTU announce.
    AppDataTooLarge {
        /// Supplied application-data length in bytes.
        actual: usize,
        /// Maximum accepted application-data length in bytes.
        maximum: usize,
    },
    /// Every configured pending-announce slot is occupied.
    QueueFull {
        /// Configured pending-announce capacity.
        limit: usize,
    },
    /// The native protocol implementation rejected announce construction or
    /// queueing after product-owned admission checks passed.
    NativeRejected,
}

impl From<RnsAnnounceAdmissionError> for AnnounceAdmissionError {
    fn from(error: RnsAnnounceAdmissionError) -> Self {
        match error {
            RnsAnnounceAdmissionError::AppDataTooLarge { actual, maximum } => {
                Self::AppDataTooLarge { actual, maximum }
            }
            RnsAnnounceAdmissionError::QueueFull { limit } => Self::QueueFull { limit },
            RnsAnnounceAdmissionError::NativeRejected => Self::NativeRejected,
        }
    }
}

/// Full RNS hash used to correlate one prepared DATA packet with its delivery
/// proof or timeout.
///
/// This is the Reticulum hashable-part digest covered by a proof. It is not a
/// SHA-256 digest of every byte in the encoded interface packet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AttemptToken(pub(crate) [u8; 32]);

impl AttemptToken {
    /// Borrow the complete proof-correlation hash.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Receipt table that owns one proof-correlated DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataReceiptKind {
    /// Encrypted DATA addressed directly to an RNS destination.
    DestinationData,
    /// Ordinary encrypted DATA carried by an authenticated Link.
    LinkData,
}

impl DataReceiptKind {
    pub(crate) fn from_rns(kind: ReceiptKind) -> Option<Self> {
        match kind {
            ReceiptKind::Data => Some(Self::DestinationData),
            ReceiptKind::LinkData => Some(Self::LinkData),
            ReceiptKind::Channel => None,
        }
    }
}

/// SHA-256 digest of every byte in one complete encoded interface packet.
///
/// This is deliberately distinct from [`AttemptToken`], whose bytes cover
/// Reticulum's proof-correlated hashable portion rather than the complete
/// encoded packet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EncodedPacketSha256([u8; 32]);

impl EncodedPacketSha256 {
    /// Construct a digest from SHA-256 over every encoded packet byte.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete full-packet digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Opaque, generation-checked identity of one submitted DATA attempt.
///
/// Unlike [`AttemptToken`], this handle remains unambiguous if packet hashes
/// are ever repeated after an earlier attempt has been acknowledged. It is
/// scoped to both the Reticulum identity and this in-memory node incarnation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AttemptHandle {
    pub(crate) owner_identity: [u8; 16],
    pub(crate) instance: NodeInstanceId,
    pub(crate) slot: usize,
    pub(crate) generation: u64,
}

/// Why a successfully prepared DATA attempt became definitely unsent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptUnsentReason {
    /// A prepared job was proven not to have entered the dispatcher handoff.
    QueueRollback,
    /// The selected interface's synchronous resource policy rejected the final hop.
    PolicyDenied(TxPolicyDenial),
    /// The packet-owner deadline expired before the final hop was permitted.
    PermitDeadlineExpired,
    /// The exact dispatch had already entered fail-closed recovery.
    RecoveryRequired,
    /// A definitely-unpermitted final hop completed without a richer denial.
    Unpermitted(TxCompletionCode),
}

/// Application-visible outcome for one destination-DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptOutcome {
    /// A valid Reticulum delivery proof was accepted.
    Delivered,
    /// The Reticulum delivery receipt expired without a proof.
    DeliveryTimeout,
    /// The complete serialized route ended without any permitted transmission.
    Unsent(AttemptUnsentReason),
}

/// One terminal attempt retained until the application acknowledges it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalAttempt {
    pub(crate) handle: AttemptHandle,
    pub(crate) token: AttemptToken,
    pub(crate) kind: DataReceiptKind,
    pub(crate) link: Option<LinkHandle>,
    pub(crate) outcome: AttemptOutcome,
    pub(crate) ingress: Option<PacketIngressObservation>,
}

impl TerminalAttempt {
    /// Opaque handle required to acknowledge this terminal record.
    pub const fn handle(self) -> AttemptHandle {
        self.handle
    }

    /// Complete RNS proof-correlation hash for this attempt.
    pub const fn token(self) -> AttemptToken {
        self.token
    }

    /// Receipt table that produced this terminal outcome.
    pub const fn receipt_kind(self) -> DataReceiptKind {
        self.kind
    }

    /// Exact Link that carried this attempt, when it used Link DATA.
    ///
    /// Destination DATA attempts return `None`. The handle remains available
    /// after a receipt timeout so the product can retire a session whose peer
    /// may have rebooted while native Link state still appears active.
    pub const fn link(self) -> Option<LinkHandle> {
        self.link
    }

    /// Delivery outcome retained by the tombstone.
    pub const fn outcome(self) -> AttemptOutcome {
        self.outcome
    }

    /// Ingress packet that proved delivery, including optional final-hop
    /// physical-link values.
    ///
    /// Delivered attempts carry the proof packet's observation when ingress
    /// used the signal-aware API. Timeout and definitely-unsent outcomes have
    /// no ingress observation.
    pub const fn ingress(self) -> Option<PacketIngressObservation> {
        self.ingress
    }
}

/// Stable identifier assigned to one externally owned packet buffer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PacketSlotId(pub(crate) u16);

impl PacketSlotId {
    /// Numeric slot index assigned by the node owner.
    pub const fn get(self) -> u16 {
        self.0
    }

    pub(crate) fn index(self) -> usize {
        usize::from(self.0)
    }
}
