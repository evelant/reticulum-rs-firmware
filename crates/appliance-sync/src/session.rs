use core::{fmt, num::NonZeroU64};

use reticulum_appliance_store::{
    AcceptanceIds, DestinationHash, DeviceBinding, EncodedPacketSha256, InboundMessage, MessageId,
    MessageIngressObservation, MessageInterfaceId, MessageLocation, MessageSignalObservation,
    OutboxMaterial, PacketEvidence, SubmissionFailure, SubmissionId, SubmissionState,
    UnixTimestampMillis,
};
use reticulum_device_api as device_api;
use reticulum_lxmf_wire::{MessageView, WireLimits, decode_sideband_location_fields};
use sha2::{Digest, Sha256};

const MAX_REQUEST_SESSION_LXMF_WIRE_BYTES: usize = 16 * 1024 * 1024;

/// Stable committed-message handle used only within one live device session.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct InboxCursor(NonZeroU64);

impl InboxCursor {
    /// Construct a cursor from a positive device message handle.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Complete device message handle.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Authenticated metadata sufficient to skip an already-imported message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InboxSummary {
    cursor: InboxCursor,
    message_id: MessageId,
}

impl InboxSummary {
    /// Construct one stable inbox summary.
    pub const fn new(cursor: InboxCursor, message_id: MessageId) -> Self {
        Self { cursor, message_id }
    }

    /// Stable handle for this live device-store generation.
    pub const fn cursor(self) -> InboxCursor {
        self.cursor
    }

    /// Authenticated LXMF message identifier.
    pub const fn message_id(self) -> MessageId {
        self.message_id
    }
}

/// Sequential application operations provided by one authenticated device.
pub trait LxmfSession {
    /// Session-specific failure.
    type Error;

    /// Read the authenticated device and local-destination binding.
    fn binding(&mut self) -> Result<DeviceBinding, Self::Error>;

    /// Read the durable product-owned appliance label.
    fn appliance_label_get(&mut self) -> Result<device_api::ApplianceLabelSnapshot, Self::Error>;

    /// Compare-and-swap the durable product-owned appliance label.
    fn appliance_label_mutate(
        &mut self,
        request: device_api::ApplianceLabelMutationRequest<'_>,
    ) -> Result<device_api::ApplianceLabelMutationOutcome, Self::Error>;

