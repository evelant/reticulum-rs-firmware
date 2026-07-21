//! Persistent board-neutral dispatch for one RNode-compatible LoRa radio.
//!
//! [`SoleRadioTxDispatcher`] is the sole consumer of one transport-neutral
//! interface-actor queue and the DATA/ordinary permit exchanges. It retains
//! each router-issued completion ticket beside the active node-core typestate
//! while serializing both families over one [`SoleRnodeRadio`]. Every packet
//! performs randomized initial backoff,
//! bounded retry backoff and CAD before it requests an exact configuration- and
//! airtime-bound permit. Packet bytes are exposed only after a matching grant
//! and the final fresh channel-access check.
//! Once DATA bytes have been exposed, the dispatcher sends the exact
//! [`AuthorizedFrameObservation`] through its dedicated handoff and retains the
//! owning completion until the node echoes an identical durable
//! acknowledgement. There is deliberately no acknowledgement timeout. A
//! mismatch, or any acknowledgement queued before its request is accepted,
//! disables the dispatcher while retaining all correlation evidence and the
//! owner.
//!
//! Idle receive is serialized through the same owner. Starting RX is an
//! explicit scheduler choice whenever no TX is already active: a queued job is
//! left in the actor queue instead of being claimed merely because RX was
//! selected. This makes bounded RX/TX fairness expressible by the permanent
//! supervisor while retaining receive cancellation as an explicit fail-closed
//! recovery phase.
//!
//! Short radio-operation futures are cancellation-contained: before awaiting
//! RX, CAD, or TX, the receive phase or unique packet owner is moved into
//! persistent dispatcher state. If a supervisor drops such a future, it must call
//! [`SoleRadioTxDispatcher::recover_cancelled_radio_operation`] before making
//! further progress. A mismatched non-`Copy` permit reply cannot be returned by
//! the current one-way handoff API, so that case deliberately fails closed and
//! retains the exact reply rather than weakening ownership.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::{
    future::poll_fn,
    mem,
    task::{Context, Poll},
};

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_time::{Instant, Timer};
use rand_core::{CryptoRng, RngCore};
use reticulum_interface_router::{
    ActorCompletionSendError, DataCompletionMismatch, DataCompletionTicket, InterfaceConfigId,
    InterfaceTxActorHandoff, InterfaceTxCompletion, InterfaceTxJob, OrdinaryCompletionMismatch,
    OrdinaryCompletionTicket,
};
use reticulum_node_core::{
    AuthorizedFrameObservation, AuthorizedTx, ExpiredAuthorizedTx, MonotonicMillis,
    OrdinaryAuthorizedTx, OrdinaryExpiredAuthorizedTx, OrdinaryPermitPendingTx,
    OrdinaryPermitReplyMismatch, OrdinaryPermitResolution, OrdinaryTxCompletion, OrdinaryTxJob,
    OrdinaryTxPermitReply, OrdinaryTxPermitRequest, OrdinaryUnpermittedTx, PacketInterfaceId,
    PermitPendingTx, PermitReplyMismatch, PermitResolution, RoutedTxJob, TxAuthorizationCandidate,
    TxAuthorizationPolicy, TxCompletion, TxCompletionCode, TxFrameError, TxPermitReply,
    TxPermitRequest, TxPermitRequirements, TxPermitReservation, TxPermitResourceId,
    TxPolicyDecision, TxPolicyDenial, UnpermittedTx,
};
use reticulum_radio_interface::{
    BoundedRxObservation, BoundedRxOutcome, LabRxProfile, LogicalPacketAccessAction,
    LogicalPacketAccessConfig, LogicalPacketAccessPhase, LogicalPacketAccessRejection,
    LogicalPacketChannelAccess, PacketTxProgress, RadioConfigurationFingerprint, RnodeAirtimeError,
    RnodeTxFrameBuffer, SX1262_FRAME_MTU, SoleRadioFault, SoleRadioFaultClass, SoleRadioFaultPhase,
    SoleRnodeRadio, frame_rns_packet,
};
use reticulum_tx_handoff::{
    AuthorizedFrameDispatcherHandoff, DispatcherPermitHandoff, OrdinaryDispatcherPermitHandoff,
};

/// Packet-owner family currently serialized over the sole radio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchFamily {
    /// Destination-DATA owner and receipt attempt.
    Data,
    /// Ordinary Rete action packet and serialized fan-out hop.
    Ordinary,
}

/// Caller-owned completion-code namespace for real radio dispatch.
///
/// Node-core derives conservative sent/unsent classification from typestate;
/// these bounded numbers are diagnostics only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTxCompletionCodes {
    /// Every physical frame completed successfully.
    pub transmitted: TxCompletionCode,
    /// Channel access or policy ended before any authorization.
    pub unpermitted: TxCompletionCode,
    /// A grant arrived too late to expose packet bytes.
    pub expired_authorization: TxCompletionCode,
    /// A post-grant channel-access projection rejected dispatch.
    pub post_grant_rejection: TxCompletionCode,
    /// CAD failed before permit negotiation.
    pub cad_fault: TxCompletionCode,
    /// The radio returned a TX fault after authorization.
    pub tx_fault: TxCompletionCode,
    /// A permit exchange or retained control value became unrecoverable.
    pub control_plane_recovery: TxCompletionCode,
    /// Authorized frame metadata or framing violated an invariant.
    pub frame_invariant_recovery: TxCompletionCode,
    /// A dropped short radio-operation future was explicitly reconciled.
    pub cancelled_radio_operation: TxCompletionCode,
}

impl RadioTxCompletionCodes {
    /// Construct an explicit completion-code mapping.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        transmitted: TxCompletionCode,
        unpermitted: TxCompletionCode,
        expired_authorization: TxCompletionCode,
        post_grant_rejection: TxCompletionCode,
        cad_fault: TxCompletionCode,
        tx_fault: TxCompletionCode,
        control_plane_recovery: TxCompletionCode,
        frame_invariant_recovery: TxCompletionCode,
        cancelled_radio_operation: TxCompletionCode,
    ) -> Self {
        Self {
            transmitted,
            unpermitted,
            expired_authorization,
            post_grant_rejection,
            cad_fault,
            tx_fault,
            control_plane_recovery,
            frame_invariant_recovery,
            cancelled_radio_operation,
        }
    }
}

/// Immutable policy for the sole-radio dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTxDispatcherConfig {
    /// Opaque router configuration identity owned by this concrete actor.
    pub expected_interface_config: InterfaceConfigId,
    /// Bounded randomized channel-access policy.
    pub channel_access: LogicalPacketAccessConfig,
    /// Additional time after the owner deadline before an unresolved permit
    /// exchange becomes retained recovery.
    pub permit_recovery_grace_us: u64,
    /// First four-bit RNode sequence value used after authorization.
    pub initial_rnode_sequence: u8,
    /// Project-owned completion-code mapping.
    pub completion_codes: RadioTxCompletionCodes,
}

impl RadioTxDispatcherConfig {
    /// Construct an explicit dispatcher policy.
    pub const fn new(
        expected_interface_config: InterfaceConfigId,
        channel_access: LogicalPacketAccessConfig,
        permit_recovery_grace_us: u64,
        initial_rnode_sequence: u8,
        completion_codes: RadioTxCompletionCodes,
    ) -> Self {
        Self {
            expected_interface_config,
            channel_access,
            permit_recovery_grace_us,
            initial_rnode_sequence: initial_rnode_sequence & 0x0f,
            completion_codes,
        }
    }
}

/// Exact transport policy for one immutable RNode-compatible LoRa interface.
///
/// This validator does not trust the actor-supplied resource quantity. It
/// requires the selected Reticulum interface and complete radio-configuration
/// fingerprint to match its construction inputs, recomputes the one- or
/// two-frame airtime from the native packet length and immutable LoRa profile,
/// and authorizes only that exact number of RF microseconds.
///
/// This type deliberately contains no regional duty-cycle, dwell-time,
/// aggregate budget, or multi-radio coordination policy. It is sufficient for
/// an explicitly controlled development profile and is the exact-binding
/// primitive a product regional scheduler must apply before granting a
/// reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactLoRaAirtimePolicy {
    interface: PacketInterfaceId,
    profile: LabRxProfile,
    resource: TxPermitResourceId,
}

impl ExactLoRaAirtimePolicy {
    /// Bind the policy to one Reticulum interface, immutable modem profile,
    /// and complete concrete-radio configuration.
    pub const fn new(
        interface: PacketInterfaceId,
        profile: LabRxProfile,
        fingerprint: RadioConfigurationFingerprint,
    ) -> Self {
        Self {
            interface,
            profile,
            resource: TxPermitResourceId::new(fingerprint.as_bytes()),
        }
    }

    /// Reticulum packet interface this validator accepts.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Immutable LoRa airtime profile used for independent recomputation.
    pub const fn profile(self) -> LabRxProfile {
        self.profile
    }

    /// Opaque complete-radio resource bound into every accepted request.
    pub const fn resource(self) -> TxPermitResourceId {
        self.resource
    }

    /// Validate one candidate and construct only its exact RF-time reservation.
    ///
    /// Exposing this scalar validation lets a later regional scheduler apply
    /// additional rolling budgets without duplicating or weakening the packet
    /// length, interface, fingerprint, and airtime checks.
    pub fn validate(
        self,
        candidate: TxAuthorizationCandidate,
    ) -> Result<TxPermitReservation, TxPolicyDenial> {
        if candidate.interface != self.interface {
            return Err(TxPolicyDenial::PolicyDenied);
        }
        if candidate.requirements.resource() != self.resource {
            return Err(TxPolicyDenial::ResourceUnavailable);
        }
        let expected_airtime = self
            .profile
            .rnode_packet_airtime(candidate.packet_len.into())
            .map_err(|_| TxPolicyDenial::ResourceUnavailable)?
            .aggregate_time_on_air_us();
        if candidate.requirements.required_units() != expected_airtime {
            return Err(TxPolicyDenial::PolicyDenied);
        }
        TxPermitReservation::try_new(self.resource, expected_airtime)
            .map_err(|_| TxPolicyDenial::ResourceUnavailable)
    }
}

impl TxAuthorizationPolicy for ExactLoRaAirtimePolicy {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        match self.validate(candidate) {
            Ok(reservation) => TxPolicyDecision::Authorize(reservation),
            Err(reason) => TxPolicyDecision::Deny(reason),
        }
    }
}

/// Terminal scalar diagnosis for the most recently reconciled radio job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchOutcome {
    /// Every physical frame completed successfully.
    Transmitted,
    /// Channel access rejected before a permit was requested.
    AccessRejected(LogicalPacketAccessRejection),
    /// The node owner denied the exact permit request.
    PermitDenied,
    /// A matching grant arrived at or after the owner deadline.
    AuthorizationExpired,
    /// A grant was consumed but the final fresh channel-access projection rejected.
    PostGrantAccessRejected(LogicalPacketAccessRejection),
    /// The active radio could not calculate airtime for the packet.
    AirtimeRejected(RnodeAirtimeError),
    /// A millisecond owner deadline could not be represented in microseconds.
    DeadlineConversionOverflow,
    /// The sole radio was already inactive before authorization.
    RadioInactive,
    /// The router-stamped actor configuration does not belong to this dispatcher.
    InterfaceConfigurationMismatch {
        /// Configuration identity this dispatcher instance owns.
        expected: InterfaceConfigId,
        /// Configuration identity stamped when the router admitted the owner.
        stamped: InterfaceConfigId,
    },
    /// The radio's immutable fingerprint or airtime profile changed before permit negotiation.
    RadioConfigurationChangedBeforePermit,
    /// The radio's immutable fingerprint or airtime profile changed after the permit request.
    RadioConfigurationChangedAfterPermit,
    /// CAD failed with the portable phase and class supplied by the adapter.
    CadFault {
        /// Operation phase reported by the radio adapter.
        phase: SoleRadioFaultPhase,
        /// Stable failure class reported by the radio adapter.
        class: SoleRadioFaultClass,
    },
    /// TX failed after authorization, possibly after one physical frame.
    TxFault {
        /// Operation phase reported by the radio adapter.
        phase: SoleRadioFaultPhase,
        /// Stable failure class reported by the radio adapter.
        class: SoleRadioFaultClass,
    },
    /// A permit exchange was lost, stale, or otherwise unrecoverable.
    ControlPlaneRecovery,
    /// Authorized byte exposure or RNode framing violated an invariant.
    FrameInvariantRecovery,
    /// A supervisor explicitly reconciled a dropped CAD or TX future.
    CancelledRadioOperation,
}

/// Copy-only report retained independently of the owning completion channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchReport {
    family: DispatchFamily,
    outcome: DispatchOutcome,
    frame_count: u8,
    progress: Option<PacketTxProgress>,
    authorized_frame: Option<AuthorizedFrameObservation>,
}

/// Exact expected and received observations retained after an acknowledgement mismatch.
///
/// The owning DATA completion and its router ticket remain inside the disabled
/// dispatcher. This copy-only view exposes enough correlation evidence for a
/// supervisor diagnostic without granting access to either owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorizedFrameAcknowledgementMismatch {
    expected: AuthorizedFrameObservation,
    actual: AuthorizedFrameObservation,
}

impl AuthorizedFrameAcknowledgementMismatch {
    /// Observation the dispatcher sent and continued retaining.
    pub const fn expected(self) -> AuthorizedFrameObservation {
        self.expected
    }

    /// Non-matching acknowledgement received from the node owner.
    pub const fn actual(self) -> AuthorizedFrameObservation {
        self.actual
    }
}

impl DispatchReport {
    const fn new(
        family: DispatchFamily,
        outcome: DispatchOutcome,
        frame_count: u8,
        progress: Option<PacketTxProgress>,
    ) -> Self {
        Self {
            family,
            outcome,
            frame_count,
            progress,
            authorized_frame: None,
        }
    }

    const fn with_authorized_frame(
        mut self,
        authorized_frame: Option<AuthorizedFrameObservation>,
    ) -> Self {
        self.authorized_frame = authorized_frame;
        self
    }

    /// Packet-owner family to which this report belongs.
    pub const fn family(self) -> DispatchFamily {
        self.family
    }

    /// Terminal diagnosis independent of node-core's owning classification.
    pub const fn outcome(self) -> DispatchOutcome {
        self.outcome
    }

    /// Planned physical-frame count, or zero if planning never succeeded.
    pub const fn frame_count(self) -> u8 {
        self.frame_count
    }

    /// Exact adapter-reported TX progress when a TX future returned.
    ///
    /// `None` means no transmit future returned an observation. In particular,
    /// cancellation cannot safely assert that zero frames reached RF.
    pub const fn progress(self) -> Option<PacketTxProgress> {
        self.progress
    }

    /// Exact authorized DATA frame observed before interface-specific framing.
    ///
    /// This is `None` for ordinary packets and failures that occurred before
    /// node-core exposed the authorized packet bytes.
    pub const fn authorized_frame(self) -> Option<AuthorizedFrameObservation> {
        self.authorized_frame
    }
}

/// High-level persistent dispatcher phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTxDispatcherPhase {
    /// No job is owned.
    Idle,
    /// A bounded receive future was started and may have been dropped.
    ReceiveInFlight,
    /// One received job awaits a fresh start timestamp.
    JobReady(DispatchFamily),
    /// Mandatory initial or retry backoff is active.
    BackingOff(DispatchFamily),
    /// The next short operation must run CAD.
    CadReady(DispatchFamily),
    /// A CAD future was started and may have been dropped.
    CadInFlight(DispatchFamily),
    /// A permit request is retained under request-channel pressure.
    PermitSend(DispatchFamily),
    /// The exact pending owner awaits its permit reply.
    PermitWait(DispatchFamily),
    /// A matching grant is ready for a fresh final access check and TX.
    TransmitReady(DispatchFamily),
    /// A transmit future was started and may have been dropped.
    TransmitInFlight(DispatchFamily),
    /// An authorized DATA observation is retained under request-channel pressure.
    AuthorizedFrameRequest,
    /// The owning DATA completion is gated on an exact durable acknowledgement.
    AuthorizedFrameAcknowledgementWait,
    /// An owning completion is retained under return-channel pressure.
    CompletionReturn(DispatchFamily),
    /// A non-`Copy` mismatch or terminal radio/control failure is retained.
    Disabled,
}

/// Channel currently applying synchronous backpressure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTxDispatcherChannel {
    /// Destination-DATA permit request.
    DataPermitRequest,
    /// Ordinary-action permit request.
    OrdinaryPermitRequest,
    /// Authorized DATA frame observation request.
    AuthorizedFrameObservationRequest,
    /// Ticket-bound completion for the active family.
    InterfaceCompletion(DispatchFamily),
}

/// Phase-aware result of polling authorized-frame request capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizedFrameRequestCapacity {
    /// Advisory capacity is available for the exact retained observation.
    Ready,
    /// No authorized-frame request is retained under channel pressure.
    NotRetained(RadioTxDispatcherPhase),
}

/// Result of polling or awaiting one exact authorized-frame acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "authorized-frame acknowledgement progress must be handled"]
pub enum AuthorizedFrameAcknowledgementProgress {
    /// The exact retained observation was acknowledged; completion may advance.
    Matched,
    /// No owning completion is currently gated on an acknowledgement.
    NotRetained(RadioTxDispatcherPhase),
    /// A mismatched acknowledgement disabled the dispatcher and retained both values.
    Disabled(DispatcherFault),
}

/// Phase-aware result of polling retained interface-completion capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceCompletionCapacity {
    /// Advisory capacity is currently available for the retained family.
    Ready(DispatchFamily),
    /// No interface completion is currently retained under queue pressure.
    NotRetained(RadioTxDispatcherPhase),
}

/// Phase-aware advisory state for a retained permit-request channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "permit-request pressure and its recovery deadline must be handled"]
pub enum PermitRequestCapacity {
    /// The family retains its exact request, and channel capacity is unavailable.
    Pending {
        /// Packet-owner family retaining the request.
        family: DispatchFamily,
        /// Recovery threshold for the retained unsent request.
        grace_deadline_us: u64,
    },
    /// Advisory channel capacity is available for the retained request.
    Ready {
        /// Packet-owner family retaining the request.
        family: DispatchFamily,
        /// Recovery threshold still governing the subsequent retry.
        grace_deadline_us: u64,
    },
    /// No permit request is currently retained under channel pressure.
    NotRetained(RadioTxDispatcherPhase),
}

/// Result of one non-awaiting persistent transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "dispatcher progress and backpressure must be handled"]
pub enum RadioTxDispatcherStep {
    /// One ownership-preserving transition completed.
    Advanced,
    /// The transport-neutral interface job queue currently contains no work.
    NeedJob,
    /// A dropped receive future must be reconciled before any TX progress.
    NeedReceiveRecovery,
    /// Wait until this absolute monotonic microsecond timestamp.
    WaitUntil {
        /// Family retaining the job.
        family: DispatchFamily,
        /// Earliest next transition.
        retry_at_us: u64,
    },
    /// Invoke [`SoleRadioTxDispatcher::perform_radio_operation`] to run CAD.
    NeedCad(DispatchFamily),
    /// A pending owner is waiting for a permit reply.
    NeedPermitReply {
        /// Family retaining the pending owner.
        family: DispatchFamily,
        /// Recovery threshold for the outstanding exchange.
        grace_deadline_us: u64,
    },
    /// Invoke [`SoleRadioTxDispatcher::perform_radio_operation`] to finalize
    /// channel access, expose bytes once and transmit the logical packet.
    NeedTransmit(DispatchFamily),
    /// Wait for the node owner to acknowledge the exact authorized DATA observation.
    NeedAuthorizedFrameAcknowledgement,
    /// A full channel returned its unchanged value to persistent state.
    Backpressured(RadioTxDispatcherChannel),
    /// The machine is permanently fail-closed.
    Disabled(DispatcherFault),
}

/// Result of one short CAD or TX operation.
///
/// The terminal variant intentionally carries the complete scalar authorized
/// frame observation alongside radio progress; keeping both values together
/// prevents an executor scheduling boundary from separating their evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
#[must_use = "radio-operation progress must be handled"]
pub enum RadioOperationStep {
    /// The operation advanced persistent state without a terminal report.
    Advanced,
    /// One CAD observation was applied to channel access.
    CadObserved {
        /// Family whose channel contest advanced.
        family: DispatchFamily,
        /// Whether the configured LoRa activity was detected.
        activity_detected: bool,
        /// Adapter-sampled monotonic completion time.
        observed_at_us: u64,
    },
    /// The radio operation is terminal and its owning completion is retained.
    ///
    /// If the report contains an authorized-frame observation, completion is
    /// still gated on the exact node acknowledgement before it can enter the
    /// interface return path.
    Terminal(DispatchReport),
    /// The current phase has no radio operation to perform.
    NotReady,
    /// A previously started radio future must first be reconciled explicitly.
    CancelledFutureNeedsRecovery(DispatchFamily),
    /// A previously started receive future must first be reconciled explicitly.
    ReceiveFutureNeedsRecovery,
    /// The machine is permanently fail-closed.
    Disabled(DispatcherFault),
}

/// Result of starting or completing one idle-only receive operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "receive progress and cancellation recovery must be handled"]
pub enum RadioReceiveStep {
    /// A validated physical-frame observation accompanies bytes in the caller buffer.
    Frame(BoundedRxObservation),
    /// The configured symbol timeout expired before preamble detection.
    NoPreambleTimeout,
    /// An already-active TX owner excludes starting RX on the sole radio.
    TxPriority(RadioTxDispatcherPhase),
    /// A previously started receive future was dropped and requires recovery.
    CancelledFutureNeedsRecovery,
    /// The radio returned a terminal receive fault and the dispatcher disabled itself.
    Fault {
        /// Operation phase reported by the radio adapter.
        phase: SoleRadioFaultPhase,
        /// Stable failure class reported by the radio adapter.
        class: SoleRadioFaultClass,
    },
    /// The adapter reported more bytes than fit the caller-owned physical buffer.
    InvalidObservation {
        /// Untrusted driver-reported byte count.
        len: usize,
    },
    /// The machine is permanently fail-closed.
    Disabled(DispatcherFault),
}

/// Fail-closed reason retained by the dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatcherFault {
    /// An unsent DATA permit request crossed its recovery threshold.
    DataPermitRequestGraceExpired,
    /// An unsent ordinary permit request could not be cancelled consistently.
    OrdinaryPermitCancellationMismatch,
    /// An outstanding permit reply did not arrive by the recovery threshold.
    PermitReplyGraceExpired(DispatchFamily),
    /// A received permit reply did not match the sole pending owner.
    PermitReplyMismatch(DispatchFamily),
    /// A permit reply was queued while no exchange was active.
    OrphanPermitReply(DispatchFamily),
    /// The sole radio became terminally inactive.
    RadioUnavailable,
    /// A bounded receive future was explicitly recovered after cancellation.
    ReceiveCancelled,
    /// A receive adapter reported metadata outside the fixed buffer contract.
    ReceiveObservationInvalid,
    /// A terminal completion did not match its retained router ticket.
    CompletionTicketMismatch(DispatchFamily),
    /// A durable-frame acknowledgement did not match the retained observation.
    AuthorizedFrameAcknowledgementMismatch,
    /// An acknowledgement was queued before its observation request was accepted.
    UnexpectedAuthorizedFrameAcknowledgement,
    /// The actor capability rejected its own ticket-bound completion as foreign.
    CompletionQueueMismatch(DispatchFamily),
    /// Private family/state invariants disagreed.
    InternalInvariant,
}

/// Kind of exact non-`Copy` control value retained after disablement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatcherFaultResidueKind {
    /// Unsent destination-DATA permit request.
    DataPermitRequest,
    /// Destination-DATA mismatched or orphan permit reply.
    DataPermitReply,
    /// Ordinary mismatched request/reply pair.
    OrdinaryPermitMismatch,
    /// Ordinary mismatched or orphan permit reply.
    OrdinaryPermitReply,
    /// DATA completion and its mismatched one-shot router ticket.
    DataCompletionTicketMismatch,
    /// Ordinary completion and its mismatched one-shot router ticket.
    OrdinaryCompletionTicketMismatch,
    /// A completion or ticket was missing its expected active-family partner.
    CompletionTicketInvariant,
    /// A ticket-bound interface completion rejected by the actor capability.
    InterfaceCompletion,
    /// Expected and actual authorized-frame acknowledgement values differ.
    AuthorizedFrameAcknowledgementMismatch,
    /// An acknowledgement arrived before the dispatcher sent its retained request.
    UnexpectedAuthorizedFrameAcknowledgement,
}

#[derive(Clone, Copy)]
struct DispatchMeta {
    deadline_us: u64,
    grace_deadline_us: u64,
    frame_count: u8,
    aggregate_airtime_us: u64,
    permit_resource: TxPermitResourceId,
    airtime_profile: LabRxProfile,
}

impl DispatchMeta {
    fn try_new(
        deadline_ms: u64,
        grace_us: u64,
        frame_count: u8,
        aggregate_airtime_us: u64,
        permit_resource: TxPermitResourceId,
        airtime_profile: LabRxProfile,
    ) -> Option<Self> {
        let deadline_us = deadline_ms.checked_mul(1_000)?;
        Some(Self {
            deadline_us,
            grace_deadline_us: deadline_us.saturating_add(grace_us),
            frame_count,
            aggregate_airtime_us,
            permit_resource,
            airtime_profile,
        })
    }
}

enum DataRetainedControl {
    None,
    UnsentRequest(TxPermitRequest),
    Reply(TxPermitReply),
}

impl DataRetainedControl {
    fn kind(&self) -> Option<DispatcherFaultResidueKind> {
        match self {
            Self::None => None,
            Self::UnsentRequest(request) => {
                let _ = request;
                Some(DispatcherFaultResidueKind::DataPermitRequest)
            }
            Self::Reply(reply) => {
                let _ = reply;
                Some(DispatcherFaultResidueKind::DataPermitReply)
            }
        }
    }
}

