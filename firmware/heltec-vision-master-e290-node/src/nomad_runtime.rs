//! Boot-lifetime product ownership for one bounded outbound Nomad page fetch.
//!
//! This module remains independent from the authenticated device API. The
//! product's operation-scoped adapter calls [`NomadRuntimeState::start`] and
//! inspects the retained terminal result, while the permanent node task owns
//! all native Reticulum transitions through the contained coordinator.

use reticulum_node_core::{ApplicationEvent, ApplicationRequestFailReason, RequestHandle};
use reticulum_nomad_protocol::{
    CachedLink, DEFAULT_REQUEST_TIMEOUT_MS, DestinationHash, FetchFailure, FetchOutcome,
    FetchPhase, LinkId, MonotonicMillis, ObservationDisposition, Page, PagePath, RequestFailure,
    RequestFailureStage, RequestId,
};

use crate::nomad_coordinator::{
    CoordinatorCommand, CoordinatorStartError, DEFAULT_LINK_TIMEOUT_MS, DEFAULT_PATH_TIMEOUT_MS,
    InvariantFault, NomadCoordinator, NomadCoordinatorConfig,
};

/// Diagnostic code for a native request timeout reported before product timeout
/// cancellation won the race.
pub const REQUEST_EVENT_TIMEOUT_CODE: u16 = 1;
/// Diagnostic code for a native request terminated by Link closure.
pub const REQUEST_EVENT_LINK_CLOSED_CODE: u16 = 2;
/// Diagnostic code for a failed response Resource transfer.
pub const REQUEST_EVENT_RESOURCE_FAILED_CODE: u16 = 3;
/// Diagnostic code for a Link-close event whose native surface carries no
/// additional reason.
pub const LINK_CLOSED_EVENT_CODE: u16 = 1;

/// Result of offering one transport-neutral application event to Nomad.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NomadEventObservation {
    /// The event exactly advanced or cleaned up retained Nomad state.
    Applied,
    /// The event did not name the active or cached Nomad operation.
    Unrelated,
    /// Exact correlation exposed the coordinator's first sticky invariant fault.
    Fault(InvariantFault),
}

/// One host-testable scheduler decision after path availability arbitration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NomadDriveStep<Token> {
    /// A usable path was consumed before any equal-or-later timeout.
    Progressed,
    /// One exact native command is ready for product execution.
    Command(CoordinatorCommand<Token>),
    /// No native work or local observation is currently ready.
    Idle,
}

/// Boot-lifetime owner joining one coordinator to its active request metadata.
///
/// `Token` is the copy-only native request correlation retained by the
/// coordinator. The E290 product specializes it to [`RequestHandle`]; host tests
/// use a scalar token to exercise lifecycle races without constructing hardware.
#[must_use = "the resident Nomad coordinator must remain alive for the node task"]
pub struct NomadRuntimeState<Token> {
    coordinator: NomadCoordinator<Token>,
    active_destination: Option<DestinationHash>,
}

impl<Token: Copy + Eq> NomadRuntimeState<Token> {
    /// Construct one idle product coordinator using the reviewed product
    /// timeout profile.
    pub const fn new() -> Self {
        let Some(config) = NomadCoordinatorConfig::new(
            DEFAULT_REQUEST_TIMEOUT_MS,
            DEFAULT_PATH_TIMEOUT_MS,
            DEFAULT_LINK_TIMEOUT_MS,
        ) else {
            panic!("the fixed Nomad timeout profile must be valid");
        };
        Self {
            coordinator: NomadCoordinator::new(config),
            active_destination: None,
        }
    }

    /// Begin one internal outbound page fetch.
    ///
    /// This is intentionally not itself a device API. It is the narrow
    /// boot-owner seam used by the authenticated adapter without acquiring
    /// native Reticulum ownership.
    pub fn start(
        &mut self,
        destination: DestinationHash,
        path: PagePath,
        timestamp_unix_ms: u64,
    ) -> Result<(), CoordinatorStartError> {
        self.coordinator
            .start(destination, path, timestamp_unix_ms)?;
        self.active_destination = Some(destination);
        Ok(())
    }

    /// Destination retained for the active or unread terminal fetch.
    pub const fn active_destination(&self) -> Option<DestinationHash> {
        self.active_destination
    }

    /// Current coarse fetch phase.
    pub const fn phase(&self) -> FetchPhase {
        self.coordinator.phase()
    }

