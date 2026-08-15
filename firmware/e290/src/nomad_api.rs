//! Authenticated device-API ownership for one bounded outbound Nomad fetch.
//!
//! The boot-lifetime API state retains only principal/idempotency metadata and
//! an opaque fetch identifier. An operation-scoped port temporarily borrows it
//! together with the transport-neutral Nomad runtime. No credential, flash,
//! Link, request, router, radio, or bearer owner crosses this boundary.

use reticulum_device_api::{
    CapabilityAvailability, IdempotencyKey, MAX_NOMAD_PAGE_BYTES, MAX_NOMAD_PAGE_PATH_BYTES,
    MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS, NomadFetchFailure, NomadFetchId, NomadFetchPhase,
    NomadFetchPollResponse, NomadFetchStartRequest, NomadPage, PrincipalId,
};
use reticulum_device_api_adapter::{
    NomadFetchPort, NomadFetchPortError, NomadFetchStartDisposition,
};
use reticulum_nomad_protocol::{
    DestinationHash, FetchFailure, FetchPhase, MAX_PAGE_BYTES, MAX_PAGE_PATH_BYTES, PagePath,
};

use crate::{
    nomad_coordinator::{
        CoordinatorStartError,
        MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS as COORDINATOR_MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS,
    },
    nomad_runtime::NomadRuntimeState,
};

const _: () = assert!(MAX_NOMAD_PAGE_PATH_BYTES == MAX_PAGE_PATH_BYTES);
const _: () = assert!(MAX_NOMAD_PAGE_BYTES == MAX_PAGE_BYTES);
const _: () =
    assert!(MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS == COORDINATOR_MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS);

/// Reviewed upper bound for boot-lifetime API metadata beside the Nomad owner.
pub const MAXIMUM_NOMAD_FETCH_API_STATE_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FetchSemantics {
    destination: DestinationHash,
    path: PagePath,
    timestamp_unix_ms: u64,
}

