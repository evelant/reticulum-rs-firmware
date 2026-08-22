//! Product-owned OTA protocol and transport-independent update coordinator.
//!
//! PRNS receives and verifies each ordinary Resource. This module owns only
//! the application protocol above that Resource: an enrolled management Link
//! opens a session, one bounded image chunk is armed at a time, verified bytes
//! are written to the inactive ESP slot, and the slot is activated only after
//! the complete manifest digest and ESP image checks pass.

use reticulum_device_api::{
    ESP_IMAGE_MAGIC, OTA_CHUNK_METADATA_BYTES, OTA_IMAGE_CHUNK_BYTES, OtaChunkMetadata, OtaFailure,
    OtaManifest, OtaPhase, OtaProtocolError, OtaSessionId, OtaSlot, OtaStatus,
};
use sha2::{Digest, Sha256};

use crate::partition_contract::OTA_0_LEN;
use crate::prns_node::OTA_RESOURCE_METADATA_BYTES;

pub use reticulum_device_api::{
    MIN_OTA_IMAGE_BYTES, OTA_NEXT_PATH, OTA_NEXT_REQUEST_BYTES, OTA_PROTOCOL_VERSION,
    OTA_REBOOT_PATH, OTA_REBOOT_REQUEST_VALUE, OTA_START_PATH, OTA_STATUS_PATH,
    OTA_STATUS_REQUEST_VALUE, OTA_STATUS_RESPONSE_MAX_BYTES, OTA_VERSION_BYTES, OtaVersion,
    decode_next_request, decode_start_request, decode_status_response, encode_next_request,
    encode_start_request, encode_status_response,
};

/// Largest image that fits either reviewed E290 OTA slot.
pub const MAX_OTA_IMAGE_BYTES: u32 = OTA_0_LEN;

const SESSION_DOMAIN: &[u8] = b"reticulum-e290-ota-session-v1";

const _: () = assert!(OTA_CHUNK_METADATA_BYTES <= OTA_RESOURCE_METADATA_BYTES);
const _: () = assert!(OTA_IMAGE_CHUNK_BYTES.is_multiple_of(4));

/// Board-owned inactive-slot operations used by the pure coordinator.
pub trait OtaBackend {
    /// Storage or boot-selection error retained for board diagnostics.
    type Error;

    /// Select and erase the inactive slot after validating its image bound.
    fn prepare_inactive(&mut self, image_bytes: u32) -> Result<OtaSlot, Self::Error>;

    /// Write one chunk and compare every programmed byte with flash readback.
    fn write_verified(
        &mut self,
        slot: OtaSlot,
        offset: u32,
        data: &[u8],
    ) -> Result<(), Self::Error>;

    /// Validate the complete staged ESP image structure before activation.
    fn validate_staged_image(&mut self, slot: OtaSlot, image_bytes: u32)
    -> Result<(), Self::Error>;

    /// Select the already-verified slot for the next boot.
    fn activate(&mut self, slot: OtaSlot) -> Result<(), Self::Error>;
}

struct ActiveSession {
    link: [u8; 16],
    id: OtaSessionId,
    manifest: OtaManifest,
    slot: OtaSlot,
    verified_bytes: u32,
    next_chunk: u32,
    resource_armed: bool,
    digest: Sha256,
}

/// Failure returned to the product owner while preserving backend detail.
#[derive(Debug, Eq, PartialEq)]
pub enum OtaCoordinatorError<E> {
    /// Another update is receiving or already armed.
    Busy,
    /// Session identity, Link binding, or expected chunk did not match.
    NotExpected,
    /// Application protocol bytes were malformed.
    Protocol(OtaProtocolError),
    /// Board flash or boot-selection operation failed.
    Backend(E),
}

/// One transport-independent update owner.
pub struct OtaCoordinator {
    active: Option<ActiveSession>,
    status: OtaStatus,
}

impl OtaCoordinator {
    /// Construct an idle coordinator.
    pub const fn new() -> Self {
        Self {
            active: None,
            status: OtaStatus::idle(),
        }
    }

