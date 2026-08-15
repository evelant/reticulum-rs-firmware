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
    AttemptHandle, AttemptToken, AuthorizedFrameObservation, AuthorizedTx, EncodedPacketSha256,
    ExpiredAuthorizedTx, MonotonicMillis, OrdinaryAuthorizedTx, OrdinaryExpiredAuthorizedTx,
    OrdinaryPermitPendingTx, OrdinaryPermitReplyMismatch, OrdinaryPermitResolution,
    OrdinaryTxCompletion, OrdinaryTxJob, OrdinaryTxPermitReply, OrdinaryTxPermitRequest,
    OrdinaryUnpermittedTx, PacketInterfaceId, PermitPendingTx, PermitReplyMismatch,
    PermitResolution, RoutedTxJob, TxAuthorizationCandidate, TxAuthorizationPolicy, TxCompletion,
    TxCompletionCode, TxFrameError, TxPermitReply, TxPermitRequest, TxPermitRequirements,
    TxPermitReservation, TxPermitResourceId, TxPolicyDecision, TxPolicyDenial, UnpermittedTx,
};
use reticulum_radio_interface::{
    BoundedRxObservation, BoundedRxOutcome, LoRaProfile, LogicalPacketAccessAction,
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
    profile: LoRaProfile,
    resource: TxPermitResourceId,
}

impl ExactLoRaAirtimePolicy {
    /// Bind the policy to one Reticulum interface, immutable modem profile,
    /// and complete concrete-radio configuration.
    pub const fn new(
        interface: PacketInterfaceId,
        profile: LoRaProfile,
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
    pub const fn profile(self) -> LoRaProfile {
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
    data_packet: Option<DataPacketDispatchObservation>,
    ordinary_packet: Option<OrdinaryPacketDispatchObservation>,
    authorized_frame: Option<AuthorizedFrameObservation>,
}

/// Copy-only identity of one ordinary packet selected for radio dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinaryPacketDispatchObservation {
    token: reticulum_node_core::OrdinaryDispatchToken,
    interface: PacketInterfaceId,
    packet_len: u16,
    encoded_packet_sha256: EncodedPacketSha256,
}

impl OrdinaryPacketDispatchObservation {
    fn from_job(job: &OrdinaryTxJob<'_>) -> Self {
        let prepared = job.prepared();
        Self {
            token: prepared.dispatch_token(),
            interface: prepared.interface(),
            packet_len: prepared.packet_len(),
            encoded_packet_sha256: prepared.encoded_packet_sha256(),
        }
    }

    /// Transport-neutral packet-buffer generation.
    pub const fn token(self) -> reticulum_node_core::OrdinaryDispatchToken {
        self.token
    }

    /// Concrete Reticulum interface selected for this hop.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Complete encoded packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// SHA-256 over every byte in the complete encoded packet.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }
}

/// Copy-only identity of one prepared DATA packet selected for radio dispatch.
///
/// Unlike [`AuthorizedFrameObservation`], this evidence does not assert that
/// packet bytes were exposed to the radio or reached RF. It is available as
/// soon as routing selected the exact interface, so pre-authorization channel
/// access and control-plane failures remain correlatable with app-side packet
/// evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataPacketDispatchObservation {
    attempt_handle: AttemptHandle,
    attempt: AttemptToken,
    interface: PacketInterfaceId,
    packet_len: u16,
    encoded_packet_sha256: EncodedPacketSha256,
}

impl DataPacketDispatchObservation {
    fn from_job(job: &RoutedTxJob<'_>) -> Self {
        Self {
            attempt_handle: job.attempt_handle(),
            attempt: job.attempt(),
            interface: job.interface(),
            packet_len: job.packet_len(),
            encoded_packet_sha256: job.prepared().encoded_packet_sha256(),
        }
    }

    /// Generation-checked same-boot identity of this DATA attempt.
    pub const fn attempt_handle(self) -> AttemptHandle {
        self.attempt_handle
    }

    /// Hop-invariant Reticulum proof-correlation hash for this DATA attempt.
    pub const fn attempt(self) -> AttemptToken {
        self.attempt
    }

