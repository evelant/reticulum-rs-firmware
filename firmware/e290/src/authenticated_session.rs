//! Owner-preserving authenticated-session machine shared by E290 API bearers.
//!
//! This module composes the portable session typestates with the E290's
//! credential-selection and logical-request handoffs. It deliberately supports
//! one handshake attempt per accepted connection at a time; one authenticated request
//! may be in flight. A canonical client hello may replace an idle established
//! session without replacing the bearer connection epoch. Replacement never
//! displaces request handoff, an awaiting node reply, reply transmission, or
//! pairing exclusivity. A refusal, malformed handshake, framing fault,
//! established-session authentication/order fault, or malformed logical request
//! terminates ordinary session handling until an explicit reset.
//!
//! The sole [`SessionEpochAllocator`] is retained across reset, while every
//! connection-scoped owner (selected credential, key schedule, session keys,
//! partial transmit flight, and awaiting-reply route) is dropped. Pairing
//! exclusivity uses the same fail-closed boundary. If part of a record has
//! already reached the bearer backend, the caller is told to drain that record
//! before retrying the close.
//!
//! Attempt counters, timeout/rate policy, session resumption, authenticated
//! close records, logical reply decoding, encryption, and concurrent requests
//! are intentionally future bearer policy rather than hidden behavior here.
//! A second simultaneous bearer also needs a product-wide correlation design:
//! either bearer-qualified globally unique connection/session epochs or
//! disjoint reply channels under one global pairing-exclusivity coordinator.
//! Independent allocators must not feed colliding epochs into one shared lane.

use core::mem;

use embassy_sync::blocking_mutex::raw::RawMutex;
use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::decode_request;
use reticulum_device_api_framing::{DecodeEvent, Record, TxAdvanceError};
use reticulum_device_api_handoff::{
    LocalApiReply, LocalApiRequest, RequestKey, RequestSender, SessionEpoch,
};
use reticulum_device_api_session::{
    ActiveCredential, AuthenticatedGrant, AwaitingReply, ClientHello, PendingClientProof,
    RECORD_KIND_CLIENT_HELLO, ReplyFlight, ReplyRouteFaultKind, ServerHelloFlight,
    ServerParameters, ServerProofFlight, ServerSession, SessionEpochAllocator, SessionFault,
};

use crate::session_admission_handoff::{
    BearerSessionAdmissionHandoff, SessionAdmissionCommand, SessionAdmissionOutcome,
    SessionAdmissionReply,
};
use reticulum_device_api_pairing_policy::{ConnectionId, MonotonicMillis as PairingMillis};

/// Conservative fixed-RAM ceiling for the complete authenticated session owner.
pub const AUTHENTICATED_SESSION_RAM_CEILING: usize = 2_048;

/// Externally observable phase of the authenticated-session machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticatedSessionPhase {
    /// No accepted bearer connection is owned.
    Disconnected,
    /// The one client hello allowed on this connection has not arrived.
    AwaitingClientHello,
    /// The exact credential-selection command is retained under handoff pressure.
    AdmissionCommandPending,
    /// The client hello is retained while the node selects a credential.
    AwaitingAdmissionReply,
    /// The server-hello record is being acknowledged by the bearer backend.
    ServerHelloFlight,
    /// The server-proof record is being acknowledged by the bearer backend.
    ServerProofFlight,
    /// The server is waiting for the exact client-proof record.
    PendingClientProof,
    /// An authenticated session is idle and may accept one request.
    Established,
    /// An authenticated request is retained under node-handoff pressure.
    RequestHandoffPending,
    /// The request was transferred and its exact node reply is awaited.
    AwaitingReply,
    /// The authenticated response record is being acknowledged by the bearer.
    ReplyFlight,
    /// Ordinary session state was dropped before pairing-exclusive acknowledgement.
    PairingExclusive,
    /// A fatal fault closed ordinary handling until explicit reset.
    TerminatedUntilReset,
}

/// Coarse framing fault supplied by the bearer stream decoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionFramingFault {
    /// A delimited COBS body was malformed.
    MalformedCobs,
    /// A decoded body was not one canonical framing record.
    MalformedRecord,
    /// An overlong body was discarded through its delimiter.
    Overflow,
}

