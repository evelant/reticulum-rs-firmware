use std::{
    collections::VecDeque,
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::*;

static NEXT_PATH: AtomicU64 = AtomicU64::new(0);
const BLE_PROOF_REQUEST_WIRE: &str = "0007524441310126010101010101010101010101010101010102010101010101010240020101010202020239101112131415161718191a1b1c1d1e1f0807060504030201404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0101010101010101010101010101010100";

struct InMemoryDuplex {
    inbound: VecDeque<u8>,
    written: Arc<Mutex<Vec<u8>>>,
}

impl Read for InMemoryDuplex {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.inbound.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "scripted inbound stream is empty",
            ));
        }
        let length = output.len().min(self.inbound.len());
        for slot in &mut output[..length] {
            *slot = self
                .inbound
                .pop_front()
                .expect("length is bounded by the inbound queue");
        }
        Ok(length)
    }
}

impl Write for InMemoryDuplex {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.written
            .lock()
            .expect("duplex write capture mutex must not be poisoned")
            .extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn ascending<const N: usize>(start: u8) -> [u8; N] {
    let mut bytes = [0_u8; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = start.wrapping_add(index as u8);
    }
    bytes
}

fn proof_exchange_wire(bearer: BearerBinding) -> Vec<u8> {
    let sequence = 1;
    let credential_id = CredentialId::new(ascending::<16>(0x10));
    let generation = CredentialGeneration::new(0x0102_0304_0506_0708);
    let challenge = reticulum_device_api_pairing::ProofChallenge::new(
        bearer,
        DeviceId::new([0x44; 16]).unwrap(),
        reticulum_device_api_pairing::ConnectionId::new(7).unwrap(),
        reticulum_device_api_pairing::WindowId::new(9).unwrap(),
        credential_id,
        generation,
        reticulum_device_api_pairing::DeviceChallenge::new([0x55; 32]).unwrap(),
    )
    .unwrap();
    let response = PairingResponse::ProofStart(ProofStartResponse::challenge(sequence, challenge));
    let response_frame = FramedRecord::encode(&response.into_record()).unwrap();
    let written = Arc::new(Mutex::new(Vec::new()));
    let stream = InMemoryDuplex {
        inbound: response_frame.encoded().iter().copied().collect(),
        written: Arc::clone(&written),
    };
    let mut session = PairingSession::from_stream(
        stream,
        "in-memory-duplex",
        bearer,
        sequence,
        Duration::from_secs(1),
    )
    .unwrap();
    let request = ProofStartRequest::new(
        bearer,
        sequence,
        credential_id,
        generation,
        ascending::<32>(0x40),
    )
    .unwrap();
    let response = exchange_pairing(
        &mut session,
        PairingRequest::ProofStart(request),
        ExpectedResponse::ProofStart,
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap_or_else(|error| panic!("{}", error.message));
    assert!(matches!(
        response,
        PairingResponse::ProofStart(ProofStartResponse::Challenge {
            challenge,
            ..
        }) if challenge.bearer() == bearer
    ));
    assert_eq!(session.bearer(), bearer);
    assert_eq!(session.endpoint_name(), "in-memory-duplex");
    assert_eq!(session.next_sequence(), Some(sequence + 1));
    written
        .lock()
        .expect("duplex write capture mutex must not be poisoned")
        .clone()
}

#[test]
fn ble_stream_session_preserves_the_independent_proof_request_vector() {
    assert_eq!(
        proof_exchange_wire(BearerBinding::BleGatt),
        hex::decode(BLE_PROOF_REQUEST_WIRE).unwrap()
    );
}

#[test]
fn ble_stream_session_emits_and_accepts_bearer_code_two() {
    let wire = proof_exchange_wire(BearerBinding::BleGatt);
    let mut decoder = StreamDecoder::new();
    let record = wire
        .into_iter()
        .find_map(|byte| match decoder.push(byte) {
            DecodeEvent::Record(record) => Some(record),
            DecodeEvent::Pending => None,
            DecodeEvent::MalformedCobs | DecodeEvent::MalformedRecord(_) => {
                panic!("pairing session emitted a malformed BLE record")
            }
            DecodeEvent::Overflow => panic!("pairing session emitted an overlong BLE record"),
        })
        .expect("one complete BLE pairing request");
    assert_eq!(record.payload()[6], BearerBinding::BleGatt.code());
    assert!(matches!(
        PairingRequest::from_record(BearerBinding::BleGatt, record),
        Ok(PairingRequest::ProofStart(request))
            if request.bearer() == BearerBinding::BleGatt
    ));
}

fn temporary_path() -> PathBuf {
    let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "reticulum-device-pairing-state-{}-{serial}.bin",
        std::process::id()
    ))
}