    /// Borrow the ready bounded page without releasing it.
    pub const fn ready_page(&self) -> Option<&Page> {
        self.coordinator.ready_page()
    }

    /// Return the retained terminal failure without releasing it.
    pub const fn failure(&self) -> Option<FetchFailure> {
        self.coordinator.failure()
    }

    /// Return the reusable established-Link cache.
    pub const fn cached_link(&self) -> Option<CachedLink> {
        self.coordinator.cached_link()
    }

    /// Return the first sticky product/native invariant fault.
    pub const fn fault(&self) -> Option<InvariantFault> {
        self.coordinator.fault()
    }

    /// Whether healthy protocol state is awaiting one fresh native action.
    pub fn fresh_command_pending(&self) -> bool {
        self.coordinator.fresh_command_pending()
    }

    /// Produce at most one deterministic native command.
    pub fn next_command(&mut self, now: MonotonicMillis) -> Option<CoordinatorCommand<Token>> {
        self.coordinator.next_command(now)
    }

    /// Arbitrate one usable-path observation before returning native work.
    ///
    /// Only an exact externally owned, undispatched Nomad path packet suppresses
    /// the observation. Unrelated ordinary retry, ingress, proof or router
    /// pressure is deliberately absent from this boundary, so a path learned
    /// at the deadline wins over `ExpirePath`. The coordinator command is
    /// sampled exactly once per call.
    pub fn next_step(
        &mut self,
        now: MonotonicMillis,
        usable_path: bool,
        exact_undispatched_path_owner: bool,
    ) -> Result<NomadDriveStep<Token>, InvariantFault> {
        let command = self.coordinator.next_command(now);
        if self.coordinator.fault().is_some()
            || !usable_path
            || exact_undispatched_path_owner
            || self.phase() != FetchPhase::PathLookup
        {
            return Ok(command
                .map(NomadDriveStep::Command)
                .unwrap_or(NomadDriveStep::Idle));
        }
        let Some(destination) = self.active_destination else {
            return Ok(command
                .map(NomadDriveStep::Command)
                .unwrap_or(NomadDriveStep::Idle));
        };
        match command {
            Some(CoordinatorCommand::RequestPath {
                destination: requested,
            }) if requested == destination => {
                self.coordinator.path_already_available(destination)?;
                Ok(NomadDriveStep::Progressed)
            }
            Some(CoordinatorCommand::ExpirePath { candidate })
                if candidate.destination() == destination =>
            {
                match self.coordinator.path_available(destination)? {
                    ObservationDisposition::Applied => Ok(NomadDriveStep::Progressed),
                    ObservationDisposition::Unrelated | ObservationDisposition::WrongPhase => {
                        Ok(NomadDriveStep::Command(CoordinatorCommand::ExpirePath {
                            candidate,
                        }))
                    }
                }
            }
            None => match self.coordinator.path_available(destination)? {
                ObservationDisposition::Applied => Ok(NomadDriveStep::Progressed),
                ObservationDisposition::Unrelated | ObservationDisposition::WrongPhase => {
                    Ok(NomadDriveStep::Idle)
                }
            },
            Some(command) => Ok(NomadDriveStep::Command(command)),
        }
    }

    /// Mutably borrow the sole coordinator for native two-phase transitions.
    ///
    /// The node task is the only product caller. Keeping the borrow scoped to one
    /// synchronous transition prevents native ownership from escaping.
    pub fn coordinator_mut(&mut self) -> &mut NomadCoordinator<Token> {
        &mut self.coordinator
    }

