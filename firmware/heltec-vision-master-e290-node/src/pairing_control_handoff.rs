//! Depth-one command/reply handoff for the E290 pre-authentication pairing lane.
//!
//! The USB/GPIO task owns connection detection, button debounce, framing, and
//! response delivery. The node task owns pairing policy and the sole storage
//! coordinator. These channels transfer only non-secret scalar commands and
//! responses between those owners. In particular, the opaque
//! `AcquirePairingExclusive` capability never enters this handoff.

use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, TrySendError},
};
use reticulum_device_api_pairing_control::{ControlRequest, ControlResponse};
use reticulum_device_api_pairing_policy::{
    ActiveLowButton, ConnectionId, MonotonicMillis as PairingMillis,
};

/// Conservative internal-static RAM ceiling for both depth-one channels.
pub const PAIRING_CONTROL_HANDOFF_RAM_CEILING: usize = 1_024;

/// One event or request transferred from the USB/GPIO owner to the node owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a pairing-control command must be handled or explicitly rejected"]
pub enum PairingControlCommand {
    /// Announce one newly allocated boot-lifetime connection epoch.
    Connected {
        /// Monotonic time at which the bearer observed the connection.
        at: PairingMillis,
        /// Nonzero epoch allocated by the boot-lifetime bearer owner.
        connection: ConnectionId,
    },
    /// Announce loss of one exact connection epoch.
    Disconnected {
        /// Monotonic time at which the bearer observed the disconnect.
        at: PairingMillis,
        /// Exact epoch that disconnected.
        connection: ConnectionId,
    },
    /// Forward the current debounced physical-presence level.
    ObserveButton {
        /// Monotonic time of the debounced observation.
        at: PairingMillis,
        /// Connection epoch for which the observation was scheduled.
        connection: ConnectionId,
        /// Repeated debounced active-low button level.
        level: ActiveLowButton,
    },
    /// Confirm that the bearer granted exclusive ownership to one connection.
    ///
    /// The node retains the opaque acquisition capability while this scalar
    /// acknowledgement crosses the handoff.
    ExclusiveAcquired {
        /// Monotonic time at which bearer exclusivity was established.
        at: PairingMillis,
        /// Connection epoch granted exclusive ownership.
        connection: ConnectionId,
    },
    /// Deliver one decoded pre-authentication control request.
    Control {
        /// Monotonic request-admission time supplied by the bearer owner.
        at: PairingMillis,
        /// Connection epoch on which the complete record arrived.
        connection: ConnectionId,
        /// Canonically decoded status or explicit-initialization request.
        request: ControlRequest,
    },
}

impl PairingControlCommand {
    /// Connection epoch named by this command.
    pub const fn connection(&self) -> ConnectionId {
        match self {
            Self::Connected { connection, .. }
            | Self::Disconnected { connection, .. }
            | Self::ObserveButton { connection, .. }
            | Self::ExclusiveAcquired { connection, .. }
            | Self::Control { connection, .. } => *connection,
        }
    }

    /// Monotonic event time carried by this command.
    pub const fn at(&self) -> PairingMillis {
        match self {
            Self::Connected { at, .. }
            | Self::Disconnected { at, .. }
            | Self::ObserveButton { at, .. }
            | Self::ExclusiveAcquired { at, .. }
            | Self::Control { at, .. } => *at,
        }
    }
}

/// Lifecycle event acknowledged by the node owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleAcknowledgement {
    /// The connection announcement was handled.
    Connected,
    /// The disconnect announcement was handled.
    Disconnected,
}

/// Result of forwarding one repeated debounced button observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonObservationReply {
    /// No bearer-ownership transition is requested.
    Observed,
    /// The bearer must acquire exclusive ownership for this connection.
    ///
    /// The corresponding opaque policy capability remains retained by the
    /// node owner until a matching [`PairingControlCommand::ExclusiveAcquired`].
    AcquireExclusive,
}

/// Result of acknowledging bearer exclusivity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExclusiveAcquisitionReply {
    /// The connection-bound pairing window opened.
    Opened,
    /// The acquisition or open window closed before acknowledgement completed.
    Closed,
    /// The acknowledgement did not match a currently retained acquisition.
    Refused,
}

/// One node-owned outcome returned to the USB/GPIO owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingControlReplyKind {
    /// A connection lifecycle command was handled.
    Lifecycle(LifecycleAcknowledgement),
    /// A debounced button observation was handled.
    Button(ButtonObservationReply),
    /// An exclusive-acquisition acknowledgement was handled.
    Exclusive(ExclusiveAcquisitionReply),
    /// A pre-authentication control response is ready for framing.
    Control(ControlResponse),
}