/// Terminal reason retained until the bearer explicitly resets the machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticatedSessionFault {
    /// The connection's sole credential-selection attempt was refused.
    AdmissionRefused,
    /// A handshake record, proof, bearer binding, entropy source, or epoch failed.
    Handshake,
    /// A framing-layer failure invalidated the connection stream.
    Framing(SessionFramingFault),
    /// A record arrived in a phase that cannot own it.
    UnexpectedRecord {
        /// Phase that observed the record.
        phase: AuthenticatedSessionPhase,
        /// Wire record kind that arrived.
        observed_kind: u8,
    },
    /// An established request failed session authentication or ordering.
    Established(SessionFault),
    /// An authenticated payload was not one canonical logical API request.
    MalformedLogicalRequest,
    /// A matched-epoch node reply did not belong to the sole pending request.
    UnexpectedNodeReply,
    /// The portable reply framer rejected an otherwise matched reply.
    ReplyRoute(ReplyRouteFaultKind),
    /// A credential-selection reply arrived for the live connection in the wrong phase.
    UnexpectedAdmissionReply,
}

/// Kind of exact outbound record currently owned by the machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTxKind {
    /// Server hello.
    ServerHello,
    /// Server proof.
    ServerProof,
    /// Authenticated logical response.
    Reply,
}

/// Result of acknowledging bytes from the current exact transmit owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTxAdvance {
    /// The same record retains bytes that have not yet been acknowledged.
    Partial,
    /// The record completed and the state machine advanced to its next phase.
    RecordComplete {
        /// Exact record kind that completed.
        kind: SessionTxKind,
        /// Phase entered after consuming that flight.
        next: AuthenticatedSessionPhase,
    },
}

/// A transmit acknowledgement could not be applied without losing its owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTxError {
    /// No outbound record exists in the current phase.
    NotTransmitting,
    /// The backend acknowledged more bytes than the record retained.
    Advance(TxAdvanceError),
}

/// Observation produced by one decoded inbound record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionRxDisposition {
    /// No complete framing record was present.
    Pending,
    /// A canonical initial or idle-session replacement client hello was retained
    /// for node-side selection.
    ClientHelloAccepted,
    /// A replacement client hello was dropped while an in-flight owner or
    /// pairing exclusivity remained unchanged.
    ClientHelloDroppedBusy,
    /// The client proof established one authenticated session.
    SessionEstablished,
    /// One authenticated, canonical logical request awaits node handoff.
    RequestAuthenticated,
}

/// Result of trying to transfer the exact selection command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionCommandDisposition {
    /// The command moved into the depth-one node channel.
    Transferred,
    /// Channel pressure returned and retained the unchanged command.
    RetainedUnderPressure,
    /// The machine does not currently own a selection command.
    NotPending,
}

/// Result of consuming one node credential-selection reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionReplyDisposition {
    /// The matching selected credential started a server-hello flight.
    ServerHelloStarted,
    /// A reply for another or reset connection was zeroizingly drained.
    DrainedStale,
}

/// Result of trying to transfer the exact authenticated request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestHandoffDisposition {
    /// The request moved into the depth-one node channel.
    Transferred,
    /// Channel pressure returned and retained the complete request and grant.
    RetainedUnderPressure,
    /// The machine does not currently own a request awaiting handoff.
    NotPending,
}

/// Reason a node reply was intentionally classified as stale and drained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StaleNodeReplyReason {
    /// No authenticated session can still deliver any node reply.
    NoLiveSession,
    /// The reply belongs to an older or foreign boot-local session epoch.
    WrongEpoch,
}

/// Successful classification of one consumed node reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeReplyDisposition {
    /// The exact reply was sealed into an authenticated response flight.
    ReplyFlightStarted,
    /// A stale reply was deliberately consumed instead of entering a live session.
    DrainedStale {
        /// Routing key of the drained owner.
        key: RequestKey,
        /// Why the reply cannot belong to the current session.
        reason: StaleNodeReplyReason,
    },
}

/// Fatal matched-epoch reply failure retaining the complete undelivered owner.
#[must_use = "the exact node reply must be explicitly drained or quarantined"]
pub struct NodeReplyFault {
    kind: AuthenticatedSessionFault,
    reply: LocalApiReply,
}