    /// Offer one application event before the generic unavailable-consumer
    /// policy destroys it.
    pub fn observe_application_event(&mut self, event: &ApplicationEvent) -> NomadEventObservation {
        let (exact, observation) = match event {
            ApplicationEvent::LinkEstablished { link } => {
                let link = LinkId::new(*link);
                (
                    self.coordinator.correlates_link_establishment(link),
                    self.coordinator.link_established(link),
                )
            }
            ApplicationEvent::ResponseReceived {
                link,
                request,
                data,
            } => {
                let link = LinkId::new(*link);
                let request = RequestId::new(*request);
                (
                    self.coordinator.correlates_request(link, request),
                    self.coordinator.response_received(link, request, data),
                )
            }
            ApplicationEvent::RequestFailed {
                link,
                request,
                reason,
            } => {
                let link = LinkId::new(*link);
                let request = RequestId::new(*request);
                (
                    self.coordinator.correlates_request(link, request),
                    self.coordinator.request_failed(
                        link,
                        request,
                        RequestFailure::new(
                            RequestFailureStage::Remote,
                            request_failure_event_code(*reason),
                        ),
                    ),
                )
            }
            ApplicationEvent::LinkClosed { link } => {
                let link = LinkId::new(*link);
                (
                    self.coordinator.correlates_link(link),
                    self.coordinator.link_closed(link, LINK_CLOSED_EVENT_CODE),
                )
            }
            _ => return NomadEventObservation::Unrelated,
        };
        match observation {
            Ok(ObservationDisposition::Applied) => NomadEventObservation::Applied,
            Ok(ObservationDisposition::Unrelated | ObservationDisposition::WrongPhase) => {
                NomadEventObservation::Unrelated
            }
            Err(fault) if exact => NomadEventObservation::Fault(fault),
            Err(_) => NomadEventObservation::Unrelated,
        }
    }

    /// Take one terminal result and return the coordinator to idle.
    ///
    /// A future API owner should retain terminal results for repeatable polls and
    /// call this only when explicitly evicting them.
    pub fn take_outcome(&mut self) -> Result<Option<FetchOutcome>, InvariantFault> {
        let outcome = self.coordinator.take_outcome()?;
        if outcome.is_some() {
            self.active_destination = None;
        }
        Ok(outcome)
    }
}

impl<Token: Copy + Eq> Default for NomadRuntimeState<Token> {
    fn default() -> Self {
        Self::new()
    }
}

/// Concrete E290 specialization retaining exact native request handles.
pub type ProductNomadRuntimeState = NomadRuntimeState<RequestHandle>;

