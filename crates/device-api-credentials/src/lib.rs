//! Fixed-capacity device-owned authority for local API credentials.
//!
//! This crate owns the persistent semantic vocabulary and one validated,
//! immutable boot snapshot. It is deliberately below session framing so the
//! raw-NOR credential store can depend on it without pulling in
//! handshake crypto, COBS, a bearer, or firmware. The canonical E290 semantic
//! snapshot codec is likewise independent of physical flash headers and commit
//! mechanics. The session layer consumes a zeroizing [`SelectedCredential`]
//! and later asks this authority to revalidate the authenticated credential ID
//! and generation.
//!
//! No principal or permission comes from client bytes. The sole credential
//! store owner must durably commit and validate a replacement
//! snapshot before swapping authorities between requests. This slice has no
//! in-place mutation API, physical flash format, pairing UI, physical-presence
//! policy, USB/BLE/Wi-Fi code, Reticulum identity, or radio capability.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::marker::PhantomData;

use reticulum_device_api::{DispatchContext, DispatchProvenance};
use subtle::{Choice, ConditionallySelectable, ConstantTimeEq};
use zeroize::Zeroizing;

pub use reticulum_device_api::{Permissions, PrincipalId};

mod snapshot;

pub use snapshot::{
    CREDENTIAL_SNAPSHOT_IMAGE_SIZE, CREDENTIAL_SNAPSHOT_SLOT_SIZE,
    CredentialSnapshotDecodeFaultKind, CredentialSnapshotImage, decode_e290_credential_snapshot,
    encode_e290_credential_snapshot,
};

/// Exact PSK size required by qualification-suite credentials.
pub const CREDENTIAL_PSK_LENGTH: usize = 32;
/// Initial E290 product ceiling for current and retained credential records.
pub const E290_CREDENTIAL_RECORD_CAPACITY: usize = 16;

/// Opaque 128-bit paired-client credential identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CredentialId([u8; 16]);

impl CredentialId {
    /// Construct an opaque credential identifier from canonical bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the canonical credential identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Nonzero globally allocated generation of one credential record.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CredentialGeneration(u64);

impl CredentialGeneration {
    /// Construct a generation for format decoding and validated snapshot load.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Raw nonzero generation value after validation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Nonzero high-water revision of the complete credential authority.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AuthorityRevision(u64);

impl AuthorityRevision {
    /// Construct a candidate revision for validated snapshot load.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Raw revision value.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Allocate the next non-wrapping revision for a future durable mutation.
    pub const fn next(self) -> Result<Self, AuthorityRevisionExhausted> {
        match self.0.checked_add(1) {
            Some(next) => Ok(Self(next)),
            None => Err(AuthorityRevisionExhausted),
        }
    }
}

/// The global credential-authority revision cannot advance without wrapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorityRevisionExhausted;

/// Stable version of the device-owned authorization policy applied at dispatch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AuthorizationPolicyVersion(u32);

impl AuthorizationPolicyVersion {
    /// Construct a policy version for validated snapshot load.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Raw nonzero policy version.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Lifecycle status of one durable credential record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialStatus {
    /// Secret is durably staged but possession has not been confirmed.
    Pending,
    /// Credential may authenticate and authorize requests.
    Active,
    /// Tombstone prevents reuse; no PSK is retained in the live snapshot.
    Revoked,
}

/// Device-observed origin of a credential enrollment ceremony.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingOrigin {
    /// Wired lab pairing under an exclusive physical-presence window.
    UsbPhysicalPresence,
    /// Future display/code/QR-confirmed local pairing ceremony.
    ConfirmedOutOfBand,
    /// Credential imported by an explicitly authorized recovery flow.
    RecoveryImport,
}

/// Bounded reason retained by a revoked-credential tombstone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevocationReason {
    /// User or administrator explicitly revoked the client.
    Explicit,
    /// Credential was replaced during safe two-credential rotation.
    Rotated,
    /// Device-wide credential reset invalidated the client.
    FactoryReset,
    /// Pending enrollment was abandoned or failed possession proof.
    PairingAborted,
}

/// Non-secret audit facts bound to one credential record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CredentialAudit {
    created_revision: AuthorityRevision,
    modified_revision: AuthorityRevision,
    pairing_origin: PairingOrigin,
    policy_version: AuthorizationPolicyVersion,
}

impl CredentialAudit {
    /// Construct audit facts decoded or assigned by the sole credential owner.
    pub const fn new(
        created_revision: AuthorityRevision,
        modified_revision: AuthorityRevision,
        pairing_origin: PairingOrigin,
        policy_version: AuthorizationPolicyVersion,
    ) -> Self {
        Self {
            created_revision,
            modified_revision,
            pairing_origin,
            policy_version,
        }
    }

    /// Revision at which this credential ID was first enrolled.
    pub const fn created_revision(self) -> AuthorityRevision {
        self.created_revision
    }

    /// Revision at which its current authorization state was committed.
    pub const fn modified_revision(self) -> AuthorityRevision {
        self.modified_revision
    }

    /// Enrollment ceremony that introduced this credential.
    pub const fn pairing_origin(self) -> PairingOrigin {
        self.pairing_origin
    }

    /// Authorization-policy version applied to this record.
    pub const fn policy_version(self) -> AuthorizationPolicyVersion {
        self.policy_version
    }
}

enum CredentialSecret {
    Present(Zeroizing<[u8; CREDENTIAL_PSK_LENGTH]>),
    Absent,
}

fn copy_zeroizing_psk(
    source: &[u8; CREDENTIAL_PSK_LENGTH],
) -> Zeroizing<[u8; CREDENTIAL_PSK_LENGTH]> {
    let mut copy = Zeroizing::new([0; CREDENTIAL_PSK_LENGTH]);
    copy.copy_from_slice(source);
    copy
}

/// One current or retired record loaded from device-owned durable state.
///
/// This owner deliberately implements neither `Clone`, `Copy`, nor `Debug`.
/// Dropping an active or pending record zeroizes its PSK. Revoked tombstones
/// retain metadata but contain no PSK.
///
/// ```compile_fail
/// use reticulum_device_api_credentials::CredentialRecord;
/// fn require_clone<T: Clone>() {}
/// require_clone::<CredentialRecord>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_credentials::CredentialRecord;
/// fn require_copy<T: Copy>() {}
/// require_copy::<CredentialRecord>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_credentials::CredentialRecord;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<CredentialRecord>();
/// ```
pub struct CredentialRecord {
    id: CredentialId,
    generation: CredentialGeneration,
    principal: PrincipalId,
    permissions: Permissions,
    status: CredentialStatus,
    audit: CredentialAudit,
    revocation_reason: Option<RevocationReason>,
    secret: CredentialSecret,
}

impl CredentialRecord {
    /// Construct a pending or active secret-bearing record.
    ///
    /// Canonical validity is checked when a [`CredentialAuthorityBuilder`]
    /// accepts this owner. The builder rejects `Revoked` because tombstones must
    /// be PSK-free and constructed with [`Self::revoked`].
    pub fn with_secret(
        id: CredentialId,
        generation: CredentialGeneration,
        principal: PrincipalId,
        permissions: Permissions,
        status: CredentialStatus,
        audit: CredentialAudit,
        psk: [u8; CREDENTIAL_PSK_LENGTH],
    ) -> Self {
        Self::with_zeroizing_secret(
            id,
            generation,
            principal,
            permissions,
            status,
            audit,
            Zeroizing::new(psk),
        )
    }

    fn with_zeroizing_secret(
        id: CredentialId,
        generation: CredentialGeneration,
        principal: PrincipalId,
        permissions: Permissions,
        status: CredentialStatus,
        audit: CredentialAudit,
        psk: Zeroizing<[u8; CREDENTIAL_PSK_LENGTH]>,
    ) -> Self {
        Self {
            id,
            generation,
            principal,
            permissions,
            status,
            audit,
            revocation_reason: None,
            secret: CredentialSecret::Present(psk),
        }
    }