    /// Durably submit exact previously committed outbound material.
    fn submit(&mut self, material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error>;

    /// Read one accepted device submission's durable state.
    fn submission_status(&mut self, id: SubmissionId) -> Result<SubmissionState, Self::Error>;

    /// Return the next committed inbox summary after a session-local cursor.
    fn next_inbox(
        &mut self,
        after: Option<InboxCursor>,
    ) -> Result<Option<InboxSummary>, Self::Error>;

    /// Download and validate the complete message for the retained summary.
    fn read_inbox(&mut self, summary: InboxSummary) -> Result<InboundMessage, Self::Error>;

    /// Read the appliance's durable mailbox collection state.
    fn inbox_status(&mut self) -> Result<device_api::LxmfMailboxStatus, Self::Error>;

    /// Idempotently acknowledge every message through a locally durable cursor.
    fn acknowledge_inbox_through(
        &mut self,
        through: InboxCursor,
    ) -> Result<device_api::LxmfMailboxStatus, Self::Error>;

    /// Read one page from the authenticated device's volatile nearby-peer
    /// projection.
    ///
    /// The cursor is boot-scoped. Callers must inspect the returned incarnation
    /// and history-gap flag rather than treating its generation as durable.
    fn next_nearby_peer(
        &mut self,
        after: Option<device_api::LxmfPeerDiscoveryCursor>,
    ) -> Result<device_api::LxmfPeerDiscoveryPage, Self::Error>;

    /// Begin or idempotently replay one bounded anonymous NomadNet page fetch.
    fn nomad_fetch_start(
        &mut self,
        request: device_api::NomadFetchStartRequest<'_>,
    ) -> Result<device_api::NomadFetchStartAccepted, Self::Error>;

    /// Poll one principal-owned bounded NomadNet page fetch.
    fn nomad_fetch_poll(
        &mut self,
        id: device_api::NomadFetchId,
    ) -> Result<device_api::NomadFetchPollResponse, Self::Error>;

    /// Begin or idempotently replay one bounded Reticulum path-and-proof probe.
    fn reticulum_probe_start(
        &mut self,
        request: device_api::ProbeStartRequest,
    ) -> Result<device_api::ProbeStartAccepted, Self::Error>;

    /// Poll one principal-owned boot-scoped Reticulum path-and-proof probe.
    fn reticulum_probe_poll(
        &mut self,
        id: device_api::ProbeId,
    ) -> Result<device_api::ProbePollResponse, Self::Error>;

    /// Read the complete redacted desired network configuration.
    fn network_config_get(&mut self) -> Result<device_api::NetworkConfigSnapshot, Self::Error>;

    /// Apply one compare-and-swap desired-network mutation.
    fn network_config_mutate(
        &mut self,
        request: device_api::NetworkConfigMutationRequest<'_>,
    ) -> Result<device_api::NetworkConfigMutationOutcome, Self::Error>;

    /// Read current secret-free Wi-Fi station and Reticulum TCP state.
    fn network_status(&mut self) -> Result<device_api::NetworkRuntimeStatus, Self::Error>;

    /// Queue the node's ordinary primary, LXMF, and NomadNet service announces.
    fn manual_service_announce(
        &mut self,
    ) -> Result<device_api::ManualServiceAnnounceDisposition, Self::Error>;

    /// Read one bounded cross-interface node diagnostics snapshot.
    fn node_diagnostics(&mut self) -> Result<device_api::NodeDiagnosticsSnapshot, Self::Error>;

    /// Read one lexicographically ordered retained-route diagnostics page.
    fn route_diagnostics_page(
        &mut self,
        request: device_api::RouteDiagnosticsRequest,
    ) -> Result<device_api::RouteDiagnosticsPage, Self::Error>;

    /// Read one ascending page from the boot-scoped packet-correlated RF trace.
    fn radio_trace_page(
        &mut self,
        request: device_api::RadioTracePageRequest,
    ) -> Result<device_api::RadioTracePage, Self::Error>;

    /// Whether the underlying authenticated session can attempt another call.
    fn is_usable(&self) -> bool;
}

/// Typed application adapter failure around an identified PRNS request.
#[derive(Debug)]
pub enum DeviceSessionError {
    /// An identified Reticulum request could not complete.
    Request(String),
    /// The appliance returned a typed application-level rejection.
    Api(device_api::ApiErrorResponse),
    /// The appliance returned a response for a different operation.
    UnexpectedResponse {
        /// Operation response expected by the caller.
        expected: u16,
        /// Response kind returned by the appliance.
        actual: u16,
    },
    /// The node did not publish the required local `lxmf.delivery` destination.
    MissingLxmfDeliveryDestination,
    /// The device returned a zero submission identifier.
    InvalidSubmissionId,
    /// The device returned packet evidence for an empty encoded packet.
    InvalidPacketEvidence,
    /// A decoded LXMF timestamp was outside the client's exact millisecond range.
    InvalidTimestamp {
        /// Original binary64 seconds represented as raw bits.
        seconds_bits: u64,
    },
    /// The requested inbox summary was not the one retained by the adapter.
    InboxSummaryNotRetained,
    /// Complete authenticated LXMF wire failed semantic decoding.
    InvalidLxmf(String),
}

impl fmt::Display for DeviceSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(reason) => {
                write!(formatter, "Reticulum application request failed: {reason}")
            }
            Self::Api(error) => write!(
                formatter,
                "device API operation {:?} failed with {:?}",
                error.operation, error.code
            ),
            Self::UnexpectedResponse { expected, actual } => write!(
                formatter,
                "device API response kind {actual} did not match operation {expected}"
            ),
            Self::MissingLxmfDeliveryDestination => {
                formatter.write_str("device has no local LXMF delivery destination")
            }
            Self::InvalidSubmissionId => {
                formatter.write_str("device returned a zero submission identifier")
            }
            Self::InvalidPacketEvidence => {
                formatter.write_str("device returned empty encoded-packet evidence")
            }
            Self::InvalidTimestamp { seconds_bits } => write!(
                formatter,
                "LXMF timestamp bits {seconds_bits:016x} are outside the client range"
            ),
            Self::InboxSummaryNotRetained => {
                formatter.write_str("inbox summary is not retained by this session")
            }
            Self::InvalidLxmf(error) => write!(formatter, "invalid LXMF message: {error}"),
        }
    }
}

