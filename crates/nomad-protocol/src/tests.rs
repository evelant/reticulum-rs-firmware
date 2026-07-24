use super::*;

const DESTINATION: DestinationHash = DestinationHash::new([0x11; DESTINATION_HASH_LENGTH]);
const OTHER_DESTINATION: DestinationHash = DestinationHash::new([0x22; DESTINATION_HASH_LENGTH]);
const LINK: LinkId = LinkId::new([0x33; LINK_ID_LENGTH]);
const OTHER_LINK: LinkId = LinkId::new([0x44; LINK_ID_LENGTH]);
const REQUEST: RequestId = RequestId::new([0x55; REQUEST_ID_LENGTH]);
const OTHER_REQUEST: RequestId = RequestId::new([0x66; REQUEST_ID_LENGTH]);

fn client() -> NomadClient {
    NomadClient::new(FetchConfig::new(100).unwrap())
}

fn advance_to_request(client: &mut NomadClient) {
    client.start(DESTINATION, PagePath::index()).unwrap();
    assert_eq!(
        client.action(),
        Some(FetchAction::RequestPath {
            destination: DESTINATION
        })
    );
    client.confirm_path_request(DESTINATION).unwrap();
    assert_eq!(
        client.path_available(DESTINATION),
        ObservationDisposition::Applied
    );
    assert_eq!(
        client.action(),
        Some(FetchAction::EstablishLink {
            destination: DESTINATION
        })
    );
    client.confirm_link_request(DESTINATION, LINK).unwrap();
    assert_eq!(
        client.link_established(DESTINATION, LINK),
        ObservationDisposition::Applied
    );
}

fn advance_to_prepared(client: &mut NomadClient) {
    advance_to_request(client);
    assert_eq!(
        client.action(),
        Some(FetchAction::PrepareAnonymousRequest {
            destination: DESTINATION,
            link: LINK,
            path: PagePath::index(),
        })
    );
    client.request_prepared(LINK, REQUEST).unwrap();
}

fn advance_to_response(client: &mut NomadClient) {
    advance_to_prepared(client);
    client
        .confirm_request_dispatch(LINK, REQUEST, MonotonicMillis::new(1_000))
        .unwrap();
}

#[test]
fn path_validation_is_bounded_and_absolute() {
    assert_eq!(PagePath::index().as_str(), DEFAULT_INDEX_PATH);
    assert_eq!(PagePath::index().len(), DEFAULT_INDEX_PATH.len());
    assert!(!PagePath::index().is_empty());
    assert_eq!(PagePath::new("page/index.mu"), Err(PathError::Invalid));
    assert_eq!(PagePath::new("/bad\0path"), Err(PathError::Invalid));

    let maximum = std::format!("/{}", "x".repeat(MAX_PAGE_PATH_BYTES - 1));
    assert_eq!(PagePath::new(&maximum).unwrap().len(), MAX_PAGE_PATH_BYTES);

    let too_long = std::format!("/{}", "x".repeat(MAX_PAGE_PATH_BYTES));
    assert_eq!(
        PagePath::new(&too_long),
        Err(PathError::TooLong {
            actual: MAX_PAGE_PATH_BYTES + 1,
            maximum: MAX_PAGE_PATH_BYTES,
        })
    );
}

#[test]
fn full_uncached_fetch_progresses_through_explicit_actions() {
    let mut client = client();
    advance_to_response(&mut client);

    assert_eq!(client.phase(), FetchPhase::AwaitingResponse);
    assert_eq!(
        client.cached_link(),
        Some(CachedLink::new(DESTINATION, LINK))
    );
    assert_eq!(
        client.response_received(LINK, REQUEST, b"Hello, Micron"),
        ObservationDisposition::Applied
    );
    assert_eq!(client.phase(), FetchPhase::Ready);
    assert_eq!(
        client.ready_page().unwrap().as_str().unwrap(),
        "Hello, Micron"
    );

    assert_eq!(
        client.take_outcome(),
        Some(FetchOutcome::Ready(
            Page::from_utf8(b"Hello, Micron").unwrap()
        ))
    );
    assert_eq!(client.phase(), FetchPhase::Idle);
    assert_eq!(
        client.cached_link(),
        Some(CachedLink::new(DESTINATION, LINK))
    );
}

