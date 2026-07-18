//! Depth-one owning handoff for E290 authenticated-session admission.
//!
//! A device-API bearer supplies only non-secret routing facts and an opaque
//! credential identifier. The sole node credential owner either returns one
//! zeroizing [`SelectedCredential`] for that exact connection epoch or refuses
//! without disclosing whether the identifier was missing, pending, or revoked.
//! Channel pressure always returns the exact unsent owner for retry or
//! zeroizing drop.
//!
//! The current product instantiates exactly one of these handoffs for its sole
//! USB bearer, so [`ConnectionId`] is unambiguous. A later concurrent bearer
//! must not clone this singleton topology with an independent connection/epoch
//! namespace and a shared reply lane. It must first choose globally unique,
//! bearer-qualified connection and session epochs, or use strictly disjoint
//! per-bearer reply channels under one global pairing-exclusivity coordinator.

use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, TrySendError},
};
use reticulum_device_api_credentials::{CredentialId, SelectedCredential};
use reticulum_device_api_pairing_policy::{ConnectionId, MonotonicMillis as PairingMillis};

/// Conservative internal-static RAM ceiling for both depth-one owning channels.
pub const SESSION_ADMISSION_HANDOFF_RAM_CEILING: usize = 1_024;

/// One credential-selection request routed from a bearer to the node owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a session-admission command must be handled or explicitly dropped"]
pub struct SessionAdmissionCommand {
    at: PairingMillis,
    connection: ConnectionId,
    credential_id: CredentialId,
}

impl SessionAdmissionCommand {
    /// Bind one opaque credential identifier to its admission time and connection epoch.
    pub const fn new(
        at: PairingMillis,
        connection: ConnectionId,
        credential_id: CredentialId,
    ) -> Self {
        Self {
            at,
            connection,
            credential_id,
        }
    }

    /// Monotonic request-admission time supplied by the bearer owner.
    pub const fn at(&self) -> PairingMillis {
        self.at
    }

    /// Exact connection epoch on which the client hello arrived.
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Opaque credential identifier asserted by the client hello.
    pub const fn credential_id(&self) -> CredentialId {
        self.credential_id
    }
}

/// Node-owned result of one credential-selection request.
///
/// The admitted variant owns the only returned PSK selection. This type
/// deliberately implements neither `Clone`, `Copy`, nor `Debug`.
///
/// ```compile_fail
/// use reticulum_heltec_vision_master_e290_node::session_admission_handoff::SessionAdmissionOutcome;
/// fn require_clone<T: Clone>() {}
/// require_clone::<SessionAdmissionOutcome>();
/// ```
///
/// ```compile_fail
/// use reticulum_heltec_vision_master_e290_node::session_admission_handoff::SessionAdmissionOutcome;
/// fn require_copy<T: Copy>() {}
/// require_copy::<SessionAdmissionOutcome>();
/// ```
///
/// ```compile_fail
/// use reticulum_heltec_vision_master_e290_node::session_admission_handoff::SessionAdmissionOutcome;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<SessionAdmissionOutcome>();
/// ```
#[must_use = "a session-admission outcome owns selected authentication material or a refusal"]
pub enum SessionAdmissionOutcome {
    /// One active exact credential was selected for this handshake attempt.
    Selected(SelectedCredential),
    /// Admission was refused without revealing credential or policy state.
    Refused,
}

impl SessionAdmissionOutcome {
    /// Consume this outcome into selected authentication material, if admitted.
    pub fn into_selected(self) -> Option<SelectedCredential> {
        match self {
            Self::Selected(selected) => Some(selected),
            Self::Refused => None,
        }
    }

    /// Whether this outcome is the reason-free refusal.
    pub const fn is_refused(&self) -> bool {
        matches!(self, Self::Refused)
    }
}

/// One session-admission outcome routed back to its exact connection epoch.
///
/// This aggregate deliberately implements neither `Clone`, `Copy`, nor
/// `Debug` because its outcome may own the only selected credential material.
#[must_use = "a session-admission reply must be delivered or explicitly dropped as stale"]
pub struct SessionAdmissionReply {
    connection: ConnectionId,
    outcome: SessionAdmissionOutcome,
}

impl SessionAdmissionReply {
    /// Route one admission outcome to the connection that owns its client hello.
    pub const fn new(connection: ConnectionId, outcome: SessionAdmissionOutcome) -> Self {
        Self {
            connection,
            outcome,
        }
    }

    /// Exact connection epoch to which this outcome belongs.
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Borrow the outcome without separating it from routing provenance.
    pub const fn outcome(&self) -> &SessionAdmissionOutcome {
        &self.outcome
    }

    /// Consume this routed reply into its outcome.
    pub fn into_outcome(self) -> SessionAdmissionOutcome {
        self.outcome
    }
}