/// One reply routed to an exact boot-lifetime connection epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a pairing-control reply must be delivered or explicitly discarded as stale"]
pub struct PairingControlReply {
    connection: ConnectionId,
    kind: PairingControlReplyKind,
}

impl PairingControlReply {
    /// Construct a reply routed to one exact connection epoch.
    pub const fn new(connection: ConnectionId, kind: PairingControlReplyKind) -> Self {
        Self { connection, kind }
    }

    /// Exact connection epoch to which this reply belongs.
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Borrow the reply outcome without losing its routing epoch.
    pub const fn kind(&self) -> &PairingControlReplyKind {
        &self.kind
    }

    /// Consume the routed reply into its outcome.
    pub const fn into_kind(self) -> PairingControlReplyKind {
        self.kind
    }
}

/// A full depth-one channel returned its exact unsent owner.
#[must_use = "channel pressure retains the exact command or reply for retry"]
pub struct PairingControlPressure<T> {
    owner: T,
}

impl<T> PairingControlPressure<T> {
    fn new(owner: T) -> Self {
        Self { owner }
    }

    /// Recover the unchanged command or reply that was not enqueued.
    pub fn into_inner(self) -> T {
        self.owner
    }
}

fn try_enqueue<M, T>(channel: &Channel<M, T, 1>, owner: T) -> Result<(), PairingControlPressure<T>>
where
    M: RawMutex,
{
    channel
        .try_send(owner)
        .map_err(|TrySendError::Full(owner)| PairingControlPressure::new(owner))
}

/// Unique USB/GPIO-side command producer and reply consumer.
#[must_use = "dropping the USB pairing endpoint abandons its channel capabilities"]
pub struct UsbPairingHandoff<M>
where
    M: RawMutex + 'static,
{
    commands: &'static Channel<M, PairingControlCommand, 1>,
    replies: &'static Channel<M, PairingControlReply, 1>,
}

impl<M> UsbPairingHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Try to transfer one exact command to the node owner.
    pub fn try_send_command(
        &mut self,
        command: PairingControlCommand,
    ) -> Result<(), PairingControlPressure<PairingControlCommand>> {
        try_enqueue(self.commands, command)
    }

    /// Receive one node reply immediately, if queued.
    pub fn try_receive_reply(&mut self) -> Option<PairingControlReply> {
        self.replies.try_receive().ok()
    }

    /// Fixed command-channel capacity.
    #[cfg(test)]
    pub(crate) const fn command_capacity(&self) -> usize {
        1
    }

    /// Fixed reply-channel capacity.
    #[cfg(test)]
    pub(crate) const fn reply_capacity(&self) -> usize {
        1
    }
}

/// Unique node-side command consumer and reply producer.
#[must_use = "dropping the node pairing endpoint abandons its channel capabilities"]
pub struct NodePairingHandoff<M>
where
    M: RawMutex + 'static,
{
    commands: &'static Channel<M, PairingControlCommand, 1>,
    replies: &'static Channel<M, PairingControlReply, 1>,
}

impl<M> NodePairingHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Receive one USB/GPIO command immediately, if queued.
    pub fn try_receive_command(&mut self) -> Option<PairingControlCommand> {
        self.commands.try_receive().ok()
    }

    /// Try to transfer one exact reply to the USB/GPIO owner.
    pub fn try_send_reply(
        &mut self,
        reply: PairingControlReply,
    ) -> Result<(), PairingControlPressure<PairingControlReply>> {
        try_enqueue(self.replies, reply)
    }

    /// Fixed command-channel capacity.
    #[cfg(test)]
    pub(crate) const fn command_capacity(&self) -> usize {
        1
    }

    /// Fixed reply-channel capacity.
    #[cfg(test)]
    pub(crate) const fn reply_capacity(&self) -> usize {
        1
    }
}

/// Static depth-one storage for one E290 pairing-control relationship.
pub struct PairingControlHandoff<M>
where
    M: RawMutex,
{
    commands: Channel<M, PairingControlCommand, 1>,
    replies: Channel<M, PairingControlReply, 1>,
}

impl<M> PairingControlHandoff<M>
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

    /// Split this store into its only USB/GPIO and node endpoint roles.
    pub fn split(&'static mut self) -> (UsbPairingHandoff<M>, NodePairingHandoff<M>) {
        (
            UsbPairingHandoff {
                commands: &self.commands,
                replies: &self.replies,
            },
            NodePairingHandoff {
                commands: &self.commands,
                replies: &self.replies,
            },
        )
    }
}

