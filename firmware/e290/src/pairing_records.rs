//! Shared pre-authentication request demultiplexing for E290 API bearers.
//!
//! Kind routing happens before a decoded [`reticulum_device_api_framing::Record`]
//! is consumed by either codec. Sequence admission deliberately remains a
//! separate bearer-owner step and therefore runs only after this function has
//! returned a complete typed owner.

use reticulum_device_api_framing::Record;
use reticulum_device_api_pairing::{
    BearerBinding, PairingRequest, PairingResponse, RECORD_KIND_ABORT_CURRENT_REQUEST,
    RECORD_KIND_ACTIVATE_REQUEST, RECORD_KIND_BEGIN_REQUEST, RECORD_KIND_PROOF_START_REQUEST,
};
use reticulum_device_api_pairing_control::{
    ControlRequest, RECORD_KIND_INITIALIZE_REQUEST, RECORD_KIND_STATUS_REQUEST,
};

/// Exact request family and operation decoded from the shared bearer stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreAuthenticationRequestKind {
    /// Coarse credential-store status query.
    Status,
    /// Explicit empty-store initialization request.
    Initialize,
    /// Add one fresh Pending pairing credential.
    Begin,
    /// Start one possession-proof challenge.
    ProofStart,
    /// Continue one challenge into durable activation.
    Activate,
    /// Abort the device-selected current Pending credential.
    AbortCurrent,
}

impl PreAuthenticationRequestKind {
    /// Whether this request belongs to the secret-bearing live-pairing family.
    pub const fn is_live(self) -> bool {
        matches!(
            self,
            Self::Begin | Self::ProofStart | Self::Activate | Self::AbortCurrent
        )
    }

    /// Whether a live response has the exact family required by this request.
    pub const fn matches_live_response(self, response: &PairingResponse) -> bool {
        matches!(
            (self, response),
            (Self::Begin, PairingResponse::Begin(_))
                | (Self::ProofStart, PairingResponse::ProofStart(_))
                | (Self::Activate, PairingResponse::Activate(_))
                | (Self::AbortCurrent, PairingResponse::AbortCurrent(_))
        )
    }
}

/// One canonically decoded request from either pre-authentication family.
///
/// This aggregate deliberately implements neither `Clone`, `Copy`, nor
/// `Debug`: the live variant can own the only decoded client proof.
#[must_use = "a decoded pre-authentication request must be sequenced, transferred, or explicitly dropped"]
pub enum PreAuthenticationRequest {
    /// Copy-only status or explicit-initialization control request.
    Control(ControlRequest),
    /// Potentially secret-bearing live-pairing request.
    Live(PairingRequest),
}

impl PreAuthenticationRequest {
    /// Opaque shared-stream sequence.
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Control(request) => request.sequence(),
            Self::Live(request) => request.sequence(),
        }
    }

    /// Exact typed request operation.
    pub const fn kind(&self) -> PreAuthenticationRequestKind {
        match self {
            Self::Control(ControlRequest::Status { .. }) => PreAuthenticationRequestKind::Status,
            Self::Control(ControlRequest::Initialize { .. }) => {
                PreAuthenticationRequestKind::Initialize
            }
            Self::Live(PairingRequest::Begin(_)) => PreAuthenticationRequestKind::Begin,
            Self::Live(PairingRequest::ProofStart(_)) => PreAuthenticationRequestKind::ProofStart,
            Self::Live(PairingRequest::Activate(_)) => PreAuthenticationRequestKind::Activate,
            Self::Live(PairingRequest::AbortCurrent(_)) => {
                PreAuthenticationRequestKind::AbortCurrent
            }
        }
    }
}

/// Public reason one decoded framing record was not an accepted request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreAuthenticationDecodeError {
    /// Kind is a response kind, authenticated-family kind, or unknown extension.
    UnsupportedKind(u8),
    /// A selected initialization-control record was not canonical.
    InvalidControl,
    /// A selected live-pairing record was not canonical.
    InvalidLivePairing,
}

