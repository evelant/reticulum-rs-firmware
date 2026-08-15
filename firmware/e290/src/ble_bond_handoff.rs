//! Depth-one owning handoff for one freshly authenticated BLE bond.
//!
//! Trouble creates the bond in the BLE bearer task, while the permanent node
//! task is the sole owner allowed to mutate flash. This handoff moves the
//! exact secret-bearing owner between those tasks without copying it. Channel
//! pressure returns the unchanged owner, and the reply carries only a
//! connection correlation plus a non-secret durable outcome.

use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, TrySendError},
};
use reticulum_ble_bond_store::BleBond;
use reticulum_device_api_pairing_policy::ConnectionId;

/// Conservative internal-static RAM ceiling for both depth-one channels.
pub const BLE_BOND_HANDOFF_RAM_CEILING: usize = 512;

/// One freshly authenticated bond routed from the BLE bearer to the node.
///
/// This aggregate deliberately implements neither `Clone`, `Copy`, nor
/// `Debug`: it owns the only portable copy of the fresh LTK and optional IRK.
#[must_use = "a fresh BLE bond must be committed or explicitly zeroizing-dropped"]
pub struct BleBondCommitCommand {
    connection: ConnectionId,
    bond: BleBond,
}

impl BleBondCommitCommand {
    /// Bind a fresh bond to the exact connection epoch that established it.
    pub const fn new(connection: ConnectionId, bond: BleBond) -> Self {
        Self { connection, bond }
    }

    /// Exact connection epoch that established this bond.
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Borrow the secret-bearing owner without separating its correlation.
    pub const fn bond(&self) -> &BleBond {
        &self.bond
    }

    /// Consume this command into its correlation and exact bond owner.
    pub fn into_parts(self) -> (ConnectionId, BleBond) {
        (self.connection, self.bond)
    }
}

/// Secret-free terminal outcome of one node-owned bond commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleBondCommitOutcome {
    /// The exact authenticated bond became authoritative on durable media.
    Durable,
    /// Durability could not be established; the connection must fail closed.
    Failed,
}

/// One terminal bond-commit outcome routed to its originating connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a bond-commit reply must be correlated or explicitly dropped as stale"]
pub struct BleBondCommitReply {
    connection: ConnectionId,
    outcome: BleBondCommitOutcome,
}

impl BleBondCommitReply {
    /// Route one terminal outcome to the exact originating connection epoch.
    pub const fn new(connection: ConnectionId, outcome: BleBondCommitOutcome) -> Self {
        Self {
            connection,
            outcome,
        }
    }

    /// Exact connection epoch to which this outcome belongs.
    pub const fn connection(self) -> ConnectionId {
        self.connection
    }

    /// Secret-free terminal durability outcome.
    pub const fn outcome(self) -> BleBondCommitOutcome {
        self.outcome
    }
}

/// A full depth-one channel returned its exact unsent owner.
#[must_use = "BLE-bond handoff pressure retains the exact owner"]
pub struct BleBondPressure<T> {
    owner: T,
}

impl<T> BleBondPressure<T> {
    fn new(owner: T) -> Self {
        Self { owner }
    }

    /// Recover the unchanged command or reply that was not enqueued.
    pub fn into_inner(self) -> T {
        self.owner
    }
}

fn try_enqueue<M, T>(channel: &Channel<M, T, 1>, owner: T) -> Result<(), BleBondPressure<T>>
where
    M: RawMutex,
{
    channel
        .try_send(owner)
        .map_err(|TrySendError::Full(owner)| BleBondPressure::new(owner))
}

/// Unique BLE-bearer request producer and outcome consumer.
#[must_use = "dropping the bearer endpoint abandons fresh-bond durability"]
pub struct BearerBleBondHandoff<M>
where
    M: RawMutex + 'static,
{
    commands: &'static Channel<M, BleBondCommitCommand, 1>,
    replies: &'static Channel<M, BleBondCommitReply, 1>,
}