    /// Construct a PSK-free revoked tombstone.
    pub fn revoked(
        id: CredentialId,
        generation: CredentialGeneration,
        principal: PrincipalId,
        audit: CredentialAudit,
        reason: RevocationReason,
    ) -> Self {
        Self {
            id,
            generation,
            principal,
            permissions: Permissions::NONE,
            status: CredentialStatus::Revoked,
            audit,
            revocation_reason: Some(reason),
            secret: CredentialSecret::Absent,
        }
    }

    /// Opaque client credential identifier.
    pub const fn id(&self) -> CredentialId {
        self.id
    }

    /// Global generation authenticated by a session transcript.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }

    /// Device-owned principal associated with this credential.
    pub const fn principal(&self) -> PrincipalId {
        self.principal
    }

    /// Device-owned permissions in the current record.
    pub const fn permissions(&self) -> Permissions {
        self.permissions
    }

    /// Current lifecycle status.
    pub const fn status(&self) -> CredentialStatus {
        self.status
    }

    /// Non-secret audit metadata.
    pub const fn audit(&self) -> CredentialAudit {
        self.audit
    }

    /// Revocation reason present only on a canonical tombstone.
    pub const fn revocation_reason(&self) -> Option<RevocationReason> {
        self.revocation_reason
    }

    fn psk(&self) -> Option<&[u8; CREDENTIAL_PSK_LENGTH]> {
        match &self.secret {
            CredentialSecret::Present(psk) => Some(psk),
            CredentialSecret::Absent => None,
        }
    }

    fn exactly_matches(&self, other: &Self) -> bool {
        let secret_matches = match (self.psk(), other.psk()) {
            (Some(left), Some(right)) => bool::from(left.ct_eq(right)),
            (None, None) => true,
            _ => false,
        };
        self.id == other.id
            && self.generation == other.generation
            && self.principal == other.principal
            && self.permissions == other.permissions
            && self.status == other.status
            && self.audit == other.audit
            && self.revocation_reason == other.revocation_reason
            && secret_matches
    }

    fn duplicate_for_candidate(&self) -> Self {
        Self {
            id: self.id,
            generation: self.generation,
            principal: self.principal,
            permissions: self.permissions,
            status: self.status,
            audit: self.audit,
            revocation_reason: self.revocation_reason,
            secret: match self.psk() {
                Some(psk) => CredentialSecret::Present(copy_zeroizing_psk(psk)),
                None => CredentialSecret::Absent,
            },
        }
    }
}

/// Zeroizing credential material selected for one handshake attempt.
///
/// This owner deliberately implements neither `Clone`, `Copy`, nor `Debug`.
///
/// ```compile_fail
/// use reticulum_device_api_credentials::SelectedCredential;
/// fn require_clone<T: Clone>() {}
/// require_clone::<SelectedCredential>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_credentials::SelectedCredential;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<SelectedCredential>();
/// ```
pub struct SelectedCredential {
    id: CredentialId,
    generation: CredentialGeneration,
    psk: Zeroizing<[u8; CREDENTIAL_PSK_LENGTH]>,
}

impl SelectedCredential {
    /// Consume the selection into exact session-owned authentication facts.
    pub fn into_parts(
        self,
    ) -> (
        CredentialId,
        CredentialGeneration,
        Zeroizing<[u8; CREDENTIAL_PSK_LENGTH]>,
    ) {
        (self.id, self.generation, self.psk)
    }
}

/// Exact identity and generation of the sole pending enrollment.
///
/// This is a checked reference, not an authorization capability. Every proof,
/// activation, or abort operation revalidates it against the current immutable
/// authority before using it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PendingCredentialRef {
    id: CredentialId,
    generation: CredentialGeneration,
}

impl PendingCredentialRef {
    /// Construct an untrusted candidate reference for exact authority lookup.
    pub const fn new(id: CredentialId, generation: CredentialGeneration) -> Self {
        Self { id, generation }
    }

    /// Opaque pending credential identifier.
    pub const fn id(self) -> CredentialId {
        self.id
    }

    /// Exact pending generation that a proof must bind.
    pub const fn generation(self) -> CredentialGeneration {
        self.generation
    }
}

/// Device-selected facts and secret for one new pending enrollment.
///
/// The pairing manager supplies already-decided device policy facts and fresh
/// entropy. This owner deliberately implements neither `Clone`, `Copy`, nor
/// `Debug`; rejection returns the exact owner so the caller may retry or drop
/// it for zeroization.
///
/// ```compile_fail
/// use reticulum_device_api_credentials::NewPendingCredential;
/// fn require_clone<T: Clone>() {}
/// require_clone::<NewPendingCredential>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_credentials::NewPendingCredential;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<NewPendingCredential>();
/// ```
pub struct NewPendingCredential {
    id: CredentialId,
    principal: PrincipalId,
    permissions: Permissions,
    pairing_origin: PairingOrigin,
    policy_version: AuthorizationPolicyVersion,
    psk: Zeroizing<[u8; CREDENTIAL_PSK_LENGTH]>,
}

impl NewPendingCredential {
    /// Own one caller-generated enrollment proposal.
    ///
    /// Canonical values, uniqueness, capacity, and the one-pending rule are
    /// checked by [`CredentialAuthority::plan_add_pending`]. Generation and
    /// audit revisions are never caller supplied; the authority allocates the
    /// exact next revision itself. The PSK is accepted only as an existing
    /// zeroizing owner so caller-side early returns cannot leave an unprotected
    /// secret copy behind.
    pub fn new(
        id: CredentialId,
        principal: PrincipalId,
        permissions: Permissions,
        pairing_origin: PairingOrigin,
        policy_version: AuthorizationPolicyVersion,
        psk: Zeroizing<[u8; CREDENTIAL_PSK_LENGTH]>,
    ) -> Self {
        Self {
            id,
            principal,
            permissions,
            pairing_origin,
            policy_version,
            psk,
        }
    }

    /// Proposed never-before-retained credential identifier.
    pub const fn id(&self) -> CredentialId {
        self.id
    }

    /// Recover every proposal field, including the zeroizing PSK owner.
    pub fn into_parts(
        self,
    ) -> (
        CredentialId,
        PrincipalId,
        Permissions,
        PairingOrigin,
        AuthorizationPolicyVersion,
        Zeroizing<[u8; CREDENTIAL_PSK_LENGTH]>,
    ) {
        (
            self.id,
            self.principal,
            self.permissions,
            self.pairing_origin,
            self.policy_version,
            self.psk,
        )
    }
}

/// Zeroizing PSK selected from the exact sole pending enrollment.
///
/// This owner deliberately implements neither `Clone`, `Copy`, nor `Debug`.
/// It decides no proof transcript or HMAC construction.
///
/// ```compile_fail
/// use reticulum_device_api_credentials::SelectedPendingCredential;
/// fn require_clone<T: Clone>() {}
/// require_clone::<SelectedPendingCredential>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_credentials::SelectedPendingCredential;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<SelectedPendingCredential>();
/// ```
pub struct SelectedPendingCredential {
    pending: PendingCredentialRef,
    psk: Zeroizing<[u8; CREDENTIAL_PSK_LENGTH]>,
}

impl SelectedPendingCredential {
    /// Exact pending record selected for an external proof implementation.
    pub const fn pending(&self) -> PendingCredentialRef {
        self.pending
    }

    /// Consume the selection into its checked reference and zeroizing PSK.
    pub fn into_parts(self) -> (PendingCredentialRef, Zeroizing<[u8; CREDENTIAL_PSK_LENGTH]>) {
        (self.pending, self.psk)
    }
}

/// The authority contains more than one pending enrollment and is ineligible
/// for pairing lifecycle work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultiplePendingCredentials;

/// Exact pending proof material is unavailable.
///
/// Missing, non-pending, wrong-generation, and multiple-pending states share
/// one result so this selection cannot become a credential-enumeration oracle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingCredentialUnavailable;

