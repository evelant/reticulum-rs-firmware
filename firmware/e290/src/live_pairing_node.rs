//! Pure node-side correlation for one admitted durable live-pairing mutation.
//!
//! The platform flash owner produces one bounded drive outcome at a time. This
//! module keeps the request connection and sequence attached until that drive
//! reaches the matching durable terminal result. It deliberately contains no
//! executor, bearer driver, clock, entropy, journal, or physical-flash dependency.

use reticulum_device_api_pairing::{
    AbortCurrentResponse, AbortResult, ActivateFailure, ActivateResponse, BearerBinding,
    BeginResponse, PairingFailure, PairingResponse,
};
use reticulum_device_api_pairing_policy::ConnectionId;

use crate::{
    credential_pairing::{CredentialPairingDriveOutcome, PairingDriveRetry, PairingMutation},
    live_pairing_handoff::LivePairingReply,
};

/// Exact request correlation retained until one durable mutation terminates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "an admitted live-pairing mutation must retain its request correlation"]
pub struct LivePairingOperation {
    bearer: BearerBinding,
    connection: ConnectionId,
    sequence: u64,
    mutation: PairingMutation,
}

impl LivePairingOperation {
    /// Bind one admitted mutation to the request that must receive its result.
    pub const fn new(
        bearer: BearerBinding,
        connection: ConnectionId,
        sequence: u64,
        mutation: PairingMutation,
    ) -> Self {
        Self {
            bearer,
            connection,
            sequence,
            mutation,
        }
    }

    /// Exact pairing profile that admitted this operation.
    pub const fn bearer(self) -> BearerBinding {
        self.bearer
    }

    /// Exact bearer connection that admitted this operation.
    pub const fn connection(self) -> ConnectionId {
        self.connection
    }

    /// Opaque request sequence echoed by the eventual response.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Durable mutation kind that must match every typed drive outcome.
    pub const fn mutation(self) -> PairingMutation {
        self.mutation
    }

    /// Consume one physical-drive result while retaining correlation until a
    /// matching durable terminal result exists.
    pub fn apply(self, outcome: CredentialPairingDriveOutcome) -> LivePairingOperationStep {
        match outcome {
            CredentialPairingDriveOutcome::CleanupCompleted => {
                LivePairingOperationStep::Progress(self)
            }
            CredentialPairingDriveOutcome::MutationPrepared(mutation)
            | CredentialPairingDriveOutcome::ReconcileRequired(mutation)
                if mutation == self.mutation =>
            {
                LivePairingOperationStep::Progress(self)
            }
            CredentialPairingDriveOutcome::Retry {
                mutation: Some(mutation),
                reason,
            } if mutation == self.mutation => LivePairingOperationStep::Retry {
                operation: self,
                reason,
            },
            CredentialPairingDriveOutcome::BeginOffered(offer)
                if self.mutation == PairingMutation::AddPending
                    && offer.bearer() == self.bearer =>
            {
                LivePairingOperationStep::Reply(LivePairingReply::new(
                    self.connection,
                    PairingResponse::Begin(BeginResponse::offered(self.sequence, offer)),
                ))
            }
            CredentialPairingDriveOutcome::Activated(confirmation)
                if self.mutation == PairingMutation::ActivatePending =>
            {
                LivePairingOperationStep::Reply(LivePairingReply::new(
                    self.connection,
                    PairingResponse::Activate(ActivateResponse::activated_after_durable_commit(
                        self.sequence,
                        confirmation,
                    )),
                ))
            }
            CredentialPairingDriveOutcome::Aborted
                if self.mutation == PairingMutation::AbortPending =>
            {
                LivePairingOperationStep::Reply(LivePairingReply::new(
                    self.connection,
                    PairingResponse::AbortCurrent(AbortCurrentResponse::new(
                        self.sequence,
                        AbortResult::Aborted,
                    )),
                ))
            }
            // Idle, mutation-free Retry, a mismatched typed result, or any
            // blocked result contradicts the admitted correlation. Secret
            // terminal owners are dropped here and therefore zeroized.
            _ => LivePairingOperationStep::Fault(blocked_reply(self)),
        }
    }
}

/// Node scheduling result after one drive of an admitted live mutation.
#[must_use = "live-pairing correlation, replies, and retry ownership must be handled"]
pub enum LivePairingOperationStep {
    /// A bounded nonterminal stage completed; drive the same owner again.
    Progress(LivePairingOperation),
    /// Physical ambiguity retained the exact owner for a delayed retry.
    Retry {
        /// Unchanged request/mutation correlation.
        operation: LivePairingOperation,
        /// Stable retry classification.
        reason: PairingDriveRetry,
    },
    /// Matching durable terminal result routed to the original connection.
    Reply(LivePairingReply),
    /// Internal mismatch collapsed to a coarse response; latch the lane closed.
    Fault(LivePairingReply),
}

