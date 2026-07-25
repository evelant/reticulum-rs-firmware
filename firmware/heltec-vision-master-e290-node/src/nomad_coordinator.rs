//! Bounded product coordination for one resident NomadNet page fetch.
//!
//! [`reticulum_nomad_protocol::NomadClient`] owns the protocol state. This
//! module adds only the product-side facts that protocol state deliberately
//! cannot own: the client-supplied Unix request timestamp, path and Link
//! establishment deadlines, and the exact native request token that must be
//! canceled before a dispatch rollback or timeout can be committed.
//!
//! The coordinator performs no I/O and allocates no memory. Callers repeatedly
//! inspect [`NomadCoordinator::next_command`], perform the named native action,
//! and report the result through the corresponding transition. Commands that
//! release native authority are two-phase: the native operation must complete
//! before the `*_after_*` transition is called.
//!
//! Path and Link packets remain outside this coordinator until their first
//! real interface dispatch. While such a packet is retained by a retry slot or
//! ordinary router, its owner must suppress the repeated `RequestPath` or
//! `EstablishLink` command. A definitive pre-dispatch return leaves protocol
//! state unchanged and makes that command eligible again.

use core::mem::{align_of, size_of};

use reticulum_nomad_protocol::{
    CachedLink, ControlError, DestinationHash, FetchAction, FetchConfig, FetchFailure,
    FetchOutcome, FetchPhase, LinkFailure, LinkFailureStage, LinkId, MonotonicMillis, NomadClient,
    ObservationDisposition, Page, PagePath, PreparedRequest, RequestFailure, RequestId,
    RequestTimeoutCandidate, StartError,
};

/// Default path-discovery window for one page fetch.
pub const DEFAULT_PATH_TIMEOUT_MS: u64 = 30_000;
/// Default outbound-Link establishment window for one page fetch.
pub const DEFAULT_LINK_TIMEOUT_MS: u64 = 30_000;
/// Largest client timestamp admitted by the reviewed `f64` conversion.
///
/// The integer operands remain exactly representable through this bound, but
/// the final seconds value can still lose sub-millisecond precision at extreme
/// dates because `f64` spacing grows with magnitude.
pub const MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS: u64 = (1_u64 << 53) - 1;
/// Largest native request token admitted by this allocation-free owner.
pub const MAX_NATIVE_REQUEST_TOKEN_BYTES: usize = 32;
/// Largest alignment admitted for a native request token.
pub const MAX_NATIVE_REQUEST_TOKEN_ALIGNMENT: usize = align_of::<u64>();
/// Reviewed RAM ceiling for the complete coordinator, including its page.
pub const NOMAD_COORDINATOR_RAM_CEILING: usize = 1_024;

/// Configuration for bounded product-owned Nomad operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadCoordinatorConfig {
    protocol: FetchConfig,
    path_timeout_ms: u64,
    link_timeout_ms: u64,
}

impl NomadCoordinatorConfig {
    /// Construct a configuration when every timeout is nonzero.
    pub const fn new(
        request_timeout_ms: u64,
        path_timeout_ms: u64,
        link_timeout_ms: u64,
    ) -> Option<Self> {
        let Some(protocol) = FetchConfig::new(request_timeout_ms) else {
            return None;
        };
        if path_timeout_ms == 0 || link_timeout_ms == 0 {
            return None;
        }
        Some(Self {
            protocol,
            path_timeout_ms,
            link_timeout_ms,
        })
    }

    /// Reticulum request-response timeout owned by the protocol state.
    pub const fn request_timeout_ms(self) -> u64 {
        self.protocol.request_timeout_ms()
    }

    /// Path-discovery timeout owned by this coordinator.
    pub const fn path_timeout_ms(self) -> u64 {
        self.path_timeout_ms
    }

    /// Link-establishment timeout owned by this coordinator.
    pub const fn link_timeout_ms(self) -> u64 {
        self.link_timeout_ms
    }
}

impl Default for NomadCoordinatorConfig {
    fn default() -> Self {
        Self {
            protocol: FetchConfig::default(),
            path_timeout_ms: DEFAULT_PATH_TIMEOUT_MS,
            link_timeout_ms: DEFAULT_LINK_TIMEOUT_MS,
        }
    }
}

/// Positive client-supplied Unix time in whole milliseconds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UnixTimestampMillis(u64);

impl UnixTimestampMillis {
    /// Validate a wire timestamp before it enters retained fetch state.
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 || value > MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS {
            None
        } else {
            Some(Self(value))
        }
    }

    /// Return the exact client-supplied whole-millisecond value.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Convert to the seconds representation accepted by the RNS request API.
    ///
    /// The integer seconds and millisecond remainder are converted separately
    /// so a large valid millisecond count is not rounded before division. The
    /// final `f64` remains an approximation: current Unix dates preserve whole
    /// milliseconds, while values near the accepted ceiling may not.
    pub fn as_seconds_f64(self) -> f64 {
        let seconds = self.0 / 1_000;
        let milliseconds = self.0 % 1_000;
        seconds as f64 + milliseconds as f64 / 1_000.0
    }
}

/// Why a page fetch could not enter the single coordinator slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinatorStartError {
    /// A prior invariant fault has permanently stopped this coordinator.
    Faulted(InvariantFault),
    /// Another fetch or unread outcome already owns the slot.
    Busy,
    /// The client timestamp was zero or beyond the reviewed conversion bound.
    InvalidTimestamp {
        /// Rejected whole-millisecond value.
        actual: u64,
        /// Largest accepted whole-millisecond value.
        maximum: u64,
    },
}

/// Product transition whose rejected protocol operation caused an invariant
/// fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinatorOperation {
    /// First path-request packet dispatch confirmation.
    ConfirmPathRequest,
    /// Exact path availability observation.
    PathAvailable,
    /// Exact path unavailability observation.
    PathUnavailable,
    /// First Link-request packet dispatch confirmation.
    ConfirmLinkRequest,
    /// Link preparation failure.
    LinkPreparationFailed,
    /// Exact Link establishment observation.
    LinkEstablished,
    /// Exact Link failure observation.
    LinkFailed,
    /// Exact Link closure observation.
    LinkClosed,
    /// Cached-Link request preparation failure.
    RequestLinkUnavailable,
    /// Request preparation failure before an identifier existed.
    RequestPreparationFailed,
    /// Binding a native request identifier and token.
    RequestPrepared,
    /// Canceling a request before first dispatch.
    CancelRequestDispatch,
    /// Reporting terminal request dispatch failure.
    RequestDispatchFailed,
    /// Confirming the first native request dispatch.
    ConfirmRequestDispatch,
    /// Reporting an exactly correlated remote request failure.
    RequestFailed,
    /// Reporting an exactly correlated response.
    ResponseReceived,
    /// Committing a timeout after exact native cancellation.
    ConfirmRequestTimeout,
    /// Producing the next deterministic command.
    NextCommand,
}

/// Native request authority phase retained beside the protocol client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeRequestPhase {
    /// Packet preparation succeeded but first dispatch has not been confirmed.
    Prepared,
    /// First native interface dispatch has been confirmed.
    Confirmed,
}

/// First product invariant failure retained until reboot or owner replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvariantFault {
    /// An exact protocol control transition rejected product-owned correlation.
    ProtocolControl {
        /// Transition that was being committed.
        operation: CoordinatorOperation,
        /// Protocol rejection.
        error: ControlError,
    },
    /// An exact product-owned observation was not applied by protocol state.
    ProtocolObservation {
        /// Observation that was being committed.
        operation: CoordinatorOperation,
        /// Protocol disposition.
        disposition: ObservationDisposition,
    },
    /// A second native request was supplied while one token remained retained.
    RequestAlreadyTracked,
    /// A native request callback supplied a different opaque token.
    RequestTokenMismatch {
        /// Callback whose token did not match.
        operation: CoordinatorOperation,
    },
    /// A native request callback occurred in the wrong native authority phase.
    RequestPhaseMismatch {
        /// Callback that observed the mismatch.
        operation: CoordinatorOperation,
        /// Required native authority phase.
        expected: NativeRequestPhase,
        /// Retained native authority phase.
        actual: NativeRequestPhase,
    },
    /// An exact timeout or cancellation candidate no longer matched retained
    /// product state.
    CandidateMismatch {
        /// Transition that supplied the stale or foreign candidate.
        operation: CoordinatorOperation,
    },
    /// Active protocol state lost its client-supplied wire timestamp.
    MissingTimestamp,
    /// A confirmed protocol request had no exact native request token.
    MissingNativeRequest,
    /// A terminal result was about to be taken while native authority or an
    /// establishment deadline remained retained.
    OutcomeStillOwned,
}