impl NodeReplyFault {
    /// Terminal session fault associated with this undelivered reply.
    pub const fn kind(&self) -> AuthenticatedSessionFault {
        self.kind
    }

    /// Recover the exact routing key and fixed-capacity response owner.
    pub fn into_reply(self) -> LocalApiReply {
        self.reply
    }
}

/// Result of requesting pairing exclusivity from the ordinary-session owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingExclusiveCloseDisposition {
    /// Ordinary session state was dropped; pairing may now be acknowledged exclusive.
    Closed,
    /// Part of the current record was already emitted; drain it and retry closure.
    DrainBeforeClose {
        /// Exact record whose remaining bytes must be acknowledged first.
        kind: SessionTxKind,
    },
    /// The machine had already closed ordinary handling for pairing.
    AlreadyExclusive,
    /// The request belongs to a stale connection and changed no state.
    StaleConnection,
}

struct TxOwner<T> {
    flight: T,
    started: bool,
}

impl<T> TxOwner<T> {
    const fn new(flight: T) -> Self {
        Self {
            flight,
            started: false,
        }
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "allocation-free typestate must retain each exact fixed-capacity flight owner"
)]
enum State {
    Disconnected,
    AwaitingClientHello,
    AdmissionCommandPending {
        hello: ClientHello,
        command: SessionAdmissionCommand,
    },
    AwaitingAdmissionReply {
        hello: ClientHello,
    },
    ServerHelloFlight(TxOwner<ServerHelloFlight>),
    ServerProofFlight(TxOwner<ServerProofFlight>),
    PendingClientProof(PendingClientProof),
    Established(ServerSession),
    RequestHandoffPending {
        request: LocalApiRequest<AuthenticatedGrant>,
        waiting: AwaitingReply,
    },
    AwaitingReply(AwaitingReply),
    ReplyFlight(TxOwner<ReplyFlight>),
    PairingExclusive,
    TerminatedUntilReset(AuthenticatedSessionFault),
    Transitioning,
}

impl State {
    fn phase(&self) -> AuthenticatedSessionPhase {
        match self {
            Self::Disconnected => AuthenticatedSessionPhase::Disconnected,
            Self::AwaitingClientHello => AuthenticatedSessionPhase::AwaitingClientHello,
            Self::AdmissionCommandPending { .. } => {
                AuthenticatedSessionPhase::AdmissionCommandPending
            }
            Self::AwaitingAdmissionReply { .. } => {
                AuthenticatedSessionPhase::AwaitingAdmissionReply
            }
            Self::ServerHelloFlight(_) => AuthenticatedSessionPhase::ServerHelloFlight,
            Self::ServerProofFlight(_) => AuthenticatedSessionPhase::ServerProofFlight,
            Self::PendingClientProof(_) => AuthenticatedSessionPhase::PendingClientProof,
            Self::Established(_) => AuthenticatedSessionPhase::Established,
            Self::RequestHandoffPending { .. } => AuthenticatedSessionPhase::RequestHandoffPending,
            Self::AwaitingReply(_) => AuthenticatedSessionPhase::AwaitingReply,
            Self::ReplyFlight(_) => AuthenticatedSessionPhase::ReplyFlight,
            Self::PairingExclusive => AuthenticatedSessionPhase::PairingExclusive,
            Self::TerminatedUntilReset(_) => AuthenticatedSessionPhase::TerminatedUntilReset,
            Self::Transitioning => unreachable!("transition sentinel is never externally visible"),
        }
    }
}

/// Sole boot-lifetime authenticated-session owner for the E290 API bearer.
///
/// This type deliberately implements neither `Clone` nor `Copy`: duplicating it
/// would duplicate session keys and the sole epoch allocator.
#[must_use = "the boot-lifetime authenticated session owner must be driven or explicitly torn down"]
pub struct AuthenticatedSession {
    parameters: ServerParameters,
    epochs: SessionEpochAllocator,
    connection: Option<ConnectionId>,
    active_epoch: Option<SessionEpoch>,
    state: State,
}