const fn request_failure_event_code(reason: ApplicationRequestFailReason) -> u16 {
    match reason {
        ApplicationRequestFailReason::Timeout => REQUEST_EVENT_TIMEOUT_CODE,
        ApplicationRequestFailReason::LinkClosed => REQUEST_EVENT_LINK_CLOSED_CODE,
        ApplicationRequestFailReason::ResourceFailed => REQUEST_EVENT_RESOURCE_FAILED_CODE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nomad_coordinator::{CoordinatorCommand, NativeRequestPhase};
    use reticulum_node_core::ApplicationEvent;
    use reticulum_nomad_protocol::{MAX_PAGE_BYTES, RequestFailureStage};
    use std::vec;

    const DESTINATION: DestinationHash = DestinationHash::new([0x11; 16]);
    const LINK: LinkId = LinkId::new([0x22; 16]);
    const REQUEST: RequestId = RequestId::new([0x33; 16]);
    const TOKEN: u32 = 7;
    const TIMESTAMP_MS: u64 = 1_784_732_100_123;

    fn state() -> NomadRuntimeState<u32> {
        NomadRuntimeState::new()
    }

    fn advance_to_waiting_path(state: &mut NomadRuntimeState<u32>) {
        state
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        assert_eq!(
            state.next_command(MonotonicMillis::new(1)),
            Some(CoordinatorCommand::RequestPath {
                destination: DESTINATION
            })
        );
        state
            .coordinator_mut()
            .path_request_dispatched(DESTINATION, MonotonicMillis::new(2))
            .unwrap();
    }

    fn advance_to_waiting_response(state: &mut NomadRuntimeState<u32>) {
        advance_to_waiting_path(state);
        state.coordinator_mut().path_available(DESTINATION).unwrap();
        assert_eq!(
            state.next_command(MonotonicMillis::new(3)),
            Some(CoordinatorCommand::EstablishLink {
                destination: DESTINATION
            })
        );
        state
            .coordinator_mut()
            .link_request_dispatched(DESTINATION, LINK, MonotonicMillis::new(4))
            .unwrap();
        assert_eq!(
            state.observe_application_event(&ApplicationEvent::LinkEstablished {
                link: *LINK.as_bytes(),
            }),
            NomadEventObservation::Applied
        );
        let Some(CoordinatorCommand::PrepareAnonymousRequest { link, .. }) =
            state.next_command(MonotonicMillis::new(5))
        else {
            panic!("active Link must advance to request preparation");
        };
        assert_eq!(link, LINK);
        state
            .coordinator_mut()
            .request_prepared(LINK, REQUEST, TOKEN)
            .unwrap();
        state
            .coordinator_mut()
            .request_dispatch_confirmed(TOKEN, MonotonicMillis::new(6))
            .unwrap();
    }

    #[test]
    fn retained_path_command_does_not_advance_under_backpressure() {
        let mut state = state();
        state
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        let first = state.next_command(MonotonicMillis::new(1));
        let retried = state.next_command(MonotonicMillis::new(50_000));
        assert_eq!(first, retried);
        assert_eq!(state.phase(), FetchPhase::PathLookup);

        state
            .coordinator_mut()
            .path_request_dispatched(DESTINATION, MonotonicMillis::new(50_001))
            .unwrap();
        assert_eq!(state.next_command(MonotonicMillis::new(50_002)), None);
    }

    #[test]
    fn usable_path_arbitration_ignores_unrelated_pressure_and_wins_at_deadline() {
        let mut initial = state();
        initial
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        assert_eq!(
            initial
                .next_step(MonotonicMillis::new(1), true, false)
                .unwrap(),
            NomadDriveStep::Progressed
        );
        assert!(matches!(
            initial.next_command(MonotonicMillis::new(2)),
            Some(CoordinatorCommand::EstablishLink {
                destination: DESTINATION
            })
        ));

        let mut waiting = state();
        advance_to_waiting_path(&mut waiting);
        assert_eq!(
            waiting
                .next_step(MonotonicMillis::new(30_002), true, false)
                .unwrap(),
            NomadDriveStep::Progressed
        );
        assert!(matches!(
            waiting.next_command(MonotonicMillis::new(30_003)),
            Some(CoordinatorCommand::EstablishLink {
                destination: DESTINATION
            })
        ));
    }

    #[test]
    fn exact_undispatched_path_owner_is_the_only_path_observation_gate() {
        let mut owned = state();
        owned
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        assert_eq!(
            owned
                .next_step(MonotonicMillis::new(1), true, true)
                .unwrap(),
            NomadDriveStep::Command(CoordinatorCommand::RequestPath {
                destination: DESTINATION
            })
        );
        assert_eq!(owned.phase(), FetchPhase::PathLookup);

        let mut expires = state();
        advance_to_waiting_path(&mut expires);
        assert!(matches!(
            expires
                .next_step(MonotonicMillis::new(30_002), false, false)
                .unwrap(),
            NomadDriveStep::Command(CoordinatorCommand::ExpirePath { .. })
        ));
    }

    #[test]
    fn faulted_path_lookup_does_not_consume_a_late_path() {
        let mut state = state();
        state
            .start(DESTINATION, PagePath::index(), TIMESTAMP_MS)
            .unwrap();
        let fault = state
            .coordinator_mut()
            .path_request_dispatched(DestinationHash::new([0x99; 16]), MonotonicMillis::new(1))
            .unwrap_err();
        assert_eq!(
            state.next_step(MonotonicMillis::new(2), true, false),
            Ok(NomadDriveStep::Idle)
        );
        assert_eq!(state.coordinator_mut().fault(), Some(fault));
    }

    #[test]
    fn exact_terminal_events_complete_or_fail_the_active_request() {
        let mut ready = state();
        advance_to_waiting_response(&mut ready);
        let page = vec![b'x'; MAX_PAGE_BYTES];
        assert_eq!(
            ready.observe_application_event(&ApplicationEvent::ResponseReceived {
                link: *LINK.as_bytes(),
                request: *REQUEST.as_bytes(),
                data: page.clone(),
            }),
            NomadEventObservation::Applied
        );
        assert_eq!(
            ready.ready_page().map(Page::as_bytes),
            Some(page.as_slice())
        );

        let mut failed = state();
        advance_to_waiting_response(&mut failed);
        assert_eq!(
            failed.observe_application_event(&ApplicationEvent::RequestFailed {
                link: *LINK.as_bytes(),
                request: *REQUEST.as_bytes(),
                reason: ApplicationRequestFailReason::ResourceFailed,
            }),
            NomadEventObservation::Applied
        );
        assert_eq!(
            failed.failure(),
            Some(FetchFailure::Request {
                link: LINK,
                request: Some(REQUEST),
                failure: RequestFailure::new(
                    RequestFailureStage::Remote,
                    REQUEST_EVENT_RESOURCE_FAILED_CODE,
                ),
            })
        );
    }

    #[test]
    fn native_timeout_event_wins_without_committing_product_timeout() {
        let mut state = state();
        advance_to_waiting_response(&mut state);
        assert!(matches!(
            state.next_command(MonotonicMillis::new(30_006)),
            Some(CoordinatorCommand::CancelTimedOutRequest { token: TOKEN, .. })
        ));

        assert_eq!(
            state.observe_application_event(&ApplicationEvent::RequestFailed {
                link: *LINK.as_bytes(),
                request: *REQUEST.as_bytes(),
                reason: ApplicationRequestFailReason::Timeout,
            }),
            NomadEventObservation::Applied
        );
        assert_eq!(state.next_command(MonotonicMillis::new(30_007)), None);
        assert_eq!(
            state.failure(),
            Some(FetchFailure::Request {
                link: LINK,
                request: Some(REQUEST),
                failure: RequestFailure::new(
                    RequestFailureStage::Remote,
                    REQUEST_EVENT_TIMEOUT_CODE,
                ),
            })
        );
    }

    #[test]
    fn product_timeout_requires_exact_native_cancellation_first() {
        let mut state = state();
        advance_to_waiting_response(&mut state);
        let Some(CoordinatorCommand::CancelTimedOutRequest { token, candidate }) =
            state.next_command(MonotonicMillis::new(30_006))
        else {
            panic!("request timeout must become due");
        };
        assert_eq!(token, TOKEN);
        state
            .coordinator_mut()
            .confirm_request_timeout_after_native_cancel(token, candidate)
            .unwrap();
        assert!(matches!(
            state.failure(),
            Some(FetchFailure::Timeout {
                link: LINK,
                request: REQUEST,
                ..
            })
        ));
        assert_eq!(
            state.observe_application_event(&ApplicationEvent::RequestFailed {
                link: *LINK.as_bytes(),
                request: *REQUEST.as_bytes(),
                reason: ApplicationRequestFailReason::Timeout,
            }),
            NomadEventObservation::Unrelated
        );
        assert!(matches!(
            state.failure(),
            Some(FetchFailure::Timeout {
                link: LINK,
                request: REQUEST,
                ..
            })
        ));
    }

    #[test]
    fn link_close_after_request_failure_still_clears_the_cached_link() {
        let mut state = state();
        advance_to_waiting_response(&mut state);
        assert_eq!(
            state.observe_application_event(&ApplicationEvent::RequestFailed {
                link: *LINK.as_bytes(),
                request: *REQUEST.as_bytes(),
                reason: ApplicationRequestFailReason::LinkClosed,
            }),
            NomadEventObservation::Applied
        );
        assert_eq!(
            state.observe_application_event(&ApplicationEvent::LinkClosed {
                link: *LINK.as_bytes(),
            }),
            NomadEventObservation::Applied
        );
        assert_eq!(state.cached_link(), None);
    }

    #[test]
    fn sticky_fault_exposes_exact_native_cleanup_phase() {
        let mut state = state();
        advance_to_waiting_response(&mut state);
        let fault = state
            .coordinator_mut()
            .request_dispatch_confirmed(TOKEN, MonotonicMillis::new(7))
            .unwrap_err();
        assert_eq!(
            state.next_command(MonotonicMillis::new(8)),
            Some(CoordinatorCommand::CancelRequestForInvariant {
                token: TOKEN,
                link: LINK,
                request: REQUEST,
                phase: NativeRequestPhase::Confirmed,
            })
        );
        assert_eq!(
            state
                .observe_application_event(&ApplicationEvent::LinkEstablished { link: [0x99; 16] }),
            NomadEventObservation::Unrelated
        );
        assert_eq!(
            state.observe_application_event(&ApplicationEvent::ResponseReceived {
                link: *LINK.as_bytes(),
                request: *REQUEST.as_bytes(),
                data: vec![b'x'],
            }),
            NomadEventObservation::Fault(fault)
        );
        assert_eq!(state.next_command(MonotonicMillis::new(9)), None);
    }

    #[test]
    fn product_specialization_remains_bounded() {
        assert!(
            core::mem::size_of::<ProductNomadRuntimeState>()
                <= crate::config::MAXIMUM_NOMAD_RUNTIME_BYTES
        );
        assert_eq!(
            core::mem::align_of::<ProductNomadRuntimeState>(),
            core::mem::align_of::<NomadRuntimeState<RequestHandle>>()
        );
    }
}