impl std::error::Error for DeviceSessionError {}

/// One identified Reticulum application requester used by the durable client
/// runtime.
///
/// Implementations own network lifecycle and authorization. The adapter below
/// owns only typed Device API operations and LXMF validation; it does not
/// introduce another transport protocol or Reticulum state machine.
pub trait DeviceApiRequester: Send {
    /// Stable appliance identity used to bind one clean-reset SQLite profile.
    fn appliance_id(&self) -> [u8; 16];

    /// Issue one typed request over the implementation's identified session.
    fn request(
        &mut self,
        request: device_api::DeviceRequest<'_>,
    ) -> Result<device_api::DeviceResponse, String>;

    /// Whether another request can be attempted without replacing the owner.
    fn is_usable(&self) -> bool;
}

/// [`LxmfSession`] over identified Reticulum application requests.
///
/// This is the post-bearer adapter used by PRNS clients. It consumes ordinary
/// request/response operations and deliberately knows nothing about Bluetooth,
/// framing, pairing credentials, Links, routes, or packet receipts.
pub struct DeviceApiRequestSession<R> {
    requester: R,
    retained_inbox: Option<(InboxSummary, device_api::LxmfMessageSummary)>,
}

impl<R: DeviceApiRequester> DeviceApiRequestSession<R> {
    /// Wrap one usable identified requester.
    pub const fn new(requester: R) -> Self {
        Self {
            requester,
            retained_inbox: None,
        }
    }

    fn exchange(
        &mut self,
        request: device_api::DeviceRequest<'_>,
    ) -> Result<device_api::DeviceResponse, DeviceSessionError> {
        let expected = request.operation();
        match self
            .requester
            .request(request)
            .map_err(DeviceSessionError::Request)?
        {
            device_api::DeviceResponse::Error(error) => Err(DeviceSessionError::Api(error)),
            response if response.kind() == expected => Ok(response),
            response => Err(DeviceSessionError::UnexpectedResponse {
                expected,
                actual: response.kind(),
            }),
        }
    }

    fn read_complete_lxmf(
        &mut self,
        summary: device_api::LxmfMessageSummary,
    ) -> Result<Vec<u8>, DeviceSessionError> {
        let total = usize::try_from(summary.normalized_wire_len()).map_err(|_| {
            DeviceSessionError::InvalidLxmf("wire length does not fit this client".to_owned())
        })?;
        if total > MAX_REQUEST_SESSION_LXMF_WIRE_BYTES {
            return Err(DeviceSessionError::InvalidLxmf(
                "wire exceeds the client collection limit".to_owned(),
            ));
        }
        let mut wire = Vec::new();
        wire.try_reserve_exact(total)
            .map_err(|_| DeviceSessionError::InvalidLxmf("wire allocation failed".to_owned()))?;
        let mut offset = 0_u32;
        let mut hasher = Sha256::new();
        while offset < summary.normalized_wire_len() {
            let remaining = summary.normalized_wire_len() - offset;
            let maximum = remaining.min(device_api::MAX_LXMF_READ_CHUNK_BYTES as u32) as u16;
            let max_bytes = device_api::LxmfReadLength::new(maximum)
                .expect("a non-final LXMF read always requests positive bytes");
            let response = self.exchange(device_api::DeviceRequest::LxmfRead {
                handle: summary.handle(),
                offset,
                max_bytes,
            })?;
            let device_api::DeviceResponse::LxmfRead(chunk) = response else {
                unreachable!("exchange validated the response operation")
            };
            if chunk.handle() != summary.handle()
                || chunk.offset() != offset
                || chunk.total_len() != summary.normalized_wire_len()
                || chunk.bytes().is_empty()
                || chunk.bytes().len() > usize::from(max_bytes.get())
            {
                return Err(DeviceSessionError::InvalidLxmf(
                    "read chunk did not match its authenticated summary".to_owned(),
                ));
            }
            let next = offset
                .checked_add(chunk.bytes().len() as u32)
                .ok_or_else(|| {
                    DeviceSessionError::InvalidLxmf("read offset overflowed".to_owned())
                })?;
            if next > summary.normalized_wire_len()
                || chunk.is_final() != (next == summary.normalized_wire_len())
            {
                return Err(DeviceSessionError::InvalidLxmf(
                    "read chunk had an invalid final boundary".to_owned(),
                ));
            }
            wire.extend_from_slice(chunk.bytes());
            hasher.update(chunk.bytes());
            offset = next;
        }
        let digest: [u8; 32] = hasher.finalize().into();
        if digest != *summary.exact_wire_sha256() {
            return Err(DeviceSessionError::InvalidLxmf(
                "complete wire digest did not match its summary".to_owned(),
            ));
        }
        Ok(wire)
    }
}