impl AuthenticatedSession {
    /// Construct the sole owner with a fresh boot-lifetime epoch space.
    pub const fn new(parameters: ServerParameters) -> Self {
        Self {
            parameters,
            epochs: SessionEpochAllocator::new(),
            connection: None,
            active_epoch: None,
            state: State::Disconnected,
        }
    }

    /// Current externally observable phase.
    pub fn phase(&self) -> AuthenticatedSessionPhase {
        self.state.phase()
    }

    /// Accepted connection currently bound to per-connection state.
    pub const fn connection(&self) -> Option<ConnectionId> {
        self.connection
    }

    /// Retained terminal fault, if ordinary handling is closed until reset.
    pub const fn fault(&self) -> Option<AuthenticatedSessionFault> {
        match self.state {
            State::TerminatedUntilReset(fault) => Some(fault),
            _ => None,
        }
    }

    /// Begin the one handshake attempt allowed on a newly accepted connection.
    ///
    /// Returns `false` unless an explicit reset has first returned this owner to
    /// the disconnected phase.
    pub fn begin_connection(&mut self, connection: ConnectionId) -> bool {
        if !matches!(self.state, State::Disconnected) {
            return false;
        }
        self.connection = Some(connection);
        self.active_epoch = None;
        self.state = State::AwaitingClientHello;
        true
    }

    /// Drop all connection/session owners while retaining the epoch allocator.
    pub fn reset(&mut self) {
        self.connection = None;
        self.active_epoch = None;
        self.state = State::Disconnected;
    }

    /// Feed one streaming-decoder event into the connection machine.
    pub fn accept_decode_event(
        &mut self,
        event: DecodeEvent,
        at: PairingMillis,
    ) -> Result<SessionRxDisposition, AuthenticatedSessionFault> {
        match event {
            DecodeEvent::Pending => Ok(SessionRxDisposition::Pending),
            DecodeEvent::Record(record) => self.accept_record(record, at),
            DecodeEvent::MalformedCobs => Err(self.terminate(AuthenticatedSessionFault::Framing(
                SessionFramingFault::MalformedCobs,
            ))),
            DecodeEvent::MalformedRecord(_) => Err(self.terminate(
                AuthenticatedSessionFault::Framing(SessionFramingFault::MalformedRecord),
            )),
            DecodeEvent::Overflow => Err(self.terminate(AuthenticatedSessionFault::Framing(
                SessionFramingFault::Overflow,
            ))),
        }
    }

    /// Consume one canonical record according to the current exact owner phase.
    pub fn accept_record(
        &mut self,
        record: Record,
        at: PairingMillis,
    ) -> Result<SessionRxDisposition, AuthenticatedSessionFault> {
        let observed_kind = record.kind();
        let phase = self.phase();
        let state = mem::replace(&mut self.state, State::Transitioning);
        match state {
            State::AwaitingClientHello => self.accept_client_hello(record, at),
            State::PendingClientProof(pending) => match pending.authenticate(record) {
                Ok(session) => {
                    self.active_epoch = Some(session.epoch());
                    self.state = State::Established(session);
                    Ok(SessionRxDisposition::SessionEstablished)
                }
                Err(_) => Err(self.terminate(AuthenticatedSessionFault::Handshake)),
            },
            State::Established(session) => {
                if observed_kind == RECORD_KIND_CLIENT_HELLO {
                    drop(session);
                    self.active_epoch = None;
                    self.accept_client_hello(record, at)
                } else {
                    match session.authenticate_request(record) {
                        Ok(authenticated) => {
                            let (request, waiting) = authenticated.into_parts();
                            if decode_request(request.message().encoded()).is_err() {
                                drop(request);
                                drop(waiting);
                                return Err(self.terminate(
                                    AuthenticatedSessionFault::MalformedLogicalRequest,
                                ));
                            }
                            self.state = State::RequestHandoffPending { request, waiting };
                            Ok(SessionRxDisposition::RequestAuthenticated)
                        }
                        Err(fault) => {
                            Err(self.terminate(AuthenticatedSessionFault::Established(fault)))
                        }
                    }
                }
            }
            State::RequestHandoffPending { request, waiting }
                if observed_kind == RECORD_KIND_CLIENT_HELLO =>
            {
                drop(record);
                self.state = State::RequestHandoffPending { request, waiting };
                Ok(SessionRxDisposition::ClientHelloDroppedBusy)
            }
            State::AwaitingReply(waiting) if observed_kind == RECORD_KIND_CLIENT_HELLO => {
                drop(record);
                self.state = State::AwaitingReply(waiting);
                Ok(SessionRxDisposition::ClientHelloDroppedBusy)
            }
            State::ReplyFlight(owner) if observed_kind == RECORD_KIND_CLIENT_HELLO => {
                drop(record);
                self.state = State::ReplyFlight(owner);
                Ok(SessionRxDisposition::ClientHelloDroppedBusy)
            }
            State::PairingExclusive if observed_kind == RECORD_KIND_CLIENT_HELLO => {
                drop(record);
                self.state = State::PairingExclusive;
                Ok(SessionRxDisposition::ClientHelloDroppedBusy)
            }
            other => {
                drop(other);
                Err(self.terminate(AuthenticatedSessionFault::UnexpectedRecord {
                    phase,
                    observed_kind,
                }))
            }
        }
    }