#[test]
fn one_fetch_slot_reports_busy_until_terminal_result_is_taken() {
    let mut client = client();
    client.start(DESTINATION, PagePath::index()).unwrap();
    assert_eq!(
        client.start(OTHER_DESTINATION, PagePath::index()),
        Err(StartError::Busy)
    );
    client.confirm_path_request(DESTINATION).unwrap();
    assert_eq!(
        client.path_unavailable(DESTINATION),
        ObservationDisposition::Applied
    );
    assert_eq!(
        client.start(OTHER_DESTINATION, PagePath::index()),
        Err(StartError::Busy)
    );
    assert!(matches!(
        client.take_outcome(),
        Some(FetchOutcome::Failed(FetchFailure::NoPath { .. }))
    ));
    assert!(client.start(OTHER_DESTINATION, PagePath::index()).is_ok());
}

#[test]
fn cached_matching_link_bypasses_path_and_link_actions() {
    let mut client = client();
    client
        .seed_cached_link(CachedLink::new(DESTINATION, LINK))
        .unwrap();
    client.start(DESTINATION, PagePath::index()).unwrap();
    assert_eq!(client.phase(), FetchPhase::RequestPreparation);
    assert!(matches!(
        client.action(),
        Some(FetchAction::PrepareAnonymousRequest {
            destination: DESTINATION,
            link: LINK,
            ..
        })
    ));
}

#[test]
fn cached_link_for_another_destination_does_not_skip_path_discovery() {
    let mut client = client();
    client
        .seed_cached_link(CachedLink::new(OTHER_DESTINATION, OTHER_LINK))
        .unwrap();
    client.start(DESTINATION, PagePath::index()).unwrap();
    assert_eq!(
        client.action(),
        Some(FetchAction::RequestPath {
            destination: DESTINATION
        })
    );
}

#[test]
fn path_and_link_controls_are_exact_and_cancellable() {
    let mut client = client();
    client.start(DESTINATION, PagePath::index()).unwrap();
    assert_eq!(
        client.confirm_path_request(OTHER_DESTINATION),
        Err(ControlError::DestinationMismatch)
    );
    client.confirm_path_request(DESTINATION).unwrap();
    client.cancel_path_request(DESTINATION).unwrap();
    assert!(matches!(
        client.action(),
        Some(FetchAction::RequestPath { .. })
    ));
    client.confirm_path_request(DESTINATION).unwrap();
    assert_eq!(
        client.path_available(OTHER_DESTINATION),
        ObservationDisposition::Unrelated
    );
    assert_eq!(
        client.path_available(DESTINATION),
        ObservationDisposition::Applied
    );

    client.confirm_link_request(DESTINATION, LINK).unwrap();
    assert_eq!(
        client.cancel_link_request(DESTINATION, OTHER_LINK),
        Err(ControlError::LinkMismatch)
    );
    client.cancel_link_request(DESTINATION, LINK).unwrap();
    assert!(matches!(
        client.action(),
        Some(FetchAction::EstablishLink { .. })
    ));
    client.confirm_link_request(DESTINATION, LINK).unwrap();
    assert_eq!(
        client.link_established(OTHER_DESTINATION, OTHER_LINK),
        ObservationDisposition::Unrelated
    );
    assert_eq!(
        client.link_established(DESTINATION, OTHER_LINK),
        ObservationDisposition::Unrelated
    );
    assert_eq!(
        client.link_established(DESTINATION, LINK),
        ObservationDisposition::Applied
    );
}

#[test]
fn request_prepare_confirm_cancel_is_exact_and_timeout_free_until_confirmed() {
    let mut client = client();
    advance_to_request(&mut client);

    assert_eq!(
        client.request_prepared(OTHER_LINK, REQUEST),
        Err(ControlError::LinkMismatch)
    );
    let prepared = client.request_prepared(LINK, REQUEST).unwrap();
    assert_eq!(client.prepared_request(), Some(prepared));
    assert_eq!(
        client.request_timeout_candidate(MonotonicMillis::new(u64::MAX)),
        None
    );
    assert_eq!(
        client.cancel_request_dispatch(LINK, OTHER_REQUEST),
        Err(ControlError::RequestMismatch)
    );
    client.cancel_request_dispatch(LINK, REQUEST).unwrap();
    assert_eq!(client.phase(), FetchPhase::RequestPreparation);
    assert_eq!(
        client.request_timeout_candidate(MonotonicMillis::new(u64::MAX)),
        None
    );

    client.request_prepared(LINK, REQUEST).unwrap();
    client
        .confirm_request_dispatch(LINK, REQUEST, MonotonicMillis::new(1_000))
        .unwrap();
    assert_eq!(
        client.confirm_request_dispatch(LINK, REQUEST, MonotonicMillis::new(1_001)),
        Err(ControlError::DispatchTimeMismatch)
    );
    assert_eq!(
        client.request_timeout_candidate(MonotonicMillis::new(1_099)),
        None
    );
    let due = client
        .request_timeout_candidate(MonotonicMillis::new(1_100))
        .unwrap();
    assert_eq!(due.link(), LINK);
    assert_eq!(due.request(), REQUEST);
    assert_eq!(due.dispatched_at(), MonotonicMillis::new(1_000));
    assert_eq!(due.deadline(), MonotonicMillis::new(1_100));
    assert_eq!(client.phase(), FetchPhase::AwaitingResponse);
    client.confirm_request_timeout(due).unwrap();
    assert_eq!(
        client.failure(),
        Some(FetchFailure::Timeout {
            link: LINK,
            request: REQUEST,
            dispatched_at: MonotonicMillis::new(1_000),
            deadline: MonotonicMillis::new(1_100),
        })
    );
}