impl<R: DeviceApiRequester> LxmfSession for DeviceApiRequestSession<R> {
    type Error = DeviceSessionError;

    fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::IdentitySummary)?;
        let device_api::DeviceResponse::IdentitySummary(identity) = response else {
            unreachable!("exchange validated the response operation")
        };
        let lxmf = identity
            .lxmf_delivery_destination()
            .ok_or(DeviceSessionError::MissingLxmfDeliveryDestination)?;
        Ok(DeviceBinding::new(
            self.requester.appliance_id(),
            DestinationHash::new(identity.primary_destination().0),
            DestinationHash::new(lxmf.0),
        ))
    }

    fn appliance_label_get(&mut self) -> Result<device_api::ApplianceLabelSnapshot, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::ApplianceLabelGet)?;
        let device_api::DeviceResponse::ApplianceLabel(snapshot) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(snapshot)
    }

    fn appliance_label_mutate(
        &mut self,
        request: device_api::ApplianceLabelMutationRequest<'_>,
    ) -> Result<device_api::ApplianceLabelMutationOutcome, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::ApplianceLabelMutate(request))?;
        let device_api::DeviceResponse::ApplianceLabelMutation(outcome) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(outcome)
    }

    fn submit(&mut self, material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
        let location = material.location().map(|location| {
            device_api::LxmfMessageLocation::new(
                location.latitude_e6(),
                location.longitude_e6(),
                location.altitude_cm(),
                location.speed_cm_per_second(),
                location.bearing_centidegrees(),
                location.accuracy_cm(),
                location.updated_at_unix_seconds(),
            )
            .expect("store and Device API enforce identical location bounds")
        });
        let response = self.exchange(device_api::DeviceRequest::LxmfBasicSend {
            destination: device_api::DestinationHash(*material.destination().as_bytes()),
            timestamp_unix_ms: material.timestamp().get(),
            title: material.title(),
            content: material.content(),
            location,
            idempotency_key: device_api::IdempotencyKey(*material.idempotency_key().as_bytes()),
        })?;
        let device_api::DeviceResponse::LxmfBasicSendAccepted(accepted) = response else {
            unreachable!("exchange validated the response operation")
        };
        let submission_id = SubmissionId::new(accepted.id.0)
            .map_err(|_| DeviceSessionError::InvalidSubmissionId)?;
        Ok(AcceptanceIds::new(
            submission_id,
            MessageId::new(*accepted.message_id()),
        ))
    }

    fn submission_status(&mut self, id: SubmissionId) -> Result<SubmissionState, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::SubmissionStatus {
            id: device_api::SubmissionId(id.get()),
        })?;
        let device_api::DeviceResponse::SubmissionStatus(status) = response else {
            unreachable!("exchange validated the response operation")
        };
        map_submission_state(status.state)
    }

    fn next_inbox(
        &mut self,
        after: Option<InboxCursor>,
    ) -> Result<Option<InboxSummary>, Self::Error> {
        let after = after.map(|cursor| {
            device_api::LxmfMessageHandle::new(cursor.get())
                .expect("application inbox cursors are nonzero")
        });
        let response = match self.exchange(device_api::DeviceRequest::LxmfNext { after }) {
            Ok(response) => response,
            Err(DeviceSessionError::Api(error))
                if error.code == device_api::ApiErrorCode::NotFound =>
            {
                self.retained_inbox = None;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let device_api::DeviceResponse::LxmfNext(raw) = response else {
            unreachable!("exchange validated the response operation")
        };
        if after.is_some_and(|previous| raw.handle().get() <= previous.get()) {
            return Err(DeviceSessionError::InvalidLxmf(
                "inbox cursor did not advance".to_owned(),
            ));
        }
        let cursor =
            InboxCursor::new(raw.handle().get()).expect("device message handles are nonzero");
        let summary = InboxSummary::new(cursor, MessageId::new(*raw.message_id()));
        self.retained_inbox = Some((summary, raw));
        Ok(Some(summary))
    }

    fn read_inbox(&mut self, summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
        let Some((retained, raw)) = self.retained_inbox.take() else {
            return Err(DeviceSessionError::InboxSummaryNotRetained);
        };
        if retained != summary {
            return Err(DeviceSessionError::InboxSummaryNotRetained);
        }
        let wire = self.read_complete_lxmf(raw)?;
        let view = MessageView::parse_complete(&wire, location_wire_limits(wire.len()))
            .map_err(|error| DeviceSessionError::InvalidLxmf(error.to_string()))?;
        if view.normalized_wire_len() != raw.normalized_wire_len() as usize
            || view.message_id() != *raw.message_id()
            || view.destination_hash() != &raw.destination().0
            || view.source_hash() != &raw.source().0
            || view.payload().timestamp_bits() != raw.timestamp_bits()
            || view.payload().title().as_bytes().len() != raw.title_len() as usize
            || view.payload().content().as_bytes().len() != raw.content_len() as usize
            || view.payload().fields().raw().len() != raw.fields_encoded_len() as usize
        {
            return Err(DeviceSessionError::InvalidLxmf(
                "complete wire did not match its authenticated summary".to_owned(),
            ));
        }
        let timestamp = inbound_timestamp(view.payload().timestamp())?;
        let location = decode_message_location(view.payload().fields().raw());
        Ok(InboundMessage::new(
            summary.message_id(),
            DestinationHash::new(raw.destination().0),
            DestinationHash::new(raw.source().0),
            timestamp,
            view.payload().title().as_bytes().to_vec(),
            view.payload().content().as_bytes().to_vec(),
        )
        .with_location(location)
        .with_ingress_observation(map_ingress_observation(raw.ingress_observation())))
    }

    fn inbox_status(&mut self) -> Result<device_api::LxmfMailboxStatus, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::LxmfMailboxStatus)?;
        let device_api::DeviceResponse::LxmfMailboxStatus(status) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(status)
    }

    fn acknowledge_inbox_through(
        &mut self,
        through: InboxCursor,
    ) -> Result<device_api::LxmfMailboxStatus, Self::Error> {
        let through = device_api::LxmfMessageHandle::new(through.get())
            .expect("application inbox cursors are nonzero");
        let response =
            self.exchange(device_api::DeviceRequest::LxmfMailboxAcknowledge { through })?;
        let device_api::DeviceResponse::LxmfMailboxAcknowledged(status) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(status)
    }

    fn next_nearby_peer(
        &mut self,
        after: Option<device_api::LxmfPeerDiscoveryCursor>,
    ) -> Result<device_api::LxmfPeerDiscoveryPage, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::LxmfPeerNext { after })?;
        let device_api::DeviceResponse::LxmfPeerNext(page) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(page)
    }

    fn nomad_fetch_start(
        &mut self,
        request: device_api::NomadFetchStartRequest<'_>,
    ) -> Result<device_api::NomadFetchStartAccepted, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::NomadFetchStart(request))?;
        let device_api::DeviceResponse::NomadFetchStartAccepted(accepted) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(accepted)
    }

    fn nomad_fetch_poll(
        &mut self,
        id: device_api::NomadFetchId,
    ) -> Result<device_api::NomadFetchPollResponse, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::NomadFetchPoll(
            device_api::NomadFetchPollRequest { id },
        ))?;
        let device_api::DeviceResponse::NomadFetchPoll(response) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(response)
    }

    fn reticulum_probe_start(
        &mut self,
        request: device_api::ProbeStartRequest,
    ) -> Result<device_api::ProbeStartAccepted, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::ReticulumProbeStart(request))?;
        let device_api::DeviceResponse::ReticulumProbeStartAccepted(accepted) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(accepted)
    }

    fn reticulum_probe_poll(
        &mut self,
        id: device_api::ProbeId,
    ) -> Result<device_api::ProbePollResponse, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::ReticulumProbePoll(
            device_api::ProbePollRequest::new(id),
        ))?;
        let device_api::DeviceResponse::ReticulumProbePoll(response) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(response)
    }

    fn network_config_get(&mut self) -> Result<device_api::NetworkConfigSnapshot, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::NetworkConfigGet)?;
        let device_api::DeviceResponse::NetworkConfig(config) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(config)
    }

    fn network_config_mutate(
        &mut self,
        request: device_api::NetworkConfigMutationRequest<'_>,
    ) -> Result<device_api::NetworkConfigMutationOutcome, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::NetworkConfigMutate(request))?;
        let device_api::DeviceResponse::NetworkConfigMutation(outcome) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(outcome)
    }

    fn network_status(&mut self) -> Result<device_api::NetworkRuntimeStatus, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::NetworkStatus)?;
        let device_api::DeviceResponse::NetworkStatus(status) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(status)
    }

    fn manual_service_announce(
        &mut self,
    ) -> Result<device_api::ManualServiceAnnounceDisposition, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::ManualServiceAnnounce)?;
        let device_api::DeviceResponse::ManualServiceAnnounce(disposition) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(disposition)
    }

    fn node_diagnostics(&mut self) -> Result<device_api::NodeDiagnosticsSnapshot, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::NodeDiagnostics)?;
        let device_api::DeviceResponse::NodeDiagnostics(snapshot) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(snapshot)
    }

    fn route_diagnostics_page(
        &mut self,
        request: device_api::RouteDiagnosticsRequest,
    ) -> Result<device_api::RouteDiagnosticsPage, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::RouteDiagnosticsPage(request))?;
        let device_api::DeviceResponse::RouteDiagnosticsPage(page) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(page)
    }

    fn radio_trace_page(
        &mut self,
        request: device_api::RadioTracePageRequest,
    ) -> Result<device_api::RadioTracePage, Self::Error> {
        let response = self.exchange(device_api::DeviceRequest::RadioTracePage(request))?;
        let device_api::DeviceResponse::RadioTracePage(page) = response else {
            unreachable!("exchange validated the response operation")
        };
        Ok(page)
    }

    fn is_usable(&self) -> bool {
        self.requester.is_usable()
    }
}