/// A full depth-one channel returned its exact unsent owner.
#[must_use = "session-admission channel pressure retains the exact secret-bearing owner"]
pub struct SessionAdmissionPressure<T> {
    owner: T,
}

impl<T> SessionAdmissionPressure<T> {
    fn new(owner: T) -> Self {
        Self { owner }
    }

    /// Recover the unchanged command or reply that was not enqueued.
    pub fn into_inner(self) -> T {
        self.owner
    }
}

fn try_enqueue<M, T>(
    channel: &Channel<M, T, 1>,
    owner: T,
) -> Result<(), SessionAdmissionPressure<T>>
where
    M: RawMutex,
{
    channel
        .try_send(owner)
        .map_err(|TrySendError::Full(owner)| SessionAdmissionPressure::new(owner))
}

/// Unique bearer-side command producer and outcome consumer.
///
/// The bearer may be USB, BLE, Wi-Fi, or another future device-API transport;
/// credential ownership and selection policy remain on the node side.
#[must_use = "dropping the bearer endpoint abandons its session-admission capabilities"]
pub struct BearerSessionAdmissionHandoff<M>
where
    M: RawMutex + 'static,
{
    commands: &'static Channel<M, SessionAdmissionCommand, 1>,
    replies: &'static Channel<M, SessionAdmissionReply, 1>,
}

impl<M> BearerSessionAdmissionHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Try to transfer one exact selection command to the node owner.
    pub fn try_send_command(
        &mut self,
        command: SessionAdmissionCommand,
    ) -> Result<(), SessionAdmissionPressure<SessionAdmissionCommand>> {
        try_enqueue(self.commands, command)
    }

    /// Receive one node outcome immediately, if queued.
    pub fn try_receive_reply(&mut self) -> Option<SessionAdmissionReply> {
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

/// Unique node-side command consumer and outcome producer.
#[must_use = "dropping the node endpoint abandons its session-admission capabilities"]
pub struct NodeSessionAdmissionHandoff<M>
where
    M: RawMutex + 'static,
{
    commands: &'static Channel<M, SessionAdmissionCommand, 1>,
    replies: &'static Channel<M, SessionAdmissionReply, 1>,
}

impl<M> NodeSessionAdmissionHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Receive one bearer selection command immediately, if queued.
    pub fn try_receive_command(&mut self) -> Option<SessionAdmissionCommand> {
        self.commands.try_receive().ok()
    }

    /// Try to transfer one exact outcome to the bearer owner.
    pub fn try_send_reply(
        &mut self,
        reply: SessionAdmissionReply,
    ) -> Result<(), SessionAdmissionPressure<SessionAdmissionReply>> {
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

/// Static depth-one storage for one session-admission bearer/node relationship.
pub struct SessionAdmissionHandoff<M>
where
    M: RawMutex,
{
    commands: Channel<M, SessionAdmissionCommand, 1>,
    replies: Channel<M, SessionAdmissionReply, 1>,
}

impl<M> SessionAdmissionHandoff<M>
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
    pub fn split(
        &'static mut self,
    ) -> (
        BearerSessionAdmissionHandoff<M>,
        NodeSessionAdmissionHandoff<M>,
    ) {
        (
            BearerSessionAdmissionHandoff {
                commands: &self.commands,
                replies: &self.replies,
            },
            NodeSessionAdmissionHandoff {
                commands: &self.commands,
                replies: &self.replies,
            },
        )
    }
}

impl<M> Default for SessionAdmissionHandoff<M>
where
    M: RawMutex + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(
    core::mem::size_of::<SessionAdmissionHandoff<embassy_sync::blocking_mutex::raw::NoopRawMutex>>(
    ) <= SESSION_ADMISSION_HANDOFF_RAM_CEILING
);

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use reticulum_device_api::{Permissions, PrincipalId};
    use reticulum_device_api_credentials::{
        AuthorityRevision, AuthorizationPolicyVersion, CredentialAudit, CredentialAuthorityBuilder,
        CredentialGeneration, CredentialId, CredentialRecord, CredentialStatus, PairingOrigin,
        SelectedCredential,
    };
    use reticulum_device_api_pairing_policy::{ConnectionId, MonotonicMillis as PairingMillis};

    use super::{
        SESSION_ADMISSION_HANDOFF_RAM_CEILING, SessionAdmissionCommand, SessionAdmissionHandoff,
        SessionAdmissionOutcome, SessionAdmissionReply,
    };

    const PSK: [u8; 32] = [0x5a; 32];

    fn connection(value: u64) -> ConnectionId {
        ConnectionId::new(value).expect("test connection must be nonzero")
    }

    fn credential_id(value: u8) -> CredentialId {
        CredentialId::new([value; 16])
    }

    fn selected_credential(
        id: CredentialId,
        generation: CredentialGeneration,
    ) -> SelectedCredential {
        let revision = AuthorityRevision::new(generation.get());
        let authority = CredentialAuthorityBuilder::<1>::new(revision)
            .unwrap_or_else(|fault| panic!("authority revision rejected: {:?}", fault.kind()))
            .insert(CredentialRecord::with_secret(
                id,
                generation,
                PrincipalId([0x31; 16]),
                Permissions::NONE,
                CredentialStatus::Active,
                CredentialAudit::new(
                    revision,
                    revision,
                    PairingOrigin::UsbPhysicalPresence,
                    AuthorizationPolicyVersion::new(1),
                ),
                PSK,
            ))
            .unwrap_or_else(|fault| panic!("active credential rejected: {:?}", fault.kind()))
            .finish();
        authority
            .select_for_handshake(id)
            .unwrap_or_else(|_| panic!("active credential was unavailable"))
    }

    fn handoff() -> (
        super::BearerSessionAdmissionHandoff<NoopRawMutex>,
        super::NodeSessionAdmissionHandoff<NoopRawMutex>,
    ) {
        std::boxed::Box::leak(std::boxed::Box::new(SessionAdmissionHandoff::new())).split()
    }

    #[test]
    fn command_fifo_and_pressure_return_the_exact_unsent_owner() {
        let (mut bearer, mut node) = handoff();
        let first = SessionAdmissionCommand::new(
            PairingMillis::new(10),
            connection(1),
            credential_id(0x11),
        );
        let second = SessionAdmissionCommand::new(
            PairingMillis::new(11),
            connection(2),
            credential_id(0x22),
        );

        assert_eq!(bearer.command_capacity(), 1);
        assert_eq!(node.command_capacity(), 1);
        assert!(bearer.try_send_command(first).is_ok(), "first command fits");
        let retained = bearer
            .try_send_command(second)
            .expect_err("second command observes depth-one pressure")
            .into_inner();
        assert_eq!(retained, second);

        assert_eq!(
            node.try_receive_command().expect("first command queued"),
            first
        );
        assert!(
            bearer.try_send_command(retained).is_ok(),
            "retained command fits after receive"
        );
        assert_eq!(
            node.try_receive_command().expect("retained command queued"),
            second
        );
        assert!(node.try_receive_command().is_none());
    }

    #[test]
    fn selected_reply_pressure_preserves_the_only_credential_owner() {
        let (mut bearer, mut node) = handoff();
        let selected_id = credential_id(0x33);
        let selected_generation = CredentialGeneration::new(7);
        let first = SessionAdmissionReply::new(connection(3), SessionAdmissionOutcome::Refused);
        let second = SessionAdmissionReply::new(
            connection(4),
            SessionAdmissionOutcome::Selected(selected_credential(
                selected_id,
                selected_generation,
            )),
        );

        assert_eq!(bearer.reply_capacity(), 1);
        assert_eq!(node.reply_capacity(), 1);
        assert!(node.try_send_reply(first).is_ok(), "first reply fits");
        let retained = node
            .try_send_reply(second)
            .expect_err("selected credential observes depth-one pressure")
            .into_inner();
        assert_eq!(retained.connection(), connection(4));
        assert!(!retained.outcome().is_refused());

        let refused = bearer.try_receive_reply().expect("refusal queued");
        assert_eq!(refused.connection(), connection(3));
        assert!(refused.into_outcome().is_refused());
        assert!(
            node.try_send_reply(retained).is_ok(),
            "selected credential fits after receive"
        );

        let selected = bearer.try_receive_reply().expect("selected reply queued");
        assert_eq!(selected.connection(), connection(4));
        let selected = selected
            .into_outcome()
            .into_selected()
            .expect("selected outcome kept its credential owner");
        let (id, generation, psk) = selected.into_parts();
        assert_eq!(id, selected_id);
        assert_eq!(generation, selected_generation);
        assert_eq!(psk.as_ref(), &PSK);
        assert!(bearer.try_receive_reply().is_none());
    }

    #[test]
    fn refusal_is_reason_free_and_round_trips_without_secret_material() {
        let (mut bearer, mut node) = handoff();
        assert!(
            node.try_send_reply(SessionAdmissionReply::new(
                connection(9),
                SessionAdmissionOutcome::Refused,
            ))
            .is_ok()
        );

        let reply = bearer.try_receive_reply().expect("refusal queued");
        assert_eq!(reply.connection(), connection(9));
        let outcome = reply.into_outcome();
        assert!(outcome.is_refused());
        assert!(outcome.into_selected().is_none());
    }

    #[test]
    fn owning_handoff_stays_within_its_static_ram_ceiling() {
        assert!(
            core::mem::size_of::<SessionAdmissionHandoff<NoopRawMutex>>()
                <= SESSION_ADMISSION_HANDOFF_RAM_CEILING
        );
    }
}