    /// Current copyable application status.
    pub const fn status(&self) -> OtaStatus {
        self.status
    }

    /// Start a new transfer and erase only the backend-selected inactive slot.
    pub fn begin<B: OtaBackend>(
        &mut self,
        backend: &mut B,
        link: [u8; 16],
        now_millis: u64,
        manifest: OtaManifest,
    ) -> Result<OtaSessionId, OtaCoordinatorError<B::Error>> {
        if self.active.is_some() {
            return Err(OtaCoordinatorError::Busy);
        }
        if manifest.image_bytes() > MAX_OTA_IMAGE_BYTES {
            self.fail_without_active(manifest, OtaFailure::Protocol);
            return Err(OtaCoordinatorError::Protocol(
                OtaProtocolError::InvalidImageSize,
            ));
        }
        let id = derive_session_id(link, now_millis, manifest);
        let slot = match backend.prepare_inactive(manifest.image_bytes()) {
            Ok(slot) => slot,
            Err(error) => {
                self.fail_without_active(manifest, OtaFailure::Flash);
                return Err(OtaCoordinatorError::Backend(error));
            }
        };
        self.active = Some(ActiveSession {
            link,
            id,
            manifest,
            slot,
            verified_bytes: 0,
            next_chunk: 0,
            resource_armed: false,
            digest: Sha256::new(),
        });
        self.refresh_receiving_status();
        Ok(id)
    }

    /// Mark the exact next Resource admissible after PRNS applied its Link strategy.
    pub fn arm_next<E>(
        &mut self,
        link: [u8; 16],
        session: OtaSessionId,
        index: u32,
    ) -> Result<(), OtaCoordinatorError<E>> {
        let Some(active) = self.active.as_mut() else {
            return Err(OtaCoordinatorError::NotExpected);
        };
        if active.link != link || active.id != session || active.next_chunk != index {
            return Err(OtaCoordinatorError::NotExpected);
        }
        active.resource_armed = true;
        self.refresh_receiving_status();
        Ok(())
    }

    /// Close the application gate after a PRNS strategy command failed.
    pub fn disarm(&mut self) {
        if let Some(active) = self.active.as_mut() {
            active.resource_armed = false;
            self.refresh_receiving_status();
        }
    }