fn decode_message_location(fields: &[u8]) -> Option<MessageLocation> {
    // Optional extension fields must never prevent an otherwise valid LXMF
    // message from reaching the inbox. Malformed or unsupported telemetry is
    // ignored here; the authenticated title and content remain usable.
    let telemetry = decode_sideband_location_fields(fields, location_wire_limits(fields.len()))
        .ok()
        .flatten()?;
    MessageLocation::new(
        telemetry.latitude_e6(),
        telemetry.longitude_e6(),
        telemetry.altitude_cm(),
        telemetry.speed_cm_per_second(),
        telemetry.bearing_centidegrees(),
        telemetry.accuracy_cm(),
        telemetry.updated_at_unix_seconds(),
    )
}

fn location_wire_limits(length: usize) -> WireLimits {
    WireLimits::new(
        length,
        length,
        length,
        length,
        length.saturating_mul(16).max(65_536),
        16,
    )
}

fn map_ingress_observation(
    ingress: Option<device_api::IngressObservation>,
) -> Option<MessageIngressObservation> {
    ingress.map(|ingress| {
        MessageIngressObservation::new(
            MessageInterfaceId::new(*ingress.interface_id().as_bytes()),
            ingress
                .signal()
                .map(|signal| MessageSignalObservation::new(signal.rssi_dbm(), signal.snr_db())),
        )
    })
}