fn confirmed_abort() -> ConfirmedAbort {
    AbortSummary {
        sequence: 21,
        outcome: AbortOutcome::Aborted,
    }
    .into_confirmed_abort()
    .unwrap()
}

#[test]
fn response_family_matcher_is_exact() {
    let begin = PairingResponse::Begin(BeginResponse::failure(1, PairingFailure::Unavailable));
    assert!(response_matches(ExpectedResponse::Begin, &begin));
    assert!(!response_matches(ExpectedResponse::Activate, &begin));
}

#[test]
fn initialize_workflow_advances_exact_next_across_presence_and_retry_polling() {
    let mut scripted = VecDeque::from([
        (
            ControlRequest::status(10),
            ControlResponse::status(10, InitializationStatus::InitializationRequired),
        ),
        (
            ControlRequest::initialize(11),
            ControlResponse::initialize(11, InitializeResult::PhysicalPresenceRequired),
        ),
        (
            ControlRequest::status(12),
            ControlResponse::status(12, InitializationStatus::InitializationRequired),
        ),
        (
            ControlRequest::initialize(13),
            ControlResponse::initialize(13, InitializeResult::Retrying),
        ),
        (
            ControlRequest::status(14),
            ControlResponse::status(14, InitializationStatus::InFlight),
        ),
        (
            ControlRequest::status(15),
            ControlResponse::status(15, InitializationStatus::Completed),
        ),
    ]);
    let mut waits = Vec::new();
    let mut progress = Vec::new();
    let response = initialize_workflow_with(
        10,
        |request| {
            let (expected, response) = scripted.pop_front().expect("unexpected request");
            assert_eq!(request, expected);
            Ok(response)
        },
        |last_sequence| {
            waits.push(last_sequence);
            Ok(())
        },
        &mut |event| progress.push(event),
    )
    .unwrap();

    assert_eq!(
        response,
        ControlResponse::status(15, InitializationStatus::Completed)
    );
    assert_eq!(waits, [11, 13, 14]);
    assert_eq!(
        progress
            .iter()
            .filter(|event| {
                **event
                    == PairingProgress::WaitingForPhysicalPresence(PresenceOperation::Initialize)
            })
            .count(),
        1
    );
    assert!(progress.contains(&PairingProgress::Initialized));
    assert!(scripted.is_empty());
}

#[test]
fn sequence_space_refuses_the_firmware_exhaustion_sentinel() {
    assert_eq!(next_usable_sequence(u64::MAX - 1), None);
    assert_eq!(next_usable_sequence(u64::MAX - 2), Some(u64::MAX - 1));
    assert!(ensure_usable_sequence(u64::MAX).is_err());
}

#[test]
fn pair_and_resume_reserve_every_required_sequence_before_sending() {
    assert!(ensure_pair_headroom(u64::MAX - 3).is_ok());
    assert!(ensure_pair_headroom(u64::MAX - 2).is_err());
    assert!(ensure_pair_headroom(u64::MAX - 1).is_err());

    assert!(ensure_resume_headroom(u64::MAX - 2).is_ok());
    assert!(ensure_resume_headroom(u64::MAX - 1).is_err());
}

#[test]
fn proof_challenge_device_must_match_the_persisted_offer() {
    let expected = DeviceId::new([0x11; 16]).unwrap();
    let substituted = DeviceId::new([0x22; 16]).unwrap();
    assert!(validate_challenge_device(expected, expected).is_ok());
    assert!(validate_challenge_device(expected, substituted).is_err());
}