    /// Commit one already-verified ordinary PRNS Resource to the inactive slot.
    ///
    /// The application gate is consumed before validation or flash mutation.
    /// A subsequent Resource is not armed until the client sends the exact
    /// next control request after observing updated durable progress.
    pub fn ingest_resource<B: OtaBackend>(
        &mut self,
        backend: &mut B,
        link: [u8; 16],
        metadata: &[u8],
        data: &[u8],
    ) -> Result<OtaStatus, OtaCoordinatorError<B::Error>> {
        let decoded = match OtaChunkMetadata::decode(metadata) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.fail_active(OtaFailure::Protocol);
                return Err(OtaCoordinatorError::Protocol(error));
            }
        };
        let Some(active) = self.active.as_mut() else {
            return Err(OtaCoordinatorError::NotExpected);
        };
        let was_armed = core::mem::replace(&mut active.resource_armed, false);
        if active.link != link {
            self.fail_active(OtaFailure::WrongLink);
            return Err(OtaCoordinatorError::NotExpected);
        }
        if !was_armed
            || decoded.session() != active.id
            || decoded.index() != active.next_chunk
            || decoded.offset() != active.verified_bytes
        {
            self.fail_active(OtaFailure::UnexpectedChunk);
            return Err(OtaCoordinatorError::NotExpected);
        }
        let remaining = active
            .manifest
            .image_bytes()
            .saturating_sub(active.verified_bytes);
        let expected = remaining.min(OTA_IMAGE_CHUNK_BYTES as u32) as usize;
        if data.len() != expected || data.is_empty() {
            self.fail_active(OtaFailure::UnexpectedChunk);
            return Err(OtaCoordinatorError::NotExpected);
        }
        if active.verified_bytes == 0 && data[0] != ESP_IMAGE_MAGIC {
            self.fail_active(OtaFailure::InvalidEspImage);
            return Err(OtaCoordinatorError::NotExpected);
        }
        if let Err(error) = backend.write_verified(active.slot, active.verified_bytes, data) {
            self.fail_active(OtaFailure::Flash);
            return Err(OtaCoordinatorError::Backend(error));
        }
        active.digest.update(data);
        active.verified_bytes += data.len() as u32;
        active.next_chunk += 1;

        if active.verified_bytes != active.manifest.image_bytes() {
            self.refresh_receiving_status();
            return Ok(self.status);
        }

        let actual: [u8; 32] = active.digest.clone().finalize().into();
        if actual != active.manifest.image_sha256() {
            self.fail_active(OtaFailure::DigestMismatch);
            return Err(OtaCoordinatorError::NotExpected);
        }
        if let Err(error) =
            backend.validate_staged_image(active.slot, active.manifest.image_bytes())
        {
            self.fail_active(OtaFailure::ImageValidation);
            return Err(OtaCoordinatorError::Backend(error));
        }
        if let Err(error) = backend.activate(active.slot) {
            self.fail_active(OtaFailure::Activation);
            return Err(OtaCoordinatorError::Backend(error));
        }
        let completed = self.active.take().expect("the completed session is active");
        self.status = OtaStatus {
            phase: OtaPhase::Activated,
            session: Some(completed.id),
            slot: Some(completed.slot),
            version: Some(completed.manifest.version()),
            image_bytes: completed.manifest.image_bytes(),
            verified_bytes: completed.verified_bytes,
            next_chunk: completed.next_chunk,
            resource_armed: false,
            failure: None,
        };
        Ok(self.status)
    }

    /// Fail a transfer when its bound Reticulum Link closes before activation.
    pub fn link_closed(&mut self, link: [u8; 16]) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.link == link)
        {
            self.fail_active(OtaFailure::Interrupted);
        }
    }

    fn refresh_receiving_status(&mut self) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        self.status = OtaStatus {
            phase: OtaPhase::Receiving,
            session: Some(active.id),
            slot: Some(active.slot),
            version: Some(active.manifest.version()),
            image_bytes: active.manifest.image_bytes(),
            verified_bytes: active.verified_bytes,
            next_chunk: active.next_chunk,
            resource_armed: active.resource_armed,
            failure: None,
        };
    }

    fn fail_without_active(&mut self, manifest: OtaManifest, failure: OtaFailure) {
        self.status = OtaStatus {
            phase: OtaPhase::Failed,
            session: None,
            slot: None,
            version: Some(manifest.version()),
            image_bytes: manifest.image_bytes(),
            verified_bytes: 0,
            next_chunk: 0,
            resource_armed: false,
            failure: Some(failure),
        };
    }

    fn fail_active(&mut self, failure: OtaFailure) {
        let Some(active) = self.active.take() else {
            return;
        };
        self.status = OtaStatus {
            phase: OtaPhase::Failed,
            session: Some(active.id),
            slot: Some(active.slot),
            version: Some(active.manifest.version()),
            image_bytes: active.manifest.image_bytes(),
            verified_bytes: active.verified_bytes,
            next_chunk: active.next_chunk,
            resource_armed: false,
            failure: Some(failure),
        };
    }
}

impl Default for OtaCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

fn derive_session_id(link: [u8; 16], now_millis: u64, manifest: OtaManifest) -> OtaSessionId {
    let mut digest = Sha256::new();
    digest.update(SESSION_DOMAIN);
    digest.update(link);
    digest.update(now_millis.to_be_bytes());
    digest.update(manifest.image_bytes().to_be_bytes());
    digest.update(manifest.image_sha256());
    let digest = digest.finalize();
    let mut session = [0_u8; 16];
    session.copy_from_slice(&digest[..16]);
    OtaSessionId::new(session)
}

#[cfg(test)]
mod tests {
    use std::vec;
    use std::vec::Vec;

    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    enum BackendError {
        Write,
        Invalid,
    }

