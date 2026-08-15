//! Exact, fixed-capacity device-API owners and their correlation metadata.

use core::mem;

use zeroize::Zeroize;

/// Maximum encoded request or reply size carried by the local API handoff.
///
/// This is the logical API's authoritative message limit, rather than an
/// independently repeated handoff constant.
pub const MESSAGE_CAPACITY: usize = reticulum_device_api::MAX_MESSAGE_BYTES;

/// A bearer-session generation used only for response routing.
///
/// Advancing the epoch invalidates replies for a disconnected session. It
/// does not cancel a request already transferred to the node owner.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionEpoch(u64);

impl SessionEpoch {
    /// Construct a session epoch selected by the session runtime.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the raw session-runtime value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Bearer-selected correlation for one request within a session epoch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CorrelationId(u64);

impl CorrelationId {
    /// Construct a correlation identifier.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the raw bearer-selected value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Complete response-routing key for one local API request.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestKey {
    epoch: SessionEpoch,
    correlation: CorrelationId,
}

impl RequestKey {
    /// Bind a request correlation to the session epoch that originated it.
    pub const fn new(epoch: SessionEpoch, correlation: CorrelationId) -> Self {
        Self { epoch, correlation }
    }

    /// Session epoch that originated this request.
    pub const fn epoch(self) -> SessionEpoch {
        self.epoch
    }

    /// Correlation identifier within the originating epoch.
    pub const fn correlation(self) -> CorrelationId {
        self.correlation
    }
}

/// Valid encoded message length bounded by [`MESSAGE_CAPACITY`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageLength(u16);

impl MessageLength {
    /// Validate an encoded message length.
    pub const fn new(length: usize) -> Result<Self, MessageTooLong> {
        if length <= MESSAGE_CAPACITY {
            Ok(Self(length as u16))
        } else {
            Err(MessageTooLong { length })
        }
    }

    /// Return the validated length as `usize`.
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// An encoded message length exceeded the fixed handoff capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageTooLong {
    length: usize,
}

impl MessageTooLong {
    /// Rejected encoded length.
    pub const fn length(self) -> usize {
        self.length
    }
}

/// One exact fixed-capacity encoded request or reply buffer.
///
/// Bytes beyond [`Self::length`] are deliberately retained unchanged. The
/// bearer is responsible for clearing sensitive scratch bytes before creating
/// an owner if its framing policy requires that behavior.
#[must_use = "an encoded device-API message owner must be transferred or explicitly dropped"]
pub struct OwnedMessage {
    length: MessageLength,
    buffer: [u8; MESSAGE_CAPACITY],
}

impl OwnedMessage {
    /// Bind a validated encoded length to one exact fixed buffer.
    pub const fn new(length: MessageLength, buffer: [u8; MESSAGE_CAPACITY]) -> Self {
        Self { length, buffer }
    }

    /// Valid encoded length.
    pub const fn length(&self) -> MessageLength {
        self.length
    }

    /// Encoded bytes, excluding retained scratch bytes after the valid length.
    pub fn encoded(&self) -> &[u8] {
        &self.buffer[..self.length.get()]
    }

    /// Complete fixed buffer, including bytes after the encoded length.
    pub const fn full_buffer(&self) -> &[u8; MESSAGE_CAPACITY] {
        &self.buffer
    }

    /// Erase the complete allocation, including retained framing scratch.
    ///
    /// This is normally invoked automatically by [`Drop`], but is public so a
    /// fail-stop owner can erase sensitive request bytes before retaining its
    /// routing and authorization metadata for diagnostics.
    pub fn zeroize_contents(&mut self) {
        self.buffer.zeroize();
    }

    /// Recover the complete fixed buffer and validated encoded length.
    pub fn into_parts(mut self) -> (MessageLength, [u8; MESSAGE_CAPACITY]) {
        let length = self.length;
        let buffer = mem::replace(&mut self.buffer, [0; MESSAGE_CAPACITY]);
        (length, buffer)
    }
}

impl Drop for OwnedMessage {
    fn drop(&mut self) {
        self.zeroize_contents();
    }
}

/// Exact authenticated local API request owner.
///
/// `G` should be a non-cloneable session-layer reference to device-owned
/// credential state, not a client-supplied principal or permission snapshot.
/// Enqueuing transfers that exact reference and the encoded bytes to the node;
/// later connection loss does not revoke the accepted work.
#[must_use = "a local API request owner must be enqueued, retained, or explicitly rejected"]
pub struct LocalApiRequest<G> {
    key: RequestKey,
    grant: G,
    message: OwnedMessage,
}

impl<G> LocalApiRequest<G> {
    /// Construct one exact authenticated request owner.
    pub const fn new(key: RequestKey, grant: G, message: OwnedMessage) -> Self {
        Self {
            key,
            grant,
            message,
        }
    }

    /// Response-routing key captured at admission.
    pub const fn key(&self) -> RequestKey {
        self.key
    }

    /// Opaque credential reference minted by the authenticated session layer.
    pub const fn grant(&self) -> &G {
        &self.grant
    }

    /// Exact encoded request owner.
    pub const fn message(&self) -> &OwnedMessage {
        &self.message
    }

    /// Consume this request into its exact routing, grant, and message owners.
    pub fn into_parts(self) -> (RequestKey, G, OwnedMessage) {
        (self.key, self.grant, self.message)
    }
}

/// Exact local API reply owner.
///
/// The reply echoes the complete request key. The bearer must compare both
/// epoch and correlation before delivering it to a live connection.
#[must_use = "a local API reply owner must be enqueued, retained, or explicitly discarded"]
pub struct LocalApiReply {
    key: RequestKey,
    message: OwnedMessage,
}

impl LocalApiReply {
    /// Construct an encoded reply for an exact request key.
    pub const fn new(key: RequestKey, message: OwnedMessage) -> Self {
        Self { key, message }
    }

    /// Echoed request routing key.
    pub const fn key(&self) -> RequestKey {
        self.key
    }

    /// Whether this reply belongs to the currently live session epoch.
    pub const fn belongs_to_epoch(&self, epoch: SessionEpoch) -> bool {
        self.key.epoch.0 == epoch.0
    }

    /// Whether this reply exactly matches an awaited request.
    pub const fn matches(&self, expected: RequestKey) -> bool {
        self.key.epoch.0 == expected.epoch.0 && self.key.correlation.0 == expected.correlation.0
    }

    /// Exact encoded response owner.
    pub const fn message(&self) -> &OwnedMessage {
        &self.message
    }

    /// Consume this reply into its exact routing and message owners.
    pub fn into_parts(self) -> (RequestKey, OwnedMessage) {
        (self.key, self.message)
    }
}