    /// Try to transfer the retained selection command to the node owner.
    pub fn try_send_admission_command<M>(
        &mut self,
        handoff: &mut BearerSessionAdmissionHandoff<M>,
    ) -> AdmissionCommandDisposition
    where
        M: RawMutex + 'static,
    {
        let state = mem::replace(&mut self.state, State::Transitioning);
        let State::AdmissionCommandPending { hello, command } = state else {
            self.state = state;
            return AdmissionCommandDisposition::NotPending;
        };
        match handoff.try_send_command(command) {
            Ok(()) => {
                self.state = State::AwaitingAdmissionReply { hello };
                AdmissionCommandDisposition::Transferred
            }
            Err(pressure) => {
                self.state = State::AdmissionCommandPending {
                    hello,
                    command: pressure.into_inner(),
                };
                AdmissionCommandDisposition::RetainedUnderPressure
            }
        }
    }

    /// Consume one routed selection reply, draining replies from stale connections.
    pub fn accept_admission_reply<R>(
        &mut self,
        reply: SessionAdmissionReply,
        rng: &mut R,
    ) -> Result<AdmissionReplyDisposition, AuthenticatedSessionFault>
    where
        R: RngCore + CryptoRng,
    {
        if self.connection != Some(reply.connection()) {
            drop(reply);
            return Ok(AdmissionReplyDisposition::DrainedStale);
        }
        if matches!(
            self.state,
            State::PairingExclusive | State::TerminatedUntilReset(_)
        ) {
            drop(reply);
            return Ok(AdmissionReplyDisposition::DrainedStale);
        }

        let state = mem::replace(&mut self.state, State::Transitioning);
        let State::AwaitingAdmissionReply { hello } = state else {
            drop(state);
            drop(reply);
            return Err(self.terminate(AuthenticatedSessionFault::UnexpectedAdmissionReply));
        };
        let selected = match reply.into_outcome() {
            SessionAdmissionOutcome::Selected(selected) => selected,
            SessionAdmissionOutcome::Refused => {
                return Err(self.terminate(AuthenticatedSessionFault::AdmissionRefused));
            }
        };
        let flight = match ServerHelloFlight::begin(
            hello,
            ActiveCredential::from_selected(selected),
            self.parameters,
            &mut self.epochs,
            rng,
        ) {
            Ok(flight) => flight,
            Err(_fault) => {
                return Err(self.terminate(AuthenticatedSessionFault::Handshake));
            }
        };
        self.state = State::ServerHelloFlight(TxOwner::new(flight));
        Ok(AdmissionReplyDisposition::ServerHelloStarted)
    }

    /// Exact outbound record kind currently owned, if any.
    pub const fn tx_kind(&self) -> Option<SessionTxKind> {
        match self.state {
            State::ServerHelloFlight(_) => Some(SessionTxKind::ServerHello),
            State::ServerProofFlight(_) => Some(SessionTxKind::ServerProof),
            State::ReplyFlight(_) => Some(SessionTxKind::Reply),
            _ => None,
        }
    }