/// Why a checked pairing lifecycle candidate could not be constructed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialLifecycleFaultKind {
    /// The global authority revision cannot advance without wrapping.
    AuthorityRevisionExhausted,
    /// No record slot remains for another lifetime credential ID.
    CapacityExhausted,
    /// The proposed all-zero credential ID is reserved.
    ZeroCredentialId,
    /// A retained record already owns the proposed credential ID.
    CredentialIdAlreadyRetained,
    /// The proposed all-zero principal is reserved.
    ZeroPrincipal,
    /// Authorization policy version zero is reserved.
    ZeroPolicyVersion,
    /// The proposed all-zero PSK is forbidden.
    ZeroPsk,
    /// A retained secret-bearing record already owns the proposed PSK.
    DuplicatePsk,
    /// One pending enrollment already occupies the lifecycle slot.
    PendingAlreadyExists,
    /// The authority contains multiple pending records and fails closed.
    MultiplePendingCredentials,
    /// The requested exact pending ID and generation are not current.
    PendingCredentialUnavailable,
    /// The internally built candidate failed the structural successor defense.
    StructuralSuccessorRejected,
}

/// Rejected new-pending plan retaining the exact secret-bearing proposal.
///
/// This value deliberately implements no `Debug`; diagnostics use
/// [`Self::kind`] without formatting or copying retained enrollment material.
#[must_use = "a rejected pending enrollment must be recovered or explicitly dropped"]
pub struct NewPendingCredentialFault {
    kind: CredentialLifecycleFaultKind,
    enrollment: NewPendingCredential,
}

impl NewPendingCredentialFault {
    /// Non-secret rejection category.
    pub const fn kind(&self) -> CredentialLifecycleFaultKind {
        self.kind
    }

    /// Recover the exact rejected enrollment owner.
    pub fn into_enrollment(self) -> NewPendingCredential {
        self.enrollment
    }
}

/// Rejected activation or abort lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingCredentialTransitionFault {
    kind: CredentialLifecycleFaultKind,
}

impl PendingCredentialTransitionFault {
    /// Non-secret rejection category.
    pub const fn kind(self) -> CredentialLifecycleFaultKind {
        self.kind
    }
}

/// Canonical reason a record or snapshot could not be loaded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialLoadFaultKind {
    /// Authority high-water revision is zero.
    ZeroAuthorityRevision,
    /// The fixed snapshot has no unoccupied slot.
    CapacityExhausted,
    /// Another retained record already owns the same credential ID.
    DuplicateCredentialId,
    /// Another record already uses this globally allocated generation.
    DuplicateGeneration,
    /// Another secret-bearing record already uses the same PSK.
    DuplicatePsk,
    /// The all-zero credential ID is reserved for erased state.
    ZeroCredentialId,
    /// Generation zero is reserved for erased state.
    ZeroGeneration,
    /// The all-zero principal is reserved for erased state.
    ZeroPrincipal,
    /// The all-zero PSK is forbidden as an obviously invalid credential.
    ZeroPsk,
    /// Active or pending state did not carry exactly one PSK.
    MissingSecret,
    /// A revoked record retained a PSK or lacked its reason.
    InvalidRevokedRecord,
    /// A secret-bearing record used the revoked status.
    RevokedStatusWithSecret,
    /// Record generation or audit revisions are inconsistent or exceed the snapshot.
    InvalidRevisionOrder,
    /// Authorization policy version zero is reserved for erased state.
    ZeroPolicyVersion,
}

/// Rejected load retaining the exact secret-bearing record when one was supplied.
///
/// This value deliberately implements no `Debug`; diagnostics use [`Self::kind`]
/// without formatting retained credential state.
#[must_use = "a rejected credential record must be recovered or explicitly dropped"]
pub struct CredentialLoadFault {
    kind: CredentialLoadFaultKind,
    record: Option<CredentialRecord>,
}

impl CredentialLoadFault {
    /// Non-secret failure category.
    pub const fn kind(&self) -> CredentialLoadFaultKind {
        self.kind
    }

    /// Recover the rejected credential owner, when failure occurred on insert.
    pub fn into_record(self) -> Option<CredentialRecord> {
        self.record
    }
}

/// Boot-only builder for one validated immutable authority snapshot.
///
/// Insertion consumes the builder. If any record is rejected, all records
/// already accepted into that builder are dropped and their secrets are
/// zeroized; safe code cannot finish and publish a valid prefix.
///
/// ```compile_fail
/// use reticulum_device_api_credentials::{CredentialAuthorityBuilder, CredentialRecord};
/// fn ignore_rejection<const N: usize>(
///     builder: CredentialAuthorityBuilder<N>,
///     record: CredentialRecord,
/// ) {
///     let _rejected = builder.insert(record);
///     let _partial = builder.finish();
/// }
/// ```
#[must_use = "a credential snapshot builder must be finished or consumed by a rejected insert"]
pub struct CredentialAuthorityBuilder<const CAPACITY: usize> {
    revision: AuthorityRevision,
    records: [Option<CredentialRecord>; CAPACITY],
    count: usize,
}

impl<const CAPACITY: usize> CredentialAuthorityBuilder<CAPACITY> {
    /// Start an empty snapshot at one nonzero high-water revision.
    pub fn new(revision: AuthorityRevision) -> Result<Self, CredentialLoadFault> {
        if revision.get() == 0 {
            return Err(CredentialLoadFault {
                kind: CredentialLoadFaultKind::ZeroAuthorityRevision,
                record: None,
            });
        }
        Ok(Self {
            revision,
            records: core::array::from_fn(|_| None),
            count: 0,
        })
    }

    /// Insert one canonical record, retaining the rejected owner on error.
    ///
    /// This method consumes and returns the builder on success. On failure the
    /// partial builder is dropped, so callers cannot ignore corruption and
    /// expose credentials loaded before the rejected record.
    pub fn insert(mut self, record: CredentialRecord) -> Result<Self, CredentialLoadFault> {
        if let Some(kind) = self.validate_record(&record) {
            return Err(CredentialLoadFault {
                kind,
                record: Some(record),
            });
        }

        let slot = self
            .records
            .iter_mut()
            .find(|slot| slot.is_none())
            .expect("validated capacity guarantees one empty credential slot");
        *slot = Some(record);
        self.count += 1;
        Ok(self)
    }

    /// Freeze this completely validated snapshot for request service.
    pub fn finish(self) -> CredentialAuthority<CAPACITY> {
        CredentialAuthority {
            revision: self.revision,
            records: self.records,
            count: self.count,
        }
    }

    fn validate_record(&self, record: &CredentialRecord) -> Option<CredentialLoadFaultKind> {
        if self.count == CAPACITY {
            return Some(CredentialLoadFaultKind::CapacityExhausted);
        }
        if record.id.as_bytes().iter().all(|byte| *byte == 0) {
            return Some(CredentialLoadFaultKind::ZeroCredentialId);
        }
        if record.generation.get() == 0 {
            return Some(CredentialLoadFaultKind::ZeroGeneration);
        }
        if record.principal.0.iter().all(|byte| *byte == 0) {
            return Some(CredentialLoadFaultKind::ZeroPrincipal);
        }
        if record.audit.policy_version.get() == 0 {
            return Some(CredentialLoadFaultKind::ZeroPolicyVersion);
        }
        let created = record.audit.created_revision.get();
        let modified = record.audit.modified_revision.get();
        if created == 0
            || modified == 0
            || created > modified
            || modified != record.generation.get()
            || modified > self.revision.get()
        {
            return Some(CredentialLoadFaultKind::InvalidRevisionOrder);
        }
        if self
            .records
            .iter()
            .flatten()
            .any(|existing| existing.id == record.id)
        {
            return Some(CredentialLoadFaultKind::DuplicateCredentialId);
        }
        if self
            .records
            .iter()
            .flatten()
            .any(|existing| existing.generation == record.generation)
        {
            return Some(CredentialLoadFaultKind::DuplicateGeneration);
        }
        if let Some(psk) = record.psk()
            && self
                .records
                .iter()
                .flatten()
                .filter_map(CredentialRecord::psk)
                .any(|existing| bool::from(existing.ct_eq(psk)))
        {
            return Some(CredentialLoadFaultKind::DuplicatePsk);
        }

        match (record.status, record.psk(), record.revocation_reason) {
            (CredentialStatus::Pending | CredentialStatus::Active, Some(psk), None) => {
                if psk.iter().all(|byte| *byte == 0) {
                    Some(CredentialLoadFaultKind::ZeroPsk)
                } else {
                    None
                }
            }
            (CredentialStatus::Pending | CredentialStatus::Active, None, _) => {
                Some(CredentialLoadFaultKind::MissingSecret)
            }
            (CredentialStatus::Revoked, Some(_), _) => {
                Some(CredentialLoadFaultKind::RevokedStatusWithSecret)
            }
            (CredentialStatus::Revoked, None, Some(_)) => None,
            (CredentialStatus::Revoked, None, None) => {
                Some(CredentialLoadFaultKind::InvalidRevokedRecord)
            }
            (_, Some(_), Some(_)) => Some(CredentialLoadFaultKind::InvalidRevokedRecord),
        }
    }
}

