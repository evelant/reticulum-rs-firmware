//! App-facing durable message-activity queries and projections.

use std::fmt;

use reticulum_lxmf_chat_core as core;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;

use super::{
    JsonSafeInteger, MAX_JSON_SAFE_INTEGER, MessageIngressObservationView, MessageLocationView,
    PacketEvidenceView, PhoneLocationObservationView, TimelineDirection, TimelineStatus,
    serialize_json_safe_u64, serialize_optional_json_safe_u64, status_name,
};

fn deserialize_optional_json_safe_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u64>::deserialize(deserializer)?;
    if value.is_some_and(|value| value > MAX_JSON_SAFE_INTEGER) {
        return Err(D::Error::custom(
            "integer exceeds the JSON safe-integer contract",
        ));
    }
    Ok(value)
}

/// App-facing bounded newest-first activity query.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, TS)]
#[allow(missing_docs)]
pub struct MessageActivityPageRequest {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_json_safe_u64",
        serialize_with = "serialize_optional_json_safe_u64"
    )]
    #[ts(as = "Option<JsonSafeInteger>")]
    before_event_id: Option<u64>,
    limit: u16,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_json_safe_u64",
        serialize_with = "serialize_optional_json_safe_u64"
    )]
    #[ts(as = "Option<JsonSafeInteger>")]
    timeline_sequence: Option<u64>,
}

impl MessageActivityPageRequest {
    /// Construct a request for validation by the runtime boundary.
    pub const fn new(
        before_event_id: Option<u64>,
        limit: u16,
        timeline_sequence: Option<u64>,
    ) -> Self {
        Self {
            before_event_id,
            limit,
            timeline_sequence,
        }
    }

    /// Validate the cursor, scope, and shared page-size bound.
    pub fn validate(&self) -> Result<(), MessageActivityRequestError> {
        self.as_core().map(|_| ())
    }

    pub(crate) fn as_core(
        &self,
    ) -> Result<core::MessageActivityPageRequest, MessageActivityRequestError> {
        let before = self
            .before_event_id
            .map(|value| {
                if value > MAX_JSON_SAFE_INTEGER {
                    return Err(MessageActivityRequestError::InvalidBeforeEventId);
                }
                core::MessageActivityId::new(value)
                    .ok_or(MessageActivityRequestError::InvalidBeforeEventId)
            })
            .transpose()?;
        let scope = self
            .timeline_sequence
            .map(|value| {
                if value > MAX_JSON_SAFE_INTEGER {
                    return Err(MessageActivityRequestError::InvalidTimelineSequence);
                }
                core::TimelineSequence::new(value)
                    .map(core::MessageActivityScope::Timeline)
                    .ok_or(MessageActivityRequestError::InvalidTimelineSequence)
            })
            .transpose()?
            .unwrap_or(core::MessageActivityScope::All);
        core::MessageActivityPageRequest::new(scope, before, usize::from(self.limit))
            .map_err(|_| MessageActivityRequestError::InvalidLimit)
    }
}

/// Invalid app-facing message-activity query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageActivityRequestError {
    /// The exclusive activity cursor was zero or not JSON-safe.
    InvalidBeforeEventId,
    /// The requested timeline sequence was zero or not JSON-safe.
    InvalidTimelineSequence,
    /// Page size was zero or exceeded the shared bound.
    InvalidLimit,
}

impl fmt::Display for MessageActivityRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBeforeEventId => {
                formatter.write_str("message activity cursor must be a JSON-safe non-zero integer")
            }
            Self::InvalidTimelineSequence => formatter.write_str(
                "message activity timeline sequence must be a JSON-safe non-zero integer",
            ),
            Self::InvalidLimit => write!(
                formatter,
                "message activity page limit must be within 1..={}",
                core::MAX_MESSAGE_ACTIVITY_PAGE_SIZE
            ),
        }
    }
}

/// Cause of a successful outbound replacement submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum MessageActivityRetryTriggerView {
    /// Explicit user-created replacement submission.
    Manual,
    /// Historical app-owned automatic rearm retained for older activity data.
    /// Current board retries do not emit this event.
    Automatic,
}

/// App-facing semantic payload for one durable activity event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum MessageActivityKindView {
    InboundImported {
        message_id: String,
    },
    OutboundQueued,
    OutboundAccepted {
        #[serde(serialize_with = "serialize_json_safe_u64")]
        #[ts(as = "JsonSafeInteger")]
        submission_id: u64,
        message_id: String,
    },
    OutboundStatus {
        status: TimelineStatus,
        packet_evidence: Option<PacketEvidenceView>,
    },
    OutboundRequeued {
        trigger: MessageActivityRetryTriggerView,
    },
}

impl From<core::MessageActivityKind> for MessageActivityKindView {
    fn from(kind: core::MessageActivityKind) -> Self {
        match kind {
            core::MessageActivityKind::InboundImported { message_id } => Self::InboundImported {
                message_id: hex::encode(message_id.as_bytes()),
            },
            core::MessageActivityKind::OutboundQueued { .. } => Self::OutboundQueued,
            core::MessageActivityKind::OutboundAccepted { acceptance } => Self::OutboundAccepted {
                submission_id: acceptance.submission_id().get(),
                message_id: hex::encode(acceptance.message_id().as_bytes()),
            },
            core::MessageActivityKind::OutboundStatus { state } => {
                let packet_evidence = match state {
                    core::SubmissionState::AwaitingDelivery(evidence)
                    | core::SubmissionState::Delivered(evidence) => {
                        Some(PacketEvidenceView::from(evidence))
                    }
                    core::SubmissionState::Queued
                    | core::SubmissionState::Preparing
                    | core::SubmissionState::Failed(_)
                    | core::SubmissionState::Cancelled => None,
                };
                Self::OutboundStatus {
                    status: status_name(core::OutboxStatus::Device(state)),
                    packet_evidence,
                }
            }
            core::MessageActivityKind::OutboundRequeued { trigger, .. } => Self::OutboundRequeued {
                trigger: match trigger {
                    core::MessageActivityRetryTrigger::Manual => {
                        MessageActivityRetryTriggerView::Manual
                    }
                    core::MessageActivityRetryTrigger::Automatic => {
                        MessageActivityRetryTriggerView::Automatic
                    }
                },
            },
        }
    }
}

