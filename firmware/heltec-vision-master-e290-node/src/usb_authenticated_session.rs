//! Minimal owner-preserving authenticated-session machine for the E290 USB bearer.
//!
//! This module composes the portable session typestates with the E290's
//! credential-selection and logical-request handoffs. It deliberately supports
//! one handshake attempt per accepted connection and one authenticated request
//! in flight. A refusal, malformed handshake, framing fault, established-session
//! authentication/order fault, or malformed logical request terminates ordinary
//! session handling until an explicit reset.
//!
//! The sole [`SessionEpochAllocator`] is retained across reset, while every
//! connection-scoped owner (selected credential, key schedule, session keys,
//! partial transmit flight, and awaiting-reply route) is dropped. Pairing
//! exclusivity uses the same fail-closed boundary. If part of a record has
//! already reached the USB backend, the caller is told to drain that record
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
    ReplyFlight, ReplyRouteFaultKind, ServerHelloFlight, ServerParameters, ServerProofFlight,
    ServerSession, SessionEpochAllocator, SessionFault,
};

use crate::session_admission_handoff::{
    BearerSessionAdmissionHandoff, SessionAdmissionCommand, SessionAdmissionOutcome,
    SessionAdmissionReply,
};
use reticulum_device_api_pairing_policy::{ConnectionId, MonotonicMillis as PairingMillis};

/// Conservative fixed-RAM ceiling for the complete USB session owner.
pub const USB_AUTHENTICATED_SESSION_RAM_CEILING: usize = 2_048;

/// Externally observable phase of the minimal USB authenticated-session machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbAuthenticatedSessionPhase {
    /// No accepted USB connection is owned.
    Disconnected,
    /// The one client hello allowed on this connection has not arrived.
    AwaitingClientHello,
    /// The exact credential-selection command is retained under handoff pressure.
    AdmissionCommandPending,
    /// The client hello is retained while the node selects a credential.
    AwaitingAdmissionReply,
    /// The server-hello record is being acknowledged by the USB backend.
    ServerHelloFlight,
    /// The server-proof record is being acknowledged by the USB backend.
    ServerProofFlight,
    /// The server is waiting for the exact client-proof record.
    PendingClientProof,
    /// An authenticated session is idle and may accept one request.
    Established,
    /// An authenticated request is retained under node-handoff pressure.
    RequestHandoffPending,
    /// The request was transferred and its exact node reply is awaited.
    AwaitingReply,
    /// The authenticated response record is being acknowledged by USB.
    ReplyFlight,
    /// Ordinary session state was dropped before pairing-exclusive acknowledgement.
    PairingExclusive,
    /// A fatal fault closed ordinary handling until explicit reset.
    TerminatedUntilReset,
}

/// Coarse framing fault supplied by the bearer stream decoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbSessionFramingFault {
    /// A delimited COBS body was malformed.
    MalformedCobs,
    /// A decoded body was not one canonical framing record.
    MalformedRecord,
    /// An overlong body was discarded through its delimiter.
    Overflow,
}

/// Terminal reason retained until the bearer explicitly resets the machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbAuthenticatedSessionFault {
    /// The connection's sole credential-selection attempt was refused.
    AdmissionRefused,
    /// A handshake record, proof, bearer binding, entropy source, or epoch failed.
    Handshake,
    /// A framing-layer failure invalidated the connection stream.
    Framing(UsbSessionFramingFault),
    /// A record arrived in a phase that cannot own it.
    UnexpectedRecord {
        /// Phase that observed the record.
        phase: UsbAuthenticatedSessionPhase,
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
pub enum UsbSessionTxKind {
    /// Server hello.
    ServerHello,
    /// Server proof.
    ServerProof,
    /// Authenticated logical response.
    Reply,
}

/// Result of acknowledging bytes from the current exact transmit owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbSessionTxAdvance {
    /// The same record retains bytes that have not yet been acknowledged.
    Partial,
    /// The record completed and the state machine advanced to its next phase.
    RecordComplete {
        /// Exact record kind that completed.
        kind: UsbSessionTxKind,
        /// Phase entered after consuming that flight.
        next: UsbAuthenticatedSessionPhase,
    },
}

/// A transmit acknowledgement could not be applied without losing its owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbSessionTxError {
    /// No outbound record exists in the current phase.
    NotTransmitting,
    /// The backend acknowledged more bytes than the record retained.
    Advance(TxAdvanceError),
}

/// Observation produced by one decoded inbound record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbSessionRxDisposition {
    /// No complete framing record was present.
    Pending,
    /// A canonical client hello was retained for node-side selection.
    ClientHelloAccepted,
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
    kind: UsbAuthenticatedSessionFault,
    reply: LocalApiReply,
}

