//! Transport-neutral orchestration for durable outbound Reticulum submissions.
//!
//! This crate joins the sole storage actor to a narrow native-packet node port.
//! It deliberately knows nothing about LoRa, RNode framing, radios, boards,
//! local client transports, executors, or firmware. A product task can own this
//! runtime beside the permanent node supervisor and enforce the two critical
//! ordering rules:
//!
//! 1. persist `Queued -> Preparing` before asking the node to allocate entropy
//!    or create a packet attempt; and
//! 2. persist frame, terminal, and recovery observations before acknowledging
//!    the exact upstream owner.

#![no_std]
#![deny(unsafe_code)]
#![deny(missing_docs)]

use core::mem::MaybeUninit;

use embassy_sync::blocking_mutex::raw::RawMutex;
use rand_core::{CryptoRng, RngCore};
use reticulum_node_core::{
    AcknowledgeError, AuthorizedFrameObservation, DestinationHash, LinkHandle, LinkState,
    MAX_OPPORTUNISTIC_LXMF_CARRIER, MonotonicMillis, MonotonicSeconds, SubmitError,
    TerminalAttempt, TxAuthorizationPolicy, TxLeaseDeadline, TxRecoveryObservation,
};
use reticulum_storage_actor::{
    AcceptanceProgress, BootRecoveryProgress, BoundJournalAccess, DriveError, MountError,
    PendingProgress, ProjectorOperationError, StorageActor,
};
use reticulum_storage_model::{
    AcceptanceCandidate, LifecycleState, SubmissionId, SubmissionIndex, SubmissionIntent,
};
use reticulum_submission_projector::{
    AcknowledgementAction, AcknowledgementKind, AcknowledgementReply, PersistenceProgress,
    PreparedFrameObservation, ProjectionProgress, ProjectorError, SubmissionPreparationObservation,
};
use reticulum_tx_supervisor::{
    DataRecoveryAckError, DataRouterPrepareRequest, DataRouterPrepareResult,
    NodeInterfaceDataPrepareResult, NodeInterfaceSupervisor,
};

/// Native Reticulum DATA inputs supplied to the permanent node owner.
///
/// There is intentionally no concrete interface identifier here. The node's
/// authoritative router chooses from its current eligible-interface snapshot.
pub struct SubmissionPrepareRequest<'a> {
    /// Complete destination hash for the encrypted DATA packet.
    pub destination: DestinationHash,
    /// Method-specific exact bytes.
    ///
    /// Generic submission preparation receives destination-DATA plaintext.
    /// Rehydrated opportunistic LXMF preparation receives the complete
    /// destination-prefixed signed wire message.
    pub plaintext: &'a [u8],
    /// Current Reticulum protocol clock.
    pub rns_now: MonotonicSeconds,
    /// Current packet-owner monotonic clock.
    pub owner_now: MonotonicMillis,
    /// Deadline for returning or retaining the exact packet owner.
    pub deadline: TxLeaseDeadline,
}

