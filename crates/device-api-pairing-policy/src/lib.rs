//! Allocation-free physical-presence policy for local device-API pairing.
//!
//! This crate owns only the deterministic button-hold, exclusive-window,
//! connection-binding, attempt-budget, and one-pending state machine. The
//! caller remains responsible for GPIO sampling, USB connection epochs,
//! credential-store facts, entropy, secret ownership, proof verification,
//! durable mutation, and response delivery. In particular, a permit reports
//! policy admission; it is not proof of the asserted physical media state or
//! that flash is writable. Initialization facts are trusted assertions from
//! the physical store owner. That owner must reclassify and recheck the media
//! immediately before executing an admitted initialization.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::num::NonZeroU64;

use reticulum_device_api_credentials::{CredentialGeneration, CredentialId};

/// Required continuous active-low observation before exclusivity is requested.
pub const BUTTON_HOLD_MILLIS: u64 = 2_000;
/// Lifetime of one pairing window, measured from the hold threshold.
///
/// Five minutes leaves ample time to move between the appliance display, the
/// operating-system Bluetooth prompt, and the client without weakening the
/// connection binding or the independent request-attempt budget.
pub const PAIRING_WINDOW_MILLIS: u64 = 300_000;
/// Shared admission budget for classified Begin and Proof requests.
pub const MAX_BEGIN_PROOF_ATTEMPTS: u8 = 3;
/// Conservative fixed-RAM ceiling for the initial policy owner.
pub const PAIRING_POLICY_RAM_CEILING: usize = 256;

/// Boot-lifetime strictly increasing accepted-connection epoch.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConnectionId(NonZeroU64);

impl ConnectionId {
    /// Construct a nonzero connection epoch.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Return the raw connection epoch.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Monotonic milliseconds supplied by the firmware time owner.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MonotonicMillis(u64);

impl MonotonicMillis {
    /// Construct a monotonic timestamp.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the raw millisecond timestamp.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Nonzero boot-lifetime pairing-window sequence.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId(NonZeroU64);

impl WindowId {
    /// Return the raw window sequence.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Exact non-secret reference to the one durable pending enrollment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PendingRef {
    id: CredentialId,
    generation: CredentialGeneration,
}

impl PendingRef {
    /// Validate a pending reference originating from the credential authority.
    pub fn new(
        id: CredentialId,
        generation: CredentialGeneration,
    ) -> Result<Self, InvalidPendingRef> {
        if id.as_bytes().iter().all(|byte| *byte == 0) || generation.get() == 0 {
            return Err(InvalidPendingRef);
        }
        Ok(Self { id, generation })
    }

    /// Credential identifier bound by the pending enrollment.
    pub const fn id(self) -> CredentialId {
        self.id
    }

    /// Exact pending credential generation.
    pub const fn generation(self) -> CredentialGeneration {
        self.generation
    }
}

/// A pending reference used an erased identifier or zero generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidPendingRef;

/// Durable pending state supplied when the boot-lifetime policy owner starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingState {
    /// No enrollment is pending.
    None,
    /// Exactly one validated enrollment is pending.
    One(PendingRef),
}

/// Debounced electrical level of the active-low physical-presence button.
///
/// The board owner must remove electrical bounce before supplying observations;
/// this policy enforces the full continuous-low interval between observations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveLowButton {
    /// Button is released.
    High,
    /// Button is held.
    Low,
}

/// Exact physical-media trajectory eligible for explicit initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitializableMedia {
    /// The complete credential-store provision region is exactly erased.
    ExactlyErased,
    /// The provision region matches the exact recoverable interrupted-write
    /// trajectory accepted by the physical store owner.
    RecoverableInterrupted,
}

/// Trusted facts required before explicit credential-store initialization.
///
/// These values are assertions from the sole identity/flash owner, not facts
/// proved by this policy crate. `None` means the latest classified media state
/// is not eligible. The physical operation must reclassify the media and
/// recheck identity readiness immediately before writing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitializationFacts {
    identity_ready: bool,
    media: Option<InitializableMedia>,
}

impl InitializationFacts {
    /// Construct the latest trusted initialization facts.
    pub const fn new(identity_ready: bool, media: Option<InitializableMedia>) -> Self {
        Self {
            identity_ready,
            media,
        }
    }

    const fn eligible_media(self) -> Option<InitializableMedia> {
        if self.identity_ready {
            self.media
        } else {
            None
        }
    }
}

/// Trusted credential-store facts required before allocating a new enrollment.
///
/// The sole store owner supplies these immediately before admission and must
/// revalidate them before mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BeginFacts {
    mutation_ready: bool,
    retained_capacity_available: bool,
    next_revision_available: bool,
}

impl BeginFacts {
    /// Construct the latest trusted Begin facts.
    pub const fn new(
        mutation_ready: bool,
        retained_capacity_available: bool,
        next_revision_available: bool,
    ) -> Self {
        Self {
            mutation_ready,
            retained_capacity_available,
            next_revision_available,
        }
    }
}

/// A connection announcement was rejected before replacing current ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionRefusal {
    /// Connection epochs must increase strictly for the boot lifetime.
    NotStrictlyIncreasing,
    /// The supplied timestamp regressed.
    ClockRegression,
}