fn map_submission_state(
    state: device_api::SubmissionState,
) -> Result<SubmissionState, DeviceSessionError> {
    let evidence = |details: device_api::PreparedPacketDetails| {
        PacketEvidence::new(
            details.packet_len,
            EncodedPacketSha256::new(*details.encoded_packet_sha256.as_bytes()),
        )
        .map_err(|_| DeviceSessionError::InvalidPacketEvidence)
    };
    match state {
        device_api::SubmissionState::Queued => Ok(SubmissionState::Queued),
        device_api::SubmissionState::Preparing => Ok(SubmissionState::Preparing),
        device_api::SubmissionState::AwaitingDelivery(details) => {
            Ok(SubmissionState::AwaitingDelivery(evidence(details)?))
        }
        device_api::SubmissionState::Delivered(details) => {
            Ok(SubmissionState::Delivered(evidence(details)?))
        }
        device_api::SubmissionState::ApplicationDelivered => {
            Ok(SubmissionState::ApplicationDelivered)
        }
        device_api::SubmissionState::Failed(failure) => {
            let failure = match failure {
                device_api::SubmissionFailure::NoPath => SubmissionFailure::NoPath,
                device_api::SubmissionFailure::DeliveryTimeout => {
                    SubmissionFailure::DeliveryTimeout
                }
                device_api::SubmissionFailure::Rejected => SubmissionFailure::DownstreamRejection,
                device_api::SubmissionFailure::Internal => SubmissionFailure::Internal,
            };
            Ok(SubmissionState::Failed(failure))
        }
        device_api::SubmissionState::Cancelled => Ok(SubmissionState::Cancelled),
    }
}