/// Narrow transport-neutral port implemented by the permanent node owner.
///
/// Implementations must retain every owner represented by a preparation or
/// lifecycle observation until the runtime reports the exact acknowledgement.
/// An interface adapter may route the prepared packet to LoRa, Wi-Fi, BLE, USB,
/// or several interfaces without changing this contract.
pub trait SubmissionNodePort {
    /// Prepare one native destination-DATA attempt through the authoritative
    /// interface router.
    fn prepare_submission<R>(
        &mut self,
        request: SubmissionPrepareRequest<'_>,
        rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng;

    /// Prepare one exact durable LXMF wire message through the native
    /// opportunistic Header-1 exception.
    ///
    /// `request.plaintext` contains the complete destination-prefixed signed
    /// wire message, not generic destination-DATA plaintext.
    fn prepare_rehydrated_opportunistic_lxmf_submission<R>(
        &mut self,
        request: SubmissionPrepareRequest<'_>,
        rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng;

    /// Prepare one exact durable LXMF wire message through an already-active,
    /// product-retained Link.
    ///
    /// `request.plaintext` contains the unchanged destination-prefixed signed
    /// wire message. Implementations must route the resulting LinkData packet
    /// through the same durable DATA owner path as opportunistic delivery.
    fn prepare_rehydrated_direct_lxmf_submission<R>(
        &mut self,
        link: LinkHandle,
        request: SubmissionPrepareRequest<'_>,
        rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng;

    /// Whether the node currently retains an RNS path whose bound interface is
    /// eligible for a new outbound Link request.
    fn has_usable_path(&self, destination: &DestinationHash) -> bool;

    /// Hop count of the currently retained path, when known.
    ///
    /// The runtime snapshots this value into an establishment offer so later
    /// route changes cannot silently change an attached Link's deadline.
    fn retained_path_hops(&self, destination: &DestinationHash) -> Option<u8>;

    /// Maximum first-hop MTU serialization time across the usable retained
    /// route's currently eligible interfaces.
    ///
    /// `Some(0)` represents Reticulum's fallback when bitrate is unknown;
    /// `None` means no usable route can currently be resolved.
    fn retained_path_first_hop_serialization_ms(
        &self,
        destination: &DestinationHash,
    ) -> Option<u64>;

    /// Whether the native node can currently retain one more outbound Link.
    ///
    /// This is separate from the runtime's reusable-Link registry capacity:
    /// both bounded owners must have room before `Auto` starts establishment.
    fn can_initiate_link(&self) -> bool;

    /// Read the current lifecycle state of one exact product-retained Link.
    fn link_state(&self, link: LinkHandle) -> Option<LinkState>;

    /// Whether one active Link is currently compatible with an authoritative
    /// eligible Reticulum interface.
    ///
    /// An active Link bound to an offline interface remains retained, but
    /// `Auto` must not select it for a new packet.
    fn link_is_usable(&self, link: LinkHandle) -> bool;

    /// Abort one exact Link only while it remains unestablished.
    ///
    /// Returning `false` must leave active, stale, closed, or unknown Links
    /// untouched.
    fn abort_unestablished_link(&mut self, link: LinkHandle) -> bool;

    /// Iterate exact terminal attempts retained by the node owner.
    fn terminal_attempts(&self) -> impl Iterator<Item = TerminalAttempt> + '_;

    /// Iterate recovered packet owners awaiting durable audit and release.
    fn recovered_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_;

    /// Iterate fail-closed quarantines. Quarantines have no release action.
    fn quarantined_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_;

    /// Attempt the exact acknowledgement unlocked by durable projection.
    ///
    /// `Completed` means the named upstream owner was released, `Retryable`
    /// means it is still safely retained, and `Error` is a stable adapter-owned
    /// diagnostic that must fail the durable projector closed.
    fn acknowledge(&mut self, action: AcknowledgementAction) -> AcknowledgementReply;
}

impl<
    M,
    P,
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const DATA_PACKET_BUFFERS: usize,
    const ORDINARY_PACKET_BUFFERS: usize,
    const SLOTS: usize,
    const QUEUE_DEPTH: usize,
> SubmissionNodePort
    for NodeInterfaceSupervisor<
        M,
        P,
        PATHS,
        ANNOUNCES,
        DEDUPLICATION,
        LINKS,
        DATA_PACKET_BUFFERS,
        ORDINARY_PACKET_BUFFERS,
        SLOTS,
        QUEUE_DEPTH,
    >
where
    M: RawMutex + 'static,
    P: TxAuthorizationPolicy,
{
    fn prepare_submission<R>(
        &mut self,
        request: SubmissionPrepareRequest<'_>,
        rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng,
    {
        map_preparation_result(self.try_prepare_data(
            DataRouterPrepareRequest {
                destination: request.destination,
                plaintext: request.plaintext,
                rns_now: request.rns_now,
                deadline: request.deadline,
            },
            request.owner_now,
            rng,
        ))
    }

    fn prepare_rehydrated_opportunistic_lxmf_submission<R>(
        &mut self,
        request: SubmissionPrepareRequest<'_>,
        rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng,
    {
        map_preparation_result(self.try_prepare_rehydrated_opportunistic_lxmf(
            DataRouterPrepareRequest {
                destination: request.destination,
                plaintext: request.plaintext,
                rns_now: request.rns_now,
                deadline: request.deadline,
            },
            request.owner_now,
            rng,
        ))
    }

    fn prepare_rehydrated_direct_lxmf_submission<R>(
        &mut self,
        link: LinkHandle,
        request: SubmissionPrepareRequest<'_>,
        rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng,
    {
        map_preparation_result(self.try_prepare_rehydrated_direct_lxmf(
            link,
            DataRouterPrepareRequest {
                destination: request.destination,
                plaintext: request.plaintext,
                rns_now: request.rns_now,
                deadline: request.deadline,
            },
            request.owner_now,
            rng,
        ))
    }

    fn has_usable_path(&self, destination: &DestinationHash) -> bool {
        NodeInterfaceSupervisor::has_usable_path(self, destination)
    }

    fn retained_path_hops(&self, destination: &DestinationHash) -> Option<u8> {
        NodeInterfaceSupervisor::retained_path_hops(self, destination)
    }

    fn retained_path_first_hop_serialization_ms(
        &self,
        destination: &DestinationHash,
    ) -> Option<u64> {
        NodeInterfaceSupervisor::retained_path_first_hop_serialization_ms(self, destination)
    }

    fn can_initiate_link(&self) -> bool {
        NodeInterfaceSupervisor::can_initiate_link(self)
    }

    fn link_state(&self, link: LinkHandle) -> Option<LinkState> {
        NodeInterfaceSupervisor::link_state(self, link)
    }

    fn link_is_usable(&self, link: LinkHandle) -> bool {
        NodeInterfaceSupervisor::link_is_usable(self, link)
    }

    fn abort_unestablished_link(&mut self, link: LinkHandle) -> bool {
        NodeInterfaceSupervisor::abort_unestablished_link(self, link)
    }

    fn terminal_attempts(&self) -> impl Iterator<Item = TerminalAttempt> + '_ {
        NodeInterfaceSupervisor::terminal_attempts(self)
    }

    fn recovered_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.recovered_data_observations()
    }

    fn quarantined_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.quarantined_data_observations()
    }

    fn acknowledge(&mut self, action: AcknowledgementAction) -> AcknowledgementReply {
        match action.kind() {
            AcknowledgementKind::Terminal(expected) => {
                match self.acknowledge_terminal(expected.handle()) {
                    Ok(actual) if actual == expected => AcknowledgementReply::Completed,
                    Ok(_) => AcknowledgementReply::Error(TERMINAL_ACK_RESULT_MISMATCH),
                    Err(AcknowledgeError::PacketStillBound) => AcknowledgementReply::Retryable,
                    Err(AcknowledgeError::StaleOrUnknown) => {
                        AcknowledgementReply::Error(TERMINAL_ACK_STALE_OR_UNKNOWN)
                    }
                    Err(AcknowledgeError::NotTerminal) => {
                        AcknowledgementReply::Error(TERMINAL_ACK_NOT_TERMINAL)
                    }
                    Err(AcknowledgeError::Invariant) => {
                        AcknowledgementReply::Error(TERMINAL_ACK_INVARIANT)
                    }
                }
            }
            AcknowledgementKind::Recovered(observation) => {
                match self.acknowledge_recovered_data(observation) {
                    Ok(()) => AcknowledgementReply::Completed,
                    Err(DataRecoveryAckError::SlotOutsidePool) => {
                        AcknowledgementReply::Error(RECOVERY_ACK_SLOT_OUTSIDE_POOL)
                    }
                    Err(DataRecoveryAckError::NotRecovered) => {
                        AcknowledgementReply::Error(RECOVERY_ACK_NOT_RECOVERED)
                    }
                    Err(DataRecoveryAckError::ObservationMismatch) => {
                        AcknowledgementReply::Error(RECOVERY_ACK_OBSERVATION_MISMATCH)
                    }
                }
            }
        }
    }
}

const TERMINAL_ACK_STALE_OR_UNKNOWN: u16 = 0x1001;
const TERMINAL_ACK_NOT_TERMINAL: u16 = 0x1002;
const TERMINAL_ACK_INVARIANT: u16 = 0x1003;
const TERMINAL_ACK_RESULT_MISMATCH: u16 = 0x1004;
const RECOVERY_ACK_SLOT_OUTSIDE_POOL: u16 = 0x1101;
const RECOVERY_ACK_NOT_RECOVERED: u16 = 0x1102;
const RECOVERY_ACK_OBSERVATION_MISMATCH: u16 = 0x1103;

fn map_preparation_result(
    result: NodeInterfaceDataPrepareResult,
) -> SubmissionPreparationObservation {
    match result {
        NodeInterfaceDataPrepareResult::Fault(_) => {
            SubmissionPreparationObservation::InternalFailure
        }
        NodeInterfaceDataPrepareResult::Coordinator(result) => match result {
            DataRouterPrepareResult::Prepared(hop) => {
                SubmissionPreparationObservation::Prepared(hop.prepared())
            }
            DataRouterPrepareResult::Rejected { reason, .. } => {
                SubmissionPreparationObservation::Rejected(reason)
            }
            DataRouterPrepareResult::RejectedQuarantined { observation, .. } => {
                SubmissionPreparationObservation::Quarantined(observation)
            }
            DataRouterPrepareResult::NoAvailable => SubmissionPreparationObservation::RetrySameBoot,
            DataRouterPrepareResult::OwnerMismatch | DataRouterPrepareResult::Disabled(_) => {
                SubmissionPreparationObservation::InternalFailure
            }
        },
    }
}

/// Whether the runtime can admit or advance live submissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimePhase {
    /// Complete journal replay is mounted, but interrupted submissions still
    /// require conservative durable finalization.
    Recovering,
    /// Boot recovery is complete and live service may begin.
    Ready,
}

/// One bounded boot-recovery transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryStep {
    /// One replayed submission reached its definitive boot disposition.
    Submission {
        /// Durable submission examined by this step.
        id: SubmissionId,
        /// Replay-safe, already-final, or newly finalized outcome.
        progress: BootRecoveryProgress,
    },
    /// No unreconciled replayed submission remains; service is now open.
    Complete,
    /// Service had already completed boot recovery.
    AlreadyComplete,
}

/// Why a live service operation could not be performed.
#[derive(Debug, Eq, PartialEq)]
pub enum RuntimeError<E> {
    /// Boot recovery has not completed, so live service remains gated.
    Recovering,
    /// Physical durable mutation failed or remains ambiguous.
    Storage(DriveError<E>),
    /// The actor-owned lifecycle projector rejected the operation.
    Projection(ProjectorOperationError),
}

impl<E> From<DriveError<E>> for RuntimeError<E> {
    fn from(error: DriveError<E>) -> Self {
        Self::Storage(error)
    }
}

impl<E> From<ProjectorOperationError> for RuntimeError<E> {
    fn from(error: ProjectorOperationError) -> Self {
        Self::Projection(error)
    }
}

/// Failure from a backend-free runtime control operation.
///
/// Frame observations are deliberately independent of physical storage access:
/// they only update the actor-owned projector and retain any resulting exact
/// persistence request for a later [`SubmissionRuntime::drive_step`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeControlError {
    /// Boot recovery has not completed, so live service remains gated.
    Recovering,
    /// The actor-owned lifecycle projector rejected the operation permanently
    /// or entered a fail-closed fault.
    Projection(ProjectorOperationError),
}

impl From<ProjectorOperationError> for RuntimeControlError {
    fn from(error: ProjectorOperationError) -> Self {
        Self::Projection(error)
    }
}

/// Result of offering one retained complete native-frame observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameOfferProgress {
    /// The observation is durably represented and its source may release it.
    Durable,
    /// The source must retain and offer the identical observation again after
    /// the runtime advances the actor's pending persistence or acknowledgement.
    Retain,
}

/// Wait after an emitted path request before probing destination preparation
/// again. This matches Python LXMF's opportunistic path-request wait.
pub const PATH_DISCOVERY_RESPONSE_WAIT_MS: u64 = 7_000;