/// One app-facing immutable durable message-activity event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct MessageActivityEventView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    event_id: u64,
    #[serde(serialize_with = "serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    observed_at_unix_ms: Option<u64>,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    timeline_sequence: u64,
    peer: String,
    direction: TimelineDirection,
    #[serde(serialize_with = "serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    outbox_id: Option<u64>,
    attempt_number: Option<u32>,
    attempt_location: Option<PhoneLocationObservationView>,
    ingress_observation: Option<MessageIngressObservationView>,
    message_location: Option<MessageLocationView>,
    receiver_location: Option<PhoneLocationObservationView>,
    activity: MessageActivityKindView,
}

impl MessageActivityEventView {
    /// Stable event ordering and pagination identifier.
    pub const fn event_id(&self) -> u64 {
        self.event_id
    }

    /// Stable message timeline sequence.
    pub const fn timeline_sequence(&self) -> u64 {
        self.timeline_sequence
    }

    /// Best reconstructable one-based app-created device submission.
    pub const fn attempt_number(&self) -> Option<u32> {
        self.attempt_number
    }

    /// Phone location captured when this event began an app-created submission.
    pub const fn attempt_location(&self) -> Option<PhoneLocationObservationView> {
        self.attempt_location
    }

    /// Canonical receiver-local first-arrival evidence for an inbound row.
    pub const fn ingress_observation(&self) -> Option<MessageIngressObservationView> {
        self.ingress_observation
    }

    /// Authenticated sender-attached location for an inbound message.
    pub const fn message_location(&self) -> Option<MessageLocationView> {
        self.message_location
    }

    /// Receiver phone position retained when an inbound message was imported.
    pub const fn receiver_location(&self) -> Option<PhoneLocationObservationView> {
        self.receiver_location
    }

    /// Semantic activity payload.
    pub const fn activity(&self) -> &MessageActivityKindView {
        &self.activity
    }
}

impl From<core::MessageActivityEvent> for MessageActivityEventView {
    fn from(event: core::MessageActivityEvent) -> Self {
        Self {
            event_id: event.id().get(),
            observed_at_unix_ms: event.observed_at_unix_ms(),
            timeline_sequence: event.timeline_sequence().get(),
            peer: hex::encode(event.peer().as_bytes()),
            direction: match event.direction() {
                core::TimelineDirection::Inbound => TimelineDirection::Inbound,
                core::TimelineDirection::Outbound => TimelineDirection::Outbound,
            },
            outbox_id: event.outbox_id().map(core::OutboxId::get),
            attempt_number: event.attempt_number().map(core::MessageAttemptNumber::get),
            attempt_location: event
                .attempt_location()
                .map(PhoneLocationObservationView::from),
            ingress_observation: event
                .ingress_observation()
                .map(MessageIngressObservationView::from),
            message_location: event.message_location().map(MessageLocationView::from),
            receiver_location: event.receiver_location().map(|sample| {
                PhoneLocationObservationView::from(core::AttemptLocationStamp::Available(sample))
            }),
            activity: MessageActivityKindView::from(event.kind()),
        }
    }
}

/// One bounded newest-first app-facing message-activity page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct MessageActivityPageView {
    events: Vec<MessageActivityEventView>,
    #[serde(serialize_with = "serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    next_before_event_id: Option<u64>,
    history_incomplete: bool,
}

impl MessageActivityPageView {
    /// Newest-first immutable events.
    pub fn events(&self) -> &[MessageActivityEventView] {
        &self.events
    }

    /// Exclusive cursor for the next older page.
    pub const fn next_before_event_id(&self) -> Option<u64> {
        self.next_before_event_id
    }

    /// Whether migration or retention omitted older activity.
    pub const fn history_incomplete(&self) -> bool {
        self.history_incomplete
    }
}

impl From<core::MessageActivityPage> for MessageActivityPageView {
    fn from(page: core::MessageActivityPage) -> Self {
        Self {
            events: page
                .events()
                .iter()
                .copied()
                .map(MessageActivityEventView::from)
                .collect(),
            next_before_event_id: page.next_before().map(core::MessageActivityId::get),
            history_incomplete: page.history_incomplete(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_rejects_zero_and_unbounded_values() {
        assert_eq!(
            MessageActivityPageRequest::new(Some(0), 10, None).validate(),
            Err(MessageActivityRequestError::InvalidBeforeEventId)
        );
        assert_eq!(
            MessageActivityPageRequest::new(None, 0, None).validate(),
            Err(MessageActivityRequestError::InvalidLimit)
        );
        assert_eq!(
            MessageActivityPageRequest::new(None, 10, Some(0)).validate(),
            Err(MessageActivityRequestError::InvalidTimelineSequence)
        );
        assert!(
            MessageActivityPageRequest::new(None, 100, Some(1))
                .validate()
                .is_ok()
        );
    }
}