    struct MemoryBackend {
        bytes: Vec<u8>,
        prepared: bool,
        activated: bool,
        fail_write: bool,
    }

    impl MemoryBackend {
        fn new() -> Self {
            Self {
                bytes: vec![0xff; OTA_IMAGE_CHUNK_BYTES * 2],
                prepared: false,
                activated: false,
                fail_write: false,
            }
        }
    }

    impl OtaBackend for MemoryBackend {
        type Error = BackendError;

        fn prepare_inactive(&mut self, image_bytes: u32) -> Result<OtaSlot, Self::Error> {
            if image_bytes as usize > self.bytes.len() {
                return Err(BackendError::Invalid);
            }
            self.bytes.fill(0xff);
            self.prepared = true;
            Ok(OtaSlot::Ota1)
        }

        fn write_verified(
            &mut self,
            _slot: OtaSlot,
            offset: u32,
            data: &[u8],
        ) -> Result<(), Self::Error> {
            if self.fail_write {
                return Err(BackendError::Write);
            }
            let start = offset as usize;
            self.bytes[start..start + data.len()].copy_from_slice(data);
            if self.bytes[start..start + data.len()] != *data {
                return Err(BackendError::Write);
            }
            Ok(())
        }

        fn validate_staged_image(
            &mut self,
            _slot: OtaSlot,
            _image_bytes: u32,
        ) -> Result<(), Self::Error> {
            (self.bytes[0] == ESP_IMAGE_MAGIC)
                .then_some(())
                .ok_or(BackendError::Invalid)
        }

        fn activate(&mut self, _slot: OtaSlot) -> Result<(), Self::Error> {
            self.activated = true;
            Ok(())
        }
    }

    fn image(len: usize) -> Vec<u8> {
        let mut image = vec![0x5a; len];
        image[0] = ESP_IMAGE_MAGIC;
        image
    }