/// Minimum separation between fresh path requests for one destination.
///
/// Pinned Rete and Python Reticulum throttle automated requests for twenty
/// seconds, so the sender waits one additional second before its retry.
pub const PATH_DISCOVERY_RETRY_INTERVAL_MS: u64 = 21_000;

/// Maximum number of tagged path requests emitted for one submission in a
/// single boot before `NoPath` becomes terminal.
pub const PATH_DISCOVERY_MAX_REQUESTS: u8 = 2;

/// Product-owned minimum deadline for one outbound Link to become active.
///
/// The clock starts only after the exact LinkRequest's first real interface
/// dispatch is acknowledged. Until then, ordinary-router backpressure cannot
/// consume the establishment budget.
pub const DIRECT_LINK_ESTABLISHMENT_TIMEOUT_MS: u64 = 30_000;

/// Conservative Rete-compatible Link establishment allowance per route stage.
///
/// One interval covers the first-hop response allowance and one covers each
/// retained path hop. The route supervisor supplies the separate first-hop MTU
/// serialization allowance from authoritative interface metadata.
pub const DIRECT_LINK_ESTABLISHMENT_PER_HOP_MS: u64 = 6_000;

/// Product scheduling guard between router acceptance and physical TX
/// completion.
///
/// Reticulum's first-hop serialization term covers nominal airtime. The
/// permanent firmware confirms dispatch when the interface actor accepts the
/// packet, so CAD, radio setup, and bounded driver variance need a separate
/// transport-neutral allowance.
pub const DIRECT_LINK_ESTABLISHMENT_DISPATCH_GUARD_MS: u64 = 2_000;

const fn direct_link_establishment_timeout(
    retained_path_hops: Option<u8>,
    first_hop_serialization_ms: Option<u64>,
) -> u64 {
    let hops = match retained_path_hops {
        Some(hops) if hops > 0 => hops as u64,
        Some(_) | None => 1,
    };
    let serialization_ms = match first_hop_serialization_ms {
        Some(milliseconds) => milliseconds,
        None => 0,
    };
    let route_window = (hops + 1)
        .saturating_mul(DIRECT_LINK_ESTABLISHMENT_PER_HOP_MS)
        .saturating_add(serialization_ms)
        .saturating_add(DIRECT_LINK_ESTABLISHMENT_DISPATCH_GUARD_MS);
    if route_window > DIRECT_LINK_ESTABLISHMENT_TIMEOUT_MS {
        route_window
    } else {
        DIRECT_LINK_ESTABLISHMENT_TIMEOUT_MS
    }
}

/// Exact transport-neutral path-request offer awaiting first router dispatch.
///
/// The runtime does not start response or retry clocks when this value is
/// created. The product owner must return the unchanged offer through
/// [`SubmissionRuntime::acknowledge_path_request_dispatched`] only after one
/// eligible interface has accepted the corresponding packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathDiscoveryOffer {
    id: SubmissionId,
    destination: DestinationHash,
    ordinal: u8,
}

impl PathDiscoveryOffer {
    /// Durable submission whose preparation first requested this shared path.
    pub const fn id(self) -> SubmissionId {
        self.id
    }

    /// Destination shared by every submission waiting on this discovery.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// One-based request ordinal for this destination in the current boot.
    pub const fn ordinal(self) -> u8 {
        self.ordinal
    }
}

/// A router-dispatch acknowledgement did not match the exact pending offer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathDiscoveryAcknowledgeError {
    /// No discovery is currently retained for the offered destination.
    DestinationNotPending,
    /// The destination is pending, but a different offer owns its next edge.
    OfferMismatch,
}

/// Exact request to create one outbound initiator Link for a durable message.
///
/// The product owner must initiate the Link for [`Self::destination`], retain
/// the returned ordinary action envelope, and attach the returned opaque
/// handle through [`SubmissionRuntime::attach_created_link`]. The generation
/// prevents an old callback from attaching to a later retry for the same
/// submission and destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkEstablishmentOffer {
    id: SubmissionId,
    destination: DestinationHash,
    generation: u64,
    establishment_timeout_ms: u64,
}

impl LinkEstablishmentOffer {
    /// Durable submission waiting for this Link.
    pub const fn id(self) -> SubmissionId {
        self.id
    }

    /// LXMF delivery destination the new Link must target.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Boot-unique transaction generation used for exact callback matching.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Snapshotted hop-aware establishment window after first dispatch.
    pub const fn establishment_timeout_ms(self) -> u64 {
        self.establishment_timeout_ms
    }
}

/// A backend-free direct-Link control callback did not match its exact owner.
///
/// Every error leaves the pending transaction unchanged so the product can
/// retain its action envelope and fail closed without losing correlation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkEstablishmentControlError {
    /// Boot recovery has not completed, so live controls remain gated.
    Recovering,
    /// No serialized direct-Link transaction is currently retained.
    NoTransactionPending,
    /// A different offer owns the current direct-Link transaction.
    OfferMismatch,
    /// The callback is not valid in the transaction's current phase.
    WrongPhase,
    /// A different opaque Link handle is attached to the exact offer.
    LinkMismatch,
    /// An idempotent dispatch acknowledgement supplied a different timestamp.
    DispatchTimeMismatch,
}

