use std::collections::VecDeque;
use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use reticulum_appliance_store::{
    AcceptanceIds, IdempotencyKey, InboundMessage, MessageId, SubmissionId, UnixTimestampMillis,
};
use reticulum_appliance_sync::{InboxCursor, InboxSummary};

use super::*;

static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
type FakeInbox = Vec<(InboxSummary, InboundMessage)>;
type ConnectOutcome = Result<(DeviceBinding, FakeInbox), String>;

fn empty_radio_trace_page() -> reticulum_device_api::RadioTracePage {
    let profile = reticulum_device_api::RadioTraceAppliedLoraProfile::new(
        [0x51; 16],
        915_000_000,
        125_000,
        8,
        22,
        10,
        5,
        true,
        true,
        false,
    );
    reticulum_device_api::RadioTracePage::new(1, profile, 1, 1, false, [None, None], None).unwrap()
}

struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "reticulum-appliance-{label}-{}-{nonce}-{sequence}.sqlite3",
            std::process::id()
        )))
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let _ = fs::remove_file(self.0.with_extension("sqlite3-shm"));
        let _ = fs::remove_file(self.0.with_extension("sqlite3-wal"));
    }
}

fn destination(tag: u8) -> DestinationHash {
    DestinationHash::new([tag; 16])
}

fn binding(tag: u8) -> DeviceBinding {
    DeviceBinding::new(
        [tag; 16],
        destination(tag.wrapping_add(1)),
        destination(tag.wrapping_add(2)),
    )
}

fn outbound(tag: u8) -> OutboxMaterial {
    OutboxMaterial::new(
        destination(tag),
        UnixTimestampMillis::new(1_000 + u64::from(tag)).unwrap(),
        IdempotencyKey::new([tag; 16]),
        b"title".to_vec(),
        vec![tag],
    )
}

fn message_location(latitude_e6: i32, updated_at_unix_seconds: u32) -> MessageLocation {
    MessageLocation::new(
        latitude_e6,
        -72_345_678,
        12_345,
        678,
        27_050,
        950,
        updated_at_unix_seconds,
    )
    .unwrap()
}

fn phone_location(latitude_e6: i32, captured_at_unix_ms: u64) -> PhoneLocationObservationView {
    PhoneLocationObservationView::Available {
        latitude_e6,
        longitude_e6: -72_345_678,
        altitude_mm: Some(12_345),
        horizontal_accuracy_mm: Some(5_500),
        vertical_accuracy_mm: Some(8_500),
        captured_at_unix_ms,
        authorization: PhoneLocationAuthorizationView::Precise,
        source: PhoneLocationSourceView::ForegroundStream,
        mocked: Some(false),
    }
}

fn inbox(handle: u64, tag: u8) -> (InboxSummary, InboundMessage) {
    let id = MessageId::new([tag; 32]);
    (
        InboxSummary::new(InboxCursor::new(handle).unwrap(), id),
        InboundMessage::new(
            id,
            destination(0xa0),
            destination(tag),
            UnixTimestampMillis::new(2_000 + u64::from(tag)).unwrap(),
            b"inbound".to_vec(),
            vec![tag],
        ),
    )
}

#[test]
fn connection_bearer_is_part_of_the_transport_neutral_projection() {
    let state = ConnectionState::Ready {
        transport: ConnectionTransport::UsbSerial,
        endpoint: "/dev/cu.usbmodem-test".to_owned(),
        device_label: "001122334455".to_owned(),
    };
    assert!(matches!(
        state.transport(),
        Some(ConnectionTransport::UsbSerial)
    ));
    assert_eq!(state.endpoint(), Some("/dev/cu.usbmodem-test"));
    assert_eq!(state.device_label(), Some("001122334455"));
    assert_eq!(
        serde_json::to_value(state).unwrap(),
        serde_json::json!({
            "state": "ready",
            "transport": "usb_serial",
            "endpoint": "/dev/cu.usbmodem-test",
            "device_label": "001122334455",
        })
    );
}

#[test]
fn shared_requests_validate_and_preserve_the_existing_json_contract() {
    let contact: ContactRequest =
        serde_json::from_value(serde_json::json!({ "name": "Field node" })).unwrap();
    let contact = contact.into_contact(&"ab".repeat(16)).unwrap();
    assert_eq!(contact.destination(), destination(0xab));
    assert_eq!(contact.display_name(), "Field node");
    assert_eq!(
        serde_json::to_value(MutationResponse::from(ContactUpsertOutcome::Inserted)).unwrap(),
        serde_json::json!({ "outcome": "inserted" })
    );

    let send: SendRequest = serde_json::from_value(serde_json::json!({
        "destination": "cd".repeat(16),
        "timestamp_ms": 1_234,
        "idempotency_key": "ef".repeat(16),
        "title": "hello",
        "content": "mesh",
    }))
    .unwrap();
    let material = send.into_material().unwrap();
    assert_eq!(material.destination(), destination(0xcd));
    assert_eq!(material.timestamp().get(), 1_234);
    assert_eq!(material.idempotency_key().as_bytes(), &[0xef; 16]);
    assert_eq!(material.title(), b"hello");
    assert_eq!(material.content(), b"mesh");
    assert_eq!(material.location(), None);

    let located_send: SendRequest = serde_json::from_value(serde_json::json!({
        "destination": "cd".repeat(16),
        "timestamp_ms": 1_235,
        "idempotency_key": "ee".repeat(16),
        "title": "located",
        "content": "mesh",
        "location": {
            "latitude_e6": 43_123_456,
            "longitude_e6": -72_345_678,
            "altitude_cm": 12_345,
            "speed_cm_per_second": 678,
            "bearing_centidegrees": 27_050,
            "accuracy_cm": 950,
            "updated_at_unix_seconds": 1_784_000_001,
        },
    }))
    .unwrap();
    assert_eq!(
        located_send.into_material().unwrap().location(),
        Some(message_location(43_123_456, 1_784_000_001))
    );

    let invalid_location: SendRequest = serde_json::from_value(serde_json::json!({
        "destination": "cd".repeat(16),
        "timestamp_ms": 1_236,
        "idempotency_key": "ed".repeat(16),
        "title": "invalid",
        "content": "mesh",
        "location": {
            "latitude_e6": 90_000_001,
            "longitude_e6": 0,
            "altitude_cm": 0,
            "speed_cm_per_second": 0,
            "bearing_centidegrees": 0,
            "accuracy_cm": 0,
            "updated_at_unix_seconds": 0,
        },
    }))
    .unwrap();
    assert_eq!(
        invalid_location.into_material(),
        Err(ClientRequestError::InvalidMessageLocation)
    );

    let located_at_direct_limit: SendRequest = serde_json::from_value(serde_json::json!({
        "destination": "cd".repeat(16),
        "timestamp_ms": 1_237,
        "idempotency_key": "ec".repeat(16),
        "title": "",
        "content": "x".repeat(268),
        "location": {
            "latitude_e6": 43_123_456,
            "longitude_e6": -72_345_678,
            "altitude_cm": 12_345,
            "speed_cm_per_second": 678,
            "bearing_centidegrees": 27_050,
            "accuracy_cm": 950,
            "updated_at_unix_seconds": 1_784_000_001,
        },
    }))
    .unwrap();
    assert!(located_at_direct_limit.into_material().is_ok());

    let located_over_direct_limit: SendRequest = serde_json::from_value(serde_json::json!({
        "destination": "cd".repeat(16),
        "timestamp_ms": 1_238,
        "idempotency_key": "eb".repeat(16),
        "title": "",
        "content": "x".repeat(269),
        "location": {
            "latitude_e6": 43_123_456,
            "longitude_e6": -72_345_678,
            "altitude_cm": 12_345,
            "speed_cm_per_second": 678,
            "bearing_centidegrees": 27_050,
            "accuracy_cm": 950,
            "updated_at_unix_seconds": 1_784_000_001,
        },
    }))
    .unwrap();
    assert_eq!(
        located_over_direct_limit.into_material(),
        Err(ClientRequestError::MessageTooLarge)
    );

    let combined_unlocated_over_direct_limit: SendRequest =
        serde_json::from_value(serde_json::json!({
            "destination": "cd".repeat(16),
            "timestamp_ms": 1_239,
            "idempotency_key": "ea".repeat(16),
            "title": "t".repeat(MAX_LXMF_BASIC_TITLE_BYTES),
            "content": "c".repeat(MAX_LXMF_BASIC_CONTENT_BYTES),
        }))
        .unwrap();
    assert_eq!(
        combined_unlocated_over_direct_limit.into_material(),
        Err(ClientRequestError::MessageTooLarge)
    );
    assert!(
        serde_json::from_value::<SendRequest>(serde_json::json!({
            "destination": "cd".repeat(16),
            "timestamp_ms": MAX_JSON_SAFE_INTEGER + 1,
            "idempotency_key": "ef".repeat(16),
            "title": "",
            "content": "",
        }))
        .is_err()
    );
}

#[test]
fn timeline_view_projects_a_persisted_message_location() {
    let location = message_location(44_654_321, 1_784_000_002);
    let mut store = SqliteChatStore::open_in_memory().unwrap();
    store
        .commit_outbound(outbound(0x31).with_location(Some(location)))
        .unwrap();
    let entry = store
        .conversation_timeline(destination(0x31))
        .unwrap()
        .pop()
        .unwrap();
    let view = TimelineView::from(entry);

    assert_eq!(view.location(), Some(MessageLocationView::from(location)));
    assert_eq!(
        serde_json::to_value(view).unwrap()["location"],
        serde_json::json!({
            "latitude_e6": 44_654_321,
            "longitude_e6": -72_345_678,
            "altitude_cm": 12_345,
            "speed_cm_per_second": 678,
            "bearing_centidegrees": 27_050,
            "accuracy_cm": 950,
            "updated_at_unix_seconds": 1_784_000_002,
        })
    );
}

fn empty_mailbox_status() -> reticulum_device_api::LxmfMailboxStatus {
    reticulum_device_api::LxmfMailboxStatus::new(None, None).unwrap()
}

fn acknowledged_mailbox_status(through: InboxCursor) -> reticulum_device_api::LxmfMailboxStatus {
    let handle = reticulum_device_api::LxmfMessageHandle::new(through.get()).unwrap();
    reticulum_device_api::LxmfMailboxStatus::new(Some(handle), Some(handle)).unwrap()
}