/// Failure to select authentication material without revealing record status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CredentialUnavailable;

/// Generic failure to revalidate a session-minted credential reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CredentialRejected;

/// Immutable borrow proving current device-owned authorization facts.
///
/// The lease is neither `Clone` nor `Copy`. Its borrow freezes this authority
/// through the synchronous logical adapter dispatch: mutation or replacement
/// of the same authority value requires exclusive access and cannot interleave.
/// Product integration must still invoke dispatch inside the callback rather
/// than hold a lease across an await; immediacy is a sole-owner contract, not a
/// Rust auto-trait guarantee.
///
/// ```compile_fail
/// use reticulum_device_api::DispatchContext;
/// use reticulum_device_api_credentials::DispatchLease;
/// fn escape<const N: usize>(lease: &DispatchLease<'_, N>) -> DispatchContext {
///     lease.with_dispatch_context(|context| *context)
/// }
/// ```
pub struct DispatchLease<'authority, const CAPACITY: usize> {
    context: DispatchContext,
    _authority: PhantomData<&'authority CredentialAuthority<CAPACITY>>,
}

impl<const CAPACITY: usize> DispatchLease<'_, CAPACITY> {
    /// Run one immediate synchronous operation with the revalidated context.
    ///
    /// The higher-ranked borrow prevents the exact context value from escaping
    /// in the return value. [`DispatchContext`] remains a trusted logical API
    /// value with a public constructor, so linked product code can deliberately
    /// reconstruct equivalent scalar facts. The sole-owner integration must
    /// treat this callback as the dispatch boundary; this method is not an
    /// unforgeable authorization capability against arbitrary linked Rust code.
    pub fn with_dispatch_context<R>(
        &self,
        operation: impl for<'context> FnOnce(&'context DispatchContext) -> R,
    ) -> R {
        operation(&self.context)
    }

    /// Credential that authorized this dispatch attempt.
    pub const fn credential_id(&self) -> CredentialId {
        CredentialId::new(self.provenance().credential_id())
    }

    /// Exact active credential generation revalidated for this attempt.
    pub const fn generation(&self) -> CredentialGeneration {
        CredentialGeneration::new(self.provenance().credential_generation())
    }

    /// Complete authority high-water revision observed by this lease.
    pub const fn authority_revision(&self) -> AuthorityRevision {
        AuthorityRevision::new(self.provenance().authority_revision())
    }

    /// Authorization-policy version applied by this credential record.
    pub const fn policy_version(&self) -> AuthorizationPolicyVersion {
        AuthorizationPolicyVersion::new(self.provenance().policy_version())
    }

    const fn provenance(&self) -> DispatchProvenance {
        match self.context.provenance() {
            Some(provenance) => provenance,
            None => panic!("a credential authority minted an unauthenticated dispatch lease"),
        }
    }
}

/// Immutable fixed-capacity device-owned credential authority.
///
/// This owner is intentionally neither `Clone` nor `Copy`. Replace it only
/// between requests after the sole persistent credential owner has committed
/// and validated a new snapshot.
pub struct CredentialAuthority<const CAPACITY: usize> {
    revision: AuthorityRevision,
    records: [Option<CredentialRecord>; CAPACITY],
    count: usize,
}

/// Why a boot-validated snapshot is not a monotonic successor to a live authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialSuccessorFaultKind {
    /// The candidate authority revision is not exactly the next global value.
    AuthorityRevisionNotNext,
    /// Authorization-relevant record state changed at an old generation.
    ChangedWithoutGenerationAdvance,
    /// A retained credential disappeared instead of becoming a tombstone.
    CredentialRemovedWithoutTombstone,
    /// More than one record changed in one globally serialized mutation.
    MultipleRecordMutations,
    /// The authority revision advanced without changing exactly one record.
    NoRecordMutation,
    /// An existing credential changed immutable enrollment audit facts.
    ExistingCredentialAuditChanged,
    /// A revoked credential tombstone was modified or resurrected.
    RevokedCredentialChanged,
    /// A newly introduced credential did not originate at the new revision.
    NewCredentialRevisionInvalid,
}

/// Rejected successor retaining the complete secret-bearing candidate owner.
///
/// This value deliberately implements no `Debug`; diagnostics use
/// [`Self::kind`] without formatting credential state.
#[must_use = "a rejected successor must be recovered or explicitly dropped"]
pub struct CredentialSuccessorFault<const CAPACITY: usize> {
    kind: CredentialSuccessorFaultKind,
    candidate: CredentialAuthority<CAPACITY>,
}

impl<const CAPACITY: usize> CredentialSuccessorFault<CAPACITY> {
    /// Non-secret rejection category.
    pub const fn kind(&self) -> CredentialSuccessorFaultKind {
        self.kind
    }

    /// Recover the complete rejected candidate snapshot.
    pub fn into_candidate(self) -> CredentialAuthority<CAPACITY> {
        self.candidate
    }
}

/// One live-authority successor structurally validated as one global mutation.
///
/// The plan borrows the current authority so it cannot be replaced while a
/// sole storage owner durably commits `candidate`. Calling
/// [`Self::publish_after_commit`] is a public caller assertion that this durable
/// commit and readback have completed successfully; the type system cannot
/// verify that physical fact or restrict the method to one downstream crate.
#[must_use = "a validated credential successor must be durably committed or dropped"]
pub struct PlannedCredentialSuccessor<'current, const CAPACITY: usize> {
    candidate: CredentialAuthority<CAPACITY>,
    _current: PhantomData<&'current CredentialAuthority<CAPACITY>>,
}

impl<const CAPACITY: usize> PlannedCredentialSuccessor<'_, CAPACITY> {
    /// Borrow the complete candidate for the canonical durable encoder.
    pub const fn candidate(&self) -> &CredentialAuthority<CAPACITY> {
        &self.candidate
    }

    /// Cancel publication and recover the still-unpublished candidate owner.
    ///
    /// A sole storage owner uses this path after a canceled, failed, or
    /// ambiguous physical commit. Unlike [`Self::publish_after_commit`], this
    /// method makes no durability assertion and must not make the recovered
    /// authority live.
    pub fn into_unpublished_candidate(self) -> CredentialAuthority<CAPACITY> {
        self.candidate
    }

    /// Publish the candidate only after its durable commit and readback.
    ///
    /// The portable semantic crate cannot itself prove a physical flash commit;
    /// this public method trusts its caller. Restricting that assertion to the
    /// sole credential-store owner is a composition and review obligation, not
    /// an unforgeable Rust capability.
    pub fn publish_after_commit(self) -> CredentialAuthority<CAPACITY> {
        self.candidate
    }
}

/// Lifecycle operation authorized by a checked pairing candidate plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingLifecycleTransition {
    /// A fresh, never-retained ID becomes the sole `Pending` enrollment.
    AddPending,
    /// The exact sole `Pending` enrollment becomes `Active`.
    ActivatePending,
    /// The exact sole `Pending` enrollment becomes a PSK-free aborted tombstone.
    AbortPending,
}

/// Opaque pairing-lifecycle successor authorized above the structural plan.
///
/// This distinct owner lets the sole credential-store integration accept only
/// a lifecycle-authorized candidate. The typed composition path intentionally
/// does not expose the candidate by shared reference; the supported product
/// path selects pending proof material from the newly mounted, publishable
/// authority after physical commit, readback, and predecessor retirement, and
/// the repository source guard confines the unchecked bridges to the semantic
/// authority and sole physical store. Erase-only inactive cleanup may remain.
#[must_use = "a pairing lifecycle successor must be durably committed or dropped"]
pub struct PlannedPairingLifecycleSuccessor<'current, const CAPACITY: usize> {
    transition: PairingLifecycleTransition,
    expected_pending: Option<ExpectedPendingCredential>,
    plan: PlannedCredentialSuccessor<'current, CAPACITY>,
}

