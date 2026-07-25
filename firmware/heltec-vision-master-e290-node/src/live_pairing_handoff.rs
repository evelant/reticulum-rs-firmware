//! Depth-one owning handoff for the E290 live-pairing protocol.
//!
//! This is deliberately separate from the copy-only initialization-control
//! handoff. Live requests may own client proofs and live responses may own a
//! newly offered PSK, challenge, or activation confirmation. Channel pressure
//! therefore always returns the exact unsent owner for retry or zeroizing drop.

use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, TrySendError},
};
use reticulum_device_api_pairing::{BearerBinding, PairingRequest, PairingResponse};
use reticulum_device_api_pairing_policy::{ConnectionId, MonotonicMillis as PairingMillis};

/// Conservative internal-static RAM ceiling for both depth-one owning channels.
pub const LIVE_PAIRING_HANDOFF_RAM_CEILING: usize = 2_048;

/// One decoded live-pairing request routed from the bearer to the node owner.
///
/// This aggregate deliberately implements neither `Clone`, `Copy`, nor
/// `Debug`: an Activate request can contain the only client-proof owner.
#[must_use = "a live-pairing command must be handled or explicitly dropped"]
pub struct LivePairingCommand {
    at: PairingMillis,
    bearer: BearerBinding,
    connection: ConnectionId,
    request: PairingRequest,
}

impl LivePairingCommand {
    /// Bind one decoded request to its admission time and connection epoch.
    pub const fn new(
        at: PairingMillis,
        bearer: BearerBinding,
        connection: ConnectionId,
        request: PairingRequest,
    ) -> Self {
        Self {
            at,
            bearer,
            connection,
            request,
        }
    }

    /// Exact transport profile that decoded and owns this request.
    pub const fn bearer(&self) -> BearerBinding {
        self.bearer
    }

    /// Monotonic request-admission time supplied by the bearer owner.
    pub const fn at(&self) -> PairingMillis {
        self.at
    }

    /// Exact connection on which the complete request arrived.
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Borrow the request without separating it from routing provenance.
    pub const fn request(&self) -> &PairingRequest {
        &self.request
    }

    /// Consume this routed owner into its decoded request.
    pub fn into_request(self) -> PairingRequest {
        self.request
    }
}

/// One live-pairing response routed back to its exact connection epoch.
///
/// This aggregate deliberately implements neither `Clone`, `Copy`, nor
/// `Debug`: a successful Begin response owns the only outbound PSK offer.
#[must_use = "a live-pairing reply must be delivered or explicitly dropped as stale"]
pub struct LivePairingReply {
    connection: ConnectionId,
    response: PairingResponse,
}

impl LivePairingReply {
    /// Route one response to the connection that owns its request sequence.
    pub const fn new(connection: ConnectionId, response: PairingResponse) -> Self {
        Self {
            connection,
            response,
        }
    }

    /// Exact connection epoch to which this response belongs.
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Borrow the response without losing its routing epoch.
    pub const fn response(&self) -> &PairingResponse {
        &self.response
    }

    /// Consume this routed owner into its response.
    pub fn into_response(self) -> PairingResponse {
        self.response
    }
}

/// A full depth-one channel returned its exact unsent owner.
#[must_use = "live-pairing channel pressure retains the exact secret-bearing owner"]
pub struct LivePairingPressure<T> {
    owner: T,
}

impl<T> LivePairingPressure<T> {
    fn new(owner: T) -> Self {
        Self { owner }
    }

    /// Recover the unchanged command or reply that was not enqueued.
    pub fn into_inner(self) -> T {
        self.owner
    }
}

fn try_enqueue<M, T>(channel: &Channel<M, T, 1>, owner: T) -> Result<(), LivePairingPressure<T>>
where
    M: RawMutex,
{
    channel
        .try_send(owner)
        .map_err(|TrySendError::Full(owner)| LivePairingPressure::new(owner))
}