/// Why an exclusive pairing window closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseReason {
    /// The third classified Begin/Proof attempt spent the shared budget.
    ThirdAttempt,
    /// The monotonic deadline was reached.
    Timeout,
    /// The bound accepted connection disconnected or was replaced.
    Disconnect,
    /// A validated activation was reported durably committed.
    Activated,
    /// The supplied monotonic clock regressed.
    ClockFault,
}

/// Exact non-secret facts for one window closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowClosed {
    reason: CloseReason,
    window: WindowId,
    connection: ConnectionId,
}

impl WindowClosed {
    /// Closure reason.
    pub const fn reason(self) -> CloseReason {
        self.reason
    }

    /// Closed window.
    pub const fn window(self) -> WindowId {
        self.window
    }

    /// Connection to which the window was bound.
    pub const fn connection(self) -> ConnectionId {
        self.connection
    }
}

/// Fail-closed internal resource or time fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyFault {
    /// A supplied timestamp was lower than one already observed.
    ClockRegression,
    /// The exact hold threshold or window deadline could not be represented.
    DeadlineOverflow,
    /// The boot-lifetime window sequence was exhausted.
    WindowIdExhausted,
}

/// Effect produced by one button observation.
pub enum ButtonEffect {
    /// No ownership change is required.
    None,
    /// The bearer arbiter must terminate ordinary session ownership before
    /// acknowledging this capability.
    AcquirePairingExclusive(AcquirePairingExclusive),
    /// A window expired before exclusivity could be requested.
    Closed(WindowClosed),
    /// The observation failed closed.
    Fault(PolicyFault),
}

/// Opaque capability requesting exclusive ownership of the bound connection.
///
/// This owner is deliberately neither `Clone`, `Copy`, nor `Debug`.
#[must_use = "exclusive acquisition must be acknowledged or allowed to time out"]
pub struct AcquirePairingExclusive {
    key: WindowKey,
}

/// Public facts for a successfully opened window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowOpened {
    window: WindowId,
    connection: ConnectionId,
    deadline: MonotonicMillis,
}

impl WindowOpened {
    /// Open window sequence.
    pub const fn window(self) -> WindowId {
        self.window
    }

    /// Bound connection epoch.
    pub const fn connection(self) -> ConnectionId {
        self.connection
    }

    /// Exclusive deadline; requests at or after it are refused.
    pub const fn deadline(self) -> MonotonicMillis {
        self.deadline
    }
}

/// Result of acknowledging an exclusive-acquisition capability.
pub enum ExclusiveAcquireOutcome {
    /// The window is now open for requests from its bound connection.
    Opened(WindowOpened),
    /// The deadline won before the acknowledgement.
    Closed(WindowClosed),
    /// The capability no longer names the current acquisition.
    Stale,
    /// Time validation failed closed.
    Fault(PolicyFault),
}

/// Effect produced by monotonic polling or disconnect processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyEvent {
    /// No window closed.
    None,
    /// One window closed.
    Closed(WindowClosed),
    /// Time validation failed closed.
    Fault(PolicyFault),
}

/// Why an ordinary authenticated session cannot currently be admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinarySessionRefusal {
    /// No accepted connection exists.
    NotConnected,
    /// The request names a different connection.
    WrongConnection,
    /// Pairing acquisition, an open window, or an owned operation excludes it.
    PairingExclusive,
    /// The supplied timestamp regressed.
    ClockRegression,
    /// The boot-lifetime admission generation cannot advance safely.
    GenerationExhausted,
}

/// Opaque revocable ordinary-session admission.
#[must_use = "ordinary-session admission must be revalidated before use"]
pub struct OrdinarySessionPermit {
    connection: ConnectionId,
    generation: u64,
}

/// Why a non-attempt pairing request was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestRefusal {
    /// No accepted connection exists.
    NotConnected,
    /// The request names a different connection.
    WrongConnection,
    /// No pairing window is open.
    WindowNotOpen,
    /// The deadline closed the window before this request.
    TimedOut,
    /// Another accepted operation still owns completion.
    OperationInFlight,
    /// Initialization facts do not assert identity readiness and one exact
    /// eligible physical-media trajectory.
    InitializationNotEligible,
    /// Initialization is incompatible with a durable pending enrollment.
    PendingExists,
    /// The requested pending enrollment does not exist.
    PendingMissing,
    /// The request did not name the exact pending identifier and generation.
    PendingMismatch,
    /// The supplied timestamp regressed.
    ClockRegression,
    /// The operation sequence was exhausted.
    OperationIdExhausted,
}

/// Refusal report retaining a closure emitted during request preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestRefused {
    reason: RequestRefusal,
    closed: Option<WindowClosed>,
}

impl RequestRefused {
    /// Refusal reason.
    pub const fn reason(self) -> RequestRefusal {
        self.reason
    }

    /// Closure caused by the same request, if any.
    pub const fn closed(self) -> Option<WindowClosed> {
        self.closed
    }
}