    /// Borrow at most `maximum` bytes from the exact current transmit owner.
    pub fn next_tx_chunk(&self, maximum: usize) -> Option<&[u8]> {
        match &self.state {
            State::ServerHelloFlight(owner) => Some(owner.flight.next_chunk(maximum)),
            State::ServerProofFlight(owner) => Some(owner.flight.next_chunk(maximum)),
            State::ReplyFlight(owner) => Some(owner.flight.next_chunk(maximum)),
            _ => None,
        }
    }

    /// Advance the exact current flight after a completed backend write.
    pub fn advance_tx(&mut self, acknowledged: usize) -> Result<SessionTxAdvance, SessionTxError> {
        let state = mem::replace(&mut self.state, State::Transitioning);
        match state {
            State::ServerHelloFlight(mut owner) => {
                if let Err(error) = owner.flight.advance(acknowledged) {
                    self.state = State::ServerHelloFlight(owner);
                    return Err(SessionTxError::Advance(error));
                }
                owner.started |= acknowledged != 0;
                if owner.flight.remaining().is_empty() {
                    let proof = owner
                        .flight
                        .try_finish()
                        .unwrap_or_else(|_| unreachable!("empty hello flight must finish"));
                    self.state = State::ServerProofFlight(TxOwner::new(proof));
                    Ok(SessionTxAdvance::RecordComplete {
                        kind: SessionTxKind::ServerHello,
                        next: AuthenticatedSessionPhase::ServerProofFlight,
                    })
                } else {
                    self.state = State::ServerHelloFlight(owner);
                    Ok(SessionTxAdvance::Partial)
                }
            }
            State::ServerProofFlight(mut owner) => {
                if let Err(error) = owner.flight.advance(acknowledged) {
                    self.state = State::ServerProofFlight(owner);
                    return Err(SessionTxError::Advance(error));
                }
                owner.started |= acknowledged != 0;
                if owner.flight.remaining().is_empty() {
                    let pending = owner
                        .flight
                        .try_finish()
                        .unwrap_or_else(|_| unreachable!("empty proof flight must finish"));
                    self.state = State::PendingClientProof(pending);
                    Ok(SessionTxAdvance::RecordComplete {
                        kind: SessionTxKind::ServerProof,
                        next: AuthenticatedSessionPhase::PendingClientProof,
                    })
                } else {
                    self.state = State::ServerProofFlight(owner);
                    Ok(SessionTxAdvance::Partial)
                }
            }
            State::ReplyFlight(mut owner) => {
                if let Err(error) = owner.flight.advance(acknowledged) {
                    self.state = State::ReplyFlight(owner);
                    return Err(SessionTxError::Advance(error));
                }
                owner.started |= acknowledged != 0;
                if owner.flight.remaining().is_empty() {
                    let session = owner
                        .flight
                        .try_finish()
                        .unwrap_or_else(|_| unreachable!("empty reply flight must finish"));
                    self.state = State::Established(session);
                    Ok(SessionTxAdvance::RecordComplete {
                        kind: SessionTxKind::Reply,
                        next: AuthenticatedSessionPhase::Established,
                    })
                } else {
                    self.state = State::ReplyFlight(owner);
                    Ok(SessionTxAdvance::Partial)
                }
            }
            other => {
                self.state = other;
                Err(SessionTxError::NotTransmitting)
            }
        }
    }

    /// Try to transfer the retained authenticated request to the node owner.
    pub fn try_send_request<M>(
        &mut self,
        sender: &mut RequestSender<M, AuthenticatedGrant>,
    ) -> RequestHandoffDisposition
    where
        M: RawMutex + 'static,
    {
        let state = mem::replace(&mut self.state, State::Transitioning);
        let State::RequestHandoffPending { request, waiting } = state else {
            self.state = state;
            return RequestHandoffDisposition::NotPending;
        };
        match sender.try_send(request) {
            Ok(()) => {
                self.state = State::AwaitingReply(waiting);
                RequestHandoffDisposition::Transferred
            }
            Err(pressure) => {
                self.state = State::RequestHandoffPending {
                    request: pressure.into_inner(),
                    waiting,
                };
                RequestHandoffDisposition::RetainedUnderPressure
            }
        }
    }