#[cfg(unix)]
#[test]
fn reserved_marker_is_owner_only_rejected_by_resume_and_discardable() {
    ensure_secure_persistence_host().unwrap();
    let path = temporary_path();
    let reservation = ReservedStateFile::reserve(&path).unwrap();
    let staging_path = reservation.staging_path.clone();
    let marker = fs::read(&path).unwrap();
    assert_eq!(marker.len(), STATE_FILE_LENGTH);
    assert_eq!(&marker[..8], &STATE_MAGIC);
    assert_eq!(marker[10], STATE_RESERVED);
    assert!(marker[11..].iter().all(|byte| *byte == 0));
    assert_eq!(
        reservation.file.metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        reservation
            .staging_file
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let error = PendingStateFile::open_pending(&path).err().unwrap();
    assert!(error.contains("only a reservation"));

    reservation.discard().unwrap();
    assert!(!path.exists());
    assert!(!staging_path.exists());
}

#[cfg(unix)]
#[test]
fn pending_state_round_trips_atomically_and_active_is_not_resumable() {
    let path = temporary_path();
    let device_id = DeviceId::new([0x11; 16]).unwrap();
    let credential_id = CredentialId::new([0x22; 16]);
    let generation = CredentialGeneration::new(7);
    let activated_generation = CredentialGeneration::new(11);
    let psk = PairingPsk::new([0x33; 32]).unwrap();
    let reservation = ReservedStateFile::reserve(&path).unwrap();
    let staging_path = reservation.staging_path.clone();
    let state = reservation
        .commit_pending(device_id, credential_id, generation, &psk)
        .unwrap();
    assert!(!staging_path.exists());
    let pending = fs::read(&path).unwrap();
    assert_eq!(pending.len(), STATE_FILE_LENGTH);
    assert_eq!(&pending[..8], &STATE_MAGIC);
    assert_eq!(&pending[8..10], &STATE_FORMAT_VERSION.to_le_bytes());
    assert_eq!(pending[10], STATE_PENDING);
    assert_eq!(&pending[16..32], device_id.as_bytes());
    assert_eq!(&pending[32..48], credential_id.as_bytes());
    assert_eq!(&pending[48..56], &generation.get().to_le_bytes());
    assert_eq!(&pending[56..88], psk.as_bytes());
    assert_eq!(
        state.file.metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    let error = load_activated_credential(&path).err().unwrap();
    assert!(error.to_string().contains("is not Active"));

    let persisted = PendingStateFile::open_pending(&path).unwrap();
    assert_eq!(persisted.device_id, device_id);
    assert_eq!(persisted.credential_id, credential_id);
    assert_eq!(persisted.generation, generation);
    assert_eq!(persisted.psk.as_bytes(), psk.as_bytes());
    drop(persisted);

    let pending_inode = fs::metadata(&path).unwrap().ino();
    assert_eq!(state.file.metadata().unwrap().ino(), pending_inode);
    let ambiguous = state
        .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
        .unwrap();
    assert_eq!(
        classify_credential_artifact(&path),
        CredentialArtifactClassification::ActivationAmbiguous
    );
    assert_eq!(fs::read(&path).unwrap()[10], STATE_ACTIVATION_AMBIGUOUS);
    let ambiguous_inode = fs::metadata(&path).unwrap().ino();
    assert_ne!(ambiguous_inode, pending_inode);
    let error = PendingStateFile::open_pending(&path).err().unwrap();
    assert!(error.contains("activation-ambiguous"));
    ambiguous
        .mark_active(device_id, credential_id, activated_generation, &psk)
        .unwrap();
    let active = fs::read(&path).unwrap();
    assert_eq!(active.len(), STATE_FILE_LENGTH);
    assert_eq!(active[10], STATE_ACTIVE);
    assert_eq!(&active[48..56], &activated_generation.get().to_le_bytes());
    assert_eq!(&active[56..88], psk.as_bytes());
    assert_ne!(fs::metadata(&path).unwrap().ino(), ambiguous_inode);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let activated = load_activated_credential(&path).unwrap();
    let (loaded_device, loaded_credential, loaded_generation, loaded_psk) = activated.into_parts();
    assert_eq!(loaded_device, device_id);
    assert_eq!(loaded_credential, credential_id);
    assert_eq!(loaded_generation, activated_generation);
    assert_eq!(loaded_psk.as_ref(), psk.as_bytes());
    let error = PendingStateFile::open_pending(&path).err().unwrap();
    assert!(error.contains("already Active"));
    fs::remove_file(path).unwrap();
}

#[cfg(unix)]
#[test]
fn hard_linked_pending_is_invalid_and_cannot_cross_the_activation_boundary() {
    let path = temporary_path();
    let alias = path.with_extension("pending-hard-link");
    let device_id = DeviceId::new([0x31; 16]).unwrap();
    let credential_id = CredentialId::new([0x32; 16]);
    let generation = CredentialGeneration::new(4);
    let psk = PairingPsk::new([0x33; 32]).unwrap();
    let pending = ReservedStateFile::reserve(&path)
        .unwrap()
        .commit_pending(device_id, credential_id, generation, &psk)
        .unwrap();
    fs::hard_link(&path, &alias).unwrap();

    assert_eq!(
        classify_credential_artifact(&path),
        CredentialArtifactClassification::Invalid
    );
    assert_eq!(
        classify_credential_artifact(&alias),
        CredentialArtifactClassification::Invalid
    );
    assert!(PendingStateFile::open_pending(&path).is_err());
    assert!(
        pending
            .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap()[10], STATE_PENDING);
    assert_eq!(fs::read(&alias).unwrap()[10], STATE_PENDING);

    fs::remove_file(path).unwrap();
    fs::remove_file(alias).unwrap();
}

#[cfg(unix)]
#[test]
fn activation_marker_is_conservative_and_definite_no_send_can_restore_pending() {
    let path = temporary_path();
    let device_id = DeviceId::new([0x41; 16]).unwrap();
    let credential_id = CredentialId::new([0x42; 16]);
    let generation = CredentialGeneration::new(9);
    let psk = PairingPsk::new([0x43; 32]).unwrap();
    let pending = ReservedStateFile::reserve(&path)
        .unwrap()
        .commit_pending(device_id, credential_id, generation, &psk)
        .unwrap();
    assert_eq!(
        classify_credential_artifact(&path),
        CredentialArtifactClassification::PendingResumeSafe
    );
    let ambiguous = pending
        .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
        .unwrap();
    assert_eq!(
        classify_credential_artifact(&path),
        CredentialArtifactClassification::ActivationAmbiguous
    );
    assert!(PendingStateFile::open_pending(&path).is_err());

    let pending = ambiguous
        .restore_pending(device_id, credential_id, generation, &psk)
        .unwrap();
    drop(pending);
    assert_eq!(
        classify_credential_artifact(&path),
        CredentialArtifactClassification::PendingResumeSafe
    );
    assert!(PendingStateFile::open_pending(&path).is_ok());
    fs::remove_file(path).unwrap();
}

#[cfg(unix)]
#[test]
fn confirmed_abort_cleanup_removes_only_reserved_or_resume_safe_pending() {
    for outcome in [
        AbortOutcome::PhysicalPresenceRequired,
        AbortOutcome::Refused,
        AbortOutcome::Blocked,
        AbortOutcome::Unavailable,
    ] {
        assert!(
            AbortSummary {
                sequence: 20,
                outcome,
            }
            .into_confirmed_abort()
            .is_none()
        );
    }

    let reserved_path = temporary_path();
    let reservation = ReservedStateFile::reserve(&reserved_path).unwrap();
    reservation.retain_ambiguous_begin_marker().unwrap();
    assert_eq!(
        classify_credential_artifact(&reserved_path),
        CredentialArtifactClassification::ReservedOrAmbiguousBegin
    );
    assert_eq!(
        cleanup_after_confirmed_abort(confirmed_abort(), &reserved_path).unwrap(),
        CleanedCredentialArtifact::ReservedOrAmbiguousBegin
    );
    assert_eq!(
        classify_credential_artifact(&reserved_path),
        CredentialArtifactClassification::Missing
    );

    let pending_path = temporary_path();
    let psk = PairingPsk::new([0x53; 32]).unwrap();
    let pending = ReservedStateFile::reserve(&pending_path)
        .unwrap()
        .commit_pending(
            DeviceId::new([0x51; 16]).unwrap(),
            CredentialId::new([0x52; 16]),
            CredentialGeneration::new(3),
            &psk,
        )
        .unwrap();
    drop(pending);
    assert_eq!(
        cleanup_after_confirmed_abort(confirmed_abort(), &pending_path).unwrap(),
        CleanedCredentialArtifact::PendingResumeSafe
    );
    assert_eq!(
        classify_credential_artifact(&pending_path),
        CredentialArtifactClassification::Missing
    );
}

#[cfg(unix)]
#[test]
fn confirmed_abort_cleanup_refuses_ambiguous_active_and_multiply_linked_files() {
    let ambiguous_path = temporary_path();
    let device_id = DeviceId::new([0x61; 16]).unwrap();
    let credential_id = CredentialId::new([0x62; 16]);
    let generation = CredentialGeneration::new(5);
    let psk = PairingPsk::new([0x63; 32]).unwrap();
    let ambiguous = ReservedStateFile::reserve(&ambiguous_path)
        .unwrap()
        .commit_pending(device_id, credential_id, generation, &psk)
        .unwrap()
        .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
        .unwrap();
    drop(ambiguous);
    assert!(cleanup_after_confirmed_abort(confirmed_abort(), &ambiguous_path).is_err());
    assert!(ambiguous_path.exists());
    fs::remove_file(&ambiguous_path).unwrap();

    let active_path = temporary_path();
    ReservedStateFile::reserve(&active_path)
        .unwrap()
        .commit_pending(device_id, credential_id, generation, &psk)
        .unwrap()
        .mark_activation_ambiguous(device_id, credential_id, generation, &psk)
        .unwrap()
        .mark_active(device_id, credential_id, CredentialGeneration::new(7), &psk)
        .unwrap();
    assert_eq!(
        classify_credential_artifact(&active_path),
        CredentialArtifactClassification::Active
    );
    assert!(cleanup_after_confirmed_abort(confirmed_abort(), &active_path).is_err());
    assert!(active_path.exists());
    fs::remove_file(&active_path).unwrap();

    let linked_path = temporary_path();
    let linked_alias = linked_path.with_extension("alias");
    let pending = ReservedStateFile::reserve(&linked_path)
        .unwrap()
        .commit_pending(device_id, credential_id, generation, &psk)
        .unwrap();
    drop(pending);
    fs::hard_link(&linked_path, &linked_alias).unwrap();
    assert!(cleanup_after_confirmed_abort(confirmed_abort(), &linked_path).is_err());
    assert!(linked_path.exists());
    fs::remove_file(linked_path).unwrap();
    fs::remove_file(linked_alias).unwrap();
}

#[cfg(unix)]
#[test]
fn resume_rejects_a_pending_file_with_broad_permissions() {
    let path = temporary_path();
    let psk = PairingPsk::new([3; 32]).unwrap();
    let state = ReservedStateFile::reserve(&path)
        .unwrap()
        .commit_pending(
            DeviceId::new([1; 16]).unwrap(),
            CredentialId::new([2; 16]),
            CredentialGeneration::new(1),
            &psk,
        )
        .unwrap();
    drop(state);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let error = PendingStateFile::open_pending(&path).err().unwrap();
    assert!(error.contains("owner-readable/writable"));
    assert_eq!(
        classify_credential_artifact(&path),
        CredentialArtifactClassification::Invalid
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(path).unwrap();
}

#[cfg(unix)]
#[test]
fn reservation_never_overwrites_existing_material() {
    let path = temporary_path();
    fs::write(&path, b"existing").unwrap();
    let result = ReservedStateFile::reserve(&path);
    assert!(result.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"existing");
    fs::remove_file(path).unwrap();
}

#[cfg(not(unix))]
#[test]
fn secret_persistence_is_rejected_before_touching_a_path() {
    let path = temporary_path();
    assert!(ReservedStateFile::reserve(&path).is_err());
    assert!(!path.exists());
}

#[test]
fn public_names_never_include_secret_material() {
    assert_eq!(
        pairing_failure_name(PairingFailure::PhysicalPresenceRequired),
        "physical-presence-required"
    );
    assert_eq!(AbortOutcome::Aborted.name(), "aborted");
}