impl NodeReplyFault {
    /// Terminal session fault associated with this undelivered reply.
    pub const fn kind(&self) -> UsbAuthenticatedSessionFault {
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
        kind: UsbSessionTxKind,
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
    TerminatedUntilReset(UsbAuthenticatedSessionFault),
    Transitioning,
}

impl State {
    fn phase(&self) -> UsbAuthenticatedSessionPhase {
        match self {
            Self::Disconnected => UsbAuthenticatedSessionPhase::Disconnected,
            Self::AwaitingClientHello => UsbAuthenticatedSessionPhase::AwaitingClientHello,
            Self::AdmissionCommandPending { .. } => {
                UsbAuthenticatedSessionPhase::AdmissionCommandPending
            }
            Self::AwaitingAdmissionReply { .. } => {
                UsbAuthenticatedSessionPhase::AwaitingAdmissionReply
            }
            Self::ServerHelloFlight(_) => UsbAuthenticatedSessionPhase::ServerHelloFlight,
            Self::ServerProofFlight(_) => UsbAuthenticatedSessionPhase::ServerProofFlight,
            Self::PendingClientProof(_) => UsbAuthenticatedSessionPhase::PendingClientProof,
            Self::Established(_) => UsbAuthenticatedSessionPhase::Established,
            Self::RequestHandoffPending { .. } => {
                UsbAuthenticatedSessionPhase::RequestHandoffPending
            }
            Self::AwaitingReply(_) => UsbAuthenticatedSessionPhase::AwaitingReply,
            Self::ReplyFlight(_) => UsbAuthenticatedSessionPhase::ReplyFlight,
            Self::PairingExclusive => UsbAuthenticatedSessionPhase::PairingExclusive,
            Self::TerminatedUntilReset(_) => UsbAuthenticatedSessionPhase::TerminatedUntilReset,
            Self::Transitioning => unreachable!("transition sentinel is never externally visible"),
        }
    }
}

/// Sole boot-lifetime authenticated-session owner for the E290 USB bearer.
///
/// This type deliberately implements neither `Clone` nor `Copy`: duplicating it
/// would duplicate session keys and the sole epoch allocator.
#[must_use = "the boot-lifetime USB session owner must be driven or explicitly torn down"]
pub struct UsbAuthenticatedSession {
    parameters: ServerParameters,
    epochs: SessionEpochAllocator,
    connection: Option<ConnectionId>,
    active_epoch: Option<SessionEpoch>,
    state: State,
}

impl UsbAuthenticatedSession {
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
    pub fn phase(&self) -> UsbAuthenticatedSessionPhase {
        self.state.phase()
    }

    /// Accepted connection currently bound to per-connection state.
    pub const fn connection(&self) -> Option<ConnectionId> {
        self.connection
    }

    /// Retained terminal fault, if ordinary handling is closed until reset.
    pub const fn fault(&self) -> Option<UsbAuthenticatedSessionFault> {
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
    ) -> Result<UsbSessionRxDisposition, UsbAuthenticatedSessionFault> {
        match event {
            DecodeEvent::Pending => Ok(UsbSessionRxDisposition::Pending),
            DecodeEvent::Record(record) => self.accept_record(record, at),
            DecodeEvent::MalformedCobs => Err(self.terminate(
                UsbAuthenticatedSessionFault::Framing(UsbSessionFramingFault::MalformedCobs),
            )),
            DecodeEvent::MalformedRecord(_) => Err(self.terminate(
                UsbAuthenticatedSessionFault::Framing(UsbSessionFramingFault::MalformedRecord),
            )),
            DecodeEvent::Overflow => Err(self.terminate(UsbAuthenticatedSessionFault::Framing(
                UsbSessionFramingFault::Overflow,
            ))),
        }
    }