struct FakeSession {
    binding: DeviceBinding,
    inbox: Vec<(InboxSummary, InboundMessage)>,
    submitted: Arc<Mutex<Vec<OutboxMaterial>>>,
    status: SubmissionState,
    nearby_generation: Option<Arc<AtomicU64>>,
}

impl LxmfSession for FakeSession {
    type Error = DeviceSessionError;

    fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
        Ok(self.binding)
    }

    fn submit(&mut self, material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
        let mut submitted = self.submitted.lock().unwrap();
        submitted.push(material.clone());
        let id = SubmissionId::new(submitted.len() as u64).unwrap();
        Ok(AcceptanceIds::new(
            id,
            MessageId::new([material.content()[0]; 32]),
        ))
    }

    fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
        Ok(self.status)
    }

    fn next_inbox(
        &mut self,
        after: Option<InboxCursor>,
    ) -> Result<Option<InboxSummary>, Self::Error> {
        let after = after.map_or(0, InboxCursor::get);
        Ok(self
            .inbox
            .iter()
            .map(|(summary, _)| *summary)
            .find(|summary| summary.cursor().get() > after))
    }

    fn read_inbox(&mut self, summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
        Ok(self
            .inbox
            .iter()
            .find(|(candidate, _)| *candidate == summary)
            .unwrap()
            .1
            .clone())
    }

    fn inbox_status(&mut self) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
        Ok(empty_mailbox_status())
    }

    fn acknowledge_inbox_through(
        &mut self,
        through: InboxCursor,
    ) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
        Ok(acknowledged_mailbox_status(through))
    }

    fn next_nearby_peer(
        &mut self,
        _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
    ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
        let generation = self
            .nearby_generation
            .as_ref()
            .map(|generation| generation.load(Ordering::Relaxed))
            .expect("ordinary actor tests do not request nearby peers");
        let generation = reticulum_device_api::LxmfPeerGeneration::new(generation)
            .expect("test nearby generations are nonzero");
        let incarnation = reticulum_device_api::LxmfPeerDiscoveryIncarnation::new([0x7a; 8]);
        Ok(reticulum_device_api::LxmfPeerDiscoveryPage::new(
            reticulum_device_api::LxmfPeerDiscoveryCursor::new(incarnation, generation.get()),
            Some(generation),
            None,
            false,
            None,
        ))
    }

    fn nomad_fetch_start(
        &mut self,
        _request: reticulum_device_api::NomadFetchStartRequest<'_>,
    ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
        unreachable!("ordinary actor tests do not request NomadNet fetches")
    }

    fn nomad_fetch_poll(
        &mut self,
        _id: reticulum_device_api::NomadFetchId,
    ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
        unreachable!("ordinary actor tests do not request NomadNet fetches")
    }

    fn reticulum_probe_start(
        &mut self,
        _request: reticulum_device_api::ProbeStartRequest,
    ) -> Result<reticulum_device_api::ProbeStartAccepted, Self::Error> {
        unreachable!("ordinary actor tests do not request Reticulum probes")
    }

    fn reticulum_probe_poll(
        &mut self,
        _id: reticulum_device_api::ProbeId,
    ) -> Result<reticulum_device_api::ProbePollResponse, Self::Error> {
        unreachable!("ordinary actor tests do not request Reticulum probes")
    }

    fn network_config_get(
        &mut self,
    ) -> Result<reticulum_device_api::NetworkConfigSnapshot, Self::Error> {
        unreachable!("ordinary actor tests do not request network configuration")
    }

    fn network_config_mutate(
        &mut self,
        _request: reticulum_device_api::NetworkConfigMutationRequest<'_>,
    ) -> Result<reticulum_device_api::NetworkConfigMutationOutcome, Self::Error> {
        unreachable!("ordinary actor tests do not mutate network configuration")
    }

    fn network_status(
        &mut self,
    ) -> Result<reticulum_device_api::NetworkRuntimeStatus, Self::Error> {
        unreachable!("ordinary actor tests do not request network status")
    }

    fn manual_service_announce(
        &mut self,
    ) -> Result<reticulum_device_api::ManualServiceAnnounceDisposition, Self::Error> {
        unreachable!("ordinary actor tests do not request service announces")
    }

    fn node_diagnostics(
        &mut self,
    ) -> Result<reticulum_device_api::NodeDiagnosticsSnapshot, Self::Error> {
        unreachable!("ordinary actor tests do not request node diagnostics")
    }

    fn route_diagnostics_page(
        &mut self,
        _request: reticulum_device_api::RouteDiagnosticsRequest,
    ) -> Result<reticulum_device_api::RouteDiagnosticsPage, Self::Error> {
        unreachable!("ordinary actor tests do not request route diagnostics")
    }

    fn radio_trace_page(
        &mut self,
        _request: reticulum_device_api::RadioTracePageRequest,
    ) -> Result<reticulum_device_api::RadioTracePage, Self::Error> {
        Ok(empty_radio_trace_page())
    }

    fn is_usable(&self) -> bool {
        true
    }
}

struct FakeConnector {
    outcomes: VecDeque<ConnectOutcome>,
    attempts: Arc<AtomicUsize>,
    submitted: Arc<Mutex<Vec<OutboxMaterial>>>,
    status: SubmissionState,
    nearby_generation: Option<Arc<AtomicU64>>,
}

#[derive(Debug, Eq, PartialEq)]
struct ObservedNomadStart {
    destination: [u8; 16],
    path: String,
    timestamp_unix_ms: u64,
    idempotency_key: [u8; 16],
}

#[derive(Default)]
struct NomadTrace {
    starts: Vec<ObservedNomadStart>,
    polls: Vec<reticulum_device_api::NomadFetchId>,
    probe_starts: Vec<ObservedProbeStart>,
    probe_polls: Vec<reticulum_device_api::ProbeId>,
}

#[derive(Debug, Eq, PartialEq)]
struct ObservedProbeStart {
    destination: [u8; 16],
    idempotency_key: [u8; 16],
}

struct NomadSession {
    trace: Arc<Mutex<NomadTrace>>,
}

impl LxmfSession for NomadSession {
    type Error = DeviceSessionError;

    fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
        Ok(binding(0x81))
    }

    fn submit(&mut self, _material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
        unreachable!("the Nomad actor test has no outbox work")
    }

    fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
        unreachable!("the Nomad actor test has no accepted submissions")
    }

    fn next_inbox(
        &mut self,
        _after: Option<InboxCursor>,
    ) -> Result<Option<InboxSummary>, Self::Error> {
        Ok(None)
    }

    fn read_inbox(&mut self, _summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
        unreachable!("the Nomad actor test has no inbox messages")
    }

    fn inbox_status(&mut self) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
        Ok(empty_mailbox_status())
    }

    fn acknowledge_inbox_through(
        &mut self,
        through: InboxCursor,
    ) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
        Ok(acknowledged_mailbox_status(through))
    }

    fn next_nearby_peer(
        &mut self,
        _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
    ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
        unreachable!("the Nomad actor test does not request nearby peers")
    }

    fn nomad_fetch_start(
        &mut self,
        request: reticulum_device_api::NomadFetchStartRequest<'_>,
    ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
        self.trace.lock().unwrap().starts.push(ObservedNomadStart {
            destination: request.destination().0,
            path: request.path().as_str().to_owned(),
            timestamp_unix_ms: request.timestamp_unix_ms().get(),
            idempotency_key: request.idempotency_key().0,
        });
        Ok(reticulum_device_api::NomadFetchStartAccepted {
            id: reticulum_device_api::NomadFetchId::new([0x82; 8], 1).unwrap(),
            outcome: reticulum_device_api::NomadFetchStartOutcome::Accepted,
        })
    }

    fn nomad_fetch_poll(
        &mut self,
        id: reticulum_device_api::NomadFetchId,
    ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
        self.trace.lock().unwrap().polls.push(id);
        Ok(reticulum_device_api::NomadFetchPollResponse::Ready(
            reticulum_device_api::NomadPage::new(b">Metalbeard").unwrap(),
        ))
    }

    fn reticulum_probe_start(
        &mut self,
        request: reticulum_device_api::ProbeStartRequest,
    ) -> Result<reticulum_device_api::ProbeStartAccepted, Self::Error> {
        self.trace
            .lock()
            .unwrap()
            .probe_starts
            .push(ObservedProbeStart {
                destination: request.destination().0,
                idempotency_key: request.idempotency_key().0,
            });
        Ok(reticulum_device_api::ProbeStartAccepted::new(
            reticulum_device_api::ProbeId::new([0x85; 16]).unwrap(),
            reticulum_device_api::ProbeStartOutcome::Accepted,
        ))
    }

    fn reticulum_probe_poll(
        &mut self,
        id: reticulum_device_api::ProbeId,
    ) -> Result<reticulum_device_api::ProbePollResponse, Self::Error> {
        self.trace.lock().unwrap().probe_polls.push(id);
        Ok(reticulum_device_api::ProbePollResponse::Succeeded(
            reticulum_device_api::ProbeSuccess::new(
                1_234,
                2,
                reticulum_device_api::IngressObservation::new(
                    7,
                    Some(reticulum_device_api::IngressSignal::new(-91, 7)),
                ),
            ),
        ))
    }

    fn network_config_get(
        &mut self,
    ) -> Result<reticulum_device_api::NetworkConfigSnapshot, Self::Error> {
        unreachable!("the Nomad actor test does not request network configuration")
    }

    fn network_config_mutate(
        &mut self,
        _request: reticulum_device_api::NetworkConfigMutationRequest<'_>,
    ) -> Result<reticulum_device_api::NetworkConfigMutationOutcome, Self::Error> {
        unreachable!("the Nomad actor test does not mutate network configuration")
    }

    fn network_status(
        &mut self,
    ) -> Result<reticulum_device_api::NetworkRuntimeStatus, Self::Error> {
        unreachable!("the Nomad actor test does not request network status")
    }

    fn manual_service_announce(
        &mut self,
    ) -> Result<reticulum_device_api::ManualServiceAnnounceDisposition, Self::Error> {
        unreachable!("the Nomad actor test does not request service announces")
    }

    fn node_diagnostics(
        &mut self,
    ) -> Result<reticulum_device_api::NodeDiagnosticsSnapshot, Self::Error> {
        unreachable!("the Nomad actor test does not request node diagnostics")
    }

    fn route_diagnostics_page(
        &mut self,
        _request: reticulum_device_api::RouteDiagnosticsRequest,
    ) -> Result<reticulum_device_api::RouteDiagnosticsPage, Self::Error> {
        unreachable!("the Nomad actor test does not request route diagnostics")
    }

    fn radio_trace_page(
        &mut self,
        _request: reticulum_device_api::RadioTracePageRequest,
    ) -> Result<reticulum_device_api::RadioTracePage, Self::Error> {
        Ok(empty_radio_trace_page())
    }

    fn is_usable(&self) -> bool {
        true
    }
}