/// Decode the six shared pre-authentication request kinds under one exact
/// transport profile.
///
/// Initialization-control records are bearer independent. Secret-bearing live
/// pairing records authenticate the supplied bearer code as part of their
/// canonical wire profile and transcript.
pub fn decode_pre_authentication_request(
    bearer: BearerBinding,
    record: Record,
) -> Result<PreAuthenticationRequest, PreAuthenticationDecodeError> {
    let kind = record.kind();
    match kind {
        RECORD_KIND_STATUS_REQUEST | RECORD_KIND_INITIALIZE_REQUEST => {
            ControlRequest::from_record(record)
                .map(PreAuthenticationRequest::Control)
                .map_err(|_| PreAuthenticationDecodeError::InvalidControl)
        }
        RECORD_KIND_BEGIN_REQUEST
        | RECORD_KIND_PROOF_START_REQUEST
        | RECORD_KIND_ACTIVATE_REQUEST
        | RECORD_KIND_ABORT_CURRENT_REQUEST => PairingRequest::from_record(bearer, record)
            .map(PreAuthenticationRequest::Live)
            .map_err(|_| PreAuthenticationDecodeError::InvalidLivePairing),
        _ => Err(PreAuthenticationDecodeError::UnsupportedKind(kind)),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use reticulum_device_api_framing::{
        AUTH_TAG_LENGTH, PAYLOAD_CAPACITY, PayloadLength, Record, SESSION_ID_LENGTH,
    };
    use reticulum_device_api_pairing::{
        ActivateRequest, BearerBinding, BeginRequest, ClientProof, PairingRequest,
        ProofStartRequest, RECORD_KIND_BEGIN_RESPONSE,
    };
    use reticulum_device_api_pairing_control::{
        ControlRequest, ControlResponse, InitializationStatus,
    };
    use reticulum_device_api_pairing_policy::ConnectionId;

    use super::{
        PreAuthenticationDecodeError, PreAuthenticationRequest, PreAuthenticationRequestKind,
        decode_pre_authentication_request,
    };
    use crate::pairing_policy::{ExactNextSequenceGate, SequenceRefusal};

    fn connection() -> ConnectionId {
        ConnectionId::new(1).expect("test connection is nonzero")
    }

    fn decode_and_accept(record: Record, gate: &mut ExactNextSequenceGate) {
        let request = decode_pre_authentication_request(BearerBinding::BleGatt, record)
            .expect("test request must decode canonically");
        gate.accept(connection(), request.sequence())
            .expect("shared sequence must be exact-next");
    }

    #[test]
    fn control_then_live_and_live_then_control_share_one_sequence_space() {
        let mut first = ExactNextSequenceGate::new(connection());
        decode_and_accept(ControlRequest::status(0).into_record(), &mut first);
        decode_and_accept(
            PairingRequest::Begin(BeginRequest::new(1)).into_record(),
            &mut first,
        );
        assert_eq!(first.next_expected(), Some(2));

        let mut second = ExactNextSequenceGate::new(connection());
        decode_and_accept(
            PairingRequest::Begin(BeginRequest::new(0)).into_record(),
            &mut second,
        );
        decode_and_accept(ControlRequest::initialize(1).into_record(), &mut second);
        assert_eq!(second.next_expected(), Some(2));
    }

    #[test]
    fn duplicate_and_gap_rejection_cross_protocol_families() {
        let mut gate = ExactNextSequenceGate::new(connection());
        decode_and_accept(ControlRequest::status(0).into_record(), &mut gate);

        let duplicate = decode_pre_authentication_request(
            BearerBinding::BleGatt,
            PairingRequest::Begin(BeginRequest::new(0)).into_record(),
        )
        .expect("duplicate live request still decodes canonically");
        assert!(matches!(
            gate.accept(connection(), duplicate.sequence()),
            Err(SequenceRefusal::Duplicate { .. })
        ));
        let gap = decode_pre_authentication_request(
            BearerBinding::BleGatt,
            ControlRequest::status(2).into_record(),
        )
        .expect("gap control request still decodes canonically");
        assert!(matches!(
            gate.accept(connection(), gap.sequence()),
            Err(SequenceRefusal::Gap { .. })
        ));
        assert_eq!(gate.next_expected(), Some(1));
    }

    #[test]
    fn rejected_records_do_not_advance_the_callers_sequence_gate() {
        let mut gate = ExactNextSequenceGate::new(connection());
        let response = ControlResponse::status(0, InitializationStatus::Completed).into_record();
        assert!(matches!(
            decode_pre_authentication_request(BearerBinding::BleGatt, response),
            Err(PreAuthenticationDecodeError::UnsupportedKind(_))
        ));

        let mut payload = [0_u8; PAYLOAD_CAPACITY];
        payload[0] = 1;
        let malformed = Record::new(
            reticulum_device_api_pairing::RECORD_KIND_BEGIN_REQUEST,
            [0; SESSION_ID_LENGTH],
            0,
            PayloadLength::new(1).expect("one byte fits"),
            payload,
            [0; AUTH_TAG_LENGTH],
        );
        assert_eq!(
            decode_pre_authentication_request(BearerBinding::BleGatt, malformed).err(),
            Some(PreAuthenticationDecodeError::InvalidLivePairing)
        );
        let unknown = Record::new(
            0xfe,
            [0; SESSION_ID_LENGTH],
            0,
            PayloadLength::new(0).expect("empty payload fits"),
            [0; PAYLOAD_CAPACITY],
            [0; AUTH_TAG_LENGTH],
        );
        assert_eq!(
            decode_pre_authentication_request(BearerBinding::BleGatt, unknown).err(),
            Some(PreAuthenticationDecodeError::UnsupportedKind(0xfe))
        );
        let pairing_response = Record::new(
            RECORD_KIND_BEGIN_RESPONSE,
            [0; SESSION_ID_LENGTH],
            0,
            PayloadLength::new(1).expect("one byte fits"),
            [0; PAYLOAD_CAPACITY],
            [0; AUTH_TAG_LENGTH],
        );
        assert!(matches!(
            decode_pre_authentication_request(BearerBinding::BleGatt, pairing_response),
            Err(PreAuthenticationDecodeError::UnsupportedKind(_))
        ));
        assert_eq!(gate.next_expected(), Some(0));
        assert!(gate.accept(connection(), 0).is_ok());
    }

    #[test]
    fn activate_decode_retains_the_only_proof_owner_and_exact_kind() {
        let request = ActivateRequest::new(
            9,
            reticulum_device_api_credentials::CredentialId::new([0x22; 16]),
            reticulum_device_api_credentials::CredentialGeneration::new(4),
            ClientProof::from_bytes([0x77; 32]),
        )
        .expect("test activate request is valid");
        let decoded = decode_pre_authentication_request(
            BearerBinding::BleGatt,
            PairingRequest::Activate(request).into_record(),
        )
        .expect("activate record decodes");
        assert_eq!(decoded.kind(), PreAuthenticationRequestKind::Activate);
        match decoded {
            PreAuthenticationRequest::Live(PairingRequest::Activate(request)) => {
                assert_eq!(request.sequence(), 9);
                assert_eq!(request.proof().as_bytes(), &[0x77; 32]);
            }
            _ => panic!("activate owner changed family during demultiplexing"),
        }
    }

    #[test]
    fn ble_live_requests_are_decoded_only_under_the_ble_profile() {
        let request = ProofStartRequest::new(
            BearerBinding::BleGatt,
            0,
            reticulum_device_api_credentials::CredentialId::new([0x33; 16]),
            reticulum_device_api_credentials::CredentialGeneration::new(5),
            [0x44; 32],
        )
        .expect("test proof-start request is valid")
        .into_record();

        assert!(matches!(
            decode_pre_authentication_request(BearerBinding::BleGatt, request),
            Ok(PreAuthenticationRequest::Live(PairingRequest::ProofStart(
                _
            )))
        ));

        let wrong_bearer = ProofStartRequest::new(
            BearerBinding::BleGatt,
            0,
            reticulum_device_api_credentials::CredentialId::new([0x33; 16]),
            reticulum_device_api_credentials::CredentialGeneration::new(5),
            [0x44; 32],
        )
        .expect("test proof-start request is valid")
        .into_record();
        let (kind, session_id, sequence, length, mut payload, tag) = wrong_bearer.into_parts();
        payload[6] = 1;
        let wrong_bearer = Record::new(kind, session_id, sequence, length, payload, tag);
        assert_eq!(
            decode_pre_authentication_request(BearerBinding::BleGatt, wrong_bearer).err(),
            Some(PreAuthenticationDecodeError::InvalidLivePairing)
        );
    }
}