/// Unique bearer-side request producer and response consumer.
///
/// The bearer may be USB, BLE, Wi-Fi, or another future device-API transport;
/// connection-epoch allocation and framing remain outside this handoff.
#[must_use = "dropping the bearer endpoint abandons its live-pairing capabilities"]
pub struct BearerLivePairingHandoff<M>
where
    M: RawMutex + 'static,
{
    commands: &'static Channel<M, LivePairingCommand, 1>,
    replies: &'static Channel<M, LivePairingReply, 1>,
}

impl<M> BearerLivePairingHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Try to transfer one exact command to the node owner.
    pub fn try_send_command(
        &mut self,
        command: LivePairingCommand,
    ) -> Result<(), LivePairingPressure<LivePairingCommand>> {
        try_enqueue(self.commands, command)
    }

    /// Receive one node reply immediately, if queued.
    pub fn try_receive_reply(&mut self) -> Option<LivePairingReply> {
        self.replies.try_receive().ok()
    }

    #[cfg(test)]
    const fn command_capacity(&self) -> usize {
        1
    }

    #[cfg(test)]
    const fn reply_capacity(&self) -> usize {
        1
    }
}

/// Unique node-side request consumer and response producer.
#[must_use = "dropping the node endpoint abandons its live-pairing capabilities"]
pub struct NodeLivePairingHandoff<M>
where
    M: RawMutex + 'static,
{
    commands: &'static Channel<M, LivePairingCommand, 1>,
    replies: &'static Channel<M, LivePairingReply, 1>,
}

impl<M> NodeLivePairingHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Receive one bearer command immediately, if queued.
    pub fn try_receive_command(&mut self) -> Option<LivePairingCommand> {
        self.commands.try_receive().ok()
    }

    /// Try to transfer one exact reply to the bearer owner.
    pub fn try_send_reply(
        &mut self,
        reply: LivePairingReply,
    ) -> Result<(), LivePairingPressure<LivePairingReply>> {
        try_enqueue(self.replies, reply)
    }

    #[cfg(test)]
    const fn command_capacity(&self) -> usize {
        1
    }

    #[cfg(test)]
    const fn reply_capacity(&self) -> usize {
        1
    }
}

/// Static depth-one storage for one live-pairing bearer/node relationship.
pub struct LivePairingHandoff<M>
where
    M: RawMutex,
{
    commands: Channel<M, LivePairingCommand, 1>,
    replies: Channel<M, LivePairingReply, 1>,
}

impl<M> LivePairingHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Construct empty command and reply stores.
    pub const fn new() -> Self {
        Self {
            commands: Channel::new(),
            replies: Channel::new(),
        }
    }

    /// Split this store into its only bearer and node endpoint roles.
    pub fn split(&'static mut self) -> (BearerLivePairingHandoff<M>, NodeLivePairingHandoff<M>) {
        (
            BearerLivePairingHandoff {
                commands: &self.commands,
                replies: &self.replies,
            },
            NodeLivePairingHandoff {
                commands: &self.commands,
                replies: &self.replies,
            },
        )
    }
}