impl<const CAPACITY: usize> PlannedPairingLifecycleSuccessor<'_, CAPACITY> {
    /// Authorized lifecycle operation represented by this candidate.
    pub const fn transition(&self) -> PairingLifecycleTransition {
        self.transition
    }

    /// Exact next revision of the unpublished candidate.
    pub const fn candidate_revision(&self) -> AuthorityRevision {
        self.plan.candidate.revision
    }

    /// Consume the authorization into the exact candidate for the sole store.
    ///
    /// The returned authority remains unpublished. Product composition must
    /// give it directly to the sole durable store owner and must not select or
    /// offer a pending PSK from it.
    pub fn into_store_candidate(self) -> PairingLifecycleStoreCandidate<CAPACITY> {
        PairingLifecycleStoreCandidate {
            transition: self.transition,
            expected_pending: self.expected_pending,
            candidate: self.plan.into_unpublished_candidate(),
        }
    }
}

/// Opaque owned pairing candidate awaiting the sole durable store transition.
///
/// Keeping the lifecycle discriminator attached lets store composition expose
/// a typed commit path instead of accepting an indistinguishable bare semantic
/// authority. This owner intentionally provides no shared authority borrow and
/// neither `Clone` nor `Debug`.
///
/// ```compile_fail
/// use reticulum_device_api_credentials::PairingLifecycleStoreCandidate;
/// fn require_clone<T: Clone>() {}
/// require_clone::<PairingLifecycleStoreCandidate<16>>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_credentials::PairingLifecycleStoreCandidate;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<PairingLifecycleStoreCandidate<16>>();
/// ```
#[must_use = "a pairing store candidate must be committed, reconciled, or dropped"]
pub struct PairingLifecycleStoreCandidate<const CAPACITY: usize> {
    transition: PairingLifecycleTransition,
    expected_pending: Option<ExpectedPendingCredential>,
    candidate: CredentialAuthority<CAPACITY>,
}

struct ExpectedPendingCredential {
    pending: PendingCredentialRef,
    record: CredentialRecord,
}

impl ExpectedPendingCredential {
    fn from_record(record: &CredentialRecord) -> Self {
        Self {
            pending: PendingCredentialRef::new(record.id, record.generation),
            record: record.duplicate_for_candidate(),
        }
    }

    fn exactly_matches(&self, record: &CredentialRecord) -> bool {
        self.pending == PendingCredentialRef::new(record.id, record.generation)
            && self.record.exactly_matches(record)
    }
}

/// Lifecycle-candidate preflight rejection category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingLifecycleStoreCandidateFaultKind {
    /// The candidate is not one exact structural successor.
    Structural(CredentialSuccessorFaultKind),
    /// The structural successor does not implement its declared pairing
    /// lifecycle transition against this predecessor.
    TransitionMismatch,
}

/// Preflight rejection retaining the opaque lifecycle candidate.
///
/// This value deliberately implements no `Debug`; diagnostics use
/// [`Self::kind`] without formatting credential state. Recovering the rejected
/// owner yields another [`PairingLifecycleStoreCandidate`], never a bare
/// authority, so callers cannot accidentally discard its lifecycle
/// authorization while handling a pre-I/O semantic failure.
#[must_use = "a rejected pairing store candidate must be recovered or explicitly dropped"]
pub struct PairingLifecycleStoreCandidateFault<const CAPACITY: usize> {
    kind: PairingLifecycleStoreCandidateFaultKind,
    candidate: PairingLifecycleStoreCandidate<CAPACITY>,
}

impl<const CAPACITY: usize> PairingLifecycleStoreCandidateFault<CAPACITY> {
    /// Non-secret lifecycle-candidate rejection category.
    pub const fn kind(&self) -> PairingLifecycleStoreCandidateFaultKind {
        self.kind
    }

    /// Recover the exact opaque lifecycle candidate for retry or disposal.
    pub fn into_candidate(self) -> PairingLifecycleStoreCandidate<CAPACITY> {
        self.candidate
    }
}

impl<const CAPACITY: usize> PairingLifecycleStoreCandidate<CAPACITY> {
    /// Authorized lifecycle operation retained with this candidate.
    pub const fn transition(&self) -> PairingLifecycleTransition {
        self.transition
    }

    /// Exact unpublished revision awaiting durable store commit.
    pub const fn revision(&self) -> AuthorityRevision {
        self.candidate.revision
    }

    /// Preflight this lifecycle candidate against a live authority.
    ///
    /// Generic successor validation runs first. A structurally valid candidate
    /// must then implement the exact Add, Activate, or Abort delta named by its
    /// lifecycle discriminator. Both failure paths retain the discriminator and
    /// complete secret owner without reconstructing a bare candidate.
    #[allow(
        clippy::result_large_err,
        reason = "allocation-free rejection must retain the opaque candidate and its exact zeroizing source binding"
    )]
    pub fn validate_against(
        self,
        current: &CredentialAuthority<CAPACITY>,
    ) -> Result<Self, PairingLifecycleStoreCandidateFault<CAPACITY>> {
        let Self {
            transition,
            expected_pending,
            candidate,
        } = self;
        match current.plan_successor(candidate) {
            Ok(plan)
                if pairing_transition_matches(
                    transition,
                    expected_pending.as_ref(),
                    current,
                    plan.candidate(),
                ) =>
            {
                Ok(Self {
                    transition,
                    expected_pending,
                    candidate: plan.into_unpublished_candidate(),
                })
            }
            Ok(plan) => Err(PairingLifecycleStoreCandidateFault {
                kind: PairingLifecycleStoreCandidateFaultKind::TransitionMismatch,
                candidate: Self {
                    transition,
                    expected_pending,
                    candidate: plan.into_unpublished_candidate(),
                },
            }),
            Err(fault) => {
                let kind = PairingLifecycleStoreCandidateFaultKind::Structural(fault.kind());
                Err(PairingLifecycleStoreCandidateFault {
                    kind,
                    candidate: Self {
                        transition,
                        expected_pending,
                        candidate: fault.into_candidate(),
                    },
                })
            }
        }
    }

    fn into_unpublished_authority_internal(self) -> CredentialAuthority<CAPACITY> {
        self.candidate
    }
}

/// Narrow integration bridges reserved for the sole credential-store crate.
///
/// These extension traits are public only because Rust crates are separate
/// compilation units. They are hidden from generated documentation, sealed
/// against external implementations, and deliberately use conspicuous
/// `for_store_unchecked` names. Product composition must not import them.
#[doc(hidden)]
pub mod credential_store_integration {
    use super::{
        CredentialAuthority, PairingLifecycleStoreCandidate, PendingCredentialRef,
        PendingCredentialUnavailable, SelectedPendingCredential,
    };

    mod sealed {
        pub trait Sealed {}

        impl<const CAPACITY: usize> Sealed for super::CredentialAuthority<CAPACITY> {}
        impl<const CAPACITY: usize> Sealed for super::PairingLifecycleStoreCandidate<CAPACITY> {}
    }

    /// Store-only extraction of a preflighted unpublished lifecycle authority.
    pub trait PairingLifecycleStoreCandidateIntegration<const CAPACITY: usize>:
        sealed::Sealed
    {
        /// Consume the opaque owner after store preflight.
        fn into_unpublished_authority_for_store_unchecked(self) -> CredentialAuthority<CAPACITY>;
    }

    impl<const CAPACITY: usize> PairingLifecycleStoreCandidateIntegration<CAPACITY>
        for PairingLifecycleStoreCandidate<CAPACITY>
    {
        fn into_unpublished_authority_for_store_unchecked(self) -> CredentialAuthority<CAPACITY> {
            self.into_unpublished_authority_internal()
        }
    }