struct NomadConnector {
    trace: Arc<Mutex<NomadTrace>>,
    connected: bool,
}

impl Connector for NomadConnector {
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
        if self.connected {
            return Err(ConnectFailure::retryable(
                "test session was already claimed",
            ));
        }
        self.connected = true;
        Ok(ConnectedSession::new(
            NomadSession {
                trace: self.trace.clone(),
            },
            ConnectionMetadata::new(ConnectionTransport::UsbSerial, "/dev/fake", "001122334455"),
        ))
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ObservedNetworkMutation {
    profile_id: [u8; 16],
    enabled: bool,
    priority: u8,
    ssid: Vec<u8>,
    replacement_passphrase_len: Option<usize>,
    expected_revision: u64,
    idempotency_key: [u8; 16],
}

#[derive(Default)]
struct NetworkTrace {
    announces: usize,
    config_reads: usize,
    status_reads: usize,
    diagnostics_reads: usize,
    route_reads: usize,
    mutations: Vec<ObservedNetworkMutation>,
}

struct NetworkSession {
    trace: Arc<Mutex<NetworkTrace>>,
}

impl LxmfSession for NetworkSession {
    type Error = DeviceSessionError;

    fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
        Ok(binding(0x91))
    }

    fn submit(&mut self, _material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
        unreachable!("the network actor test has no outbox work")
    }

    fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
        unreachable!("the network actor test has no accepted submissions")
    }

    fn next_inbox(
        &mut self,
        _after: Option<InboxCursor>,
    ) -> Result<Option<InboxSummary>, Self::Error> {
        Ok(None)
    }

    fn read_inbox(&mut self, _summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
        unreachable!("the network actor test has no inbox messages")
    }

    fn inbox_status(&mut self) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
        Ok(empty_mailbox_status())
    }

    fn acknowledge_inbox_through(
        &mut self,
        through: InboxCursor,
    ) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
        Ok(acknowledged_mailbox_status(through))
    }

    fn next_nearby_peer(
        &mut self,
        _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
    ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
        unreachable!("the network actor test does not request nearby peers")
    }

    fn nomad_fetch_start(
        &mut self,
        _request: reticulum_device_api::NomadFetchStartRequest<'_>,
    ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
        unreachable!("the network actor test does not request NomadNet pages")
    }

    fn nomad_fetch_poll(
        &mut self,
        _id: reticulum_device_api::NomadFetchId,
    ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
        unreachable!("the network actor test does not request NomadNet pages")
    }

    fn reticulum_probe_start(
        &mut self,
        _request: reticulum_device_api::ProbeStartRequest,
    ) -> Result<reticulum_device_api::ProbeStartAccepted, Self::Error> {
        unreachable!("the network actor test does not request Reticulum probes")
    }

    fn reticulum_probe_poll(
        &mut self,
        _id: reticulum_device_api::ProbeId,
    ) -> Result<reticulum_device_api::ProbePollResponse, Self::Error> {
        unreachable!("the network actor test does not request Reticulum probes")
    }

    fn network_config_get(
        &mut self,
    ) -> Result<reticulum_device_api::NetworkConfigSnapshot, Self::Error> {
        self.trace.lock().unwrap().config_reads += 1;
        let profile_id = reticulum_device_api::WifiNetworkProfileId::new([0x92; 16]).unwrap();
        let profile = reticulum_device_api::WifiNetworkConfigSummary::new(
            profile_id,
            true,
            220,
            b"mesh\xff",
            true,
        )
        .unwrap();
        Ok(reticulum_device_api::NetworkConfigSnapshot::with_defaults(
            5,
            [Some(profile), None, None, None],
            None,
        )
        .unwrap())
    }

    fn network_config_mutate(
        &mut self,
        request: reticulum_device_api::NetworkConfigMutationRequest<'_>,
    ) -> Result<reticulum_device_api::NetworkConfigMutationOutcome, Self::Error> {
        let reticulum_device_api::NetworkConfigMutation::UpsertWifi {
            profile_id,
            network,
        } = request.mutation()
        else {
            unreachable!("the network actor test submits a Wi-Fi upsert")
        };
        self.trace
            .lock()
            .unwrap()
            .mutations
            .push(ObservedNetworkMutation {
                profile_id: *profile_id.as_bytes(),
                enabled: network.enabled(),
                priority: network.priority(),
                ssid: network.ssid().as_bytes().to_vec(),
                replacement_passphrase_len: network.credential().replacement().map(<[u8]>::len),
                expected_revision: request.expected_revision(),
                idempotency_key: request.idempotency_key().0,
            });
        Ok(
            reticulum_device_api::NetworkConfigMutationOutcome::Applied {
                revision: 6,
                reboot_required: true,
            },
        )
    }

    fn network_status(
        &mut self,
    ) -> Result<reticulum_device_api::NetworkRuntimeStatus, Self::Error> {
        self.trace.lock().unwrap().status_reads += 1;
        Ok(reticulum_device_api::NetworkRuntimeStatus::new(
            6,
            5,
            reticulum_device_api::WifiStationState::Connected,
            Some(reticulum_device_api::WifiNetworkProfileId::new([0x92; 16]).unwrap()),
            Some(b"mesh\xff"),
            Some([192, 0, 2, 33]),
            Some(-68),
            reticulum_device_api::ReticulumTcpPeerState::WaitingForNetwork,
        )
        .unwrap())
    }

    fn manual_service_announce(
        &mut self,
    ) -> Result<reticulum_device_api::ManualServiceAnnounceDisposition, Self::Error> {
        self.trace.lock().unwrap().announces += 1;
        Ok(reticulum_device_api::ManualServiceAnnounceDisposition::Queued)
    }

    fn node_diagnostics(
        &mut self,
    ) -> Result<reticulum_device_api::NodeDiagnosticsSnapshot, Self::Error> {
        self.trace.lock().unwrap().diagnostics_reads += 1;
        Ok(reticulum_device_api::NodeDiagnosticsSnapshot::new(
            4_500,
            [
                Some(reticulum_device_api::DiagnosticInterfaceRecord::new(
                    1,
                    reticulum_device_api::DiagnosticInterfaceKind::LoRa,
                    reticulum_device_api::DiagnosticInterfaceState::Online,
                    2,
                    500,
                    Some(5_470),
                )),
                None,
                None,
                None,
            ],
            Some(reticulum_device_api::LoraDiagnostics::new(
                22,
                915_000_000,
                125_000,
                7,
                5,
                8,
                7,
                1,
                0,
                3,
                2,
                5,
                1,
                0,
                4,
                9,
                Some(reticulum_device_api::DiagnosticLoraLastRx::new(500, -91, 7)),
                Some(reticulum_device_api::DiagnosticLoraLastTx::data(
                    700,
                    reticulum_device_api::DiagnosticLoraTxOutcome::Completed,
                    reticulum_device_api::DiagnosticLoraDataTxEvidence::try_new(
                        1,
                        183,
                        reticulum_device_api::EncodedPacketSha256::new([0xab; 32]),
                    )
                    .unwrap(),
                )),
                None,
            )),
            reticulum_device_api::RnsDiagnostics::new(9, 2, 1, 0, 4, 1, 0, 0, 0, 0, 1),
            3,
            1,
            1,
        ))
    }

    fn route_diagnostics_page(
        &mut self,
        request: reticulum_device_api::RouteDiagnosticsRequest,
    ) -> Result<reticulum_device_api::RouteDiagnosticsPage, Self::Error> {
        self.trace.lock().unwrap().route_reads += 1;
        assert_eq!(request.after(), None);
        let route = reticulum_device_api::RouteDiagnosticEntry::new(
            reticulum_device_api::DestinationHash([0x95; 16]),
            Some(reticulum_device_api::IdentityHash::new([0x96; 16])),
            2,
            Some(1),
            reticulum_device_api::RouteDiagnosticResolution::ExactReady,
            Some(1_200),
            Some(400),
            Some(28_800),
        );
        Ok(reticulum_device_api::RouteDiagnosticsPage::new(
            1,
            1,
            [Some(route), None, None, None],
            None,
        )
        .unwrap())
    }

    fn radio_trace_page(
        &mut self,
        _request: reticulum_device_api::RadioTracePageRequest,
    ) -> Result<reticulum_device_api::RadioTracePage, Self::Error> {
        Ok(empty_radio_trace_page())
    }

    fn is_usable(&self) -> bool {
        true
    }
}

struct NetworkConnector {
    trace: Arc<Mutex<NetworkTrace>>,
    connected: bool,
}

impl Connector for NetworkConnector {
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
        if self.connected {
            return Err(ConnectFailure::retryable(
                "test session was already claimed",
            ));
        }
        self.connected = true;
        Ok(ConnectedSession::new(
            NetworkSession {
                trace: self.trace.clone(),
            },
            ConnectionMetadata::new(ConnectionTransport::UsbSerial, "/dev/fake", "001122334455"),
        ))
    }
}

struct UnavailableConnector {
    attempts: Arc<AtomicUsize>,
}

impl Connector for UnavailableConnector {
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
        self.attempts.fetch_add(1, Ordering::Relaxed);
        Err(ConnectFailure::unavailable(
            ConnectionTransport::BluetoothLowEnergy,
            "BLE adapter is not implemented",
        ))
    }
}

struct CountingLease(Arc<AtomicUsize>);