/// Why one classified Begin or Proof request was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptRefusal {
    /// No accepted connection exists.
    NotConnected,
    /// The request names a different connection.
    WrongConnection,
    /// No pairing window is open.
    WindowNotOpen,
    /// The deadline closed the window before this request.
    TimedOut,
    /// Another accepted operation still owns completion.
    OperationInFlight,
    /// A durable pending enrollment already occupies the one slot.
    PendingExists,
    /// No durable pending enrollment exists.
    PendingMissing,
    /// The proof did not name the exact pending identifier and generation.
    PendingMismatch,
    /// The store is not in a clean credential-mutation state.
    MutationBlocked,
    /// The fixed retained-ID lifetime capacity is exhausted.
    CapacityExhausted,
    /// The global authority revision cannot advance.
    RevisionExhausted,
    /// The supplied timestamp regressed.
    ClockRegression,
    /// The operation sequence was exhausted.
    OperationIdExhausted,
}

/// Admission result for one classified Begin or Proof request.
pub enum AttemptDecision<P> {
    /// The attempt was spent and produced one owned operation permit.
    Admitted {
        /// Single-use operation capability.
        permit: P,
        /// Shared attempt ordinal, from one through three.
        ordinal: u8,
        /// Present when this third attempt closed new admission immediately.
        closed_to_new_attempts: Option<WindowClosed>,
    },
    /// The request was refused.
    Refused {
        /// Exact non-secret refusal category.
        reason: AttemptRefusal,
        /// Attempt ordinal when a bound open-window request spent budget.
        ordinal: Option<u8>,
        /// Closure caused by timeout or by spending attempt three.
        closed_to_new_attempts: Option<WindowClosed>,
    },
}

/// Explicit credential-store initialization capability.
///
/// This capability retains the exact trusted media trajectory admitted by the
/// policy. It is deliberately neither `Clone` nor `Copy`, and its media value
/// is not a substitute for the physical runtime's immediate reclassification.
///
/// ```compile_fail
/// use reticulum_device_api_pairing_policy::InitializationPermit;
/// fn require_clone<T: Clone>() {}
/// require_clone::<InitializationPermit>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_pairing_policy::InitializationPermit;
/// fn require_copy<T: Copy>() {}
/// require_copy::<InitializationPermit>();
/// ```
#[must_use = "dropped initialization ownership leaves pairing fail closed"]
pub struct InitializationPermit {
    operation: OperationKey,
    media: InitializableMedia,
}

impl InitializationPermit {
    /// Return the admitted media trajectory for physical reclassification.
    pub const fn media(&self) -> InitializableMedia {
        self.media
    }
}

/// New-pending admission capability.
///
/// ```compile_fail
/// use reticulum_device_api_pairing_policy::BeginAttemptPermit;
/// fn require_clone<T: Clone>() {}
/// require_clone::<BeginAttemptPermit>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_pairing_policy::BeginAttemptPermit;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<BeginAttemptPermit>();
/// ```
#[must_use = "dropped Begin ownership leaves pairing fail closed"]
pub struct BeginAttemptPermit {
    operation: OperationKey,
}

/// Exact-pending proof-attempt capability.
#[must_use = "dropped Proof ownership leaves pairing fail closed"]
pub struct ProofAttemptPermit {
    operation: OperationKey,
    pending: PendingRef,
    deadline: MonotonicMillis,
    continuation_generation: u64,
}

impl ProofAttemptPermit {
    /// Accepted connection to which this exact proof operation is bound.
    pub const fn connection(&self) -> ConnectionId {
        self.operation.window.connection
    }

    /// Physical-presence window to which this proof operation is bound.
    pub const fn window(&self) -> WindowId {
        self.operation.window.window
    }

    /// Original exclusive-window deadline for bounding the continuation even
    /// when this attempt itself closed new admission.
    pub const fn deadline(&self) -> MonotonicMillis {
        self.deadline
    }

    /// Exact durable Pending credential admitted for proof.
    pub const fn pending(&self) -> PendingRef {
        self.pending
    }
}

/// Exact-pending abort capability.
#[must_use = "dropped abort ownership leaves pairing fail closed"]
pub struct AbortPendingPermit {
    operation: OperationKey,
    pending: PendingRef,
}

impl AbortPendingPermit {
    /// Exact durable Pending credential admitted for abort.
    pub const fn pending(&self) -> PendingRef {
        self.pending
    }
}

/// Capability to report the durable result after a verified proof.
#[must_use = "dropped activation ownership leaves pairing fail closed"]
pub struct ActivationPermit {
    operation: OperationKey,
    pending: PendingRef,
}

impl ActivationPermit {
    /// Exact durable Pending credential whose verified proof authorized activation.
    pub const fn pending(&self) -> PendingRef {
        self.pending
    }
}

/// Definite outcome of a Begin mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BeginOutcome {
    /// No pending successor became durable.
    NotCommitted,
    /// This exact pending reference became durable and was read back.
    PendingCommitted(PendingRef),
}

/// Definite outcome of an abort mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbortOutcome {
    /// No tombstone successor became durable.
    NotCommitted,
    /// The exact pending record became a durable aborted tombstone.
    TombstoneCommitted,
}

/// Definite outcome of an activation mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationOutcome {
    /// No Active successor became durable.
    NotCommitted,
    /// The exact pending record became durable and publishable as Active.
    ActiveCommitted,
}

