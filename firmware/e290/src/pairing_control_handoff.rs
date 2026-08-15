//! Depth-one command/reply handoff for the E290 pre-authentication pairing lane.
//!
//! The bearer/GPIO task owns connection detection, button debounce, framing, and
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

/// One event or request transferred from the bearer/GPIO owner to the node owner.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ButtonObservationFlightState {
    Idle,
    Ready {
        at: PairingMillis,
        connection: ConnectionId,
        level: ActiveLowButton,
    },
    Awaiting {
        at: PairingMillis,
        connection: ConnectionId,
    },
}

/// Immediate progress from one nonblocking physical-presence publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonObservationFlightProgress {
    /// No observation is currently owned.
    Idle,
    /// The exact command or its eventual reply remains owned by this flight.
    Pending,
    /// The node accepted the observation without changing bearer ownership.
    Observed,
    /// The node requested connection-bound pairing exclusivity.
    AcquireExclusive,
}

/// A routed reply did not match the one outstanding button observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ButtonObservationReplyMismatch;

/// One nonblocking button command/reply owner.
///
/// GPIO sampling must continue while the node actor is busy. This owner keeps
/// the exact unsent command or expected reply without awaiting either channel,
/// allowing the bearer task to return to its millisecond sampling loop.
#[must_use = "an in-flight button observation must be polled until it resolves"]
pub struct ButtonObservationFlight {
    state: ButtonObservationFlightState,
}

impl ButtonObservationFlight {
    /// Construct an idle observation owner.
    pub const fn new() -> Self {
        Self {
            state: ButtonObservationFlightState::Idle,
        }
    }

    /// Whether a command or its eventual reply remains owned.
    pub const fn is_pending(&self) -> bool {
        !matches!(self.state, ButtonObservationFlightState::Idle)
    }

    /// Sampling time of the outstanding observation, if any.
    pub const fn started_at(&self) -> Option<PairingMillis> {
        match self.state {
            ButtonObservationFlightState::Idle => None,
            ButtonObservationFlightState::Ready { at, .. }
            | ButtonObservationFlightState::Awaiting { at, .. } => Some(at),
        }
    }

    fn try_schedule(
        &mut self,
        at: PairingMillis,
        connection: ConnectionId,
        level: ActiveLowButton,
    ) -> bool {
        if self.is_pending() {
            return false;
        }
        self.state = ButtonObservationFlightState::Ready {
            at,
            connection,
            level,
        };
        true
    }

    /// Retain and immediately enqueue one observation before the bearer can
    /// yield to another clock-owning task.
    ///
    /// The node pairing policy also polls timeouts from its own task. Merely
    /// retaining an observation and waiting for a later bearer turn can let
    /// that policy observe a newer timestamp first, making the otherwise
    /// valid button observation look like a clock regression. This combined
    /// operation makes the first command-transfer attempt in the same
    /// non-yielding turn as timestamp capture while preserving the ordinary
    /// retained-pressure behavior when the depth-one channel is occupied.
    ///
    /// `Ok(None)` means another observation was already owned. Otherwise the
    /// returned progress is the result of the immediate transfer attempt.
    pub fn try_schedule_and_poll<M>(
        &mut self,
        handoff: &mut BearerPairingHandoff<M>,
        at: PairingMillis,
        connection: ConnectionId,
        level: ActiveLowButton,
    ) -> Result<Option<ButtonObservationFlightProgress>, ButtonObservationReplyMismatch>
    where
        M: RawMutex + 'static,
    {
        if !self.try_schedule(at, connection, level) {
            return Ok(None);
        }
        self.poll(handoff).map(Some)
    }

    /// Make one immediate command/reply transfer attempt without awaiting.
    ///
    /// Replies for retired connection epochs are discarded as stale. An exact
    /// connection with a non-button reply is a protocol mismatch.
    pub fn poll<M>(
        &mut self,
        handoff: &mut BearerPairingHandoff<M>,
    ) -> Result<ButtonObservationFlightProgress, ButtonObservationReplyMismatch>
    where
        M: RawMutex + 'static,
    {
        if let ButtonObservationFlightState::Ready {
            at,
            connection,
            level,
        } = self.state
        {
            let command = PairingControlCommand::ObserveButton {
                at,
                connection,
                level,
            };
            if let Err(pressure) = handoff.try_send_command(command) {
                let retained = pressure.into_inner();
                debug_assert_eq!(retained, command);
                return Ok(ButtonObservationFlightProgress::Pending);
            }
            self.state = ButtonObservationFlightState::Awaiting { at, connection };
        }

        let ButtonObservationFlightState::Awaiting { connection, .. } = self.state else {
            return Ok(ButtonObservationFlightProgress::Idle);
        };
        while let Some(reply) = handoff.try_receive_reply() {
            if reply.connection() != connection {
                continue;
            }
            self.state = ButtonObservationFlightState::Idle;
            return match reply.into_kind() {
                PairingControlReplyKind::Button(ButtonObservationReply::Observed) => {
                    Ok(ButtonObservationFlightProgress::Observed)
                }
                PairingControlReplyKind::Button(ButtonObservationReply::AcquireExclusive) => {
                    Ok(ButtonObservationFlightProgress::AcquireExclusive)
                }
                _ => Err(ButtonObservationReplyMismatch),
            };
        }
        Ok(ButtonObservationFlightProgress::Pending)
    }
}

impl Default for ButtonObservationFlight {
    fn default() -> Self {
        Self::new()
    }
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

/// One node-owned outcome returned to the bearer/GPIO owner.
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

/// Unique bearer/GPIO-side command producer and reply consumer.
#[must_use = "dropping the bearer pairing endpoint abandons its channel capabilities"]
pub struct BearerPairingHandoff<M>
where
    M: RawMutex + 'static,
{
    commands: &'static Channel<M, PairingControlCommand, 1>,
    replies: &'static Channel<M, PairingControlReply, 1>,
}

impl<M> BearerPairingHandoff<M>
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
    /// Receive one bearer/GPIO command immediately, if queued.
    pub fn try_receive_command(&mut self) -> Option<PairingControlCommand> {
        self.commands.try_receive().ok()
    }

    /// Try to transfer one exact reply to the bearer/GPIO owner.
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

    /// Split this store into its only bearer/GPIO and node endpoint roles.
    pub fn split(&'static mut self) -> (BearerPairingHandoff<M>, NodePairingHandoff<M>) {
        (
            BearerPairingHandoff {
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
#[path = "pairing_control_handoff_tests.rs"]
mod tests;
