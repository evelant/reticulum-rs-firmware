use super::*;

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
    pub(crate) fn from_job(job: &OrdinaryTxJob<'_>) -> Self {
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
    pub(crate) fn from_job(job: &RoutedTxJob<'_>) -> Self {
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
    pub(crate) expected: AuthorizedFrameObservation,
    pub(crate) actual: AuthorizedFrameObservation,
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
    pub(crate) const fn new(
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

    pub(crate) const fn data(
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

    pub(crate) const fn ordinary(
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

    pub(crate) const fn with_ordinary_packet(
        mut self,
        ordinary_packet: OrdinaryPacketDispatchObservation,
    ) -> Self {
        self.ordinary_packet = Some(ordinary_packet);
        self
    }

    pub(crate) const fn with_authorized_frame(
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