    /// Store-only pending-secret selection after publication eligibility.
    pub trait CredentialAuthorityStoreIntegration<const CAPACITY: usize>: sealed::Sealed {
        /// Select proof material only after the store has established that this
        /// bare authority is the publishable mounted successor.
        fn select_pending_for_proof_for_store_unchecked(
            &self,
            pending: PendingCredentialRef,
        ) -> Result<SelectedPendingCredential, PendingCredentialUnavailable>;
    }

    impl<const CAPACITY: usize> CredentialAuthorityStoreIntegration<CAPACITY>
        for CredentialAuthority<CAPACITY>
    {
        fn select_pending_for_proof_for_store_unchecked(
            &self,
            pending: PendingCredentialRef,
        ) -> Result<SelectedPendingCredential, PendingCredentialUnavailable> {
            self.select_pending_for_proof_internal(pending)
        }
    }
}

fn pairing_transition_matches<const CAPACITY: usize>(
    transition: PairingLifecycleTransition,
    expected_pending: Option<&ExpectedPendingCredential>,
    current: &CredentialAuthority<CAPACITY>,
    candidate: &CredentialAuthority<CAPACITY>,
) -> bool {
    match transition {
        PairingLifecycleTransition::AddPending => {
            expected_pending.is_none() && add_pending_delta_matches(current, candidate)
        }
        PairingLifecycleTransition::ActivatePending => {
            activate_pending_delta_matches(expected_pending, current, candidate)
        }
        PairingLifecycleTransition::AbortPending => {
            abort_pending_delta_matches(expected_pending, current, candidate)
        }
    }
}

fn add_pending_delta_matches<const CAPACITY: usize>(
    current: &CredentialAuthority<CAPACITY>,
    candidate: &CredentialAuthority<CAPACITY>,
) -> bool {
    if !matches!(current.sole_pending_record(), Ok(None))
        || current.count.checked_add(1) != Some(candidate.count)
    {
        return false;
    }
    let pending = match candidate.sole_pending_record() {
        Ok(Some(pending)) => pending,
        Ok(None) | Err(_) => return false,
    };
    if current.retains_id(pending.id) {
        return false;
    }
    current.records.iter().flatten().all(|record| {
        candidate
            .find_by_id(record.id)
            .is_some_and(|next| record.exactly_matches(next))
    })
}

fn activate_pending_delta_matches<const CAPACITY: usize>(
    expected_pending: Option<&ExpectedPendingCredential>,
    current: &CredentialAuthority<CAPACITY>,
    candidate: &CredentialAuthority<CAPACITY>,
) -> bool {
    let pending = match current.sole_pending_record() {
        Ok(Some(pending)) => pending,
        Ok(None) | Err(_) => return false,
    };
    if !expected_pending.is_some_and(|expected| expected.exactly_matches(pending))
        || !matches!(candidate.sole_pending_record(), Ok(None))
        || candidate.count != current.count
    {
        return false;
    }
    let Some(activated) = candidate.find_by_id(pending.id) else {
        return false;
    };
    active_transition_record_matches(pending, activated)
        && all_other_records_match(current, candidate, pending.id)
}

fn abort_pending_delta_matches<const CAPACITY: usize>(
    expected_pending: Option<&ExpectedPendingCredential>,
    current: &CredentialAuthority<CAPACITY>,
    candidate: &CredentialAuthority<CAPACITY>,
) -> bool {
    let pending = match current.sole_pending_record() {
        Ok(Some(pending)) => pending,
        Ok(None) | Err(_) => return false,
    };
    if !expected_pending.is_some_and(|expected| expected.exactly_matches(pending))
        || !matches!(candidate.sole_pending_record(), Ok(None))
        || candidate.count != current.count
    {
        return false;
    }
    let Some(aborted) = candidate.find_by_id(pending.id) else {
        return false;
    };
    aborted_transition_record_matches(pending, aborted)
        && all_other_records_match(current, candidate, pending.id)
}

fn active_transition_record_matches(pending: &CredentialRecord, active: &CredentialRecord) -> bool {
    pending.id == active.id
        && pending.status == CredentialStatus::Pending
        && active.status == CredentialStatus::Active
        && active.generation.get() > pending.generation.get()
        && pending.principal == active.principal
        && pending.permissions == active.permissions
        && audit_immutable_facts_match(pending.audit, active.audit)
        && active.audit.modified_revision.get() > pending.audit.modified_revision.get()
        && pending.revocation_reason.is_none()
        && active.revocation_reason.is_none()
        && secret_matches(pending, active)
}

fn aborted_transition_record_matches(
    pending: &CredentialRecord,
    aborted: &CredentialRecord,
) -> bool {
    pending.id == aborted.id
        && pending.status == CredentialStatus::Pending
        && aborted.status == CredentialStatus::Revoked
        && aborted.generation.get() > pending.generation.get()
        && pending.principal == aborted.principal
        && aborted.permissions == Permissions::NONE
        && audit_immutable_facts_match(pending.audit, aborted.audit)
        && aborted.audit.modified_revision.get() > pending.audit.modified_revision.get()
        && pending.revocation_reason.is_none()
        && aborted.revocation_reason == Some(RevocationReason::PairingAborted)
        && aborted.psk().is_none()
}

fn audit_immutable_facts_match(current: CredentialAudit, candidate: CredentialAudit) -> bool {
    current.created_revision == candidate.created_revision
        && current.pairing_origin == candidate.pairing_origin
        && current.policy_version == candidate.policy_version
}

fn all_other_records_match<const CAPACITY: usize>(
    current: &CredentialAuthority<CAPACITY>,
    candidate: &CredentialAuthority<CAPACITY>,
    target: CredentialId,
) -> bool {
    current
        .records
        .iter()
        .flatten()
        .filter(|record| record.id != target)
        .all(|record| {
            candidate
                .find_by_id(record.id)
                .is_some_and(|next| record.exactly_matches(next))
        })
}

fn secret_matches(left: &CredentialRecord, right: &CredentialRecord) -> bool {
    match (left.psk(), right.psk()) {
        (Some(left), Some(right)) => bool::from(left.ct_eq(right)),
        (None, None) => true,
        _ => false,
    }
}

/// Internal-RAM ceiling for the initial 16-record E290 authority snapshot.
pub const E290_CREDENTIAL_AUTHORITY_RAM_CEILING: usize = 2_048;

const _: () = assert!(
    core::mem::size_of::<CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>>()
        <= E290_CREDENTIAL_AUTHORITY_RAM_CEILING
);

impl<const CAPACITY: usize> CredentialAuthority<CAPACITY> {
    /// Complete authority high-water revision.
    pub const fn revision(&self) -> AuthorityRevision {
        self.revision
    }

    /// Number of current and retained records in this snapshot.
    pub const fn record_count(&self) -> usize {
        self.count
    }

    /// Number of records currently permitted to authenticate.
    pub fn active_count(&self) -> usize {
        self.records
            .iter()
            .flatten()
            .filter(|record| record.status == CredentialStatus::Active)
            .count()
    }

    /// Test whether any current or tombstoned record retains this ID.
    ///
    /// Every fixed table slot is inspected with a constant-time ID comparison.
    /// This is intended for fresh-ID collision rejection; a revoked ID remains
    /// retained and can never be reused.
    pub fn retains_id(&self, id: CredentialId) -> bool {
        let mut retained = Choice::from(0);
        for record in self.records.iter().flatten() {
            retained |= record.id.as_bytes().ct_eq(id.as_bytes());
        }
        bool::from(retained)
    }

    /// Return the exact sole pending enrollment, if one exists.
    ///
    /// More than one pending record is a fail-closed authority state rather
    /// than an arbitrary selection.
    pub fn pending_credential(
        &self,
    ) -> Result<Option<PendingCredentialRef>, MultiplePendingCredentials> {
        self.sole_pending_record().map(|record| {
            record.map(|record| PendingCredentialRef {
                id: record.id,
                generation: record.generation,
            })
        })
    }