#[test]
fn response_and_request_failure_require_both_link_and_request_match() {
    let mut client = client();
    advance_to_response(&mut client);
    assert_eq!(
        client.response_received(OTHER_LINK, REQUEST, b"ok"),
        ObservationDisposition::Unrelated
    );
    assert_eq!(
        client.response_received(LINK, OTHER_REQUEST, b"ok"),
        ObservationDisposition::Unrelated
    );
    assert_eq!(client.phase(), FetchPhase::AwaitingResponse);

    let failure = RequestFailure::new(RequestFailureStage::Remote, 9);
    assert_eq!(
        client.request_failed(OTHER_LINK, REQUEST, failure),
        ObservationDisposition::Unrelated
    );
    assert_eq!(
        client.request_failed(LINK, OTHER_REQUEST, failure),
        ObservationDisposition::Unrelated
    );
    assert_eq!(
        client.request_failed(LINK, REQUEST, failure),
        ObservationDisposition::Applied
    );
    assert_eq!(
        client.failure(),
        Some(FetchFailure::Request {
            link: LINK,
            request: Some(REQUEST),
            failure,
        })
    );
}

#[test]
fn decoded_response_body_is_retained_without_transport_framing() {
    let body = b"# A small page";
    let mut client = client();
    advance_to_response(&mut client);
    assert_eq!(
        client.response_received(LINK, REQUEST, body),
        ObservationDisposition::Applied
    );
    assert_eq!(client.ready_page().unwrap().as_bytes(), body);
}

#[test]
fn empty_page_is_valid() {
    let mut client = client();
    advance_to_response(&mut client);
    client.response_received(LINK, REQUEST, b"");
    let page = client.ready_page().unwrap();
    assert!(page.is_empty());
    assert_eq!(page.as_str().unwrap(), "");
}

#[test]
fn response_cap_accepts_400_and_rejects_401() {
    let maximum = [b'x'; MAX_PAGE_BYTES];
    let mut maximum_client = client();
    advance_to_response(&mut maximum_client);
    maximum_client.response_received(LINK, REQUEST, &maximum);
    assert_eq!(maximum_client.ready_page().unwrap().len(), MAX_PAGE_BYTES);

    let oversized = [b'x'; MAX_PAGE_BYTES + 1];
    let mut oversized_client = client();
    advance_to_response(&mut oversized_client);
    oversized_client.response_received(LINK, REQUEST, &oversized);
    assert_eq!(
        oversized_client.failure(),
        Some(FetchFailure::TooLarge {
            actual: MAX_PAGE_BYTES + 1,
            maximum: MAX_PAGE_BYTES,
        })
    );
}

#[test]
fn non_utf8_response_fails_closed() {
    let mut client = client();
    advance_to_response(&mut client);
    client.response_received(LINK, REQUEST, &[0xc3, 0x28]);
    assert_eq!(client.failure(), Some(FetchFailure::InvalidUtf8));
}