impl<M> BearerBleBondHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Try to transfer one exact fresh bond to the node owner.
    pub fn try_send_command(
        &mut self,
        command: BleBondCommitCommand,
    ) -> Result<(), BleBondPressure<BleBondCommitCommand>> {
        try_enqueue(self.commands, command)
    }

    /// Receive one terminal node outcome immediately, if queued.
    pub fn try_receive_reply(&mut self) -> Option<BleBondCommitReply> {
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

/// Unique node-side fresh-bond consumer and terminal-outcome producer.
#[must_use = "dropping the node endpoint abandons fresh-bond durability"]
pub struct NodeBleBondHandoff<M>
where
    M: RawMutex + 'static,
{
    commands: &'static Channel<M, BleBondCommitCommand, 1>,
    replies: &'static Channel<M, BleBondCommitReply, 1>,
}

impl<M> NodeBleBondHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Receive one fresh bond immediately, if queued.
    pub fn try_receive_command(&mut self) -> Option<BleBondCommitCommand> {
        self.commands.try_receive().ok()
    }

    /// Try to transfer one secret-free terminal outcome to the BLE owner.
    pub fn try_send_reply(
        &mut self,
        reply: BleBondCommitReply,
    ) -> Result<(), BleBondPressure<BleBondCommitReply>> {
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

/// Static depth-one storage for the BLE-bearer/node bond relationship.
pub struct BleBondHandoff<M>
where
    M: RawMutex,
{
    commands: Channel<M, BleBondCommitCommand, 1>,
    replies: Channel<M, BleBondCommitReply, 1>,
}

impl<M> BleBondHandoff<M>
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
    pub fn split(&'static mut self) -> (BearerBleBondHandoff<M>, NodeBleBondHandoff<M>) {
        (
            BearerBleBondHandoff {
                commands: &self.commands,
                replies: &self.replies,
            },
            NodeBleBondHandoff {
                commands: &self.commands,
                replies: &self.replies,
            },
        )
    }
}

impl<M> Default for BleBondHandoff<M>
where
    M: RawMutex + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(
    core::mem::size_of::<BleBondHandoff<embassy_sync::blocking_mutex::raw::NoopRawMutex>>()
        <= BLE_BOND_HANDOFF_RAM_CEILING
);

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use reticulum_ble_bond_store::{BleAddressKind, BleBond};
    use reticulum_device_api_pairing_policy::ConnectionId;

    use super::{
        BLE_BOND_HANDOFF_RAM_CEILING, BleBondCommitCommand, BleBondCommitOutcome,
        BleBondCommitReply, BleBondHandoff,
    };

    fn connection(value: u64) -> ConnectionId {
        ConnectionId::new(value).expect("test connection must be nonzero")
    }

    fn bond(tag: u8) -> BleBond {
        BleBond::new(
            BleAddressKind::Random,
            [tag; 6],
            Some([tag.wrapping_add(1); 16]),
            [tag.wrapping_add(2); 16],
        )
    }

    fn handoff() -> (
        super::BearerBleBondHandoff<NoopRawMutex>,
        super::NodeBleBondHandoff<NoopRawMutex>,
    ) {
        std::boxed::Box::leak(std::boxed::Box::new(BleBondHandoff::new())).split()
    }

    #[test]
    fn command_pressure_returns_the_exact_secret_owner() {
        let (mut bearer, mut node) = handoff();
        assert!(
            bearer
                .try_send_command(BleBondCommitCommand::new(connection(1), bond(7)))
                .is_ok(),
            "first command fits"
        );

        let pressure = bearer
            .try_send_command(BleBondCommitCommand::new(connection(2), bond(11)))
            .expect_err("depth-one pressure returns the second owner");
        let retained = pressure.into_inner();
        assert_eq!(retained.connection(), connection(2));
        assert_eq!(*retained.bond().address(), [11; 6]);
        assert_eq!(retained.bond().irk(), Some(&[12; 16]));
        assert_eq!(*retained.bond().ltk(), [13; 16]);

        let first = node
            .try_receive_command()
            .expect("the queued owner remains unchanged");
        assert_eq!(first.connection(), connection(1));
        assert_eq!(*first.bond().address(), [7; 6]);
    }

    #[test]
    fn reply_pressure_and_connection_correlation_are_exact() {
        let (mut bearer, mut node) = handoff();
        assert!(
            node.try_send_reply(BleBondCommitReply::new(
                connection(3),
                BleBondCommitOutcome::Durable,
            ))
            .is_ok(),
            "first reply fits"
        );

        let retained = node
            .try_send_reply(BleBondCommitReply::new(
                connection(4),
                BleBondCommitOutcome::Failed,
            ))
            .expect_err("depth-one pressure returns the exact reply")
            .into_inner();
        assert_eq!(retained.connection(), connection(4));
        assert_eq!(retained.outcome(), BleBondCommitOutcome::Failed);

        let queued = bearer
            .try_receive_reply()
            .expect("the first correlated reply remains queued");
        assert_eq!(queued.connection(), connection(3));
        assert_eq!(queued.outcome(), BleBondCommitOutcome::Durable);
    }

    #[test]
    fn endpoints_are_depth_one_and_static_storage_stays_bounded() {
        let (bearer, node) = handoff();
        assert_eq!(bearer.command_capacity(), 1);
        assert_eq!(bearer.reply_capacity(), 1);
        assert_eq!(node.command_capacity(), 1);
        assert_eq!(node.reply_capacity(), 1);
        assert!(
            core::mem::size_of::<BleBondHandoff<NoopRawMutex>>() <= BLE_BOND_HANDOFF_RAM_CEILING
        );
    }
}