    fn manifest(image: &[u8]) -> OtaManifest {
        OtaManifest::new(
            image.len() as u32,
            Sha256::digest(image).into(),
            OtaVersion::try_from_bytes(b"0.2.0-test").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn protocol_round_trips_exact_start_next_and_chunk_values() {
        let image = image(128);
        let manifest = manifest(&image);
        let mut start = [0_u8; 96];
        let len = encode_start_request(manifest, &mut start).unwrap();
        assert_eq!(decode_start_request(&start[..len]), Ok(manifest));

        let session = OtaSessionId::new([0x31; 16]);
        let next = encode_next_request(session, 7);
        assert_eq!(decode_next_request(&next), Ok((session, 7)));

        let metadata = OtaChunkMetadata::new(session, 7, 49).encode();
        assert_eq!(
            OtaChunkMetadata::decode(&metadata),
            Ok(OtaChunkMetadata::new(session, 7, 49))
        );
        assert_eq!(
            OtaChunkMetadata::decode(&metadata[..metadata.len() - 1]),
            Err(OtaProtocolError::Malformed)
        );

        let status = OtaStatus {
            phase: OtaPhase::Receiving,
            session: Some(session),
            slot: Some(OtaSlot::Ota1),
            version: Some(manifest.version()),
            image_bytes: manifest.image_bytes(),
            verified_bytes: 49,
            next_chunk: 7,
            resource_armed: true,
            failure: None,
        };
        let mut response = [0_u8; OTA_STATUS_RESPONSE_MAX_BYTES];
        let len = encode_status_response(status, &mut response).unwrap();
        assert_eq!(decode_status_response(&response[..len]), Ok(status));
    }

    #[test]
    fn two_verified_resources_activate_only_after_the_complete_digest() {
        let image = image(OTA_IMAGE_CHUNK_BYTES + 37);
        let manifest = manifest(&image);
        let link = [0x41; 16];
        let mut backend = MemoryBackend::new();
        let mut coordinator = OtaCoordinator::new();
        let session = coordinator.begin(&mut backend, link, 42, manifest).unwrap();
        assert!(backend.prepared);
        assert!(!backend.activated);

        coordinator
            .arm_next::<BackendError>(link, session, 0)
            .unwrap();
        let first = OtaChunkMetadata::new(session, 0, 0).encode();
        let status = coordinator
            .ingest_resource(&mut backend, link, &first, &image[..OTA_IMAGE_CHUNK_BYTES])
            .unwrap();
        assert_eq!(status.phase, OtaPhase::Receiving);
        assert_eq!(status.verified_bytes, OTA_IMAGE_CHUNK_BYTES as u32);
        assert!(!status.resource_armed);
        assert!(!backend.activated);

        coordinator
            .arm_next::<BackendError>(link, session, 1)
            .unwrap();
        let second = OtaChunkMetadata::new(session, 1, OTA_IMAGE_CHUNK_BYTES as u32).encode();
        let status = coordinator
            .ingest_resource(&mut backend, link, &second, &image[OTA_IMAGE_CHUNK_BYTES..])
            .unwrap();
        assert_eq!(status.phase, OtaPhase::Activated);
        assert_eq!(status.verified_bytes, image.len() as u32);
        assert!(backend.activated);
        assert_eq!(&backend.bytes[..image.len()], image.as_slice());
    }

    #[test]
    fn wrong_link_and_wrong_digest_never_activate() {
        let image = image(128);
        let valid_manifest = manifest(&image);
        let mut wrong_digest = valid_manifest.image_sha256();
        wrong_digest[0] ^= 0xff;
        let wrong_manifest = OtaManifest::new(
            valid_manifest.image_bytes(),
            wrong_digest,
            valid_manifest.version(),
        )
        .unwrap();
        let link = [0x51; 16];
        let mut backend = MemoryBackend::new();
        let mut coordinator = OtaCoordinator::new();
        let session = coordinator
            .begin(&mut backend, link, 51, wrong_manifest)
            .unwrap();
        coordinator
            .arm_next::<BackendError>(link, session, 0)
            .unwrap();
        let metadata = OtaChunkMetadata::new(session, 0, 0).encode();
        assert_eq!(
            coordinator.ingest_resource(&mut backend, [0x52; 16], &metadata, &image),
            Err(OtaCoordinatorError::NotExpected)
        );
        assert_eq!(coordinator.status().failure, Some(OtaFailure::WrongLink));
        assert!(!backend.activated);

        let mut coordinator = OtaCoordinator::new();
        let session = coordinator
            .begin(&mut backend, link, 52, wrong_manifest)
            .unwrap();
        coordinator
            .arm_next::<BackendError>(link, session, 0)
            .unwrap();
        let metadata = OtaChunkMetadata::new(session, 0, 0).encode();
        assert_eq!(
            coordinator.ingest_resource(&mut backend, link, &metadata, &image),
            Err(OtaCoordinatorError::NotExpected)
        );
        assert_eq!(
            coordinator.status().failure,
            Some(OtaFailure::DigestMismatch)
        );
        assert!(!backend.activated);
    }

    #[test]
    fn flash_failure_and_link_interruption_are_terminal_without_activation() {
        let image = image(128);
        let manifest = manifest(&image);
        let link = [0x61; 16];
        let mut backend = MemoryBackend::new();
        backend.fail_write = true;
        let mut coordinator = OtaCoordinator::new();
        let session = coordinator.begin(&mut backend, link, 61, manifest).unwrap();
        coordinator
            .arm_next::<BackendError>(link, session, 0)
            .unwrap();
        let metadata = OtaChunkMetadata::new(session, 0, 0).encode();
        assert_eq!(
            coordinator.ingest_resource(&mut backend, link, &metadata, &image),
            Err(OtaCoordinatorError::Backend(BackendError::Write))
        );
        assert_eq!(coordinator.status().failure, Some(OtaFailure::Flash));
        assert!(!backend.activated);

        backend.fail_write = false;
        let mut coordinator = OtaCoordinator::new();
        coordinator.begin(&mut backend, link, 62, manifest).unwrap();
        coordinator.link_closed(link);
        assert_eq!(coordinator.status().failure, Some(OtaFailure::Interrupted));
        assert!(!backend.activated);
    }
}