    /// Consume one canonical record according to the current exact owner phase.
    pub fn accept_record(
        &mut self,
        record: Record,
        at: PairingMillis,
    ) -> Result<UsbSessionRxDisposition, UsbAuthenticatedSessionFault> {
        let observed_kind = record.kind();
        let phase = self.phase();
        let state = mem::replace(&mut self.state, State::Transitioning);
        match state {
            State::AwaitingClientHello => {
                let hello = match ClientHello::from_record(record) {
                    Ok(hello) => hello,
                    Err(_) => {
                        return Err(self.terminate(UsbAuthenticatedSessionFault::Handshake));
                    }
                };
                let Some(connection) = self.connection else {
                    return Err(self.terminate(UsbAuthenticatedSessionFault::Handshake));
                };
                let command = SessionAdmissionCommand::new(at, connection, hello.credential_id());
                self.state = State::AdmissionCommandPending { hello, command };
                Ok(UsbSessionRxDisposition::ClientHelloAccepted)
            }
            State::PendingClientProof(pending) => match pending.authenticate(record) {
                Ok(session) => {
                    self.active_epoch = Some(session.epoch());
                    self.state = State::Established(session);
                    Ok(UsbSessionRxDisposition::SessionEstablished)
                }
                Err(_) => Err(self.terminate(UsbAuthenticatedSessionFault::Handshake)),
            },
            State::Established(session) => match session.authenticate_request(record) {
                Ok(authenticated) => {
                    let (request, waiting) = authenticated.into_parts();
                    if decode_request(request.message().encoded()).is_err() {
                        drop(request);
                        drop(waiting);
                        return Err(
                            self.terminate(UsbAuthenticatedSessionFault::MalformedLogicalRequest)
                        );
                    }
                    self.state = State::RequestHandoffPending { request, waiting };
                    Ok(UsbSessionRxDisposition::RequestAuthenticated)
                }
                Err(fault) => Err(self.terminate(UsbAuthenticatedSessionFault::Established(fault))),
            },
            other => {
                drop(other);
                Err(
                    self.terminate(UsbAuthenticatedSessionFault::UnexpectedRecord {
                        phase,
                        observed_kind,
                    }),
                )
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
    ) -> Result<AdmissionReplyDisposition, UsbAuthenticatedSessionFault>
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
            return Err(self.terminate(UsbAuthenticatedSessionFault::UnexpectedAdmissionReply));
        };
        let selected = match reply.into_outcome() {
            SessionAdmissionOutcome::Selected(selected) => selected,
            SessionAdmissionOutcome::Refused => {
                return Err(self.terminate(UsbAuthenticatedSessionFault::AdmissionRefused));
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
                return Err(self.terminate(UsbAuthenticatedSessionFault::Handshake));
            }
        };
        self.state = State::ServerHelloFlight(TxOwner::new(flight));
        Ok(AdmissionReplyDisposition::ServerHelloStarted)
    }

    /// Exact outbound record kind currently owned, if any.
    pub const fn tx_kind(&self) -> Option<UsbSessionTxKind> {
        match self.state {
            State::ServerHelloFlight(_) => Some(UsbSessionTxKind::ServerHello),
            State::ServerProofFlight(_) => Some(UsbSessionTxKind::ServerProof),
            State::ReplyFlight(_) => Some(UsbSessionTxKind::Reply),
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
    pub fn advance_tx(
        &mut self,
        acknowledged: usize,
    ) -> Result<UsbSessionTxAdvance, UsbSessionTxError> {
        let state = mem::replace(&mut self.state, State::Transitioning);
        match state {
            State::ServerHelloFlight(mut owner) => {
                if let Err(error) = owner.flight.advance(acknowledged) {
                    self.state = State::ServerHelloFlight(owner);
                    return Err(UsbSessionTxError::Advance(error));
                }
                owner.started |= acknowledged != 0;
                if owner.flight.remaining().is_empty() {
                    let proof = owner
                        .flight
                        .try_finish()
                        .unwrap_or_else(|_| unreachable!("empty hello flight must finish"));
                    self.state = State::ServerProofFlight(TxOwner::new(proof));
                    Ok(UsbSessionTxAdvance::RecordComplete {
                        kind: UsbSessionTxKind::ServerHello,
                        next: UsbAuthenticatedSessionPhase::ServerProofFlight,
                    })
                } else {
                    self.state = State::ServerHelloFlight(owner);
                    Ok(UsbSessionTxAdvance::Partial)
                }
            }
            State::ServerProofFlight(mut owner) => {
                if let Err(error) = owner.flight.advance(acknowledged) {
                    self.state = State::ServerProofFlight(owner);
                    return Err(UsbSessionTxError::Advance(error));
                }
                owner.started |= acknowledged != 0;
                if owner.flight.remaining().is_empty() {
                    let pending = owner
                        .flight
                        .try_finish()
                        .unwrap_or_else(|_| unreachable!("empty proof flight must finish"));
                    self.state = State::PendingClientProof(pending);
                    Ok(UsbSessionTxAdvance::RecordComplete {
                        kind: UsbSessionTxKind::ServerProof,
                        next: UsbAuthenticatedSessionPhase::PendingClientProof,
                    })
                } else {
                    self.state = State::ServerProofFlight(owner);
                    Ok(UsbSessionTxAdvance::Partial)
                }
            }
            State::ReplyFlight(mut owner) => {
                if let Err(error) = owner.flight.advance(acknowledged) {
                    self.state = State::ReplyFlight(owner);
                    return Err(UsbSessionTxError::Advance(error));
                }
                owner.started |= acknowledged != 0;
                if owner.flight.remaining().is_empty() {
                    let session = owner
                        .flight
                        .try_finish()
                        .unwrap_or_else(|_| unreachable!("empty reply flight must finish"));
                    self.state = State::Established(session);
                    Ok(UsbSessionTxAdvance::RecordComplete {
                        kind: UsbSessionTxKind::Reply,
                        next: UsbAuthenticatedSessionPhase::Established,
                    })
                } else {
                    self.state = State::ReplyFlight(owner);
                    Ok(UsbSessionTxAdvance::Partial)
                }
            }
            other => {
                self.state = other;
                Err(UsbSessionTxError::NotTransmitting)
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
            let kind = self.terminate(UsbAuthenticatedSessionFault::UnexpectedNodeReply);
            return Err(NodeReplyFault { kind, reply });
        };
        if !reply.matches(waiting.request_key()) {
            drop(waiting);
            let kind = self.terminate(UsbAuthenticatedSessionFault::UnexpectedNodeReply);
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
                let kind = self.terminate(UsbAuthenticatedSessionFault::ReplyRoute(fault_kind));
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
                    kind: UsbSessionTxKind::ServerHello,
                };
            }
            State::ServerProofFlight(owner) if owner.started => {
                return PairingExclusiveCloseDisposition::DrainBeforeClose {
                    kind: UsbSessionTxKind::ServerProof,
                };
            }
            State::ReplyFlight(owner) if owner.started => {
                return PairingExclusiveCloseDisposition::DrainBeforeClose {
                    kind: UsbSessionTxKind::Reply,
                };
            }
            _ => {}
        }
        self.active_epoch = None;
        self.state = State::PairingExclusive;
        PairingExclusiveCloseDisposition::Closed
    }

    fn terminate(&mut self, fault: UsbAuthenticatedSessionFault) -> UsbAuthenticatedSessionFault {
        self.active_epoch = None;
        self.state = State::TerminatedUntilReset(fault);
        fault
    }
}

const _: () = assert!(
    core::mem::size_of::<UsbAuthenticatedSession>() <= USB_AUTHENTICATED_SESSION_RAM_CEILING
);

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rand_core::{CryptoRng, RngCore};
    use reticulum_device_api::{
        ApiVersion, DeviceRequest, Permissions, PrincipalId, RequestEnvelope, RequestId,
        encode_request,
    };
    use reticulum_device_api_credentials::{
        AuthorityRevision, AuthorizationPolicyVersion, CredentialAudit, CredentialAuthorityBuilder,
        CredentialGeneration, CredentialId, CredentialRecord, CredentialStatus, PairingOrigin,
        SelectedCredential,
    };
    use reticulum_device_api_framing::{DecodeEvent, Record, StreamDecoder};
    use reticulum_device_api_handoff::{
        DeviceApiHandoff, LocalApiReply, MESSAGE_CAPACITY, MessageLength, OwnedMessage,
        SessionEpoch,
    };
    use reticulum_device_api_pairing_policy::{ConnectionId, MonotonicMillis as PairingMillis};
    use reticulum_device_api_session::{
        AuthenticatedGrant, BearerBinding, ClientCredential, ClientHelloFlight, ClientParameters,
        ClientSession, DeviceId, ServerParameters,
    };