impl<M> Default for PairingControlHandoff<M>
where
    M: RawMutex + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(
    core::mem::size_of::<PairingControlHandoff<embassy_sync::blocking_mutex::raw::NoopRawMutex>>()
        <= PAIRING_CONTROL_HANDOFF_RAM_CEILING
);

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use reticulum_device_api_pairing_control::{
        ControlRequest, ControlResponse, InitializationStatus,
    };
    use reticulum_device_api_pairing_policy::{
        ActiveLowButton, ConnectionId, MonotonicMillis as PairingMillis,
    };

    use super::{
        ButtonObservationReply, ExclusiveAcquisitionReply, LifecycleAcknowledgement,
        PairingControlCommand, PairingControlHandoff, PairingControlReply, PairingControlReplyKind,
    };

    fn connection(value: u64) -> ConnectionId {
        ConnectionId::new(value).expect("test connection must be nonzero")
    }

    fn handoff() -> (
        super::UsbPairingHandoff<NoopRawMutex>,
        super::NodePairingHandoff<NoopRawMutex>,
    ) {
        std::boxed::Box::leak(std::boxed::Box::new(PairingControlHandoff::new())).split()
    }

    #[test]
    fn command_pressure_returns_the_exact_unsent_command() {
        let (mut usb, mut node) = handoff();
        let first = PairingControlCommand::Connected {
            at: PairingMillis::new(10),
            connection: connection(1),
        };
        let second = PairingControlCommand::ObserveButton {
            at: PairingMillis::new(11),
            connection: connection(1),
            level: ActiveLowButton::Low,
        };

        assert_eq!(usb.command_capacity(), 1);
        assert_eq!(node.command_capacity(), 1);
        assert!(usb.try_send_command(first).is_ok());
        let retained = usb
            .try_send_command(second)
            .expect_err("the second command must observe depth-one pressure")
            .into_inner();
        assert_eq!(retained, second);
        assert_eq!(node.try_receive_command(), Some(first));
        assert!(usb.try_send_command(retained).is_ok());
        assert_eq!(node.try_receive_command(), Some(second));
        assert_eq!(node.try_receive_command(), None);
    }

    #[test]
    fn reply_pressure_returns_the_exact_routed_reply() {
        let (mut usb, mut node) = handoff();
        let first = PairingControlReply::new(
            connection(4),
            PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Connected),
        );
        let response = ControlResponse::status(99, InitializationStatus::InFlight);
        let second =
            PairingControlReply::new(connection(4), PairingControlReplyKind::Control(response));

        assert_eq!(usb.reply_capacity(), 1);
        assert_eq!(node.reply_capacity(), 1);
        assert!(node.try_send_reply(first).is_ok());
        let retained = node
            .try_send_reply(second)
            .expect_err("the second reply must observe depth-one pressure")
            .into_inner();
        assert_eq!(retained, second);
        assert_eq!(usb.try_receive_reply(), Some(first));
        assert!(node.try_send_reply(retained).is_ok());
        assert_eq!(usb.try_receive_reply(), Some(second));
        assert_eq!(usb.try_receive_reply(), None);
    }

    #[test]
    fn every_command_preserves_its_event_time_and_connection() {
        let at = PairingMillis::new(0x1122_3344);
        let connection = connection(7);
        let commands = [
            PairingControlCommand::Connected { at, connection },
            PairingControlCommand::Disconnected { at, connection },
            PairingControlCommand::ObserveButton {
                at,
                connection,
                level: ActiveLowButton::High,
            },
            PairingControlCommand::ExclusiveAcquired { at, connection },
            PairingControlCommand::Control {
                at,
                connection,
                request: ControlRequest::initialize(12),
            },
        ];

        for command in commands {
            assert_eq!(command.connection(), connection);
            assert_eq!(command.at(), at);
        }
    }

    #[test]
    fn reply_shapes_are_connection_routed_and_capability_free() {
        let connection = connection(21);
        let kinds = [
            PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Connected),
            PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Disconnected),
            PairingControlReplyKind::Button(ButtonObservationReply::Observed),
            PairingControlReplyKind::Button(ButtonObservationReply::AcquireExclusive),
            PairingControlReplyKind::Exclusive(ExclusiveAcquisitionReply::Opened),
            PairingControlReplyKind::Exclusive(ExclusiveAcquisitionReply::Closed),
            PairingControlReplyKind::Exclusive(ExclusiveAcquisitionReply::Refused),
            PairingControlReplyKind::Control(ControlResponse::status(
                3,
                InitializationStatus::Completed,
            )),
        ];

        for kind in kinds {
            let reply = PairingControlReply::new(connection, kind);
            assert_eq!(reply.connection(), connection);
            assert_eq!(reply.kind(), &kind);
            assert_eq!(reply.into_kind(), kind);
        }
    }
}