fn inbound_timestamp(seconds: f64) -> Result<UnixTimestampMillis, DeviceSessionError> {
    let milliseconds = seconds * 1_000.0;
    if !milliseconds.is_finite() || milliseconds < 1.0 || milliseconds > u64::MAX as f64 {
        return Err(DeviceSessionError::InvalidTimestamp {
            seconds_bits: seconds.to_bits(),
        });
    }
    UnixTimestampMillis::new(milliseconds.round() as u64).map_err(|_| {
        DeviceSessionError::InvalidTimestamp {
            seconds_bits: seconds.to_bits(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_lxmf_wire::{
        MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES, SidebandLocationTelemetry,
        encode_sideband_location_fields,
    };

    #[test]
    fn ingress_mapping_preserves_historical_interface_and_receiver_signal() {
        let mapped = map_ingress_observation(Some(device_api::IngressObservation::new(
            device_api::ReticulumInterfaceId::new([0, 0, 0, 0, 0, 0, 0, 7]),
            Some(device_api::IngressSignal::new(-97, 4)),
        )))
        .expect("ingress observation");

        assert_eq!(mapped.interface().as_bytes(), &[0, 0, 0, 0, 0, 0, 0, 7]);
        let signal = mapped.signal().expect("physical signal");
        assert_eq!(signal.rssi_dbm(), -97);
        assert_eq!(signal.snr_db(), 4);
        assert_eq!(map_ingress_observation(None), None);
    }

    #[test]
    fn sideband_location_fields_map_into_durable_message_units() {
        let expected = SidebandLocationTelemetry::new(
            44_123_456,
            -73_987_654,
            12_345,
            678,
            27_050,
            321,
            1_754_000_123,
        );
        let mut fields = [0_u8; MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES];
        let length = encode_sideband_location_fields(expected, &mut fields).expect("encode");

        let actual = decode_message_location(&fields[..length]).expect("location");
        assert_eq!(actual.latitude_e6(), expected.latitude_e6());
        assert_eq!(actual.longitude_e6(), expected.longitude_e6());
        assert_eq!(actual.altitude_cm(), expected.altitude_cm());
        assert_eq!(actual.speed_cm_per_second(), expected.speed_cm_per_second());
        assert_eq!(
            actual.bearing_centidegrees(),
            expected.bearing_centidegrees()
        );
        assert_eq!(actual.accuracy_cm(), expected.accuracy_cm());
        assert_eq!(
            actual.updated_at_unix_seconds(),
            expected.updated_at_unix_seconds()
        );
    }

    #[test]
    fn malformed_optional_telemetry_does_not_hide_message_content() {
        // One telemetry field whose value is not MessagePack binary.
        assert_eq!(decode_message_location(&[0x81, 0x02, 0xc0]), None);
    }
}