/// One bounded useful transition selected by the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStep {
    /// An exact actor-owned ambiguous mutation was reconciled.
    Pending(PendingProgress),
    /// One exact projector record became durable.
    Persistence(PersistenceProgress),
    /// One durable upstream acknowledgement was attempted and reported.
    Acknowledgement {
        /// Exact action attempted.
        action: AcknowledgementAction,
        /// Adapter disposition reported to the projector.
        reply: AcknowledgementReply,
    },
    /// One terminal attempt was offered to durable projection.
    Terminal {
        /// Exact node terminal observation.
        terminal: TerminalAttempt,
        /// Projector disposition.
        progress: ProjectionProgress,
    },
    /// One recovered owner was offered to durable transport audit.
    Recovered {
        /// Exact recovered-owner observation.
        observation: TxRecoveryObservation,
        /// Projector disposition.
        progress: ProjectionProgress,
    },
    /// One quarantine was offered to durable transport audit.
    Quarantined {
        /// Exact quarantined-owner observation.
        observation: TxRecoveryObservation,
        /// Projector disposition.
        progress: ProjectionProgress,
    },
    /// One durable intent was offered to the permanent node owner.
    Preparation {
        /// Durable submission being prepared.
        id: SubmissionId,
        /// Projector disposition for the node's synchronous result.
        progress: ProjectionProgress,
    },
    /// An unknown destination remains durably `Preparing` while the product
    /// emits one tagged, transport-neutral Reticulum path request.
    PathDiscoveryRequest {
        /// Exact offer retained until the first interface accepts dispatch.
        offer: PathDiscoveryOffer,
        /// Projector disposition retaining the unbound `Preparing` barrier.
        progress: ProjectionProgress,
    },
    /// One exact product-owned outbound Link must be created and retained.
    LinkEstablishment {
        /// Exact offer to attach after successful native Link creation.
        offer: LinkEstablishmentOffer,
        /// Projector disposition retaining the durable `Preparing` barrier.
        progress: ProjectionProgress,
    },
    /// A bounded owner required for new Link establishment has no capacity.
    ///
    /// This covers both the product's reusable-Link registry and the native
    /// Link table. The durable message stays `Preparing`; the product should
    /// apply bounded retry backoff until admission becomes available.
    LinkCapacityBackpressured {
        /// Exact durable submission that could not start Link creation.
        id: SubmissionId,
        /// Configured reusable direct-Link registry capacity.
        limit: usize,
    },
    /// A dispatched Link did not establish before its bounded deadline.
    LinkEstablishmentExpired {
        /// Exact offer whose establishment window elapsed.
        offer: LinkEstablishmentOffer,
        /// Exact opaque Link handle passed to the abort operation.
        link: LinkHandle,
        /// Whether the node confirmed that it removed the unestablished Link.
        aborted: bool,
    },
    /// A retained Link disappeared or became unavailable before use.
    LinkEstablishmentLost {
        /// Exact offer whose retained Link can no longer establish.
        offer: LinkEstablishmentOffer,
        /// Exact opaque Link handle that was polled.
        link: LinkHandle,
        /// Last state returned by the node, or `None` if the handle vanished.
        state: Option<LinkState>,
    },
    /// One queued submission began its durable no-replay barrier.
    PreparationBarrier {
        /// Durable submission entering `Preparing`.
        id: SubmissionId,
        /// Projector disposition, normally an exact persistence handle.
        progress: ProjectionProgress,
    },
    /// No useful submission transition was currently available.
    Idle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PathDiscovery {
    destination: DestinationHash,
    phase: PathDiscoveryPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PathDiscoveryPhase {
    AwaitingDispatch {
        offer: PathDiscoveryOffer,
        first_request_ms: Option<u64>,
    },
    Waiting {
        first_request_ms: u64,
        next_probe_ms: u64,
        requests_sent: u8,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReusableDirectLink {
    destination: DestinationHash,
    link: LinkHandle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectLinkTransaction {
    phase: DirectLinkPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectLinkPhase {
    AwaitingLink {
        offer: LinkEstablishmentOffer,
    },
    AwaitingFirstDispatch {
        offer: LinkEstablishmentOffer,
        link: LinkHandle,
    },
    Establishing {
        offer: LinkEstablishmentOffer,
        link: LinkHandle,
        dispatched_at_ms: u64,
        deadline_ms: u64,
    },
}

/// Sole portable owner of durable submission state and scheduling phase.
///
/// The physical journal remains outside this value. Operations that can touch
/// NOR borrow a bound journal view for only that call, while projection-only
/// observations retain no backend borrow.
///
/// The permanent node owner is passed mutably to [`Self::drive_step`] so a
/// firmware task can keep node timers, ingress, and ordinary RNS actions in the
/// same coordinator without this crate depending on an executor or product
/// composition.
pub struct SubmissionRuntime<
    const SUBMISSIONS: usize,
    const PROJECTED: usize,
    const DIRECT_LINKS: usize = 4,
> {
    storage: StorageActor<SUBMISSIONS, PROJECTED>,
    boot_sequence: u64,
    recovery_cursor: u64,
    recovery_exhausted: bool,
    phase: RuntimePhase,
    path_discoveries: [Option<PathDiscovery>; SUBMISSIONS],
    direct_link: Option<DirectLinkTransaction>,
    reusable_direct_links: [Option<ReusableDirectLink>; DIRECT_LINKS],
    link_capacity_waiting: [bool; SUBMISSIONS],
    next_link_offer_generation: u64,
    resource_waiting: [SubmissionId; SUBMISSIONS],
    resource_waiting_len: usize,
}

impl<const SUBMISSIONS: usize, const PROJECTED: usize, const DIRECT_LINKS: usize>
    SubmissionRuntime<SUBMISSIONS, PROJECTED, DIRECT_LINKS>
{
    /// Mount and completely replay one already-formatted bound journal.
    ///
    /// Live service remains gated until [`Self::recover_boot_step`] reports
    /// [`RecoveryStep::Complete`]. Journal provisioning is deliberately outside
    /// this API because formatting policy is board/product specific.
    pub fn mount<A>(
        access: &mut A,
        first_submission_id: SubmissionId,
        boot_sequence: u64,
    ) -> Result<Self, MountError<A::Error>>
    where
        A: BoundJournalAccess,
    {
        Ok(Self {
            storage: StorageActor::mount(access, first_submission_id)?,
            boot_sequence,
            recovery_cursor: 0,
            recovery_exhausted: false,
            phase: RuntimePhase::Recovering,
            path_discoveries: [None; SUBMISSIONS],
            direct_link: None,
            reusable_direct_links: [None; DIRECT_LINKS],
            link_capacity_waiting: [false; SUBMISSIONS],
            next_link_offer_generation: 1,
            resource_waiting: [SubmissionId::new(0); SUBMISSIONS],
            resource_waiting_len: 0,
        })
    }

    /// Mount directly into caller-provided uninitialized storage.
    ///
    /// This placement form avoids constructing the complete runtime as a
    /// caller-stack return value. It is intended for products that keep a
    /// bounded but comparatively large durable index in external memory.
    /// `destination` contains a fully initialized runtime only when `Ok` is
    /// returned. On error it must still be treated as uninitialized, although
    /// an internal no-drop placement field may already have been written; the
    /// same storage may be supplied to an exact retry.
    ///
    /// The out-of-line mount projects the actor field inside `destination` and
    /// asks [`StorageActor::mount_into`] to initialize that field directly.
    /// Only after the complete physical and semantic replay succeeds are the
    /// remaining infallible runtime fields initialized. This is part of the
    /// placement API's bounded-stack contract, not throughput tuning.
    ///
    /// The caller must supply genuinely uninitialized storage or the same
    /// storage after this method returned an error. Calling this method with a
    /// destination that contains an exposed live runtime remains invalid. The
    /// compile-time no-drop invariant makes failed-attempt overwrite safe and
    /// fails closed if a future runtime field owns resources.
    #[allow(
        unsafe_code,
        reason = "one audited raw-field projection initializes the large actor directly in caller-owned MaybeUninit storage"
    )]
    #[inline(never)]
    pub fn mount_into<'a, A>(
        destination: &'a mut MaybeUninit<Self>,
        access: &mut A,
        first_submission_id: SubmissionId,
        boot_sequence: u64,
    ) -> Result<&'a mut Self, MountError<A::Error>>
    where
        A: BoundJournalAccess,
    {
        const {
            assert!(
                !core::mem::needs_drop::<SubmissionRuntime<SUBMISSIONS, PROJECTED, DIRECT_LINKS>>(),
                "runtime placement retries require a no-drop runtime"
            );
        }
        let runtime = destination.as_mut_ptr();
        // SAFETY: `runtime` is aligned, uniquely borrowed, and points to a
        // `MaybeUninit<Self>`. `MaybeUninit<T>` has the same layout as `T`, so
        // projecting the `storage` field and viewing that field as
        // `MaybeUninit<StorageActor<..>>` creates no reference to an
        // uninitialized `StorageActor`. The called placement API initializes
        // that exact field only on `Ok`; on `Err`, the outer value remains
        // uninitialized and this function returns before exposing it.
        let storage_destination = unsafe {
            &mut *core::ptr::addr_of_mut!((*runtime).storage)
                .cast::<MaybeUninit<StorageActor<SUBMISSIONS, PROJECTED>>>()
        };
        StorageActor::mount_into(storage_destination, access, first_submission_id)?;

        // SAFETY: the storage field is fully initialized above. Every
        // following write is infallible and initializes one distinct remaining
        // field exactly once, after which all fields of `Self` are initialized.
        unsafe {
            core::ptr::addr_of_mut!((*runtime).boot_sequence).write(boot_sequence);
            core::ptr::addr_of_mut!((*runtime).recovery_cursor).write(0);
            core::ptr::addr_of_mut!((*runtime).recovery_exhausted).write(false);
            core::ptr::addr_of_mut!((*runtime).phase).write(RuntimePhase::Recovering);
            let path_discoveries = core::ptr::addr_of_mut!((*runtime).path_discoveries)
                .cast::<Option<PathDiscovery>>();
            for slot in 0..SUBMISSIONS {
                path_discoveries.add(slot).write(None);
            }
            core::ptr::addr_of_mut!((*runtime).direct_link).write(None);
            let reusable_direct_links = core::ptr::addr_of_mut!((*runtime).reusable_direct_links)
                .cast::<Option<ReusableDirectLink>>();
            for slot in 0..DIRECT_LINKS {
                reusable_direct_links.add(slot).write(None);
            }
            let link_capacity_waiting =
                core::ptr::addr_of_mut!((*runtime).link_capacity_waiting).cast::<bool>();
            for slot in 0..SUBMISSIONS {
                link_capacity_waiting.add(slot).write(false);
            }
            core::ptr::addr_of_mut!((*runtime).next_link_offer_generation).write(1);
            let resource_waiting =
                core::ptr::addr_of_mut!((*runtime).resource_waiting).cast::<SubmissionId>();
            for slot in 0..SUBMISSIONS {
                resource_waiting.add(slot).write(SubmissionId::new(0));
            }
            core::ptr::addr_of_mut!((*runtime).resource_waiting_len).write(0);
            Ok(&mut *runtime)
        }
    }

    /// Current boot/service phase.
    pub const fn phase(&self) -> RuntimePhase {
        self.phase
    }

    /// Read-only authoritative durable submission index.
    pub const fn index(&self) -> &SubmissionIndex<SUBMISSIONS> {
        self.storage.index()
    }

    /// Read-only access to the backend-independent mounted storage state.
    pub const fn storage(&self) -> &StorageActor<SUBMISSIONS, PROJECTED> {
        &self.storage
    }

    /// Consume the runtime and recover its backend-independent storage actor.
    pub fn into_storage(self) -> StorageActor<SUBMISSIONS, PROJECTED> {
        self.storage
    }

    /// Conservatively resolve at most one replayed submission.
    pub fn recover_boot_step<A>(
        &mut self,
        access: &mut A,
    ) -> Result<RecoveryStep, RuntimeError<A::Error>>
    where
        A: BoundJournalAccess,
    {
        if self.phase == RuntimePhase::Ready {
            return Ok(RecoveryStep::AlreadyComplete);
        }
        self.storage
            .validate_access(access)
            .map_err(|error| RuntimeError::Storage(DriveError::Binding(error)))?;
        let candidate = (!self.recovery_exhausted).then(|| {
            self.storage
                .index()
                .iter()
                .filter(|submission| submission.accepted().id().get() >= self.recovery_cursor)
                .min_by_key(|submission| submission.accepted().id().get())
                .map(|submission| submission.accepted().id())
        });
        let candidate = candidate.flatten();
        let Some(id) = candidate else {
            self.phase = RuntimePhase::Ready;
            return Ok(RecoveryStep::Complete);
        };
        let progress = self
            .storage
            .finalize_boot_recovery(access, id, self.boot_sequence)?;
        if id.get() == u64::MAX {
            self.recovery_exhausted = true;
        } else {
            self.recovery_cursor = id.get() + 1;
        }
        Ok(RecoveryStep::Submission { id, progress })
    }

    /// Durably accept one authenticated, idempotent submission candidate.
    ///
    /// Physical access is borrowed only through the supplied bound journal.
    pub fn accept<A>(
        &mut self,
        access: &mut A,
        candidate: AcceptanceCandidate,
    ) -> Result<AcceptanceProgress, RuntimeError<A::Error>>
    where
        A: BoundJournalAccess,
    {
        self.ensure_ready()?;
        self.storage
            .accept(access, candidate)
            .map_err(RuntimeError::Storage)
    }

    /// Offer one exact complete native Reticulum frame observation.
    ///
    /// The interface dispatcher must retain the observation while `Retain` is
    /// returned. This includes transient actor serialization, projector-write,
    /// and acknowledgement pressure. Once ordinary runtime steps advance that
    /// retained work and the observation is durably represented, re-offering
    /// the same scalar returns `Durable`; only then may the dispatcher forget
    /// it. Permanent correlation errors and fail-closed faults remain explicit
    /// [`RuntimeControlError`] values. This is independent of any RNode or radio
    /// fragmentation performed after the native-frame boundary.
    pub fn offer_frame(
        &mut self,
        observation: PreparedFrameObservation,
    ) -> Result<FrameOfferProgress, RuntimeControlError> {
        self.ensure_control_ready()?;
        let progress = match self.storage.observe_frame(observation) {
            Ok(progress) => progress,
            Err(error) if frame_offer_must_retry(error) => {
                return Ok(FrameOfferProgress::Retain);
            }
            Err(error) => return Err(error.into()),
        };
        let persistence_pending = self.storage.pending_kind().is_some()
            || self
                .storage
                .projector()
                .pending_persistence()
                .next()
                .is_some();
        if progress == ProjectionProgress::AlreadyObserved && !persistence_pending {
            Ok(FrameOfferProgress::Durable)
        } else {
            Ok(FrameOfferProgress::Retain)
        }
    }

    /// Offer the native observation emitted by a concrete interface dispatcher.
    ///
    /// This is the production counterpart to [`Self::offer_frame`]. It drops
    /// only the selected interface scalar; durable lifecycle state is defined
    /// by the complete native packet identity and attempt correlation.
    pub fn offer_authorized_frame(
        &mut self,
        observation: AuthorizedFrameObservation,
    ) -> Result<FrameOfferProgress, RuntimeControlError> {
        self.offer_frame(PreparedFrameObservation::from(observation))
    }

    /// Attach the exact opaque Link created for one establishment offer.
    ///
    /// This operation is backend-free because the durable message remains
    /// unchanged and `Preparing`. Exact duplicate attachment is idempotent;
    /// every mismatch leaves the pending transaction untouched.
    pub fn attach_created_link(
        &mut self,
        offer: LinkEstablishmentOffer,
        link: LinkHandle,
    ) -> Result<(), LinkEstablishmentControlError> {
        self.ensure_link_control_ready()?;
        let Some(transaction) = self.direct_link.as_mut() else {
            return Err(LinkEstablishmentControlError::NoTransactionPending);
        };
        match transaction.phase {
            DirectLinkPhase::AwaitingLink { offer: expected } => {
                if offer != expected {
                    return Err(LinkEstablishmentControlError::OfferMismatch);
                }
                transaction.phase = DirectLinkPhase::AwaitingFirstDispatch { offer, link };
                Ok(())
            }
            DirectLinkPhase::AwaitingFirstDispatch {
                offer: expected,
                link: expected_link,
            }
            | DirectLinkPhase::Establishing {
                offer: expected,
                link: expected_link,
                ..
            } => {
                if offer != expected {
                    Err(LinkEstablishmentControlError::OfferMismatch)
                } else if link != expected_link {
                    Err(LinkEstablishmentControlError::LinkMismatch)
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Start the bounded establishment clock after the exact LinkRequest's
    /// first real interface dispatch.
    ///
    /// The caller must correlate the ordinary router's protocol token before
    /// invoking this backend-free edge. Exact duplicate acknowledgement is
    /// idempotent; a changed offer, handle, or dispatch instant fails closed.
    pub fn acknowledge_link_request_dispatched(
        &mut self,
        offer: LinkEstablishmentOffer,
        link: LinkHandle,
        dispatched_at: MonotonicMillis,
    ) -> Result<(), LinkEstablishmentControlError> {
        self.ensure_link_control_ready()?;
        let Some(transaction) = self.direct_link.as_mut() else {
            return Err(LinkEstablishmentControlError::NoTransactionPending);
        };
        match transaction.phase {
            DirectLinkPhase::AwaitingLink { offer: expected } => {
                if offer != expected {
                    Err(LinkEstablishmentControlError::OfferMismatch)
                } else {
                    Err(LinkEstablishmentControlError::WrongPhase)
                }
            }
            DirectLinkPhase::AwaitingFirstDispatch {
                offer: expected,
                link: expected_link,
            } => {
                if offer != expected {
                    return Err(LinkEstablishmentControlError::OfferMismatch);
                }
                if link != expected_link {
                    return Err(LinkEstablishmentControlError::LinkMismatch);
                }
                let dispatched_at_ms = dispatched_at.get();
                transaction.phase = DirectLinkPhase::Establishing {
                    offer,
                    link,
                    dispatched_at_ms,
                    deadline_ms: dispatched_at_ms.saturating_add(offer.establishment_timeout_ms()),
                };
                Ok(())
            }
            DirectLinkPhase::Establishing {
                offer: expected,
                link: expected_link,
                dispatched_at_ms,
                ..
            } => {
                if offer != expected {
                    Err(LinkEstablishmentControlError::OfferMismatch)
                } else if link != expected_link {
                    Err(LinkEstablishmentControlError::LinkMismatch)
                } else if dispatched_at.get() != dispatched_at_ms {
                    Err(LinkEstablishmentControlError::DispatchTimeMismatch)
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Advance at most one useful live submission transition.
    ///
    /// Priority is deliberately durability-first: reconcile an ambiguous
    /// physical mutation, persist one planned record, perform one unlocked
    /// acknowledgement, drain node observations, prepare an already-barriered
    /// intent, then begin one new barrier. The supplied access is validated
    /// before any acknowledgement or node preparation side effect.
    pub fn drive_step<A, N, R>(
        &mut self,
        access: &mut A,
        node: &mut N,
        rns_now: MonotonicSeconds,
        owner_now: MonotonicMillis,
        deadline: TxLeaseDeadline,
        rng: &mut R,
    ) -> Result<RuntimeStep, RuntimeError<A::Error>>
    where
        A: BoundJournalAccess,
        N: SubmissionNodePort,
        R: RngCore + CryptoRng,
    {
        self.ensure_ready()?;
        self.storage
            .validate_access(access)
            .map_err(|error| RuntimeError::Storage(DriveError::Binding(error)))?;

        if self.storage.pending_kind().is_some() {
            return self
                .storage
                .drive_pending(access)
                .map(RuntimeStep::Pending)
                .map_err(RuntimeError::Storage);
        }

        let pending_persistence = self.storage.projector().pending_persistence().next();
        if let Some(request) = pending_persistence {
            return self
                .storage
                .persist_projector(access, request)
                .map(RuntimeStep::Persistence)
                .map_err(RuntimeError::Storage);
        }

        let pending_acknowledgement = self.storage.pending_acknowledgements().next();
        if let Some(action) = pending_acknowledgement {
            let reply = node.acknowledge(action);
            self.storage.report_acknowledgement(action, reply)?;
            return Ok(RuntimeStep::Acknowledgement { action, reply });
        }

        let terminal = node.terminal_attempts().next();
        if let Some(terminal) = terminal {
            let progress = self.storage.observe_terminal(terminal)?;
            return Ok(RuntimeStep::Terminal { terminal, progress });
        }

        let recovered = node.recovered_observations().next();
        if let Some(observation) = recovered {
            let progress = self.storage.observe_recovered(observation)?;
            return Ok(RuntimeStep::Recovered {
                observation,
                progress,
            });
        }

        // Quarantines deliberately remain upstream forever. Skip exact durable
        // duplicates so they cannot starve later preparation work.
        for observation in node.quarantined_observations() {
            let progress = self.storage.observe_quarantined(observation)?;
            if progress != ProjectionProgress::AlreadyObserved {
                return Ok(RuntimeStep::Quarantined {
                    observation,
                    progress,
                });
            }
        }

        if let Some(transaction) = self.direct_link {
            match transaction.phase {
                DirectLinkPhase::AwaitingLink { offer } => {
                    if !node.can_initiate_link() {
                        self.direct_link = None;
                        self.mark_link_capacity_waiting(offer.id);
                        return Ok(RuntimeStep::LinkCapacityBackpressured {
                            id: offer.id,
                            limit: DIRECT_LINKS,
                        });
                    }
                    // Link creation has no retained owner until the product
                    // attaches the returned handle. Re-emit the identical
                    // generation so transient Link-table/action-allocation
                    // pressure can retry without creating an ABA offer.
                    return Ok(RuntimeStep::LinkEstablishment {
                        offer,
                        progress: ProjectionProgress::NoAction,
                    });
                }
                DirectLinkPhase::AwaitingFirstDispatch { .. } => {
                    return Ok(RuntimeStep::Idle);
                }
                DirectLinkPhase::Establishing {
                    offer,
                    link,
                    deadline_ms,
                    ..
                } => {
                    let state = node.link_state(link);
                    match state {
                        Some(LinkState::Active) => {
                            if !self.retain_reusable_direct_link(offer.destination, link) {
                                let progress = self.storage.observe_preparation(
                                    offer.id,
                                    SubmissionPreparationObservation::InternalFailure,
                                )?;
                                return Ok(RuntimeStep::Preparation {
                                    id: offer.id,
                                    progress,
                                });
                            }
                            self.direct_link = None;
                            if !node.link_is_usable(link) {
                                return Ok(RuntimeStep::LinkEstablishmentLost {
                                    offer,
                                    link,
                                    state,
                                });
                            }
                            self.clear_path_discovery(offer.destination);
                            let Some(SubmissionIntent::LxmfMessage(intent)) =
                                self.storage.ready_intent(offer.id)
                            else {
                                return Ok(RuntimeStep::Idle);
                            };
                            if DestinationHash::new(*intent.destination().as_bytes())
                                != offer.destination
                            {
                                return Ok(RuntimeStep::Idle);
                            }
                            let request = SubmissionPrepareRequest {
                                destination: offer.destination,
                                plaintext: intent.wire(),
                                rns_now,
                                owner_now,
                                deadline,
                            };
                            let observation =
                                node.prepare_rehydrated_direct_lxmf_submission(link, request, rng);
                            let observation =
                                self.classify_direct_preparation(offer.id, link, observation);
                            let progress =
                                self.storage.observe_preparation(offer.id, observation)?;
                            return Ok(RuntimeStep::Preparation {
                                id: offer.id,
                                progress,
                            });
                        }
                        Some(LinkState::Pending | LinkState::Handshake)
                            if owner_now.get() < deadline_ms =>
                        {
                            return Ok(RuntimeStep::Idle);
                        }
                        Some(LinkState::Pending | LinkState::Handshake) => {
                            let aborted = node.abort_unestablished_link(link);
                            if aborted {
                                self.direct_link = None;
                            }
                            return Ok(RuntimeStep::LinkEstablishmentExpired {
                                offer,
                                link,
                                aborted,
                            });
                        }
                        Some(LinkState::Stale) => {
                            // A stale Link can revive on Reticulum traffic.
                            // Preserve its destination correlation, but release
                            // the serialized establishment transaction so
                            // unrelated durable work can continue.
                            let _ = self.retain_reusable_direct_link(offer.destination, link);
                            self.direct_link = None;
                            return Ok(RuntimeStep::LinkEstablishmentLost { offer, link, state });
                        }
                        Some(LinkState::Closed) | None => {
                            self.direct_link = None;
                            self.evict_reusable_direct_link(link);
                            return Ok(RuntimeStep::LinkEstablishmentLost { offer, link, state });
                        }
                    }
                }
            }
        }

        self.refresh_link_capacity_waiting(node);
        let ready =
            self.storage
                .index()
                .iter()
                .enumerate()
                .find_map(|(submission_slot, submission)| {
                    if self.link_capacity_waiting[submission_slot] {
                        return None;
                    }
                    let id = submission.accepted().id();
                    let intent = self.storage.ready_intent(id)?;
                    if self.resource_is_waiting(id) {
                        return None;
                    }
                    let destination = DestinationHash::new(*intent.destination().as_bytes());
                    self.path_discovery_due(destination, owner_now.get())
                        .then_some((submission_slot, id, intent, destination))
                });
        if let Some((submission_slot, id, intent, destination)) = ready {
            let request = SubmissionPrepareRequest {
                destination,
                plaintext: match &intent {
                    SubmissionIntent::ExperimentalRnsData(intent) => intent.payload(),
                    SubmissionIntent::LxmfMessage(intent) => intent.wire(),
                },
                rns_now,
                owner_now,
                deadline,
            };
            let observation = match intent {
                SubmissionIntent::ExperimentalRnsData(_) => node.prepare_submission(request, rng),
                SubmissionIntent::LxmfMessage(_) => {
                    let active_link = self.reusable_direct_link_for(node, destination);
                    if let Some(link) = active_link {
                        self.clear_path_discovery(destination);
                        let observation =
                            node.prepare_rehydrated_direct_lxmf_submission(link, request, rng);
                        let observation = self.classify_direct_preparation(id, link, observation);
                        let progress = self.storage.observe_preparation(id, observation)?;
                        return Ok(RuntimeStep::Preparation { id, progress });
                    }

                    if intent.lxmf_message().is_some_and(|intent| {
                        intent.opportunistic_carrier().len() > MAX_OPPORTUNISTIC_LXMF_CARRIER
                    }) {
                        if !node.has_usable_path(&destination) {
                            let (observation, path_offer) = self.classify_path_discovery(
                                id,
                                destination,
                                SubmissionPreparationObservation::Rejected(
                                    SubmitError::UnknownDestination,
                                ),
                                owner_now.get(),
                            );
                            let progress = self.storage.observe_preparation(id, observation)?;
                            if let Some(offer) = path_offer {
                                return Ok(RuntimeStep::PathDiscoveryRequest { offer, progress });
                            }
                            return Ok(RuntimeStep::Preparation { id, progress });
                        }
                        if !self.direct_link_admission_available(node) {
                            self.link_capacity_waiting[submission_slot] = true;
                            let progress = self.storage.observe_preparation(
                                id,
                                SubmissionPreparationObservation::RetrySameBoot,
                            )?;
                            debug_assert_eq!(progress, ProjectionProgress::NoAction);
                            return Ok(RuntimeStep::LinkCapacityBackpressured {
                                id,
                                limit: DIRECT_LINKS,
                            });
                        }
                        let Some(offer) = self.begin_direct_link(
                            id,
                            destination,
                            node.retained_path_hops(&destination),
                            node.retained_path_first_hop_serialization_ms(&destination),
                        ) else {
                            let progress = self.storage.observe_preparation(
                                id,
                                SubmissionPreparationObservation::InternalFailure,
                            )?;
                            return Ok(RuntimeStep::Preparation { id, progress });
                        };
                        let progress = self.storage.observe_preparation(
                            id,
                            SubmissionPreparationObservation::RetrySameBoot,
                        )?;
                        return Ok(RuntimeStep::LinkEstablishment { offer, progress });
                    }

                    match node.prepare_rehydrated_opportunistic_lxmf_submission(request, rng) {
                        SubmissionPreparationObservation::Rejected(
                            SubmitError::PayloadTooLarge { .. },
                        ) => {
                            if !node.has_usable_path(&destination) {
                                let (observation, path_offer) = self.classify_path_discovery(
                                    id,
                                    destination,
                                    SubmissionPreparationObservation::Rejected(
                                        SubmitError::UnknownDestination,
                                    ),
                                    owner_now.get(),
                                );
                                let progress = self.storage.observe_preparation(id, observation)?;
                                if let Some(offer) = path_offer {
                                    return Ok(RuntimeStep::PathDiscoveryRequest {
                                        offer,
                                        progress,
                                    });
                                }
                                return Ok(RuntimeStep::Preparation { id, progress });
                            }
                            if !self.direct_link_admission_available(node) {
                                self.link_capacity_waiting[submission_slot] = true;
                                let progress = self.storage.observe_preparation(
                                    id,
                                    SubmissionPreparationObservation::RetrySameBoot,
                                )?;
                                debug_assert_eq!(progress, ProjectionProgress::NoAction);
                                return Ok(RuntimeStep::LinkCapacityBackpressured {
                                    id,
                                    limit: DIRECT_LINKS,
                                });
                            }
                            let Some(offer) = self.begin_direct_link(
                                id,
                                destination,
                                node.retained_path_hops(&destination),
                                node.retained_path_first_hop_serialization_ms(&destination),
                            ) else {
                                let progress = self.storage.observe_preparation(
                                    id,
                                    SubmissionPreparationObservation::InternalFailure,
                                )?;
                                return Ok(RuntimeStep::Preparation { id, progress });
                            };
                            let progress = self.storage.observe_preparation(
                                id,
                                SubmissionPreparationObservation::RetrySameBoot,
                            )?;
                            return Ok(RuntimeStep::LinkEstablishment { offer, progress });
                        }
                        observation => observation,
                    }
                }
            };
            let (observation, path_offer) =
                self.classify_path_discovery(id, destination, observation, owner_now.get());
            let progress = self.storage.observe_preparation(id, observation)?;
            if let Some(offer) = path_offer {
                return Ok(RuntimeStep::PathDiscoveryRequest { offer, progress });
            }
            return Ok(RuntimeStep::Preparation { id, progress });
        }

        let queued = self
            .storage
            .index()
            .iter()
            .find(|submission| submission.state() == LifecycleState::Queued)
            .map(|submission| submission.accepted().id());
        if let Some(id) = queued {
            let progress = self.storage.begin_preparation(id)?;
            return Ok(RuntimeStep::PreparationBarrier { id, progress });
        }

        Ok(RuntimeStep::Idle)
    }

    fn reusable_direct_link_for<N>(
        &mut self,
        node: &N,
        destination: DestinationHash,
    ) -> Option<LinkHandle>
    where
        N: SubmissionNodePort,
    {
        self.prune_reusable_direct_links(node);
        self.reusable_direct_links
            .iter()
            .flatten()
            .find(|candidate| {
                candidate.destination == destination && node.link_is_usable(candidate.link)
            })
            .map(|candidate| candidate.link)
    }

    fn direct_link_admission_available<N>(&mut self, node: &N) -> bool
    where
        N: SubmissionNodePort,
    {
        self.prune_reusable_direct_links(node);
        node.can_initiate_link() && self.reusable_direct_links.iter().any(Option::is_none)
    }

    fn refresh_link_capacity_waiting<N>(&mut self, node: &N)
    where
        N: SubmissionNodePort,
    {
        self.prune_reusable_direct_links(node);
        if node.can_initiate_link() && self.reusable_direct_links.iter().any(Option::is_none) {
            self.link_capacity_waiting.fill(false);
            return;
        }

        for submission_slot in 0..SUBMISSIONS {
            if self.link_capacity_waiting[submission_slot]
                && self.link_capacity_wait_should_clear(node, submission_slot)
            {
                self.link_capacity_waiting[submission_slot] = false;
            }
        }
    }

    fn link_capacity_wait_should_clear<N>(&self, node: &N, submission_slot: usize) -> bool
    where
        N: SubmissionNodePort,
    {
        let Some(submission) = self.storage.index().iter().nth(submission_slot) else {
            return true;
        };
        let Some(intent) = self.storage.ready_intent(submission.accepted().id()) else {
            return true;
        };
        let destination = DestinationHash::new(*intent.destination().as_bytes());
        self.reusable_direct_links
            .iter()
            .flatten()
            .any(|candidate| {
                candidate.destination == destination && node.link_is_usable(candidate.link)
            })
    }

    fn mark_link_capacity_waiting(&mut self, id: SubmissionId) {
        if let Some(submission_slot) = self
            .storage
            .index()
            .iter()
            .position(|submission| submission.accepted().id() == id)
        {
            self.link_capacity_waiting[submission_slot] = true;
        }
    }

    fn prune_reusable_direct_links<N>(&mut self, node: &N)
    where
        N: SubmissionNodePort,
    {
        for entry in &mut self.reusable_direct_links {
            let Some(candidate) = *entry else {
                continue;
            };
            if matches!(
                node.link_state(candidate.link),
                None | Some(LinkState::Closed)
            ) {
                *entry = None;
            }
        }
    }

    fn retain_reusable_direct_link(
        &mut self,
        destination: DestinationHash,
        link: LinkHandle,
    ) -> bool {
        if let Some(candidate) = self
            .reusable_direct_links
            .iter()
            .flatten()
            .find(|candidate| candidate.link == link)
        {
            return candidate.destination == destination;
        }
        let Some(slot) = self
            .reusable_direct_links
            .iter_mut()
            .find(|slot| slot.is_none())
        else {
            return false;
        };
        *slot = Some(ReusableDirectLink { destination, link });
        true
    }

    fn evict_reusable_direct_link(&mut self, link: LinkHandle) {
        if let Some(slot) = self
            .reusable_direct_links
            .iter_mut()
            .find(|entry| entry.is_some_and(|candidate| candidate.link == link))
        {
            *slot = None;
        }
    }

    fn begin_direct_link(
        &mut self,
        id: SubmissionId,
        destination: DestinationHash,
        retained_path_hops: Option<u8>,
        first_hop_serialization_ms: Option<u64>,
    ) -> Option<LinkEstablishmentOffer> {
        if self.direct_link.is_some() {
            return None;
        }
        let generation = self.next_link_offer_generation;
        self.next_link_offer_generation = generation.checked_add(1)?;
        let offer = LinkEstablishmentOffer {
            id,
            destination,
            generation,
            establishment_timeout_ms: direct_link_establishment_timeout(
                retained_path_hops,
                first_hop_serialization_ms,
            ),
        };
        self.direct_link = Some(DirectLinkTransaction {
            phase: DirectLinkPhase::AwaitingLink { offer },
        });
        Some(offer)
    }

    fn classify_direct_preparation(
        &mut self,
        id: SubmissionId,
        link: LinkHandle,
        observation: SubmissionPreparationObservation,
    ) -> SubmissionPreparationObservation {
        match observation {
            SubmissionPreparationObservation::Rejected(SubmitError::PayloadTooLarge { .. }) => {
                if self.mark_resource_waiting(id) {
                    SubmissionPreparationObservation::RetrySameBoot
                } else {
                    SubmissionPreparationObservation::InternalFailure
                }
            }
            SubmissionPreparationObservation::Rejected(
                SubmitError::LinkNotFound | SubmitError::LinkNotActive,
            ) => {
                self.evict_reusable_direct_link(link);
                SubmissionPreparationObservation::RetrySameBoot
            }
            SubmissionPreparationObservation::Rejected(
                SubmitError::LinkDestinationMismatch | SubmitError::LinkInterfaceUnknown,
            ) => {
                self.evict_reusable_direct_link(link);
                SubmissionPreparationObservation::InternalFailure
            }
            observation => observation,
        }
    }

    fn resource_is_waiting(&self, id: SubmissionId) -> bool {
        self.resource_waiting[..self.resource_waiting_len].contains(&id)
    }

    fn mark_resource_waiting(&mut self, id: SubmissionId) -> bool {
        if self.resource_is_waiting(id) {
            return true;
        }
        let Some(slot) = self.resource_waiting.get_mut(self.resource_waiting_len) else {
            return false;
        };
        *slot = id;
        self.resource_waiting_len += 1;
        true
    }

    fn path_discovery_due(&self, destination: DestinationHash, now_ms: u64) -> bool {
        self.path_discoveries
            .iter()
            .flatten()
            .find(|discovery| discovery.destination == destination)
            .is_none_or(|discovery| match discovery.phase {
                PathDiscoveryPhase::AwaitingDispatch { .. } => false,
                PathDiscoveryPhase::Waiting { next_probe_ms, .. } => now_ms >= next_probe_ms,
            })
    }

    fn classify_path_discovery(
        &mut self,
        id: SubmissionId,
        destination: DestinationHash,
        observation: SubmissionPreparationObservation,
        now_ms: u64,
    ) -> (SubmissionPreparationObservation, Option<PathDiscoveryOffer>) {
        if observation
            != SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination)
        {
            if !matches!(
                observation,
                SubmissionPreparationObservation::RetrySameBoot
                    | SubmissionPreparationObservation::Rejected(
                        SubmitError::AttemptLedgerFull { .. }
                            | SubmitError::ReceiptTableFull { .. }
                            | SubmitError::ReceiptHashAlreadyTracked
                    )
            ) {
                self.clear_path_discovery(destination);
            }
            return (observation, None);
        }

        let existing = self
            .path_discoveries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.destination == destination));
        let Some(index) = existing else {
            let Some(index) = self.path_discoveries.iter().position(Option::is_none) else {
                return (SubmissionPreparationObservation::InternalFailure, None);
            };
            let offer = PathDiscoveryOffer {
                id,
                destination,
                ordinal: 1,
            };
            self.path_discoveries[index] = Some(PathDiscovery {
                destination,
                phase: PathDiscoveryPhase::AwaitingDispatch {
                    offer,
                    first_request_ms: None,
                },
            });
            return (SubmissionPreparationObservation::RetrySameBoot, Some(offer));
        };

        let discovery = self.path_discoveries[index]
            .as_mut()
            .expect("the located discovery slot must remain occupied");
        match discovery.phase {
            PathDiscoveryPhase::AwaitingDispatch { .. } => {
                (SubmissionPreparationObservation::RetrySameBoot, None)
            }
            PathDiscoveryPhase::Waiting {
                first_request_ms,
                requests_sent,
                ..
            } if requests_sent < PATH_DISCOVERY_MAX_REQUESTS => {
                let retry_not_before =
                    first_request_ms.saturating_add(PATH_DISCOVERY_RETRY_INTERVAL_MS);
                if now_ms < retry_not_before {
                    discovery.phase = PathDiscoveryPhase::Waiting {
                        first_request_ms,
                        next_probe_ms: retry_not_before,
                        requests_sent,
                    };
                    return (SubmissionPreparationObservation::RetrySameBoot, None);
                }
                let ordinal = requests_sent.saturating_add(1);
                let offer = PathDiscoveryOffer {
                    id,
                    destination,
                    ordinal,
                };
                discovery.phase = PathDiscoveryPhase::AwaitingDispatch {
                    offer,
                    first_request_ms: Some(first_request_ms),
                };
                (SubmissionPreparationObservation::RetrySameBoot, Some(offer))
            }
            PathDiscoveryPhase::Waiting { .. } => {
                self.path_discoveries[index] = None;
                (observation, None)
            }
        }
    }

    fn clear_path_discovery(&mut self, destination: DestinationHash) {
        if let Some(index) = self
            .path_discoveries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.destination == destination))
        {
            self.path_discoveries[index] = None;
        }
    }

    /// Start response and retry clocks after the request's first real router
    /// dispatch onto an eligible interface.
    ///
    /// This operation is backend-free. The exact offer remains pending on any
    /// mismatch so a caller can fail closed without losing correlation.
    pub fn acknowledge_path_request_dispatched(
        &mut self,
        offer: PathDiscoveryOffer,
        dispatched_at: MonotonicMillis,
    ) -> Result<(), PathDiscoveryAcknowledgeError> {
        let Some(discovery) = self
            .path_discoveries
            .iter_mut()
            .flatten()
            .find(|discovery| discovery.destination == offer.destination)
        else {
            return Err(PathDiscoveryAcknowledgeError::DestinationNotPending);
        };
        let PathDiscoveryPhase::AwaitingDispatch {
            offer: expected,
            first_request_ms,
        } = discovery.phase
        else {
            return Err(PathDiscoveryAcknowledgeError::OfferMismatch);
        };
        if expected != offer {
            return Err(PathDiscoveryAcknowledgeError::OfferMismatch);
        }
        let dispatched_at = dispatched_at.get();
        discovery.phase = PathDiscoveryPhase::Waiting {
            first_request_ms: first_request_ms.unwrap_or(dispatched_at),
            next_probe_ms: dispatched_at.saturating_add(PATH_DISCOVERY_RESPONSE_WAIT_MS),
            requests_sent: offer.ordinal,
        };
        Ok(())
    }

    fn ensure_ready<E>(&self) -> Result<(), RuntimeError<E>> {
        if self.phase == RuntimePhase::Ready {
            Ok(())
        } else {
            Err(RuntimeError::Recovering)
        }
    }

    fn ensure_control_ready(&self) -> Result<(), RuntimeControlError> {
        if self.phase == RuntimePhase::Ready {
            Ok(())
        } else {
            Err(RuntimeControlError::Recovering)
        }
    }

    fn ensure_link_control_ready(&self) -> Result<(), LinkEstablishmentControlError> {
        if self.phase == RuntimePhase::Ready {
            Ok(())
        } else {
            Err(LinkEstablishmentControlError::Recovering)
        }
    }
}

fn frame_offer_must_retry(error: ProjectorOperationError) -> bool {
    matches!(
        error,
        ProjectorOperationError::Busy { .. }
            | ProjectorOperationError::Rejected(
                ProjectorError::WritePending | ProjectorError::AcknowledgementPending
            )
    )
}

#[cfg(test)]
mod tests;