#[test]
fn no_path_link_and_request_failures_are_typed() {
    let mut no_path = client();
    no_path.start(DESTINATION, PagePath::index()).unwrap();
    no_path.confirm_path_request(DESTINATION).unwrap();
    no_path.path_unavailable(DESTINATION);
    assert_eq!(
        no_path.failure(),
        Some(FetchFailure::NoPath {
            destination: DESTINATION
        })
    );

    let link_failure = LinkFailure::new(LinkFailureStage::Establishment, 7);
    let mut link = client();
    link.start(DESTINATION, PagePath::index()).unwrap();
    link.confirm_path_request(DESTINATION).unwrap();
    link.path_available(DESTINATION);
    link.confirm_link_request(DESTINATION, LINK).unwrap();
    link.link_failed(DESTINATION, LINK, link_failure);
    assert_eq!(
        link.failure(),
        Some(FetchFailure::Link {
            destination: DESTINATION,
            failure: link_failure,
        })
    );

    let preparation_failure = LinkFailure::new(LinkFailureStage::Preparation, 70);
    let mut link_preparation = client();
    link_preparation
        .start(DESTINATION, PagePath::index())
        .unwrap();
    link_preparation.confirm_path_request(DESTINATION).unwrap();
    link_preparation.path_available(DESTINATION);
    assert_eq!(
        link_preparation.link_preparation_failed(DESTINATION, preparation_failure),
        ObservationDisposition::Applied
    );
    assert_eq!(
        link_preparation.failure(),
        Some(FetchFailure::Link {
            destination: DESTINATION,
            failure: preparation_failure,
        })
    );

    let request_failure = RequestFailure::new(RequestFailureStage::Preparation, 8);
    let mut request = client();
    advance_to_request(&mut request);
    request.request_preparation_failed(LINK, request_failure);
    assert_eq!(
        request.failure(),
        Some(FetchFailure::Request {
            link: LINK,
            request: None,
            failure: request_failure,
        })
    );
}

#[test]
fn dispatch_failure_retains_exact_prepared_request() {
    let mut client = client();
    advance_to_prepared(&mut client);
    let failure = RequestFailure::new(RequestFailureStage::Dispatch, 12);
    assert_eq!(
        client.request_dispatch_failed(OTHER_LINK, REQUEST, failure),
        ObservationDisposition::Unrelated
    );
    assert_eq!(
        client.request_dispatch_failed(LINK, OTHER_REQUEST, failure),
        ObservationDisposition::Unrelated
    );
    assert_eq!(
        client.request_dispatch_failed(LINK, REQUEST, failure),
        ObservationDisposition::Applied
    );
    assert_eq!(
        client.failure(),
        Some(FetchFailure::Request {
            link: LINK,
            request: Some(REQUEST),
            failure,
        })
    );
}

#[test]
fn dispatch_confirmation_rollback_keeps_nomad_prepared_until_native_cancel() {
    let mut client = client();
    advance_to_prepared(&mut client);

    assert_eq!(
        client.confirm_request_dispatch(LINK, OTHER_REQUEST, MonotonicMillis::new(1_000)),
        Err(ControlError::RequestMismatch)
    );
    assert_eq!(client.phase(), FetchPhase::AwaitingDispatchConfirmation);
    assert_eq!(
        client.prepared_request(),
        Some(PreparedRequest {
            destination: DESTINATION,
            link: LINK,
            request: REQUEST,
            path: PagePath::index(),
        })
    );

    // The adapter cancels its already-confirmed native request here. Only
    // after that external rollback succeeds does the exact failure enter
    // Nomad state.
    let failure = RequestFailure::new(RequestFailureStage::Dispatch, 31);
    assert_eq!(
        client.request_dispatch_failed(LINK, REQUEST, failure),
        ObservationDisposition::Applied
    );
    assert_eq!(
        client.failure(),
        Some(FetchFailure::Request {
            link: LINK,
            request: Some(REQUEST),
            failure,
        })
    );
}

#[test]
fn exact_link_close_invalidates_cache_and_active_fetch() {
    let mut client = client();
    advance_to_request(&mut client);
    assert_eq!(
        client.link_closed(OTHER_LINK, 1),
        ObservationDisposition::Unrelated
    );
    assert!(client.cached_link().is_some());
    assert_eq!(
        client.link_closed(LINK, 13),
        ObservationDisposition::Applied
    );
    assert_eq!(client.cached_link(), None);
    assert_eq!(
        client.failure(),
        Some(FetchFailure::Link {
            destination: DESTINATION,
            failure: LinkFailure::new(LinkFailureStage::Closed, 13),
        })
    );
}

#[test]
fn unavailable_request_link_invalidates_cache_before_later_retry() {
    let mut client = client();
    advance_to_request(&mut client);
    assert_eq!(
        client.cached_link(),
        Some(CachedLink::new(DESTINATION, LINK))
    );

    assert_eq!(
        client.request_link_unavailable(OTHER_LINK, 40),
        ObservationDisposition::Unrelated
    );
    assert_eq!(
        client.cached_link(),
        Some(CachedLink::new(DESTINATION, LINK))
    );
    assert_eq!(
        client.request_link_unavailable(LINK, 41),
        ObservationDisposition::Applied
    );
    assert_eq!(client.cached_link(), None);
    assert_eq!(
        client.failure(),
        Some(FetchFailure::Link {
            destination: DESTINATION,
            failure: LinkFailure::new(LinkFailureStage::Unavailable, 41),
        })
    );

    assert!(matches!(
        client.take_outcome(),
        Some(FetchOutcome::Failed(FetchFailure::Link { .. }))
    ));
    client.start(DESTINATION, PagePath::index()).unwrap();
    assert_eq!(
        client.action(),
        Some(FetchAction::RequestPath {
            destination: DESTINATION,
        })
    );
}