enum OrdinaryRetainedControl {
    None,
    CancellationMismatch(reticulum_node_core::OrdinaryPermitCancelMismatch<'static>),
    Reply(OrdinaryTxPermitReply),
}

enum ActiveCompletionTicket {
    None,
    Data(DataCompletionTicket),
    Ordinary(OrdinaryCompletionTicket),
}

enum CompletionTicketResidue {
    DataTicket(DataCompletionTicket),
    OrdinaryTicket(OrdinaryCompletionTicket),
    DataMismatch(DataCompletionMismatch),
    OrdinaryMismatch(OrdinaryCompletionMismatch),
    DataWithoutTicket(TxCompletion<'static>),
    OrdinaryWithoutTicket(OrdinaryTxCompletion<'static>),
    DataWithOrdinaryTicket {
        ticket: OrdinaryCompletionTicket,
        completion: TxCompletion<'static>,
    },
    OrdinaryWithDataTicket {
        ticket: DataCompletionTicket,
        completion: OrdinaryTxCompletion<'static>,
    },
    InterfaceCompletion(InterfaceTxCompletion),
}

impl CompletionTicketResidue {
    fn kind(&self) -> DispatcherFaultResidueKind {
        match self {
            Self::DataTicket(ticket) => {
                let _ = ticket;
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::OrdinaryTicket(ticket) => {
                let _ = ticket;
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::DataMismatch(mismatch) => {
                let _ = mismatch;
                DispatcherFaultResidueKind::DataCompletionTicketMismatch
            }
            Self::OrdinaryMismatch(mismatch) => {
                let _ = mismatch;
                DispatcherFaultResidueKind::OrdinaryCompletionTicketMismatch
            }
            Self::DataWithoutTicket(completion) => {
                let _ = completion;
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::OrdinaryWithoutTicket(completion) => {
                let _ = completion;
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::DataWithOrdinaryTicket { ticket, completion } => {
                let _ = (ticket, completion);
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::OrdinaryWithDataTicket { ticket, completion } => {
                let _ = (ticket, completion);
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::InterfaceCompletion(completion) => {
                let _ = completion;
                DispatcherFaultResidueKind::InterfaceCompletion
            }
        }
    }
}

impl OrdinaryRetainedControl {
    fn kind(&self) -> Option<DispatcherFaultResidueKind> {
        match self {
            Self::None => None,
            Self::CancellationMismatch(mismatch) => {
                let _ = mismatch;
                Some(DispatcherFaultResidueKind::OrdinaryPermitMismatch)
            }
            Self::Reply(reply) => {
                let _ = reply;
                Some(DispatcherFaultResidueKind::OrdinaryPermitReply)
            }
        }
    }
}

enum DataAfterReturn {
    Resume,
    Disable {
        fault: DispatcherFault,
        retained: DataRetainedControl,
    },
}

enum OrdinaryAfterReturn {
    Resume,
    Disable {
        fault: DispatcherFault,
        retained: OrdinaryRetainedControl,
    },
}

enum DataState {
    Idle,
    Job(RoutedTxJob<'static>),
    Access {
        job: RoutedTxJob<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    CadInFlight {
        job: RoutedTxJob<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitSend {
        pending: PermitPendingTx<'static>,
        request: TxPermitRequest,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitWait {
        pending: PermitPendingTx<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitReply {
        pending: PermitPendingTx<'static>,
        reply: TxPermitReply,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    TransmitReady {
        owner: AuthorizedTx<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    TxInFlight {
        owner: AuthorizedTx<'static>,
        meta: DispatchMeta,
        authorized_frame: AuthorizedFrameObservation,
    },
    Expired {
        owner: ExpiredAuthorizedTx<'static>,
        meta: DispatchMeta,
    },
    Unpermitted {
        owner: UnpermittedTx<'static>,
        meta: DispatchMeta,
    },
    AuthorizedFrameRequest {
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
        expected: AuthorizedFrameObservation,
        request: AuthorizedFrameObservation,
    },
    AuthorizedFrameAcknowledgementWait {
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
        expected: AuthorizedFrameObservation,
    },
    AuthorizedFrameAcknowledgementFault {
        fault: DispatcherFault,
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
        request: Option<AuthorizedFrameObservation>,
        residue: AuthorizedFrameAcknowledgementMismatch,
    },
    Return {
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
    },
    InterfaceReturn {
        completion: InterfaceTxCompletion,
        after: DataAfterReturn,
    },
    Disabled {
        fault: DispatcherFault,
        retained: DataRetainedControl,
    },
    Transitioning,
}

enum OrdinaryState {
    Idle,
    Job(OrdinaryTxJob<'static>),
    Access {
        job: OrdinaryTxJob<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    CadInFlight {
        job: OrdinaryTxJob<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitSend {
        pending: OrdinaryPermitPendingTx<'static>,
        request: OrdinaryTxPermitRequest,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitWait {
        pending: OrdinaryPermitPendingTx<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitReply {
        pending: OrdinaryPermitPendingTx<'static>,
        reply: OrdinaryTxPermitReply,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    TransmitReady {
        owner: OrdinaryAuthorizedTx<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    TxInFlight {
        owner: OrdinaryAuthorizedTx<'static>,
        meta: DispatchMeta,
    },
    Expired {
        owner: OrdinaryExpiredAuthorizedTx<'static>,
        meta: DispatchMeta,
    },
    Unpermitted {
        owner: OrdinaryUnpermittedTx<'static>,
        meta: DispatchMeta,
    },
    Return {
        completion: OrdinaryTxCompletion<'static>,
        after: OrdinaryAfterReturn,
    },
    InterfaceReturn {
        completion: InterfaceTxCompletion,
        after: OrdinaryAfterReturn,
    },
    Disabled {
        fault: DispatcherFault,
        retained: OrdinaryRetainedControl,
    },
    Transitioning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveFamily {
    None,
    Data,
    Ordinary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReceiveState {
    Idle,
    InFlight,
    Disabled(DispatcherFault),
}

struct ReceiveCancellationGuard<'a, R>
where
    R: SoleRnodeRadio,
{
    radio: &'a mut R,
    preserve_active: bool,
}

impl<'a, R> ReceiveCancellationGuard<'a, R>
where
    R: SoleRnodeRadio,
{
    fn new(radio: &'a mut R) -> Self {
        Self {
            radio,
            preserve_active: false,
        }
    }

    fn preserve_active(&mut self) {
        self.preserve_active = true;
    }
}

impl<R> Drop for ReceiveCancellationGuard<'_, R>
where
    R: SoleRnodeRadio,
{
    fn drop(&mut self) {
        if !self.preserve_active {
            self.radio.shutdown();
        }
    }
}

/// Persistent serializer for destination-DATA and ordinary packets over one radio.
///
/// Store this value outside the executor task future that borrows it. All
/// non-`Copy` owners, requests, replies and completions reside in its separate
/// family-specific states. At most one family is active, so one logical
/// split-frame transmit retains sole radio ownership across both frames.
#[must_use = "dropping the dispatcher abandons its radio, handoffs, and retained owners"]
pub struct SoleRadioTxDispatcher<R, G, M, const QUEUE_DEPTH: usize>
where
    R: SoleRnodeRadio,
    G: RngCore + CryptoRng,
    M: RawMutex + 'static,
{
    radio: R,
    rng: G,
    tx_handoff: InterfaceTxActorHandoff<M, QUEUE_DEPTH>,
    data_permits: DispatcherPermitHandoff<M>,
    ordinary_permits: OrdinaryDispatcherPermitHandoff<M>,
    authorized_frames: AuthorizedFrameDispatcherHandoff<M>,
    config: RadioTxDispatcherConfig,
    data_state: DataState,
    ordinary_state: OrdinaryState,
    active_ticket: ActiveCompletionTicket,
    completion_ticket_residue: Option<CompletionTicketResidue>,
    receive_state: ReceiveState,
    active: ActiveFamily,
    sequence: u8,
    first_frame: RnodeTxFrameBuffer,
    second_frame: RnodeTxFrameBuffer,
    last_report: Option<DispatchReport>,
    last_receive_step: Option<RadioReceiveStep>,
}

impl<R, G, M, const QUEUE_DEPTH: usize> SoleRadioTxDispatcher<R, G, M, QUEUE_DEPTH>
where
    R: SoleRnodeRadio,
    G: RngCore + CryptoRng,
    M: RawMutex + 'static,
{
    /// Consume the sole radio, entropy source, interface TX capability, permit
    /// handoffs, and authorized-frame durability handoff.
    pub fn new(
        radio: R,
        rng: G,
        tx_handoff: InterfaceTxActorHandoff<M, QUEUE_DEPTH>,
        data_permits: DispatcherPermitHandoff<M>,
        ordinary_permits: OrdinaryDispatcherPermitHandoff<M>,
        authorized_frames: AuthorizedFrameDispatcherHandoff<M>,
        config: RadioTxDispatcherConfig,
    ) -> Self {
        Self {
            radio,
            rng,
            tx_handoff,
            data_permits,
            ordinary_permits,
            authorized_frames,
            config,
            data_state: DataState::Idle,
            ordinary_state: OrdinaryState::Idle,
            active_ticket: ActiveCompletionTicket::None,
            completion_ticket_residue: None,
            receive_state: ReceiveState::Idle,
            active: ActiveFamily::None,
            sequence: config.initial_rnode_sequence,
            first_frame: [0; reticulum_radio_interface::SX1262_FRAME_MTU],
            second_frame: [0; reticulum_radio_interface::SX1262_FRAME_MTU],
            last_report: None,
            last_receive_step: None,
        }
    }

    /// Borrow the initialized sole radio.
    pub const fn radio(&self) -> &R {
        &self.radio
    }

    /// Board-qualified watchdog bound for a started receive future.
    ///
    /// Expiry requires dropping the future and calling
    /// [`Self::recover_cancelled_radio_operation`]; it is not a normal receive
    /// timeout.
    pub fn maximum_receive_operation_us(&self) -> core::num::NonZeroU64 {
        self.radio.maximum_receive_operation_us()
    }

    /// Current high-level phase without exposing either typestate family.
    pub fn phase(&self) -> RadioTxDispatcherPhase {
        match self.receive_state {
            ReceiveState::InFlight => return RadioTxDispatcherPhase::ReceiveInFlight,
            ReceiveState::Disabled(_) => return RadioTxDispatcherPhase::Disabled,
            ReceiveState::Idle => {}
        }
        match self.active {
            ActiveFamily::None => RadioTxDispatcherPhase::Idle,
            ActiveFamily::Data => data_phase(&self.data_state),
            ActiveFamily::Ordinary => ordinary_phase(&self.ordinary_state),
        }
    }

    /// Most recent terminal scalar report, if any.
    ///
    /// This is diagnostic only. Authorized-frame ownership is retained in the
    /// DATA state machine, so clearing this value cannot bypass durability.
    pub const fn last_report(&self) -> Option<DispatchReport> {
        self.last_report
    }

    /// Clear and return the most recent terminal scalar report.
    ///
    /// This never clears a retained authorized-frame request or completion.
    pub fn take_last_report(&mut self) -> Option<DispatchReport> {
        self.last_report.take()
    }

    /// Most recent completed receive result, retained even if its future output is ignored.
    pub const fn last_receive_step(&self) -> Option<RadioReceiveStep> {
        self.last_receive_step
    }

    /// Clear and return the most recent completed receive result.
    pub fn take_last_receive_step(&mut self) -> Option<RadioReceiveStep> {
        self.last_receive_step.take()
    }

    /// Retained fail-closed reason, if any.
    pub fn fault(&self) -> Option<DispatcherFault> {
        if let ReceiveState::Disabled(fault) = self.receive_state {
            return Some(fault);
        }
        match (&self.data_state, &self.ordinary_state) {
            (DataState::Disabled { fault, .. }, _) => Some(*fault),
            (_, OrdinaryState::Disabled { fault, .. }) => Some(*fault),
            (
                DataState::Return {
                    after: DataAfterReturn::Disable { fault, .. },
                    ..
                },
                _,
            ) => Some(*fault),
            (
                DataState::InterfaceReturn {
                    after: DataAfterReturn::Disable { fault, .. },
                    ..
                },
                _,
            ) => Some(*fault),
            (
                DataState::AuthorizedFrameRequest {
                    after: DataAfterReturn::Disable { fault, .. },
                    ..
                }
                | DataState::AuthorizedFrameAcknowledgementWait {
                    after: DataAfterReturn::Disable { fault, .. },
                    ..
                },
                _,
            ) => Some(*fault),
            (DataState::AuthorizedFrameAcknowledgementFault { fault, .. }, _) => Some(*fault),
            (
                _,
                OrdinaryState::Return {
                    after: OrdinaryAfterReturn::Disable { fault, .. },
                    ..
                },
            ) => Some(*fault),
            (
                _,
                OrdinaryState::InterfaceReturn {
                    after: OrdinaryAfterReturn::Disable { fault, .. },
                    ..
                },
            ) => Some(*fault),
            _ => None,
        }
    }

    /// Kind of exact unmatched control value retained after a fault.
    pub fn fault_residue_kind(&self) -> Option<DispatcherFaultResidueKind> {
        if let Some(residue) = self.completion_ticket_residue.as_ref() {
            return Some(residue.kind());
        }
        match (&self.data_state, &self.ordinary_state) {
            (DataState::Disabled { retained, .. }, _) => retained.kind(),
            (_, OrdinaryState::Disabled { retained, .. }) => retained.kind(),
            (
                DataState::Return {
                    after: DataAfterReturn::Disable { retained, .. },
                    ..
                },
                _,
            ) => retained.kind(),
            (
                DataState::InterfaceReturn {
                    after: DataAfterReturn::Disable { retained, .. },
                    ..
                },
                _,
            ) => retained.kind(),
            (
                DataState::AuthorizedFrameRequest {
                    after: DataAfterReturn::Disable { retained, .. },
                    ..
                }
                | DataState::AuthorizedFrameAcknowledgementWait {
                    after: DataAfterReturn::Disable { retained, .. },
                    ..
                },
                _,
            ) => retained.kind(),
            (DataState::AuthorizedFrameAcknowledgementFault { fault, .. }, _) => {
                Some(match fault {
                    DispatcherFault::AuthorizedFrameAcknowledgementMismatch => {
                        DispatcherFaultResidueKind::AuthorizedFrameAcknowledgementMismatch
                    }
                    DispatcherFault::UnexpectedAuthorizedFrameAcknowledgement => {
                        DispatcherFaultResidueKind::UnexpectedAuthorizedFrameAcknowledgement
                    }
                    _ => DispatcherFaultResidueKind::AuthorizedFrameAcknowledgementMismatch,
                })
            }
            (
                _,
                OrdinaryState::Return {
                    after: OrdinaryAfterReturn::Disable { retained, .. },
                    ..
                },
            ) => retained.kind(),
            (
                _,
                OrdinaryState::InterfaceReturn {
                    after: OrdinaryAfterReturn::Disable { retained, .. },
                    ..
                },
            ) => retained.kind(),
            _ => None,
        }
    }

    /// Expected and actual observations retained after an acknowledgement mismatch.
    ///
    /// The associated owning completion and router ticket remain inaccessible
    /// inside the disabled dispatcher.
    pub fn authorized_frame_acknowledgement_mismatch(
        &self,
    ) -> Option<AuthorizedFrameAcknowledgementMismatch> {
        match &self.data_state {
            DataState::AuthorizedFrameAcknowledgementFault {
                fault: DispatcherFault::AuthorizedFrameAcknowledgementMismatch,
                residue,
                ..
            } => Some(*residue),
            _ => None,
        }
    }

    /// Expected request and premature acknowledgement retained after protocol violation.
    pub fn unexpected_authorized_frame_acknowledgement(
        &self,
    ) -> Option<AuthorizedFrameAcknowledgementMismatch> {
        match &self.data_state {
            DataState::AuthorizedFrameAcknowledgementFault {
                fault: DispatcherFault::UnexpectedAuthorizedFrameAcknowledgement,
                residue,
                ..
            } => Some(*residue),
            _ => None,
        }
    }

    /// Run exactly one non-awaiting ownership transition.
    ///
    /// `now_us` must be a fresh sample from the same monotonic microsecond
    /// domain used by the radio adapter's CAD/TX observations. Owner deadlines
    /// are converted from milliseconds with checked multiplication before a
    /// channel contest starts.
    pub fn step(&mut self, now_us: u64) -> RadioTxDispatcherStep {
        match self.receive_state {
            ReceiveState::InFlight => return RadioTxDispatcherStep::NeedReceiveRecovery,
            ReceiveState::Disabled(fault) => return RadioTxDispatcherStep::Disabled(fault),
            ReceiveState::Idle => {}
        }
        match self.active {
            ActiveFamily::None => self.step_idle(now_us),
            ActiveFamily::Data => self.step_data(now_us),
            ActiveFamily::Ordinary => self.step_ordinary(now_us),
        }
    }

    fn step_idle(&mut self, now_us: u64) -> RadioTxDispatcherStep {
        if let Some(fault) = self.reject_orphan_permit_reply() {
            return RadioTxDispatcherStep::Disabled(fault);
        }

        if let Some(job) = self.tx_handoff.try_receive_job() {
            return self.start_interface_job(job, now_us);
        }
        RadioTxDispatcherStep::NeedJob
    }

    fn reject_orphan_permit_reply(&mut self) -> Option<DispatcherFault> {
        if let Some(reply) = self.data_permits.replies().try_receive() {
            let fault = DispatcherFault::OrphanPermitReply(DispatchFamily::Data);
            self.data_state = DataState::Disabled {
                fault,
                retained: DataRetainedControl::Reply(reply),
            };
            self.active = ActiveFamily::Data;
            return Some(fault);
        }
        if let Some(reply) = self.ordinary_permits.replies().try_receive() {
            let fault = DispatcherFault::OrphanPermitReply(DispatchFamily::Ordinary);
            self.ordinary_state = OrdinaryState::Disabled {
                fault,
                retained: OrdinaryRetainedControl::Reply(reply),
            };
            self.active = ActiveFamily::Ordinary;
            return Some(fault);
        }
        None
    }

    fn start_interface_job(
        &mut self,
        interface_job: InterfaceTxJob,
        now_us: u64,
    ) -> RadioTxDispatcherStep {
        self.accept_interface_job(interface_job, Some(now_us))
    }

    fn accept_interface_job(
        &mut self,
        interface_job: InterfaceTxJob,
        start_at_us: Option<u64>,
    ) -> RadioTxDispatcherStep {
        if !matches!(self.active_ticket, ActiveCompletionTicket::None) {
            return self.disable_stray_ticket();
        }
        match interface_job {
            InterfaceTxJob::Data(interface_job) => {
                let stamped_config = interface_job.context().config();
                let config_matches = stamped_config == self.config.expected_interface_config;
                let (ticket, job) = interface_job.into_parts();
                self.active_ticket = ActiveCompletionTicket::Data(ticket);
                if !config_matches {
                    self.active = ActiveFamily::Data;
                    return self.finish_data_unpermitted_job(
                        job,
                        DispatchReport::new(
                            DispatchFamily::Data,
                            DispatchOutcome::InterfaceConfigurationMismatch {
                                expected: self.config.expected_interface_config,
                                stamped: stamped_config,
                            },
                            0,
                            None,
                        ),
                        false,
                    );
                }
                match start_at_us {
                    Some(now_us) => self.start_data(job, now_us),
                    None => {
                        self.data_state = DataState::Job(job);
                        self.active = ActiveFamily::Data;
                        RadioTxDispatcherStep::Advanced
                    }
                }
            }
            InterfaceTxJob::Ordinary(interface_job) => {
                let stamped_config = interface_job.context().config();
                let config_matches = stamped_config == self.config.expected_interface_config;
                let (ticket, job) = interface_job.into_parts();
                self.active_ticket = ActiveCompletionTicket::Ordinary(ticket);
                if !config_matches {
                    self.active = ActiveFamily::Ordinary;
                    return self.finish_ordinary_unpermitted_job(
                        job,
                        DispatchReport::new(
                            DispatchFamily::Ordinary,
                            DispatchOutcome::InterfaceConfigurationMismatch {
                                expected: self.config.expected_interface_config,
                                stamped: stamped_config,
                            },
                            0,
                            None,
                        ),
                        false,
                    );
                }
                match start_at_us {
                    Some(now_us) => self.start_ordinary(job, now_us),
                    None => {
                        self.ordinary_state = OrdinaryState::Job(job);
                        self.active = ActiveFamily::Ordinary;
                        RadioTxDispatcherStep::Advanced
                    }
                }
            }
        }
    }

    fn start_data(&mut self, job: RoutedTxJob<'static>, now_us: u64) -> RadioTxDispatcherStep {
        self.active = ActiveFamily::Data;
        let airtime_profile = self.radio.airtime_profile();
        let permit_resource =
            TxPermitResourceId::new(self.radio.configuration_fingerprint().as_bytes());
        match airtime_profile.rnode_packet_airtime(job.packet_len().into()) {
            Ok(airtime) => {
                let meta = match DispatchMeta::try_new(
                    job.deadline().instant().get(),
                    self.config.permit_recovery_grace_us,
                    airtime.frame_count(),
                    airtime.aggregate_time_on_air_us(),
                    permit_resource,
                    airtime_profile,
                ) {
                    Some(meta) => meta,
                    None => {
                        return self.finish_data_unpermitted_job(
                            job,
                            DispatchReport::new(
                                DispatchFamily::Data,
                                DispatchOutcome::DeadlineConversionOverflow,
                                airtime.frame_count(),
                                None,
                            ),
                            false,
                        );
                    }
                };
                if !self.radio.is_active() {
                    return self.finish_data_unpermitted_job(
                        job,
                        DispatchReport::new(
                            DispatchFamily::Data,
                            DispatchOutcome::RadioInactive,
                            airtime.frame_count(),
                            None,
                        ),
                        true,
                    );
                }
                match LogicalPacketChannelAccess::try_start(
                    self.config.channel_access,
                    airtime,
                    now_us,
                    meta.deadline_us,
                    self.rng.next_u64(),
                ) {
                    Ok(access) => {
                        self.data_state = DataState::Access { job, access, meta };
                        RadioTxDispatcherStep::Advanced
                    }
                    Err(rejection) => self.finish_data_unpermitted_job(
                        job,
                        DispatchReport::new(
                            DispatchFamily::Data,
                            DispatchOutcome::AccessRejected(rejection),
                            airtime.frame_count(),
                            None,
                        ),
                        false,
                    ),
                }
            }
            Err(error) => self.finish_data_unpermitted_job(
                job,
                DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::AirtimeRejected(error),
                    0,
                    None,
                ),
                true,
            ),
        }
    }

    fn start_ordinary(
        &mut self,
        job: OrdinaryTxJob<'static>,
        now_us: u64,
    ) -> RadioTxDispatcherStep {
        self.active = ActiveFamily::Ordinary;
        let airtime_profile = self.radio.airtime_profile();
        let permit_resource =
            TxPermitResourceId::new(self.radio.configuration_fingerprint().as_bytes());
        let airtime = match airtime_profile.rnode_packet_airtime(job.packet_len().into()) {
            Ok(airtime) => airtime,
            Err(error) => {
                return self.finish_ordinary_unpermitted_job(
                    job,
                    DispatchReport::new(
                        DispatchFamily::Ordinary,
                        DispatchOutcome::AirtimeRejected(error),
                        0,
                        None,
                    ),
                    true,
                );
            }
        };
        let meta = match DispatchMeta::try_new(
            job.deadline().instant().get(),
            self.config.permit_recovery_grace_us,
            airtime.frame_count(),
            airtime.aggregate_time_on_air_us(),
            permit_resource,
            airtime_profile,
        ) {
            Some(meta) => meta,
            None => {
                return self.finish_ordinary_unpermitted_job(
                    job,
                    DispatchReport::new(
                        DispatchFamily::Ordinary,
                        DispatchOutcome::DeadlineConversionOverflow,
                        airtime.frame_count(),
                        None,
                    ),
                    false,
                );
            }
        };
        if !self.radio.is_active() {
            return self.finish_ordinary_unpermitted_job(
                job,
                DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::RadioInactive,
                    airtime.frame_count(),
                    None,
                ),
                true,
            );
        }
        match LogicalPacketChannelAccess::try_start(
            self.config.channel_access,
            airtime,
            now_us,
            meta.deadline_us,
            self.rng.next_u64(),
        ) {
            Ok(access) => {
                self.ordinary_state = OrdinaryState::Access { job, access, meta };
                RadioTxDispatcherStep::Advanced
            }
            Err(rejection) => self.finish_ordinary_unpermitted_job(
                job,
                DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::AccessRejected(rejection),
                    airtime.frame_count(),
                    None,
                ),
                false,
            ),
        }
    }

    fn step_data(&mut self, now_us: u64) -> RadioTxDispatcherStep {
        let state = mem::replace(&mut self.data_state, DataState::Transitioning);
        match state {
            DataState::Access {
                job,
                mut access,
                meta,
            } => match access.phase() {
                LogicalPacketAccessPhase::BackingOff => match access.poll_backoff(now_us) {
                    Ok(action) => self.apply_data_access_action(job, access, meta, action),
                    Err(_) => self.fail_data_job_invariant(job, meta),
                },
                LogicalPacketAccessPhase::AwaitingCad => {
                    self.data_state = DataState::Access { job, access, meta };
                    RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
                }
                _ => self.fail_data_job_invariant(job, meta),
            },
            DataState::CadInFlight { job, access, meta } => {
                self.data_state = DataState::CadInFlight { job, access, meta };
                RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
            }
            DataState::PermitSend {
                pending,
                request,
                access,
                meta,
            } => {
                if now_us >= meta.grace_deadline_us {
                    let report = DispatchReport::new(
                        DispatchFamily::Data,
                        DispatchOutcome::ControlPlaneRecovery,
                        meta.frame_count,
                        None,
                    );
                    self.record_report(report);
                    self.data_state = DataState::Return {
                        completion: pending
                            .recovery_fault(self.config.completion_codes.control_plane_recovery),
                        after: DataAfterReturn::Disable {
                            fault: DispatcherFault::DataPermitRequestGraceExpired,
                            retained: DataRetainedControl::UnsentRequest(request),
                        },
                    };
                    RadioTxDispatcherStep::Advanced
                } else {
                    match self.data_permits.requests().try_send(request) {
                        Ok(()) => {
                            self.data_state = DataState::PermitWait {
                                pending,
                                access,
                                meta,
                            };
                            RadioTxDispatcherStep::Advanced
                        }
                        Err(full) => {
                            self.data_state = DataState::PermitSend {
                                pending,
                                request: full.into_inner(),
                                access,
                                meta,
                            };
                            RadioTxDispatcherStep::Backpressured(
                                RadioTxDispatcherChannel::DataPermitRequest,
                            )
                        }
                    }
                }
            }
            DataState::PermitWait {
                pending,
                access,
                meta,
            } => {
                if let Some(reply) = self.data_permits.replies().try_receive() {
                    self.data_state = DataState::PermitReply {
                        pending,
                        reply,
                        access,
                        meta,
                    };
                    RadioTxDispatcherStep::Advanced
                } else if now_us >= meta.grace_deadline_us {
                    let report = DispatchReport::new(
                        DispatchFamily::Data,
                        DispatchOutcome::ControlPlaneRecovery,
                        meta.frame_count,
                        None,
                    );
                    self.record_report(report);
                    self.data_state = DataState::Return {
                        completion: pending
                            .recovery_fault(self.config.completion_codes.control_plane_recovery),
                        after: DataAfterReturn::Disable {
                            fault: DispatcherFault::PermitReplyGraceExpired(DispatchFamily::Data),
                            retained: DataRetainedControl::None,
                        },
                    };
                    RadioTxDispatcherStep::Advanced
                } else {
                    self.data_state = DataState::PermitWait {
                        pending,
                        access,
                        meta,
                    };
                    RadioTxDispatcherStep::NeedPermitReply {
                        family: DispatchFamily::Data,
                        grace_deadline_us: meta.grace_deadline_us,
                    }
                }
            }
            DataState::PermitReply {
                pending,
                reply,
                access,
                meta,
            } => match pending.resolve(reply, monotonic_millis_ceiling(now_us)) {
                Ok(PermitResolution::Authorized(owner)) => {
                    self.data_state = DataState::TransmitReady {
                        owner,
                        access,
                        meta,
                    };
                    RadioTxDispatcherStep::Advanced
                }
                Ok(PermitResolution::Expired(owner)) => {
                    self.data_state = DataState::Expired { owner, meta };
                    RadioTxDispatcherStep::Advanced
                }
                Ok(PermitResolution::Unpermitted(owner)) => {
                    self.data_state = DataState::Unpermitted { owner, meta };
                    RadioTxDispatcherStep::Advanced
                }
                Err(mismatch) => self.fail_data_mismatch(mismatch, meta),
            },
            DataState::TransmitReady {
                owner,
                access,
                meta,
            } => {
                self.data_state = DataState::TransmitReady {
                    owner,
                    access,
                    meta,
                };
                RadioTxDispatcherStep::NeedTransmit(DispatchFamily::Data)
            }
            DataState::TxInFlight {
                owner,
                meta,
                authorized_frame,
            } => {
                self.data_state = DataState::TxInFlight {
                    owner,
                    meta,
                    authorized_frame,
                };
                RadioTxDispatcherStep::NeedTransmit(DispatchFamily::Data)
            }
            DataState::Expired { owner, meta } => {
                let report = DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::AuthorizationExpired,
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.data_state = DataState::Return {
                    completion: owner.complete(self.config.completion_codes.expired_authorization),
                    after: DataAfterReturn::Resume,
                };
                RadioTxDispatcherStep::Advanced
            }
            DataState::Unpermitted { owner, meta } => {
                let report = DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::PermitDenied,
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.data_state = DataState::Return {
                    completion: owner.complete(self.config.completion_codes.unpermitted),
                    after: DataAfterReturn::Resume,
                };
                RadioTxDispatcherStep::Advanced
            }
            DataState::AuthorizedFrameRequest {
                completion,
                after,
                expected,
                request,
            } => {
                if let Some(actual) = self.authorized_frames.acknowledgements().try_receive() {
                    self.data_state = DataState::AuthorizedFrameAcknowledgementFault {
                        fault: DispatcherFault::UnexpectedAuthorizedFrameAcknowledgement,
                        completion,
                        after,
                        request: Some(request),
                        residue: AuthorizedFrameAcknowledgementMismatch { expected, actual },
                    };
                    return RadioTxDispatcherStep::Disabled(
                        DispatcherFault::UnexpectedAuthorizedFrameAcknowledgement,
                    );
                }
                match self.authorized_frames.requests().try_send(request) {
                    Ok(()) => {
                        self.data_state = DataState::AuthorizedFrameAcknowledgementWait {
                            completion,
                            after,
                            expected,
                        };
                        RadioTxDispatcherStep::Advanced
                    }
                    Err(full) => {
                        self.data_state = DataState::AuthorizedFrameRequest {
                            completion,
                            after,
                            expected,
                            request: full.into_inner(),
                        };
                        RadioTxDispatcherStep::Backpressured(
                            RadioTxDispatcherChannel::AuthorizedFrameObservationRequest,
                        )
                    }
                }
            }
            DataState::AuthorizedFrameAcknowledgementWait {
                completion,
                after,
                expected,
            } => match self.authorized_frames.acknowledgements().try_receive() {
                Some(actual) => {
                    authorized_frame_progress_step(self.apply_authorized_frame_acknowledgement(
                        completion, after, expected, actual,
                    ))
                }
                None => {
                    self.data_state = DataState::AuthorizedFrameAcknowledgementWait {
                        completion,
                        after,
                        expected,
                    };
                    RadioTxDispatcherStep::NeedAuthorizedFrameAcknowledgement
                }
            },
            DataState::AuthorizedFrameAcknowledgementFault {
                fault,
                completion,
                after,
                request,
                residue,
            } => {
                self.data_state = DataState::AuthorizedFrameAcknowledgementFault {
                    fault,
                    completion,
                    after,
                    request,
                    residue,
                };
                RadioTxDispatcherStep::Disabled(fault)
            }
            DataState::Return { completion, after } => {
                self.return_data_completion(completion, after)
            }
            DataState::InterfaceReturn { completion, after } => {
                self.retry_data_interface_completion(completion, after)
            }
            DataState::Disabled { fault, retained } => {
                self.data_state = DataState::Disabled { fault, retained };
                RadioTxDispatcherStep::Disabled(fault)
            }
            DataState::Idle | DataState::Transitioning => self.disable_internal(),
            DataState::Job(job) => self.start_data(job, now_us),
        }
    }

    fn step_ordinary(&mut self, now_us: u64) -> RadioTxDispatcherStep {
        let state = mem::replace(&mut self.ordinary_state, OrdinaryState::Transitioning);
        match state {
            OrdinaryState::Access {
                job,
                mut access,
                meta,
            } => match access.phase() {
                LogicalPacketAccessPhase::BackingOff => match access.poll_backoff(now_us) {
                    Ok(action) => self.apply_ordinary_access_action(job, access, meta, action),
                    Err(_) => self.fail_ordinary_job_invariant(job, meta),
                },
                LogicalPacketAccessPhase::AwaitingCad => {
                    self.ordinary_state = OrdinaryState::Access { job, access, meta };
                    RadioTxDispatcherStep::NeedCad(DispatchFamily::Ordinary)
                }
                _ => self.fail_ordinary_job_invariant(job, meta),
            },
            OrdinaryState::CadInFlight { job, access, meta } => {
                self.ordinary_state = OrdinaryState::CadInFlight { job, access, meta };
                RadioTxDispatcherStep::NeedCad(DispatchFamily::Ordinary)
            }
            OrdinaryState::PermitSend {
                pending,
                request,
                access,
                meta,
            } => {
                if now_us >= meta.grace_deadline_us {
                    match pending.cancel_before_send(
                        request,
                        self.config.completion_codes.control_plane_recovery,
                    ) {
                        Ok(completion) => {
                            let report = DispatchReport::new(
                                DispatchFamily::Ordinary,
                                DispatchOutcome::ControlPlaneRecovery,
                                meta.frame_count,
                                None,
                            );
                            self.record_report(report);
                            self.ordinary_state = OrdinaryState::Return {
                                completion,
                                after: OrdinaryAfterReturn::Resume,
                            };
                            RadioTxDispatcherStep::Advanced
                        }
                        Err(mismatch) => {
                            let fault = DispatcherFault::OrdinaryPermitCancellationMismatch;
                            self.ordinary_state = OrdinaryState::Disabled {
                                fault,
                                retained: OrdinaryRetainedControl::CancellationMismatch(mismatch),
                            };
                            RadioTxDispatcherStep::Disabled(fault)
                        }
                    }
                } else {
                    match self.ordinary_permits.requests().try_send(request) {
                        Ok(()) => {
                            self.ordinary_state = OrdinaryState::PermitWait {
                                pending,
                                access,
                                meta,
                            };
                            RadioTxDispatcherStep::Advanced
                        }
                        Err(full) => {
                            self.ordinary_state = OrdinaryState::PermitSend {
                                pending,
                                request: full.into_inner(),
                                access,
                                meta,
                            };
                            RadioTxDispatcherStep::Backpressured(
                                RadioTxDispatcherChannel::OrdinaryPermitRequest,
                            )
                        }
                    }
                }
            }
            OrdinaryState::PermitWait {
                pending,
                access,
                meta,
            } => {
                if let Some(reply) = self.ordinary_permits.replies().try_receive() {
                    self.ordinary_state = OrdinaryState::PermitReply {
                        pending,
                        reply,
                        access,
                        meta,
                    };
                    RadioTxDispatcherStep::Advanced
                } else if now_us >= meta.grace_deadline_us {
                    let report = DispatchReport::new(
                        DispatchFamily::Ordinary,
                        DispatchOutcome::ControlPlaneRecovery,
                        meta.frame_count,
                        None,
                    );
                    self.record_report(report);
                    self.ordinary_state = OrdinaryState::Return {
                        completion: pending
                            .recovery_fault(self.config.completion_codes.control_plane_recovery),
                        after: OrdinaryAfterReturn::Disable {
                            fault: DispatcherFault::PermitReplyGraceExpired(
                                DispatchFamily::Ordinary,
                            ),
                            retained: OrdinaryRetainedControl::None,
                        },
                    };
                    RadioTxDispatcherStep::Advanced
                } else {
                    self.ordinary_state = OrdinaryState::PermitWait {
                        pending,
                        access,
                        meta,
                    };
                    RadioTxDispatcherStep::NeedPermitReply {
                        family: DispatchFamily::Ordinary,
                        grace_deadline_us: meta.grace_deadline_us,
                    }
                }
            }
            OrdinaryState::PermitReply {
                pending,
                reply,
                access,
                meta,
            } => match pending.resolve(reply, monotonic_millis_ceiling(now_us)) {
                Ok(OrdinaryPermitResolution::Authorized(owner)) => {
                    self.ordinary_state = OrdinaryState::TransmitReady {
                        owner,
                        access,
                        meta,
                    };
                    RadioTxDispatcherStep::Advanced
                }
                Ok(OrdinaryPermitResolution::Expired(owner)) => {
                    self.ordinary_state = OrdinaryState::Expired { owner, meta };
                    RadioTxDispatcherStep::Advanced
                }
                Ok(OrdinaryPermitResolution::Unpermitted(owner)) => {
                    self.ordinary_state = OrdinaryState::Unpermitted { owner, meta };
                    RadioTxDispatcherStep::Advanced
                }
                Err(mismatch) => self.fail_ordinary_mismatch(mismatch, meta),
            },
            OrdinaryState::TransmitReady {
                owner,
                access,
                meta,
            } => {
                self.ordinary_state = OrdinaryState::TransmitReady {
                    owner,
                    access,
                    meta,
                };
                RadioTxDispatcherStep::NeedTransmit(DispatchFamily::Ordinary)
            }
            OrdinaryState::TxInFlight { owner, meta } => {
                self.ordinary_state = OrdinaryState::TxInFlight { owner, meta };
                RadioTxDispatcherStep::NeedTransmit(DispatchFamily::Ordinary)
            }
            OrdinaryState::Expired { owner, meta } => {
                let report = DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::AuthorizationExpired,
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: owner.cancel(self.config.completion_codes.expired_authorization),
                    after: OrdinaryAfterReturn::Resume,
                };
                RadioTxDispatcherStep::Advanced
            }
            OrdinaryState::Unpermitted { owner, meta } => {
                let report = DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::PermitDenied,
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: owner.complete(self.config.completion_codes.unpermitted),
                    after: OrdinaryAfterReturn::Resume,
                };
                RadioTxDispatcherStep::Advanced
            }
            OrdinaryState::Return { completion, after } => {
                self.return_ordinary_completion(completion, after)
            }
            OrdinaryState::InterfaceReturn { completion, after } => {
                self.retry_ordinary_interface_completion(completion, after)
            }
            OrdinaryState::Disabled { fault, retained } => {
                self.ordinary_state = OrdinaryState::Disabled { fault, retained };
                RadioTxDispatcherStep::Disabled(fault)
            }
            OrdinaryState::Job(job) => self.start_ordinary(job, now_us),
            OrdinaryState::Idle | OrdinaryState::Transitioning => self.disable_internal(),
        }
    }

    fn return_data_completion(
        &mut self,
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
    ) -> RadioTxDispatcherStep {
        let ticket = mem::replace(&mut self.active_ticket, ActiveCompletionTicket::None);
        let completion = match ticket {
            ActiveCompletionTicket::Data(ticket) => match ticket.complete(completion) {
                Ok(completion) => completion,
                Err(mismatch) => {
                    self.completion_ticket_residue =
                        Some(CompletionTicketResidue::DataMismatch(mismatch));
                    let fault = DispatcherFault::CompletionTicketMismatch(DispatchFamily::Data);
                    self.data_state = DataState::Disabled {
                        fault,
                        retained: DataRetainedControl::None,
                    };
                    return RadioTxDispatcherStep::Disabled(fault);
                }
            },
            ActiveCompletionTicket::None => {
                self.completion_ticket_residue =
                    Some(CompletionTicketResidue::DataWithoutTicket(completion));
                let fault = DispatcherFault::CompletionTicketMismatch(DispatchFamily::Data);
                self.data_state = DataState::Disabled {
                    fault,
                    retained: DataRetainedControl::None,
                };
                return RadioTxDispatcherStep::Disabled(fault);
            }
            ActiveCompletionTicket::Ordinary(ticket) => {
                self.completion_ticket_residue =
                    Some(CompletionTicketResidue::DataWithOrdinaryTicket { ticket, completion });
                let fault = DispatcherFault::CompletionTicketMismatch(DispatchFamily::Data);
                self.data_state = DataState::Disabled {
                    fault,
                    retained: DataRetainedControl::None,
                };
                return RadioTxDispatcherStep::Disabled(fault);
            }
        };
        self.retry_data_interface_completion(completion, after)
    }

    fn gate_data_completion_on_authorized_frame(
        &mut self,
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
        expected: AuthorizedFrameObservation,
    ) {
        self.data_state = DataState::AuthorizedFrameRequest {
            completion,
            after,
            expected,
            request: expected,
        };
    }

    fn apply_authorized_frame_acknowledgement(
        &mut self,
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
        expected: AuthorizedFrameObservation,
        actual: AuthorizedFrameObservation,
    ) -> AuthorizedFrameAcknowledgementProgress {
        if actual == expected {
            self.data_state = DataState::Return { completion, after };
            AuthorizedFrameAcknowledgementProgress::Matched
        } else {
            self.data_state = DataState::AuthorizedFrameAcknowledgementFault {
                fault: DispatcherFault::AuthorizedFrameAcknowledgementMismatch,
                completion,
                after,
                request: None,
                residue: AuthorizedFrameAcknowledgementMismatch { expected, actual },
            };
            AuthorizedFrameAcknowledgementProgress::Disabled(
                DispatcherFault::AuthorizedFrameAcknowledgementMismatch,
            )
        }
    }

    fn retry_data_interface_completion(
        &mut self,
        completion: InterfaceTxCompletion,
        after: DataAfterReturn,
    ) -> RadioTxDispatcherStep {
        match self.tx_handoff.try_send_completion(completion) {
            Ok(()) => match after {
                DataAfterReturn::Resume => {
                    self.data_state = DataState::Idle;
                    self.active = ActiveFamily::None;
                    RadioTxDispatcherStep::Advanced
                }
                DataAfterReturn::Disable { fault, retained } => {
                    self.data_state = DataState::Disabled { fault, retained };
                    RadioTxDispatcherStep::Disabled(fault)
                }
            },
            Err(failure) => {
                let reason = failure.reason();
                let completion = failure.into_completion();
                match reason {
                    ActorCompletionSendError::QueueFull(_) => {
                        self.data_state = DataState::InterfaceReturn { completion, after };
                        RadioTxDispatcherStep::Backpressured(
                            RadioTxDispatcherChannel::InterfaceCompletion(DispatchFamily::Data),
                        )
                    }
                    ActorCompletionSendError::ForeignQueue { .. } => {
                        self.completion_ticket_residue =
                            Some(CompletionTicketResidue::InterfaceCompletion(completion));
                        let fault = DispatcherFault::CompletionQueueMismatch(DispatchFamily::Data);
                        self.data_state = DataState::Disabled {
                            fault,
                            retained: DataRetainedControl::None,
                        };
                        RadioTxDispatcherStep::Disabled(fault)
                    }
                }
            }
        }
    }

    fn return_ordinary_completion(
        &mut self,
        completion: OrdinaryTxCompletion<'static>,
        after: OrdinaryAfterReturn,
    ) -> RadioTxDispatcherStep {
        let ticket = mem::replace(&mut self.active_ticket, ActiveCompletionTicket::None);
        let completion = match ticket {
            ActiveCompletionTicket::Ordinary(ticket) => match ticket.complete(completion) {
                Ok(completion) => completion,
                Err(mismatch) => {
                    self.completion_ticket_residue =
                        Some(CompletionTicketResidue::OrdinaryMismatch(mismatch));
                    let fault = DispatcherFault::CompletionTicketMismatch(DispatchFamily::Ordinary);
                    self.ordinary_state = OrdinaryState::Disabled {
                        fault,
                        retained: OrdinaryRetainedControl::None,
                    };
                    return RadioTxDispatcherStep::Disabled(fault);
                }
            },
            ActiveCompletionTicket::None => {
                self.completion_ticket_residue =
                    Some(CompletionTicketResidue::OrdinaryWithoutTicket(completion));
                let fault = DispatcherFault::CompletionTicketMismatch(DispatchFamily::Ordinary);
                self.ordinary_state = OrdinaryState::Disabled {
                    fault,
                    retained: OrdinaryRetainedControl::None,
                };
                return RadioTxDispatcherStep::Disabled(fault);
            }
            ActiveCompletionTicket::Data(ticket) => {
                self.completion_ticket_residue =
                    Some(CompletionTicketResidue::OrdinaryWithDataTicket { ticket, completion });
                let fault = DispatcherFault::CompletionTicketMismatch(DispatchFamily::Ordinary);
                self.ordinary_state = OrdinaryState::Disabled {
                    fault,
                    retained: OrdinaryRetainedControl::None,
                };
                return RadioTxDispatcherStep::Disabled(fault);
            }
        };
        self.retry_ordinary_interface_completion(completion, after)
    }

    fn retry_ordinary_interface_completion(
        &mut self,
        completion: InterfaceTxCompletion,
        after: OrdinaryAfterReturn,
    ) -> RadioTxDispatcherStep {
        match self.tx_handoff.try_send_completion(completion) {
            Ok(()) => match after {
                OrdinaryAfterReturn::Resume => {
                    self.ordinary_state = OrdinaryState::Idle;
                    self.active = ActiveFamily::None;
                    RadioTxDispatcherStep::Advanced
                }
                OrdinaryAfterReturn::Disable { fault, retained } => {
                    self.ordinary_state = OrdinaryState::Disabled { fault, retained };
                    RadioTxDispatcherStep::Disabled(fault)
                }
            },
            Err(failure) => {
                let reason = failure.reason();
                let completion = failure.into_completion();
                match reason {
                    ActorCompletionSendError::QueueFull(_) => {
                        self.ordinary_state = OrdinaryState::InterfaceReturn { completion, after };
                        RadioTxDispatcherStep::Backpressured(
                            RadioTxDispatcherChannel::InterfaceCompletion(DispatchFamily::Ordinary),
                        )
                    }
                    ActorCompletionSendError::ForeignQueue { .. } => {
                        self.completion_ticket_residue =
                            Some(CompletionTicketResidue::InterfaceCompletion(completion));
                        let fault =
                            DispatcherFault::CompletionQueueMismatch(DispatchFamily::Ordinary);
                        self.ordinary_state = OrdinaryState::Disabled {
                            fault,
                            retained: OrdinaryRetainedControl::None,
                        };
                        RadioTxDispatcherStep::Disabled(fault)
                    }
                }
            }
        }
    }

    /// Start one receive operation when no TX owner is already active.
    ///
    /// Calling this method is an explicit scheduler decision to service RX.
    /// Jobs already waiting in the actor queue remain queued and unchanged;
    /// the scheduler can run a bounded receive and then call [`Self::step`] or
    /// [`Self::wait_for_job`] to select TX. On success RX is marked in flight
    /// before the adapter future is constructed. Consequently, dropping the
    /// returned future without polling it still leaves a persistent recovery
    /// obligation and invokes the adapter's unpolled-cancellation containment.
    ///
    /// The returned future must run to a frame or normal no-preamble timeout.
    /// If a supervisor applies a whole-operation watchdog after preamble
    /// detection, dropping the future is terminal cancellation and must be
    /// followed by [`Self::recover_cancelled_radio_operation`].
    pub fn start_receive<'a>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
    ) -> Result<impl core::future::Future<Output = RadioReceiveStep> + 'a, RadioReceiveStep> {
        match self.receive_state {
            ReceiveState::InFlight => {
                return Err(RadioReceiveStep::CancelledFutureNeedsRecovery);
            }
            ReceiveState::Disabled(fault) => return Err(RadioReceiveStep::Disabled(fault)),
            ReceiveState::Idle => {}
        }

        if self.active != ActiveFamily::None {
            return Err(RadioReceiveStep::TxPriority(self.phase()));
        }

        if let Some(fault) = self.reject_orphan_permit_reply() {
            return Err(RadioReceiveStep::Disabled(fault));
        }

        if !self.radio.is_active() {
            self.radio.shutdown();
            self.receive_state = ReceiveState::Disabled(DispatcherFault::RadioUnavailable);
            return Err(RadioReceiveStep::Disabled(
                DispatcherFault::RadioUnavailable,
            ));
        }

        self.receive_state = ReceiveState::InFlight;
        let receive_state = &mut self.receive_state;
        let last_receive_step = &mut self.last_receive_step;
        let mut guard = ReceiveCancellationGuard::new(&mut self.radio);

        Ok(async move {
            let result = guard.radio.receive_bounded(buffer).await;
            let step = match result {
                Ok(BoundedRxOutcome::Frame(observation)) => {
                    if observation.len() > SX1262_FRAME_MTU {
                        *receive_state =
                            ReceiveState::Disabled(DispatcherFault::ReceiveObservationInvalid);
                        RadioReceiveStep::InvalidObservation {
                            len: observation.len(),
                        }
                    } else if !guard.radio.is_active() {
                        *receive_state = ReceiveState::Disabled(DispatcherFault::RadioUnavailable);
                        RadioReceiveStep::Disabled(DispatcherFault::RadioUnavailable)
                    } else {
                        guard.preserve_active();
                        *receive_state = ReceiveState::Idle;
                        RadioReceiveStep::Frame(observation)
                    }
                }
                Ok(BoundedRxOutcome::NoPreambleTimeout) => {
                    if !guard.radio.is_active() {
                        *receive_state = ReceiveState::Disabled(DispatcherFault::RadioUnavailable);
                        RadioReceiveStep::Disabled(DispatcherFault::RadioUnavailable)
                    } else {
                        guard.preserve_active();
                        *receive_state = ReceiveState::Idle;
                        RadioReceiveStep::NoPreambleTimeout
                    }
                }
                Err(fault) => {
                    *receive_state = ReceiveState::Disabled(DispatcherFault::RadioUnavailable);
                    RadioReceiveStep::Fault {
                        phase: fault.phase(),
                        class: fault.class(),
                    }
                }
            };
            *last_receive_step = Some(step);
            step
        })
    }

    /// Perform the one short radio operation required by the current phase.
    ///
    /// For TX, `dispatch_start_observed_at_us` must be sampled immediately
    /// before this call. The method applies the matching grant's actual
    /// reservation to channel access, exposes authorized bytes once, assigns
    /// one wrapping RNode sequence, frames the complete packet, and makes one
    /// [`SoleRnodeRadio::transmit`] call spanning both physical frames.
    pub async fn perform_radio_operation(
        &mut self,
        dispatch_start_observed_at_us: u64,
    ) -> RadioOperationStep {
        match (self.active, self.phase()) {
            (ActiveFamily::Data, RadioTxDispatcherPhase::CadReady(DispatchFamily::Data)) => {
                self.perform_data_cad().await
            }
            (
                ActiveFamily::Ordinary,
                RadioTxDispatcherPhase::CadReady(DispatchFamily::Ordinary),
            ) => self.perform_ordinary_cad().await,
            (ActiveFamily::Data, RadioTxDispatcherPhase::TransmitReady(DispatchFamily::Data)) => {
                self.perform_data_tx(dispatch_start_observed_at_us).await
            }
            (
                ActiveFamily::Ordinary,
                RadioTxDispatcherPhase::TransmitReady(DispatchFamily::Ordinary),
            ) => {
                self.perform_ordinary_tx(dispatch_start_observed_at_us)
                    .await
            }
            (
                ActiveFamily::Data,
                RadioTxDispatcherPhase::CadInFlight(DispatchFamily::Data)
                | RadioTxDispatcherPhase::TransmitInFlight(DispatchFamily::Data),
            ) => RadioOperationStep::CancelledFutureNeedsRecovery(DispatchFamily::Data),
            (
                ActiveFamily::Ordinary,
                RadioTxDispatcherPhase::CadInFlight(DispatchFamily::Ordinary)
                | RadioTxDispatcherPhase::TransmitInFlight(DispatchFamily::Ordinary),
            ) => RadioOperationStep::CancelledFutureNeedsRecovery(DispatchFamily::Ordinary),
            (_, RadioTxDispatcherPhase::ReceiveInFlight) => {
                RadioOperationStep::ReceiveFutureNeedsRecovery
            }
            (_, RadioTxDispatcherPhase::Disabled) => RadioOperationStep::Disabled(
                self.fault().unwrap_or(DispatcherFault::InternalInvariant),
            ),
            _ => RadioOperationStep::NotReady,
        }
    }

    async fn perform_data_cad(&mut self) -> RadioOperationStep {
        let (job, access, meta) = match mem::replace(&mut self.data_state, DataState::Transitioning)
        {
            DataState::Access { job, access, meta }
                if access.phase() == LogicalPacketAccessPhase::AwaitingCad =>
            {
                (job, access, meta)
            }
            state => {
                self.data_state = state;
                return RadioOperationStep::NotReady;
            }
        };
        self.data_state = DataState::CadInFlight { job, access, meta };
        let result = self.radio.cad().await;
        let (job, mut access, meta) =
            match mem::replace(&mut self.data_state, DataState::Transitioning) {
                DataState::CadInFlight { job, access, meta } => (job, access, meta),
                _ => return self.disable_internal_radio(),
            };
        match result {
            Ok(observation) => {
                let activity_detected = observation.activity_detected();
                let observed_at_us = observation.observed_at_us();
                match access.observe_cad(observed_at_us, activity_detected, self.rng.next_u64()) {
                    Ok(action) => {
                        let _ = self.apply_data_access_action(job, access, meta, action);
                        if matches!(self.data_state, DataState::Return { .. }) {
                            RadioOperationStep::Terminal(
                                self.last_report.expect("terminal CAD report recorded"),
                            )
                        } else if let DataState::Disabled { fault, .. } = &self.data_state {
                            RadioOperationStep::Disabled(*fault)
                        } else {
                            RadioOperationStep::CadObserved {
                                family: DispatchFamily::Data,
                                activity_detected,
                                observed_at_us,
                            }
                        }
                    }
                    Err(_) => {
                        let _ = self.fail_data_job_invariant(job, meta);
                        RadioOperationStep::Terminal(self.last_report.expect("report recorded"))
                    }
                }
            }
            Err(fault) => {
                let report = DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::CadFault {
                        phase: fault.phase(),
                        class: fault.class(),
                    },
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.data_state = DataState::Return {
                    completion: job
                        .return_unpermitted()
                        .complete(self.config.completion_codes.cad_fault),
                    after: DataAfterReturn::Disable {
                        fault: DispatcherFault::RadioUnavailable,
                        retained: DataRetainedControl::None,
                    },
                };
                RadioOperationStep::Terminal(report)
            }
        }
    }

    async fn perform_ordinary_cad(&mut self) -> RadioOperationStep {
        let (job, access, meta) =
            match mem::replace(&mut self.ordinary_state, OrdinaryState::Transitioning) {
                OrdinaryState::Access { job, access, meta }
                    if access.phase() == LogicalPacketAccessPhase::AwaitingCad =>
                {
                    (job, access, meta)
                }
                state => {
                    self.ordinary_state = state;
                    return RadioOperationStep::NotReady;
                }
            };
        self.ordinary_state = OrdinaryState::CadInFlight { job, access, meta };
        let result = self.radio.cad().await;
        let (job, mut access, meta) =
            match mem::replace(&mut self.ordinary_state, OrdinaryState::Transitioning) {
                OrdinaryState::CadInFlight { job, access, meta } => (job, access, meta),
                _ => return self.disable_internal_radio(),
            };
        match result {
            Ok(observation) => {
                let activity_detected = observation.activity_detected();
                let observed_at_us = observation.observed_at_us();
                match access.observe_cad(observed_at_us, activity_detected, self.rng.next_u64()) {
                    Ok(action) => {
                        let _ = self.apply_ordinary_access_action(job, access, meta, action);
                        if matches!(self.ordinary_state, OrdinaryState::Return { .. }) {
                            RadioOperationStep::Terminal(
                                self.last_report.expect("terminal CAD report recorded"),
                            )
                        } else if let OrdinaryState::Disabled { fault, .. } = &self.ordinary_state {
                            RadioOperationStep::Disabled(*fault)
                        } else {
                            RadioOperationStep::CadObserved {
                                family: DispatchFamily::Ordinary,
                                activity_detected,
                                observed_at_us,
                            }
                        }
                    }
                    Err(_) => {
                        let _ = self.fail_ordinary_job_invariant(job, meta);
                        RadioOperationStep::Terminal(self.last_report.expect("report recorded"))
                    }
                }
            }
            Err(fault) => {
                let report = DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::CadFault {
                        phase: fault.phase(),
                        class: fault.class(),
                    },
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: job
                        .return_unpermitted()
                        .complete(self.config.completion_codes.cad_fault),
                    after: OrdinaryAfterReturn::Disable {
                        fault: DispatcherFault::RadioUnavailable,
                        retained: OrdinaryRetainedControl::None,
                    },
                };
                RadioOperationStep::Terminal(report)
            }
        }
    }

    async fn perform_data_tx(&mut self, now_us: u64) -> RadioOperationStep {
        let (mut owner, mut access, meta) =
            match mem::replace(&mut self.data_state, DataState::Transitioning) {
                DataState::TransmitReady {
                    owner,
                    access,
                    meta,
                } => (owner, access, meta),
                state => {
                    self.data_state = state;
                    return RadioOperationStep::NotReady;
                }
            };
        let reservation = owner.reservation();
        let active_resource =
            TxPermitResourceId::new(self.radio.configuration_fingerprint().as_bytes());
        if reservation.resource() != meta.permit_resource
            || active_resource != meta.permit_resource
            || self.radio.airtime_profile() != meta.airtime_profile
        {
            self.radio.shutdown();
            let report = DispatchReport::new(
                DispatchFamily::Data,
                DispatchOutcome::RadioConfigurationChangedAfterPermit,
                meta.frame_count,
                None,
            );
            self.record_report(report);
            self.data_state = DataState::Return {
                completion: owner.complete(self.config.completion_codes.post_grant_rejection),
                after: DataAfterReturn::Disable {
                    fault: DispatcherFault::RadioUnavailable,
                    retained: DataRetainedControl::None,
                },
            };
            return RadioOperationStep::Terminal(report);
        }
        let reserved_us = reservation.reserved_units();
        match access.permit_granted(now_us, reserved_us) {
            Ok(LogicalPacketAccessAction::TransmitLogicalPacket { .. }) => {}
            Ok(LogicalPacketAccessAction::Reject(rejection)) => {
                let report = DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::PostGrantAccessRejected(rejection),
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.data_state = DataState::Return {
                    completion: owner.complete(self.config.completion_codes.post_grant_rejection),
                    after: DataAfterReturn::Resume,
                };
                return RadioOperationStep::Terminal(report);
            }
            Ok(_) | Err(_) => {
                let report = DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::FrameInvariantRecovery,
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.data_state = DataState::Return {
                    completion: owner
                        .recovery_fault(self.config.completion_codes.frame_invariant_recovery),
                    after: DataAfterReturn::Disable {
                        fault: DispatcherFault::InternalInvariant,
                        retained: DataRetainedControl::None,
                    },
                };
                return RadioOperationStep::Terminal(report);
            }
        }
        let frame_result = owner.frame(monotonic_millis_ceiling(now_us));
        let (frames, authorized_frame) = match frame_result {
            Ok(frame) => {
                let authorized_frame = frame.observation();
                let frames = frame_rns_packet(
                    frame.bytes(),
                    self.take_sequence(),
                    &mut self.first_frame,
                    &mut self.second_frame,
                );
                (frames, authorized_frame)
            }
            Err(error) => {
                return self.finish_data_frame_error(owner, meta, error);
            }
        };
        let frames = match frames {
            Ok(frames) => frames,
            Err(_) => {
                let report = DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::FrameInvariantRecovery,
                    meta.frame_count,
                    None,
                )
                .with_authorized_frame(Some(authorized_frame));
                self.record_report(report);
                let completion =
                    owner.recovery_fault(self.config.completion_codes.frame_invariant_recovery);
                self.gate_data_completion_on_authorized_frame(
                    completion,
                    DataAfterReturn::Disable {
                        fault: DispatcherFault::InternalInvariant,
                        retained: DataRetainedControl::None,
                    },
                    authorized_frame,
                );
                return RadioOperationStep::Terminal(report);
            }
        };
        if !self.radio.is_active() {
            let report = DispatchReport::new(
                DispatchFamily::Data,
                DispatchOutcome::RadioInactive,
                meta.frame_count,
                Some(PacketTxProgress::none()),
            )
            .with_authorized_frame(Some(authorized_frame));
            self.record_report(report);
            let completion = owner.complete(self.config.completion_codes.tx_fault);
            self.gate_data_completion_on_authorized_frame(
                completion,
                DataAfterReturn::Disable {
                    fault: DispatcherFault::RadioUnavailable,
                    retained: DataRetainedControl::None,
                },
                authorized_frame,
            );
            return RadioOperationStep::Terminal(report);
        }
        self.data_state = DataState::TxInFlight {
            owner,
            meta,
            authorized_frame,
        };
        let result = self.radio.transmit(frames).await;
        let (owner, meta, authorized_frame) =
            match mem::replace(&mut self.data_state, DataState::Transitioning) {
                DataState::TxInFlight {
                    owner,
                    meta,
                    authorized_frame,
                } => (owner, meta, authorized_frame),
                _ => return self.disable_internal_radio(),
            };
        match result {
            Ok(observation) => {
                let progress = observation.progress();
                if progress.completed_frame_count() != meta.frame_count {
                    self.radio.shutdown();
                    let report = DispatchReport::new(
                        DispatchFamily::Data,
                        DispatchOutcome::FrameInvariantRecovery,
                        meta.frame_count,
                        Some(progress),
                    )
                    .with_authorized_frame(Some(authorized_frame));
                    self.record_report(report);
                    let completion =
                        owner.recovery_fault(self.config.completion_codes.frame_invariant_recovery);
                    self.gate_data_completion_on_authorized_frame(
                        completion,
                        DataAfterReturn::Disable {
                            fault: DispatcherFault::InternalInvariant,
                            retained: DataRetainedControl::None,
                        },
                        authorized_frame,
                    );
                    return RadioOperationStep::Terminal(report);
                }
                let report = DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::Transmitted,
                    meta.frame_count,
                    Some(progress),
                )
                .with_authorized_frame(Some(authorized_frame));
                self.record_report(report);
                let completion = owner.complete(self.config.completion_codes.transmitted);
                self.gate_data_completion_on_authorized_frame(
                    completion,
                    DataAfterReturn::Resume,
                    authorized_frame,
                );
                RadioOperationStep::Terminal(report)
            }
            Err(fault) => {
                let progress = fault.progress();
                if progress.completed_frame_count() > meta.frame_count {
                    self.radio.shutdown();
                    let report = DispatchReport::new(
                        DispatchFamily::Data,
                        DispatchOutcome::FrameInvariantRecovery,
                        meta.frame_count,
                        Some(progress),
                    )
                    .with_authorized_frame(Some(authorized_frame));
                    self.record_report(report);
                    let completion =
                        owner.recovery_fault(self.config.completion_codes.frame_invariant_recovery);
                    self.gate_data_completion_on_authorized_frame(
                        completion,
                        DataAfterReturn::Disable {
                            fault: DispatcherFault::InternalInvariant,
                            retained: DataRetainedControl::None,
                        },
                        authorized_frame,
                    );
                    return RadioOperationStep::Terminal(report);
                }
                let report = DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::TxFault {
                        phase: fault.fault().phase(),
                        class: fault.fault().class(),
                    },
                    meta.frame_count,
                    Some(progress),
                )
                .with_authorized_frame(Some(authorized_frame));
                self.record_report(report);
                let completion = owner.complete(self.config.completion_codes.tx_fault);
                self.gate_data_completion_on_authorized_frame(
                    completion,
                    DataAfterReturn::Disable {
                        fault: DispatcherFault::RadioUnavailable,
                        retained: DataRetainedControl::None,
                    },
                    authorized_frame,
                );
                RadioOperationStep::Terminal(report)
            }
        }
    }

    async fn perform_ordinary_tx(&mut self, now_us: u64) -> RadioOperationStep {
        let (mut owner, mut access, meta) =
            match mem::replace(&mut self.ordinary_state, OrdinaryState::Transitioning) {
                OrdinaryState::TransmitReady {
                    owner,
                    access,
                    meta,
                } => (owner, access, meta),
                state => {
                    self.ordinary_state = state;
                    return RadioOperationStep::NotReady;
                }
            };
        let reservation = owner.reservation();
        let active_resource =
            TxPermitResourceId::new(self.radio.configuration_fingerprint().as_bytes());
        if reservation.resource() != meta.permit_resource
            || active_resource != meta.permit_resource
            || self.radio.airtime_profile() != meta.airtime_profile
        {
            self.radio.shutdown();
            let report = DispatchReport::new(
                DispatchFamily::Ordinary,
                DispatchOutcome::RadioConfigurationChangedAfterPermit,
                meta.frame_count,
                None,
            );
            self.record_report(report);
            self.ordinary_state = OrdinaryState::Return {
                completion: owner.cancel(self.config.completion_codes.post_grant_rejection),
                after: OrdinaryAfterReturn::Disable {
                    fault: DispatcherFault::RadioUnavailable,
                    retained: OrdinaryRetainedControl::None,
                },
            };
            return RadioOperationStep::Terminal(report);
        }
        let reserved_us = reservation.reserved_units();
        match access.permit_granted(now_us, reserved_us) {
            Ok(LogicalPacketAccessAction::TransmitLogicalPacket { .. }) => {}
            Ok(LogicalPacketAccessAction::Reject(rejection)) => {
                let report = DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::PostGrantAccessRejected(rejection),
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: owner.cancel(self.config.completion_codes.post_grant_rejection),
                    after: OrdinaryAfterReturn::Resume,
                };
                return RadioOperationStep::Terminal(report);
            }
            Ok(_) | Err(_) => {
                let report = DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::FrameInvariantRecovery,
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: owner
                        .recovery_fault(self.config.completion_codes.frame_invariant_recovery),
                    after: OrdinaryAfterReturn::Disable {
                        fault: DispatcherFault::InternalInvariant,
                        retained: OrdinaryRetainedControl::None,
                    },
                };
                return RadioOperationStep::Terminal(report);
            }
        }
        let frame_result = owner.frame(monotonic_millis_ceiling(now_us));
        let frames = match frame_result {
            Ok(frame) => frame_rns_packet(
                frame.bytes(),
                self.take_sequence(),
                &mut self.first_frame,
                &mut self.second_frame,
            ),
            Err(error) => return self.finish_ordinary_frame_error(owner, meta, error),
        };
        let frames = match frames {
            Ok(frames) => frames,
            Err(_) => {
                let report = DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::FrameInvariantRecovery,
                    meta.frame_count,
                    None,
                );
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: owner
                        .recovery_fault(self.config.completion_codes.frame_invariant_recovery),
                    after: OrdinaryAfterReturn::Disable {
                        fault: DispatcherFault::InternalInvariant,
                        retained: OrdinaryRetainedControl::None,
                    },
                };
                return RadioOperationStep::Terminal(report);
            }
        };
        if !self.radio.is_active() {
            let report = DispatchReport::new(
                DispatchFamily::Ordinary,
                DispatchOutcome::RadioInactive,
                meta.frame_count,
                Some(PacketTxProgress::none()),
            );
            self.record_report(report);
            self.ordinary_state = OrdinaryState::Return {
                completion: owner.cancel(self.config.completion_codes.tx_fault),
                after: OrdinaryAfterReturn::Disable {
                    fault: DispatcherFault::RadioUnavailable,
                    retained: OrdinaryRetainedControl::None,
                },
            };
            return RadioOperationStep::Terminal(report);
        }
        self.ordinary_state = OrdinaryState::TxInFlight { owner, meta };
        let result = self.radio.transmit(frames).await;
        let (owner, meta) =
            match mem::replace(&mut self.ordinary_state, OrdinaryState::Transitioning) {
                OrdinaryState::TxInFlight { owner, meta } => (owner, meta),
                _ => return self.disable_internal_radio(),
            };
        match result {
            Ok(observation) => {
                let progress = observation.progress();
                if progress.completed_frame_count() != meta.frame_count {
                    self.radio.shutdown();
                    let report = DispatchReport::new(
                        DispatchFamily::Ordinary,
                        DispatchOutcome::FrameInvariantRecovery,
                        meta.frame_count,
                        Some(progress),
                    );
                    self.record_report(report);
                    self.ordinary_state = OrdinaryState::Return {
                        completion: owner
                            .recovery_fault(self.config.completion_codes.frame_invariant_recovery),
                        after: OrdinaryAfterReturn::Disable {
                            fault: DispatcherFault::InternalInvariant,
                            retained: OrdinaryRetainedControl::None,
                        },
                    };
                    return RadioOperationStep::Terminal(report);
                }
                let report = DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::Transmitted,
                    meta.frame_count,
                    Some(progress),
                );
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: owner.complete(self.config.completion_codes.transmitted),
                    after: OrdinaryAfterReturn::Resume,
                };
                RadioOperationStep::Terminal(report)
            }
            Err(fault) => {
                let progress = fault.progress();
                if progress.completed_frame_count() > meta.frame_count {
                    self.radio.shutdown();
                    let report = DispatchReport::new(
                        DispatchFamily::Ordinary,
                        DispatchOutcome::FrameInvariantRecovery,
                        meta.frame_count,
                        Some(progress),
                    );
                    self.record_report(report);
                    self.ordinary_state = OrdinaryState::Return {
                        completion: owner
                            .recovery_fault(self.config.completion_codes.frame_invariant_recovery),
                        after: OrdinaryAfterReturn::Disable {
                            fault: DispatcherFault::InternalInvariant,
                            retained: OrdinaryRetainedControl::None,
                        },
                    };
                    return RadioOperationStep::Terminal(report);
                }
                let report = DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::TxFault {
                        phase: fault.fault().phase(),
                        class: fault.fault().class(),
                    },
                    meta.frame_count,
                    Some(progress),
                );
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: owner.cancel(self.config.completion_codes.tx_fault),
                    after: OrdinaryAfterReturn::Disable {
                        fault: DispatcherFault::RadioUnavailable,
                        retained: OrdinaryRetainedControl::None,
                    },
                };
                RadioOperationStep::Terminal(report)
            }
        }
    }

    /// Reconcile a short RX, CAD, or TX future dropped while pending.
    ///
    /// The sole-radio trait guarantees that dropping the inner radio future
    /// contains RF and leaves the radio inactive. RX cancellation records a
    /// retained receive result and disables without consuming any queued TX
    /// owner. A pre-grant CAD cancellation is definitely unpermitted. A TX
    /// cancellation remains authorized; its packet report retains the exact
    /// frame observation but deliberately carries no progress claim because no
    /// adapter result was returned.
    pub fn recover_cancelled_radio_operation(&mut self) -> RadioTxDispatcherStep {
        if self.receive_state == ReceiveState::InFlight {
            self.radio.shutdown();
            self.receive_state = ReceiveState::Disabled(DispatcherFault::ReceiveCancelled);
            self.last_receive_step = Some(RadioReceiveStep::Disabled(
                DispatcherFault::ReceiveCancelled,
            ));
            return RadioTxDispatcherStep::Disabled(DispatcherFault::ReceiveCancelled);
        }
        match self.active {
            ActiveFamily::Data => {
                let state = mem::replace(&mut self.data_state, DataState::Transitioning);
                let (completion, frame_count, authorized_frame) = match state {
                    DataState::CadInFlight { job, meta, .. } => (
                        job.return_unpermitted()
                            .complete(self.config.completion_codes.cancelled_radio_operation),
                        meta.frame_count,
                        None,
                    ),
                    DataState::TxInFlight {
                        owner,
                        meta,
                        authorized_frame,
                    } => (
                        owner.complete(self.config.completion_codes.cancelled_radio_operation),
                        meta.frame_count,
                        Some(authorized_frame),
                    ),
                    state => {
                        self.data_state = state;
                        return RadioTxDispatcherStep::Advanced;
                    }
                };
                self.radio.shutdown();
                let report = DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::CancelledRadioOperation,
                    frame_count,
                    None,
                )
                .with_authorized_frame(authorized_frame);
                self.record_report(report);
                let after = DataAfterReturn::Disable {
                    fault: DispatcherFault::RadioUnavailable,
                    retained: DataRetainedControl::None,
                };
                if let Some(expected) = authorized_frame {
                    self.gate_data_completion_on_authorized_frame(completion, after, expected);
                } else {
                    self.data_state = DataState::Return { completion, after };
                }
                RadioTxDispatcherStep::Advanced
            }
            ActiveFamily::Ordinary => {
                let state = mem::replace(&mut self.ordinary_state, OrdinaryState::Transitioning);
                let (completion, frame_count) = match state {
                    OrdinaryState::CadInFlight { job, meta, .. } => (
                        job.return_unpermitted()
                            .complete(self.config.completion_codes.cancelled_radio_operation),
                        meta.frame_count,
                    ),
                    OrdinaryState::TxInFlight { owner, meta } => (
                        owner.cancel(self.config.completion_codes.cancelled_radio_operation),
                        meta.frame_count,
                    ),
                    state => {
                        self.ordinary_state = state;
                        return RadioTxDispatcherStep::Advanced;
                    }
                };
                self.radio.shutdown();
                let report = DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::CancelledRadioOperation,
                    frame_count,
                    None,
                );
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion,
                    after: OrdinaryAfterReturn::Disable {
                        fault: DispatcherFault::RadioUnavailable,
                        retained: OrdinaryRetainedControl::None,
                    },
                };
                RadioTxDispatcherStep::Advanced
            }
            ActiveFamily::None => RadioTxDispatcherStep::Advanced,
        }
    }

    /// Poll advisory capacity for an exact permit request retained by this dispatcher.
    ///
    /// Only [`RadioTxDispatcherPhase::PermitSend`] polls a request endpoint.
    /// Both pending and ready results carry the retained family and recovery
    /// threshold so a supervisor can race the registered capacity wake against
    /// the deadline. This operation never moves the request or reserves
    /// capacity; after readiness, call [`Self::step`] with a fresh timestamp to
    /// retry the exact request or apply deadline recovery.
    pub fn poll_permit_request_capacity(
        &mut self,
        context: &mut Context<'_>,
    ) -> PermitRequestCapacity {
        let (family, grace_deadline_us) = match (&self.data_state, &self.ordinary_state) {
            (DataState::PermitSend { meta, .. }, _) => {
                (DispatchFamily::Data, meta.grace_deadline_us)
            }
            (_, OrdinaryState::PermitSend { meta, .. }) => {
                (DispatchFamily::Ordinary, meta.grace_deadline_us)
            }
            _ => return PermitRequestCapacity::NotRetained(self.phase()),
        };
        let readiness = match family {
            DispatchFamily::Data => self.data_permits.requests().poll_ready_to_send(context),
            DispatchFamily::Ordinary => {
                self.ordinary_permits.requests().poll_ready_to_send(context)
            }
        };
        match readiness {
            Poll::Pending => PermitRequestCapacity::Pending {
                family,
                grace_deadline_us,
            },
            Poll::Ready(()) => PermitRequestCapacity::Ready {
                family,
                grace_deadline_us,
            },
        }
    }

    /// Wait cancellation-safely for a retained permit request to become retryable.
    ///
    /// This future owns no request. Cancelling it leaves the exact pending owner
    /// and request in dispatcher state and leaves channel capacity unreserved.
    /// Supervisors that must race capacity against `grace_deadline_us` should
    /// poll [`Self::poll_permit_request_capacity`] alongside their timer.
    pub async fn wait_for_permit_request_capacity(&mut self) -> PermitRequestCapacity {
        poll_fn(|context| match self.poll_permit_request_capacity(context) {
            PermitRequestCapacity::Pending { .. } => Poll::Pending,
            result => Poll::Ready(result),
        })
        .await
    }

    /// Poll advisory capacity for the exact retained authorized-frame request.
    ///
    /// A pending poll only registers the waker. It never moves the observation
    /// or owning completion and never reserves channel capacity. After
    /// readiness, call [`Self::step`] to retry the unchanged request.
    pub fn poll_authorized_frame_request_capacity(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<AuthorizedFrameRequestCapacity> {
        if !matches!(&self.data_state, DataState::AuthorizedFrameRequest { .. }) {
            return Poll::Ready(AuthorizedFrameRequestCapacity::NotRetained(self.phase()));
        }
        self.authorized_frames
            .requests()
            .poll_ready_to_send(context)
            .map(|()| AuthorizedFrameRequestCapacity::Ready)
    }

    /// Wait cancellation-safely for the retained frame request to become retryable.
    ///
    /// Cancelling this future leaves the exact request, completion, and router
    /// ticket in dispatcher state.
    pub async fn wait_for_authorized_frame_request_capacity(
        &mut self,
    ) -> AuthorizedFrameRequestCapacity {
        poll_fn(|context| self.poll_authorized_frame_request_capacity(context)).await
    }

    /// Poll for and correlate the exact durable authorized-frame acknowledgement.
    ///
    /// `Pending` leaves the owning completion and expected observation in
    /// dispatcher state. A matching value advances to completion return. A
    /// mismatch disables and retains the completion, router ticket, expected
    /// observation, and actual acknowledgement. No timeout releases this gate.
    pub fn poll_authorized_frame_acknowledgement(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<AuthorizedFrameAcknowledgementProgress> {
        if !matches!(
            &self.data_state,
            DataState::AuthorizedFrameAcknowledgementWait { .. }
        ) {
            return Poll::Ready(AuthorizedFrameAcknowledgementProgress::NotRetained(
                self.phase(),
            ));
        }
        let actual = match self
            .authorized_frames
            .acknowledgements()
            .poll_receive(context)
        {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(actual) => actual,
        };
        let state = mem::replace(&mut self.data_state, DataState::Transitioning);
        match state {
            DataState::AuthorizedFrameAcknowledgementWait {
                completion,
                after,
                expected,
            } => Poll::Ready(
                self.apply_authorized_frame_acknowledgement(completion, after, expected, actual),
            ),
            state => {
                self.data_state = state;
                Poll::Ready(AuthorizedFrameAcknowledgementProgress::NotRetained(
                    self.phase(),
                ))
            }
        }
    }

    /// Wait cancellation-safely for one exact authorized-frame acknowledgement.
    pub async fn wait_for_authorized_frame_acknowledgement(
        &mut self,
    ) -> AuthorizedFrameAcknowledgementProgress {
        poll_fn(|context| self.poll_authorized_frame_acknowledgement(context)).await
    }

    /// Poll advisory capacity for an exact interface completion retained by this dispatcher.
    ///
    /// When the dispatcher is in [`RadioTxDispatcherPhase::CompletionReturn`],
    /// a pending result only registers the current waker. It never moves the
    /// stored completion or reserves capacity. After readiness, call
    /// [`Self::step`] to retry the exact completion. Every other phase returns
    /// [`InterfaceCompletionCapacity::NotRetained`] immediately.
    pub fn poll_interface_completion_capacity(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<InterfaceCompletionCapacity> {
        let family = match (&self.data_state, &self.ordinary_state) {
            (DataState::InterfaceReturn { .. }, _) => DispatchFamily::Data,
            (_, OrdinaryState::InterfaceReturn { .. }) => DispatchFamily::Ordinary,
            _ => {
                return Poll::Ready(InterfaceCompletionCapacity::NotRetained(self.phase()));
            }
        };
        self.tx_handoff
            .poll_completion_capacity(context)
            .map(|()| InterfaceCompletionCapacity::Ready(family))
    }

    /// Wait cancellation-safely for a retained interface completion to become retryable.
    ///
    /// This future owns no completion. Cancelling it leaves the exact retained
    /// completion in dispatcher state and leaves queue capacity unreserved.
    pub async fn wait_for_interface_completion_capacity(&mut self) -> InterfaceCompletionCapacity {
        poll_fn(|context| self.poll_interface_completion_capacity(context)).await
    }

    /// Await the next transport-neutral interface job while completely idle.
    ///
    /// Cancelling while pending leaves the job queued. Once ready, the receive
    /// poll stores both the exact node owner and its completion ticket before
    /// returning `Ready`. Call [`Self::step`] with a fresh timestamp to begin
    /// mandatory initial backoff.
    pub async fn wait_for_job(&mut self) -> RadioTxDispatcherStep {
        match self.receive_state {
            ReceiveState::InFlight => return RadioTxDispatcherStep::NeedReceiveRecovery,
            ReceiveState::Disabled(fault) => return RadioTxDispatcherStep::Disabled(fault),
            ReceiveState::Idle => {}
        }
        if self.active != ActiveFamily::None {
            return RadioTxDispatcherStep::Advanced;
        }
        if !matches!(self.active_ticket, ActiveCompletionTicket::None) {
            return self.disable_stray_ticket();
        }
        let interface_job = self.tx_handoff.receive_job().await;
        self.accept_interface_job(interface_job, None)
    }

    /// Await the active family's permit reply and store it before returning.
    pub async fn wait_for_permit_reply(&mut self) -> RadioTxDispatcherStep {
        match self.receive_state {
            ReceiveState::InFlight => return RadioTxDispatcherStep::NeedReceiveRecovery,
            ReceiveState::Disabled(fault) => return RadioTxDispatcherStep::Disabled(fault),
            ReceiveState::Idle => {}
        }
        match self.active {
            ActiveFamily::Data => {
                let state = mem::replace(&mut self.data_state, DataState::Transitioning);
                match state {
                    DataState::PermitWait {
                        pending,
                        access,
                        meta,
                    } => {
                        self.data_state = DataState::PermitWait {
                            pending,
                            access,
                            meta,
                        };
                        let reply =
                            poll_fn(|context| self.data_permits.replies().poll_receive(context))
                                .await;
                        let (pending, access, meta) =
                            match mem::replace(&mut self.data_state, DataState::Transitioning) {
                                DataState::PermitWait {
                                    pending,
                                    access,
                                    meta,
                                } => (pending, access, meta),
                                _ => return self.disable_internal(),
                            };
                        self.data_state = DataState::PermitReply {
                            pending,
                            reply,
                            access,
                            meta,
                        };
                        RadioTxDispatcherStep::Advanced
                    }
                    state => {
                        self.data_state = state;
                        RadioTxDispatcherStep::Advanced
                    }
                }
            }
            ActiveFamily::Ordinary => {
                let state = mem::replace(&mut self.ordinary_state, OrdinaryState::Transitioning);
                match state {
                    OrdinaryState::PermitWait {
                        pending,
                        access,
                        meta,
                    } => {
                        self.ordinary_state = OrdinaryState::PermitWait {
                            pending,
                            access,
                            meta,
                        };
                        let reply = poll_fn(|context| {
                            self.ordinary_permits.replies().poll_receive(context)
                        })
                        .await;
                        let (pending, access, meta) = match mem::replace(
                            &mut self.ordinary_state,
                            OrdinaryState::Transitioning,
                        ) {
                            OrdinaryState::PermitWait {
                                pending,
                                access,
                                meta,
                            } => (pending, access, meta),
                            _ => return self.disable_internal(),
                        };
                        self.ordinary_state = OrdinaryState::PermitReply {
                            pending,
                            reply,
                            access,
                            meta,
                        };
                        RadioTxDispatcherStep::Advanced
                    }
                    state => {
                        self.ordinary_state = state;
                        RadioTxDispatcherStep::Advanced
                    }
                }
            }
            ActiveFamily::None => RadioTxDispatcherStep::NeedJob,
        }
    }

    fn apply_data_access_action(
        &mut self,
        job: RoutedTxJob<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
        action: LogicalPacketAccessAction,
    ) -> RadioTxDispatcherStep {
        match action {
            LogicalPacketAccessAction::WaitUntil { retry_at_us, .. } => {
                self.data_state = DataState::Access { job, access, meta };
                RadioTxDispatcherStep::WaitUntil {
                    family: DispatchFamily::Data,
                    retry_at_us,
                }
            }
            LogicalPacketAccessAction::PerformCad { .. } => {
                self.data_state = DataState::Access { job, access, meta };
                RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
            }
            LogicalPacketAccessAction::ChannelClear {
                frame_count,
                required_aggregate_rf_time_on_air_us,
                ..
            } => {
                if frame_count != meta.frame_count
                    || required_aggregate_rf_time_on_air_us != meta.aggregate_airtime_us
                {
                    return self.fail_data_job_invariant(job, meta);
                }
                let active_resource =
                    TxPermitResourceId::new(self.radio.configuration_fingerprint().as_bytes());
                if active_resource != meta.permit_resource
                    || self.radio.airtime_profile() != meta.airtime_profile
                {
                    self.radio.shutdown();
                    return self.finish_data_unpermitted_job(
                        job,
                        DispatchReport::new(
                            DispatchFamily::Data,
                            DispatchOutcome::RadioConfigurationChangedBeforePermit,
                            meta.frame_count,
                            None,
                        ),
                        true,
                    );
                }
                let requirements = match TxPermitRequirements::try_new(
                    meta.permit_resource,
                    meta.aggregate_airtime_us,
                ) {
                    Ok(requirements) => requirements,
                    Err(_) => return self.fail_data_job_invariant(job, meta),
                };
                let (pending, request) = job.begin_permit(requirements);
                self.data_state = DataState::PermitSend {
                    pending,
                    request,
                    access,
                    meta,
                };
                RadioTxDispatcherStep::Advanced
            }
            LogicalPacketAccessAction::Reject(rejection) => self.finish_data_unpermitted_job(
                job,
                DispatchReport::new(
                    DispatchFamily::Data,
                    DispatchOutcome::AccessRejected(rejection),
                    meta.frame_count,
                    None,
                ),
                false,
            ),
            LogicalPacketAccessAction::TransmitLogicalPacket { .. } => {
                self.fail_data_job_invariant(job, meta)
            }
        }
    }

    fn apply_ordinary_access_action(
        &mut self,
        job: OrdinaryTxJob<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
        action: LogicalPacketAccessAction,
    ) -> RadioTxDispatcherStep {
        match action {
            LogicalPacketAccessAction::WaitUntil { retry_at_us, .. } => {
                self.ordinary_state = OrdinaryState::Access { job, access, meta };
                RadioTxDispatcherStep::WaitUntil {
                    family: DispatchFamily::Ordinary,
                    retry_at_us,
                }
            }
            LogicalPacketAccessAction::PerformCad { .. } => {
                self.ordinary_state = OrdinaryState::Access { job, access, meta };
                RadioTxDispatcherStep::NeedCad(DispatchFamily::Ordinary)
            }
            LogicalPacketAccessAction::ChannelClear {
                frame_count,
                required_aggregate_rf_time_on_air_us,
                ..
            } => {
                if frame_count != meta.frame_count
                    || required_aggregate_rf_time_on_air_us != meta.aggregate_airtime_us
                {
                    return self.fail_ordinary_job_invariant(job, meta);
                }
                let active_resource =
                    TxPermitResourceId::new(self.radio.configuration_fingerprint().as_bytes());
                if active_resource != meta.permit_resource
                    || self.radio.airtime_profile() != meta.airtime_profile
                {
                    self.radio.shutdown();
                    return self.finish_ordinary_unpermitted_job(
                        job,
                        DispatchReport::new(
                            DispatchFamily::Ordinary,
                            DispatchOutcome::RadioConfigurationChangedBeforePermit,
                            meta.frame_count,
                            None,
                        ),
                        true,
                    );
                }
                let requirements = match TxPermitRequirements::try_new(
                    meta.permit_resource,
                    meta.aggregate_airtime_us,
                ) {
                    Ok(requirements) => requirements,
                    Err(_) => return self.fail_ordinary_job_invariant(job, meta),
                };
                let (pending, request) = job.begin_permit(requirements);
                self.ordinary_state = OrdinaryState::PermitSend {
                    pending,
                    request,
                    access,
                    meta,
                };
                RadioTxDispatcherStep::Advanced
            }
            LogicalPacketAccessAction::Reject(rejection) => self.finish_ordinary_unpermitted_job(
                job,
                DispatchReport::new(
                    DispatchFamily::Ordinary,
                    DispatchOutcome::AccessRejected(rejection),
                    meta.frame_count,
                    None,
                ),
                false,
            ),
            LogicalPacketAccessAction::TransmitLogicalPacket { .. } => {
                self.fail_ordinary_job_invariant(job, meta)
            }
        }
    }

    fn finish_data_unpermitted_job(
        &mut self,
        job: RoutedTxJob<'static>,
        report: DispatchReport,
        disable_after_return: bool,
    ) -> RadioTxDispatcherStep {
        self.record_report(report);
        self.data_state = DataState::Return {
            completion: job
                .return_unpermitted()
                .complete(self.config.completion_codes.unpermitted),
            after: if disable_after_return {
                DataAfterReturn::Disable {
                    fault: DispatcherFault::RadioUnavailable,
                    retained: DataRetainedControl::None,
                }
            } else {
                DataAfterReturn::Resume
            },
        };
        RadioTxDispatcherStep::Advanced
    }

    fn finish_ordinary_unpermitted_job(
        &mut self,
        job: OrdinaryTxJob<'static>,
        report: DispatchReport,
        disable_after_return: bool,
    ) -> RadioTxDispatcherStep {
        self.record_report(report);
        self.ordinary_state = OrdinaryState::Return {
            completion: job
                .return_unpermitted()
                .complete(self.config.completion_codes.unpermitted),
            after: if disable_after_return {
                OrdinaryAfterReturn::Disable {
                    fault: DispatcherFault::RadioUnavailable,
                    retained: OrdinaryRetainedControl::None,
                }
            } else {
                OrdinaryAfterReturn::Resume
            },
        };
        RadioTxDispatcherStep::Advanced
    }

    fn fail_data_job_invariant(
        &mut self,
        job: RoutedTxJob<'static>,
        meta: DispatchMeta,
    ) -> RadioTxDispatcherStep {
        let report = DispatchReport::new(
            DispatchFamily::Data,
            DispatchOutcome::FrameInvariantRecovery,
            meta.frame_count,
            None,
        );
        self.record_report(report);
        self.data_state = DataState::Return {
            completion: job.recovery_fault(self.config.completion_codes.frame_invariant_recovery),
            after: DataAfterReturn::Disable {
                fault: DispatcherFault::InternalInvariant,
                retained: DataRetainedControl::None,
            },
        };
        RadioTxDispatcherStep::Advanced
    }

    fn fail_ordinary_job_invariant(
        &mut self,
        job: OrdinaryTxJob<'static>,
        meta: DispatchMeta,
    ) -> RadioTxDispatcherStep {
        let report = DispatchReport::new(
            DispatchFamily::Ordinary,
            DispatchOutcome::FrameInvariantRecovery,
            meta.frame_count,
            None,
        );
        self.record_report(report);
        self.ordinary_state = OrdinaryState::Return {
            completion: job.recovery_fault(self.config.completion_codes.frame_invariant_recovery),
            after: OrdinaryAfterReturn::Disable {
                fault: DispatcherFault::InternalInvariant,
                retained: OrdinaryRetainedControl::None,
            },
        };
        RadioTxDispatcherStep::Advanced
    }

    fn fail_data_mismatch(
        &mut self,
        mismatch: PermitReplyMismatch<'static>,
        meta: DispatchMeta,
    ) -> RadioTxDispatcherStep {
        let (pending, reply) = mismatch.into_parts();
        let report = DispatchReport::new(
            DispatchFamily::Data,
            DispatchOutcome::ControlPlaneRecovery,
            meta.frame_count,
            None,
        );
        self.record_report(report);
        self.data_state = DataState::Return {
            completion: pending.recovery_fault(self.config.completion_codes.control_plane_recovery),
            after: DataAfterReturn::Disable {
                fault: DispatcherFault::PermitReplyMismatch(DispatchFamily::Data),
                retained: DataRetainedControl::Reply(reply),
            },
        };
        RadioTxDispatcherStep::Advanced
    }

    fn fail_ordinary_mismatch(
        &mut self,
        mismatch: OrdinaryPermitReplyMismatch<'static>,
        meta: DispatchMeta,
    ) -> RadioTxDispatcherStep {
        let (pending, reply) = mismatch.into_parts();
        let report = DispatchReport::new(
            DispatchFamily::Ordinary,
            DispatchOutcome::ControlPlaneRecovery,
            meta.frame_count,
            None,
        );
        self.record_report(report);
        self.ordinary_state = OrdinaryState::Return {
            completion: pending.recovery_fault(self.config.completion_codes.control_plane_recovery),
            after: OrdinaryAfterReturn::Disable {
                fault: DispatcherFault::PermitReplyMismatch(DispatchFamily::Ordinary),
                retained: OrdinaryRetainedControl::Reply(reply),
            },
        };
        RadioTxDispatcherStep::Advanced
    }

    fn finish_data_frame_error(
        &mut self,
        owner: AuthorizedTx<'static>,
        meta: DispatchMeta,
        error: TxFrameError,
    ) -> RadioOperationStep {
        let (outcome, completion, after) = match error {
            TxFrameError::DeadlineExpired { .. } => (
                DispatchOutcome::AuthorizationExpired,
                owner.complete(self.config.completion_codes.expired_authorization),
                DataAfterReturn::Resume,
            ),
            TxFrameError::AlreadyTaken | TxFrameError::Invariant => (
                DispatchOutcome::FrameInvariantRecovery,
                owner.recovery_fault(self.config.completion_codes.frame_invariant_recovery),
                DataAfterReturn::Disable {
                    fault: DispatcherFault::InternalInvariant,
                    retained: DataRetainedControl::None,
                },
            ),
        };
        let report = DispatchReport::new(DispatchFamily::Data, outcome, meta.frame_count, None);
        self.record_report(report);
        self.data_state = DataState::Return { completion, after };
        RadioOperationStep::Terminal(report)
    }

    fn finish_ordinary_frame_error(
        &mut self,
        owner: OrdinaryAuthorizedTx<'static>,
        meta: DispatchMeta,
        error: reticulum_node_core::OrdinaryFrameError,
    ) -> RadioOperationStep {
        let (outcome, completion, after) = match error {
            reticulum_node_core::OrdinaryFrameError::DeadlineExpired { .. } => (
                DispatchOutcome::AuthorizationExpired,
                owner.cancel(self.config.completion_codes.expired_authorization),
                OrdinaryAfterReturn::Resume,
            ),
            reticulum_node_core::OrdinaryFrameError::AlreadyTaken
            | reticulum_node_core::OrdinaryFrameError::Invariant => (
                DispatchOutcome::FrameInvariantRecovery,
                owner.recovery_fault(self.config.completion_codes.frame_invariant_recovery),
                OrdinaryAfterReturn::Disable {
                    fault: DispatcherFault::InternalInvariant,
                    retained: OrdinaryRetainedControl::None,
                },
            ),
        };
        let report = DispatchReport::new(DispatchFamily::Ordinary, outcome, meta.frame_count, None);
        self.record_report(report);
        self.ordinary_state = OrdinaryState::Return { completion, after };
        RadioOperationStep::Terminal(report)
    }

    fn take_sequence(&mut self) -> u8 {
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1) & 0x0f;
        sequence
    }

    fn record_report(&mut self, report: DispatchReport) {
        self.last_report = Some(report);
    }

    fn disable_stray_ticket(&mut self) -> RadioTxDispatcherStep {
        let ticket = mem::replace(&mut self.active_ticket, ActiveCompletionTicket::None);
        self.completion_ticket_residue = match ticket {
            ActiveCompletionTicket::None => None,
            ActiveCompletionTicket::Data(ticket) => {
                Some(CompletionTicketResidue::DataTicket(ticket))
            }
            ActiveCompletionTicket::Ordinary(ticket) => {
                Some(CompletionTicketResidue::OrdinaryTicket(ticket))
            }
        };
        let fault = DispatcherFault::InternalInvariant;
        self.data_state = DataState::Disabled {
            fault,
            retained: DataRetainedControl::None,
        };
        self.active = ActiveFamily::Data;
        RadioTxDispatcherStep::Disabled(fault)
    }

    fn disable_internal(&mut self) -> RadioTxDispatcherStep {
        let fault = DispatcherFault::InternalInvariant;
        match self.active {
            ActiveFamily::Data => {
                self.data_state = DataState::Disabled {
                    fault,
                    retained: DataRetainedControl::None,
                };
            }
            ActiveFamily::Ordinary => {
                self.ordinary_state = OrdinaryState::Disabled {
                    fault,
                    retained: OrdinaryRetainedControl::None,
                };
            }
            ActiveFamily::None => {
                self.data_state = DataState::Disabled {
                    fault,
                    retained: DataRetainedControl::None,
                };
                self.active = ActiveFamily::Data;
            }
        }
        RadioTxDispatcherStep::Disabled(fault)
    }

    fn disable_internal_radio(&mut self) -> RadioOperationStep {
        let _ = self.disable_internal();
        RadioOperationStep::Disabled(DispatcherFault::InternalInvariant)
    }
}

fn data_phase(state: &DataState) -> RadioTxDispatcherPhase {
    match state {
        DataState::Idle => RadioTxDispatcherPhase::Idle,
        DataState::Job(_) => RadioTxDispatcherPhase::JobReady(DispatchFamily::Data),
        DataState::Access { access, .. } => match access.phase() {
            LogicalPacketAccessPhase::BackingOff => {
                RadioTxDispatcherPhase::BackingOff(DispatchFamily::Data)
            }
            LogicalPacketAccessPhase::AwaitingCad => {
                RadioTxDispatcherPhase::CadReady(DispatchFamily::Data)
            }
            _ => RadioTxDispatcherPhase::Disabled,
        },
        DataState::CadInFlight { .. } => RadioTxDispatcherPhase::CadInFlight(DispatchFamily::Data),
        DataState::PermitSend { .. } => RadioTxDispatcherPhase::PermitSend(DispatchFamily::Data),
        DataState::PermitWait { .. } | DataState::PermitReply { .. } => {
            RadioTxDispatcherPhase::PermitWait(DispatchFamily::Data)
        }
        DataState::TransmitReady { .. } => {
            RadioTxDispatcherPhase::TransmitReady(DispatchFamily::Data)
        }
        DataState::TxInFlight { .. } => {
            RadioTxDispatcherPhase::TransmitInFlight(DispatchFamily::Data)
        }
        DataState::Expired { .. } | DataState::Unpermitted { .. } => {
            RadioTxDispatcherPhase::PermitWait(DispatchFamily::Data)
        }
        DataState::AuthorizedFrameRequest { .. } => RadioTxDispatcherPhase::AuthorizedFrameRequest,
        DataState::AuthorizedFrameAcknowledgementWait { .. } => {
            RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
        }
        DataState::Return { .. } | DataState::InterfaceReturn { .. } => {
            RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Data)
        }
        DataState::AuthorizedFrameAcknowledgementFault { .. }
        | DataState::Disabled { .. }
        | DataState::Transitioning => RadioTxDispatcherPhase::Disabled,
    }
}

const fn authorized_frame_progress_step(
    progress: AuthorizedFrameAcknowledgementProgress,
) -> RadioTxDispatcherStep {
    match progress {
        AuthorizedFrameAcknowledgementProgress::Matched
        | AuthorizedFrameAcknowledgementProgress::NotRetained(_) => RadioTxDispatcherStep::Advanced,
        AuthorizedFrameAcknowledgementProgress::Disabled(fault) => {
            RadioTxDispatcherStep::Disabled(fault)
        }
    }
}

fn ordinary_phase(state: &OrdinaryState) -> RadioTxDispatcherPhase {
    match state {
        OrdinaryState::Idle => RadioTxDispatcherPhase::Idle,
        OrdinaryState::Job(_) => RadioTxDispatcherPhase::JobReady(DispatchFamily::Ordinary),
        OrdinaryState::Access { access, .. } => match access.phase() {
            LogicalPacketAccessPhase::BackingOff => {
                RadioTxDispatcherPhase::BackingOff(DispatchFamily::Ordinary)
            }
            LogicalPacketAccessPhase::AwaitingCad => {
                RadioTxDispatcherPhase::CadReady(DispatchFamily::Ordinary)
            }
            _ => RadioTxDispatcherPhase::Disabled,
        },
        OrdinaryState::CadInFlight { .. } => {
            RadioTxDispatcherPhase::CadInFlight(DispatchFamily::Ordinary)
        }
        OrdinaryState::PermitSend { .. } => {
            RadioTxDispatcherPhase::PermitSend(DispatchFamily::Ordinary)
        }
        OrdinaryState::PermitWait { .. } | OrdinaryState::PermitReply { .. } => {
            RadioTxDispatcherPhase::PermitWait(DispatchFamily::Ordinary)
        }
        OrdinaryState::TransmitReady { .. } => {
            RadioTxDispatcherPhase::TransmitReady(DispatchFamily::Ordinary)
        }
        OrdinaryState::TxInFlight { .. } => {
            RadioTxDispatcherPhase::TransmitInFlight(DispatchFamily::Ordinary)
        }
        OrdinaryState::Expired { .. } | OrdinaryState::Unpermitted { .. } => {
            RadioTxDispatcherPhase::PermitWait(DispatchFamily::Ordinary)
        }
        OrdinaryState::Return { .. } | OrdinaryState::InterfaceReturn { .. } => {
            RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Ordinary)
        }
        OrdinaryState::Disabled { .. } | OrdinaryState::Transitioning => {
            RadioTxDispatcherPhase::Disabled
        }
    }
}

const fn monotonic_millis_ceiling(now_us: u64) -> MonotonicMillis {
    let whole_ms = now_us / 1_000;
    let partial_ms = if now_us.is_multiple_of(1_000) { 0 } else { 1 };
    MonotonicMillis::new(whole_ms + partial_ms)
}

/// Current Embassy monotonic time in whole microseconds.
pub fn embassy_now_us() -> u64 {
    Instant::now().as_micros()
}

/// Wait until an absolute Embassy monotonic microsecond timestamp.
pub async fn embassy_wait_until_us(deadline_us: u64) {
    Timer::at(Instant::from_micros(deadline_us)).await;
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        future::Future,
        ops::{Deref, DerefMut},
        pin::{Pin, pin},
        task::{Context, Poll, Waker},
    };
    use std::{
        boxed::Box,
        collections::VecDeque,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::Wake,
        vec,
        vec::Vec,
    };

    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use reticulum_interface_router::{
        InterfaceConfigId, InterfaceCost, InterfaceFabric, InterfaceProperties, LogicalMtu,
        OutboundCompletion, OutboundRouter,
    };
    use reticulum_node_core::{
        AnnounceEmissionTime, DestinationHash, InterfaceSet, MAX_ANNOUNCE_APP_DATA,
        MAX_DATA_PAYLOAD, MonotonicSeconds, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId,
        OrdinaryActionAdmissionRequest, OrdinaryActionOwner, OrdinaryCompletionDisposition,
        OrdinaryPacketBuffer, OrdinaryQuarantineReason, PacketInterfaceId, PrepareDataRequest,
        TxAuthorizationCandidate, TxAuthorizationPolicy, TxCompletionDisposition, TxLeaseDeadline,
        TxPacketBuffer, TxPermitReservation, TxPolicyDecision, TxPolicyDenial,
        TxRecoveryPriorPhase, TxRecoveryReason,
    };
    use reticulum_radio_interface::{
        CadObservation, FrameSignal, LabRxProfile, LabRxProfileConfig, PacketTxFault,
        PacketTxObservation, RadioConfigurationFingerprint, ReceiveFrequencyRange, RnodeTxFrames,
        SoleRadioFaultSummary,
    };
    use reticulum_tx_handoff::{
        AuthorizedFrameHandoff, AuthorizedFrameNodeHandoff, DataNodePermitHandoff,
        DataPermitHandoff, DispatcherPermitHandoff, OrdinaryDispatcherPermitHandoff,
        OrdinaryNodePermitHandoff, OrdinaryPermitHandoff,
    };
    use static_cell::{ConstStaticCell, StaticCell};

    use super::*;

    type TestNode<const N: usize> = NodeCore<4, 4, 8, 2, N>;
    type PortableTestDispatcher = SoleRadioTxDispatcher<MockRadio, CounterRng, NoopRawMutex, 1>;
    type TestRouter = OutboundRouter<NoopRawMutex, 1, 1>;

    struct TestDispatcher {
        inner: PortableTestDispatcher,
        authorized_frames: AuthorizedFrameNodeHandoff<NoopRawMutex>,
    }

    impl Deref for TestDispatcher {
        type Target = PortableTestDispatcher;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl DerefMut for TestDispatcher {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }

    impl TestDispatcher {
        fn step(&mut self, now_us: u64) -> RadioTxDispatcherStep {
            let result = self.inner.step(now_us);
            if self.inner.phase() != RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait {
                return result;
            }
            let Some(observation) = self.authorized_frames.requests().try_receive() else {
                return result;
            };
            if self
                .authorized_frames
                .acknowledgements()
                .try_send(observation)
                .is_err()
            {
                return result;
            }
            let acknowledged = self.inner.step(now_us);
            assert_eq!(acknowledged, RadioTxDispatcherStep::Advanced);
            self.inner.step(now_us)
        }
    }

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct CounterRng(u64);

    impl RngCore for CounterRng {
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }

        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            self.0
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            for chunk in destination.chunks_mut(8) {
                let bytes = self.next_u64().to_le_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for CounterRng {}

    #[derive(Clone, Copy)]
    enum CadScript {
        Observation { busy: bool, at_us: u64 },
        Fault,
        Pending,
    }

    #[derive(Clone, Copy)]
    enum TxScript {
        SuccessOne(u64),
        SuccessTwo(u64, u64),
        Fault(PacketTxProgress),
        Pending,
    }

    #[derive(Clone, Copy)]
    enum RxScript {
        Frame {
            len: usize,
            signal: FrameSignal,
            at_us: u64,
        },
        NoPreambleTimeout,
        Fault,
        Pending,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct CapturedPacket {
        first: Vec<u8>,
        second: Option<Vec<u8>>,
    }

    struct MockRadio {
        active: bool,
        fingerprint: RadioConfigurationFingerprint,
        profile: LabRxProfile,
        cad: VecDeque<CadScript>,
        tx: VecDeque<TxScript>,
        rx: VecDeque<RxScript>,
        captured: Vec<CapturedPacket>,
        maximum_receive_operation_us: core::num::NonZeroU64,
    }

    impl MockRadio {
        fn new(cad: Vec<CadScript>, tx: Vec<TxScript>) -> Self {
            Self {
                active: true,
                fingerprint: RadioConfigurationFingerprint::new([0x91; 16]),
                profile: profile(),
                cad: cad.into(),
                tx: tx.into(),
                rx: VecDeque::new(),
                captured: Vec::new(),
                maximum_receive_operation_us: core::num::NonZeroU64::new(1_500_000).unwrap(),
            }
        }

        fn with_rx(mut self, rx: Vec<RxScript>) -> Self {
            self.rx = rx.into();
            self
        }
    }

    struct MockCadFuture<'a> {
        radio: &'a mut MockRadio,
        complete: bool,
    }

    impl Future for MockCadFuture<'_> {
        type Output = Result<CadObservation, SoleRadioFaultSummary>;

        fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
            let Some(script) = self.radio.cad.front().copied() else {
                panic!("mock CAD script exhausted")
            };
            if matches!(script, CadScript::Pending) {
                return Poll::Pending;
            }
            let _ = self.radio.cad.pop_front();
            self.complete = true;
            match script {
                CadScript::Observation { busy, at_us } => {
                    Poll::Ready(Ok(CadObservation::new(busy, at_us)))
                }
                CadScript::Fault => {
                    self.radio.active = false;
                    Poll::Ready(Err(SoleRadioFaultSummary::new(
                        SoleRadioFaultPhase::ChannelActivityDetection,
                        SoleRadioFaultClass::Operation,
                    )))
                }
                CadScript::Pending => unreachable!(),
            }
        }
    }

    impl Drop for MockCadFuture<'_> {
        fn drop(&mut self) {
            if !self.complete {
                self.radio.active = false;
            }
        }
    }