    /// Consume one node reply, sealing a match or explicitly draining a stale owner.
    #[allow(clippy::result_large_err)]
    pub fn accept_node_reply(
        &mut self,
        reply: LocalApiReply,
    ) -> Result<NodeReplyDisposition, NodeReplyFault> {
        let key = reply.key();
        let Some(epoch) = self.active_epoch else {
            drop(reply);
            return Ok(NodeReplyDisposition::DrainedStale {
                key,
                reason: StaleNodeReplyReason::NoLiveSession,
            });
        };
        if key.epoch() != epoch {
            drop(reply);
            return Ok(NodeReplyDisposition::DrainedStale {
                key,
                reason: StaleNodeReplyReason::WrongEpoch,
            });
        }

        let state = mem::replace(&mut self.state, State::Transitioning);
        let State::AwaitingReply(waiting) = state else {
            drop(state);
            let kind = self.terminate(AuthenticatedSessionFault::UnexpectedNodeReply);
            return Err(NodeReplyFault { kind, reply });
        };
        if !reply.matches(waiting.request_key()) {
            drop(waiting);
            let kind = self.terminate(AuthenticatedSessionFault::UnexpectedNodeReply);
            return Err(NodeReplyFault { kind, reply });
        }
        match waiting.frame_reply(reply) {
            Ok(flight) => {
                self.state = State::ReplyFlight(TxOwner::new(flight));
                Ok(NodeReplyDisposition::ReplyFlightStarted)
            }
            Err(fault) => {
                let fault_kind = fault.kind();
                let reply = fault.into_reply();
                let kind = self.terminate(AuthenticatedSessionFault::ReplyRoute(fault_kind));
                Err(NodeReplyFault { kind, reply })
            }
        }
    }

    /// Close ordinary state before acknowledging pairing exclusivity.
    ///
    /// If a record was partially emitted, its exact remaining bytes stay owned
    /// and the caller must drain them before retrying this method.
    pub fn close_for_pairing_exclusivity(
        &mut self,
        connection: ConnectionId,
    ) -> PairingExclusiveCloseDisposition {
        if self.connection != Some(connection) {
            return PairingExclusiveCloseDisposition::StaleConnection;
        }
        match &self.state {
            State::PairingExclusive => {
                return PairingExclusiveCloseDisposition::AlreadyExclusive;
            }
            State::ServerHelloFlight(owner) if owner.started => {
                return PairingExclusiveCloseDisposition::DrainBeforeClose {
                    kind: SessionTxKind::ServerHello,
                };
            }
            State::ServerProofFlight(owner) if owner.started => {
                return PairingExclusiveCloseDisposition::DrainBeforeClose {
                    kind: SessionTxKind::ServerProof,
                };
            }
            State::ReplyFlight(owner) if owner.started => {
                return PairingExclusiveCloseDisposition::DrainBeforeClose {
                    kind: SessionTxKind::Reply,
                };
            }
            _ => {}
        }
        self.active_epoch = None;
        self.state = State::PairingExclusive;
        PairingExclusiveCloseDisposition::Closed
    }

    fn accept_client_hello(
        &mut self,
        record: Record,
        at: PairingMillis,
    ) -> Result<SessionRxDisposition, AuthenticatedSessionFault> {
        let hello = match ClientHello::from_record(record) {
            Ok(hello) => hello,
            Err(_) => return Err(self.terminate(AuthenticatedSessionFault::Handshake)),
        };
        let Some(connection) = self.connection else {
            return Err(self.terminate(AuthenticatedSessionFault::Handshake));
        };
        let command = SessionAdmissionCommand::new(at, connection, hello.credential_id());
        self.active_epoch = None;
        self.state = State::AdmissionCommandPending { hello, command };
        Ok(SessionRxDisposition::ClientHelloAccepted)
    }

    fn terminate(&mut self, fault: AuthenticatedSessionFault) -> AuthenticatedSessionFault {
        self.active_epoch = None;
        self.state = State::TerminatedUntilReset(fault);
        fault
    }
}

const _: () =
    assert!(core::mem::size_of::<AuthenticatedSession>() <= AUTHENTICATED_SESSION_RAM_CEILING);

#[cfg(test)]
#[path = "authenticated_session_tests.rs"]
mod tests;