/// A completion capability did not match the manager-owned operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermitError {
    /// No operation is currently owned.
    NoOperation,
    /// The operation ID, window, connection, or kind did not match.
    StaleOrWrongKind,
    /// The reported durable outcome conflicts with current pending state.
    PendingStateConflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowKey {
    window: WindowId,
    connection: ConnectionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OperationKey {
    id: NonZeroU64,
    window: WindowKey,
}

#[derive(Clone, Copy)]
struct Window {
    key: WindowKey,
    deadline: u64,
    attempts: u8,
}

enum WindowState {
    Idle,
    Acquiring(Window),
    Open(Window),
    Draining,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum OperationKind {
    Initialize(InitializableMedia),
    Begin,
    Proof(PendingRef),
    Abort(PendingRef),
    Activate(PendingRef),
}

#[derive(Clone, Copy)]
struct InFlight {
    key: OperationKey,
    kind: OperationKind,
}

/// Boot-lifetime pairing policy owner.
///
/// This manager is deliberately neither `Clone`, `Copy`, nor `Debug`. An
/// accepted operation remains manager-owned across timeout or disconnect so a
/// late durable outcome can be reconciled. Dropping its permit therefore
/// leaves admission fail closed instead of guessing that no mutation occurred.
pub struct PairingPolicy {
    active_connection: Option<ConnectionId>,
    last_connection: u64,
    last_now: Option<u64>,
    released_to_arm: bool,
    hold_started: Option<u64>,
    next_window: u64,
    next_operation: u64,
    admission_generation: u64,
    proof_continuation_generation: u64,
    window: WindowState,
    in_flight: Option<InFlight>,
    pending: Option<PendingRef>,
}

const _: () = assert!(core::mem::size_of::<PairingPolicy>() <= PAIRING_POLICY_RAM_CEILING);

impl PairingPolicy {
    /// Construct one boot-lifetime manager from validated durable pending state.
    pub const fn new(pending: PendingState) -> Self {
        Self {
            active_connection: None,
            last_connection: 0,
            last_now: None,
            released_to_arm: false,
            hold_started: None,
            next_window: 0,
            next_operation: 0,
            admission_generation: 0,
            proof_continuation_generation: 0,
            window: WindowState::Idle,
            in_flight: None,
            pending: match pending {
                PendingState::None => None,
                PendingState::One(pending) => Some(pending),
            },
        }
    }

    /// Accept a strictly newer connection epoch and close any older window.
    pub fn connected(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
    ) -> Result<Option<WindowClosed>, ConnectionRefusal> {
        if self.observe_time(now).is_err() {
            return Err(ConnectionRefusal::ClockRegression);
        }
        if connection.get() <= self.last_connection {
            return Err(ConnectionRefusal::NotStrictlyIncreasing);
        }
        let closed = if self.active_connection.is_some() {
            self.close_window(CloseReason::Disconnect)
        } else {
            None
        };
        self.active_connection = Some(connection);
        self.last_connection = connection.get();
        self.reset_hold();
        self.bump_admission_generation();
        self.invalidate_proof_continuations();
        Ok(closed)
    }

    /// Process a disconnect only when it names the current connection.
    pub fn disconnected(&mut self, now: MonotonicMillis, connection: ConnectionId) -> PolicyEvent {
        if self.observe_time(now).is_err() {
            return PolicyEvent::Fault(PolicyFault::ClockRegression);
        }
        if self.active_connection != Some(connection) {
            return PolicyEvent::None;
        }
        let closed = self.close_window(CloseReason::Disconnect);
        self.active_connection = None;
        self.reset_hold();
        self.bump_admission_generation();
        self.invalidate_proof_continuations();
        closed.map_or(PolicyEvent::None, PolicyEvent::Closed)
    }

    /// Observe one button sample and request exclusivity at the exact threshold.
    pub fn observe_button(&mut self, now: MonotonicMillis, level: ActiveLowButton) -> ButtonEffect {
        if self.observe_time(now).is_err() {
            return ButtonEffect::Fault(PolicyFault::ClockRegression);
        }
        if let Some(closed) = self.expire_window(now.get()) {
            return ButtonEffect::Closed(closed);
        }
        if self.active_connection.is_none()
            || self.in_flight.is_some()
            || !matches!(self.window, WindowState::Idle)
        {
            return ButtonEffect::None;
        }
        match level {
            ActiveLowButton::High => {
                self.released_to_arm = true;
                self.hold_started = None;
                ButtonEffect::None
            }
            ActiveLowButton::Low if !self.released_to_arm => ButtonEffect::None,
            ActiveLowButton::Low => {
                let Some(started) = self.hold_started else {
                    self.hold_started = Some(now.get());
                    return ButtonEffect::None;
                };
                let Some(threshold) = started.checked_add(BUTTON_HOLD_MILLIS) else {
                    self.reset_hold();
                    return ButtonEffect::Fault(PolicyFault::DeadlineOverflow);
                };
                if now.get() < threshold {
                    return ButtonEffect::None;
                }
                let Some(deadline) = threshold.checked_add(PAIRING_WINDOW_MILLIS) else {
                    self.reset_hold();
                    return ButtonEffect::Fault(PolicyFault::DeadlineOverflow);
                };
                let Some(window_id) = self.allocate_window_id() else {
                    self.reset_hold();
                    return ButtonEffect::Fault(PolicyFault::WindowIdExhausted);
                };
                let connection = self
                    .active_connection
                    .expect("button admission checked the accepted connection");
                let window = Window {
                    key: WindowKey {
                        window: window_id,
                        connection,
                    },
                    deadline,
                    attempts: 0,
                };
                self.reset_hold();
                self.window = WindowState::Acquiring(window);
                self.bump_admission_generation();
                if now.get() >= deadline {
                    let closed = self
                        .close_window(CloseReason::Timeout)
                        .expect("the just-created window is closeable");
                    return ButtonEffect::Closed(closed);
                }
                ButtonEffect::AcquirePairingExclusive(AcquirePairingExclusive { key: window.key })
            }
        }
    }

    /// Confirm that the bearer has acquired exclusive ownership.
    pub fn exclusive_acquired(
        &mut self,
        now: MonotonicMillis,
        effect: AcquirePairingExclusive,
    ) -> ExclusiveAcquireOutcome {
        if self.observe_time(now).is_err() {
            return ExclusiveAcquireOutcome::Fault(PolicyFault::ClockRegression);
        }
        if let Some(closed) = self.expire_window(now.get()) {
            return ExclusiveAcquireOutcome::Closed(closed);
        }
        let WindowState::Acquiring(window) = self.window else {
            return ExclusiveAcquireOutcome::Stale;
        };
        if window.key != effect.key || self.active_connection != Some(window.key.connection) {
            return ExclusiveAcquireOutcome::Stale;
        }
        self.window = WindowState::Open(window);
        ExclusiveAcquireOutcome::Opened(WindowOpened {
            window: window.key.window,
            connection: window.key.connection,
            deadline: MonotonicMillis::new(window.deadline),
        })
    }

    /// Close an acquiring/open window at its monotonic deadline.
    pub fn poll_timeout(&mut self, now: MonotonicMillis) -> PolicyEvent {
        if self.observe_time(now).is_err() {
            return PolicyEvent::Fault(PolicyFault::ClockRegression);
        }
        self.expire_window(now.get())
            .map_or(PolicyEvent::None, PolicyEvent::Closed)
    }

    /// Admit one ordinary session only while pairing owns nothing exclusive.
    pub fn ordinary_session(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
    ) -> Result<OrdinarySessionPermit, OrdinarySessionRefusal> {
        if self.observe_time(now).is_err() {
            return Err(OrdinarySessionRefusal::ClockRegression);
        }
        self.expire_window(now.get());
        let Some(active) = self.active_connection else {
            return Err(OrdinarySessionRefusal::NotConnected);
        };
        if active != connection {
            return Err(OrdinarySessionRefusal::WrongConnection);
        }
        if self.in_flight.is_some() || !matches!(self.window, WindowState::Idle) {
            return Err(OrdinarySessionRefusal::PairingExclusive);
        }
        if self.admission_generation == u64::MAX {
            return Err(OrdinarySessionRefusal::GenerationExhausted);
        }
        Ok(OrdinarySessionPermit {
            connection,
            generation: self.admission_generation,
        })
    }

    /// Revalidate an unused ordinary-session admission immediately before use.
    pub fn ordinary_session_is_current(&self, permit: &OrdinarySessionPermit) -> bool {
        self.active_connection == Some(permit.connection)
            && self.admission_generation == permit.generation
            && permit.generation != u64::MAX
            && self.in_flight.is_none()
            && matches!(self.window, WindowState::Idle)
    }

    /// Admit explicit initialization only under an open bound window and
    /// trusted identity/media facts.
    ///
    /// Admission does not spend the shared Begin/Proof attempt budget. The
    /// physical runtime must reclassify and recheck the asserted trajectory
    /// immediately before writing.
    pub fn initialize(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
        facts: InitializationFacts,
    ) -> Result<InitializationPermit, RequestRefused> {
        let window = self.request_window(now, connection)?;
        if self.in_flight.is_some() {
            return Err(Self::request_refused(RequestRefusal::OperationInFlight));
        }
        if self.pending.is_some() {
            return Err(Self::request_refused(RequestRefusal::PendingExists));
        }
        let Some(media) = facts.eligible_media() else {
            return Err(Self::request_refused(
                RequestRefusal::InitializationNotEligible,
            ));
        };
        let operation = self
            .start_operation(window.key, OperationKind::Initialize(media))
            .map_err(Self::request_refused)?;
        Ok(InitializationPermit { operation, media })
    }

    /// Release initialization ownership after a definite physical outcome.
    ///
    /// Ambiguous storage outcomes must retain this exact permit while the
    /// physical runtime reclassifies and reconciles; they must not call this
    /// method or obtain replacement ownership.
    pub fn finish_initialization(
        &mut self,
        permit: InitializationPermit,
    ) -> Result<(), PermitError> {
        self.finish_exact(permit.operation, OperationKind::Initialize(permit.media))?;
        self.finish_draining();
        Ok(())
    }

    /// Spend one shared attempt and, when eligible, reserve the pending slot.
    pub fn begin(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
        facts: BeginFacts,
    ) -> AttemptDecision<BeginAttemptPermit> {
        let window = match self.attempt_window(now, connection) {
            Ok(window) => window,
            Err(decision) => return decision,
        };
        let ordinal = self.spend_attempt();
        let refusal = if self.in_flight.is_some() {
            Some(AttemptRefusal::OperationInFlight)
        } else if self.pending.is_some() {
            Some(AttemptRefusal::PendingExists)
        } else if !facts.mutation_ready {
            Some(AttemptRefusal::MutationBlocked)
        } else if !facts.retained_capacity_available {
            Some(AttemptRefusal::CapacityExhausted)
        } else if !facts.next_revision_available {
            Some(AttemptRefusal::RevisionExhausted)
        } else {
            None
        };
        if let Some(reason) = refusal {
            let closed = self.close_on_third(ordinal);
            return AttemptDecision::Refused {
                reason,
                ordinal: Some(ordinal),
                closed_to_new_attempts: closed,
            };
        }
        let operation = match self.start_operation(window.key, OperationKind::Begin) {
            Ok(operation) => operation,
            Err(_) => {
                let closed = self.close_on_third(ordinal);
                return AttemptDecision::Refused {
                    reason: AttemptRefusal::OperationIdExhausted,
                    ordinal: Some(ordinal),
                    closed_to_new_attempts: closed,
                };
            }
        };
        let closed = self.close_on_third(ordinal);
        AttemptDecision::Admitted {
            permit: BeginAttemptPermit { operation },
            ordinal,
            closed_to_new_attempts: closed,
        }
    }

    /// Reconcile the definite durable result of one Begin operation.
    pub fn finish_begin(
        &mut self,
        permit: BeginAttemptPermit,
        outcome: BeginOutcome,
    ) -> Result<(), PermitError> {
        self.require_exact(permit.operation, OperationKind::Begin)?;
        if self.pending.is_some() {
            return Err(PermitError::PendingStateConflict);
        }
        if let BeginOutcome::PendingCommitted(pending) = outcome {
            self.pending = Some(pending);
        }
        self.in_flight = None;
        self.bump_admission_generation();
        self.finish_draining();
        Ok(())
    }

    /// Spend one shared attempt and admit proof work only for the exact pending
    /// identifier and generation.
    pub fn proof(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
        pending: PendingRef,
    ) -> AttemptDecision<ProofAttemptPermit> {
        let window = match self.attempt_window(now, connection) {
            Ok(window) => window,
            Err(decision) => return decision,
        };
        let ordinal = self.spend_attempt();
        let refusal = if self.in_flight.is_some() {
            Some(AttemptRefusal::OperationInFlight)
        } else {
            match self.pending {
                None => Some(AttemptRefusal::PendingMissing),
                Some(current) if current != pending => Some(AttemptRefusal::PendingMismatch),
                Some(_) => None,
            }
        };
        if let Some(reason) = refusal {
            let closed = self.close_on_third(ordinal);
            return AttemptDecision::Refused {
                reason,
                ordinal: Some(ordinal),
                closed_to_new_attempts: closed,
            };
        }
        let operation = match self.start_operation(window.key, OperationKind::Proof(pending)) {
            Ok(operation) => operation,
            Err(_) => {
                let closed = self.close_on_third(ordinal);
                return AttemptDecision::Refused {
                    reason: AttemptRefusal::OperationIdExhausted,
                    ordinal: Some(ordinal),
                    closed_to_new_attempts: closed,
                };
            }
        };
        let closed = self.close_on_third(ordinal);
        AttemptDecision::Admitted {
            permit: ProofAttemptPermit {
                operation,
                pending,
                deadline: MonotonicMillis::new(window.deadline),
                continuation_generation: self.proof_continuation_generation,
            },
            ordinal,
            closed_to_new_attempts: closed,
        }
    }

    /// Release one rejected proof while retaining the durable pending record.
    pub fn proof_rejected(&mut self, permit: ProofAttemptPermit) -> Result<(), PermitError> {
        self.finish_exact(permit.operation, OperationKind::Proof(permit.pending))?;
        self.finish_draining();
        Ok(())
    }

    /// Revalidate one admitted proof immediately before cryptographic
    /// continuation verification.
    ///
    /// An admitted third attempt remains valid while the policy drains, but a
    /// timeout, disconnect, replacement connection, or clock regression
    /// invalidates the continuation. The exact operation remains owned so the
    /// caller can release it with [`Self::proof_rejected`].
    pub fn proof_continuation_is_current(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
        permit: &ProofAttemptPermit,
    ) -> bool {
        if self.observe_time(now).is_err() {
            return false;
        }
        if now >= permit.deadline {
            self.expire_window(now.get());
            return false;
        }
        self.active_connection == Some(connection)
            && permit.connection() == connection
            && self.proof_continuation_generation == permit.continuation_generation
            && self
                .require_exact(permit.operation, OperationKind::Proof(permit.pending))
                .is_ok()
    }

    /// Convert an admitted, cryptographically verified proof into exact
    /// activation-mutation ownership.
    pub fn proof_verified(
        &mut self,
        permit: ProofAttemptPermit,
    ) -> Result<ActivationPermit, PermitError> {
        self.require_exact(permit.operation, OperationKind::Proof(permit.pending))?;
        let Some(in_flight) = self.in_flight.as_mut() else {
            return Err(PermitError::NoOperation);
        };
        in_flight.kind = OperationKind::Activate(permit.pending);
        Ok(ActivationPermit {
            operation: permit.operation,
            pending: permit.pending,
        })
    }

    /// Reconcile one activation mutation, closing an otherwise-open window only
    /// after the Active successor is durable and publishable.
    pub fn finish_activation(
        &mut self,
        permit: ActivationPermit,
        outcome: ActivationOutcome,
    ) -> Result<Option<WindowClosed>, PermitError> {
        self.require_exact(permit.operation, OperationKind::Activate(permit.pending))?;
        if self.pending != Some(permit.pending) {
            return Err(PermitError::PendingStateConflict);
        }
        if outcome == ActivationOutcome::ActiveCommitted {
            self.pending = None;
        }
        self.in_flight = None;
        self.bump_admission_generation();
        let closed = if outcome == ActivationOutcome::ActiveCommitted {
            self.close_window(CloseReason::Activated)
        } else {
            None
        };
        self.finish_draining();
        Ok(closed)
    }

    /// Admit explicit abort only for the exact durable pending record.
    pub fn abort_pending(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
        pending: PendingRef,
    ) -> Result<AbortPendingPermit, RequestRefused> {
        let window = self.request_window(now, connection)?;
        if self.in_flight.is_some() {
            return Err(Self::request_refused(RequestRefusal::OperationInFlight));
        }
        match self.pending {
            None => return Err(Self::request_refused(RequestRefusal::PendingMissing)),
            Some(current) if current != pending => {
                return Err(Self::request_refused(RequestRefusal::PendingMismatch));
            }
            Some(_) => {}
        }
        let operation = self
            .start_operation(window.key, OperationKind::Abort(pending))
            .map_err(Self::request_refused)?;
        Ok(AbortPendingPermit { operation, pending })
    }

    /// Admit identifier-free abort for the device-selected durable Pending.
    ///
    /// Connection and physical-window checks run before inspecting pending
    /// state, so an unbound caller cannot distinguish missing from present
    /// enrollment state.
    pub fn abort_current(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
    ) -> Result<AbortPendingPermit, RequestRefused> {
        let window = self.request_window(now, connection)?;
        if self.in_flight.is_some() {
            return Err(Self::request_refused(RequestRefusal::OperationInFlight));
        }
        let pending = self
            .pending
            .ok_or_else(|| Self::request_refused(RequestRefusal::PendingMissing))?;
        let operation = self
            .start_operation(window.key, OperationKind::Abort(pending))
            .map_err(Self::request_refused)?;
        Ok(AbortPendingPermit { operation, pending })
    }

    /// Reconcile one exact pending-abort mutation.
    pub fn finish_abort(
        &mut self,
        permit: AbortPendingPermit,
        outcome: AbortOutcome,
    ) -> Result<(), PermitError> {
        self.require_exact(permit.operation, OperationKind::Abort(permit.pending))?;
        if self.pending != Some(permit.pending) {
            return Err(PermitError::PendingStateConflict);
        }
        if outcome == AbortOutcome::TombstoneCommitted {
            self.pending = None;
        }
        self.in_flight = None;
        self.bump_admission_generation();
        self.finish_draining();
        Ok(())
    }

    /// Current exact durable pending reference, if any.
    pub const fn pending(&self) -> Option<PendingRef> {
        self.pending
    }

    /// Whether an accepted operation still owns reconciliation.
    pub const fn operation_outstanding(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Currently accepted connection epoch, if any.
    pub const fn active_connection(&self) -> Option<ConnectionId> {
        self.active_connection
    }

    fn observe_time(&mut self, now: MonotonicMillis) -> Result<(), ()> {
        if self.last_now.is_some_and(|last| now.get() < last) {
            self.close_window(CloseReason::ClockFault);
            self.reset_hold();
            self.bump_admission_generation();
            self.invalidate_proof_continuations();
            return Err(());
        }
        self.last_now = Some(now.get());
        Ok(())
    }

    fn reset_hold(&mut self) {
        self.released_to_arm = false;
        self.hold_started = None;
    }

    fn bump_admission_generation(&mut self) {
        self.admission_generation = self.admission_generation.saturating_add(1);
    }

    fn invalidate_proof_continuations(&mut self) {
        self.proof_continuation_generation = self.proof_continuation_generation.saturating_add(1);
    }

    fn allocate_window_id(&mut self) -> Option<WindowId> {
        let next = self.next_window.checked_add(1)?;
        let nonzero = NonZeroU64::new(next)?;
        self.next_window = next;
        Some(WindowId(nonzero))
    }

    fn allocate_operation_id(&mut self) -> Option<NonZeroU64> {
        let next = self.next_operation.checked_add(1)?;
        let nonzero = NonZeroU64::new(next)?;
        self.next_operation = next;
        Some(nonzero)
    }

    fn expire_window(&mut self, now: u64) -> Option<WindowClosed> {
        let deadline = match self.window {
            WindowState::Acquiring(window) | WindowState::Open(window) => window.deadline,
            WindowState::Idle | WindowState::Draining => return None,
        };
        if now < deadline {
            return None;
        }
        let closed = self.close_window(CloseReason::Timeout);
        if closed.is_some() {
            self.invalidate_proof_continuations();
        }
        closed
    }

    fn close_window(&mut self, reason: CloseReason) -> Option<WindowClosed> {
        let window = match self.window {
            WindowState::Acquiring(window) | WindowState::Open(window) => window,
            WindowState::Idle | WindowState::Draining => return None,
        };
        let closed = WindowClosed {
            reason,
            window: window.key.window,
            connection: window.key.connection,
        };
        self.window = if self.in_flight.is_some() {
            WindowState::Draining
        } else {
            WindowState::Idle
        };
        self.reset_hold();
        self.bump_admission_generation();
        Some(closed)
    }

    fn request_window(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
    ) -> Result<Window, RequestRefused> {
        if self.observe_time(now).is_err() {
            return Err(Self::request_refused(RequestRefusal::ClockRegression));
        }
        if let Some(closed) = self.expire_window(now.get()) {
            return Err(RequestRefused {
                reason: RequestRefusal::TimedOut,
                closed: Some(closed),
            });
        }
        let Some(active) = self.active_connection else {
            return Err(Self::request_refused(RequestRefusal::NotConnected));
        };
        if active != connection {
            return Err(Self::request_refused(RequestRefusal::WrongConnection));
        }
        match self.window {
            WindowState::Open(window) if window.key.connection == connection => Ok(window),
            WindowState::Idle
            | WindowState::Acquiring(_)
            | WindowState::Open(_)
            | WindowState::Draining => Err(Self::request_refused(RequestRefusal::WindowNotOpen)),
        }
    }

    fn request_refused(reason: RequestRefusal) -> RequestRefused {
        RequestRefused {
            reason,
            closed: None,
        }
    }

    fn attempt_window<P>(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
    ) -> Result<Window, AttemptDecision<P>> {
        if self.observe_time(now).is_err() {
            return Err(AttemptDecision::Refused {
                reason: AttemptRefusal::ClockRegression,
                ordinal: None,
                closed_to_new_attempts: None,
            });
        }
        if let Some(closed) = self.expire_window(now.get()) {
            return Err(AttemptDecision::Refused {
                reason: AttemptRefusal::TimedOut,
                ordinal: None,
                closed_to_new_attempts: Some(closed),
            });
        }
        let Some(active) = self.active_connection else {
            return Err(AttemptDecision::Refused {
                reason: AttemptRefusal::NotConnected,
                ordinal: None,
                closed_to_new_attempts: None,
            });
        };
        if active != connection {
            return Err(AttemptDecision::Refused {
                reason: AttemptRefusal::WrongConnection,
                ordinal: None,
                closed_to_new_attempts: None,
            });
        }
        match self.window {
            WindowState::Open(window) if window.key.connection == connection => Ok(window),
            WindowState::Idle
            | WindowState::Acquiring(_)
            | WindowState::Open(_)
            | WindowState::Draining => Err(AttemptDecision::Refused {
                reason: AttemptRefusal::WindowNotOpen,
                ordinal: None,
                closed_to_new_attempts: None,
            }),
        }
    }

    fn spend_attempt(&mut self) -> u8 {
        let window = match &mut self.window {
            WindowState::Open(window) => window,
            WindowState::Idle | WindowState::Acquiring(_) | WindowState::Draining => {
                unreachable!("attempt admission proved an open window")
            }
        };
        window.attempts += 1;
        window.attempts
    }

    fn close_on_third(&mut self, ordinal: u8) -> Option<WindowClosed> {
        (ordinal == MAX_BEGIN_PROOF_ATTEMPTS)
            .then(|| self.close_window(CloseReason::ThirdAttempt))
            .flatten()
    }

    fn start_operation(
        &mut self,
        window: WindowKey,
        kind: OperationKind,
    ) -> Result<OperationKey, RequestRefusal> {
        if self.in_flight.is_some() {
            return Err(RequestRefusal::OperationInFlight);
        }
        let Some(id) = self.allocate_operation_id() else {
            return Err(RequestRefusal::OperationIdExhausted);
        };
        let key = OperationKey { id, window };
        self.in_flight = Some(InFlight { key, kind });
        self.bump_admission_generation();
        Ok(key)
    }

    fn require_exact(
        &self,
        operation: OperationKey,
        kind: OperationKind,
    ) -> Result<(), PermitError> {
        let Some(current) = self.in_flight else {
            return Err(PermitError::NoOperation);
        };
        if current.key != operation || current.kind != kind {
            return Err(PermitError::StaleOrWrongKind);
        }
        Ok(())
    }

    fn finish_exact(
        &mut self,
        operation: OperationKey,
        kind: OperationKind,
    ) -> Result<(), PermitError> {
        self.require_exact(operation, kind)?;
        self.in_flight = None;
        self.bump_admission_generation();
        Ok(())
    }

    fn finish_draining(&mut self) {
        if self.in_flight.is_none() && matches!(self.window, WindowState::Draining) {
            self.window = WindowState::Idle;
            self.bump_admission_generation();
        }
    }
}

#[cfg(test)]
mod tests;