    struct MockTxFuture<'a> {
        radio: &'a mut MockRadio,
        complete: bool,
    }

    impl Future for MockTxFuture<'_> {
        type Output = Result<PacketTxObservation, PacketTxFault<SoleRadioFaultSummary>>;

        fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
            let Some(script) = self.radio.tx.front().copied() else {
                panic!("mock TX script exhausted")
            };
            if matches!(script, TxScript::Pending) {
                return Poll::Pending;
            }
            let _ = self.radio.tx.pop_front();
            self.complete = true;
            match script {
                TxScript::SuccessOne(at_us) => {
                    Poll::Ready(Ok(PacketTxObservation::one_frame(at_us)))
                }
                TxScript::SuccessTwo(first_us, second_us) => {
                    Poll::Ready(Ok(PacketTxObservation::two_frames(first_us, second_us)))
                }
                TxScript::Fault(progress) => {
                    self.radio.active = false;
                    Poll::Ready(Err(PacketTxFault::new(
                        SoleRadioFaultSummary::new(
                            SoleRadioFaultPhase::FrameTransmission,
                            SoleRadioFaultClass::Operation,
                        ),
                        progress,
                    )))
                }
                TxScript::Pending => unreachable!(),
            }
        }
    }

    impl Drop for MockTxFuture<'_> {
        fn drop(&mut self) {
            if !self.complete {
                self.radio.active = false;
            }
        }
    }

    struct MockRxFuture<'a> {
        radio: &'a mut MockRadio,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
        complete: bool,
    }

    impl Future for MockRxFuture<'_> {
        type Output = Result<BoundedRxOutcome, SoleRadioFaultSummary>;

        fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
            let Some(script) = self.radio.rx.front().copied() else {
                panic!("mock RX script exhausted")
            };
            if matches!(script, RxScript::Pending) {
                return Poll::Pending;
            }
            let _ = self.radio.rx.pop_front();
            self.complete = true;
            match script {
                RxScript::Frame { len, signal, at_us } => {
                    for (index, byte) in self.buffer.iter_mut().take(len).enumerate() {
                        *byte = index as u8;
                    }
                    self.radio.active = true;
                    Poll::Ready(Ok(BoundedRxOutcome::Frame(BoundedRxObservation::new(
                        len, signal, at_us,
                    ))))
                }
                RxScript::NoPreambleTimeout => {
                    self.radio.active = true;
                    Poll::Ready(Ok(BoundedRxOutcome::NoPreambleTimeout))
                }
                RxScript::Fault => Poll::Ready(Err(SoleRadioFaultSummary::new(
                    SoleRadioFaultPhase::Receive,
                    SoleRadioFaultClass::Operation,
                ))),
                RxScript::Pending => unreachable!(),
            }
        }
    }

    impl Drop for MockRxFuture<'_> {
        fn drop(&mut self) {
            if !self.complete {
                self.radio.active = false;
            }
        }
    }

    impl SoleRnodeRadio for MockRadio {
        type Fault = SoleRadioFaultSummary;

        fn configuration_fingerprint(&self) -> RadioConfigurationFingerprint {
            self.fingerprint
        }

        fn airtime_profile(&self) -> LabRxProfile {
            self.profile
        }

        fn maximum_receive_operation_us(&self) -> core::num::NonZeroU64 {
            self.maximum_receive_operation_us
        }

        fn receive_bounded<'a>(
            &'a mut self,
            buffer: &'a mut [u8; SX1262_FRAME_MTU],
        ) -> impl Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a {
            self.active = false;
            MockRxFuture {
                radio: self,
                buffer,
                complete: false,
            }
        }

        fn cad(&mut self) -> impl Future<Output = Result<CadObservation, Self::Fault>> + '_ {
            MockCadFuture {
                radio: self,
                complete: false,
            }
        }

        fn transmit<'a>(
            &'a mut self,
            frames: RnodeTxFrames<'a>,
        ) -> impl Future<Output = Result<PacketTxObservation, PacketTxFault<Self::Fault>>> + 'a
        {
            self.captured.push(CapturedPacket {
                first: frames.first().to_vec(),
                second: frames.second().map(<[u8]>::to_vec),
            });
            MockTxFuture {
                radio: self,
                complete: false,
            }
        }

        fn shutdown(&mut self) {
            self.active = false;
        }

        fn is_active(&self) -> bool {
            self.active
        }
    }

    const TEST_PERMIT_RESOURCE_BYTES: [u8; 16] = [0x91; 16];
    const TEST_INTERFACE: PacketInterfaceId = PacketInterfaceId::new(1);
    const TEST_INTERFACE_CONFIG: InterfaceConfigId = InterfaceConfigId::new(1);

    fn test_permit_resource() -> TxPermitResourceId {
        TxPermitResourceId::new(TEST_PERMIT_RESOURCE_BYTES)
    }

    fn exact_requirements(packet_len: u16) -> TxPermitRequirements {
        let required_units = profile()
            .rnode_packet_airtime(packet_len.into())
            .unwrap()
            .aggregate_time_on_air_us();
        TxPermitRequirements::try_new(test_permit_resource(), required_units).unwrap()
    }

    struct ExactPolicy;

    impl TxAuthorizationPolicy for ExactPolicy {
        fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
            ExactLoRaAirtimePolicy::new(
                TEST_INTERFACE,
                profile(),
                RadioConfigurationFingerprint::new(TEST_PERMIT_RESOURCE_BYTES),
            )
            .authorize(candidate)
        }
    }

    struct DenyPolicy;

    impl TxAuthorizationPolicy for DenyPolicy {
        fn authorize(&mut self, _candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
            TxPolicyDecision::Deny(TxPolicyDenial::PolicyDenied)
        }
    }

    fn profile() -> LabRxProfile {
        let range = ReceiveFrequencyRange::try_new(863_000_000, 928_000_000).unwrap();
        LabRxProfile::validate(
            LabRxProfileConfig {
                frequency_hz: Some(915_000_000),
                spreading_factor: 7,
                bandwidth_hz: 500_000,
                coding_rate_denominator: 5,
                preamble_symbols: 18,
                explicit_header: true,
                crc: true,
                iq_inverted: false,
            },
            range,
        )
        .unwrap()
    }

    fn changed_profile() -> LabRxProfile {
        let range = ReceiveFrequencyRange::try_new(863_000_000, 928_000_000).unwrap();
        LabRxProfile::validate(
            LabRxProfileConfig {
                frequency_hz: Some(916_000_000),
                spreading_factor: 8,
                bandwidth_hz: 500_000,
                coding_rate_denominator: 5,
                preamble_symbols: 18,
                explicit_header: true,
                crc: true,
                iq_inverted: false,
            },
            range,
        )
        .unwrap()
    }

    fn policy_candidate(
        packet_len: u16,
        interface: PacketInterfaceId,
        resource: TxPermitResourceId,
        required_units: u64,
    ) -> TxAuthorizationCandidate {
        TxAuthorizationCandidate {
            interface,
            packet_len,
            requirements: TxPermitRequirements::try_new(resource, required_units).unwrap(),
            now: MonotonicMillis::new(1_000),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(2_000)),
            may_have_transmitted: false,
        }
    }

    #[test]
    fn exact_policy_recomputes_airtime_and_rejects_wrong_resource_interface_or_units() {
        let packet_len = 300;
        let expected_units = profile()
            .rnode_packet_airtime(packet_len.into())
            .unwrap()
            .aggregate_time_on_air_us();
        let mut policy = ExactPolicy;

        assert_eq!(
            policy.authorize(policy_candidate(
                packet_len,
                TEST_INTERFACE,
                test_permit_resource(),
                expected_units,
            )),
            TxPolicyDecision::Authorize(
                TxPermitReservation::try_new(test_permit_resource(), expected_units).unwrap()
            )
        );
        assert_eq!(
            policy.authorize(policy_candidate(
                packet_len,
                TEST_INTERFACE,
                test_permit_resource(),
                expected_units - 1,
            )),
            TxPolicyDecision::Deny(TxPolicyDenial::PolicyDenied)
        );
        assert_eq!(
            policy.authorize(policy_candidate(
                packet_len,
                TEST_INTERFACE,
                TxPermitResourceId::new([0x92; 16]),
                expected_units,
            )),
            TxPolicyDecision::Deny(TxPolicyDenial::ResourceUnavailable)
        );
        assert_eq!(
            policy.authorize(policy_candidate(
                packet_len,
                PacketInterfaceId::new(2),
                test_permit_resource(),
                expected_units,
            )),
            TxPolicyDecision::Deny(TxPolicyDenial::PolicyDenied)
        );
    }

    fn config(maximum_cad_attempts: u8) -> RadioTxDispatcherConfig {
        RadioTxDispatcherConfig::new(
            TEST_INTERFACE_CONFIG,
            LogicalPacketAccessConfig::try_new(
                maximum_cad_attempts,
                10,
                20,
                100_000,
                100,
                100,
                100,
            )
            .unwrap(),
            500_000,
            3,
            RadioTxCompletionCodes::new(
                TxCompletionCode::new(1),
                TxCompletionCode::new(2),
                TxCompletionCode::new(3),
                TxCompletionCode::new(4),
                TxCompletionCode::new(5),
                TxCompletionCode::new(6),
                TxCompletionCode::new(7),
                TxCompletionCode::new(8),
                TxCompletionCode::new(9),
            ),
        )
    }

    fn test_dispatcher(
        radio: MockRadio,
        rng: CounterRng,
        data_permits: DispatcherPermitHandoff<NoopRawMutex>,
        ordinary_permits: OrdinaryDispatcherPermitHandoff<NoopRawMutex>,
        dispatcher_config: RadioTxDispatcherConfig,
    ) -> (TestDispatcher, TestRouter) {
        assert_eq!(
            dispatcher_config.expected_interface_config,
            TEST_INTERFACE_CONFIG
        );
        test_dispatcher_with_registry_config(
            radio,
            rng,
            data_permits,
            ordinary_permits,
            dispatcher_config,
            TEST_INTERFACE_CONFIG,
        )
    }

    fn test_dispatcher_with_registry_config(
        radio: MockRadio,
        rng: CounterRng,
        data_permits: DispatcherPermitHandoff<NoopRawMutex>,
        ordinary_permits: OrdinaryDispatcherPermitHandoff<NoopRawMutex>,
        dispatcher_config: RadioTxDispatcherConfig,
        registry_config: InterfaceConfigId,
    ) -> (TestDispatcher, TestRouter) {
        test_dispatcher_with_registry_config_and_frame_blocker(
            radio,
            rng,
            data_permits,
            ordinary_permits,
            dispatcher_config,
            registry_config,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn test_dispatcher_with_registry_config_and_frame_blocker(
        radio: MockRadio,
        rng: CounterRng,
        data_permits: DispatcherPermitHandoff<NoopRawMutex>,
        ordinary_permits: OrdinaryDispatcherPermitHandoff<NoopRawMutex>,
        dispatcher_config: RadioTxDispatcherConfig,
        registry_config: InterfaceConfigId,
        frame_request_blocker: Option<AuthorizedFrameObservation>,
    ) -> (TestDispatcher, TestRouter) {
        let authorized_frame_handoff =
            Box::leak(Box::new(AuthorizedFrameHandoff::<NoopRawMutex>::new()));
        let (authorized_frame_node, mut authorized_frame_dispatcher) =
            authorized_frame_handoff.split();
        if let Some(blocker) = frame_request_blocker {
            assert!(
                authorized_frame_dispatcher
                    .requests()
                    .try_send(blocker)
                    .is_ok()
            );
        }
        let fabric = Box::leak(Box::new(InterfaceFabric::<NoopRawMutex, 1, 1>::new()));
        let (mut router, [actor_handoff]) = fabric.split();
        let (tx_handoff, _unused_ingress_handoff, _unused_lifecycle_handoff) =
            actor_handoff.into_parts();
        router
            .register(
                tx_handoff.queue_id(),
                TEST_INTERFACE,
                InterfaceProperties::new(
                    LogicalMtu::try_new(u16::MAX).unwrap(),
                    registry_config,
                    None,
                    InterfaceCost::new(0),
                ),
                true,
            )
            .unwrap();
        (
            TestDispatcher {
                inner: SoleRadioTxDispatcher::new(
                    radio,
                    rng,
                    tx_handoff,
                    data_permits,
                    ordinary_permits,
                    authorized_frame_dispatcher,
                    dispatcher_config,
                ),
                authorized_frames: authorized_frame_node,
            },
            router,
        )
    }

    fn route_data(router: &mut TestRouter, job: RoutedTxJob<'static>) {
        router
            .try_route_data(job)
            .unwrap_or_else(|failure| panic!("DATA route: {:?}", failure));
    }

    fn route_ordinary(router: &mut TestRouter, job: OrdinaryTxJob<'static>) {
        router
            .try_route_ordinary(job)
            .unwrap_or_else(|failure| panic!("ordinary route: {:?}", failure));
    }

    fn take_data_completion(router: &mut TestRouter) -> TxCompletion<'static> {
        match router
            .try_receive_completion()
            .unwrap_or_else(|failure| panic!("DATA completion route: {:?}", failure))
            .expect("DATA completion")
        {
            OutboundCompletion::Data(completion) => completion,
            OutboundCompletion::Ordinary(_) => panic!("ordinary completion crossed into DATA"),
        }
    }

    fn take_ordinary_completion(router: &mut TestRouter) -> OrdinaryTxCompletion<'static> {
        match router
            .try_receive_completion()
            .unwrap_or_else(|failure| panic!("ordinary completion route: {:?}", failure))
            .expect("ordinary completion")
        {
            OutboundCompletion::Ordinary(completion) => completion,
            OutboundCompletion::Data(_) => panic!("DATA completion crossed into ordinary"),
        }
    }

    fn dispatcher_with_rx(rx: Vec<RxScript>) -> TestDispatcher {
        let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
        let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
        let (_, data_dispatch) = data_handoff.split();
        let (_, ordinary_dispatch) = ordinary_handoff.split();
        test_dispatcher(
            MockRadio::new(vec![], vec![]).with_rx(rx),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        )
        .0
    }

    fn identity(tag: u8) -> NodeIdentity {
        NodeIdentity::from_private_key(&[tag; 64]).unwrap()
    }

    fn node<const N: usize>(tag: u8, aspect: &'static str) -> TestNode<N> {
        TestNode::new(
            identity(tag),
            "reticulum",
            &[aspect],
            NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
            NodeConfig::endpoint(),
        )
        .unwrap()
    }

    fn prepare_data<const N: usize>(
        sender: &mut TestNode<N>,
        buffer: &'static mut TxPacketBuffer,
        destination: DestinationHash,
        plaintext: &[u8],
        deadline_ms: u64,
    ) -> RoutedTxJob<'static> {
        sender
            .prepare_data_into_slot(
                buffer,
                PrepareDataRequest {
                    destination,
                    plaintext,
                    rns_now: MonotonicSeconds::new(1),
                    owner_now: MonotonicMillis::new(1_000),
                    deadline: TxLeaseDeadline::new(MonotonicMillis::new(deadline_ms)),
                    enabled_interfaces: InterfaceSet::from_bits(1 << 1),
                },
                &mut CounterRng::default(),
            )
            .unwrap_or_else(|failure| panic!("DATA preparation: {:?}", failure.reason()))
    }

    fn prepare_ordinary(
        node: &mut TestNode<1>,
        owner: &mut OrdinaryActionOwner<1>,
        refs: &'static mut [&'static mut OrdinaryPacketBuffer; 1],
        app_data_len: usize,
        deadline_ms: u64,
    ) -> OrdinaryTxJob<'static> {
        owner.register_packet_buffer(refs[0]).unwrap();
        let app_data = [0x42; MAX_ANNOUNCE_APP_DATA];
        node.queue_announce(
            Some(&app_data[..app_data_len]),
            AnnounceEmissionTime::new(1).unwrap(),
            &mut CounterRng::default(),
        )
        .unwrap();
        let actions = node.flush_announces(MonotonicSeconds::new(1), &mut CounterRng::default());
        let mut batch = owner
            .admit(
                actions,
                refs,
                OrdinaryActionAdmissionRequest {
                    owner_now: MonotonicMillis::new(1_000),
                    deadline: TxLeaseDeadline::new(MonotonicMillis::new(deadline_ms)),
                    enabled_interfaces: InterfaceSet::from_bits(1 << 1),
                },
            )
            .unwrap();
        batch.take_next_packet().expect("announce packet missing")
    }

    fn start_and_clear_data(dispatcher: &mut TestDispatcher, now_us: u64) {
        assert_eq!(dispatcher.step(now_us), RadioTxDispatcherStep::Advanced);
        assert!(matches!(
            dispatcher.step(now_us + 100),
            RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
        ));
        assert!(matches!(
            block_on(dispatcher.perform_radio_operation(now_us + 101)),
            RadioOperationStep::CadObserved {
                family: DispatchFamily::Data,
                activity_detected: false,
                ..
            }
        ));
    }

    fn grant_data<const N: usize>(
        dispatcher: &mut TestDispatcher,
        node_ports: &mut DataNodePermitHandoff<NoopRawMutex>,
        owner: &mut TestNode<N>,
        policy: &mut impl TxAuthorizationPolicy,
        authorization_ms: u64,
        dispatcher_now_us: u64,
    ) {
        assert_eq!(
            dispatcher.step(dispatcher_now_us),
            RadioTxDispatcherStep::Advanced
        );
        let request = node_ports.requests().try_receive().expect("DATA request");
        let reply = owner
            .authorize_tx(request, MonotonicMillis::new(authorization_ms), policy)
            .unwrap_or_else(|failure| panic!("DATA authorize: {:?}", failure.reason()));
        node_ports
            .replies()
            .try_send(reply)
            .unwrap_or_else(|_| panic!("DATA reply full"));
        assert_eq!(
            dispatcher.step(dispatcher_now_us + 1),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(dispatcher_now_us + 2),
            RadioTxDispatcherStep::Advanced
        );
    }

    fn terminal_data_gate_fixture(
        tag: u8,
        tx: TxScript,
        frame_request_blocker: Option<AuthorizedFrameObservation>,
    ) -> (TestNode<1>, TestDispatcher, TestRouter, DispatchReport) {
        let mut owner = node::<1>(tag, "authorized-frame-gate-owner");
        let receiver = node::<0>(tag.wrapping_add(1), "authorized-frame-gate-receiver");
        owner
            .register_peer(
                &identity(tag.wrapping_add(1)),
                "reticulum",
                &["authorized-frame-gate-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"durable authorized frame gate",
            100_000,
        );
        let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
        let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
        let (mut data_node, data_dispatch) = data_handoff.split();
        let (_, ordinary_dispatch) = ordinary_handoff.split();
        let (mut dispatcher, mut router) = test_dispatcher_with_registry_config_and_frame_blocker(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![tx],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
            TEST_INTERFACE_CONFIG,
            frame_request_blocker,
        );
        route_data(&mut router, job);
        assert_eq!(
            block_on(dispatcher.wait_for_job()),
            RadioTxDispatcherStep::Advanced
        );
        start_and_clear_data(&mut dispatcher, 1_000_000);
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut ExactPolicy,
            1_001,
            1_000_200,
        );
        let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("DATA TX did not terminate: {other:?}"),
        };
        assert!(report.authorized_frame().is_some());
        (owner, dispatcher, router, report)
    }

    fn assert_no_completion(router: &mut TestRouter) {
        assert!(
            router
                .try_receive_completion()
                .unwrap_or_else(|failure| panic!("completion route failed: {failure:?}"))
                .is_none()
        );
    }

    #[test]
    fn authorized_frame_completion_cannot_overtake_matching_acknowledgement() {
        let (mut owner, mut dispatcher, mut router, report) =
            terminal_data_gate_fixture(82, TxScript::SuccessOne(1_000_500), None);
        let expected = report.authorized_frame().unwrap();
        assert_eq!(
            dispatcher.inner.phase(),
            RadioTxDispatcherPhase::AuthorizedFrameRequest
        );
        assert_eq!(dispatcher.inner.take_last_report(), Some(report));
        assert_eq!(dispatcher.inner.last_report(), None);
        assert_no_completion(&mut router);

        assert_eq!(
            dispatcher.inner.step(1_000_600),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.inner.phase(),
            RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
        );
        assert_eq!(
            dispatcher.inner.step(1_000_601),
            RadioTxDispatcherStep::NeedAuthorizedFrameAcknowledgement
        );
        assert_no_completion(&mut router);
        {
            let mut wait = pin!(dispatcher.inner.wait_for_authorized_frame_acknowledgement());
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
        }
        assert_eq!(
            dispatcher.inner.phase(),
            RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
        );
        assert_eq!(
            dispatcher.authorized_frames.requests().try_receive(),
            Some(expected)
        );
        dispatcher
            .authorized_frames
            .acknowledgements()
            .try_send(expected)
            .unwrap_or_else(|_| panic!("matching acknowledgement channel full"));
        assert_eq!(
            block_on(dispatcher.inner.wait_for_authorized_frame_acknowledgement()),
            AuthorizedFrameAcknowledgementProgress::Matched
        );
        assert_eq!(
            dispatcher.inner.phase(),
            RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Data)
        );
        assert_no_completion(&mut router);
        assert_eq!(
            dispatcher.inner.step(1_000_602),
            RadioTxDispatcherStep::Advanced
        );
        let completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));
        assert_eq!(dispatcher.inner.phase(), RadioTxDispatcherPhase::Idle);
    }

    #[test]
    fn frame_ack_wait_retains_queued_ordinary_job_and_excludes_completion_and_rx() {
        let (mut data_owner, mut dispatcher, mut router, report) =
            terminal_data_gate_fixture(93, TxScript::SuccessOne(1_000_500), None);
        let expected_frame = report.authorized_frame().unwrap();
        assert_eq!(
            dispatcher.inner.step(1_000_600),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.authorized_frames.requests().try_receive(),
            Some(expected_frame)
        );
        assert_eq!(
            dispatcher.inner.phase(),
            RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
        );

        let mut ordinary_node = node::<1>(94, "frame-ack-wait-ordinary");
        let mut ordinary_owner = ordinary_node.take_ordinary_action_owner::<1>().unwrap();
        let ordinary_buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
        let ordinary_refs = Box::leak(Box::new([ordinary_buffer]));
        let ordinary_job = prepare_ordinary(
            &mut ordinary_node,
            &mut ordinary_owner,
            ordinary_refs,
            4,
            2_000,
        );
        let expected_ordinary = ordinary_job.prepared();
        route_ordinary(&mut router, ordinary_job);

        dispatcher
            .inner
            .radio
            .rx
            .push_back(RxScript::NoPreambleTimeout);
        let mut rx_buffer = [0; SX1262_FRAME_MTU];
        assert!(matches!(
            dispatcher.inner.start_receive(&mut rx_buffer),
            Err(RadioReceiveStep::TxPriority(
                RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
            ))
        ));
        assert_eq!(dispatcher.inner.radio.rx.len(), 1);

        // Advancing beyond the queued ordinary owner's deadline cannot make
        // either family escape the exact DATA durability gate.
        for now_us in [2_000_000, 3_000_000, 60_000_000] {
            assert_eq!(
                dispatcher.inner.step(now_us),
                RadioTxDispatcherStep::NeedAuthorizedFrameAcknowledgement
            );
            assert_eq!(
                dispatcher.inner.phase(),
                RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
            );
            assert_no_completion(&mut router);
            assert_eq!(dispatcher.inner.radio.captured.len(), 1);
            assert_eq!(dispatcher.inner.radio.rx.len(), 1);
        }

        dispatcher
            .authorized_frames
            .acknowledgements()
            .try_send(expected_frame)
            .unwrap_or_else(|_| panic!("matching acknowledgement channel full"));
        assert_eq!(
            dispatcher.inner.step(60_000_001),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.inner.phase(),
            RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Data)
        );
        assert_eq!(
            dispatcher.inner.step(60_000_002),
            RadioTxDispatcherStep::Advanced
        );
        let data_completion = take_data_completion(&mut router);
        assert!(matches!(
            data_owner.complete_tx(data_completion, MonotonicMillis::new(60_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));
        assert_eq!(dispatcher.inner.phase(), RadioTxDispatcherPhase::Idle);

        // The exact ordinary owner remained queued. Once DATA releases, its
        // now-expired metadata returns unchanged without another RF operation.
        assert_eq!(
            dispatcher.inner.step(60_000_003),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.inner.phase(),
            RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Ordinary)
        );
        assert_eq!(
            dispatcher.inner.step(60_000_004),
            RadioTxDispatcherStep::Advanced
        );
        let ordinary_completion = take_ordinary_completion(&mut router);
        assert_eq!(ordinary_completion.prepared(), expected_ordinary);
        assert_eq!(dispatcher.inner.radio.captured.len(), 1);
        assert_eq!(dispatcher.inner.radio.rx.len(), 1);
        assert_eq!(dispatcher.inner.phase(), RadioTxDispatcherPhase::Idle);
    }

    #[test]
    fn authorized_frame_request_backpressure_retains_exact_observation_and_completion() {
        let (_blocker_owner, _blocker_dispatcher, _blocker_router, blocker_report) =
            terminal_data_gate_fixture(84, TxScript::SuccessOne(1_000_500), None);
        let blocker = blocker_report.authorized_frame().unwrap();
        let (mut owner, mut dispatcher, mut router, report) =
            terminal_data_gate_fixture(86, TxScript::SuccessOne(1_000_500), Some(blocker));
        let expected = report.authorized_frame().unwrap();
        assert_ne!(expected, blocker);

        assert_eq!(
            dispatcher.inner.step(1_000_600),
            RadioTxDispatcherStep::Backpressured(
                RadioTxDispatcherChannel::AuthorizedFrameObservationRequest
            )
        );
        assert_eq!(
            dispatcher.inner.phase(),
            RadioTxDispatcherPhase::AuthorizedFrameRequest
        );
        assert_no_completion(&mut router);
        let wake_counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(wake_counter.clone());
        let mut context = Context::from_waker(&waker);
        assert!(matches!(
            dispatcher
                .inner
                .poll_authorized_frame_request_capacity(&mut context),
            Poll::Pending
        ));
        {
            let mut wait = pin!(
                dispatcher
                    .inner
                    .wait_for_authorized_frame_request_capacity()
            );
            assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
        }
        assert_eq!(
            dispatcher.authorized_frames.requests().try_receive(),
            Some(blocker)
        );
        assert_eq!(wake_counter.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            dispatcher
                .inner
                .poll_authorized_frame_request_capacity(&mut context),
            Poll::Ready(AuthorizedFrameRequestCapacity::Ready)
        );
        assert_eq!(
            dispatcher.inner.step(1_000_601),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.authorized_frames.requests().try_receive(),
            Some(expected),
            "full-channel retry changed the retained observation"
        );
        assert_no_completion(&mut router);
        dispatcher
            .authorized_frames
            .acknowledgements()
            .try_send(expected)
            .unwrap_or_else(|_| panic!("matching acknowledgement channel full"));
        assert_eq!(
            dispatcher.inner.step(1_000_602),
            RadioTxDispatcherStep::Advanced
        );
        assert_no_completion(&mut router);
        assert_eq!(
            dispatcher.inner.step(1_000_603),
            RadioTxDispatcherStep::Advanced
        );
        let completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));
    }

    #[test]
    fn mismatched_frame_acknowledgement_disables_and_retains_both_observations() {
        let (_other_owner, _other_dispatcher, _other_router, other_report) =
            terminal_data_gate_fixture(88, TxScript::SuccessOne(1_000_500), None);
        let actual = other_report.authorized_frame().unwrap();
        let (_owner, mut dispatcher, mut router, report) =
            terminal_data_gate_fixture(90, TxScript::SuccessOne(1_000_500), None);
        let expected = report.authorized_frame().unwrap();
        assert_ne!(expected, actual);
        assert_eq!(
            dispatcher.inner.step(1_000_600),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.authorized_frames.requests().try_receive(),
            Some(expected)
        );
        dispatcher
            .authorized_frames
            .acknowledgements()
            .try_send(actual)
            .unwrap_or_else(|_| panic!("mismatched acknowledgement channel full"));
        assert_eq!(
            dispatcher.inner.step(1_000_601),
            RadioTxDispatcherStep::Disabled(
                DispatcherFault::AuthorizedFrameAcknowledgementMismatch
            )
        );
        assert_eq!(
            dispatcher.inner.fault(),
            Some(DispatcherFault::AuthorizedFrameAcknowledgementMismatch)
        );
        assert_eq!(
            dispatcher.inner.fault_residue_kind(),
            Some(DispatcherFaultResidueKind::AuthorizedFrameAcknowledgementMismatch)
        );
        let residue = dispatcher
            .inner
            .authorized_frame_acknowledgement_mismatch()
            .unwrap();
        assert_eq!(residue.expected(), expected);
        assert_eq!(residue.actual(), actual);
        assert_no_completion(&mut router);
        assert_eq!(
            dispatcher.inner.step(1_000_602),
            RadioTxDispatcherStep::Disabled(
                DispatcherFault::AuthorizedFrameAcknowledgementMismatch
            )
        );
        assert_no_completion(&mut router);
    }

    #[test]
    fn acknowledgement_before_request_is_rejected_even_when_exact() {
        let (_owner, mut dispatcher, mut router, report) =
            terminal_data_gate_fixture(92, TxScript::SuccessOne(1_000_500), None);
        let expected = report.authorized_frame().unwrap();
        dispatcher
            .authorized_frames
            .acknowledgements()
            .try_send(expected)
            .unwrap_or_else(|_| panic!("premature acknowledgement channel full"));
        assert_eq!(
            dispatcher.inner.step(1_000_600),
            RadioTxDispatcherStep::Disabled(
                DispatcherFault::UnexpectedAuthorizedFrameAcknowledgement
            )
        );
        assert_eq!(
            dispatcher.inner.fault_residue_kind(),
            Some(DispatcherFaultResidueKind::UnexpectedAuthorizedFrameAcknowledgement)
        );
        let residue = dispatcher
            .inner
            .unexpected_authorized_frame_acknowledgement()
            .unwrap();
        assert_eq!(residue.expected(), expected);
        assert_eq!(residue.actual(), expected);
        assert!(
            dispatcher
                .authorized_frames
                .requests()
                .try_receive()
                .is_none()
        );
        assert_no_completion(&mut router);
    }

    fn start_and_clear_ordinary(dispatcher: &mut TestDispatcher, now_us: u64) {
        assert_eq!(dispatcher.step(now_us), RadioTxDispatcherStep::Advanced);
        assert!(matches!(
            dispatcher.step(now_us + 100),
            RadioTxDispatcherStep::NeedCad(DispatchFamily::Ordinary)
        ));
        assert!(matches!(
            block_on(dispatcher.perform_radio_operation(now_us + 101)),
            RadioOperationStep::CadObserved {
                family: DispatchFamily::Ordinary,
                activity_detected: false,
                ..
            }
        ));
    }

    fn grant_ordinary(
        dispatcher: &mut TestDispatcher,
        node_ports: &mut OrdinaryNodePermitHandoff<NoopRawMutex>,
        owner: &mut OrdinaryActionOwner<1>,
        policy: &mut impl TxAuthorizationPolicy,
        now_us: u64,
    ) {
        assert_eq!(dispatcher.step(now_us), RadioTxDispatcherStep::Advanced);
        let request = node_ports
            .requests()
            .try_receive()
            .expect("ordinary request");
        let reply = owner
            .authorize_tx(request, MonotonicMillis::new(now_us / 1_000), policy)
            .unwrap_or_else(|failure| panic!("ordinary authorize: {:?}", failure.reason()));
        node_ports
            .replies()
            .try_send(reply)
            .unwrap_or_else(|_| panic!("ordinary reply full"));
        assert_eq!(dispatcher.step(now_us + 1), RadioTxDispatcherStep::Advanced);
        assert_eq!(dispatcher.step(now_us + 2), RadioTxDispatcherStep::Advanced);
    }

    #[test]
    fn mismatched_data_interface_config_returns_exact_owner_without_radio_or_permit_access() {
        let mut owner = node::<1>(61, "mismatched-data-config");
        let receiver = node::<0>(62, "mismatched-data-config-receiver");
        owner
            .register_peer(
                &identity(62),
                "reticulum",
                &["mismatched-data-config-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"wrong actor config",
            100_000,
        );
        let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
        let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
        let (mut data_node, data_dispatch) = data_handoff.split();
        let (_, ordinary_dispatch) = ordinary_handoff.split();
        let stamped = InterfaceConfigId::new(2);
        let (mut dispatcher, mut router) = test_dispatcher_with_registry_config(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![TxScript::SuccessOne(1_000_200)],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
            stamped,
        );
        route_data(&mut router, job);

        assert_eq!(
            block_on(dispatcher.wait_for_job()),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.last_report().map(DispatchReport::outcome),
            Some(DispatchOutcome::InterfaceConfigurationMismatch {
                expected: TEST_INTERFACE_CONFIG,
                stamped,
            })
        );
        assert!(data_node.requests().try_receive().is_none());
        assert_eq!(dispatcher.radio().cad.len(), 1);
        assert_eq!(dispatcher.radio().tx.len(), 1);
        assert!(dispatcher.radio().captured.is_empty());

        assert_eq!(dispatcher.step(1_000_001), RadioTxDispatcherStep::Advanced);
        let completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    }

    #[test]
    fn mismatched_ordinary_interface_config_returns_exact_owner_without_radio_or_permit_access() {
        let mut node = node::<1>(63, "mismatched-ordinary-config");
        let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
        let buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
        let refs = Box::leak(Box::new([buffer]));
        let job = prepare_ordinary(&mut node, &mut owner, refs, 8, 100_000);
        let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
        let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
        let (_, data_dispatch) = data_handoff.split();
        let (mut ordinary_node, ordinary_dispatch) = ordinary_handoff.split();
        let stamped = InterfaceConfigId::new(2);
        let (mut dispatcher, mut router) = test_dispatcher_with_registry_config(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![TxScript::SuccessOne(1_000_200)],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
            stamped,
        );
        route_ordinary(&mut router, job);

        assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.last_report().map(DispatchReport::outcome),
            Some(DispatchOutcome::InterfaceConfigurationMismatch {
                expected: TEST_INTERFACE_CONFIG,
                stamped,
            })
        );
        assert!(ordinary_node.requests().try_receive().is_none());
        assert_eq!(dispatcher.radio().cad.len(), 1);
        assert_eq!(dispatcher.radio().tx.len(), 1);
        assert!(dispatcher.radio().captured.is_empty());

        assert_eq!(dispatcher.step(1_000_001), RadioTxDispatcherStep::Advanced);
        let completion = take_ordinary_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(OrdinaryCompletionDisposition::Returned(_))
        ));
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    }

    #[test]
    fn data_permit_capacity_wake_cancel_retries_and_reconciles_exact_request() {
        let mut owner = node::<2>(65, "data-permit-capacity");
        let receiver = node::<0>(66, "data-permit-capacity-receiver");
        owner
            .register_peer(
                &identity(66),
                "reticulum",
                &["data-permit-capacity-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let blocker_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        owner.register_packet_buffer(blocker_buffer).unwrap();
        let active_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        owner.register_packet_buffer(active_buffer).unwrap();
        let blocker_job = prepare_data(
            &mut owner,
            blocker_buffer,
            receiver.destination_hash(),
            b"permit request blocker",
            100_000,
        );
        let active_job = prepare_data(
            &mut owner,
            active_buffer,
            receiver.destination_hash(),
            b"retained exact request",
            100_000,
        );
        let blocker_requirements = exact_requirements(blocker_job.packet_len());
        let (blocker_pending, blocker_request) = blocker_job.begin_permit(blocker_requirements);

        let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
        let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
        let (mut data_node, mut data_dispatch) = data_handoff.split();
        data_dispatch
            .requests()
            .try_send(blocker_request)
            .unwrap_or_else(|_| panic!("empty DATA request channel rejected blocker"));
        let (_, ordinary_dispatch) = ordinary_handoff.split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, active_job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        assert_eq!(
            dispatcher.step(1_000_200),
            RadioTxDispatcherStep::Backpressured(RadioTxDispatcherChannel::DataPermitRequest)
        );
        let grace_deadline_us = 100_500_000;

        let wake_counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&wake_counter));
        let mut context = Context::from_waker(&waker);
        assert_eq!(
            dispatcher.poll_permit_request_capacity(&mut context),
            PermitRequestCapacity::Pending {
                family: DispatchFamily::Data,
                grace_deadline_us,
            }
        );
        let mut capacity_wait = Box::pin(dispatcher.wait_for_permit_request_capacity());
        assert!(matches!(
            capacity_wait.as_mut().poll(&mut context),
            Poll::Pending
        ));

        let blocker_request = data_node
            .requests()
            .try_receive()
            .expect("DATA blocker request");
        assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
        let blocker_reply = owner
            .authorize_tx(
                blocker_request,
                MonotonicMillis::new(1_001),
                &mut DenyPolicy,
            )
            .unwrap_or_else(|failure| panic!("DATA blocker authorize: {:?}", failure.reason()));
        let blocker = match blocker_pending
            .resolve(blocker_reply, MonotonicMillis::new(1_001))
            .unwrap_or_else(|_| panic!("DATA blocker reply crossed owners"))
        {
            PermitResolution::Unpermitted(owner) => owner.complete(TxCompletionCode::new(0x81)),
            _ => panic!("denied DATA blocker became authorized"),
        };
        assert!(matches!(
            owner.complete_tx(blocker, MonotonicMillis::new(1_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));

        // Cancellation after the wake cannot move the active request or
        // reserve the newly available scalar-control slot.
        drop(capacity_wait);
        assert_eq!(
            dispatcher.poll_permit_request_capacity(&mut context),
            PermitRequestCapacity::Ready {
                family: DispatchFamily::Data,
                grace_deadline_us,
            }
        );
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut DenyPolicy,
            1_001,
            1_000_201,
        );
        assert_eq!(dispatcher.step(1_000_204), RadioTxDispatcherStep::Advanced);
        assert_eq!(dispatcher.step(1_000_205), RadioTxDispatcherStep::Advanced);
        let completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    }

    #[test]
    fn ordinary_permit_capacity_wake_cancel_retries_and_reconciles_exact_request() {
        let mut blocker_node = node::<1>(67, "ordinary-permit-capacity-blocker");
        let mut blocker_owner = blocker_node.take_ordinary_action_owner::<1>().unwrap();
        let blocker_buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
        let blocker_refs = Box::leak(Box::new([blocker_buffer]));
        let blocker_job = prepare_ordinary(
            &mut blocker_node,
            &mut blocker_owner,
            blocker_refs,
            8,
            100_000,
        );
        let blocker_requirements = exact_requirements(blocker_job.packet_len());
        let (blocker_pending, blocker_request) = blocker_job.begin_permit(blocker_requirements);

        let mut node = node::<1>(68, "ordinary-permit-capacity");
        let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
        let active_buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
        let active_refs = Box::leak(Box::new([active_buffer]));
        let active_job = prepare_ordinary(&mut node, &mut owner, active_refs, 8, 100_000);

        let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
        let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
        let (_, data_dispatch) = data_handoff.split();
        let (mut ordinary_node, mut ordinary_dispatch) = ordinary_handoff.split();
        ordinary_dispatch
            .requests()
            .try_send(blocker_request)
            .unwrap_or_else(|_| panic!("empty ordinary request channel rejected blocker"));
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_ordinary(&mut router, active_job);
        start_and_clear_ordinary(&mut dispatcher, 1_000_000);
        assert_eq!(
            dispatcher.step(1_000_200),
            RadioTxDispatcherStep::Backpressured(RadioTxDispatcherChannel::OrdinaryPermitRequest)
        );
        let grace_deadline_us = 100_500_000;

        let wake_counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&wake_counter));
        let mut context = Context::from_waker(&waker);
        assert_eq!(
            dispatcher.poll_permit_request_capacity(&mut context),
            PermitRequestCapacity::Pending {
                family: DispatchFamily::Ordinary,
                grace_deadline_us,
            }
        );
        let mut capacity_wait = Box::pin(dispatcher.wait_for_permit_request_capacity());
        assert!(matches!(
            capacity_wait.as_mut().poll(&mut context),
            Poll::Pending
        ));

        let blocker_request = ordinary_node
            .requests()
            .try_receive()
            .expect("ordinary blocker request");
        assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
        let blocker_reply = blocker_owner
            .authorize_tx(
                blocker_request,
                MonotonicMillis::new(1_001),
                &mut DenyPolicy,
            )
            .unwrap_or_else(|failure| panic!("ordinary blocker authorize: {:?}", failure.reason()));
        let blocker = match blocker_pending
            .resolve(blocker_reply, MonotonicMillis::new(1_001))
            .unwrap_or_else(|_| panic!("ordinary blocker reply crossed owners"))
        {
            OrdinaryPermitResolution::Unpermitted(owner) => {
                owner.complete(TxCompletionCode::new(0x82))
            }
            _ => panic!("denied ordinary blocker became authorized"),
        };
        assert!(matches!(
            blocker_owner.complete_tx(blocker, MonotonicMillis::new(1_001)),
            Ok(OrdinaryCompletionDisposition::Returned(_))
        ));

        drop(capacity_wait);
        assert_eq!(
            dispatcher.poll_permit_request_capacity(&mut context),
            PermitRequestCapacity::Ready {
                family: DispatchFamily::Ordinary,
                grace_deadline_us,
            }
        );
        grant_ordinary(
            &mut dispatcher,
            &mut ordinary_node,
            &mut owner,
            &mut DenyPolicy,
            1_000_201,
        );
        assert_eq!(dispatcher.step(1_000_204), RadioTxDispatcherStep::Advanced);
        assert_eq!(dispatcher.step(1_000_205), RadioTxDispatcherStep::Advanced);
        let completion = take_ordinary_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(OrdinaryCompletionDisposition::Returned(_))
        ));
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    }

    static SUCCESS_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static SUCCESS_ORDINARY_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
        ConstStaticCell::new(OrdinaryPacketBuffer::new());
    static SUCCESS_ORDINARY_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> =
        StaticCell::new();
    static SUCCESS_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static SUCCESS_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn data_and_ordinary_success_share_one_wrapping_sequence_and_exact_permits() {
        let mut owner = node::<1>(1, "success-sender");
        let receiver = node::<0>(2, "success-receiver");
        owner
            .register_peer(
                &identity(2),
                "reticulum",
                &["success-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let data_buffer = SUCCESS_DATA_BUFFER.take();
        owner.register_packet_buffer(data_buffer).unwrap();
        let data_job = prepare_data(
            &mut owner,
            data_buffer,
            receiver.destination_hash(),
            b"one frame",
            100_000,
        );
        let expected_data = data_job.prepared();
        let expected_data_interface = data_job.interface();
        let mut ordinary_owner = owner.take_ordinary_action_owner::<1>().unwrap();
        let ordinary_buffer = SUCCESS_ORDINARY_BUFFER.take();
        let ordinary_refs = SUCCESS_ORDINARY_REFS.init([ordinary_buffer]);
        let ordinary_job =
            prepare_ordinary(&mut owner, &mut ordinary_owner, ordinary_refs, 8, 100_000);
        let (mut data_node, data_dispatch) = SUCCESS_DATA_HANDOFF.take().split();
        let (mut ordinary_node, ordinary_dispatch) = SUCCESS_ORDINARY_HANDOFF.take().split();
        let radio = MockRadio::new(
            vec![
                CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                },
                CadScript::Observation {
                    busy: false,
                    at_us: 2_000_100,
                },
            ],
            vec![
                TxScript::SuccessOne(1_000_500),
                TxScript::SuccessOne(2_000_500),
            ],
        );
        let (mut dispatcher, mut router) = test_dispatcher(
            radio,
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(3),
        );
        route_data(&mut router, data_job);

        assert_eq!(
            block_on(dispatcher.wait_for_job()),
            RadioTxDispatcherStep::Advanced
        );
        route_ordinary(&mut router, ordinary_job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut ExactPolicy,
            1_001,
            1_000_200,
        );
        let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("DATA TX did not terminate: {other:?}"),
        };
        assert_eq!(report.outcome(), DispatchOutcome::Transmitted);
        assert_eq!(report.frame_count(), 1);
        assert_eq!(report.progress().unwrap().completed_frame_count(), 1);
        let authorized_frame = report
            .authorized_frame()
            .expect("DATA byte exposure must be observable");
        assert_eq!(authorized_frame.attempt_handle(), expected_data.handle());
        assert_eq!(authorized_frame.attempt(), expected_data.attempt());
        assert_eq!(authorized_frame.interface(), expected_data_interface);
        assert_eq!(
            authorized_frame.packet_len(),
            usize::from(expected_data.packet_len())
        );
        assert_eq!(
            authorized_frame.encoded_packet_sha256(),
            expected_data.encoded_packet_sha256()
        );
        assert_eq!(dispatcher.step(1_000_600), RadioTxDispatcherStep::Advanced);
        let data_completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(data_completion, MonotonicMillis::new(1_002)),
            Ok(TxCompletionDisposition::Available(_))
        ));

        start_and_clear_ordinary(&mut dispatcher, 2_000_000);
        grant_ordinary(
            &mut dispatcher,
            &mut ordinary_node,
            &mut ordinary_owner,
            &mut ExactPolicy,
            2_000_200,
        );
        let report = match block_on(dispatcher.perform_radio_operation(2_000_300)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("ordinary TX did not terminate: {other:?}"),
        };
        assert_eq!(report.family(), DispatchFamily::Ordinary);
        assert_eq!(report.outcome(), DispatchOutcome::Transmitted);
        assert_eq!(report.authorized_frame(), None);
        assert_eq!(dispatcher.step(2_000_600), RadioTxDispatcherStep::Advanced);
        let completion = take_ordinary_completion(&mut router);
        assert!(matches!(
            ordinary_owner.complete_tx(completion, MonotonicMillis::new(2_001)),
            Ok(OrdinaryCompletionDisposition::Returned(_))
        ));

        let captured = &dispatcher.radio().captured;
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].first[0] >> 4, 3);
        assert_eq!(captured[1].first[0] >> 4, 4);
        assert!(captured.iter().all(|packet| packet.second.is_none()));
    }

    static BUSY_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static BUSY_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static BUSY_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn busy_retry_exhaustion_never_requests_a_permit_or_transmits() {
        let mut owner = node::<1>(3, "busy-sender");
        let receiver = node::<0>(4, "busy-receiver");
        owner
            .register_peer(
                &identity(4),
                "reticulum",
                &["busy-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = BUSY_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"busy",
            100_000,
        );
        let (mut data_node, data_dispatch) = BUSY_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = BUSY_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![
                    CadScript::Observation {
                        busy: true,
                        at_us: 1_000_100,
                    },
                    CadScript::Observation {
                        busy: true,
                        at_us: 1_000_300,
                    },
                ],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);
        assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
        assert!(matches!(
            dispatcher.step(1_000_050),
            RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
        ));
        let _ = block_on(dispatcher.perform_radio_operation(1_000_051));
        assert!(matches!(
            dispatcher.step(1_000_250),
            RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
        ));
        let terminal = block_on(dispatcher.perform_radio_operation(1_000_251));
        assert!(matches!(terminal, RadioOperationStep::Terminal(_)));
        let report = dispatcher.last_report().expect("busy report");
        assert_eq!(
            report.outcome(),
            DispatchOutcome::AccessRejected(LogicalPacketAccessRejection::ChannelBusyExhausted {
                attempts: 2
            })
        );
        assert!(data_node.requests().try_receive().is_none());
        assert!(dispatcher.radio().captured.is_empty());
        assert_eq!(dispatcher.step(1_000_400), RadioTxDispatcherStep::Advanced);
        let _ = take_data_completion(&mut router);
    }

    static CAD_CANCEL_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static CAD_CANCEL_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static CAD_CANCEL_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn dropped_cad_future_returns_unpermitted_owner_then_disables_radio() {
        let mut owner = node::<1>(23, "cad-cancel-sender");
        let receiver = node::<0>(24, "cad-cancel-receiver");
        owner
            .register_peer(
                &identity(24),
                "reticulum",
                &["cad-cancel-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = CAD_CANCEL_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"cancel CAD",
            100_000,
        );
        let (mut data_node, data_dispatch) = CAD_CANCEL_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = CAD_CANCEL_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(vec![CadScript::Pending], vec![]),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);

        assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.step(1_000_100),
            RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
        );
        {
            let mut operation = pin!(dispatcher.perform_radio_operation(1_000_101));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                operation.as_mut().poll(&mut context),
                Poll::Pending
            ));
        }
        assert_eq!(
            dispatcher.phase(),
            RadioTxDispatcherPhase::CadInFlight(DispatchFamily::Data)
        );
        assert!(!dispatcher.radio().is_active());
        assert_eq!(
            block_on(dispatcher.perform_radio_operation(1_000_102)),
            RadioOperationStep::CancelledFutureNeedsRecovery(DispatchFamily::Data)
        );
        assert!(data_node.requests().try_receive().is_none());
        assert!(dispatcher.radio().captured.is_empty());
        assert_eq!(
            dispatcher.recover_cancelled_radio_operation(),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.last_report().unwrap().outcome(),
            DispatchOutcome::CancelledRadioOperation
        );
        assert_eq!(dispatcher.last_report().unwrap().progress(), None);
        assert_eq!(
            dispatcher.step(1_000_200),
            RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
        );
        let completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));
    }

    static DENY_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static DENY_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static DENY_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn policy_denial_returns_definitely_unpermitted_without_byte_access() {
        let mut owner = node::<1>(5, "deny-sender");
        let receiver = node::<0>(6, "deny-receiver");
        owner
            .register_peer(
                &identity(6),
                "reticulum",
                &["deny-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = DENY_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"deny",
            100_000,
        );
        let (mut data_node, data_dispatch) = DENY_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = DENY_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut DenyPolicy,
            1_001,
            1_000_200,
        );
        assert_eq!(dispatcher.step(1_000_300), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.last_report().unwrap().outcome(),
            DispatchOutcome::PermitDenied
        );
        assert!(dispatcher.radio().captured.is_empty());
        assert_eq!(dispatcher.step(1_000_301), RadioTxDispatcherStep::Advanced);
        let _ = take_data_completion(&mut router);
    }

    static EXPIRED_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static EXPIRED_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static EXPIRED_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn grant_issued_before_deadline_but_delivered_at_deadline_stays_authorized() {
        let mut owner = node::<1>(7, "expired-sender");
        let receiver = node::<0>(8, "expired-receiver");
        owner
            .register_peer(
                &identity(8),
                "reticulum",
                &["expired-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = EXPIRED_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"late grant",
            1_100,
        );
        let (mut data_node, data_dispatch) = EXPIRED_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = EXPIRED_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        assert_eq!(dispatcher.step(1_000_200), RadioTxDispatcherStep::Advanced);
        let request = data_node.requests().try_receive().unwrap();
        let reply = owner
            .authorize_tx(request, MonotonicMillis::new(1_050), &mut ExactPolicy)
            .unwrap_or_else(|failure| panic!("late DATA authorize: {:?}", failure.reason()));
        data_node
            .replies()
            .try_send(reply)
            .unwrap_or_else(|_| panic!("reply full"));
        assert_eq!(dispatcher.step(1_100_000), RadioTxDispatcherStep::Advanced);
        assert_eq!(dispatcher.step(1_100_000), RadioTxDispatcherStep::Advanced);
        assert_eq!(dispatcher.step(1_100_001), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.last_report().unwrap().outcome(),
            DispatchOutcome::AuthorizationExpired
        );
        assert!(dispatcher.radio().captured.is_empty());
        assert_eq!(dispatcher.step(1_100_002), RadioTxDispatcherStep::Advanced);
        let _ = take_data_completion(&mut router);
    }

    static DATA_GRACE_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static DATA_GRACE_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static DATA_GRACE_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn lost_data_permit_reply_returns_exact_recovery_then_disables() {
        let mut owner = node::<1>(25, "data-grace-sender");
        let receiver = node::<0>(26, "data-grace-receiver");
        owner
            .register_peer(
                &identity(26),
                "reticulum",
                &["data-grace-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = DATA_GRACE_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"lost DATA reply",
            1_100,
        );
        let (mut data_node, data_dispatch) = DATA_GRACE_HANDOFF.take().split();
        let (_, ordinary_dispatch) = DATA_GRACE_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);

        start_and_clear_data(&mut dispatcher, 1_000_000);
        assert_eq!(dispatcher.step(1_000_200), RadioTxDispatcherStep::Advanced);
        let request = data_node.requests().try_receive().expect("DATA request");
        let lost_reply = owner
            .authorize_tx(request, MonotonicMillis::new(1_001), &mut ExactPolicy)
            .unwrap_or_else(|failure| panic!("DATA authorize: {:?}", failure.reason()));
        drop(lost_reply);
        assert_eq!(
            dispatcher.step(1_599_999),
            RadioTxDispatcherStep::NeedPermitReply {
                family: DispatchFamily::Data,
                grace_deadline_us: 1_600_000,
            }
        );
        assert_eq!(dispatcher.step(1_600_000), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.last_report().unwrap().outcome(),
            DispatchOutcome::ControlPlaneRecovery
        );
        assert_eq!(
            dispatcher.step(1_600_001),
            RadioTxDispatcherStep::Disabled(DispatcherFault::PermitReplyGraceExpired(
                DispatchFamily::Data
            ))
        );
        let completion = take_data_completion(&mut router);
        let quarantine = match owner
            .complete_tx(completion, MonotonicMillis::new(1_600))
            .unwrap_or_else(|failure| panic!("DATA recovery correlation: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Quarantined(quarantine) => quarantine,
            _ => panic!("lost DATA reply did not quarantine"),
        };
        assert_eq!(
            quarantine.record().reason(),
            TxRecoveryReason::CompletionFault(TxCompletionCode::new(7))
        );
        assert_eq!(
            quarantine.record().prior_phase(),
            TxRecoveryPriorPhase::Authorized
        );
    }

    static ORDINARY_GRACE_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
        ConstStaticCell::new(OrdinaryPacketBuffer::new());
    static ORDINARY_GRACE_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> =
        StaticCell::new();
    static ORDINARY_GRACE_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static ORDINARY_GRACE_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn lost_ordinary_permit_reply_returns_exact_recovery_then_disables() {
        let mut node = node::<1>(27, "ordinary-grace");
        let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
        let buffer = ORDINARY_GRACE_BUFFER.take();
        let refs = ORDINARY_GRACE_REFS.init([buffer]);
        let job = prepare_ordinary(&mut node, &mut owner, refs, 8, 1_100);
        let (_, data_dispatch) = ORDINARY_GRACE_DATA_HANDOFF.take().split();
        let (mut ordinary_node, ordinary_dispatch) = ORDINARY_GRACE_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_ordinary(&mut router, job);

        start_and_clear_ordinary(&mut dispatcher, 1_000_000);
        assert_eq!(dispatcher.step(1_000_200), RadioTxDispatcherStep::Advanced);
        let request = ordinary_node
            .requests()
            .try_receive()
            .expect("ordinary request");
        let lost_reply = owner
            .authorize_tx(request, MonotonicMillis::new(1_001), &mut ExactPolicy)
            .unwrap_or_else(|failure| panic!("ordinary authorize: {:?}", failure.reason()));
        drop(lost_reply);
        assert_eq!(
            dispatcher.step(1_599_999),
            RadioTxDispatcherStep::NeedPermitReply {
                family: DispatchFamily::Ordinary,
                grace_deadline_us: 1_600_000,
            }
        );
        assert_eq!(dispatcher.step(1_600_000), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.last_report().unwrap().outcome(),
            DispatchOutcome::ControlPlaneRecovery
        );
        assert_eq!(
            dispatcher.step(1_600_001),
            RadioTxDispatcherStep::Disabled(DispatcherFault::PermitReplyGraceExpired(
                DispatchFamily::Ordinary
            ))
        );
        let completion = take_ordinary_completion(&mut router);
        let quarantine = match owner
            .complete_tx(completion, MonotonicMillis::new(1_600))
            .expect("ordinary recovery correlation")
        {
            OrdinaryCompletionDisposition::Quarantined(quarantine) => quarantine,
            _ => panic!("lost ordinary reply did not quarantine"),
        };
        assert_eq!(
            quarantine.reason(),
            OrdinaryQuarantineReason::RecoveryFault(TxCompletionCode::new(7))
        );
    }

    static STALE_CLEAR_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static STALE_CLEAR_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static STALE_CLEAR_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn post_grant_stale_clear_rejects_without_byte_access_and_resumes() {
        let mut owner = node::<1>(28, "stale-clear-sender");
        let receiver = node::<0>(29, "stale-clear-receiver");
        owner
            .register_peer(
                &identity(29),
                "reticulum",
                &["stale-clear-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = STALE_CLEAR_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"stale clear",
            100_000,
        );
        let (mut data_node, data_dispatch) = STALE_CLEAR_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = STALE_CLEAR_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut ExactPolicy,
            1_001,
            1_000_200,
        );

        let report = match block_on(dispatcher.perform_radio_operation(1_100_001)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("stale clear did not terminate: {other:?}"),
        };
        assert_eq!(
            report.outcome(),
            DispatchOutcome::PostGrantAccessRejected(
                LogicalPacketAccessRejection::ClearObservationTooOld {
                    clear_observed_at_us: 1_000_100,
                    dispatch_start_observed_at_us: 1_100_001,
                    predicted_first_rf_start_us: 1_100_101,
                    observed_age_us: 100_001,
                    maximum_age_us: 100_000,
                }
            )
        );
        assert_eq!(report.progress(), None);
        assert!(dispatcher.radio().is_active());
        assert!(dispatcher.radio().captured.is_empty());
        assert_eq!(dispatcher.step(1_100_002), RadioTxDispatcherStep::Advanced);
        let completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_101)),
            Ok(TxCompletionDisposition::Available(_))
        ));
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    }

    static PRE_PERMIT_DRIFT_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static PRE_PERMIT_DRIFT_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static PRE_PERMIT_DRIFT_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());
    static FINGERPRINT_DRIFT_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static FINGERPRINT_DRIFT_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static FINGERPRINT_DRIFT_ORDINARY_HANDOFF: ConstStaticCell<
        OrdinaryPermitHandoff<NoopRawMutex>,
    > = ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn pre_permit_fingerprint_drift_returns_unpermitted_without_request_or_transmit() {
        let mut owner = node::<1>(34, "pre-permit-drift-sender");
        let receiver = node::<0>(35, "pre-permit-drift-receiver");
        owner
            .register_peer(
                &identity(35),
                "reticulum",
                &["pre-permit-drift-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = PRE_PERMIT_DRIFT_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"pre-permit fingerprint drift",
            100_000,
        );
        let (mut data_node, data_dispatch) = PRE_PERMIT_DRIFT_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = PRE_PERMIT_DRIFT_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);

        assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
        assert!(matches!(
            dispatcher.step(1_000_100),
            RadioTxDispatcherStep::NeedCad(DispatchFamily::Data)
        ));
        dispatcher.radio.fingerprint = RadioConfigurationFingerprint::new([0x92; 16]);
        let report = match block_on(dispatcher.perform_radio_operation(1_000_101)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("pre-permit drift did not terminate: {other:?}"),
        };

        assert_eq!(
            report.outcome(),
            DispatchOutcome::RadioConfigurationChangedBeforePermit
        );
        assert_eq!(report.progress(), None);
        assert!(data_node.requests().try_receive().is_none());
        assert!(!dispatcher.radio().is_active());
        assert!(dispatcher.radio().captured.is_empty());
        assert_eq!(
            dispatcher.step(1_000_102),
            RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
        );
        let completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));
    }

    #[test]
    fn post_grant_fingerprint_drift_shuts_down_without_transmit() {
        let mut owner = node::<1>(30, "fingerprint-drift-sender");
        let receiver = node::<0>(31, "fingerprint-drift-receiver");
        owner
            .register_peer(
                &identity(31),
                "reticulum",
                &["fingerprint-drift-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = FINGERPRINT_DRIFT_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"fingerprint drift",
            100_000,
        );
        let (mut data_node, data_dispatch) = FINGERPRINT_DRIFT_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = FINGERPRINT_DRIFT_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut ExactPolicy,
            1_001,
            1_000_200,
        );
        dispatcher.radio.fingerprint = RadioConfigurationFingerprint::new([0x92; 16]);

        let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("fingerprint drift did not terminate: {other:?}"),
        };
        assert_eq!(
            report.outcome(),
            DispatchOutcome::RadioConfigurationChangedAfterPermit
        );
        assert_eq!(report.progress(), None);
        assert!(!dispatcher.radio().is_active());
        assert!(dispatcher.radio().captured.is_empty());
        assert_eq!(
            dispatcher.step(1_000_301),
            RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
        );
        let completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));
    }

    static PROFILE_DRIFT_ORDINARY_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
        ConstStaticCell::new(OrdinaryPacketBuffer::new());
    static PROFILE_DRIFT_ORDINARY_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> =
        StaticCell::new();
    static PROFILE_DRIFT_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static PROFILE_DRIFT_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn post_grant_profile_drift_shuts_down_ordinary_without_transmit() {
        let mut node = node::<1>(32, "profile-drift");
        let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
        let buffer = PROFILE_DRIFT_ORDINARY_BUFFER.take();
        let refs = PROFILE_DRIFT_ORDINARY_REFS.init([buffer]);
        let job = prepare_ordinary(&mut node, &mut owner, refs, 8, 100_000);
        let (_, data_dispatch) = PROFILE_DRIFT_DATA_HANDOFF.take().split();
        let (mut ordinary_node, ordinary_dispatch) = PROFILE_DRIFT_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_ordinary(&mut router, job);
        start_and_clear_ordinary(&mut dispatcher, 1_000_000);
        grant_ordinary(
            &mut dispatcher,
            &mut ordinary_node,
            &mut owner,
            &mut ExactPolicy,
            1_000_200,
        );
        dispatcher.radio.profile = changed_profile();

        let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("profile drift did not terminate: {other:?}"),
        };
        assert_eq!(report.family(), DispatchFamily::Ordinary);
        assert_eq!(
            report.outcome(),
            DispatchOutcome::RadioConfigurationChangedAfterPermit
        );
        assert_eq!(report.progress(), None);
        assert!(!dispatcher.radio().is_active());
        assert!(dispatcher.radio().captured.is_empty());
        assert_eq!(
            dispatcher.step(1_000_301),
            RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
        );
        let completion = take_ordinary_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(OrdinaryCompletionDisposition::Returned(_))
        ));
    }

    static DATA_RETURN_PRESSURE_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static DATA_RETURN_PRESSURE_BLOCKER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static DATA_RETURN_PRESSURE_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static DATA_RETURN_PRESSURE_ORDINARY_HANDOFF: ConstStaticCell<
        OrdinaryPermitHandoff<NoopRawMutex>,
    > = ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn data_completion_return_survives_channel_backpressure_and_retry() {
        let mut owner = node::<2>(33, "data-return-pressure-sender");
        let receiver = node::<0>(34, "data-return-pressure-receiver");
        owner
            .register_peer(
                &identity(34),
                "reticulum",
                &["data-return-pressure-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = DATA_RETURN_PRESSURE_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let blocker_buffer = DATA_RETURN_PRESSURE_BLOCKER.take();
        owner.register_packet_buffer(blocker_buffer).unwrap();
        let blocker_job = prepare_data(
            &mut owner,
            blocker_buffer,
            receiver.destination_hash(),
            b"DATA completion blocker",
            u64::MAX,
        );
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"DATA return pressure",
            100_000,
        );
        let (mut data_node, data_dispatch) = DATA_RETURN_PRESSURE_HANDOFF.take().split();
        let (_, ordinary_dispatch) = DATA_RETURN_PRESSURE_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, blocker_job);
        assert_eq!(dispatcher.step(900_000), RadioTxDispatcherStep::Advanced);
        assert_eq!(dispatcher.step(900_001), RadioTxDispatcherStep::Advanced);
        route_data(&mut router, job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut DenyPolicy,
            1_001,
            1_000_200,
        );
        assert_eq!(dispatcher.step(1_000_300), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.step(1_000_301),
            RadioTxDispatcherStep::Backpressured(RadioTxDispatcherChannel::InterfaceCompletion(
                DispatchFamily::Data
            ))
        );
        assert_eq!(
            dispatcher.phase(),
            RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Data)
        );

        let wake_counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&wake_counter));
        let mut context = Context::from_waker(&waker);
        let mut capacity_wait = Box::pin(dispatcher.wait_for_interface_completion_capacity());
        assert!(matches!(
            capacity_wait.as_mut().poll(&mut context),
            Poll::Pending
        ));

        let blocker = take_data_completion(&mut router);
        assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
        assert!(matches!(
            owner.complete_tx(blocker, MonotonicMillis::new(1_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));

        // Cancellation after the wake cannot move the stored completion or
        // reserve the newly available queue slot.
        drop(capacity_wait);
        assert_eq!(
            dispatcher.poll_interface_completion_capacity(&mut context),
            Poll::Ready(InterfaceCompletionCapacity::Ready(DispatchFamily::Data))
        );
        assert_eq!(dispatcher.step(1_000_302), RadioTxDispatcherStep::Advanced);
        let completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(TxCompletionDisposition::Available(_))
        ));
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    }

    static ORDINARY_RETURN_PRESSURE_ACTIVE_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
        ConstStaticCell::new(OrdinaryPacketBuffer::new());
    static ORDINARY_RETURN_PRESSURE_ACTIVE_REFS: StaticCell<
        [&'static mut OrdinaryPacketBuffer; 1],
    > = StaticCell::new();
    static ORDINARY_RETURN_PRESSURE_BLOCKER_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
        ConstStaticCell::new(OrdinaryPacketBuffer::new());
    static ORDINARY_RETURN_PRESSURE_BLOCKER_REFS: StaticCell<
        [&'static mut OrdinaryPacketBuffer; 1],
    > = StaticCell::new();
    static ORDINARY_RETURN_PRESSURE_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static ORDINARY_RETURN_PRESSURE_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn ordinary_completion_return_survives_channel_backpressure_and_retry() {
        let mut blocker_node = node::<1>(35, "ordinary-return-blocker");
        let mut blocker_owner = blocker_node.take_ordinary_action_owner::<1>().unwrap();
        let blocker_refs = ORDINARY_RETURN_PRESSURE_BLOCKER_REFS
            .init([ORDINARY_RETURN_PRESSURE_BLOCKER_BUFFER.take()]);
        let blocker_job = prepare_ordinary(
            &mut blocker_node,
            &mut blocker_owner,
            blocker_refs,
            8,
            u64::MAX,
        );

        let mut node = node::<1>(36, "ordinary-return-pressure");
        let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
        let refs = ORDINARY_RETURN_PRESSURE_ACTIVE_REFS
            .init([ORDINARY_RETURN_PRESSURE_ACTIVE_BUFFER.take()]);
        let job = prepare_ordinary(&mut node, &mut owner, refs, 8, 100_000);
        let (_, data_dispatch) = ORDINARY_RETURN_PRESSURE_DATA_HANDOFF.take().split();
        let (mut ordinary_node, ordinary_dispatch) =
            ORDINARY_RETURN_PRESSURE_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_ordinary(&mut router, blocker_job);
        assert_eq!(dispatcher.step(900_000), RadioTxDispatcherStep::Advanced);
        assert_eq!(dispatcher.step(900_001), RadioTxDispatcherStep::Advanced);
        route_ordinary(&mut router, job);
        start_and_clear_ordinary(&mut dispatcher, 1_000_000);
        grant_ordinary(
            &mut dispatcher,
            &mut ordinary_node,
            &mut owner,
            &mut DenyPolicy,
            1_000_200,
        );
        assert_eq!(dispatcher.step(1_000_300), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.step(1_000_301),
            RadioTxDispatcherStep::Backpressured(RadioTxDispatcherChannel::InterfaceCompletion(
                DispatchFamily::Ordinary
            ))
        );
        assert_eq!(
            dispatcher.phase(),
            RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Ordinary)
        );
        let blocker = take_ordinary_completion(&mut router);
        assert!(matches!(
            blocker_owner.complete_tx(blocker, MonotonicMillis::new(1_001)),
            Ok(OrdinaryCompletionDisposition::Returned(_))
        ));
        assert_eq!(dispatcher.step(1_000_302), RadioTxDispatcherStep::Advanced);
        let completion = take_ordinary_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(1_001)),
            Ok(OrdinaryCompletionDisposition::Returned(_))
        ));
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    }

    static OVERFLOW_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static OVERFLOW_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static OVERFLOW_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn deadline_conversion_overflow_preserves_planned_frame_count() {
        let mut owner = node::<1>(19, "overflow-sender");
        let receiver = node::<0>(20, "overflow-receiver");
        owner
            .register_peer(
                &identity(20),
                "reticulum",
                &["overflow-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = OVERFLOW_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"overflow",
            u64::MAX,
        );
        let expected_frames = profile()
            .rnode_packet_airtime(job.packet_len().into())
            .unwrap()
            .frame_count();
        let (_data_node, data_dispatch) = OVERFLOW_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = OVERFLOW_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(vec![], vec![]),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);

        assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
        let report = dispatcher.last_report().expect("overflow report");
        assert_eq!(
            report.outcome(),
            DispatchOutcome::DeadlineConversionOverflow
        );
        assert_eq!(report.frame_count(), expected_frames);
        assert_eq!(dispatcher.step(1_000_001), RadioTxDispatcherStep::Advanced);
        let _ = take_data_completion(&mut router);
        assert!(dispatcher.radio().captured.is_empty());
    }

    static PARTIAL_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static PARTIAL_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static PARTIAL_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn split_frame_tx_fault_preserves_first_frame_progress_and_one_sequence() {
        let mut owner = node::<1>(9, "partial-sender");
        let receiver = node::<0>(10, "partial-receiver");
        owner
            .register_peer(
                &identity(10),
                "reticulum",
                &["partial-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = PARTIAL_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let plaintext = [0x5a; MAX_DATA_PAYLOAD];
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            &plaintext,
            100_000,
        );
        assert!(job.packet_len() > 254);
        let (mut data_node, data_dispatch) = PARTIAL_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = PARTIAL_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![TxScript::Fault(PacketTxProgress::first_completed(
                    1_000_500,
                ))],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut ExactPolicy,
            1_001,
            1_000_200,
        );
        let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("partial TX did not terminate: {other:?}"),
        };
        assert_eq!(report.frame_count(), 2);
        assert!(matches!(report.outcome(), DispatchOutcome::TxFault { .. }));
        assert_eq!(report.progress().unwrap().completed_frame_count(), 1);
        let packet = &dispatcher.radio().captured[0];
        assert!(packet.second.is_some());
        assert_eq!(packet.first[0], packet.second.as_ref().unwrap()[0]);
        assert_eq!(packet.first[0] >> 4, 3);
    }

    static FAULT_OVERCOUNT_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static FAULT_OVERCOUNT_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static FAULT_OVERCOUNT_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn one_frame_tx_fault_with_two_completed_frames_fails_closed() {
        let mut owner = node::<1>(21, "fault-overcount-sender");
        let receiver = node::<0>(22, "fault-overcount-receiver");
        owner
            .register_peer(
                &identity(22),
                "reticulum",
                &["fault-overcount-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = FAULT_OVERCOUNT_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"one frame",
            100_000,
        );
        assert!(job.packet_len() <= 254);
        let (mut data_node, data_dispatch) = FAULT_OVERCOUNT_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = FAULT_OVERCOUNT_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![TxScript::Fault(PacketTxProgress::both_completed(
                    1_000_500, 1_000_700,
                ))],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut ExactPolicy,
            1_001,
            1_000_200,
        );
        let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("fault overcount did not terminate: {other:?}"),
        };
        assert_eq!(report.outcome(), DispatchOutcome::FrameInvariantRecovery);
        assert_eq!(report.frame_count(), 1);
        assert_eq!(report.progress().unwrap().completed_frame_count(), 2);
        assert!(!dispatcher.radio().is_active());
        assert_eq!(
            dispatcher.step(1_000_800),
            RadioTxDispatcherStep::Disabled(DispatcherFault::InternalInvariant)
        );
        let completion = take_data_completion(&mut router);
        let quarantine = match owner
            .complete_tx(completion, MonotonicMillis::new(1_001))
            .unwrap_or_else(|failure| panic!("fault-overcount correlation: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Quarantined(quarantine) => quarantine,
            _ => panic!("fault overcount did not quarantine"),
        };
        assert_eq!(
            quarantine.record().reason(),
            TxRecoveryReason::CompletionFault(TxCompletionCode::new(8))
        );
        assert_eq!(
            quarantine.record().prior_phase(),
            TxRecoveryPriorPhase::Authorized
        );
    }

    static SPLIT_SUCCESS_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static SPLIT_SUCCESS_ORDINARY_BUFFER: ConstStaticCell<OrdinaryPacketBuffer> =
        ConstStaticCell::new(OrdinaryPacketBuffer::new());
    static SPLIT_SUCCESS_ORDINARY_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 1]> =
        StaticCell::new();
    static SPLIT_SUCCESS_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static SPLIT_SUCCESS_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn split_success_requires_two_completions_and_wrong_ordinary_count_fails_closed() {
        let mut owner = node::<1>(17, "split-success-sender");
        let receiver = node::<0>(18, "split-success-receiver");
        owner
            .register_peer(
                &identity(18),
                "reticulum",
                &["split-success-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let data_buffer = SPLIT_SUCCESS_DATA_BUFFER.take();
        owner.register_packet_buffer(data_buffer).unwrap();
        let plaintext = [0x6b; MAX_DATA_PAYLOAD];
        let data_job = prepare_data(
            &mut owner,
            data_buffer,
            receiver.destination_hash(),
            &plaintext,
            100_000,
        );
        assert!(data_job.packet_len() > 254);
        let mut ordinary_owner = owner.take_ordinary_action_owner::<1>().unwrap();
        let ordinary_buffer = SPLIT_SUCCESS_ORDINARY_BUFFER.take();
        let ordinary_refs = SPLIT_SUCCESS_ORDINARY_REFS.init([ordinary_buffer]);
        let ordinary_job =
            prepare_ordinary(&mut owner, &mut ordinary_owner, ordinary_refs, 8, 100_000);
        assert!(ordinary_job.packet_len() <= 254);
        let (mut data_node, data_dispatch) = SPLIT_SUCCESS_DATA_HANDOFF.take().split();
        let (mut ordinary_node, ordinary_dispatch) = SPLIT_SUCCESS_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![
                    CadScript::Observation {
                        busy: false,
                        at_us: 1_000_100,
                    },
                    CadScript::Observation {
                        busy: false,
                        at_us: 2_000_100,
                    },
                ],
                vec![
                    TxScript::SuccessTwo(1_000_500, 1_000_700),
                    TxScript::SuccessTwo(2_000_500, 2_000_700),
                ],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(3),
        );
        route_data(&mut router, data_job);

        start_and_clear_data(&mut dispatcher, 1_000_000);
        route_ordinary(&mut router, ordinary_job);
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut ExactPolicy,
            1_001,
            1_000_200,
        );
        let report = match block_on(dispatcher.perform_radio_operation(1_000_300)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("split success did not terminate: {other:?}"),
        };
        assert_eq!(report.outcome(), DispatchOutcome::Transmitted);
        assert_eq!(report.frame_count(), 2);
        assert_eq!(report.progress().unwrap().completed_frame_count(), 2);
        assert_eq!(dispatcher.step(1_000_800), RadioTxDispatcherStep::Advanced);
        let data_completion = take_data_completion(&mut router);
        assert!(matches!(
            owner.complete_tx(data_completion, MonotonicMillis::new(1_002)),
            Ok(TxCompletionDisposition::Available(_))
        ));

        start_and_clear_ordinary(&mut dispatcher, 2_000_000);
        grant_ordinary(
            &mut dispatcher,
            &mut ordinary_node,
            &mut ordinary_owner,
            &mut ExactPolicy,
            2_000_200,
        );
        let report = match block_on(dispatcher.perform_radio_operation(2_000_300)) {
            RadioOperationStep::Terminal(report) => report,
            other => panic!("ordinary mismatch did not terminate: {other:?}"),
        };
        assert_eq!(report.family(), DispatchFamily::Ordinary);
        assert_eq!(report.outcome(), DispatchOutcome::FrameInvariantRecovery);
        assert_eq!(report.frame_count(), 1);
        assert_eq!(report.progress().unwrap().completed_frame_count(), 2);
        assert!(!dispatcher.radio().is_active());
        assert_eq!(
            dispatcher.step(2_000_800),
            RadioTxDispatcherStep::Disabled(DispatcherFault::InternalInvariant)
        );
        let completion = take_ordinary_completion(&mut router);
        let quarantine = match ordinary_owner
            .complete_tx(completion, MonotonicMillis::new(2_001))
            .expect("ordinary frame-count correlation")
        {
            OrdinaryCompletionDisposition::Quarantined(quarantine) => quarantine,
            _ => panic!("ordinary frame-count mismatch did not quarantine"),
        };
        assert_eq!(
            quarantine.reason(),
            OrdinaryQuarantineReason::RecoveryFault(TxCompletionCode::new(8))
        );
    }

    static CANCEL_DATA_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static CANCEL_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static CANCEL_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn dropped_tx_future_retains_authorized_owner_until_explicit_recovery() {
        let mut owner = node::<1>(11, "cancel-sender");
        let receiver = node::<0>(12, "cancel-receiver");
        owner
            .register_peer(
                &identity(12),
                "reticulum",
                &["cancel-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = CANCEL_DATA_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"cancel",
            100_000,
        );
        let expected_data = job.prepared();
        let expected_data_interface = job.interface();
        let (mut data_node, data_dispatch) = CANCEL_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = CANCEL_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![TxScript::Pending],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        grant_data(
            &mut dispatcher,
            &mut data_node,
            &mut owner,
            &mut ExactPolicy,
            1_001,
            1_000_200,
        );
        {
            let mut operation = pin!(dispatcher.perform_radio_operation(1_000_300));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                operation.as_mut().poll(&mut context),
                Poll::Pending
            ));
        }
        assert_eq!(
            dispatcher.phase(),
            RadioTxDispatcherPhase::TransmitInFlight(DispatchFamily::Data)
        );
        assert!(!dispatcher.radio().is_active());
        assert_eq!(
            dispatcher.recover_cancelled_radio_operation(),
            RadioTxDispatcherStep::Advanced
        );
        let report = dispatcher.last_report().unwrap();
        assert_eq!(report.outcome(), DispatchOutcome::CancelledRadioOperation);
        assert_eq!(report.progress(), None);
        let authorized_frame = report
            .authorized_frame()
            .expect("cancelled TX must retain prior authorized byte exposure");
        assert_eq!(authorized_frame.attempt_handle(), expected_data.handle());
        assert_eq!(authorized_frame.attempt(), expected_data.attempt());
        assert_eq!(authorized_frame.interface(), expected_data_interface);
        assert_eq!(
            authorized_frame.packet_len(),
            usize::from(expected_data.packet_len())
        );
        assert_eq!(
            authorized_frame.encoded_packet_sha256(),
            expected_data.encoded_packet_sha256()
        );
        assert_eq!(
            dispatcher.inner.phase(),
            RadioTxDispatcherPhase::AuthorizedFrameRequest
        );
        assert_no_completion(&mut router);
        assert_eq!(
            dispatcher.inner.step(1_000_400),
            RadioTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.authorized_frames.requests().try_receive(),
            Some(authorized_frame)
        );
        assert_no_completion(&mut router);
        dispatcher
            .authorized_frames
            .acknowledgements()
            .try_send(authorized_frame)
            .unwrap_or_else(|_| panic!("cancelled frame acknowledgement channel full"));
        assert_eq!(
            dispatcher.inner.step(1_000_401),
            RadioTxDispatcherStep::Advanced
        );
        assert_no_completion(&mut router);
        assert_eq!(
            dispatcher.inner.step(1_000_402),
            RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
        );
        let _ = take_data_completion(&mut router);
    }

    static STALE_SOURCE_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static STALE_ACTIVE_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static STALE_DATA_HANDOFF: ConstStaticCell<DataPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(DataPermitHandoff::new());
    static STALE_ORDINARY_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn stale_reply_is_retained_and_matching_pending_owner_returns_recovery() {
        let mut stale_owner = node::<1>(13, "stale-source");
        let stale_receiver = node::<0>(14, "stale-source-receiver");
        stale_owner
            .register_peer(
                &identity(14),
                "reticulum",
                &["stale-source-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let stale_buffer = STALE_SOURCE_BUFFER.take();
        stale_owner.register_packet_buffer(stale_buffer).unwrap();
        let stale_job = prepare_data(
            &mut stale_owner,
            stale_buffer,
            stale_receiver.destination_hash(),
            b"stale",
            100_000,
        );
        let stale_airtime = profile()
            .rnode_packet_airtime(stale_job.packet_len().into())
            .unwrap();
        let stale_requirements = TxPermitRequirements::try_new(
            test_permit_resource(),
            stale_airtime.aggregate_time_on_air_us(),
        )
        .unwrap();
        let (stale_pending, stale_request) = stale_job.begin_permit(stale_requirements);
        let stale_reply = stale_owner
            .authorize_tx(stale_request, MonotonicMillis::new(1_001), &mut ExactPolicy)
            .unwrap_or_else(|failure| panic!("stale source authorize: {:?}", failure.reason()));
        let stale_completion = stale_pending.recovery_fault(TxCompletionCode::new(0x77));
        assert!(
            stale_owner
                .complete_tx(stale_completion, MonotonicMillis::new(1_002))
                .is_ok()
        );

        let mut active_owner = node::<1>(15, "stale-active");
        let active_receiver = node::<0>(16, "stale-active-receiver");
        active_owner
            .register_peer(
                &identity(16),
                "reticulum",
                &["stale-active-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let active_buffer = STALE_ACTIVE_BUFFER.take();
        active_owner.register_packet_buffer(active_buffer).unwrap();
        let active_job = prepare_data(
            &mut active_owner,
            active_buffer,
            active_receiver.destination_hash(),
            b"active",
            100_000,
        );
        let (mut data_node, data_dispatch) = STALE_DATA_HANDOFF.take().split();
        let (_, ordinary_dispatch) = STALE_ORDINARY_HANDOFF.take().split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(
                vec![CadScript::Observation {
                    busy: false,
                    at_us: 1_000_100,
                }],
                vec![],
            ),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, active_job);
        start_and_clear_data(&mut dispatcher, 1_000_000);
        assert_eq!(dispatcher.step(1_000_200), RadioTxDispatcherStep::Advanced);
        assert!(!data_node.requests().is_empty());
        data_node
            .replies()
            .try_send(stale_reply)
            .unwrap_or_else(|_| panic!("stale reply full"));
        assert_eq!(dispatcher.step(1_000_201), RadioTxDispatcherStep::Advanced);
        assert_eq!(dispatcher.step(1_000_202), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.fault(),
            Some(DispatcherFault::PermitReplyMismatch(DispatchFamily::Data))
        );
        assert_eq!(
            dispatcher.fault_residue_kind(),
            Some(DispatcherFaultResidueKind::DataPermitReply)
        );
        assert_eq!(
            dispatcher.step(1_000_203),
            RadioTxDispatcherStep::Disabled(DispatcherFault::PermitReplyMismatch(
                DispatchFamily::Data
            ))
        );
        let _ = take_data_completion(&mut router);
        assert!(dispatcher.radio().captured.is_empty());
    }

    #[test]
    fn bounded_receive_timeout_frame_and_zero_length_are_reusable() {
        let signal = FrameSignal::new(-87, 9);
        let mut dispatcher = dispatcher_with_rx(vec![
            RxScript::NoPreambleTimeout,
            RxScript::Frame {
                len: 3,
                signal,
                at_us: 1_234_567,
            },
            RxScript::Frame {
                len: SX1262_FRAME_MTU,
                signal: FrameSignal::new(-95, 3),
                at_us: 1_234_888,
            },
            RxScript::Frame {
                len: 0,
                signal: FrameSignal::new(-101, -2),
                at_us: 1_234_999,
            },
        ]);
        let mut buffer = [0xa5; SX1262_FRAME_MTU];
        assert_eq!(dispatcher.maximum_receive_operation_us().get(), 1_500_000);

        let timeout = dispatcher
            .start_receive(&mut buffer)
            .unwrap_or_else(|step| panic!("RX did not start: {step:?}"));
        assert_eq!(block_on(timeout), RadioReceiveStep::NoPreambleTimeout);
        assert_eq!(buffer, [0xa5; SX1262_FRAME_MTU]);
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
        assert!(dispatcher.radio().is_active());

        let receive = dispatcher
            .start_receive(&mut buffer)
            .unwrap_or_else(|step| panic!("second RX did not start: {step:?}"));
        let observation = match block_on(receive) {
            RadioReceiveStep::Frame(observation) => observation,
            step => panic!("unexpected frame result: {step:?}"),
        };
        assert_eq!(observation.len(), 3);
        assert_eq!(observation.signal(), signal);
        assert_eq!(observation.received_at_us(), 1_234_567);
        assert_eq!(observation.payload(&buffer), Some(&[0, 1, 2][..]));
        assert_eq!(&buffer[3..], &[0xa5; SX1262_FRAME_MTU - 3]);
        assert_eq!(
            dispatcher.last_receive_step(),
            Some(RadioReceiveStep::Frame(observation))
        );

        let maximum = dispatcher
            .start_receive(&mut buffer)
            .unwrap_or_else(|step| panic!("maximum-length RX did not start: {step:?}"));
        let maximum = match block_on(maximum) {
            RadioReceiveStep::Frame(observation) => observation,
            step => panic!("unexpected maximum-length result: {step:?}"),
        };
        assert_eq!(maximum.len(), SX1262_FRAME_MTU);
        assert_eq!(maximum.payload(&buffer).map(<[u8]>::len), Some(255));
        assert_eq!(buffer[254], 254);

        let before_zero = buffer;
        let zero = dispatcher
            .start_receive(&mut buffer)
            .unwrap_or_else(|step| panic!("zero-length RX did not start: {step:?}"));
        let zero = match block_on(zero) {
            RadioReceiveStep::Frame(observation) => observation,
            step => panic!("unexpected zero-length result: {step:?}"),
        };
        assert!(zero.is_empty());
        assert_eq!(zero.payload(&buffer), Some(&[][..]));
        assert_eq!(buffer, before_zero);
        assert!(dispatcher.radio().is_active());
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
    }

    #[test]
    fn scheduler_can_service_receive_before_queued_data_then_select_tx() {
        let mut sender = node::<1>(41, "rx-priority-data");
        let receiver = node::<0>(42, "rx-priority-data-receiver");
        sender
            .register_peer(
                &identity(42),
                "reticulum",
                &["rx-priority-data-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        sender.register_packet_buffer(buffer).unwrap();
        let job = prepare_data(
            &mut sender,
            buffer,
            receiver.destination_hash(),
            b"priority",
            100_000,
        );
        let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
        let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
        let (_data_node, data_dispatch) = data_handoff.split();
        let (_, ordinary_dispatch) = ordinary_handoff.split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(vec![], vec![]).with_rx(vec![RxScript::NoPreambleTimeout]),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_data(&mut router, job);
        let mut rx_buffer = [0; SX1262_FRAME_MTU];

        let receive = dispatcher
            .start_receive(&mut rx_buffer)
            .unwrap_or_else(|step| panic!("scheduler-selected RX did not start: {step:?}"));
        assert_eq!(block_on(receive), RadioReceiveStep::NoPreambleTimeout);
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
        assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.phase(),
            RadioTxDispatcherPhase::BackingOff(DispatchFamily::Data)
        );
        assert!(dispatcher.radio().is_active());
        assert!(matches!(
            dispatcher.start_receive(&mut rx_buffer),
            Err(RadioReceiveStep::TxPriority(
                RadioTxDispatcherPhase::BackingOff(DispatchFamily::Data)
            ))
        ));
    }

    #[test]
    fn scheduler_can_service_receive_before_queued_ordinary_then_select_tx() {
        let mut node = node::<1>(43, "rx-priority-ordinary");
        let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
        let ordinary_buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
        let refs = Box::leak(Box::new([ordinary_buffer]));
        let job = prepare_ordinary(&mut node, &mut owner, refs, 4, 100_000);
        let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
        let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
        let (_, data_dispatch) = data_handoff.split();
        let (_ordinary_node, ordinary_dispatch) = ordinary_handoff.split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(vec![], vec![]).with_rx(vec![RxScript::NoPreambleTimeout]),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        route_ordinary(&mut router, job);
        let mut rx_buffer = [0; SX1262_FRAME_MTU];

        let receive = dispatcher
            .start_receive(&mut rx_buffer)
            .unwrap_or_else(|step| panic!("scheduler-selected RX did not start: {step:?}"));
        assert_eq!(block_on(receive), RadioReceiveStep::NoPreambleTimeout);
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Idle);
        assert_eq!(dispatcher.step(1_000_000), RadioTxDispatcherStep::Advanced);
        assert_eq!(
            dispatcher.phase(),
            RadioTxDispatcherPhase::BackingOff(DispatchFamily::Ordinary)
        );
        assert!(dispatcher.radio().is_active());
    }

    #[test]
    fn dropping_unpolled_receive_shuts_down_and_requires_explicit_recovery() {
        let mut dispatcher = dispatcher_with_rx(vec![RxScript::Pending]);
        let mut buffer = [0; SX1262_FRAME_MTU];
        let receive = dispatcher
            .start_receive(&mut buffer)
            .unwrap_or_else(|step| panic!("RX did not start: {step:?}"));
        drop(receive);

        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::ReceiveInFlight);
        assert!(!dispatcher.radio().is_active());
        assert_eq!(dispatcher.last_receive_step(), None);
        assert_eq!(
            block_on(dispatcher.perform_radio_operation(1_000_001)),
            RadioOperationStep::ReceiveFutureNeedsRecovery
        );
        assert_eq!(
            dispatcher.recover_cancelled_radio_operation(),
            RadioTxDispatcherStep::Disabled(DispatcherFault::ReceiveCancelled)
        );
        assert_eq!(
            dispatcher.last_receive_step(),
            Some(RadioReceiveStep::Disabled(
                DispatcherFault::ReceiveCancelled
            ))
        );
        assert_eq!(
            dispatcher.step(1_000_002),
            RadioTxDispatcherStep::Disabled(DispatcherFault::ReceiveCancelled)
        );
    }

    #[test]
    fn pending_receive_gates_the_shared_queued_job_wait_until_recovery() {
        let mut sender = node::<1>(44, "rx-cancel-data");
        let receiver = node::<0>(45, "rx-cancel-data-receiver");
        sender
            .register_peer(
                &identity(45),
                "reticulum",
                &["rx-cancel-data-receiver"],
                MonotonicSeconds::new(0),
            )
            .unwrap();
        let data_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        sender.register_packet_buffer(data_buffer).unwrap();
        let data_job = prepare_data(
            &mut sender,
            data_buffer,
            receiver.destination_hash(),
            b"queued while rx cancelled",
            100_000,
        );
        let mut ordinary_node_core = node::<1>(46, "rx-cancel-ordinary");
        let mut ordinary_owner = ordinary_node_core
            .take_ordinary_action_owner::<1>()
            .unwrap();
        let ordinary_buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
        let ordinary_refs = Box::leak(Box::new([ordinary_buffer]));
        let ordinary_job = prepare_ordinary(
            &mut ordinary_node_core,
            &mut ordinary_owner,
            ordinary_refs,
            4,
            100_000,
        );
        let data_handoff = Box::leak(Box::new(DataPermitHandoff::<NoopRawMutex>::new()));
        let ordinary_handoff = Box::leak(Box::new(OrdinaryPermitHandoff::<NoopRawMutex>::new()));
        let (_data_node, data_dispatch) = data_handoff.split();
        let (_ordinary_node, ordinary_dispatch) = ordinary_handoff.split();
        let (mut dispatcher, mut router) = test_dispatcher(
            MockRadio::new(vec![], vec![]).with_rx(vec![RxScript::Pending]),
            CounterRng::default(),
            data_dispatch,
            ordinary_dispatch,
            config(2),
        );
        let mut rx_buffer = [0; SX1262_FRAME_MTU];
        {
            let mut receive = pin!(
                dispatcher
                    .start_receive(&mut rx_buffer)
                    .unwrap_or_else(|step| panic!("RX did not start: {step:?}"))
            );
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(receive.as_mut().poll(&mut context), Poll::Pending));
        }
        route_data(&mut router, data_job);
        let ordinary_job = match router.try_route_ordinary(ordinary_job) {
            Err(failure) => failure.into_job(),
            Ok(_) => panic!("shared interface queue accepted an over-capacity ordinary job"),
        };

        assert_eq!(
            block_on(dispatcher.wait_for_job()),
            RadioTxDispatcherStep::NeedReceiveRecovery
        );
        let ordinary_job = match router.try_route_ordinary(ordinary_job) {
            Err(failure) => failure.into_job(),
            Ok(_) => panic!("RX-gated DATA owner left the interface queue"),
        };
        assert_eq!(
            block_on(dispatcher.wait_for_permit_reply()),
            RadioTxDispatcherStep::NeedReceiveRecovery
        );
        assert_eq!(
            dispatcher.step(1_000_001),
            RadioTxDispatcherStep::NeedReceiveRecovery
        );
        assert!(!dispatcher.radio().is_active());
        assert_eq!(
            dispatcher.recover_cancelled_radio_operation(),
            RadioTxDispatcherStep::Disabled(DispatcherFault::ReceiveCancelled)
        );
        assert!(router.try_route_ordinary(ordinary_job).is_err());
    }

    #[test]
    fn receive_fault_disables_radio_and_retains_phase_and_class() {
        let mut dispatcher = dispatcher_with_rx(vec![RxScript::Fault]);
        let mut buffer = [0; SX1262_FRAME_MTU];
        let receive = dispatcher
            .start_receive(&mut buffer)
            .unwrap_or_else(|step| panic!("RX did not start: {step:?}"));
        let expected = RadioReceiveStep::Fault {
            phase: SoleRadioFaultPhase::Receive,
            class: SoleRadioFaultClass::Operation,
        };
        assert_eq!(block_on(receive), expected);
        assert!(!dispatcher.radio().is_active());
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Disabled);
        assert_eq!(dispatcher.last_receive_step(), Some(expected));
        assert_eq!(
            dispatcher.step(1_000_001),
            RadioTxDispatcherStep::Disabled(DispatcherFault::RadioUnavailable)
        );
        assert_eq!(dispatcher.take_last_receive_step(), Some(expected));
    }

    #[test]
    fn invalid_receive_length_fails_closed_and_remains_diagnosable() {
        let invalid_len = SX1262_FRAME_MTU + 1;
        let mut dispatcher = dispatcher_with_rx(vec![RxScript::Frame {
            len: invalid_len,
            signal: FrameSignal::new(-80, 4),
            at_us: 7,
        }]);
        let mut buffer = [0; SX1262_FRAME_MTU];
        let receive = dispatcher
            .start_receive(&mut buffer)
            .unwrap_or_else(|step| panic!("RX did not start: {step:?}"));
        let expected = RadioReceiveStep::InvalidObservation { len: invalid_len };
        assert_eq!(block_on(receive), expected);
        assert!(!dispatcher.radio().is_active());
        assert_eq!(dispatcher.phase(), RadioTxDispatcherPhase::Disabled);
        assert_eq!(dispatcher.last_receive_step(), Some(expected));
        assert_eq!(
            dispatcher.step(1_000_001),
            RadioTxDispatcherStep::Disabled(DispatcherFault::ReceiveObservationInvalid)
        );
    }

    #[test]
    fn embassy_helpers_have_the_expected_absolute_microsecond_shape() {
        let _now: fn() -> u64 = embassy_now_us;
        let wait = embassy_wait_until_us(0);
        drop(wait);
        let _unused_fault_script = CadScript::Fault;
        let _unused_two_frame_success = TxScript::SuccessTwo(1, 2);
        assert_eq!(monotonic_millis_ceiling(0).get(), 0);
        assert_eq!(monotonic_millis_ceiling(1).get(), 1);
        assert_eq!(monotonic_millis_ceiling(1_000).get(), 1);
        assert_eq!(
            monotonic_millis_ceiling(u64::MAX).get(),
            18_446_744_073_709_552
        );
    }
}