    fn select_pending_for_proof_internal(
        &self,
        pending: PendingCredentialRef,
    ) -> Result<SelectedPendingCredential, PendingCredentialUnavailable> {
        let mut selected = 0_u64;
        let mut found = Choice::from(0);
        let mut pending_count = 0_usize;
        for (index, slot) in self.records.iter().enumerate() {
            let Some(record) = slot else {
                continue;
            };
            let is_pending = Choice::from(u8::from(record.status == CredentialStatus::Pending));
            pending_count += usize::from(bool::from(is_pending));
            let id_matches = record.id.as_bytes().ct_eq(pending.id.as_bytes());
            let generation_matches = record.generation.get().ct_eq(&pending.generation.get());
            let choose = is_pending & id_matches & generation_matches & !found;
            let index =
                u64::try_from(index).expect("a fixed credential table index always fits in u64");
            selected = u64::conditional_select(&selected, &index, choose);
            found |= is_pending & id_matches & generation_matches;
        }
        if pending_count != 1 || !bool::from(found) {
            return Err(PendingCredentialUnavailable);
        }
        let selected =
            usize::try_from(selected).expect("selected pending index originated as usize");
        let record = self.records[selected]
            .as_ref()
            .ok_or(PendingCredentialUnavailable)?;
        let psk = record.psk().ok_or(PendingCredentialUnavailable)?;
        Ok(SelectedPendingCredential {
            pending,
            psk: copy_zeroizing_psk(psk),
        })
    }

    /// Construct the exact next complete authority with one new `Pending`.
    ///
    /// Existing records, including every private PSK, are duplicated only
    /// inside this crate into the unpublished complete candidate. The caller
    /// supplies trusted device policy facts and fresh entropy; this method
    /// allocates the generation and both audit revisions from the exact next
    /// authority revision, enforces the lifetime-ID and one-pending rules, and
    /// finally routes the candidate through [`Self::plan_successor`].
    pub fn plan_add_pending(
        &self,
        enrollment: NewPendingCredential,
    ) -> Result<PlannedPairingLifecycleSuccessor<'_, CAPACITY>, NewPendingCredentialFault> {
        match self.sole_pending_record() {
            Ok(Some(_)) => {
                return Err(new_pending_fault(
                    CredentialLifecycleFaultKind::PendingAlreadyExists,
                    enrollment,
                ));
            }
            Err(_) => {
                return Err(new_pending_fault(
                    CredentialLifecycleFaultKind::MultiplePendingCredentials,
                    enrollment,
                ));
            }
            Ok(None) => {}
        }
        let next_revision = match self.revision.next() {
            Ok(revision) => revision,
            Err(_) => {
                return Err(new_pending_fault(
                    CredentialLifecycleFaultKind::AuthorityRevisionExhausted,
                    enrollment,
                ));
            }
        };
        if self.count == CAPACITY {
            return Err(new_pending_fault(
                CredentialLifecycleFaultKind::CapacityExhausted,
                enrollment,
            ));
        }
        if enrollment.id.as_bytes().iter().all(|byte| *byte == 0) {
            return Err(new_pending_fault(
                CredentialLifecycleFaultKind::ZeroCredentialId,
                enrollment,
            ));
        }
        if self.retains_id(enrollment.id) {
            return Err(new_pending_fault(
                CredentialLifecycleFaultKind::CredentialIdAlreadyRetained,
                enrollment,
            ));
        }
        if enrollment.principal.0.iter().all(|byte| *byte == 0) {
            return Err(new_pending_fault(
                CredentialLifecycleFaultKind::ZeroPrincipal,
                enrollment,
            ));
        }
        if enrollment.policy_version.get() == 0 {
            return Err(new_pending_fault(
                CredentialLifecycleFaultKind::ZeroPolicyVersion,
                enrollment,
            ));
        }
        if enrollment.psk.iter().all(|byte| *byte == 0) {
            return Err(new_pending_fault(
                CredentialLifecycleFaultKind::ZeroPsk,
                enrollment,
            ));
        }
        let mut duplicate_psk = Choice::from(0);
        for record in self.records.iter().flatten() {
            if let Some(psk) = record.psk() {
                duplicate_psk |= psk.ct_eq(&*enrollment.psk);
            }
        }
        if bool::from(duplicate_psk) {
            return Err(new_pending_fault(
                CredentialLifecycleFaultKind::DuplicatePsk,
                enrollment,
            ));
        }

        let mut candidate = self.duplicate_with_revision(next_revision);
        let target = candidate
            .records
            .iter_mut()
            .find(|record| record.is_none())
            .expect("capacity preflight proved one candidate slot is empty");
        *target = Some(CredentialRecord::with_zeroizing_secret(
            enrollment.id,
            CredentialGeneration::new(next_revision.get()),
            enrollment.principal,
            enrollment.permissions,
            CredentialStatus::Pending,
            CredentialAudit::new(
                next_revision,
                next_revision,
                enrollment.pairing_origin,
                enrollment.policy_version,
            ),
            copy_zeroizing_psk(&enrollment.psk),
        ));
        candidate.count += 1;