impl FetchSemantics {
    fn from_request(request: NomadFetchStartRequest<'_>) -> Result<Self, NomadFetchPortError> {
        let path = PagePath::new(request.path().as_str())
            .map_err(|_| NomadFetchPortError::InvalidRequest)?;
        Ok(Self {
            destination: DestinationHash::new(request.destination().0),
            path,
            timestamp_unix_ms: request.timestamp_unix_ms().get(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FetchRecord {
    id: NomadFetchId,
    principal: PrincipalId,
    idempotency_key: IdempotencyKey,
    semantics: FetchSemantics,
}

/// Boot-lifetime API metadata for the product's single Nomad fetch slot.
#[must_use = "fetch metadata must remain beside the boot-lifetime Nomad runtime"]
pub struct NomadFetchApiState {
    next_sequence: Option<u64>,
    record: Option<FetchRecord>,
}

impl NomadFetchApiState {
    /// Construct an empty API owner whose first accepted fetch has sequence one.
    pub const fn new() -> Self {
        Self {
            next_sequence: Some(1),
            record: None,
        }
    }
}

impl Default for NomadFetchApiState {
    fn default() -> Self {
        Self::new()
    }
}

const _: () =
    assert!(core::mem::size_of::<NomadFetchApiState>() <= MAXIMUM_NOMAD_FETCH_API_STATE_BYTES);

/// Operation-scoped adapter joining API metadata to the neutral Nomad runtime.
pub struct ProductNomadFetchPort<'owner, Token> {
    runtime: &'owner mut NomadRuntimeState<Token>,
    api: &'owner mut NomadFetchApiState,
    incarnation: [u8; 8],
    service_enabled: bool,
}

impl<'owner, Token: Copy + Eq> ProductNomadFetchPort<'owner, Token> {
    /// Borrow the two disjoint owners for one synchronous authenticated call.
    pub fn new(
        runtime: &'owner mut NomadRuntimeState<Token>,
        api: &'owner mut NomadFetchApiState,
        incarnation: [u8; 8],
        service_enabled: bool,
    ) -> Self {
        Self {
            runtime,
            api,
            incarnation,
            service_enabled,
        }
    }

    fn terminal_outcome_retained(&self) -> bool {
        matches!(self.runtime.phase(), FetchPhase::Ready | FetchPhase::Failed)
    }

    fn metadata_matches_runtime(&self) -> bool {
        match self.api.record {
            None => self.runtime.phase() == FetchPhase::Idle,
            Some(record) => {
                self.runtime.phase() != FetchPhase::Idle
                    && self.runtime.active_destination() == Some(record.semantics.destination)
                    && record.id.incarnation() == self.incarnation
            }
        }
    }

    fn release_terminal_for_fresh_start(&mut self) -> Result<(), NomadFetchPortError> {
        if !self.terminal_outcome_retained() {
            return Err(NomadFetchPortError::Busy);
        }
        match self.runtime.take_outcome() {
            Ok(Some(_)) => {
                self.api.record = None;
                Ok(())
            }
            Ok(None) => Err(NomadFetchPortError::Invariant),
            Err(_) => Err(NomadFetchPortError::Faulted),
        }
    }

    fn allocate_id(&self) -> Result<NomadFetchId, NomadFetchStartDisposition> {
        let Some(sequence) = self.api.next_sequence else {
            return Err(NomadFetchStartDisposition::CapacityExhausted);
        };
        NomadFetchId::new(self.incarnation, sequence)
            .map_err(|_| NomadFetchStartDisposition::CapacityExhausted)
    }

    fn poll_exact(&self) -> Result<NomadFetchPollResponse, NomadFetchPortError> {
        if self.runtime.fault().is_some() {
            return Ok(NomadFetchPollResponse::Failed(NomadFetchFailure::Internal));
        }
        match self.runtime.phase() {
            FetchPhase::Idle => Err(NomadFetchPortError::Invariant),
            FetchPhase::PathLookup => {
                Ok(NomadFetchPollResponse::Pending(NomadFetchPhase::PathLookup))
            }
            FetchPhase::LinkEstablishment => Ok(NomadFetchPollResponse::Pending(
                NomadFetchPhase::LinkEstablishment,
            )),
            FetchPhase::RequestPreparation => Ok(NomadFetchPollResponse::Pending(
                NomadFetchPhase::RequestPreparation,
            )),
            FetchPhase::AwaitingDispatchConfirmation => Ok(NomadFetchPollResponse::Pending(
                NomadFetchPhase::AwaitingDispatchConfirmation,
            )),
            FetchPhase::AwaitingResponse => Ok(NomadFetchPollResponse::Pending(
                NomadFetchPhase::AwaitingResponse,
            )),
            FetchPhase::Ready => {
                let page = self
                    .runtime
                    .ready_page()
                    .ok_or(NomadFetchPortError::Invariant)?;
                let page =
                    NomadPage::new(page.as_bytes()).map_err(|_| NomadFetchPortError::Invariant)?;
                Ok(NomadFetchPollResponse::Ready(page))
            }
            FetchPhase::Failed => {
                let failure = self
                    .runtime
                    .failure()
                    .ok_or(NomadFetchPortError::Invariant)?;
                Ok(NomadFetchPollResponse::Failed(map_failure(failure)))
            }
        }
    }
}

impl<Token: Copy + Eq> NomadFetchPort for ProductNomadFetchPort<'_, Token> {
    fn availability(&mut self) -> CapabilityAvailability {
        if self.service_enabled {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Disabled
        }
    }

    fn start(
        &mut self,
        principal: PrincipalId,
        request: NomadFetchStartRequest<'_>,
    ) -> Result<NomadFetchStartDisposition, NomadFetchPortError> {
        if !self.service_enabled {
            return Err(NomadFetchPortError::Unavailable);
        }
        if !self.metadata_matches_runtime() {
            return Err(NomadFetchPortError::Invariant);
        }
        let runtime_faulted = self.runtime.fault().is_some();
        let semantics = FetchSemantics::from_request(request)?;
        if let Some(record) = self.api.record {
            if record.principal == principal && record.idempotency_key == request.idempotency_key()
            {
                return if record.semantics == semantics {
                    Ok(NomadFetchStartDisposition::Replay(record.id))
                } else {
                    Ok(NomadFetchStartDisposition::IdempotencyConflict)
                };
            }
            if runtime_faulted {
                return Err(NomadFetchPortError::Faulted);
            }
            if self.terminal_outcome_retained() {
                if self.api.next_sequence.is_none() {
                    return Ok(NomadFetchStartDisposition::CapacityExhausted);
                }
                self.release_terminal_for_fresh_start()?;
            } else {
                return Ok(NomadFetchStartDisposition::CapacityExhausted);
            }
        }
        if runtime_faulted {
            return Err(NomadFetchPortError::Faulted);
        }

        let id = match self.allocate_id() {
            Ok(id) => id,
            Err(disposition) => return Ok(disposition),
        };
        match self.runtime.start(
            semantics.destination,
            semantics.path,
            semantics.timestamp_unix_ms,
        ) {
            Ok(()) => {}
            Err(CoordinatorStartError::Busy) => return Err(NomadFetchPortError::Invariant),
            Err(CoordinatorStartError::InvalidTimestamp { .. }) => {
                return Err(NomadFetchPortError::InvalidRequest);
            }
            Err(CoordinatorStartError::Faulted(_)) => return Err(NomadFetchPortError::Faulted),
        }
        self.api.record = Some(FetchRecord {
            id,
            principal,
            idempotency_key: request.idempotency_key(),
            semantics,
        });
        self.api.next_sequence = id.sequence().checked_add(1);
        Ok(NomadFetchStartDisposition::Accepted(id))
    }

    fn poll(
        &mut self,
        principal: PrincipalId,
        id: NomadFetchId,
    ) -> Result<Option<NomadFetchPollResponse>, NomadFetchPortError> {
        if !self.service_enabled {
            return Err(NomadFetchPortError::Unavailable);
        }
        let Some(record) = self.api.record else {
            return Ok(None);
        };
        if record.principal != principal || record.id != id {
            return Ok(None);
        }
        if !self.metadata_matches_runtime() {
            return Err(NomadFetchPortError::Invariant);
        }
        self.poll_exact().map(Some)
    }
}

const fn map_failure(failure: FetchFailure) -> NomadFetchFailure {
    match failure {
        FetchFailure::NoPath { .. } => NomadFetchFailure::NoPath,
        FetchFailure::Link { .. } => NomadFetchFailure::Link,
        FetchFailure::Request { .. } => NomadFetchFailure::Request,
        FetchFailure::Timeout { .. } => NomadFetchFailure::Timeout,
        FetchFailure::TooLarge { .. } => NomadFetchFailure::PageTooLarge,
        FetchFailure::InvalidUtf8 => NomadFetchFailure::InvalidUtf8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nomad_coordinator::CoordinatorCommand;
    use reticulum_device_api::{
        DestinationHash as ApiDestinationHash, NomadPagePath, NomadRequestTimestampUnixMs,
    };
    use reticulum_nomad_protocol::{
        LinkFailure, LinkFailureStage, LinkId, MonotonicMillis, ObservationDisposition, RequestId,
    };

    const INCARNATION: [u8; 8] = [0xa5; 8];
    const PRINCIPAL: PrincipalId = PrincipalId([0x11; 16]);
    const OTHER_PRINCIPAL: PrincipalId = PrincipalId([0x22; 16]);
    const DESTINATION: [u8; 16] = [0x33; 16];
    const KEY: IdempotencyKey = IdempotencyKey([0x44; 16]);
    const OTHER_KEY: IdempotencyKey = IdempotencyKey([0x55; 16]);
    const TIMESTAMP: u64 = 1_784_732_100_123;
    const LINK: LinkId = LinkId::new([0x66; 16]);
    const REQUEST: RequestId = RequestId::new([0x77; 16]);

    fn request(path: &'static str, key: IdempotencyKey) -> NomadFetchStartRequest<'static> {
        NomadFetchStartRequest::new(
            ApiDestinationHash(DESTINATION),
            NomadPagePath::new(path).unwrap(),
            NomadRequestTimestampUnixMs::new(TIMESTAMP).unwrap(),
            key,
        )
    }

    fn accepted_id(
        runtime: &mut NomadRuntimeState<u32>,
        api: &mut NomadFetchApiState,
    ) -> NomadFetchId {
        let mut port = ProductNomadFetchPort::new(runtime, api, INCARNATION, true);
        match port
            .start(PRINCIPAL, request("/page/index.mu", KEY))
            .unwrap()
        {
            NomadFetchStartDisposition::Accepted(id) => id,
            other => panic!("fresh start was not accepted: {other:?}"),
        }
    }

    fn assert_pending(
        runtime: &mut NomadRuntimeState<u32>,
        api: &mut NomadFetchApiState,
        id: NomadFetchId,
        expected: NomadFetchPhase,
    ) {
        let mut port = ProductNomadFetchPort::new(runtime, api, INCARNATION, true);
        assert_eq!(
            port.poll(PRINCIPAL, id),
            Ok(Some(NomadFetchPollResponse::Pending(expected)))
        );
    }

    #[test]
    fn start_is_principal_scoped_idempotent_and_capacity_bounded() {
        let mut runtime = NomadRuntimeState::<u32>::new();
        let mut api = NomadFetchApiState::new();
        let id = accepted_id(&mut runtime, &mut api);
        assert_eq!(id.incarnation(), INCARNATION);
        assert_eq!(id.sequence(), 1);

        let mut port = ProductNomadFetchPort::new(&mut runtime, &mut api, INCARNATION, true);
        assert_eq!(
            port.start(PRINCIPAL, request("/page/index.mu", KEY)),
            Ok(NomadFetchStartDisposition::Replay(id))
        );
        assert_eq!(
            port.start(PRINCIPAL, request("/page/other.mu", KEY)),
            Ok(NomadFetchStartDisposition::IdempotencyConflict)
        );
        assert_eq!(
            port.start(PRINCIPAL, request("/page/index.mu", OTHER_KEY)),
            Ok(NomadFetchStartDisposition::CapacityExhausted)
        );
        assert_eq!(
            port.start(OTHER_PRINCIPAL, request("/page/index.mu", KEY)),
            Ok(NomadFetchStartDisposition::CapacityExhausted)
        );
    }

    #[test]
    fn poll_hides_foreign_ids_and_maps_each_nonterminal_phase() {
        let mut runtime = NomadRuntimeState::<u32>::new();
        let mut api = NomadFetchApiState::new();
        let id = accepted_id(&mut runtime, &mut api);
        let other_id = NomadFetchId::new(INCARNATION, 99).unwrap();

        {
            let mut port = ProductNomadFetchPort::new(&mut runtime, &mut api, INCARNATION, true);
            assert_eq!(port.poll(OTHER_PRINCIPAL, id), Ok(None));
            assert_eq!(port.poll(PRINCIPAL, other_id), Ok(None));
            assert_eq!(
                port.poll(PRINCIPAL, id),
                Ok(Some(NomadFetchPollResponse::Pending(
                    NomadFetchPhase::PathLookup
                )))
            );
        }

        runtime
            .coordinator_mut()
            .path_already_available(DestinationHash::new(DESTINATION))
            .unwrap();
        let mut port = ProductNomadFetchPort::new(&mut runtime, &mut api, INCARNATION, true);
        assert_eq!(
            port.poll(PRINCIPAL, id),
            Ok(Some(NomadFetchPollResponse::Pending(
                NomadFetchPhase::LinkEstablishment
            )))
        );
    }

    #[test]
    fn ready_page_is_repeatable_and_a_distinct_start_evicts_the_terminal() {
        let mut runtime = NomadRuntimeState::<u32>::new();
        let mut api = NomadFetchApiState::new();
        let id = accepted_id(&mut runtime, &mut api);
        let destination = DestinationHash::new(DESTINATION);
        runtime
            .coordinator_mut()
            .path_already_available(destination)
            .unwrap();
        assert_pending(
            &mut runtime,
            &mut api,
            id,
            NomadFetchPhase::LinkEstablishment,
        );
        assert!(matches!(
            runtime.next_command(MonotonicMillis::new(1)),
            Some(CoordinatorCommand::EstablishLink { .. })
        ));
        runtime
            .coordinator_mut()
            .link_request_dispatched(destination, LINK, MonotonicMillis::new(2))
            .unwrap();
        assert_eq!(
            runtime.coordinator_mut().link_established(LINK).unwrap(),
            ObservationDisposition::Applied
        );
        assert_pending(
            &mut runtime,
            &mut api,
            id,
            NomadFetchPhase::RequestPreparation,
        );
        assert!(matches!(
            runtime.next_command(MonotonicMillis::new(3)),
            Some(CoordinatorCommand::PrepareAnonymousRequest { .. })
        ));
        runtime
            .coordinator_mut()
            .request_prepared(LINK, REQUEST, 9)
            .unwrap();
        assert_pending(
            &mut runtime,
            &mut api,
            id,
            NomadFetchPhase::AwaitingDispatchConfirmation,
        );
        runtime
            .coordinator_mut()
            .request_dispatch_confirmed(9, MonotonicMillis::new(4))
            .unwrap();
        assert_pending(
            &mut runtime,
            &mut api,
            id,
            NomadFetchPhase::AwaitingResponse,
        );
        assert_eq!(
            runtime
                .coordinator_mut()
                .response_received(LINK, REQUEST, b">Metalbeard")
                .unwrap(),
            ObservationDisposition::Applied
        );

        let expected = NomadFetchPollResponse::Ready(NomadPage::new(b">Metalbeard").unwrap());
        let mut port = ProductNomadFetchPort::new(&mut runtime, &mut api, INCARNATION, true);
        assert_eq!(port.poll(PRINCIPAL, id), Ok(Some(expected)));
        assert_eq!(port.poll(PRINCIPAL, id), Ok(Some(expected)));
        port.api.next_sequence = None;
        assert_eq!(
            port.start(PRINCIPAL, request("/page/other.mu", OTHER_KEY)),
            Ok(NomadFetchStartDisposition::CapacityExhausted)
        );
        assert_eq!(port.poll(PRINCIPAL, id), Ok(Some(expected)));
        port.api.next_sequence = Some(2);
        assert_eq!(
            port.start(PRINCIPAL, request("/page/other.mu", OTHER_KEY)),
            Ok(NomadFetchStartDisposition::Accepted(
                NomadFetchId::new(INCARNATION, 2).unwrap()
            ))
        );
        assert_eq!(port.poll(PRINCIPAL, id), Ok(None));
    }

    #[test]
    fn terminal_failure_and_disabled_profile_map_without_leaking_diagnostics() {
        let mut runtime = NomadRuntimeState::<u32>::new();
        let mut api = NomadFetchApiState::new();
        let id = accepted_id(&mut runtime, &mut api);
        let destination = DestinationHash::new(DESTINATION);
        runtime
            .coordinator_mut()
            .path_already_available(destination)
            .unwrap();
        assert_eq!(
            runtime
                .coordinator_mut()
                .link_preparation_failed(
                    destination,
                    LinkFailure::new(LinkFailureStage::Preparation, 17),
                )
                .unwrap(),
            ObservationDisposition::Applied
        );

        {
            let mut port = ProductNomadFetchPort::new(&mut runtime, &mut api, INCARNATION, true);
            assert_eq!(
                port.poll(PRINCIPAL, id),
                Ok(Some(NomadFetchPollResponse::Failed(
                    NomadFetchFailure::Link
                )))
            );
        }

        let mut disabled = ProductNomadFetchPort::new(&mut runtime, &mut api, INCARNATION, false);
        assert_eq!(disabled.availability(), CapabilityAvailability::Disabled);
        assert_eq!(
            disabled.poll(PRINCIPAL, id),
            Err(NomadFetchPortError::Unavailable)
        );
    }

    #[test]
    fn sticky_faults_and_runtime_metadata_disagreement_fail_closed() {
        let mut faulted_runtime = NomadRuntimeState::<u32>::new();
        let mut faulted_api = NomadFetchApiState::new();
        let id = accepted_id(&mut faulted_runtime, &mut faulted_api);
        faulted_runtime
            .coordinator_mut()
            .path_request_dispatched(DestinationHash::new([0x99; 16]), MonotonicMillis::new(1))
            .expect_err("mismatched dispatch must latch a coordinator fault");
        let mut faulted =
            ProductNomadFetchPort::new(&mut faulted_runtime, &mut faulted_api, INCARNATION, true);
        assert_eq!(
            faulted.start(PRINCIPAL, request("/page/index.mu", KEY)),
            Ok(NomadFetchStartDisposition::Replay(id))
        );
        assert_eq!(
            faulted.start(PRINCIPAL, request("/page/other.mu", OTHER_KEY)),
            Err(NomadFetchPortError::Faulted)
        );
        assert_eq!(
            faulted.poll(PRINCIPAL, id),
            Ok(Some(NomadFetchPollResponse::Failed(
                NomadFetchFailure::Internal
            )))
        );

        let mut mismatched_runtime = NomadRuntimeState::<u32>::new();
        mismatched_runtime
            .start(
                DestinationHash::new(DESTINATION),
                PagePath::index(),
                TIMESTAMP,
            )
            .unwrap();
        let mut empty_api = NomadFetchApiState::new();
        let mut mismatched =
            ProductNomadFetchPort::new(&mut mismatched_runtime, &mut empty_api, INCARNATION, true);
        assert_eq!(
            mismatched.start(PRINCIPAL, request("/page/index.mu", KEY)),
            Err(NomadFetchPortError::Invariant)
        );
        assert_eq!(
            mismatched.poll(PRINCIPAL, NomadFetchId::new(INCARNATION, 1).unwrap()),
            Ok(None)
        );

        let mut record_runtime = NomadRuntimeState::<u32>::new();
        let mut record_api = NomadFetchApiState::new();
        let exact_id = accepted_id(&mut record_runtime, &mut record_api);
        record_runtime = NomadRuntimeState::new();
        let stale_id = NomadFetchId::new(INCARNATION, exact_id.sequence() + 1).unwrap();
        let mut record_mismatch =
            ProductNomadFetchPort::new(&mut record_runtime, &mut record_api, INCARNATION, true);
        assert_eq!(record_mismatch.poll(OTHER_PRINCIPAL, exact_id), Ok(None));
        assert_eq!(record_mismatch.poll(PRINCIPAL, stale_id), Ok(None));
        assert_eq!(
            record_mismatch.poll(PRINCIPAL, exact_id),
            Err(NomadFetchPortError::Invariant)
        );
    }
}