impl<M> Default for LivePairingHandoff<M>
where
    M: RawMutex + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(
    core::mem::size_of::<LivePairingHandoff<embassy_sync::blocking_mutex::raw::NoopRawMutex>>()
        <= LIVE_PAIRING_HANDOFF_RAM_CEILING
);

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use reticulum_device_api_credentials::{CredentialGeneration, CredentialId};
    use reticulum_device_api_pairing::{
        AbortCurrentRequest, AbortCurrentResponse, AbortResult, BeginOffer, BeginRequest,
        BeginResponse, DeviceId, PairingPsk, PairingRequest, PairingResponse,
    };
    use reticulum_device_api_pairing_policy::{ConnectionId, MonotonicMillis as PairingMillis};

    use super::{
        LIVE_PAIRING_HANDOFF_RAM_CEILING, LivePairingCommand, LivePairingHandoff, LivePairingReply,
    };

    fn connection(value: u64) -> ConnectionId {
        ConnectionId::new(value).expect("test connection must be nonzero")
    }

    fn handoff() -> (
        super::BearerLivePairingHandoff<NoopRawMutex>,
        super::NodeLivePairingHandoff<NoopRawMutex>,
    ) {
        std::boxed::Box::leak(std::boxed::Box::new(LivePairingHandoff::new())).split()
    }

    #[test]
    fn command_fifo_and_pressure_return_the_exact_owner() {
        let (mut usb, mut node) = handoff();
        let first = LivePairingCommand::new(
            PairingMillis::new(10),
            reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
            connection(1),
            PairingRequest::Begin(BeginRequest::new(7)),
        );
        let second = LivePairingCommand::new(
            PairingMillis::new(11),
            reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
            connection(1),
            PairingRequest::AbortCurrent(AbortCurrentRequest::new(8)),
        );

        assert_eq!(usb.command_capacity(), 1);
        assert_eq!(node.command_capacity(), 1);
        assert!(usb.try_send_command(first).is_ok(), "first command fits");
        let retained = usb
            .try_send_command(second)
            .expect_err("second command observes depth-one pressure")
            .into_inner();
        assert_eq!(retained.at(), PairingMillis::new(11));
        assert_eq!(retained.connection(), connection(1));
        assert_eq!(retained.request().sequence(), 8);

        let received = node.try_receive_command().expect("first command queued");
        assert_eq!(received.request().sequence(), 7);
        assert!(
            usb.try_send_command(retained).is_ok(),
            "retained command fits after receive"
        );
        assert_eq!(
            node.try_receive_command()
                .expect("retained command queued")
                .into_request()
                .sequence(),
            8
        );
        assert!(node.try_receive_command().is_none());
    }

    #[test]
    fn secret_response_pressure_preserves_the_only_psk_owner() {
        let (mut usb, mut node) = handoff();
        let first = LivePairingReply::new(
            connection(4),
            PairingResponse::AbortCurrent(AbortCurrentResponse::new(20, AbortResult::Aborted)),
        );
        let offer = BeginOffer::after_pending_commit(
            reticulum_device_api_pairing::BearerBinding::UsbSerialJtag,
            DeviceId::new([0x11; 16]).expect("device ID is nonzero"),
            CredentialId::new([0x22; 16]),
            CredentialGeneration::new(3),
            PairingPsk::new([0x33; 32]).expect("PSK is nonzero"),
        )
        .expect("offer fields are valid");
        let second = LivePairingReply::new(
            connection(4),
            PairingResponse::Begin(BeginResponse::offered(21, offer)),
        );

        assert_eq!(usb.reply_capacity(), 1);
        assert_eq!(node.reply_capacity(), 1);
        assert!(node.try_send_reply(first).is_ok(), "first reply fits");
        let retained = node
            .try_send_reply(second)
            .expect_err("secret reply observes depth-one pressure")
            .into_inner();
        assert_eq!(retained.connection(), connection(4));
        assert_eq!(retained.response().sequence(), 21);

        assert_eq!(
            usb.try_receive_reply()
                .expect("first reply queued")
                .into_response()
                .sequence(),
            20
        );
        assert!(
            node.try_send_reply(retained).is_ok(),
            "secret reply fits after receive"
        );
        let response = usb
            .try_receive_reply()
            .expect("secret reply queued")
            .into_response();
        match response {
            PairingResponse::Begin(response) => {
                let offer = response.offer().expect("Begin response kept its offer");
                assert_eq!(offer.psk().as_bytes(), &[0x33; 32]);
            }
            _ => panic!("wrong response variant crossed the handoff"),
        }
        assert!(usb.try_receive_reply().is_none());
    }

    #[test]
    fn owning_handoff_stays_within_its_static_ram_ceiling() {
        assert!(
            core::mem::size_of::<LivePairingHandoff<NoopRawMutex>>()
                <= LIVE_PAIRING_HANDOFF_RAM_CEILING
        );
    }
}