#[test]
fn pending_link_terminal_observations_require_the_prepared_link() {
    let mut client = client();
    client.start(DESTINATION, PagePath::index()).unwrap();
    client.confirm_path_request(DESTINATION).unwrap();
    assert_eq!(
        client.path_available(DESTINATION),
        ObservationDisposition::Applied
    );
    client.confirm_link_request(DESTINATION, LINK).unwrap();

    let failure = LinkFailure::new(LinkFailureStage::Dispatch, 19);
    assert_eq!(
        client.link_failed(DESTINATION, OTHER_LINK, failure),
        ObservationDisposition::Unrelated
    );
    assert_eq!(
        client.link_closed(OTHER_LINK, 20),
        ObservationDisposition::Unrelated
    );
    assert_eq!(client.phase(), FetchPhase::LinkEstablishment);
    assert_eq!(
        client.link_closed(LINK, 21),
        ObservationDisposition::Applied
    );
    assert_eq!(
        client.failure(),
        Some(FetchFailure::Link {
            destination: DESTINATION,
            failure: LinkFailure::new(LinkFailureStage::Closed, 21),
        })
    );
}

#[test]
fn closing_an_idle_cached_link_only_forgets_the_cache() {
    let mut client = client();
    client
        .seed_cached_link(CachedLink::new(DESTINATION, LINK))
        .unwrap();
    assert_eq!(client.link_closed(LINK, 0), ObservationDisposition::Applied);
    assert_eq!(client.cached_link(), None);
    assert_eq!(client.phase(), FetchPhase::Idle);
}

#[test]
fn timeout_deadline_saturates_at_maximum_time() {
    let mut client = client();
    advance_to_prepared(&mut client);
    client
        .confirm_request_dispatch(
            LINK,
            REQUEST,
            MonotonicMillis::new(u64::MAX.saturating_sub(10)),
        )
        .unwrap();
    assert_eq!(
        client.request_timeout_candidate(MonotonicMillis::new(u64::MAX - 1)),
        None
    );
    let due = client
        .request_timeout_candidate(MonotonicMillis::new(u64::MAX))
        .unwrap();
    assert_eq!(due.deadline(), MonotonicMillis::new(u64::MAX));
    client.confirm_request_timeout(due).unwrap();
    assert!(matches!(
        client.failure(),
        Some(FetchFailure::Timeout {
            deadline: MonotonicMillis(u64::MAX),
            ..
        })
    ));
}

#[test]
fn timeout_confirmation_rejects_wrong_phase_and_exact_candidate_mismatches() {
    let mut source = client();
    advance_to_response(&mut source);
    let exact = source
        .request_timeout_candidate(MonotonicMillis::new(1_100))
        .unwrap();

    let mut wrong_phase = client();
    assert_eq!(
        wrong_phase.confirm_request_timeout(exact),
        Err(ControlError::WrongPhase)
    );

    let mut correlation = client();
    advance_to_response(&mut correlation);
    let mismatches = [
        (
            RequestTimeoutCandidate {
                link: OTHER_LINK,
                ..exact
            },
            ControlError::LinkMismatch,
        ),
        (
            RequestTimeoutCandidate {
                request: OTHER_REQUEST,
                ..exact
            },
            ControlError::RequestMismatch,
        ),
        (
            RequestTimeoutCandidate {
                dispatched_at: MonotonicMillis::new(999),
                ..exact
            },
            ControlError::DispatchTimeMismatch,
        ),
        (
            RequestTimeoutCandidate {
                deadline: MonotonicMillis::new(1_101),
                ..exact
            },
            ControlError::DeadlineMismatch,
        ),
    ];
    for (candidate, expected) in mismatches {
        assert_eq!(
            correlation.confirm_request_timeout(candidate),
            Err(expected)
        );
        assert_eq!(correlation.phase(), FetchPhase::AwaitingResponse);
    }

    correlation.confirm_request_timeout(exact).unwrap();
    assert!(matches!(
        correlation.failure(),
        Some(FetchFailure::Timeout { .. })
    ));
}