        match self.plan_successor(candidate) {
            Ok(plan) => Ok(PlannedPairingLifecycleSuccessor {
                transition: PairingLifecycleTransition::AddPending,
                expected_pending: None,
                plan,
            }),
            Err(fault) => {
                drop(fault);
                Err(new_pending_fault(
                    CredentialLifecycleFaultKind::StructuralSuccessorRejected,
                    enrollment,
                ))
            }
        }
    }

    /// Construct the exact next complete authority changing one `Pending` to
    /// `Active` while preserving its PSK and all device-owned policy facts.
    pub fn plan_activate_pending(
        &self,
        pending: PendingCredentialRef,
    ) -> Result<PlannedPairingLifecycleSuccessor<'_, CAPACITY>, PendingCredentialTransitionFault>
    {
        self.plan_pending_transition(pending, PendingTransition::Activate)
    }

    /// Construct the exact next complete authority changing one `Pending` to
    /// a PSK-free `PairingAborted` tombstone.
    pub fn plan_abort_pending(
        &self,
        pending: PendingCredentialRef,
    ) -> Result<PlannedPairingLifecycleSuccessor<'_, CAPACITY>, PendingCredentialTransitionFault>
    {
        self.plan_pending_transition(pending, PendingTransition::Abort)
    }

    /// Validate one boot-built candidate as the sole next structural mutation.
    ///
    /// Boot replay may construct an authority directly with
    /// [`CredentialAuthorityBuilder`]. Live replacement must additionally pass
    /// this cross-snapshot check: the authority revision advances exactly once,
    /// exactly one record changes, every change receives that fresh revision as
    /// its generation, and no retained ID silently disappears. This does not
    /// authorize lifecycle or policy changes. Pairing code must use the checked
    /// lifecycle planners in this crate; future unrelated administration needs
    /// its own policy owner before constructing a generic candidate.
    /// The returned plan borrows this authority until the candidate is either
    /// durably committed and published or dropped.
    pub fn plan_successor(
        &self,
        candidate: CredentialAuthority<CAPACITY>,
    ) -> Result<PlannedCredentialSuccessor<'_, CAPACITY>, CredentialSuccessorFault<CAPACITY>> {
        let expected_revision = match self.revision.next() {
            Ok(revision) => revision,
            Err(_) => {
                return Err(CredentialSuccessorFault {
                    kind: CredentialSuccessorFaultKind::AuthorityRevisionNotNext,
                    candidate,
                });
            }
        };
        if candidate.revision != expected_revision {
            return Err(CredentialSuccessorFault {
                kind: CredentialSuccessorFaultKind::AuthorityRevisionNotNext,
                candidate,
            });
        }

        let mut mutations = 0_usize;
        for current in self.records.iter().flatten() {
            let Some(next) = candidate.find_by_id(current.id) else {
                return Err(CredentialSuccessorFault {
                    kind: CredentialSuccessorFaultKind::CredentialRemovedWithoutTombstone,
                    candidate,
                });
            };
            if !current.exactly_matches(next) {
                mutations += 1;
            }
        }

        for next in candidate.records.iter().flatten() {
            if self.find_by_id(next.id).is_some() {
                continue;
            }
            mutations += 1;
        }

        if mutations == 0 {
            return Err(CredentialSuccessorFault {
                kind: CredentialSuccessorFaultKind::NoRecordMutation,
                candidate,
            });
        }
        if mutations > 1 {
            return Err(CredentialSuccessorFault {
                kind: CredentialSuccessorFaultKind::MultipleRecordMutations,
                candidate,
            });
        }

        for current in self.records.iter().flatten() {
            let next = candidate
                .find_by_id(current.id)
                .expect("the first successor pass rejected every removed record");
            if current.exactly_matches(next) {
                continue;
            }
            if current.status == CredentialStatus::Revoked {
                return Err(CredentialSuccessorFault {
                    kind: CredentialSuccessorFaultKind::RevokedCredentialChanged,
                    candidate,
                });
            }
            if next.audit.created_revision != current.audit.created_revision
                || next.audit.pairing_origin != current.audit.pairing_origin
            {
                return Err(CredentialSuccessorFault {
                    kind: CredentialSuccessorFaultKind::ExistingCredentialAuditChanged,
                    candidate,
                });
            }
            if next.generation.get() != expected_revision.get()
                || next.audit.modified_revision != expected_revision
            {
                return Err(CredentialSuccessorFault {
                    kind: CredentialSuccessorFaultKind::ChangedWithoutGenerationAdvance,
                    candidate,
                });
            }
        }

        for next in candidate.records.iter().flatten() {
            if self.find_by_id(next.id).is_some() {
                continue;
            }
            if next.generation.get() != expected_revision.get()
                || next.audit.created_revision != expected_revision
                || next.audit.modified_revision != expected_revision
            {
                return Err(CredentialSuccessorFault {
                    kind: CredentialSuccessorFaultKind::NewCredentialRevisionInvalid,
                    candidate,
                });
            }
        }

        Ok(PlannedCredentialSuccessor {
            candidate,
            _current: PhantomData,
        })
    }

    /// Select zeroizing PSK material for one client handshake.
    ///
    /// Missing, pending and revoked IDs all map to the same result. Rotation
    /// after selection remains safe because the admitted request carries this
    /// generation and must be revalidated again immediately before dispatch.
    pub fn select_for_handshake(
        &self,
        id: CredentialId,
    ) -> Result<SelectedCredential, CredentialUnavailable> {
        let record = self.find_active(id).ok_or(CredentialUnavailable)?;
        let psk = record.psk().ok_or(CredentialUnavailable)?;
        Ok(SelectedCredential {
            id: record.id,
            generation: record.generation,
            psk: copy_zeroizing_psk(psk),
        })
    }

    /// Revalidate exact authenticated credential facts and mint a dispatch lease.
    pub fn revalidate(
        &self,
        id: CredentialId,
        generation: CredentialGeneration,
    ) -> Result<DispatchLease<'_, CAPACITY>, CredentialRejected> {
        let record = self.find_active(id).ok_or(CredentialRejected)?;
        if record.generation != generation {
            return Err(CredentialRejected);
        }
        let provenance = match DispatchProvenance::new(
            *record.id.as_bytes(),
            record.generation.get(),
            self.revision.get(),
            record.audit.policy_version.get(),
        ) {
            Ok(provenance) => provenance,
            // The validated authority enforces every provenance invariant. A
            // rejection here is a defensive fail-closed guard if those owners
            // ever drift apart.
            Err(_) => return Err(CredentialRejected),
        };
        Ok(DispatchLease {
            context: DispatchContext::authenticated(
                record.principal,
                record.permissions,
                provenance,
            ),
            _authority: PhantomData,
        })
    }

    fn find_active(&self, id: CredentialId) -> Option<&CredentialRecord> {
        let mut selected = 0_u64;
        let mut found = Choice::from(0);
        for (index, slot) in self.records.iter().enumerate() {
            let Some(record) = slot else {
                continue;
            };
            let id_matches = record.id.as_bytes().ct_eq(id.as_bytes());
            let is_active = Choice::from(u8::from(record.status == CredentialStatus::Active));
            let choose = id_matches & is_active & !found;
            let index =
                u64::try_from(index).expect("a fixed credential table index always fits in u64");
            selected = u64::conditional_select(&selected, &index, choose);
            found |= id_matches & is_active;
        }
        if bool::from(found) {
            let selected =
                usize::try_from(selected).expect("selected credential index originated as usize");
            self.records[selected].as_ref()
        } else {
            None
        }
    }

    fn find_by_id(&self, id: CredentialId) -> Option<&CredentialRecord> {
        self.records.iter().flatten().find(|record| record.id == id)
    }

    fn sole_pending_record(&self) -> Result<Option<&CredentialRecord>, MultiplePendingCredentials> {
        let mut pending = None;
        for record in self.records.iter().flatten() {
            if record.status != CredentialStatus::Pending {
                continue;
            }
            if pending.is_some() {
                return Err(MultiplePendingCredentials);
            }
            pending = Some(record);
        }
        Ok(pending)
    }

    fn duplicate_with_revision(&self, revision: AuthorityRevision) -> Self {
        Self {
            revision,
            records: core::array::from_fn(|index| {
                self.records[index]
                    .as_ref()
                    .map(CredentialRecord::duplicate_for_candidate)
            }),
            count: self.count,
        }
    }

    fn plan_pending_transition(
        &self,
        pending: PendingCredentialRef,
        transition: PendingTransition,
    ) -> Result<PlannedPairingLifecycleSuccessor<'_, CAPACITY>, PendingCredentialTransitionFault>
    {
        let current = match self.sole_pending_record() {
            Ok(Some(record))
                if record.id == pending.id && record.generation == pending.generation =>
            {
                record
            }
            Ok(_) => {
                return Err(pending_transition_fault(
                    CredentialLifecycleFaultKind::PendingCredentialUnavailable,
                ));
            }
            Err(_) => {
                return Err(pending_transition_fault(
                    CredentialLifecycleFaultKind::MultiplePendingCredentials,
                ));
            }
        };
        let next_revision = self.revision.next().map_err(|_| {
            pending_transition_fault(CredentialLifecycleFaultKind::AuthorityRevisionExhausted)
        })?;
        let replacement = match transition {
            PendingTransition::Activate => CredentialRecord::with_zeroizing_secret(
                current.id,
                CredentialGeneration::new(next_revision.get()),
                current.principal,
                current.permissions,
                CredentialStatus::Active,
                CredentialAudit::new(
                    current.audit.created_revision,
                    next_revision,
                    current.audit.pairing_origin,
                    current.audit.policy_version,
                ),
                copy_zeroizing_psk(
                    current
                        .psk()
                        .expect("a validated pending credential always owns one PSK"),
                ),
            ),
            PendingTransition::Abort => CredentialRecord::revoked(
                current.id,
                CredentialGeneration::new(next_revision.get()),
                current.principal,
                CredentialAudit::new(
                    current.audit.created_revision,
                    next_revision,
                    current.audit.pairing_origin,
                    current.audit.policy_version,
                ),
                RevocationReason::PairingAborted,
            ),
        };

        let mut candidate = self.duplicate_with_revision(next_revision);
        let target = candidate
            .records
            .iter_mut()
            .flatten()
            .find(|record| record.id == pending.id)
            .expect("the exact pending record originated in this authority");
        *target = replacement;
        self.plan_successor(candidate)
            .map(|plan| PlannedPairingLifecycleSuccessor {
                transition: match transition {
                    PendingTransition::Activate => PairingLifecycleTransition::ActivatePending,
                    PendingTransition::Abort => PairingLifecycleTransition::AbortPending,
                },
                expected_pending: Some(ExpectedPendingCredential::from_record(current)),
                plan,
            })
            .map_err(|fault| {
                drop(fault);
                pending_transition_fault(CredentialLifecycleFaultKind::StructuralSuccessorRejected)
            })
    }
}

#[derive(Clone, Copy)]
enum PendingTransition {
    Activate,
    Abort,
}

fn new_pending_fault(
    kind: CredentialLifecycleFaultKind,
    enrollment: NewPendingCredential,
) -> NewPendingCredentialFault {
    NewPendingCredentialFault { kind, enrollment }
}

const fn pending_transition_fault(
    kind: CredentialLifecycleFaultKind,
) -> PendingCredentialTransitionFault {
    PendingCredentialTransitionFault { kind }
}

#[cfg(test)]
mod tests;