impl Drop for CountingLease {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

struct LeaseConnector {
    connected: bool,
    lease_drops: Arc<AtomicUsize>,
}

impl Connector for LeaseConnector {
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
        if self.connected {
            return Err(ConnectFailure::retryable("device remains disconnected"));
        }
        self.connected = true;
        Ok(ConnectedSession::new(
            FakeSession {
                binding: binding(0x71),
                inbox: Vec::new(),
                submitted: Arc::new(Mutex::new(Vec::new())),
                status: SubmissionState::Queued,
                nearby_generation: None,
            },
            ConnectionMetadata::new(ConnectionTransport::UsbSerial, "/dev/fake", "001122334455"),
        )
        .with_connection_lease(CountingLease(self.lease_drops.clone())))
    }
}

struct DropOrderSession {
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl Drop for DropOrderSession {
    fn drop(&mut self) {
        self.order.lock().unwrap().push("session");
    }
}

impl LxmfSession for DropOrderSession {
    type Error = DeviceSessionError;

    fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn submit(&mut self, _material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn next_inbox(
        &mut self,
        _after: Option<InboxCursor>,
    ) -> Result<Option<InboxSummary>, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn read_inbox(&mut self, _summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn inbox_status(&mut self) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn acknowledge_inbox_through(
        &mut self,
        _through: InboxCursor,
    ) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn next_nearby_peer(
        &mut self,
        _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
    ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn nomad_fetch_start(
        &mut self,
        _request: reticulum_device_api::NomadFetchStartRequest<'_>,
    ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn nomad_fetch_poll(
        &mut self,
        _id: reticulum_device_api::NomadFetchId,
    ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn reticulum_probe_start(
        &mut self,
        _request: reticulum_device_api::ProbeStartRequest,
    ) -> Result<reticulum_device_api::ProbeStartAccepted, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn reticulum_probe_poll(
        &mut self,
        _id: reticulum_device_api::ProbeId,
    ) -> Result<reticulum_device_api::ProbePollResponse, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn network_config_get(
        &mut self,
    ) -> Result<reticulum_device_api::NetworkConfigSnapshot, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn network_config_mutate(
        &mut self,
        _request: reticulum_device_api::NetworkConfigMutationRequest<'_>,
    ) -> Result<reticulum_device_api::NetworkConfigMutationOutcome, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn network_status(
        &mut self,
    ) -> Result<reticulum_device_api::NetworkRuntimeStatus, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn manual_service_announce(
        &mut self,
    ) -> Result<reticulum_device_api::ManualServiceAnnounceDisposition, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn node_diagnostics(
        &mut self,
    ) -> Result<reticulum_device_api::NodeDiagnosticsSnapshot, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn route_diagnostics_page(
        &mut self,
        _request: reticulum_device_api::RouteDiagnosticsRequest,
    ) -> Result<reticulum_device_api::RouteDiagnosticsPage, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn radio_trace_page(
        &mut self,
        _request: reticulum_device_api::RadioTracePageRequest,
    ) -> Result<reticulum_device_api::RadioTracePage, Self::Error> {
        Err(DeviceSessionError::MissingLxmfDeliveryDestination)
    }

    fn is_usable(&self) -> bool {
        false
    }
}

struct DropOrderLease(Arc<Mutex<Vec<&'static str>>>);

impl Drop for DropOrderLease {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("lease");
    }
}

struct BindingFailureConnector {
    returned_session: bool,
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl Connector for BindingFailureConnector {
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
        if self.returned_session {
            return Err(ConnectFailure::retryable("device remains disconnected"));
        }
        self.returned_session = true;
        Ok(ConnectedSession::new(
            DropOrderSession {
                order: self.order.clone(),
            },
            ConnectionMetadata::new(ConnectionTransport::UsbSerial, "/dev/fake", "001122334455"),
        )
        .with_connection_lease(DropOrderLease(self.order.clone())))
    }
}

impl Connector for FakeConnector {
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
        self.attempts.fetch_add(1, Ordering::Relaxed);
        let (binding, inbox) = self
            .outcomes
            .pop_front()
            .unwrap_or_else(|| Err("no scripted connection remains".to_owned()))
            .map_err(ConnectFailure::retryable)?;
        Ok(ConnectedSession::new(
            FakeSession {
                binding,
                inbox,
                submitted: self.submitted.clone(),
                status: self.status,
                nearby_generation: self.nearby_generation.clone(),
            },
            ConnectionMetadata::new(ConnectionTransport::UsbSerial, "/dev/fake", "001122334455"),
        ))
    }
}

fn config(database: &TestDatabase) -> ApplianceConfig {
    let mut config = ApplianceConfig::new(database.0.clone());
    config.reconnect_initial = Duration::from_millis(5);
    config.reconnect_maximum = Duration::from_millis(20);
    config.operation_gap = Duration::from_millis(2);
    config.inbox_poll_interval = Duration::from_millis(5);
    config.status_poll_interval = Duration::from_millis(5);
    config.retry_later_backoff = Duration::from_millis(7);
    config
}

fn connected_fake_actor(
    config: ApplianceConfig,
    store: SqliteChatStore,
    inbox: FakeInbox,
) -> (Actor<FakeConnector>, Arc<Mutex<Vec<OutboxMaterial>>>) {
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let connector = FakeConnector {
        outcomes: VecDeque::from([Ok((binding(0x28), inbox))]),
        attempts: Arc::new(AtomicUsize::new(0)),
        submitted: submitted.clone(),
        status: SubmissionState::Queued,
        nearby_generation: None,
    };
    let (_commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let initial = Arc::new(ApplianceSnapshot::starting());
    let published = Arc::new(RwLock::new(initial.clone()));
    let (revisions, _revision_rx) = watch::channel(initial.revision());
    let mut actor = Actor::new(
        config,
        ChatEngine::new(store),
        connector,
        receiver,
        published,
        revisions,
    );
    actor.connect();
    assert!(actor.session.is_some());
    (actor, submitted)
}

fn background_api_error(
    code: ApiErrorCode,
    operation: reticulum_device_client::Operation,
    wire_operation: u16,
) -> EngineError<SqliteStoreError, DeviceSessionError> {
    EngineError::Session(DeviceSessionError::Client(ClientError::Api {
        operation,
        error: reticulum_device_api::ApiErrorResponse {
            code,
            operation: Some(wire_operation),
        },
    }))
}

#[test]
fn retry_later_and_structural_capacity_use_distinct_bounded_backoffs() {
    let database = TestDatabase::new("api-background-backoffs");
    let config = ApplianceConfig::new(database.0.clone());
    assert_eq!(config.retry_later_backoff, Duration::from_millis(250));
    assert_eq!(
        background_api_retry_delay(&config, ApiErrorCode::RetryLater),
        Some(config.retry_later_backoff)
    );
    assert_eq!(
        background_api_retry_delay(&config, ApiErrorCode::CapacityExhausted),
        Some(config.capacity_backoff)
    );
    assert_ne!(config.retry_later_backoff, config.capacity_backoff);
    assert_eq!(
        background_api_retry_delay(&config, ApiErrorCode::Internal),
        None
    );
    assert_eq!(
        background_api_retry_delay(&config, ApiErrorCode::CapabilityUnavailable),
        None
    );
}

#[test]
fn retryable_reconcile_error_rearms_hot_row_without_dropping_the_session() {
    for (label, code, expected_delay) in [
        (
            "retry-later",
            ApiErrorCode::RetryLater,
            Duration::from_millis(7),
        ),
        (
            "capacity",
            ApiErrorCode::CapacityExhausted,
            Duration::from_millis(43),
        ),
    ] {
        let database = TestDatabase::new(label);
        let mut config = config(&database);
        config.capacity_backoff = Duration::from_millis(43);
        let store = SqliteChatStore::open(&database.0).unwrap();
        let (mut actor, _submitted) = connected_fake_actor(config, store, Vec::new());
        let hot = OutboxId::new(17).unwrap();
        actor.urgent_reconcile_outbox = Some(hot);
        actor.urgent_reconcile_due = false;
        actor.urgent_reconcile_reserves_lane = true;
        let unchanged_inbox = actor.next_inbox;
        let before = Instant::now();

        actor.background_error(
            BackgroundWork::Reconcile,
            background_api_error(
                code,
                reticulum_device_client::Operation::SubmissionStatus,
                reticulum_device_api::OP_SUBMISSION_STATUS,
            ),
        );
        let after = Instant::now();

        assert!(actor.session.is_some());
        assert!(!actor.permanent_fault);
        assert!(matches!(
            actor.snapshot.connection(),
            ConnectionState::Ready { .. }
        ));
        assert_eq!(actor.urgent_reconcile_outbox, Some(hot));
        assert!(actor.urgent_reconcile_due);
        assert!(!actor.urgent_reconcile_reserves_lane);
        assert_eq!(actor.background_error_work, Some(BackgroundWork::Reconcile));
        assert_eq!(actor.next_inbox, unchanged_inbox);
        assert!(actor.next_reconcile >= before + expected_delay);
        assert!(actor.next_reconcile <= after + expected_delay);
    }
}

#[test]
fn retry_later_delays_only_inbox_work_then_retries_on_the_same_session() {
    let database = TestDatabase::new("retry-later-inbox");
    let store = SqliteChatStore::open(&database.0).unwrap();
    let (mut actor, _submitted) = connected_fake_actor(config(&database), store, Vec::new());
    let unchanged_reconcile = actor.next_reconcile;
    let before = Instant::now();
    actor.background_error(
        BackgroundWork::Inbox,
        background_api_error(
            ApiErrorCode::RetryLater,
            reticulum_device_client::Operation::LxmfNext,
            reticulum_device_api::OP_LXMF_NEXT,
        ),
    );
    let after = Instant::now();

    assert!(actor.session.is_some());
    assert!(!actor.permanent_fault);
    assert_eq!(actor.next_reconcile, unchanged_reconcile);
    assert!(actor.next_inbox >= before + Duration::from_millis(7));
    assert!(actor.next_inbox <= after + Duration::from_millis(7));
    assert_eq!(actor.background_error_work, Some(BackgroundWork::Inbox));

    actor.next_inbox = Instant::now();
    actor.inbox_turn();
    assert!(actor.session.is_some());
    assert_eq!(actor.background_error_work, None);
    assert_eq!(actor.snapshot.last_error(), None);
}

#[test]
fn retry_later_reconcile_runs_the_same_durable_row_after_backoff() {
    let database = TestDatabase::new("retry-later-reconcile");
    let mut store = SqliteChatStore::open(&database.0).unwrap();
    let outcome = store.commit_outbound(outbound(0x29)).unwrap();
    let outbox_id = outcome.outbox_id();
    let (mut actor, submitted) = connected_fake_actor(config(&database), store, Vec::new());
    actor.urgent_reconcile_outbox = Some(outbox_id);
    actor.urgent_reconcile_due = false;
    actor.urgent_reconcile_reserves_lane = true;
    actor.background_error(
        BackgroundWork::Reconcile,
        background_api_error(
            ApiErrorCode::RetryLater,
            reticulum_device_client::Operation::LxmfBasicSend,
            reticulum_device_api::OP_LXMF_BASIC_SEND,
        ),
    );

    actor.next_reconcile = Instant::now();
    actor.reconcile_turn();

    assert!(actor.session.is_some());
    assert!(!actor.permanent_fault);
    assert_eq!(submitted.lock().unwrap().as_slice(), &[outbound(0x29)]);
    assert_eq!(actor.background_error_work, None);
    assert_eq!(actor.snapshot.last_error(), None);
}

async fn wait_for(handle: &ApplianceHandle, predicate: impl Fn(&ApplianceSnapshot) -> bool) {
    for _ in 0..200 {
        let snapshot = handle.snapshot();
        if predicate(&snapshot) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "timed out waiting for appliance state: {:?}",
        handle.snapshot()
    );
}

#[test]
fn foreground_turn_drains_commands_that_are_already_queued() {
    let database = TestDatabase::new("foreground-command-burst");
    let store = SqliteChatStore::open(&database.0).unwrap();
    let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let initial = Arc::new(ApplianceSnapshot::starting());
    let published = Arc::new(RwLock::new(initial.clone()));
    let (revisions, _revision_rx) = watch::channel(initial.revision());
    let mut actor = Actor::new(
        config(&database),
        ChatEngine::new(store),
        UnavailableConnector {
            attempts: Arc::new(AtomicUsize::new(0)),
        },
        receiver,
        published,
        revisions,
    );
    let (contacts_reply, contacts) = oneshot::channel::<Result<Vec<ContactView>, String>>();
    let (peers_reply, peers) = oneshot::channel::<Result<Vec<ConversationPeerView>, String>>();
    let (timeline_reply, timeline) = oneshot::channel::<Result<Vec<TimelineView>, String>>();
    commands
        .send(Command::ConversationPeers(peers_reply))
        .unwrap();
    commands
        .send(Command::Timeline {
            peer: destination(0x44),
            reply: timeline_reply,
        })
        .unwrap();

    assert!(actor.foreground_turn(Command::Contacts(contacts_reply)));
    assert!(contacts.blocking_recv().unwrap().unwrap().is_empty());
    assert!(peers.blocking_recv().unwrap().unwrap().is_empty());
    assert!(timeline.blocking_recv().unwrap().unwrap().is_empty());
    assert!(matches!(
        actor.commands.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn foreground_turn_is_bounded_by_command_channel_capacity() {
    let database = TestDatabase::new("foreground-command-bound");
    let store = SqliteChatStore::open(&database.0).unwrap();
    let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let initial = Arc::new(ApplianceSnapshot::starting());
    let published = Arc::new(RwLock::new(initial.clone()));
    let (revisions, _revision_rx) = watch::channel(initial.revision());
    let mut actor = Actor::new(
        config(&database),
        ChatEngine::new(store),
        UnavailableConnector {
            attempts: Arc::new(AtomicUsize::new(0)),
        },
        receiver,
        published,
        revisions,
    );
    let (first_reply, _first) = oneshot::channel();
    for _ in 0..COMMAND_CAPACITY {
        let (reply, _receive) = oneshot::channel();
        commands.send(Command::SyncNow(reply)).unwrap();
    }

    assert!(actor.foreground_turn(Command::SyncNow(first_reply)));
    assert!(matches!(actor.commands.try_recv(), Ok(Command::SyncNow(_))));
}

#[test]
fn foreground_local_burst_defers_device_io_until_after_background_work() {
    let database = TestDatabase::new("foreground-device-boundary");
    let store = SqliteChatStore::open(&database.0).unwrap();
    let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let initial = Arc::new(ApplianceSnapshot::starting());
    let published = Arc::new(RwLock::new(initial.clone()));
    let (revisions, _revision_rx) = watch::channel(initial.revision());
    let mut actor = Actor::new(
        config(&database),
        ChatEngine::new(store),
        UnavailableConnector {
            attempts: Arc::new(AtomicUsize::new(0)),
        },
        receiver,
        published,
        revisions,
    );
    let (device_reply, _device_result) = oneshot::channel();
    let (trailing_reply, _trailing_result) = oneshot::channel();
    commands.send(Command::NetworkStatus(device_reply)).unwrap();
    commands.send(Command::Contacts(trailing_reply)).unwrap();
    let (first_reply, first_result) = oneshot::channel();

    assert!(actor.foreground_turn(Command::Contacts(first_reply)));
    assert!(first_result.blocking_recv().unwrap().unwrap().is_empty());
    assert!(matches!(
        actor.deferred_foreground.as_ref(),
        Some(Command::NetworkStatus(_))
    ));
    assert!(matches!(
        actor.commands.try_recv(),
        Ok(Command::Contacts(_))
    ));
}

#[test]
fn offline_enqueue_does_not_reserve_or_block_the_device_lane() {
    let database = TestDatabase::new("offline-enqueue-lane");
    let store = SqliteChatStore::open(&database.0).unwrap();
    let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let initial = Arc::new(ApplianceSnapshot::starting());
    let published = Arc::new(RwLock::new(initial.clone()));
    let (revisions, _revision_rx) = watch::channel(initial.revision());
    let mut actor = Actor::new(
        config(&database),
        ChatEngine::new(store),
        UnavailableConnector {
            attempts: Arc::new(AtomicUsize::new(0)),
        },
        receiver,
        published,
        revisions,
    );
    let (device_reply, _device_result) = oneshot::channel();
    commands.send(Command::NetworkStatus(device_reply)).unwrap();
    let (send_reply, send_result) = oneshot::channel();

    assert!(actor.foreground_turn(Command::EnqueueSend {
        material: outbound(0x45),
        reply: send_reply,
    }));
    assert!(send_result.blocking_recv().unwrap().is_ok());
    assert!(actor.session.is_none());
    assert!(actor.urgent_reconcile_outbox.is_some());
    assert!(actor.urgent_reconcile_due);
    assert!(!actor.urgent_reconcile_reserves_lane);
    assert_eq!(actor.urgent_status_wait(Instant::now()), None);
    assert!(matches!(
        actor.deferred_foreground.as_ref(),
        Some(Command::NetworkStatus(_))
    ));
    assert!(!actor.command_blocked_by_urgent_status(actor.deferred_foreground.as_ref().unwrap()));
}

#[test]
fn deferred_device_io_waits_through_the_hot_rows_first_status() {
    let database = TestDatabase::new("deferred-device-after-hot-status");
    let mut store = SqliteChatStore::open(&database.0).unwrap();
    let hot = store.commit_outbound(outbound(0x43)).unwrap().outbox_id();
    let (mut actor, submitted) = connected_fake_actor(config(&database), store, Vec::new());
    actor.urgent_reconcile_outbox = Some(hot);
    actor.urgent_reconcile_due = true;
    actor.urgent_reconcile_reserves_lane = false;
    actor.next_reconcile = Instant::now();

    // The first background turn submits the exact UI-hot row.
    actor.background_turn();
    assert_eq!(submitted.lock().unwrap().as_slice(), &[outbound(0x43)]);
    assert!(actor.urgent_reconcile_due);
    assert!(actor.urgent_reconcile_reserves_lane);

    let (device_reply, _device_result) = oneshot::channel();
    actor.deferred_foreground = Some(Command::NetworkStatus(device_reply));
    assert!(actor.command_blocked_by_urgent_status(actor.deferred_foreground.as_ref().unwrap()));
    assert!(actor.urgent_status_wait(Instant::now()).is_some());

    // Once the exact first status is projected, the deferred device RPC
    // may run on the following loop iteration.
    actor.next_reconcile = Instant::now();
    actor.background_turn();
    assert!(!actor.urgent_reconcile_reserves_lane);
    assert!(!actor.command_blocked_by_urgent_status(actor.deferred_foreground.as_ref().unwrap()));
}

#[test]
fn urgent_send_reconciliation_precedes_due_inbox_and_radio_trace_work() {
    let database = TestDatabase::new("urgent-reconcile-order");
    let store = SqliteChatStore::open(&database.0).unwrap();
    let (_commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let initial = Arc::new(ApplianceSnapshot::starting());
    let published = Arc::new(RwLock::new(initial.clone()));
    let (revisions, _revision_rx) = watch::channel(initial.revision());
    let mut actor = Actor::new(
        config(&database),
        ChatEngine::new(store),
        UnavailableConnector {
            attempts: Arc::new(AtomicUsize::new(0)),
        },
        receiver,
        published,
        revisions,
    );
    let now = Instant::now();
    actor.next_inbox = now;
    actor.next_reconcile = now;
    actor.next_radio_trace = now;
    actor.urgent_reconcile_outbox = Some(OutboxId::new(1).unwrap());
    actor.urgent_reconcile_due = true;
    actor.radio_trace_chat_yields_remaining = 0;
    actor.prefer_inbox = true;

    assert_eq!(
        actor.select_background_work(now),
        Some(BackgroundWork::Reconcile)
    );

    actor.next_reconcile = now + Duration::from_millis(10);
    actor.urgent_reconcile_reserves_lane = true;
    assert_eq!(actor.select_background_work(now), None);

    actor.urgent_reconcile_reserves_lane = false;
    assert_eq!(
        actor.select_background_work(now),
        Some(BackgroundWork::RadioTrace)
    );
}

#[test]
fn hot_outbox_alternates_with_ordinary_fairness_without_starving_older_work() {
    let database = TestDatabase::new("hot-outbox-fairness");
    let mut store = SqliteChatStore::open(&database.0).unwrap();
    let older = store.commit_outbound(outbound(0x41)).unwrap().outbox_id();
    let hot = store.commit_outbound(outbound(0x42)).unwrap().outbox_id();
    assert_ne!(older, hot);
    let (_commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let initial = Arc::new(ApplianceSnapshot::starting());
    let published = Arc::new(RwLock::new(initial.clone()));
    let (revisions, _revision_rx) = watch::channel(initial.revision());
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let mut actor = Actor::new(
        config(&database),
        ChatEngine::new(store),
        UnavailableConnector {
            attempts: Arc::new(AtomicUsize::new(0)),
        },
        receiver,
        published,
        revisions,
    );
    actor.session = Some(Box::new(FakeSession {
        binding: binding(0x71),
        inbox: Vec::new(),
        submitted: submitted.clone(),
        status: SubmissionState::Preparing,
        nearby_generation: None,
    }));
    actor.urgent_reconcile_outbox = Some(hot);
    actor.urgent_reconcile_due = true;
    actor.urgent_reconcile_reserves_lane = true;

    actor.reconcile_turn();
    assert_eq!(submitted.lock().unwrap().as_slice(), &[outbound(0x42)]);
    assert_eq!(actor.urgent_reconcile_outbox, Some(hot));
    assert!(actor.urgent_reconcile_due);
    assert!(actor.urgent_reconcile_reserves_lane);

    actor.reconcile_turn();
    assert_eq!(actor.urgent_reconcile_outbox, Some(hot));
    assert!(!actor.urgent_reconcile_due);
    assert!(!actor.urgent_reconcile_reserves_lane);

    actor.reconcile_turn();
    assert_eq!(
        submitted.lock().unwrap().as_slice(),
        &[outbound(0x42), outbound(0x41)]
    );
    assert_eq!(actor.urgent_reconcile_outbox, Some(hot));
    assert!(actor.urgent_reconcile_due);
    assert!(!actor.urgent_reconcile_reserves_lane);

    actor.reconcile_turn();
    assert_eq!(actor.urgent_reconcile_outbox, Some(hot));
    assert!(!actor.urgent_reconcile_due);
    assert!(!actor.urgent_reconcile_reserves_lane);
}

#[test]
fn idle_radio_trace_poll_is_slower_than_chat_status_polling() {
    let database = TestDatabase::new("radio-trace-idle-cadence");
    let config = ApplianceConfig::new(database.0.clone());
    assert_eq!(config.radio_trace_idle_interval, Duration::from_secs(5));
    assert!(config.radio_trace_idle_interval > config.status_poll_interval);
    assert!(config.radio_trace_idle_interval > config.inbox_poll_interval);
}

#[test]
fn nonterminal_status_refresh_uses_status_cadence_not_operation_gap() {
    let database = TestDatabase::new("status-refresh-cadence");
    let config = ApplianceConfig::new(database.0.clone());
    assert_eq!(
        reconcile_refresh_delay(&config, SubmissionState::Preparing),
        config.status_poll_interval
    );
    assert_ne!(
        reconcile_refresh_delay(&config, SubmissionState::Preparing),
        config.operation_gap
    );
    assert_eq!(
        reconcile_refresh_delay(&config, SubmissionState::Cancelled),
        config.operation_gap
    );
}

#[test]
fn newly_connected_chat_work_precedes_due_trace_catch_up() {
    let database = TestDatabase::new("connected-chat-before-trace");
    let store = SqliteChatStore::open(&database.0).unwrap();
    let (_commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let initial = Arc::new(ApplianceSnapshot::starting());
    let published = Arc::new(RwLock::new(initial.clone()));
    let (revisions, _revision_rx) = watch::channel(initial.revision());
    let mut actor = Actor::new(
        config(&database),
        ChatEngine::new(store),
        UnavailableConnector {
            attempts: Arc::new(AtomicUsize::new(0)),
        },
        receiver,
        published,
        revisions,
    );
    let now = Instant::now();
    actor.next_inbox = now;
    actor.next_reconcile = now;
    actor.next_radio_trace = now;
    actor.radio_trace_chat_yields_remaining = RADIO_TRACE_CHAT_YIELD_TURNS;

    assert_eq!(
        actor.select_background_work(now),
        Some(BackgroundWork::Reconcile)
    );
    assert_eq!(
        actor.select_background_work(now),
        Some(BackgroundWork::Inbox)
    );
}

#[tokio::test]
async fn actor_reconnects_binds_imports_and_processes_offline_outbox() {
    let database = TestDatabase::new("actor-flow");
    let expected_binding = binding(0x21);
    let inbound = inbox(1, 0x31);
    let attempts = Arc::new(AtomicUsize::new(0));
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let connector = FakeConnector {
        outcomes: VecDeque::from([
            Err("board absent".to_owned()),
            Ok((expected_binding, vec![inbound.clone()])),
        ]),
        attempts: attempts.clone(),
        submitted: submitted.clone(),
        status: SubmissionState::Queued,
        nearby_generation: None,
    };
    let store = SqliteChatStore::open(&database.0).unwrap();
    let handle = start_with_connector(config(&database), store, connector).unwrap();

    let committed = handle.enqueue_send(outbound(0x41)).await.unwrap();
    assert!(matches!(committed, OutboxCommitOutcome::Inserted(_)));
    wait_for(&handle, |snapshot| {
        matches!(snapshot.connection(), ConnectionState::Ready { .. })
            && snapshot.imported_this_run == 1
    })
    .await;
    wait_for(&handle, |_| !submitted.lock().unwrap().is_empty()).await;
    assert!(attempts.load(Ordering::Relaxed) >= 2);
    assert_eq!(submitted.lock().unwrap().as_slice(), &[outbound(0x41)]);
    let timeline = handle.timeline(destination(0x31)).await.unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].content.encoding, BytesEncoding::Utf8);
    assert_eq!(timeline[0].content.value, "1");
    assert!(handle.contacts().await.unwrap().is_empty());
    let unknown = handle
        .conversation_peers()
        .await
        .unwrap()
        .into_iter()
        .find(|peer| peer.destination() == hex::encode([0x31; 16]))
        .expect("imported unknown sender must be queryable");
    assert_eq!(unknown.name(), None);
    assert_eq!(unknown.message_count(), 1);
    assert_eq!(unknown.inbound_message_count(), 1);
    assert_eq!(
        unknown.last_message().unwrap().direction(),
        TimelineDirection::Inbound
    );

    handle
        .upsert_contact(Contact::new(destination(0x31), "Field sender"))
        .await
        .unwrap();
    handle
        .upsert_contact(Contact::new(destination(0x31), "Renamed sender"))
        .await
        .unwrap();
    let renamed = handle
        .conversation_peers()
        .await
        .unwrap()
        .into_iter()
        .find(|peer| peer.destination() == hex::encode([0x31; 16]))
        .expect("renamed sender must remain queryable");
    assert_eq!(renamed.name(), Some("Renamed sender"));
    assert_eq!(renamed.message_count(), 1);
    assert_eq!(renamed.inbound_message_count(), 1);
    assert_eq!(handle.timeline(destination(0x31)).await.unwrap().len(), 1);

    handle.shutdown_and_wait().await.unwrap();
    let reopened = SqliteChatStore::open(&database.0).unwrap();
    assert_eq!(reopened.device_binding().unwrap(), Some(expected_binding));
}

fn seed_terminal_outbox(database: &TestDatabase, marker: u8) -> OutboxId {
    let mut store = SqliteChatStore::open(&database.0).unwrap();
    let outbox_id = store.commit_outbound(outbound(marker)).unwrap().outbox_id();
    let acceptance = AcceptanceIds::new(
        SubmissionId::new(700 + u64::from(marker)).unwrap(),
        MessageId::new([marker; 32]),
    );
    store.record_acceptance(outbox_id, acceptance).unwrap();
    store
        .project_submission_status(
            acceptance.submission_id(),
            SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
        )
        .unwrap();
    store.close().unwrap();
    outbox_id
}

#[tokio::test]
async fn terminal_outbox_is_not_resubmitted_on_startup_sync_or_reconnect() {
    let database = TestDatabase::new("terminal-outbox-reconnect");
    let outbox_id = seed_terminal_outbox(&database, 0x42);
    let connection_attempts = Arc::new(AtomicUsize::new(0));
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let connector = FakeConnector {
        outcomes: VecDeque::from([
            Ok((binding(0x22), Vec::new())),
            Ok((binding(0x22), Vec::new())),
        ]),
        attempts: connection_attempts.clone(),
        submitted: submitted.clone(),
        status: SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
        nearby_generation: None,
    };
    let store = SqliteChatStore::open(&database.0).unwrap();
    let handle = start_with_connector(config(&database), store, connector).unwrap();

    wait_for(&handle, |snapshot| {
        matches!(snapshot.connection(), ConnectionState::Ready { .. })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(submitted.lock().unwrap().is_empty());
    assert_eq!(handle.snapshot().pending_outbox(), 0);

    handle.sync_now().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        submitted.lock().unwrap().is_empty(),
        "an explicit sync must not rearm a terminal device submission"
    );

    handle.reconnect().await.unwrap();
    wait_for(&handle, |snapshot| {
        connection_attempts.load(Ordering::Relaxed) >= 2
            && matches!(snapshot.connection(), ConnectionState::Ready { .. })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        submitted.lock().unwrap().is_empty(),
        "reconnecting must not create a fresh device submission"
    );

    handle.shutdown_and_wait().await.unwrap();
    let reopened = SqliteChatStore::open(&database.0).unwrap();
    let terminal = reopened.outbox(outbox_id).unwrap().unwrap();
    assert!(matches!(
        terminal.status(),
        OutboxStatus::Device(SubmissionState::Failed(SubmissionFailure::DeliveryTimeout))
    ));
}

#[tokio::test]
async fn nearby_generation_changes_do_not_rearm_a_terminal_outbox() {
    let database = TestDatabase::new("terminal-outbox-nearby");
    seed_terminal_outbox(&database, 0x43);
    let nearby_generation = Arc::new(AtomicU64::new(1));
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let connector = FakeConnector {
        outcomes: VecDeque::from([Ok((binding(0x23), Vec::new()))]),
        attempts: Arc::new(AtomicUsize::new(0)),
        submitted: submitted.clone(),
        status: SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
        nearby_generation: Some(nearby_generation.clone()),
    };
    let store = SqliteChatStore::open(&database.0).unwrap();
    let handle = start_with_connector(config(&database), store, connector).unwrap();

    wait_for(&handle, |snapshot| {
        matches!(snapshot.connection(), ConnectionState::Ready { .. })
    })
    .await;
    assert!(handle.nearby_peers().await.unwrap().is_empty());
    nearby_generation.store(2, Ordering::Relaxed);
    assert!(handle.nearby_peers().await.unwrap().is_empty());
    handle.sync_now().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        submitted.lock().unwrap().is_empty(),
        "nearby observations and sync activity must not create a fresh device submission"
    );
    handle.shutdown_and_wait().await.unwrap();
}

#[tokio::test]
async fn actor_serializes_nomad_fetches_and_probes_through_its_existing_session() {
    let database = TestDatabase::new("nomad-fetch");
    let trace = Arc::new(Mutex::new(NomadTrace::default()));
    let connector = NomadConnector {
        trace: trace.clone(),
        connected: false,
    };
    let store = SqliteChatStore::open(&database.0).unwrap();
    let handle = start_with_connector(config(&database), store, connector).unwrap();
    wait_for(&handle, |snapshot| {
        matches!(snapshot.connection(), ConnectionState::Ready { .. })
    })
    .await;

    let start: NomadFetchStartRequest = serde_json::from_value(serde_json::json!({
        "destination": "83".repeat(16),
        "path": "/page/index.mu",
        "timestamp_unix_ms": 1_784_732_100_001_u64,
        "idempotency_key": "84".repeat(16),
    }))
    .unwrap();
    let accepted = handle.nomad_fetch_start(start).await.unwrap();
    let id = reticulum_device_api::NomadFetchId::new([0x82; 8], 1).unwrap();
    assert_eq!(
        serde_json::to_value(accepted).unwrap(),
        serde_json::json!({
            "id": hex::encode(id.as_bytes()),
            "outcome": "accepted",
        })
    );

    let poll: NomadFetchPollRequest = serde_json::from_value(serde_json::json!({
        "id": hex::encode(id.as_bytes()),
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(handle.nomad_fetch_poll(poll).await.unwrap()).unwrap(),
        serde_json::json!({
            "state": "ready",
            "page": ">Metalbeard",
        })
    );

    let probe_start: ReticulumProbeStartRequest = serde_json::from_value(serde_json::json!({
        "destination": "86".repeat(16),
        "idempotency_key": "87".repeat(16),
    }))
    .unwrap();
    let accepted = handle.reticulum_probe_start(probe_start).await.unwrap();
    let probe_id = reticulum_device_api::ProbeId::new([0x85; 16]).unwrap();
    assert_eq!(
        serde_json::to_value(accepted).unwrap(),
        serde_json::json!({
            "id": hex::encode(probe_id.as_bytes()),
            "outcome": "accepted",
        })
    );
    let probe_poll: ReticulumProbePollRequest = serde_json::from_value(serde_json::json!({
        "id": hex::encode(probe_id.as_bytes()),
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(handle.reticulum_probe_poll(probe_poll).await.unwrap()).unwrap(),
        serde_json::json!({
            "state": "succeeded",
            "result": {
                "round_trip_ms": 1_234,
                "hops": 2,
                "ingress_observation": {
                    "interface_id": 7,
                    "signal": {
                        "rssi_dbm": -91,
                        "snr_db": 7,
                    },
                },
            },
        })
    );
    {
        let trace = trace.lock().unwrap();
        assert_eq!(
            trace.starts,
            [ObservedNomadStart {
                destination: [0x83; 16],
                path: "/page/index.mu".to_owned(),
                timestamp_unix_ms: 1_784_732_100_001,
                idempotency_key: [0x84; 16],
            }]
        );
        assert_eq!(trace.polls, [id]);
        assert_eq!(
            trace.probe_starts,
            [ObservedProbeStart {
                destination: [0x86; 16],
                idempotency_key: [0x87; 16],
            }]
        );
        assert_eq!(trace.probe_polls, [probe_id]);
    }

    handle.shutdown_and_wait().await.unwrap();
}

#[tokio::test]
async fn actor_serializes_network_reads_and_secret_bearing_mutations() {
    let database = TestDatabase::new("network-config");
    let trace = Arc::new(Mutex::new(NetworkTrace::default()));
    let connector = NetworkConnector {
        trace: trace.clone(),
        connected: false,
    };
    let store = SqliteChatStore::open(&database.0).unwrap();
    let handle = start_with_connector(config(&database), store, connector).unwrap();
    wait_for(&handle, |snapshot| {
        matches!(snapshot.connection(), ConnectionState::Ready { .. })
    })
    .await;

    assert_eq!(
        serde_json::to_value(handle.network_config().await.unwrap()).unwrap(),
        serde_json::json!({
            "revision": 5,
            "wifi_profiles": [{
                "profile_id": "92".repeat(16),
                "enabled": true,
                "priority": 220,
                "ssid": {"encoding": "hex", "value": "6d657368ff"},
                "credential_configured": true
            }],
            "tcp_peer": null,
            "wifi_transport_enabled": true,
            "automatic_announces_enabled": true,
            "rmap_discovery_enabled": false,
            "rmap_share_location": false,
            "rmap_phone_location": null,
            "lora_tx_power_dbm": 14,
            "lora_profile": {
                "frequency_hz": 915_000_000,
                "bandwidth_hz": 125_000,
                "spreading_factor": 7,
                "coding_rate_denominator": 5,
                "tx_power_dbm": 14
            },
            "device_name": null
        })
    );
    assert_eq!(
        serde_json::to_value(handle.network_status().await.unwrap()).unwrap(),
        serde_json::json!({
            "configured_revision": 6,
            "applied_revision": 5,
            "wifi_state": "connected",
            "active_wifi_profile": "92".repeat(16),
            "connected_ssid": {"encoding": "hex", "value": "6d657368ff"},
            "ipv4_address": "192.0.2.33",
            "rssi_dbm": -68,
            "tcp_peer_state": "waiting_for_network",
            "last_tcp_failure": null,
            "dns_diagnostics": null,
            "rmap_status": null
        })
    );
    assert_eq!(
        handle.manual_service_announce().await.unwrap(),
        ManualServiceAnnounceDisposition::Queued
    );
    assert_eq!(
        serde_json::to_value(handle.radio_routes_status().await.unwrap()).unwrap(),
        serde_json::json!({
            "uptime_ms": 4_500,
            "interfaces": [{
                "id": 1,
                "kind": "lora",
                "state": "online",
                "generation": 2,
                "logical_mtu": 500,
                "bitrate": 5_470
            }],
            "lora": {
                "applied_tx_power_dbm": 22,
                "frequency_hz": 915_000_000,
                "bandwidth_hz": 125_000,
                "spreading_factor": 7,
                "coding_rate_denominator": 5,
                "rx_physical_frames": 8,
                "rx_packets": 7,
                "rx_errors": 1,
                "rx_drops": 0,
                "tx_terminal_jobs": 3,
                "tx_successes": 2,
                "tx_completed_frames": 5,
                "tx_access_rejects": 1,
                "tx_failures": 0,
                "cad_busy": 4,
                "cad_clear": 9,
                "last_rx": {"age_ms": 500, "rssi_dbm": -91, "snr_db": 7},
                "last_tx": {
                    "age_ms": 700,
                    "outcome": "completed",
                    "family": "data",
                    "data_evidence": {
                        "interface_id": 1,
                        "encoded_packet_len": 183,
                        "encoded_packet_sha256": "ab".repeat(32)
                    }
                },
                "last_data_tx": {
                    "age_ms": 700,
                    "outcome": "completed",
                    "family": "data",
                    "data_evidence": {
                        "interface_id": 1,
                        "encoded_packet_len": 183,
                        "encoded_packet_sha256": "ab".repeat(32)
                    }
                }
            },
            "rns": {
                "received": 9,
                "forwarded": 2,
                "dedup_drops": 1,
                "invalid_drops": 0,
                "announces_received": 4,
                "paths_learned": 1,
                "paths_expired": 0,
                "links_established": 0,
                "links_closed": 0,
                "links_failed": 0
            },
            "observed_peer_count": 3,
            "retained_route_count": 1,
            "usable_route_count": 1,
            "route_table_revision": 1,
            "routes": [{
                "destination": "95".repeat(16),
                "next_hop_identity": "96".repeat(16),
                "hops": 2,
                "retained_interface_id": 1,
                "resolution": "exact_ready",
                "learned_age_ms": 1_200,
                "last_local_use_age_ms": 400,
                "expires_in_ms": 28_800
            }]
        })
    );

    let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "upsert_wifi",
            "profile_id": "93".repeat(16),
            "enabled": true,
            "priority": 230,
            "ssid": {"encoding": "utf8", "value": "field-node"},
            "credential": {
                "kind": "replace",
                "passphrase": "correct horse battery staple"
            }
        },
        "expected_revision": 5,
        "idempotency_key": "94".repeat(16)
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(handle.mutate_network_config(request).await.unwrap()).unwrap(),
        serde_json::json!({
            "outcome": "applied",
            "revision": 6,
            "reboot_required": true
        })
    );

    {
        let trace = trace.lock().unwrap();
        assert_eq!(trace.announces, 1);
        assert_eq!(trace.config_reads, 1);
        assert_eq!(trace.status_reads, 1);
        assert_eq!(trace.diagnostics_reads, 1);
        assert_eq!(trace.route_reads, 1);
        assert_eq!(
            trace.mutations,
            [ObservedNetworkMutation {
                profile_id: [0x93; 16],
                enabled: true,
                priority: 230,
                ssid: b"field-node".to_vec(),
                replacement_passphrase_len: Some(28),
                expected_revision: 5,
                idempotency_key: [0x94; 16],
            }]
        );
    }

    handle.shutdown_and_wait().await.unwrap();
}

#[tokio::test]
async fn database_binding_mismatch_faults_without_retrying_another_board() {
    let database = TestDatabase::new("binding-mismatch");
    let mut store = SqliteChatStore::open(&database.0).unwrap();
    store.bind_device(binding(0x51)).unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let connector = FakeConnector {
        outcomes: VecDeque::from([Ok((binding(0x61), Vec::new()))]),
        attempts: attempts.clone(),
        submitted: Arc::new(Mutex::new(Vec::new())),
        status: SubmissionState::Queued,
        nearby_generation: None,
    };
    let handle = start_with_connector(config(&database), store, connector).unwrap();
    wait_for(&handle, |snapshot| {
        snapshot.connection() == &ConnectionState::Faulted
    })
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(attempts.load(Ordering::Relaxed), 1);
    assert!(handle.snapshot().last_error().unwrap().contains("mismatch"));
    handle.shutdown_and_wait().await.unwrap();
}

#[tokio::test]
async fn unavailable_connector_settles_without_background_retry() {
    let database = TestDatabase::new("unavailable");
    let attempts = Arc::new(AtomicUsize::new(0));
    let connector = UnavailableConnector {
        attempts: attempts.clone(),
    };
    let store = SqliteChatStore::open(&database.0).unwrap();
    let handle = start_with_connector(config(&database), store, connector).unwrap();
    wait_for(&handle, |snapshot| {
        snapshot.connection()
            == &ConnectionState::Unavailable {
                transport: ConnectionTransport::BluetoothLowEnergy,
            }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(attempts.load(Ordering::Relaxed), 1);
    assert_eq!(
        handle.snapshot().last_error(),
        Some("BLE adapter is not implemented")
    );
    handle.shutdown_and_wait().await.unwrap();
}

#[tokio::test]
async fn manual_retry_stamps_the_latest_phone_observation_without_rewriting_the_first_attempt() {
    let database = TestDatabase::new("manual-retry-location");
    let material = outbound(0xa2);
    let mut store = SqliteChatStore::open(&database.0).unwrap();
    let outbox_id = store.commit_outbound(material).unwrap().outbox_id();
    let accepted = AcceptanceIds::new(SubmissionId::new(0xa2).unwrap(), MessageId::new([0xa2; 32]));
    store.record_acceptance(outbox_id, accepted).unwrap();
    store
        .project_submission_status(
            accepted.submission_id(),
            SubmissionState::Failed(SubmissionFailure::NoPath),
        )
        .unwrap();
    let connector = UnavailableConnector {
        attempts: Arc::new(AtomicUsize::new(0)),
    };
    let handle = start_with_connector(config(&database), store, connector).unwrap();
    wait_for(&handle, |snapshot| {
        matches!(snapshot.connection(), ConnectionState::Unavailable { .. })
    })
    .await;

    let retry_location = phone_location(42_234_567, 1_784_732_300_003);
    handle.update_phone_location(retry_location).await.unwrap();
    assert_eq!(
        handle
            .retry_send(outbox_id, IdempotencyKey::new([0xa3; 16]))
            .await
            .unwrap(),
        OutboxRetryOutcome::Requeued(outbox_id)
    );
    let attempt_locations = handle
        .message_activity(MessageActivityPageRequest::new(None, 100, None))
        .await
        .unwrap()
        .events()
        .iter()
        .filter_map(MessageActivityEventView::attempt_location)
        .collect::<Vec<_>>();
    assert_eq!(
        attempt_locations,
        vec![
            retry_location,
            PhoneLocationObservationView::Unavailable {
                reason: PhoneLocationUnavailableReasonView::NotObserved,
            },
        ]
    );
    handle.shutdown_and_wait().await.unwrap();
}

#[tokio::test]
async fn durable_activity_query_remains_available_without_a_device_session() {
    let database = TestDatabase::new("offline-message-activity");
    let attempts = Arc::new(AtomicUsize::new(0));
    let connector = UnavailableConnector {
        attempts: attempts.clone(),
    };
    let store = SqliteChatStore::open(&database.0).unwrap();
    let handle = start_with_connector(config(&database), store, connector).unwrap();
    wait_for(&handle, |snapshot| {
        matches!(snapshot.connection(), ConnectionState::Unavailable { .. })
    })
    .await;

    assert_eq!(
        handle.phone_location_observation().await.unwrap(),
        PhoneLocationObservationView::Unavailable {
            reason: PhoneLocationUnavailableReasonView::NotObserved,
        }
    );
    let attempt_location = phone_location(43_765_432, 1_784_732_200_002);
    assert_eq!(
        handle
            .update_phone_location(attempt_location)
            .await
            .unwrap(),
        attempt_location
    );
    let committed = handle.enqueue_send(outbound(0xa1)).await.unwrap();
    let page = handle
        .message_activity(MessageActivityPageRequest::new(None, 100, None))
        .await
        .unwrap();
    assert_eq!(page.events().len(), 1);
    assert!(!page.history_incomplete());
    assert_eq!(page.events()[0].attempt_number(), Some(1));
    assert!(matches!(
        page.events()[0].activity(),
        MessageActivityKindView::OutboundQueued
    ));
    assert_eq!(page.events()[0].attempt_location(), Some(attempt_location));
    handle
        .update_phone_location(PhoneLocationObservationView::Unavailable {
            reason: PhoneLocationUnavailableReasonView::TelemetryDisabled,
        })
        .await
        .unwrap();
    assert_eq!(
        handle
            .message_activity(MessageActivityPageRequest::new(None, 100, None))
            .await
            .unwrap()
            .events()[0]
            .attempt_location(),
        Some(attempt_location),
        "later cache updates must not rewrite an existing attempt"
    );
    assert_eq!(
        handle
            .message_activity(MessageActivityPageRequest::new(
                None,
                100,
                Some(page.events()[0].timeline_sequence()),
            ))
            .await
            .unwrap()
            .events()
            .len(),
        1
    );
    assert!(matches!(committed, OutboxCommitOutcome::Inserted(_)));

    assert!(matches!(
        handle
            .message_activity(MessageActivityPageRequest::new(None, 0, None))
            .await,
        Err(ServiceError::Operation(error)) if error.contains("page limit")
    ));
    handle.shutdown_and_wait().await.unwrap();
}

#[tokio::test]
async fn reconnect_drops_the_transport_lease_with_the_session() {
    let database = TestDatabase::new("connection-lease");
    let lease_drops = Arc::new(AtomicUsize::new(0));
    let connector = LeaseConnector {
        connected: false,
        lease_drops: lease_drops.clone(),
    };
    let store = SqliteChatStore::open(&database.0).unwrap();
    let handle = start_with_connector(config(&database), store, connector).unwrap();
    wait_for(&handle, |snapshot| {
        matches!(snapshot.connection(), ConnectionState::Ready { .. })
    })
    .await;
    assert_eq!(lease_drops.load(Ordering::Relaxed), 0);

    handle.reconnect().await.unwrap();
    assert_eq!(lease_drops.load(Ordering::Relaxed), 1);
    handle.shutdown_and_wait().await.unwrap();
    assert_eq!(lease_drops.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn ensure_connected_preserves_an_already_ready_transport_lease() {
    let database = TestDatabase::new("ensure-ready-lease");
    let lease_drops = Arc::new(AtomicUsize::new(0));
    let connector = LeaseConnector {
        connected: false,
        lease_drops: lease_drops.clone(),
    };
    let store = SqliteChatStore::open(&database.0).unwrap();
    let handle = start_with_connector(config(&database), store, connector).unwrap();
    wait_for(&handle, |snapshot| {
        matches!(snapshot.connection(), ConnectionState::Ready { .. })
    })
    .await;

    handle.ensure_connected().await.unwrap();
    assert!(
        matches!(
            handle.snapshot().connection(),
            ConnectionState::Ready { .. }
        ),
        "non-destructive wake preserves the ready session"
    );
    assert_eq!(lease_drops.load(Ordering::Relaxed), 0);

    handle.shutdown_and_wait().await.unwrap();
    assert_eq!(lease_drops.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn ensure_connected_interrupts_retry_backoff_without_clearing_state() {
    let database = TestDatabase::new("ensure-backoff");
    let expected_binding = binding(0x72);
    let attempts = Arc::new(AtomicUsize::new(0));
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let connector = FakeConnector {
        outcomes: VecDeque::from([
            Err("physical link is not registered".to_owned()),
            Ok((expected_binding, Vec::new())),
        ]),
        attempts: attempts.clone(),
        submitted,
        status: SubmissionState::Queued,
        nearby_generation: None,
    };
    let mut delayed = config(&database);
    delayed.reconnect_initial = Duration::from_secs(30);
    delayed.reconnect_maximum = Duration::from_secs(30);
    let store = SqliteChatStore::open(&database.0).unwrap();
    let handle = start_with_connector(delayed, store, connector).unwrap();
    wait_for(&handle, |snapshot| {
        snapshot.connection() == &ConnectionState::Backoff
    })
    .await;
    assert_eq!(attempts.load(Ordering::Relaxed), 1);

    handle.ensure_connected().await.unwrap();
    wait_for(&handle, |snapshot| {
        matches!(snapshot.connection(), ConnectionState::Ready { .. })
    })
    .await;
    assert_eq!(attempts.load(Ordering::Relaxed), 2);
    let expected_device = DeviceView::from(expected_binding);
    assert_eq!(handle.snapshot().device(), Some(&expected_device));

    handle.shutdown_and_wait().await.unwrap();
}

#[tokio::test]
async fn binding_failure_drops_session_before_transport_lease() {
    let database = TestDatabase::new("binding-drop-order");
    let order = Arc::new(Mutex::new(Vec::new()));
    let connector = BindingFailureConnector {
        returned_session: false,
        order: order.clone(),
    };
    let store = SqliteChatStore::open(&database.0).unwrap();
    let mut failure_config = config(&database);
    failure_config.reconnect_initial = Duration::from_secs(1);
    failure_config.reconnect_maximum = Duration::from_secs(1);
    let handle = start_with_connector(failure_config, store, connector).unwrap();
    wait_for(&handle, |_| order.lock().unwrap().len() == 2).await;
    assert_eq!(
        order.lock().unwrap().as_slice(),
        &["session", "lease"],
        "the transport gate must remain leased until the old session/FD closes"
    );
    handle.shutdown_and_wait().await.unwrap();
}
