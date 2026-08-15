use core::{fmt, num::NonZeroU64};

use reticulum_device_api as device_api;
use reticulum_device_client::{BasicLxmfSend, ClientError, ClientTransport, DeviceClient};
use reticulum_lxmf_chat_core::{
    AcceptanceIds, DestinationHash, DeviceBinding, EncodedPacketSha256, InboundMessage, MessageId,
    MessageIngressObservation, MessageInterfaceId, MessageLocation, MessageSignalObservation,
    OutboxMaterial, PacketEvidence, SubmissionFailure, SubmissionId, SubmissionState,
    UnixTimestampMillis,
};
use reticulum_lxmf_wire::{WireLimits, decode_sideband_location_fields};

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
    fn inbox_status(&mut self) -> Result<device_api::LxmfMailboxStatus, Self::Error> {
        Ok(device_api::LxmfMailboxStatus::new(None, None)
            .expect("an empty mailbox status is valid"))
    }

    /// Idempotently acknowledge every message through a locally durable cursor.
    fn acknowledge_inbox_through(
        &mut self,
        through: InboxCursor,
    ) -> Result<device_api::LxmfMailboxStatus, Self::Error> {
        let handle = device_api::LxmfMessageHandle::new(through.get())
            .expect("application inbox cursors are always nonzero");
        Ok(
            device_api::LxmfMailboxStatus::new(Some(handle), Some(handle))
                .expect("acknowledging the projected mailbox tail is valid"),
        )
    }

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

/// Typed application adapter failure around [`DeviceClient`].
#[derive(Debug)]
pub enum DeviceSessionError {
    /// Authenticated device protocol or transport failure.
    Client(ClientError),
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
            Self::Client(error) => write!(formatter, "device client failed: {error}"),
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

impl std::error::Error for DeviceSessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Client(error) => Some(error),
            Self::MissingLxmfDeliveryDestination
            | Self::InvalidSubmissionId
            | Self::InvalidPacketEvidence
            | Self::InvalidTimestamp { .. }
            | Self::InboxSummaryNotRetained
            | Self::InvalidLxmf(_) => None,
        }
    }
}

impl From<ClientError> for DeviceSessionError {
    fn from(value: ClientError) -> Self {
        Self::Client(value)
    }
}

/// [`LxmfSession`] implementation over one reusable authenticated client.
pub struct DeviceClientSession<T> {
    client: DeviceClient<T>,
    retained_inbox: Option<(InboxSummary, device_api::LxmfMessageSummary)>,
}

impl<T: ClientTransport> DeviceClientSession<T> {
    /// Wrap an established authenticated device client.
    pub const fn new(client: DeviceClient<T>) -> Self {
        Self {
            client,
            retained_inbox: None,
        }
    }

    /// Borrow the underlying client for connection diagnosis.
    pub const fn client(&self) -> &DeviceClient<T> {
        &self.client
    }

    /// Recover the underlying client, ending application-layer ownership.
    pub fn into_inner(self) -> DeviceClient<T> {
        self.client
    }
}

impl<T: ClientTransport> LxmfSession for DeviceClientSession<T> {
    type Error = DeviceSessionError;

    fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
        let identity = self.client.identity_summary()?;
        let lxmf = identity
            .lxmf_delivery_destination()
            .ok_or(DeviceSessionError::MissingLxmfDeliveryDestination)?;
        Ok(DeviceBinding::new(
            *self.client.device_id().as_bytes(),
            DestinationHash::new(identity.primary_destination().0),
            DestinationHash::new(lxmf.0),
        ))
    }

    fn submit(&mut self, material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
        let mut request = BasicLxmfSend::new(
            device_api::DestinationHash(*material.destination().as_bytes()),
            material.timestamp().get(),
            material.title(),
            material.content(),
            device_api::IdempotencyKey(*material.idempotency_key().as_bytes()),
        );
        if let Some(location) = material.location() {
            let location = device_api::LxmfMessageLocation::new(
                location.latitude_e6(),
                location.longitude_e6(),
                location.altitude_cm(),
                location.speed_cm_per_second(),
                location.bearing_centidegrees(),
                location.accuracy_cm(),
                location.updated_at_unix_seconds(),
            )
            .expect("chat-core and device API enforce identical coordinate bounds");
            request = request.with_location(location);
        }
        let accepted = self.client.lxmf_basic_send(request)?;
        let submission_id = SubmissionId::new(accepted.id.0)
            .map_err(|_| DeviceSessionError::InvalidSubmissionId)?;
        Ok(AcceptanceIds::new(
            submission_id,
            MessageId::new(*accepted.message_id()),
        ))
    }

    fn submission_status(&mut self, id: SubmissionId) -> Result<SubmissionState, Self::Error> {
        let status = self
            .client
            .submission_status(device_api::SubmissionId(id.get()))?;
        map_submission_state(status.state)
    }

    fn next_inbox(
        &mut self,
        after: Option<InboxCursor>,
    ) -> Result<Option<InboxSummary>, Self::Error> {
        let after = after.map(|cursor| {
            device_api::LxmfMessageHandle::new(cursor.get())
                .expect("application inbox cursor is always nonzero")
        });
        let Some(raw) = self.client.lxmf_next(after)? else {
            self.retained_inbox = None;
            return Ok(None);
        };
        let cursor = InboxCursor::new(raw.handle().get())
            .expect("device API message handles are always nonzero");
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
        let message = self.client.lxmf_read_summary(raw)?;
        let view = message
            .view()
            .map_err(|error| DeviceSessionError::InvalidLxmf(error.to_string()))?;
        let timestamp = inbound_timestamp(view.payload().timestamp())?;
        let ingress = map_ingress_observation(raw.ingress_observation());
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
        .with_ingress_observation(ingress))
    }

    fn inbox_status(&mut self) -> Result<device_api::LxmfMailboxStatus, Self::Error> {
        Ok(self.client.lxmf_mailbox_status()?)
    }

    fn acknowledge_inbox_through(
        &mut self,
        through: InboxCursor,
    ) -> Result<device_api::LxmfMailboxStatus, Self::Error> {
        let handle = device_api::LxmfMessageHandle::new(through.get())
            .expect("application inbox cursors are always nonzero");
        Ok(self.client.lxmf_mailbox_acknowledge(handle)?)
    }

    fn next_nearby_peer(
        &mut self,
        after: Option<device_api::LxmfPeerDiscoveryCursor>,
    ) -> Result<device_api::LxmfPeerDiscoveryPage, Self::Error> {
        Ok(self.client.lxmf_peer_next(after)?)
    }

    fn nomad_fetch_start(
        &mut self,
        request: device_api::NomadFetchStartRequest<'_>,
    ) -> Result<device_api::NomadFetchStartAccepted, Self::Error> {
        Ok(self.client.nomad_fetch_start(request)?)
    }

    fn nomad_fetch_poll(
        &mut self,
        id: device_api::NomadFetchId,
    ) -> Result<device_api::NomadFetchPollResponse, Self::Error> {
        Ok(self.client.nomad_fetch_poll(id)?)
    }

    fn reticulum_probe_start(
        &mut self,
        request: device_api::ProbeStartRequest,
    ) -> Result<device_api::ProbeStartAccepted, Self::Error> {
        Ok(self.client.reticulum_probe_start(request)?)
    }

    fn reticulum_probe_poll(
        &mut self,
        id: device_api::ProbeId,
    ) -> Result<device_api::ProbePollResponse, Self::Error> {
        Ok(self.client.reticulum_probe_poll(id)?)
    }

    fn network_config_get(&mut self) -> Result<device_api::NetworkConfigSnapshot, Self::Error> {
        Ok(self.client.network_config_get()?)
    }

    fn network_config_mutate(
        &mut self,
        request: device_api::NetworkConfigMutationRequest<'_>,
    ) -> Result<device_api::NetworkConfigMutationOutcome, Self::Error> {
        Ok(self.client.network_config_mutate(request)?)
    }

    fn network_status(&mut self) -> Result<device_api::NetworkRuntimeStatus, Self::Error> {
        Ok(self.client.network_status()?)
    }

    fn manual_service_announce(
        &mut self,
    ) -> Result<device_api::ManualServiceAnnounceDisposition, Self::Error> {
        Ok(self.client.manual_service_announce()?)
    }

    fn node_diagnostics(&mut self) -> Result<device_api::NodeDiagnosticsSnapshot, Self::Error> {
        Ok(self.client.node_diagnostics()?)
    }

    fn route_diagnostics_page(
        &mut self,
        request: device_api::RouteDiagnosticsRequest,
    ) -> Result<device_api::RouteDiagnosticsPage, Self::Error> {
        Ok(self.client.route_diagnostics_page(request)?)
    }

    fn radio_trace_page(
        &mut self,
        request: device_api::RadioTracePageRequest,
    ) -> Result<device_api::RadioTracePage, Self::Error> {
        Ok(self.client.radio_trace_page(request)?)
    }

    fn is_usable(&self) -> bool {
        self.client.is_session_available()
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
    ingress: Option<device_api::LxmfIngressObservation>,
) -> Option<MessageIngressObservation> {
    ingress.map(|ingress| {
        MessageIngressObservation::new(
            MessageInterfaceId::new(ingress.interface_id()),
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
        let mapped = map_ingress_observation(Some(device_api::LxmfIngressObservation::new(
            7,
            Some(device_api::LxmfIngressSignal::new(-97, 4)),
        )))
        .expect("ingress observation");

        assert_eq!(mapped.interface().get(), 7);
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