fn blocked_reply(operation: LivePairingOperation) -> LivePairingReply {
    let response =
        match operation.mutation {
            PairingMutation::AddPending => PairingResponse::Begin(BeginResponse::failure(
                operation.sequence,
                PairingFailure::Blocked,
            )),
            PairingMutation::ActivatePending => PairingResponse::Activate(
                ActivateResponse::failure(operation.sequence, ActivateFailure::Blocked),
            ),
            PairingMutation::AbortPending => PairingResponse::AbortCurrent(
                AbortCurrentResponse::new(operation.sequence, AbortResult::Blocked),
            ),
        };
    LivePairingReply::new(operation.connection, response)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use reticulum_device_api_credentials::{CredentialGeneration, CredentialId};
    use reticulum_device_api_pairing::{
        AbortResult, ActivationConfirmation, BeginOffer, DeviceId, PairingPsk, PairingResponse,
    };
    use reticulum_device_api_pairing_policy::ConnectionId;

    use super::{LivePairingOperation, LivePairingOperationStep};
    use crate::credential_pairing::{
        CredentialPairingDriveOutcome, PairingDriveRetry, PairingMutation,
    };

    fn connection() -> ConnectionId {
        ConnectionId::new(7).expect("test connection is nonzero")
    }

    fn begin_offer() -> BeginOffer {
        BeginOffer::after_pending_commit(
            reticulum_device_api_pairing::BearerBinding::BleGatt,
            DeviceId::new([0x11; 16]).expect("test device ID is nonzero"),
            CredentialId::new([0x22; 16]),
            CredentialGeneration::new(3),
            PairingPsk::new([0x33; 32]).expect("test PSK is nonzero"),
        )
        .expect("test offer is valid")
    }

    #[test]
    fn multistep_begin_keeps_exact_correlation_until_durable_offer() {
        let operation = LivePairingOperation::new(
            reticulum_device_api_pairing::BearerBinding::BleGatt,
            connection(),
            41,
            PairingMutation::AddPending,
        );
        let operation = match operation.apply(CredentialPairingDriveOutcome::MutationPrepared(
            PairingMutation::AddPending,
        )) {
            LivePairingOperationStep::Progress(operation) => operation,
            _ => panic!("candidate preparation must retain correlation"),
        };
        let operation = match operation.apply(CredentialPairingDriveOutcome::ReconcileRequired(
            PairingMutation::AddPending,
        )) {
            LivePairingOperationStep::Progress(operation) => operation,
            _ => panic!("reconciliation must retain correlation"),
        };
        let operation = match operation.apply(CredentialPairingDriveOutcome::Retry {
            mutation: Some(PairingMutation::AddPending),
            reason: PairingDriveRetry::Backend,
        }) {
            LivePairingOperationStep::Retry { operation, reason } => {
                assert_eq!(reason, PairingDriveRetry::Backend);
                operation
            }
            _ => panic!("backend retry must retain correlation"),
        };

        let reply =
            match operation.apply(CredentialPairingDriveOutcome::BeginOffered(begin_offer())) {
                LivePairingOperationStep::Reply(reply) => reply,
                _ => panic!("durable offer must complete Begin"),
            };
        assert_eq!(reply.connection(), connection());
        assert_eq!(reply.response().sequence(), 41);
        assert!(matches!(reply.response(), PairingResponse::Begin(_)));
    }

    #[test]
    fn mismatched_or_premature_outcome_fails_closed_with_typed_response() {
        let activate = LivePairingOperation::new(
            reticulum_device_api_pairing::BearerBinding::BleGatt,
            connection(),
            52,
            PairingMutation::ActivatePending,
        );
        let reply = match activate.apply(CredentialPairingDriveOutcome::Aborted) {
            LivePairingOperationStep::Fault(reply) => reply,
            _ => panic!("mismatched terminal result must fault"),
        };
        assert_eq!(reply.connection(), connection());
        match reply.response() {
            PairingResponse::Activate(response) => {
                assert_eq!(response.sequence(), 52);
                assert_eq!(
                    response.failure_kind(),
                    Some(reticulum_device_api_pairing::ActivateFailure::Blocked)
                );
            }
            _ => panic!("fault reply must preserve the admitted request family"),
        }

        let abort = LivePairingOperation::new(
            reticulum_device_api_pairing::BearerBinding::BleGatt,
            connection(),
            53,
            PairingMutation::AbortPending,
        );
        assert!(matches!(
            abort.apply(CredentialPairingDriveOutcome::Idle),
            LivePairingOperationStep::Fault(_)
        ));
    }

    #[test]
    fn activation_and_abort_reply_only_to_matching_durable_terminals() {
        let credential_id = CredentialId::new([0x44; 16]);
        let generation = CredentialGeneration::new(5);
        let confirmation =
            ActivationConfirmation::from_bytes(credential_id, generation, [0x55; 32])
                .expect("test confirmation is nonzero and bound");
        let activate = LivePairingOperation::new(
            reticulum_device_api_pairing::BearerBinding::BleGatt,
            connection(),
            61,
            PairingMutation::ActivatePending,
        );
        let activate_reply =
            match activate.apply(CredentialPairingDriveOutcome::Activated(confirmation)) {
                LivePairingOperationStep::Reply(reply) => reply,
                _ => panic!("durable activation must complete Activate"),
            };
        assert_eq!(activate_reply.connection(), connection());
        match activate_reply.response() {
            PairingResponse::Activate(response) => {
                assert_eq!(response.sequence(), 61);
                let confirmation = response
                    .confirmation()
                    .expect("durable activation must carry confirmation");
                assert_eq!(confirmation.credential_id(), credential_id);
                assert_eq!(confirmation.generation(), generation);
            }
            _ => panic!("activation changed response family"),
        }

        let abort = LivePairingOperation::new(
            reticulum_device_api_pairing::BearerBinding::BleGatt,
            connection(),
            62,
            PairingMutation::AbortPending,
        );
        let abort_reply = match abort.apply(CredentialPairingDriveOutcome::Aborted) {
            LivePairingOperationStep::Reply(reply) => reply,
            _ => panic!("durable tombstone must complete AbortCurrent"),
        };
        assert_eq!(abort_reply.connection(), connection());
        match abort_reply.response() {
            PairingResponse::AbortCurrent(response) => {
                assert_eq!(response.sequence(), 62);
                assert_eq!(response.result(), AbortResult::Aborted);
            }
            _ => panic!("abort changed response family"),
        }
    }
}