    use crate::session_admission_handoff::{
        SessionAdmissionHandoff, SessionAdmissionOutcome, SessionAdmissionReply,
    };

    use super::{
        AdmissionCommandDisposition, AdmissionReplyDisposition, NodeReplyDisposition,
        PairingExclusiveCloseDisposition, RequestHandoffDisposition, StaleNodeReplyReason,
        USB_AUTHENTICATED_SESSION_RAM_CEILING, UsbAuthenticatedSession,
        UsbAuthenticatedSessionFault, UsbAuthenticatedSessionPhase, UsbSessionFramingFault,
        UsbSessionRxDisposition, UsbSessionTxAdvance, UsbSessionTxKind,
    };

    const CREDENTIAL_ID: CredentialId = CredentialId::new([0x31; 16]);
    const GENERATION: CredentialGeneration = CredentialGeneration::new(7);
    const PSK: [u8; 32] = [0x42; 32];
    const DEVICE_ID: DeviceId = DeviceId::new([0x53; 16]);

    struct FixedRng {
        byte: u8,
    }

    impl FixedRng {
        const fn new(byte: u8) -> Self {
            Self { byte }
        }
    }

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            u32::from_le_bytes([self.byte; 4])
        }

        fn next_u64(&mut self) -> u64 {
            u64::from_le_bytes([self.byte; 8])
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            destination.fill(self.byte);
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for FixedRng {}

    fn connection(value: u64) -> ConnectionId {
        ConnectionId::new(value).expect("test connection is nonzero")
    }

    fn selected_credential() -> SelectedCredential {
        let revision = AuthorityRevision::new(GENERATION.get());
        CredentialAuthorityBuilder::<1>::new(revision)
            .unwrap_or_else(|fault| panic!("authority revision rejected: {:?}", fault.kind()))
            .insert(CredentialRecord::with_secret(
                CREDENTIAL_ID,
                GENERATION,
                PrincipalId([0x64; 16]),
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
            .unwrap_or_else(|fault| panic!("credential rejected: {:?}", fault.kind()))
            .finish()
            .select_for_handshake(CREDENTIAL_ID)
            .unwrap_or_else(|_| panic!("active credential was not selectable"))
    }

    fn new_machine() -> UsbAuthenticatedSession {
        UsbAuthenticatedSession::new(ServerParameters::new(
            DEVICE_ID,
            BearerBinding::UsbSerialJtag,
        ))
    }

    fn decode_one(bytes: &[u8]) -> Record {
        let mut decoder = StreamDecoder::new();
        for byte in bytes {
            match decoder.push(*byte) {
                DecodeEvent::Pending => {}
                DecodeEvent::Record(record) => return record,
                DecodeEvent::MalformedCobs
                | DecodeEvent::MalformedRecord(_)
                | DecodeEvent::Overflow => panic!("test flight emitted malformed framing"),
            }
        }
        panic!("test flight emitted no complete record")
    }

    fn complete_client_flight<F>(
        remaining: impl Fn(&F) -> &[u8],
        advance: impl Fn(&mut F, usize),
        flight: &mut F,
    ) -> std::vec::Vec<u8> {
        let bytes = remaining(flight).to_vec();
        advance(flight, bytes.len());
        bytes
    }

    fn drain_machine_record(
        machine: &mut UsbAuthenticatedSession,
        expected: UsbSessionTxKind,
        maximum: usize,
    ) -> std::vec::Vec<u8> {
        let mut bytes = std::vec::Vec::new();
        while machine.tx_kind() == Some(expected) {
            let chunk = machine
                .next_tx_chunk(maximum)
                .expect("machine retained the expected flight")
                .to_vec();
            assert!(!chunk.is_empty());
            bytes.extend_from_slice(&chunk);
            let disposition = machine
                .advance_tx(chunk.len())
                .expect("exact acknowledgement is valid");
            if machine.tx_kind() == Some(expected) {
                assert_eq!(disposition, UsbSessionTxAdvance::Partial);
            }
        }
        bytes
    }

    fn establish(
        machine: &mut UsbAuthenticatedSession,
        connection: ConnectionId,
        client_nonce: u8,
        server_nonce: u8,
    ) -> ClientSession {
        assert!(machine.begin_connection(connection));
        let mut client_hello = ClientHelloFlight::begin(
            ClientParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
            ClientCredential::new(CREDENTIAL_ID, GENERATION, PSK),
            &mut FixedRng::new(client_nonce),
        )
        .expect("client hello begins");
        let hello_bytes = complete_client_flight(
            |flight: &ClientHelloFlight| flight.remaining(),
            |flight, acknowledged| flight.advance(acknowledged).unwrap(),
            &mut client_hello,
        );
        let awaiting_server_hello = client_hello
            .try_finish()
            .unwrap_or_else(|_| panic!("hello fully sent"));
        assert_eq!(
            machine
                .accept_record(decode_one(&hello_bytes), PairingMillis::new(10))
                .unwrap(),
            UsbSessionRxDisposition::ClientHelloAccepted
        );

        let admission: &'static mut SessionAdmissionHandoff<NoopRawMutex> =
            std::boxed::Box::leak(std::boxed::Box::new(SessionAdmissionHandoff::new()));
        let (mut bearer_admission, mut node_admission) = admission.split();
        assert_eq!(
            machine.try_send_admission_command(&mut bearer_admission),
            AdmissionCommandDisposition::Transferred
        );
        let command = node_admission
            .try_receive_command()
            .expect("selection command transferred");
        assert_eq!(command.connection(), connection);
        assert_eq!(command.credential_id(), CREDENTIAL_ID);
        node_admission
            .try_send_reply(SessionAdmissionReply::new(
                command.connection(),
                SessionAdmissionOutcome::Selected(selected_credential()),
            ))
            .unwrap_or_else(|_| panic!("empty selection reply channel"));
        let reply = bearer_admission
            .try_receive_reply()
            .expect("selection reply transferred");
        assert_eq!(
            machine
                .accept_admission_reply(reply, &mut FixedRng::new(server_nonce))
                .unwrap(),
            AdmissionReplyDisposition::ServerHelloStarted
        );

        let server_hello_bytes = drain_machine_record(machine, UsbSessionTxKind::ServerHello, 7);
        let awaiting_server_proof = awaiting_server_hello
            .accept(decode_one(&server_hello_bytes))
            .expect("server hello authenticates expected facts");
        let server_proof_bytes = drain_machine_record(machine, UsbSessionTxKind::ServerProof, 5);
        let mut client_proof = awaiting_server_proof
            .verify(decode_one(&server_proof_bytes))
            .expect("server proof authenticates");
        let proof_bytes = complete_client_flight(
            |flight: &reticulum_device_api_session::ClientProofFlight| flight.remaining(),
            |flight, acknowledged| flight.advance(acknowledged).unwrap(),
            &mut client_proof,
        );
        let client_session = client_proof
            .try_finish()
            .unwrap_or_else(|_| panic!("client proof fully sent"));
        assert_eq!(
            machine
                .accept_record(decode_one(&proof_bytes), PairingMillis::new(11))
                .unwrap(),
            UsbSessionRxDisposition::SessionEstablished
        );
        client_session
    }

    fn canonical_request(request_id: u64) -> OwnedMessage {
        let envelope = RequestEnvelope {
            version: ApiVersion::CURRENT,
            request_id: RequestId(request_id),
            request: DeviceRequest::SystemCapabilities,
        };
        let mut encoded = [0_u8; MESSAGE_CAPACITY];
        let length = encode_request(&envelope, &mut encoded).expect("request encodes");
        OwnedMessage::new(MessageLength::new(length).unwrap(), encoded)
    }

    fn message(bytes: &[u8]) -> OwnedMessage {
        let mut buffer = [0_u8; MESSAGE_CAPACITY];
        buffer[..bytes.len()].copy_from_slice(bytes);
        OwnedMessage::new(MessageLength::new(bytes.len()).unwrap(), buffer)
    }

    #[test]
    fn real_handshake_and_request_reply_preserve_partial_tx_and_exact_owners() {
        let mut machine = new_machine();
        let client = establish(&mut machine, connection(1), 0x71, 0x81);
        assert_eq!(machine.phase(), UsbAuthenticatedSessionPhase::Established);
        assert_eq!(client.next_client_sequence(), 0);
        assert_eq!(client.next_server_sequence(), 0);

        let mut request_flight = client
            .frame_request(canonical_request(44))
            .unwrap_or_else(|_| panic!("request frames"));
        let request_bytes = complete_client_flight(
            |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
            |flight, acknowledged| flight.advance(acknowledged).unwrap(),
            &mut request_flight,
        );
        let awaiting_response = request_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("request fully sent"));
        assert_eq!(
            machine
                .accept_record(decode_one(&request_bytes), PairingMillis::new(12))
                .unwrap(),
            UsbSessionRxDisposition::RequestAuthenticated
        );

        let api: &'static mut DeviceApiHandoff<NoopRawMutex, AuthenticatedGrant> =
            std::boxed::Box::leak(std::boxed::Box::new(DeviceApiHandoff::new()));
        let (mut bearer, mut node) = api.split();
        assert_eq!(
            machine.try_send_request(bearer.requests()),
            RequestHandoffDisposition::Transferred
        );
        let request = node
            .requests()
            .try_receive()
            .expect("node owns authenticated request");
        assert_eq!(request.key().epoch(), SessionEpoch::new(1));
        assert_eq!(request.key().correlation().get(), 0);
        assert_eq!(request.grant().admission_sequence(), 0);
        let reply = LocalApiReply::new(request.key(), message(b"exact reply"));
        drop(request);
        assert_eq!(
            machine
                .accept_node_reply(reply)
                .unwrap_or_else(|_| panic!("matching node reply starts a flight")),
            NodeReplyDisposition::ReplyFlightStarted
        );
        let reply_bytes = drain_machine_record(&mut machine, UsbSessionTxKind::Reply, 3);
        let authenticated = awaiting_response
            .authenticate(decode_one(&reply_bytes))
            .expect("client authenticates exact response");
        let (client, response) = authenticated.into_parts();
        assert_eq!(response.encoded(), b"exact reply");
        assert_eq!(client.next_client_sequence(), 1);
        assert_eq!(client.next_server_sequence(), 1);
        assert_eq!(machine.phase(), UsbAuthenticatedSessionPhase::Established);
    }

    #[test]
    fn request_handoff_pressure_retains_the_exact_request_until_capacity_returns() {
        let mut machine = new_machine();
        let client = establish(&mut machine, connection(1), 0x72, 0x82);
        let mut request_flight = client
            .frame_request(canonical_request(45))
            .unwrap_or_else(|_| panic!("request frames"));
        let bytes = complete_client_flight(
            |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
            |flight, acknowledged| flight.advance(acknowledged).unwrap(),
            &mut request_flight,
        );
        drop(
            request_flight
                .try_finish()
                .unwrap_or_else(|_| panic!("request fully sent")),
        );
        machine
            .accept_record(decode_one(&bytes), PairingMillis::new(12))
            .unwrap();

        let api: &'static mut DeviceApiHandoff<NoopRawMutex, AuthenticatedGrant> =
            std::boxed::Box::leak(std::boxed::Box::new(DeviceApiHandoff::new()));
        let (mut bearer, mut node) = api.split();
        // Occupy request capacity with a real request from a second established
        // machine, then verify the first machine keeps its exact owner.
        let mut blocker = new_machine();
        let blocker_client = establish(&mut blocker, connection(2), 0x73, 0x83);
        let mut blocker_flight = blocker_client
            .frame_request(canonical_request(46))
            .unwrap_or_else(|_| panic!("blocker frames"));
        let blocker_bytes = complete_client_flight(
            |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
            |flight, acknowledged| flight.advance(acknowledged).unwrap(),
            &mut blocker_flight,
        );
        drop(
            blocker_flight
                .try_finish()
                .unwrap_or_else(|_| panic!("blocker fully sent")),
        );
        blocker
            .accept_record(decode_one(&blocker_bytes), PairingMillis::new(12))
            .unwrap();
        assert_eq!(
            blocker.try_send_request(bearer.requests()),
            RequestHandoffDisposition::Transferred
        );

        assert_eq!(
            machine.try_send_request(bearer.requests()),
            RequestHandoffDisposition::RetainedUnderPressure
        );
        assert_eq!(
            machine.phase(),
            UsbAuthenticatedSessionPhase::RequestHandoffPending
        );
        let blocker_request = node.requests().try_receive().expect("blocker queued");
        assert_eq!(blocker_request.key().epoch(), SessionEpoch::new(1));
        drop(blocker_request);
        assert_eq!(
            machine.try_send_request(bearer.requests()),
            RequestHandoffDisposition::Transferred
        );
        let retained = node
            .requests()
            .try_receive()
            .expect("retained request queued");
        assert_eq!(
            retained.message().encoded(),
            canonical_request(45).encoded()
        );
    }

    #[test]
    fn reset_drains_stale_reply_and_advances_epoch_without_sharing_sequences() {
        let mut machine = new_machine();
        let first_client = establish(&mut machine, connection(1), 0x74, 0x84);
        let mut flight = first_client
            .frame_request(canonical_request(47))
            .unwrap_or_else(|_| panic!("first request frames"));
        let bytes = complete_client_flight(
            |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
            |flight, acknowledged| flight.advance(acknowledged).unwrap(),
            &mut flight,
        );
        drop(
            flight
                .try_finish()
                .unwrap_or_else(|_| panic!("first request fully sent")),
        );
        machine
            .accept_record(decode_one(&bytes), PairingMillis::new(12))
            .unwrap();
        let api: &'static mut DeviceApiHandoff<NoopRawMutex, AuthenticatedGrant> =
            std::boxed::Box::leak(std::boxed::Box::new(DeviceApiHandoff::new()));
        let (mut bearer, mut node) = api.split();
        machine.try_send_request(bearer.requests());
        let old_request = node.requests().try_receive().expect("old request queued");
        let old_key = old_request.key();
        drop(old_request);

        machine.reset();
        let stale = machine
            .accept_node_reply(LocalApiReply::new(old_key, message(b"late")))
            .unwrap_or_else(|_| panic!("reset reply is classified stale"));
        assert_eq!(
            stale,
            NodeReplyDisposition::DrainedStale {
                key: old_key,
                reason: StaleNodeReplyReason::NoLiveSession,
            }
        );

        let second_client = establish(&mut machine, connection(2), 0x75, 0x85);
        assert_eq!(second_client.next_client_sequence(), 0);
        assert_eq!(second_client.next_server_sequence(), 0);
        let mut second_flight = second_client
            .frame_request(canonical_request(48))
            .unwrap_or_else(|_| panic!("second request frames"));
        let second_bytes = complete_client_flight(
            |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
            |flight, acknowledged| flight.advance(acknowledged).unwrap(),
            &mut second_flight,
        );
        drop(
            second_flight
                .try_finish()
                .unwrap_or_else(|_| panic!("second request fully sent")),
        );
        machine
            .accept_record(decode_one(&second_bytes), PairingMillis::new(13))
            .unwrap();
        machine.try_send_request(bearer.requests());
        let second_request = node
            .requests()
            .try_receive()
            .expect("second request queued");
        assert_eq!(second_request.key().epoch(), SessionEpoch::new(2));
        assert_eq!(second_request.key().correlation().get(), 0);
        assert_eq!(second_request.grant().admission_sequence(), 0);
    }

    #[test]
    fn established_fault_is_terminal_until_explicit_reset() {
        let mut machine = new_machine();
        let client = establish(&mut machine, connection(1), 0x76, 0x86);
        let mut request = client
            .frame_request(canonical_request(49))
            .unwrap_or_else(|_| panic!("request frames"));
        let bytes = complete_client_flight(
            |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
            |flight, acknowledged| flight.advance(acknowledged).unwrap(),
            &mut request,
        );
        drop(
            request
                .try_finish()
                .unwrap_or_else(|_| panic!("request sent")),
        );
        machine
            .accept_record(decode_one(&bytes), PairingMillis::new(12))
            .unwrap();
        let fault = machine
            .accept_record(decode_one(&bytes), PairingMillis::new(13))
            .expect_err("second in-flight request violates ordering");
        assert!(matches!(
            fault,
            UsbAuthenticatedSessionFault::UnexpectedRecord {
                phase: UsbAuthenticatedSessionPhase::RequestHandoffPending,
                ..
            }
        ));
        assert_eq!(
            machine.phase(),
            UsbAuthenticatedSessionPhase::TerminatedUntilReset
        );
        assert!(!machine.begin_connection(connection(2)));
        machine.reset();
        assert!(machine.begin_connection(connection(2)));
        let framing = machine
            .accept_decode_event(DecodeEvent::MalformedCobs, PairingMillis::new(14))
            .expect_err("framing is terminal");
        assert_eq!(
            framing,
            UsbAuthenticatedSessionFault::Framing(UsbSessionFramingFault::MalformedCobs)
        );
    }

    #[test]
    fn pairing_exclusivity_drains_partial_record_then_drops_ordinary_state() {
        let mut machine = new_machine();
        assert!(machine.begin_connection(connection(1)));
        let mut hello = ClientHelloFlight::begin(
            ClientParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
            ClientCredential::new(CREDENTIAL_ID, GENERATION, PSK),
            &mut FixedRng::new(0x77),
        )
        .expect("hello begins");
        let hello_bytes = complete_client_flight(
            |flight: &ClientHelloFlight| flight.remaining(),
            |flight, acknowledged| flight.advance(acknowledged).unwrap(),
            &mut hello,
        );
        drop(hello.try_finish().unwrap_or_else(|_| panic!("hello sent")));
        machine
            .accept_record(decode_one(&hello_bytes), PairingMillis::new(10))
            .unwrap();
        let admission: &'static mut SessionAdmissionHandoff<NoopRawMutex> =
            std::boxed::Box::leak(std::boxed::Box::new(SessionAdmissionHandoff::new()));
        let (mut bearer, mut node) = admission.split();
        machine.try_send_admission_command(&mut bearer);
        let command = node.try_receive_command().expect("command queued");
        node.try_send_reply(SessionAdmissionReply::new(
            command.connection(),
            SessionAdmissionOutcome::Selected(selected_credential()),
        ))
        .unwrap_or_else(|_| panic!("reply channel empty"));
        machine
            .accept_admission_reply(
                bearer.try_receive_reply().expect("reply queued"),
                &mut FixedRng::new(0x87),
            )
            .unwrap();

        let first = machine.next_tx_chunk(4).unwrap().to_vec();
        assert_eq!(first.len(), 4);
        assert_eq!(
            machine.advance_tx(first.len()).unwrap(),
            UsbSessionTxAdvance::Partial
        );
        assert_eq!(
            machine.close_for_pairing_exclusivity(connection(1)),
            PairingExclusiveCloseDisposition::DrainBeforeClose {
                kind: UsbSessionTxKind::ServerHello,
            }
        );
        drain_machine_record(&mut machine, UsbSessionTxKind::ServerHello, 16);
        assert_eq!(
            machine.close_for_pairing_exclusivity(connection(1)),
            PairingExclusiveCloseDisposition::Closed
        );
        assert_eq!(
            machine.phase(),
            UsbAuthenticatedSessionPhase::PairingExclusive
        );
    }

    #[test]
    fn selected_admission_reply_after_pairing_close_is_drained_without_starting_tx() {
        let mut machine = new_machine();
        assert!(machine.begin_connection(connection(1)));
        let mut hello = ClientHelloFlight::begin(
            ClientParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
            ClientCredential::new(CREDENTIAL_ID, GENERATION, PSK),
            &mut FixedRng::new(0x78),
        )
        .expect("hello begins");
        let hello_bytes = complete_client_flight(
            |flight: &ClientHelloFlight| flight.remaining(),
            |flight, acknowledged| flight.advance(acknowledged).unwrap(),
            &mut hello,
        );
        drop(hello.try_finish().unwrap_or_else(|_| panic!("hello sent")));
        machine
            .accept_record(decode_one(&hello_bytes), PairingMillis::new(10))
            .unwrap();

        let admission: &'static mut SessionAdmissionHandoff<NoopRawMutex> =
            std::boxed::Box::leak(std::boxed::Box::new(SessionAdmissionHandoff::new()));
        let (mut bearer, mut node) = admission.split();
        assert_eq!(
            machine.try_send_admission_command(&mut bearer),
            AdmissionCommandDisposition::Transferred
        );
        let command = node.try_receive_command().expect("command queued");
        node.try_send_reply(SessionAdmissionReply::new(
            command.connection(),
            SessionAdmissionOutcome::Selected(selected_credential()),
        ))
        .unwrap_or_else(|_| panic!("reply channel empty"));

        assert_eq!(
            machine.close_for_pairing_exclusivity(connection(1)),
            PairingExclusiveCloseDisposition::Closed
        );
        assert_eq!(
            machine
                .accept_admission_reply(
                    bearer.try_receive_reply().expect("selected reply queued"),
                    &mut FixedRng::new(0x88),
                )
                .unwrap(),
            AdmissionReplyDisposition::DrainedStale
        );
        assert_eq!(
            machine.phase(),
            UsbAuthenticatedSessionPhase::PairingExclusive
        );
        assert_eq!(machine.tx_kind(), None);
    }

    #[test]
    fn owner_is_send_static_and_within_the_declared_ram_ceiling() {
        fn require_send_static<T: Send + 'static>() {}
        require_send_static::<UsbAuthenticatedSession>();
        std::println!(
            "usb_authenticated_session_size={}",
            core::mem::size_of::<UsbAuthenticatedSession>()
        );
        assert!(
            core::mem::size_of::<UsbAuthenticatedSession>()
                <= USB_AUTHENTICATED_SESSION_RAM_CEILING
        );
    }
}
