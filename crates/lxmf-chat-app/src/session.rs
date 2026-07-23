use core::{fmt, num::NonZeroU64};

use reticulum_device_api as device_api;
use reticulum_device_client::{BasicLxmfSend, ClientError, ClientTransport, DeviceClient};
use reticulum_lxmf_chat_core::{
    AcceptanceIds, DestinationHash, DeviceBinding, EncodedPacketSha256, InboundMessage, MessageId,
    OutboxMaterial, PacketEvidence, SubmissionFailure, SubmissionId, SubmissionState,
    UnixTimestampMillis,
};

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
        let accepted = self.client.lxmf_basic_send(BasicLxmfSend::new(
            device_api::DestinationHash(*material.destination().as_bytes()),
            material.timestamp().get(),
            material.title(),
            material.content(),
            device_api::IdempotencyKey(*material.idempotency_key().as_bytes()),
        ))?;
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
        Ok(InboundMessage::new(
            summary.message_id(),
            DestinationHash::new(raw.destination().0),
            DestinationHash::new(raw.source().0),
            timestamp,
            view.payload().title().as_bytes().to_vec(),
            view.payload().content().as_bytes().to_vec(),
        ))
    }

    fn is_usable(&self) -> bool {
        self.client.is_session_available()
    }
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