/// Read-only candidate for a due path-discovery timeout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathTimeoutCandidate {
    destination: DestinationHash,
    deadline: MonotonicMillis,
}

impl PathTimeoutCandidate {
    /// Destination whose path lookup is due.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Saturating monotonic deadline observed as due.
    pub const fn deadline(self) -> MonotonicMillis {
        self.deadline
    }
}

/// Read-only candidate for a due outbound-Link establishment timeout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkTimeoutCandidate {
    destination: DestinationHash,
    link: LinkId,
    deadline: MonotonicMillis,
}

impl LinkTimeoutCandidate {
    /// Destination whose Link is due.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Exact unestablished Link that must be aborted first.
    pub const fn link(self) -> LinkId {
        self.link
    }

    /// Saturating monotonic deadline observed as due.
    pub const fn deadline(self) -> MonotonicMillis {
        self.deadline
    }
}

/// Deterministic native work required to advance the coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinatorCommand<Token> {
    /// Construct and admit one path-discovery action.
    RequestPath {
        /// Destination whose path is needed.
        destination: DestinationHash,
    },
    /// Construct and admit one outbound Link action.
    EstablishLink {
        /// Destination to which the Link is opened.
        destination: DestinationHash,
    },
    /// Prepare one anonymous NomadNet request on an active Link.
    PrepareAnonymousRequest {
        /// Destination serving the requested page.
        destination: DestinationHash,
        /// Exact active Link.
        link: LinkId,
        /// Validated bounded page path.
        path: PagePath,
        /// Original client-supplied Unix timestamp.
        requested_at: UnixTimestampMillis,
    },
    /// Commit path unavailability; no native authority needs cancellation.
    ExpirePath {
        /// Exact read-only deadline candidate.
        candidate: PathTimeoutCandidate,
    },
    /// Abort the exact unestablished Link before committing its timeout.
    AbortTimedOutLink {
        /// Exact read-only deadline candidate.
        candidate: LinkTimeoutCandidate,
    },
    /// Abort retained Link authority after an invariant fault.
    AbortLinkForInvariant {
        /// Exact Link correlation retained before the fault.
        candidate: LinkTimeoutCandidate,
    },
    /// Cancel an exact confirmed native request before committing its timeout.
    CancelTimedOutRequest {
        /// Opaque native request authority.
        token: Token,
        /// Exact protocol timeout candidate.
        candidate: RequestTimeoutCandidate,
    },
    /// Cancel retained native request authority after an invariant fault.
    CancelRequestForInvariant {
        /// Opaque native request authority.
        token: Token,
        /// Link carrying the retained request.
        link: LinkId,
        /// Exact retained request identifier.
        request: RequestId,
        /// Native authority phase that must be canceled.
        phase: NativeRequestPhase,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EstablishmentDeadline {
    Path(PathTimeoutCandidate),
    Link(LinkTimeoutCandidate),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeRequest<Token> {
    token: Token,
    link: LinkId,
    request: RequestId,
    phase: NativeRequestPhase,
}

/// Allocation-free owner joining one Nomad protocol client to native product
/// ownership.
///
/// `Token` is an opaque copyable handle supplied by the native RNS adapter.
/// Construction enforces the reviewed token size and alignment so every valid
/// specialization remains below [`NOMAD_COORDINATOR_RAM_CEILING`].
pub struct NomadCoordinator<Token> {
    client: NomadClient,
    config: NomadCoordinatorConfig,
    requested_at: Option<UnixTimestampMillis>,
    establishment_deadline: Option<EstablishmentDeadline>,
    native_request: Option<NativeRequest<Token>>,
    fault: Option<InvariantFault>,
}

impl<Token: Copy + Eq> NomadCoordinator<Token> {
    const TOKEN_LAYOUT_IS_BOUNDED: () = {
        assert!(size_of::<Token>() <= MAX_NATIVE_REQUEST_TOKEN_BYTES);
        assert!(align_of::<Token>() <= MAX_NATIVE_REQUEST_TOKEN_ALIGNMENT);
    };

    /// Construct one idle, bounded coordinator.
    pub const fn new(config: NomadCoordinatorConfig) -> Self {
        let () = Self::TOKEN_LAYOUT_IS_BOUNDED;
        Self {
            client: NomadClient::new(config.protocol),
            config,
            requested_at: None,
            establishment_deadline: None,
            native_request: None,
            fault: None,
        }
    }

    /// Return the current coarse protocol phase.
    pub const fn phase(&self) -> FetchPhase {
        self.client.phase()
    }

    /// Return the first sticky invariant fault, when present.
    pub const fn fault(&self) -> Option<InvariantFault> {
        self.fault
    }

    /// Return the retained established-Link cache.
    pub const fn cached_link(&self) -> Option<CachedLink> {
        self.client.cached_link()
    }

    /// Return the exact timestamp retained for the active or unread fetch.
    pub const fn requested_at(&self) -> Option<UnixTimestampMillis> {
        self.requested_at
    }

    /// Borrow a ready bounded page without releasing the fetch slot.
    pub const fn ready_page(&self) -> Option<&Page> {
        self.client.ready_page()
    }

    /// Return a retained terminal fetch failure without releasing the slot.
    pub const fn failure(&self) -> Option<FetchFailure> {
        self.client.failure()
    }

    /// Seed one already authenticated active Link while idle.
    pub fn seed_cached_link(&mut self, link: CachedLink) -> Result<(), CoordinatorStartError> {
        self.ensure_healthy()
            .map_err(CoordinatorStartError::Faulted)?;
        self.client
            .seed_cached_link(link)
            .map_err(|error| match error {
                StartError::Busy => CoordinatorStartError::Busy,
            })
    }

    /// Begin one fetch using the timestamp supplied by the authenticated client.
    pub fn start(
        &mut self,
        destination: DestinationHash,
        path: PagePath,
        timestamp_unix_ms: u64,
    ) -> Result<(), CoordinatorStartError> {
        self.ensure_healthy()
            .map_err(CoordinatorStartError::Faulted)?;
        let Some(requested_at) = UnixTimestampMillis::new(timestamp_unix_ms) else {
            return Err(CoordinatorStartError::InvalidTimestamp {
                actual: timestamp_unix_ms,
                maximum: MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS,
            });
        };
        self.client
            .start(destination, path)
            .map_err(|error| match error {
                StartError::Busy => CoordinatorStartError::Busy,
            })?;
        self.requested_at = Some(requested_at);
        Ok(())
    }

    /// Produce at most one deterministic native command for the supplied time.
    ///
    /// Calling this method does not advance a healthy state machine. Timeout
    /// commands remain stable until the corresponding confirmation is applied.
    /// Once faulted, only exact native-authority cleanup can still be emitted.
    /// A path or Link command also remains visible while its pre-dispatch
    /// packet is owned outside this coordinator; that external owner is the
    /// required duplicate-preparation gate.
    pub fn next_command(&mut self, now: MonotonicMillis) -> Option<CoordinatorCommand<Token>> {
        if self.fault.is_some() {
            if let Some(tracked) = self.native_request {
                return Some(CoordinatorCommand::CancelRequestForInvariant {
                    token: tracked.token,
                    link: tracked.link,
                    request: tracked.request,
                    phase: tracked.phase,
                });
            }
            return match self.establishment_deadline {
                Some(EstablishmentDeadline::Link(candidate)) => {
                    Some(CoordinatorCommand::AbortLinkForInvariant { candidate })
                }
                Some(EstablishmentDeadline::Path(_)) | None => None,
            };
        }

        if let Some(action) = self.client.action() {
            return match action {
                FetchAction::RequestPath { destination } => {
                    Some(CoordinatorCommand::RequestPath { destination })
                }
                FetchAction::EstablishLink { destination } => {
                    Some(CoordinatorCommand::EstablishLink { destination })
                }
                FetchAction::PrepareAnonymousRequest {
                    destination,
                    link,
                    path,
                } => {
                    let Some(requested_at) = self.requested_at else {
                        self.latch(InvariantFault::MissingTimestamp);
                        return None;
                    };
                    Some(CoordinatorCommand::PrepareAnonymousRequest {
                        destination,
                        link,
                        path,
                        requested_at,
                    })
                }
            };
        }

        if let Some(deadline) = self.establishment_deadline {
            match deadline {
                EstablishmentDeadline::Path(candidate) if now >= candidate.deadline => {
                    return Some(CoordinatorCommand::ExpirePath { candidate });
                }
                EstablishmentDeadline::Link(candidate) if now >= candidate.deadline => {
                    return Some(CoordinatorCommand::AbortTimedOutLink { candidate });
                }
                EstablishmentDeadline::Path(_) | EstablishmentDeadline::Link(_) => {}
            }
        }

        if let Some(candidate) = self.client.request_timeout_candidate(now) {
            let Some(tracked) = self.native_request else {
                self.latch(InvariantFault::MissingNativeRequest);
                return None;
            };
            if tracked.link != candidate.link()
                || tracked.request != candidate.request()
                || tracked.phase != NativeRequestPhase::Confirmed
            {
                self.latch(InvariantFault::CandidateMismatch {
                    operation: CoordinatorOperation::NextCommand,
                });
                return self.next_command(now);
            }
            return Some(CoordinatorCommand::CancelTimedOutRequest {
                token: tracked.token,
                candidate,
            });
        }

        None
    }

    /// Confirm the exact path request's first real interface dispatch.
    ///
    /// The ordinary router owns the action before this callback. Keeping that
    /// pre-dispatch authority outside the coordinator prevents a queued packet
    /// from consuming the path-response timeout or outliving a completed
    /// fetch.
    pub fn path_request_dispatched(
        &mut self,
        destination: DestinationHash,
        dispatched_at: MonotonicMillis,
    ) -> Result<(), InvariantFault> {
        self.ensure_healthy()?;
        match self.establishment_deadline {
            Some(EstablishmentDeadline::Path(candidate))
                if candidate.destination == destination =>
            {
                let result = self.client.confirm_path_request(destination);
                return self.control(CoordinatorOperation::ConfirmPathRequest, result);
            }
            Some(_) => {
                return Err(self.latch(InvariantFault::CandidateMismatch {
                    operation: CoordinatorOperation::ConfirmPathRequest,
                }));
            }
            None => {}
        }
        let result = self.client.confirm_path_request(destination);
        self.control(CoordinatorOperation::ConfirmPathRequest, result)?;
        self.establishment_deadline = Some(EstablishmentDeadline::Path(PathTimeoutCandidate {
            destination,
            deadline: MonotonicMillis::new(
                dispatched_at
                    .get()
                    .saturating_add(self.config.path_timeout_ms),
            ),
        }));
        Ok(())
    }

    /// Consume a path that was already usable without waiting for discovery.
    ///
    /// Call this only before constructing a path-request packet, or after its
    /// external pre-dispatch owner has definitively released that packet.
    pub fn path_already_available(
        &mut self,
        destination: DestinationHash,
    ) -> Result<(), InvariantFault> {
        self.ensure_healthy()?;
        let result = self.client.confirm_path_request(destination);
        self.control(CoordinatorOperation::ConfirmPathRequest, result)?;
        let disposition = self.client.path_available(destination);
        self.expect_observation(CoordinatorOperation::PathAvailable, disposition)
    }

    /// Report one asynchronous path-available observation.
    pub fn path_available(
        &mut self,
        destination: DestinationHash,
    ) -> Result<ObservationDisposition, InvariantFault> {
        self.ensure_healthy()?;
        let disposition = self.client.path_available(destination);
        self.finish_path_observation(
            destination,
            CoordinatorOperation::PathAvailable,
            disposition,
        )
    }

    /// Report one asynchronous path-unavailable observation.
    pub fn path_unavailable(
        &mut self,
        destination: DestinationHash,
    ) -> Result<ObservationDisposition, InvariantFault> {
        self.ensure_healthy()?;
        let disposition = self.client.path_unavailable(destination);
        self.finish_path_observation(
            destination,
            CoordinatorOperation::PathUnavailable,
            disposition,
        )
    }

    /// Commit a due path timeout.
    pub fn confirm_path_timeout(
        &mut self,
        candidate: PathTimeoutCandidate,
    ) -> Result<(), InvariantFault> {
        self.ensure_healthy()?;
        if self.establishment_deadline != Some(EstablishmentDeadline::Path(candidate)) {
            return Err(self.latch(InvariantFault::CandidateMismatch {
                operation: CoordinatorOperation::PathUnavailable,
            }));
        }
        let disposition = self.client.path_unavailable(candidate.destination);
        self.establishment_deadline = None;
        self.expect_observation(CoordinatorOperation::PathUnavailable, disposition)
    }

    /// Report terminal Link preparation failure before a Link identifier exists.
    pub fn link_preparation_failed(
        &mut self,
        destination: DestinationHash,
        failure: LinkFailure,
    ) -> Result<ObservationDisposition, InvariantFault> {
        self.ensure_healthy()?;
        let disposition = self.client.link_preparation_failed(destination, failure);
        if disposition == ObservationDisposition::WrongPhase {
            return Err(self.latch(InvariantFault::ProtocolObservation {
                operation: CoordinatorOperation::LinkPreparationFailed,
                disposition,
            }));
        }
        Ok(disposition)
    }

    /// Confirm the exact Link request's first real interface dispatch.
    ///
    /// The ordinary router and its retained retry slots are the sole owners
    /// before this callback. A definitive pre-dispatch return therefore aborts
    /// the native unestablished Link without advancing this coordinator.
    pub fn link_request_dispatched(
        &mut self,
        destination: DestinationHash,
        link: LinkId,
        dispatched_at: MonotonicMillis,
    ) -> Result<(), InvariantFault> {
        let candidate = LinkTimeoutCandidate {
            destination,
            link,
            deadline: MonotonicMillis::new(
                dispatched_at
                    .get()
                    .saturating_add(self.config.link_timeout_ms),
            ),
        };
        if let Some(fault) = self.fault {
            if self.establishment_deadline.is_none() {
                self.establishment_deadline = Some(EstablishmentDeadline::Link(candidate));
            }
            return Err(fault);
        }
        match self.establishment_deadline {
            Some(EstablishmentDeadline::Link(candidate))
                if candidate.destination == destination && candidate.link == link =>
            {
                let result = self.client.confirm_link_request(destination, link);
                return self.control(CoordinatorOperation::ConfirmLinkRequest, result);
            }
            Some(_) => {
                return Err(self.latch(InvariantFault::CandidateMismatch {
                    operation: CoordinatorOperation::ConfirmLinkRequest,
                }));
            }
            None => {}
        }
        self.establishment_deadline = Some(EstablishmentDeadline::Link(candidate));
        let result = self.client.confirm_link_request(destination, link);
        self.control(CoordinatorOperation::ConfirmLinkRequest, result)?;
        Ok(())
    }

    /// Report one asynchronous exact Link-established observation.
    pub fn link_established(
        &mut self,
        link: LinkId,
    ) -> Result<ObservationDisposition, InvariantFault> {
        let Some(destination) = self.establishing_destination(link) else {
            return self
                .ensure_healthy()
                .map(|()| ObservationDisposition::Unrelated);
        };
        if let Some(fault) = self.fault {
            self.establishment_deadline = None;
            return Err(fault);
        }
        let disposition = self.client.link_established(destination, link);
        self.finish_link_observation(
            destination,
            link,
            CoordinatorOperation::LinkEstablished,
            disposition,
        )
    }

    /// Report one asynchronous exact Link failure.
    pub fn link_failed(
        &mut self,
        link: LinkId,
        failure: LinkFailure,
    ) -> Result<ObservationDisposition, InvariantFault> {
        let Some(destination) = self.establishing_destination(link) else {
            return self
                .ensure_healthy()
                .map(|()| ObservationDisposition::Unrelated);
        };
        if let Some(fault) = self.fault {
            self.establishment_deadline = None;
            return Err(fault);
        }
        let disposition = self.client.link_failed(destination, link, failure);
        self.finish_link_observation(
            destination,
            link,
            CoordinatorOperation::LinkFailed,
            disposition,
        )
    }

    /// Commit Link establishment timeout after exact native abort.
    ///
    /// An exact callback still releases retained cleanup authority after a
    /// sticky fault, while returning that original fault.
    pub fn confirm_link_timeout_after_abort(
        &mut self,
        candidate: LinkTimeoutCandidate,
        code: u16,
    ) -> Result<(), InvariantFault> {
        if let Some(fault) = self.fault {
            if self.establishment_deadline == Some(EstablishmentDeadline::Link(candidate)) {
                self.establishment_deadline = None;
            }
            return Err(fault);
        }
        if self.establishment_deadline != Some(EstablishmentDeadline::Link(candidate)) {
            return Err(self.latch(InvariantFault::CandidateMismatch {
                operation: CoordinatorOperation::LinkFailed,
            }));
        }
        let disposition = self.client.link_failed(
            candidate.destination,
            candidate.link,
            LinkFailure::new(LinkFailureStage::Establishment, code),
        );
        self.establishment_deadline = None;
        self.expect_observation(CoordinatorOperation::LinkFailed, disposition)
    }

    /// Report that an established or establishing Link closed.
    ///
    /// RNS has already reclaimed any request authority tied to this Link before
    /// projecting the application event, so exact retained request correlation
    /// is released here without an additional cancellation command. This
    /// reconciliation still occurs after a sticky fault.
    pub fn link_closed(
        &mut self,
        link: LinkId,
        code: u16,
    ) -> Result<ObservationDisposition, InvariantFault> {
        let owned_request = self
            .native_request
            .is_some_and(|tracked| tracked.link == link);
        let owned_link = matches!(
            self.establishment_deadline,
            Some(EstablishmentDeadline::Link(candidate)) if candidate.link == link
        );
        if let Some(fault) = self.fault {
            if owned_request {
                self.native_request = None;
            }
            if owned_link {
                self.establishment_deadline = None;
            }
            return Err(fault);
        }
        let disposition = self.client.link_closed(link, code);
        if owned_request {
            self.native_request = None;
        }
        if owned_link {
            self.establishment_deadline = None;
        }
        if (owned_request || owned_link) && disposition != ObservationDisposition::Applied {
            return Err(self.latch(InvariantFault::ProtocolObservation {
                operation: CoordinatorOperation::LinkClosed,
                disposition,
            }));
        }
        Ok(disposition)
    }

    /// Report that request preparation found its cached Link missing or inactive.
    pub fn request_link_unavailable(
        &mut self,
        link: LinkId,
        code: u16,
    ) -> Result<ObservationDisposition, InvariantFault> {
        self.ensure_healthy()?;
        let disposition = self.client.request_link_unavailable(link, code);
        self.require_applied_or_unrelated(CoordinatorOperation::RequestLinkUnavailable, disposition)
    }

    /// Report terminal request preparation failure before an ID existed.
    pub fn request_preparation_failed(
        &mut self,
        link: LinkId,
        failure: RequestFailure,
    ) -> Result<ObservationDisposition, InvariantFault> {
        self.ensure_healthy()?;
        let disposition = self.client.request_preparation_failed(link, failure);
        self.require_applied_or_unrelated(
            CoordinatorOperation::RequestPreparationFailed,
            disposition,
        )
    }

    /// Bind the exact native request token after packet preparation.
    ///
    /// If protocol correlation rejects this callback, the token remains owned
    /// and [`Self::next_command`] emits `CancelRequestForInvariant`.
    pub fn request_prepared(
        &mut self,
        link: LinkId,
        request: RequestId,
        token: Token,
    ) -> Result<PreparedRequest, InvariantFault> {
        self.ensure_healthy()?;
        if self.native_request.is_some() {
            return Err(self.latch(InvariantFault::RequestAlreadyTracked));
        }
        self.native_request = Some(NativeRequest {
            token,
            link,
            request,
            phase: NativeRequestPhase::Prepared,
        });
        match self.client.request_prepared(link, request) {
            Ok(prepared) => Ok(prepared),
            Err(error) => Err(self.latch(InvariantFault::ProtocolControl {
                operation: CoordinatorOperation::RequestPrepared,
                error,
            })),
        }
    }

    /// Return an undispatched request to preparation after exact native
    /// cancellation.
    ///
    /// An exact callback still releases retained cleanup authority after a
    /// sticky fault, while returning that original fault.
    pub fn request_dispatch_canceled_after_native_cancel(
        &mut self,
        token: Token,
    ) -> Result<(), InvariantFault> {
        if let Some(fault) = self.reconcile_faulted_request_cancellation(token) {
            return Err(fault);
        }
        let tracked = self.take_request_after_native_cancel(
            token,
            NativeRequestPhase::Prepared,
            CoordinatorOperation::CancelRequestDispatch,
        )?;
        let result = self
            .client
            .cancel_request_dispatch(tracked.link, tracked.request);
        self.control(CoordinatorOperation::CancelRequestDispatch, result)
    }

    /// Report terminal dispatch failure after exact native request cancellation.
    ///
    /// An exact callback still releases retained cleanup authority after a
    /// sticky fault, while returning that original fault.
    pub fn request_dispatch_failed_after_native_cancel(
        &mut self,
        token: Token,
        failure: RequestFailure,
    ) -> Result<(), InvariantFault> {
        if let Some(fault) = self.reconcile_faulted_request_cancellation(token) {
            return Err(fault);
        }
        let tracked = self.take_request_after_native_cancel(
            token,
            NativeRequestPhase::Prepared,
            CoordinatorOperation::RequestDispatchFailed,
        )?;
        let disposition =
            self.client
                .request_dispatch_failed(tracked.link, tracked.request, failure);
        self.expect_observation(CoordinatorOperation::RequestDispatchFailed, disposition)
    }

    /// Confirm the first real native request dispatch.
    ///
    /// The native adapter must have committed its own dispatch transition
    /// before calling this method. A protocol rejection retains the token in
    /// confirmed phase so invariant cleanup can cancel it exactly.
    /// After a prior sticky fault, an exact callback still records the native
    /// confirmed phase before returning that fault.
    pub fn request_dispatch_confirmed(
        &mut self,
        token: Token,
        dispatched_at: MonotonicMillis,
    ) -> Result<(), InvariantFault> {
        if let Some(fault) = self.fault {
            if let Some(tracked) = self.native_request
                && tracked.token == token
                && tracked.phase == NativeRequestPhase::Prepared
            {
                self.native_request = Some(NativeRequest {
                    phase: NativeRequestPhase::Confirmed,
                    ..tracked
                });
            }
            return Err(fault);
        }
        let tracked = self.require_request(
            token,
            NativeRequestPhase::Prepared,
            CoordinatorOperation::ConfirmRequestDispatch,
        )?;
        self.native_request = Some(NativeRequest {
            phase: NativeRequestPhase::Confirmed,
            ..tracked
        });
        let result =
            self.client
                .confirm_request_dispatch(tracked.link, tracked.request, dispatched_at);
        self.control(CoordinatorOperation::ConfirmRequestDispatch, result)
    }

    /// Report a remote request failure after RNS reclaimed native authority.
    ///
    /// Exact native correlation is released even after a sticky fault.
    pub fn request_failed(
        &mut self,
        link: LinkId,
        request: RequestId,
        failure: RequestFailure,
    ) -> Result<ObservationDisposition, InvariantFault> {
        let exact = self.request_is_exact(link, request);
        if let Some(fault) = self.fault {
            if exact {
                self.native_request = None;
            }
            return Err(fault);
        }
        let disposition = self.client.request_failed(link, request, failure);
        if exact {
            self.native_request = None;
            self.expect_observation(CoordinatorOperation::RequestFailed, disposition)?;
        }
        Ok(disposition)
    }

    /// Report a decoded response body after RNS reclaimed native authority.
    ///
    /// Exact native correlation is released even after a sticky fault.
    pub fn response_received(
        &mut self,
        link: LinkId,
        request: RequestId,
        response_body: &[u8],
    ) -> Result<ObservationDisposition, InvariantFault> {
        let exact = self.request_is_exact(link, request);
        if let Some(fault) = self.fault {
            if exact {
                self.native_request = None;
            }
            return Err(fault);
        }
        let disposition = self.client.response_received(link, request, response_body);
        if exact {
            self.native_request = None;
            self.expect_observation(CoordinatorOperation::ResponseReceived, disposition)?;
        }
        Ok(disposition)
    }

    /// Commit a due request timeout after exact native cancellation.
    ///
    /// An exact callback still releases retained cleanup authority after a
    /// sticky fault, while returning that original fault.
    pub fn confirm_request_timeout_after_native_cancel(
        &mut self,
        token: Token,
        candidate: RequestTimeoutCandidate,
    ) -> Result<(), InvariantFault> {
        if let Some(fault) = self.reconcile_faulted_request_cancellation(token) {
            return Err(fault);
        }
        let tracked = self.take_request_after_native_cancel(
            token,
            NativeRequestPhase::Confirmed,
            CoordinatorOperation::ConfirmRequestTimeout,
        )?;
        if tracked.link != candidate.link() || tracked.request != candidate.request() {
            return Err(self.latch(InvariantFault::CandidateMismatch {
                operation: CoordinatorOperation::ConfirmRequestTimeout,
            }));
        }
        let result = self.client.confirm_request_timeout(candidate);
        self.control(CoordinatorOperation::ConfirmRequestTimeout, result)
    }

    /// Acknowledge exact Link abort emitted solely to clean up after a sticky
    /// fault.
    ///
    /// The invariant fault deliberately remains sticky.
    pub fn acknowledge_invariant_link_abort(
        &mut self,
        candidate: LinkTimeoutCandidate,
    ) -> Result<(), InvariantFault> {
        if self.establishment_deadline != Some(EstablishmentDeadline::Link(candidate)) {
            return Err(self.fault.unwrap_or(InvariantFault::CandidateMismatch {
                operation: CoordinatorOperation::ConfirmLinkRequest,
            }));
        }
        self.establishment_deadline = None;
        Ok(())
    }

    /// Acknowledge cancellation emitted solely to clean up after a sticky fault.
    ///
    /// The fault deliberately remains sticky. This method only proves that no
    /// native request authority was leaked.
    pub fn acknowledge_invariant_request_cancellation(
        &mut self,
        token: Token,
    ) -> Result<(), InvariantFault> {
        let Some(tracked) = self.native_request else {
            return self.ensure_healthy();
        };
        if tracked.token != token {
            return Err(self.fault.unwrap_or(InvariantFault::RequestTokenMismatch {
                operation: CoordinatorOperation::CancelRequestDispatch,
            }));
        }
        self.native_request = None;
        Ok(())
    }

    /// Take one terminal result and release its timestamp slot.
    pub fn take_outcome(&mut self) -> Result<Option<FetchOutcome>, InvariantFault> {
        self.ensure_healthy()?;
        if (self.client.ready_page().is_some() || self.client.failure().is_some())
            && (self.native_request.is_some() || self.establishment_deadline.is_some())
        {
            return Err(self.latch(InvariantFault::OutcomeStillOwned));
        }
        let outcome = self.client.take_outcome();
        if outcome.is_some() {
            self.requested_at = None;
        }
        Ok(outcome)
    }

    fn request_is_exact(&self, link: LinkId, request: RequestId) -> bool {
        self.native_request
            .is_some_and(|tracked| tracked.link == link && tracked.request == request)
    }

    fn establishing_destination(&self, link: LinkId) -> Option<DestinationHash> {
        match self.establishment_deadline {
            Some(EstablishmentDeadline::Link(candidate)) if candidate.link == link => {
                Some(candidate.destination)
            }
            Some(EstablishmentDeadline::Path(_) | EstablishmentDeadline::Link(_)) | None => None,
        }
    }

    fn reconcile_faulted_request_cancellation(&mut self, token: Token) -> Option<InvariantFault> {
        let fault = self.fault?;
        if self
            .native_request
            .is_some_and(|tracked| tracked.token == token)
        {
            self.native_request = None;
        }
        Some(fault)
    }

    fn require_request(
        &mut self,
        token: Token,
        phase: NativeRequestPhase,
        operation: CoordinatorOperation,
    ) -> Result<NativeRequest<Token>, InvariantFault> {
        let Some(tracked) = self.native_request else {
            return Err(self.latch(InvariantFault::MissingNativeRequest));
        };
        if tracked.token != token {
            return Err(self.latch(InvariantFault::RequestTokenMismatch { operation }));
        }
        if tracked.phase != phase {
            return Err(self.latch(InvariantFault::RequestPhaseMismatch {
                operation,
                expected: phase,
                actual: tracked.phase,
            }));
        }
        Ok(tracked)
    }

    fn take_request_after_native_cancel(
        &mut self,
        token: Token,
        phase: NativeRequestPhase,
        operation: CoordinatorOperation,
    ) -> Result<NativeRequest<Token>, InvariantFault> {
        let Some(tracked) = self.native_request else {
            return Err(self.latch(InvariantFault::MissingNativeRequest));
        };
        if tracked.token != token {
            return Err(self.latch(InvariantFault::RequestTokenMismatch { operation }));
        }
        self.native_request = None;
        if tracked.phase != phase {
            return Err(self.latch(InvariantFault::RequestPhaseMismatch {
                operation,
                expected: phase,
                actual: tracked.phase,
            }));
        }
        Ok(tracked)
    }

    fn finish_path_observation(
        &mut self,
        destination: DestinationHash,
        operation: CoordinatorOperation,
        disposition: ObservationDisposition,
    ) -> Result<ObservationDisposition, InvariantFault> {
        let owned = matches!(
            self.establishment_deadline,
            Some(EstablishmentDeadline::Path(candidate))
                if candidate.destination == destination
        );
        if disposition == ObservationDisposition::Applied {
            if owned {
                self.establishment_deadline = None;
            }
        } else if owned {
            return Err(self.latch(InvariantFault::ProtocolObservation {
                operation,
                disposition,
            }));
        }
        Ok(disposition)
    }

    fn finish_link_observation(
        &mut self,
        destination: DestinationHash,
        link: LinkId,
        operation: CoordinatorOperation,
        disposition: ObservationDisposition,
    ) -> Result<ObservationDisposition, InvariantFault> {
        let owned = matches!(
            self.establishment_deadline,
            Some(EstablishmentDeadline::Link(candidate))
                if candidate.destination == destination && candidate.link == link
        );
        if disposition == ObservationDisposition::Applied {
            if owned {
                self.establishment_deadline = None;
            }
        } else if owned {
            return Err(self.latch(InvariantFault::ProtocolObservation {
                operation,
                disposition,
            }));
        }
        Ok(disposition)
    }

    fn require_applied_or_unrelated(
        &mut self,
        operation: CoordinatorOperation,
        disposition: ObservationDisposition,
    ) -> Result<ObservationDisposition, InvariantFault> {
        if disposition == ObservationDisposition::WrongPhase {
            Err(self.latch(InvariantFault::ProtocolObservation {
                operation,
                disposition,
            }))
        } else {
            Ok(disposition)
        }
    }

    fn control(
        &mut self,
        operation: CoordinatorOperation,
        result: Result<(), ControlError>,
    ) -> Result<(), InvariantFault> {
        result.map_err(|error| self.latch(InvariantFault::ProtocolControl { operation, error }))
    }

    fn expect_observation(
        &mut self,
        operation: CoordinatorOperation,
        disposition: ObservationDisposition,
    ) -> Result<(), InvariantFault> {
        if disposition == ObservationDisposition::Applied {
            Ok(())
        } else {
            Err(self.latch(InvariantFault::ProtocolObservation {
                operation,
                disposition,
            }))
        }
    }

    fn ensure_healthy(&self) -> Result<(), InvariantFault> {
        self.fault.map_or(Ok(()), Err)
    }

    fn latch(&mut self, fault: InvariantFault) -> InvariantFault {
        if self.fault.is_none() {
            self.fault = Some(fault);
        }
        self.fault.expect("fault was just populated")
    }
}

const _: () = assert!(
    size_of::<NomadCoordinator<[u64; MAX_NATIVE_REQUEST_TOKEN_BYTES / size_of::<u64>()]>>()
        <= NOMAD_COORDINATOR_RAM_CEILING
);

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_nomad_protocol::{FetchFailure, LinkFailureStage, RequestFailureStage};

    const DESTINATION: DestinationHash = DestinationHash::new([0x11; 16]);
    const OTHER_DESTINATION: DestinationHash = DestinationHash::new([0x22; 16]);
    const LINK: LinkId = LinkId::new([0x33; 16]);
    const OTHER_LINK: LinkId = LinkId::new([0x44; 16]);
    const REQUEST: RequestId = RequestId::new([0x55; 16]);
    const OTHER_REQUEST: RequestId = RequestId::new([0x66; 16]);
    const TOKEN: u32 = 7;
    const OTHER_TOKEN: u32 = 8;
    const TIMESTAMP_MS: u64 = 1_784_732_100_123;

    fn config() -> NomadCoordinatorConfig {
        NomadCoordinatorConfig::new(100, 20, 30).unwrap()
    }

    fn coordinator() -> NomadCoordinator<u32> {
        NomadCoordinator::new(config())
    }

    fn advance_to_request(coordinator: &mut NomadCoordinator<u32>) {
        coordinator
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        assert_eq!(
            coordinator.next_command(MonotonicMillis::new(0)),
            Some(CoordinatorCommand::RequestPath {
                destination: DESTINATION
            })
        );
        coordinator
            .path_request_dispatched(DESTINATION, MonotonicMillis::new(10))
            .unwrap();
        assert_eq!(
            coordinator.path_available(DESTINATION).unwrap(),
            ObservationDisposition::Applied
        );
        coordinator
            .link_request_dispatched(DESTINATION, LINK, MonotonicMillis::new(20))
            .unwrap();
        assert_eq!(
            coordinator.link_established(LINK).unwrap(),
            ObservationDisposition::Applied
        );
    }

    fn advance_to_prepared(coordinator: &mut NomadCoordinator<u32>) {
        advance_to_request(coordinator);
        coordinator.request_prepared(LINK, REQUEST, TOKEN).unwrap();
    }

    #[test]
    fn coordinator_and_maximum_token_remain_below_reviewed_ram_ceiling() {
        type MaximumCoordinator =
            NomadCoordinator<[u64; MAX_NATIVE_REQUEST_TOKEN_BYTES / size_of::<u64>()]>;
        assert!(size_of::<MaximumCoordinator>() <= NOMAD_COORDINATOR_RAM_CEILING);
        assert!(size_of::<NomadCoordinator<u32>>() <= NOMAD_COORDINATOR_RAM_CEILING);
        let _ = MaximumCoordinator::new(config());
    }

    #[test]
    fn client_timestamp_is_validated_retained_and_converted_once_at_command_boundary() {
        assert_eq!(
            coordinator().start(DESTINATION, PagePath::index(), 0),
            Err(CoordinatorStartError::InvalidTimestamp {
                actual: 0,
                maximum: MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS,
            })
        );
        assert_eq!(
            coordinator().start(
                DESTINATION,
                PagePath::index(),
                MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS + 1
            ),
            Err(CoordinatorStartError::InvalidTimestamp {
                actual: MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS + 1,
                maximum: MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS,
            })
        );

        let timestamp = UnixTimestampMillis::new(TIMESTAMP_MS).unwrap();
        assert_eq!(timestamp.get(), TIMESTAMP_MS);
        assert_eq!(timestamp.as_seconds_f64(), 1_784_732_100.123);

        let mut coordinator = coordinator();
        advance_to_request(&mut coordinator);
        assert_eq!(coordinator.requested_at(), Some(timestamp));
        assert_eq!(
            coordinator.next_command(MonotonicMillis::new(21)),
            Some(CoordinatorCommand::PrepareAnonymousRequest {
                destination: DESTINATION,
                link: LINK,
                path: PagePath::index(),
                requested_at: timestamp,
            })
        );
    }

    #[test]
    fn full_fetch_is_exact_bounded_and_reuses_cached_link() {
        let mut coordinator = coordinator();
        advance_to_prepared(&mut coordinator);
        coordinator
            .request_dispatch_confirmed(TOKEN, MonotonicMillis::new(100))
            .unwrap();
        assert_eq!(coordinator.phase(), FetchPhase::AwaitingResponse);
        assert_eq!(
            coordinator.response_received(LINK, REQUEST, b"Hello, Micron"),
            Ok(ObservationDisposition::Applied)
        );
        assert_eq!(
            coordinator.ready_page().unwrap().as_str().unwrap(),
            "Hello, Micron"
        );
        let outcome = coordinator.take_outcome().unwrap().unwrap();
        assert!(matches!(outcome, FetchOutcome::Ready(_)));
        assert_eq!(coordinator.requested_at(), None);
        assert_eq!(
            coordinator.cached_link(),
            Some(CachedLink::new(DESTINATION, LINK))
        );

        coordinator
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS + 1)
            .unwrap();
        assert!(matches!(
            coordinator.next_command(MonotonicMillis::new(201)),
            Some(CoordinatorCommand::PrepareAnonymousRequest {
                destination: DESTINATION,
                link: LINK,
                ..
            })
        ));
    }

    #[test]
    fn path_timeout_is_stable_and_commits_without_native_cancellation() {
        let mut coordinator = coordinator();
        coordinator
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        coordinator
            .path_request_dispatched(DESTINATION, MonotonicMillis::new(10))
            .unwrap();
        assert_eq!(coordinator.next_command(MonotonicMillis::new(29)), None);
        let command = coordinator.next_command(MonotonicMillis::new(30)).unwrap();
        let CoordinatorCommand::ExpirePath { candidate } = command else {
            panic!("expected path timeout");
        };
        assert_eq!(candidate.destination(), DESTINATION);
        assert_eq!(candidate.deadline(), MonotonicMillis::new(30));
        assert_eq!(
            coordinator.next_command(MonotonicMillis::new(31)),
            Some(command)
        );
        coordinator.confirm_path_timeout(candidate).unwrap();
        assert_eq!(
            coordinator.failure(),
            Some(FetchFailure::NoPath {
                destination: DESTINATION
            })
        );
    }

    #[test]
    fn link_timeout_requires_exact_abort_confirmation() {
        let mut coordinator = coordinator();
        coordinator
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        coordinator.path_already_available(DESTINATION).unwrap();
        assert_eq!(
            coordinator.next_command(MonotonicMillis::new(u64::MAX)),
            Some(CoordinatorCommand::EstablishLink {
                destination: DESTINATION,
            }),
            "a retained pre-dispatch Link action is owned outside the coordinator"
        );
        coordinator
            .link_request_dispatched(DESTINATION, LINK, MonotonicMillis::new(20))
            .unwrap();
        coordinator
            .link_request_dispatched(DESTINATION, LINK, MonotonicMillis::new(1_000))
            .unwrap();
        assert_eq!(coordinator.next_command(MonotonicMillis::new(49)), None);
        let Some(CoordinatorCommand::AbortTimedOutLink { candidate }) =
            coordinator.next_command(MonotonicMillis::new(50))
        else {
            panic!("expected Link timeout");
        };
        assert_eq!(candidate.destination(), DESTINATION);
        assert_eq!(candidate.link(), LINK);
        assert_eq!(candidate.deadline(), MonotonicMillis::new(50));
        coordinator
            .confirm_link_timeout_after_abort(candidate, 17)
            .unwrap();
        assert!(matches!(
            coordinator.failure(),
            Some(FetchFailure::Link {
                destination: DESTINATION,
                failure,
            }) if failure.stage() == LinkFailureStage::Establishment && failure.code() == 17
        ));
    }

    #[test]
    fn request_timeout_is_a_two_owner_transaction() {
        let mut coordinator = coordinator();
        advance_to_prepared(&mut coordinator);
        coordinator
            .request_dispatch_confirmed(TOKEN, MonotonicMillis::new(100))
            .unwrap();
        assert_eq!(coordinator.next_command(MonotonicMillis::new(199)), None);
        let command = coordinator.next_command(MonotonicMillis::new(200)).unwrap();
        let CoordinatorCommand::CancelTimedOutRequest { token, candidate } = command else {
            panic!("expected request timeout");
        };
        assert_eq!(token, TOKEN);
        assert_eq!(candidate.link(), LINK);
        assert_eq!(candidate.request(), REQUEST);
        assert_eq!(
            coordinator.next_command(MonotonicMillis::new(201)),
            Some(command)
        );
        coordinator
            .confirm_request_timeout_after_native_cancel(token, candidate)
            .unwrap();
        assert!(matches!(
            coordinator.failure(),
            Some(FetchFailure::Timeout {
                link: LINK,
                request: REQUEST,
                ..
            })
        ));
    }

    #[test]
    fn undispatched_request_can_retry_or_fail_only_after_native_cancel() {
        let mut retry = coordinator();
        advance_to_prepared(&mut retry);
        retry
            .request_dispatch_canceled_after_native_cancel(TOKEN)
            .unwrap();
        assert!(matches!(
            retry.next_command(MonotonicMillis::new(50)),
            Some(CoordinatorCommand::PrepareAnonymousRequest {
                destination: DESTINATION,
                link: LINK,
                ..
            })
        ));

        retry.request_prepared(LINK, REQUEST, TOKEN).unwrap();
        retry
            .request_dispatch_failed_after_native_cancel(
                TOKEN,
                RequestFailure::new(RequestFailureStage::Dispatch, 8),
            )
            .unwrap();
        assert!(matches!(
            retry.failure(),
            Some(FetchFailure::Request {
                link: LINK,
                request: Some(REQUEST),
                failure,
            }) if failure.stage() == RequestFailureStage::Dispatch && failure.code() == 8
        ));
    }

    #[test]
    fn remote_terminals_release_exact_native_correlation_only() {
        let mut coordinator = coordinator();
        advance_to_prepared(&mut coordinator);
        coordinator
            .request_dispatch_confirmed(TOKEN, MonotonicMillis::new(100))
            .unwrap();
        assert_eq!(
            coordinator
                .request_failed(
                    OTHER_LINK,
                    OTHER_REQUEST,
                    RequestFailure::new(RequestFailureStage::Remote, 2)
                )
                .unwrap(),
            ObservationDisposition::Unrelated
        );
        assert_eq!(
            coordinator
                .request_failed(
                    LINK,
                    REQUEST,
                    RequestFailure::new(RequestFailureStage::Remote, 3)
                )
                .unwrap(),
            ObservationDisposition::Applied
        );
        assert!(matches!(
            coordinator.failure(),
            Some(FetchFailure::Request {
                request: Some(REQUEST),
                failure,
                ..
            }) if failure.code() == 3
        ));
    }

    #[test]
    fn exact_link_close_reclaims_prepared_request_without_second_cancel() {
        let mut coordinator = coordinator();
        advance_to_prepared(&mut coordinator);
        assert_eq!(
            coordinator.link_closed(LINK, 9).unwrap(),
            ObservationDisposition::Applied
        );
        assert!(matches!(
            coordinator.failure(),
            Some(FetchFailure::Link {
                failure,
                ..
            }) if failure.stage() == LinkFailureStage::Closed && failure.code() == 9
        ));
        assert_eq!(coordinator.next_command(MonotonicMillis::new(500)), None);
    }

    #[test]
    fn first_invariant_fault_is_sticky_and_preserves_exact_cleanup() {
        let mut coordinator = coordinator();
        advance_to_prepared(&mut coordinator);
        let error = coordinator
            .request_dispatch_confirmed(OTHER_TOKEN, MonotonicMillis::new(100))
            .unwrap_err();
        assert_eq!(
            error,
            InvariantFault::RequestTokenMismatch {
                operation: CoordinatorOperation::ConfirmRequestDispatch
            }
        );
        assert_eq!(coordinator.fault(), Some(error));
        let Some(CoordinatorCommand::CancelRequestForInvariant {
            token,
            link,
            request,
            phase,
        }) = coordinator.next_command(MonotonicMillis::new(101))
        else {
            panic!("expected exact invariant cleanup");
        };
        assert_eq!(
            (token, link, request, phase),
            (TOKEN, LINK, REQUEST, NativeRequestPhase::Prepared)
        );
        coordinator
            .acknowledge_invariant_request_cancellation(TOKEN)
            .unwrap();
        assert_eq!(coordinator.next_command(MonotonicMillis::new(102)), None);
        assert_eq!(coordinator.fault(), Some(error));
        assert_eq!(
            coordinator.start(OTHER_DESTINATION, PagePath::index(), TIMESTAMP_MS),
            Err(CoordinatorStartError::Faulted(error))
        );
    }

    #[test]
    fn prepared_protocol_rejection_still_emits_native_rollback() {
        let mut coordinator = coordinator();
        advance_to_request(&mut coordinator);
        let error = coordinator
            .request_prepared(OTHER_LINK, REQUEST, TOKEN)
            .unwrap_err();
        assert!(matches!(
            error,
            InvariantFault::ProtocolControl {
                operation: CoordinatorOperation::RequestPrepared,
                error: ControlError::LinkMismatch,
            }
        ));
        assert_eq!(
            coordinator.next_command(MonotonicMillis::new(30)),
            Some(CoordinatorCommand::CancelRequestForInvariant {
                token: TOKEN,
                link: OTHER_LINK,
                request: REQUEST,
                phase: NativeRequestPhase::Prepared,
            })
        );
    }

    #[test]
    fn link_protocol_rejection_still_emits_exact_abort() {
        let mut coordinator = coordinator();
        coordinator
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        let error = coordinator
            .link_request_dispatched(DESTINATION, LINK, MonotonicMillis::new(10))
            .unwrap_err();
        assert!(matches!(
            error,
            InvariantFault::ProtocolControl {
                operation: CoordinatorOperation::ConfirmLinkRequest,
                error: ControlError::WrongPhase,
            }
        ));
        let Some(CoordinatorCommand::AbortLinkForInvariant { candidate }) =
            coordinator.next_command(MonotonicMillis::new(11))
        else {
            panic!("expected exact Link rollback");
        };
        assert_eq!(candidate.destination(), DESTINATION);
        assert_eq!(candidate.link(), LINK);
        coordinator
            .acknowledge_invariant_link_abort(candidate)
            .unwrap();
        assert_eq!(coordinator.next_command(MonotonicMillis::new(12)), None);
        assert_eq!(coordinator.fault(), Some(error));
    }

    #[test]
    fn exact_native_cancel_releases_token_even_when_product_phase_is_wrong() {
        let mut coordinator = coordinator();
        advance_to_prepared(&mut coordinator);
        coordinator
            .request_dispatch_confirmed(TOKEN, MonotonicMillis::new(100))
            .unwrap();
        let error = coordinator
            .request_dispatch_canceled_after_native_cancel(TOKEN)
            .unwrap_err();
        assert_eq!(
            error,
            InvariantFault::RequestPhaseMismatch {
                operation: CoordinatorOperation::CancelRequestDispatch,
                expected: NativeRequestPhase::Prepared,
                actual: NativeRequestPhase::Confirmed,
            }
        );
        assert_eq!(coordinator.next_command(MonotonicMillis::new(101)), None);
    }

    #[test]
    fn fault_then_native_request_dispatch_reconciles_the_cleanup_phase() {
        let mut coordinator = coordinator();
        advance_to_prepared(&mut coordinator);
        let fault = coordinator
            .request_preparation_failed(
                LINK,
                RequestFailure::new(RequestFailureStage::Preparation, 41),
            )
            .unwrap_err();
        assert!(matches!(
            fault,
            InvariantFault::ProtocolObservation {
                operation: CoordinatorOperation::RequestPreparationFailed,
                disposition: ObservationDisposition::WrongPhase,
            }
        ));

        assert_eq!(
            coordinator.request_dispatch_confirmed(OTHER_TOKEN, MonotonicMillis::new(100),),
            Err(fault)
        );
        assert!(matches!(
            coordinator.next_command(MonotonicMillis::new(101)),
            Some(CoordinatorCommand::CancelRequestForInvariant {
                token: TOKEN,
                phase: NativeRequestPhase::Prepared,
                ..
            })
        ));

        assert_eq!(
            coordinator.request_dispatch_confirmed(TOKEN, MonotonicMillis::new(100)),
            Err(fault)
        );
        assert!(matches!(
            coordinator.next_command(MonotonicMillis::new(101)),
            Some(CoordinatorCommand::CancelRequestForInvariant {
                token: TOKEN,
                phase: NativeRequestPhase::Confirmed,
                ..
            })
        ));
        coordinator
            .acknowledge_invariant_request_cancellation(TOKEN)
            .unwrap();
        assert_eq!(coordinator.next_command(MonotonicMillis::new(102)), None);
        assert_eq!(coordinator.fault(), Some(fault));
    }

    #[test]
    fn fault_then_response_reclaims_only_the_exact_native_request() {
        let mut coordinator = coordinator();
        advance_to_prepared(&mut coordinator);
        coordinator
            .request_dispatch_confirmed(TOKEN, MonotonicMillis::new(100))
            .unwrap();
        let fault = coordinator
            .request_preparation_failed(
                LINK,
                RequestFailure::new(RequestFailureStage::Preparation, 42),
            )
            .unwrap_err();

        assert_eq!(
            coordinator.response_received(OTHER_LINK, OTHER_REQUEST, b"stale"),
            Err(fault)
        );
        assert!(matches!(
            coordinator.next_command(MonotonicMillis::new(101)),
            Some(CoordinatorCommand::CancelRequestForInvariant {
                token: TOKEN,
                phase: NativeRequestPhase::Confirmed,
                ..
            })
        ));

        assert_eq!(
            coordinator.response_received(LINK, REQUEST, b"terminal"),
            Err(fault)
        );
        assert_eq!(coordinator.next_command(MonotonicMillis::new(102)), None);
        assert_eq!(coordinator.phase(), FetchPhase::AwaitingResponse);
        assert!(coordinator.ready_page().is_none());
        assert_eq!(coordinator.fault(), Some(fault));
    }

    #[test]
    fn fault_then_link_close_reclaims_only_exact_native_authority() {
        let mut coordinator = coordinator();
        advance_to_prepared(&mut coordinator);
        let fault = coordinator
            .request_preparation_failed(
                LINK,
                RequestFailure::new(RequestFailureStage::Preparation, 43),
            )
            .unwrap_err();

        assert_eq!(coordinator.link_closed(OTHER_LINK, 7), Err(fault));
        assert!(matches!(
            coordinator.next_command(MonotonicMillis::new(100)),
            Some(CoordinatorCommand::CancelRequestForInvariant {
                token: TOKEN,
                phase: NativeRequestPhase::Prepared,
                ..
            })
        ));

        assert_eq!(coordinator.link_closed(LINK, 8), Err(fault));
        assert_eq!(coordinator.next_command(MonotonicMillis::new(101)), None);
        assert_eq!(
            coordinator.phase(),
            FetchPhase::AwaitingDispatchConfirmation
        );
        assert_eq!(coordinator.fault(), Some(fault));
    }

    #[test]
    fn fault_then_link_establishment_releases_unestablished_cleanup_authority() {
        let mut coordinator = coordinator();
        coordinator
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        coordinator.path_already_available(DESTINATION).unwrap();
        let fault = coordinator
            .request_preparation_failed(
                LINK,
                RequestFailure::new(RequestFailureStage::Preparation, 44),
            )
            .unwrap_err();

        assert_eq!(
            coordinator.link_request_dispatched(DESTINATION, LINK, MonotonicMillis::new(20),),
            Err(fault)
        );
        assert!(matches!(
            coordinator.next_command(MonotonicMillis::new(21)),
            Some(CoordinatorCommand::AbortLinkForInvariant { candidate })
                if candidate.link() == LINK
        ));

        assert_eq!(coordinator.link_established(OTHER_LINK), Err(fault));
        assert!(matches!(
            coordinator.next_command(MonotonicMillis::new(22)),
            Some(CoordinatorCommand::AbortLinkForInvariant { candidate })
                if candidate.link() == LINK
        ));

        assert_eq!(coordinator.link_established(LINK), Err(fault));
        assert_eq!(coordinator.next_command(MonotonicMillis::new(23)), None);
        assert_eq!(coordinator.phase(), FetchPhase::LinkEstablishment);
        assert_eq!(coordinator.fault(), Some(fault));
    }

    #[test]
    fn every_native_cancel_callback_reconciles_exact_authority_after_fault() {
        let request_failure = RequestFailure::new(RequestFailureStage::Preparation, 45);

        let mut link_aborted = coordinator();
        link_aborted
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        link_aborted.path_already_available(DESTINATION).unwrap();
        link_aborted
            .link_request_dispatched(DESTINATION, LINK, MonotonicMillis::new(20))
            .unwrap();
        let Some(CoordinatorCommand::AbortTimedOutLink { candidate }) =
            link_aborted.next_command(MonotonicMillis::new(50))
        else {
            panic!("expected exact Link timeout candidate");
        };
        let link_fault = link_aborted
            .request_preparation_failed(LINK, request_failure)
            .unwrap_err();
        let foreign_candidate = LinkTimeoutCandidate {
            destination: DESTINATION,
            link: OTHER_LINK,
            deadline: candidate.deadline(),
        };
        assert_eq!(
            link_aborted.confirm_link_timeout_after_abort(foreign_candidate, 47),
            Err(link_fault)
        );
        assert!(
            link_aborted
                .next_command(MonotonicMillis::new(51))
                .is_some()
        );
        assert_eq!(
            link_aborted.confirm_link_timeout_after_abort(candidate, 48),
            Err(link_fault)
        );
        assert_eq!(link_aborted.next_command(MonotonicMillis::new(52)), None);

        let mut canceled = coordinator();
        advance_to_prepared(&mut canceled);
        let cancel_fault = canceled
            .request_preparation_failed(LINK, request_failure)
            .unwrap_err();
        assert_eq!(
            canceled.request_dispatch_canceled_after_native_cancel(OTHER_TOKEN),
            Err(cancel_fault)
        );
        assert!(canceled.next_command(MonotonicMillis::new(100)).is_some());
        assert_eq!(
            canceled.request_dispatch_canceled_after_native_cancel(TOKEN),
            Err(cancel_fault)
        );
        assert_eq!(canceled.next_command(MonotonicMillis::new(101)), None);

        let mut dispatch_failed = coordinator();
        advance_to_prepared(&mut dispatch_failed);
        let dispatch_fault = dispatch_failed
            .request_preparation_failed(LINK, request_failure)
            .unwrap_err();
        assert_eq!(
            dispatch_failed.request_dispatch_failed_after_native_cancel(
                TOKEN,
                RequestFailure::new(RequestFailureStage::Dispatch, 46),
            ),
            Err(dispatch_fault)
        );
        assert_eq!(
            dispatch_failed.next_command(MonotonicMillis::new(101)),
            None
        );

        let mut timed_out = coordinator();
        advance_to_prepared(&mut timed_out);
        timed_out
            .request_dispatch_confirmed(TOKEN, MonotonicMillis::new(100))
            .unwrap();
        let Some(CoordinatorCommand::CancelTimedOutRequest { candidate, .. }) =
            timed_out.next_command(MonotonicMillis::new(200))
        else {
            panic!("expected exact request timeout candidate");
        };
        let timeout_fault = timed_out
            .request_preparation_failed(LINK, request_failure)
            .unwrap_err();
        assert_eq!(
            timed_out.confirm_request_timeout_after_native_cancel(TOKEN, candidate,),
            Err(timeout_fault)
        );
        assert_eq!(timed_out.next_command(MonotonicMillis::new(201)), None);
    }

    #[test]
    fn path_timeout_starts_only_on_dispatch_and_does_not_extend() {
        let mut coordinator = coordinator();
        coordinator
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        assert_eq!(
            coordinator.next_command(MonotonicMillis::new(u64::MAX)),
            Some(CoordinatorCommand::RequestPath {
                destination: DESTINATION,
            }),
            "a retained pre-dispatch action is owned outside the coordinator"
        );
        coordinator
            .path_request_dispatched(DESTINATION, MonotonicMillis::new(10))
            .unwrap();
        coordinator
            .path_request_dispatched(DESTINATION, MonotonicMillis::new(1_000))
            .unwrap();
        assert!(matches!(
            coordinator.next_command(MonotonicMillis::new(30)),
            Some(CoordinatorCommand::ExpirePath { .. })
        ));
    }
}