    /// Exact Reticulum interface selected for this dispatch attempt.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Complete encoded interface-packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// SHA-256 over every byte in the complete encoded interface packet.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }
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
            data_packet: None,
            ordinary_packet: None,
            authorized_frame: None,
        }
    }

    const fn data(
        outcome: DispatchOutcome,
        frame_count: u8,
        progress: Option<PacketTxProgress>,
        data_packet: DataPacketDispatchObservation,
    ) -> Self {
        Self {
            family: DispatchFamily::Data,
            outcome,
            frame_count,
            progress,
            data_packet: Some(data_packet),
            ordinary_packet: None,
            authorized_frame: None,
        }
    }

    const fn ordinary(
        outcome: DispatchOutcome,
        frame_count: u8,
        progress: Option<PacketTxProgress>,
        ordinary_packet: OrdinaryPacketDispatchObservation,
    ) -> Self {
        Self {
            family: DispatchFamily::Ordinary,
            outcome,
            frame_count,
            progress,
            data_packet: None,
            ordinary_packet: Some(ordinary_packet),
            authorized_frame: None,
        }
    }

    const fn with_ordinary_packet(
        mut self,
        ordinary_packet: OrdinaryPacketDispatchObservation,
    ) -> Self {
        self.ordinary_packet = Some(ordinary_packet);
        self
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

    /// Prepared DATA packet identity retained for every DATA terminal outcome.
    ///
    /// This does not imply authorization or RF exposure. Ordinary reports have
    /// no DATA packet observation.
    pub const fn data_packet(self) -> Option<DataPacketDispatchObservation> {
        self.data_packet
    }

    /// Exact ordinary packet identity for every ordinary terminal outcome.
    pub const fn ordinary_packet(self) -> Option<OrdinaryPacketDispatchObservation> {
        self.ordinary_packet
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
    /// The scheduler's bounded RX service interval elapsed with no locked frame.
    SchedulerYield,
    /// A corrupt or timed-out physical frame was discarded while RX stayed armed.
    InvalidFrame,
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
    airtime_profile: LoRaProfile,
    data_packet: Option<DataPacketDispatchObservation>,
    ordinary_packet: Option<OrdinaryPacketDispatchObservation>,
}

impl DispatchMeta {
    #[allow(
        clippy::too_many_arguments,
        reason = "constructor retains the complete scalar scheduling and one-of-family trace facts"
    )]
    fn try_new(
        deadline_ms: u64,
        grace_us: u64,
        frame_count: u8,
        aggregate_airtime_us: u64,
        permit_resource: TxPermitResourceId,
        airtime_profile: LoRaProfile,
        data_packet: Option<DataPacketDispatchObservation>,
        ordinary_packet: Option<OrdinaryPacketDispatchObservation>,
    ) -> Option<Self> {
        let deadline_us = deadline_ms.checked_mul(1_000)?;
        Some(Self {
            deadline_us,
            grace_deadline_us: deadline_us.saturating_add(grace_us),
            frame_count,
            aggregate_airtime_us,
            permit_resource,
            airtime_profile,
            data_packet,
            ordinary_packet,
        })
    }

    fn data_report(
        self,
        outcome: DispatchOutcome,
        progress: Option<PacketTxProgress>,
    ) -> DispatchReport {
        DispatchReport::data(
            outcome,
            self.frame_count,
            progress,
            self.data_packet
                .expect("DATA dispatch metadata must retain packet evidence"),
        )
    }

    fn ordinary_report(
        self,
        outcome: DispatchOutcome,
        progress: Option<PacketTxProgress>,
    ) -> DispatchReport {
        DispatchReport::ordinary(
            outcome,
            self.frame_count,
            progress,
            self.ordinary_packet
                .expect("ordinary dispatch metadata must retain packet evidence"),
        )
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

#[allow(
    clippy::large_enum_variant,
    reason = "disablement must retain the exact no-alloc ordinary packet owner for recovery"
)]
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

    fn invalidate_receive_session(&mut self) {
        self.radio.invalidate_receive_session();
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
            self.invalidate_receive_session();
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
                    let data_packet = DataPacketDispatchObservation::from_job(&job);
                    self.active = ActiveFamily::Data;
                    return self.finish_data_unpermitted_job(
                        job,
                        DispatchReport::data(
                            DispatchOutcome::InterfaceConfigurationMismatch {
                                expected: self.config.expected_interface_config,
                                stamped: stamped_config,
                            },
                            0,
                            None,
                            data_packet,
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
        let data_packet = DataPacketDispatchObservation::from_job(&job);
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
                    Some(data_packet),
                    None,
                ) {
                    Some(meta) => meta,
                    None => {
                        return self.finish_data_unpermitted_job(
                            job,
                            DispatchReport::data(
                                DispatchOutcome::DeadlineConversionOverflow,
                                airtime.frame_count(),
                                None,
                                data_packet,
                            ),
                            false,
                        );
                    }
                };
                if !self.radio.is_active() {
                    return self.finish_data_unpermitted_job(
                        job,
                        DispatchReport::data(
                            DispatchOutcome::RadioInactive,
                            airtime.frame_count(),
                            None,
                            data_packet,
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
                        DispatchReport::data(
                            DispatchOutcome::AccessRejected(rejection),
                            airtime.frame_count(),
                            None,
                            data_packet,
                        ),
                        false,
                    ),
                }
            }
            Err(error) => self.finish_data_unpermitted_job(
                job,
                DispatchReport::data(
                    DispatchOutcome::AirtimeRejected(error),
                    0,
                    None,
                    data_packet,
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
        let ordinary_packet = OrdinaryPacketDispatchObservation::from_job(&job);
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
            None,
            Some(ordinary_packet),
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
                    let report = meta.data_report(DispatchOutcome::ControlPlaneRecovery, None);
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
                    let report = meta.data_report(DispatchOutcome::ControlPlaneRecovery, None);
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
                let report = meta.data_report(DispatchOutcome::AuthorizationExpired, None);
                self.record_report(report);
                self.data_state = DataState::Return {
                    completion: owner.complete(self.config.completion_codes.expired_authorization),
                    after: DataAfterReturn::Resume,
                };
                RadioTxDispatcherStep::Advanced
            }
            DataState::Unpermitted { owner, meta } => {
                let report = meta.data_report(DispatchOutcome::PermitDenied, None);
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
                            let report =
                                meta.ordinary_report(DispatchOutcome::ControlPlaneRecovery, None);
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
                    let report = meta.ordinary_report(DispatchOutcome::ControlPlaneRecovery, None);
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
                let report = meta.ordinary_report(DispatchOutcome::AuthorizationExpired, None);
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: owner.cancel(self.config.completion_codes.expired_authorization),
                    after: OrdinaryAfterReturn::Resume,
                };
                RadioTxDispatcherStep::Advanced
            }
            OrdinaryState::Unpermitted { owner, meta } => {
                let report = meta.ordinary_report(DispatchOutcome::PermitDenied, None);
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
    /// the scheduler can wait in persistent continuous RX until the supplied
    /// yield future completes, then call [`Self::step`] or
    /// [`Self::wait_for_job`] to select TX. On success RX is marked in flight
    /// before the adapter future is constructed. Consequently, dropping the
    /// returned future without polling it still leaves a persistent recovery
    /// obligation and invokes the adapter's unpolled-cancellation containment.
    ///
    /// The returned future must run to a frame, invalid-frame terminal, or
    /// normal scheduler yield. The separate progress-deadline future is polled
    /// only after receive progress and recovers a latched false preamble by
    /// rearming RX; it is not a scheduler/ TX-selection wake. If a supervisor
    /// applies a whole-operation watchdog after preamble detection, dropping
    /// the future is terminal cancellation and must be followed by
    /// [`Self::recover_cancelled_radio_operation`].
    pub fn start_continuous_receive_until<'a, SchedulerYield, ProgressDeadline>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
        scheduler_yield: SchedulerYield,
        progress_deadline: ProgressDeadline,
    ) -> Result<impl core::future::Future<Output = RadioReceiveStep> + 'a, RadioReceiveStep>
    where
        SchedulerYield: core::future::Future<Output = ()> + 'a,
        ProgressDeadline: core::future::Future<Output = ()> + 'a,
    {
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
            let result = guard
                .radio
                .receive_continuous_until(buffer, scheduler_yield, progress_deadline)
                .await;
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
                Ok(BoundedRxOutcome::NoPreambleTimeout | BoundedRxOutcome::SchedulerYield) => {
                    if !guard.radio.is_active() {
                        *receive_state = ReceiveState::Disabled(DispatcherFault::RadioUnavailable);
                        RadioReceiveStep::Disabled(DispatcherFault::RadioUnavailable)
                    } else {
                        guard.preserve_active();
                        *receive_state = ReceiveState::Idle;
                        RadioReceiveStep::SchedulerYield
                    }
                }
                Ok(BoundedRxOutcome::InvalidFrame) => {
                    if !guard.radio.is_active() {
                        *receive_state = ReceiveState::Disabled(DispatcherFault::RadioUnavailable);
                        RadioReceiveStep::Disabled(DispatcherFault::RadioUnavailable)
                    } else {
                        guard.preserve_active();
                        *receive_state = ReceiveState::Idle;
                        RadioReceiveStep::InvalidFrame
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
                let report = meta.data_report(
                    DispatchOutcome::CadFault {
                        phase: fault.phase(),
                        class: fault.class(),
                    },
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
                let report = meta.ordinary_report(
                    DispatchOutcome::CadFault {
                        phase: fault.phase(),
                        class: fault.class(),
                    },
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
            let report =
                meta.data_report(DispatchOutcome::RadioConfigurationChangedAfterPermit, None);
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
                let report =
                    meta.data_report(DispatchOutcome::PostGrantAccessRejected(rejection), None);
                self.record_report(report);
                self.data_state = DataState::Return {
                    completion: owner.complete(self.config.completion_codes.post_grant_rejection),
                    after: DataAfterReturn::Resume,
                };
                return RadioOperationStep::Terminal(report);
            }
            Ok(_) | Err(_) => {
                let report = meta.data_report(DispatchOutcome::FrameInvariantRecovery, None);
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
                let report = meta
                    .data_report(DispatchOutcome::FrameInvariantRecovery, None)
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
            let report = meta
                .data_report(DispatchOutcome::RadioInactive, None)
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
                    let report = meta
                        .data_report(DispatchOutcome::FrameInvariantRecovery, Some(progress))
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
                let report = meta
                    .data_report(DispatchOutcome::Transmitted, Some(progress))
                    .with_authorized_frame(Some(authorized_frame));
                self.record_report(report);
                let completion = match physical_completion_millis(progress, meta.frame_count) {
                    Some(completed_at) => owner.complete_transmitted(
                        self.config.completion_codes.transmitted,
                        completed_at,
                    ),
                    None => owner.complete(self.config.completion_codes.transmitted),
                };
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
                    let report = meta
                        .data_report(DispatchOutcome::FrameInvariantRecovery, Some(progress))
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
                let report = meta
                    .data_report(
                        DispatchOutcome::TxFault {
                            phase: fault.fault().phase(),
                            class: fault.fault().class(),
                        },
                        Some(progress),
                    )
                    .with_authorized_frame(Some(authorized_frame));
                self.record_report(report);
                let completion = if progress.completed_frame_count() == meta.frame_count {
                    match physical_completion_millis(progress, meta.frame_count) {
                        Some(completed_at) => owner.complete_transmitted(
                            self.config.completion_codes.tx_fault,
                            completed_at,
                        ),
                        None => owner.complete(self.config.completion_codes.tx_fault),
                    }
                } else {
                    owner.complete(self.config.completion_codes.tx_fault)
                };
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
            let report =
                meta.ordinary_report(DispatchOutcome::RadioConfigurationChangedAfterPermit, None);
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
                let report =
                    meta.ordinary_report(DispatchOutcome::PostGrantAccessRejected(rejection), None);
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: owner.cancel(self.config.completion_codes.post_grant_rejection),
                    after: OrdinaryAfterReturn::Resume,
                };
                return RadioOperationStep::Terminal(report);
            }
            Ok(_) | Err(_) => {
                let report = meta.ordinary_report(DispatchOutcome::FrameInvariantRecovery, None);
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
                let report = meta.ordinary_report(DispatchOutcome::FrameInvariantRecovery, None);
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
            let report = meta.ordinary_report(
                DispatchOutcome::RadioInactive,
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
                    let report = meta
                        .ordinary_report(DispatchOutcome::FrameInvariantRecovery, Some(progress));
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
                let report = meta.ordinary_report(DispatchOutcome::Transmitted, Some(progress));
                self.record_report(report);
                self.ordinary_state = OrdinaryState::Return {
                    completion: owner
                        .complete_transmitted(self.config.completion_codes.transmitted),
                    after: OrdinaryAfterReturn::Resume,
                };
                RadioOperationStep::Terminal(report)
            }
            Err(fault) => {
                let progress = fault.progress();
                if progress.completed_frame_count() > meta.frame_count {
                    self.radio.shutdown();
                    let report = meta
                        .ordinary_report(DispatchOutcome::FrameInvariantRecovery, Some(progress));
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
                let report = meta.ordinary_report(
                    DispatchOutcome::TxFault {
                        phase: fault.fault().phase(),
                        class: fault.fault().class(),
                    },
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
                let (completion, meta, authorized_frame) = match state {
                    DataState::CadInFlight { job, meta, .. } => (
                        job.return_unpermitted()
                            .complete(self.config.completion_codes.cancelled_radio_operation),
                        meta,
                        None,
                    ),
                    DataState::TxInFlight {
                        owner,
                        meta,
                        authorized_frame,
                    } => (
                        owner.complete(self.config.completion_codes.cancelled_radio_operation),
                        meta,
                        Some(authorized_frame),
                    ),
                    state => {
                        self.data_state = state;
                        return RadioTxDispatcherStep::Advanced;
                    }
                };
                self.radio.shutdown();
                let report = meta
                    .data_report(DispatchOutcome::CancelledRadioOperation, None)
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
                let (completion, meta) = match state {
                    OrdinaryState::CadInFlight { job, meta, .. } => (
                        job.return_unpermitted()
                            .complete(self.config.completion_codes.cancelled_radio_operation),
                        meta,
                    ),
                    OrdinaryState::TxInFlight { owner, meta } => (
                        owner.cancel(self.config.completion_codes.cancelled_radio_operation),
                        meta,
                    ),
                    state => {
                        self.ordinary_state = state;
                        return RadioTxDispatcherStep::Advanced;
                    }
                };
                self.radio.shutdown();
                let report = meta.ordinary_report(DispatchOutcome::CancelledRadioOperation, None);
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
        self.invalidate_receive_session();
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
                        meta.data_report(
                            DispatchOutcome::RadioConfigurationChangedBeforePermit,
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
                meta.data_report(DispatchOutcome::AccessRejected(rejection), None),
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
        let report = report.with_ordinary_packet(OrdinaryPacketDispatchObservation::from_job(&job));
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
        let report = meta.data_report(DispatchOutcome::FrameInvariantRecovery, None);
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
        let report = meta.ordinary_report(DispatchOutcome::FrameInvariantRecovery, None);
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
        let report = meta.data_report(DispatchOutcome::ControlPlaneRecovery, None);
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
        let report = meta.ordinary_report(DispatchOutcome::ControlPlaneRecovery, None);
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
        let report = meta.data_report(outcome, None);
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
        let report = meta.ordinary_report(outcome, None);
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

fn physical_completion_millis(
    progress: PacketTxProgress,
    frame_count: u8,
) -> Option<MonotonicMillis> {
    let final_frame = usize::from(frame_count.saturating_sub(1));
    // A missing timestamp does not erase completed-frame evidence. Returning
    // `None` deliberately selects the conservative authorized-completion path,
    // whose receipt boundary is the node owner's later reconciliation sample.
    progress
        .frame_completed_at_us(final_frame)
        .map(monotonic_millis_ceiling)
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
mod tests;
