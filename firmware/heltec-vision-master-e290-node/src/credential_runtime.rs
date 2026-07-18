//! Resident credential initialization ownership for the permanent E290 node.
//!
//! Boot remains read-only for erased or canonically interrupted credential
//! media. This coordinator consumes that boot result, owns the physical-
//! presence policy and any admitted initialization capability, and accepts only
//! forward progress when it later receives a fresh operation-scoped flash view.
//! It intentionally exposes no Begin, Proof, activation, or abort path yet.

use core::mem;

use reticulum_device_api_credential_store::{
    BoundCredentialStoreAccess, CredentialStoreBinding, CredentialStoreBindingError,
    CredentialStoreFault, CredentialStoreMountError, CredentialStoreRecovery,
    EmptyProvisionMediaClassification, MountedCredentialStore, classify_empty_provision_media,
    mount, recover_empty_provision,
};
use reticulum_device_api_pairing_policy::{
    AcquirePairingExclusive, ActiveLowButton, ButtonEffect, ConnectionId, ConnectionRefusal,
    ExclusiveAcquireOutcome, InitializableMedia, InitializationFacts, InitializationPermit,
    MonotonicMillis, PairingPolicy, PendingRef, PendingState, PermitError, PolicyEvent,
    RequestRefused, WindowClosed,
};

use crate::credential_boot::{
    CredentialBootOutcome, CredentialBootState, MAXIMUM_CREDENTIAL_BOOT_OUTCOME_BYTES,
};

/// Conservative fixed-RAM ceiling for retained credential and pairing state.
pub const MAXIMUM_CREDENTIAL_RUNTIME_BYTES: usize = MAXIMUM_CREDENTIAL_BOOT_OUTCOME_BYTES
    + reticulum_device_api_pairing_policy::PAIRING_POLICY_RAM_CEILING
    + 256;

/// Public snapshot of explicit initialization ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialInitializationStatus {
    /// The mounted or failed boot state is not eligible for initialization.
    Unavailable,
    /// One exact boot-classified trajectory may be admitted under physical
    /// presence.
    Eligible {
        /// Exact media classification retained from boot.
        media: InitializableMedia,
    },
    /// An admitted operation remains owned until a definite physical result.
    InFlight {
        /// Exact media trajectory admitted by the policy.
        media: InitializableMedia,
        /// Whether this operation may already have attempted physical writes.
        physical_io_attempted: bool,
    },
    /// Initialization is permanently blocked for this boot while the admitted
    /// capability remains retained.
    Blocked {
        /// Stable non-secret reason for refusing further physical progress.
        reason: InitializationBlockReason,
    },
    /// Canonical empty revision 1 is mounted and policy ownership was released.
    Completed,
    /// Canonical empty revision 1 is retained, but policy completion violated
    /// an internal ownership invariant, so later mutation remains disabled.
    PolicyFault {
        /// Exact policy completion failure.
        error: PermitError,
    },
}

/// Stable fail-closed reason for stopping one admitted initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitializationBlockReason {
    /// A later operation-scoped view did not name the retained product range.
    AccessBindingMismatch {
        /// Binding captured from the boot flash owner.
        expected: CredentialStoreBinding,
        /// Binding supplied by the later operation-scoped view.
        actual: CredentialStoreBinding,
    },
    /// The portable store rejected the operation-scoped binding before I/O.
    StoreBinding(CredentialStoreBindingError),
    /// Media moved outside the exact forward trajectory admitted by policy.
    MediaTrajectory {
        /// Trajectory admitted under physical presence.
        admitted: InitializableMedia,
        /// Latest read-only physical classification.
        observed: EmptyProvisionMediaClassification,
        /// Whether this owner may already have attempted physical writes.
        physical_io_attempted: bool,
    },
    /// Stable media inspection or recovery fault.
    MediaFault(CredentialStoreFault),
    /// A supposedly completed store was not canonical empty revision 1.
    NonCanonicalMountedAuthority,
}

/// Why an initialization request was not accepted by the resident owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitializationRequestRefusal {
    /// Credential boot state could not construct a safe pairing policy owner.
    PairingUnavailable,
    /// The physical-presence policy refused the request.
    Policy(RequestRefused),
}

/// Non-secret acceptance report; the actual permit remains inside the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitializationAccepted {
    media: InitializableMedia,
}

impl InitializationAccepted {
    /// Exact media trajectory bound into the retained permit.
    pub const fn media(self) -> InitializableMedia {
        self.media
    }
}

/// Retry condition that retains the exact admitted operation.
pub enum InitializationRetry<E> {
    /// The caller's fresh identity preflight was not ready; no store I/O ran.
    IdentityNotReady,
    /// A read or physical operation returned an ambiguous backend result.
    Backend(E),
    /// Exact write readback was not established; reclassification must decide
    /// whether the same operation can continue.
    Readback(CredentialStoreFault),
}

/// Result of driving at most one admitted initialization to a synchronous
/// physical outcome.
pub enum InitializationDriveOutcome<E> {
    /// Canonical empty revision 1 is mounted and mutation can proceed later.
    Completed,
    /// The exact permit remains retained for a later same-boot retry.
    Retry(InitializationRetry<E>),
    /// This boot retains the permit but will perform no further initialization
    /// I/O.
    Blocked(InitializationBlockReason),
    /// No initialization operation currently owns physical progress.
    NotInFlight(CredentialInitializationStatus),
}

enum InitializationOwnership {
    Unavailable,
    Eligible(InitializableMedia),
    InFlight {
        permit: InitializationPermit,
        physical_io_attempted: bool,
    },
    Blocked {
        permit: InitializationPermit,
        reason: InitializationBlockReason,
    },
    Completed,
    PolicyFault(PermitError),
}

/// Backend-independent resident credential owner for the permanent E290 node.
///
/// The raw flash backend is never retained here. Each physical drive borrows a
/// fresh bound view from the sole product flash owner and verifies that it is
/// exactly the range captured at boot.
#[must_use = "credential and pairing ownership must remain resident"]
pub struct CredentialRuntime {
    binding: CredentialStoreBinding,
    boot_state: CredentialBootState,
    mounted: Option<MountedCredentialStore>,
    pairing: Option<PairingPolicy>,
    initialization: InitializationOwnership,
}

const _: () = assert!(mem::size_of::<CredentialRuntime>() <= MAXIMUM_CREDENTIAL_RUNTIME_BYTES);

impl CredentialRuntime {
    /// Consume boot ownership into the expected product-flash binding.
    pub fn from_boot(outcome: CredentialBootOutcome, expected: CredentialStoreBinding) -> Self {
        let (boot_state, mounted) = outcome.into_parts_for_binding(expected);
        let initialization = match boot_state {
            CredentialBootState::UninitializedErased => {
                InitializationOwnership::Eligible(InitializableMedia::ExactlyErased)
            }
            CredentialBootState::InitializationInterrupted => {
                InitializationOwnership::Eligible(InitializableMedia::RecoverableInterrupted)
            }
            _ => InitializationOwnership::Unavailable,
        };
        let pairing = pairing_policy_for_boot(boot_state, mounted.as_ref());
        Self {
            binding: expected,
            boot_state,
            mounted,
            pairing,
            initialization,
        }
    }

    /// Credential boot admission retained by the runtime.
    pub const fn credential_boot_state(&self) -> CredentialBootState {
        self.boot_state
    }

    /// Exact physical binding required for every later operation-scoped view.
    pub const fn binding(&self) -> CredentialStoreBinding {
        self.binding
    }

    /// Non-secret mounted authority revision, when one is retained.
    pub fn revision(&self) -> Option<u64> {
        self.mounted
            .as_ref()
            .map(|mounted| mounted.revision().get())
    }

    /// Whether a retained authority can authenticate existing credentials.
    pub fn authority_publishable(&self) -> bool {
        self.boot_state.authority_publishable()
            && self
                .mounted
                .as_ref()
                .and_then(MountedCredentialStore::publishable_authority)
                .is_some()
    }

    /// Whether the retained authority is physically and locally eligible for a
    /// future credential mutation.
    pub fn mutation_eligible(&self) -> bool {
        self.boot_state.mutation_eligible()
            && self.pairing.is_some()
            && self
                .mounted
                .as_ref()
                .is_some_and(|mounted| mounted.recovery() == CredentialStoreRecovery::Clean)
            && !matches!(
                self.initialization,
                InitializationOwnership::Blocked { .. } | InitializationOwnership::PolicyFault(_)
            )
    }

    /// Whether boot supplied enough validated state to own pairing policy.
    pub const fn pairing_policy_available(&self) -> bool {
        self.pairing.is_some()
    }

    /// Current non-secret explicit-initialization state.
    pub const fn initialization_status(&self) -> CredentialInitializationStatus {
        match &self.initialization {
            InitializationOwnership::Unavailable => CredentialInitializationStatus::Unavailable,
            InitializationOwnership::Eligible(media) => {
                CredentialInitializationStatus::Eligible { media: *media }
            }
            InitializationOwnership::InFlight {
                permit,
                physical_io_attempted,
            } => CredentialInitializationStatus::InFlight {
                media: permit.media(),
                physical_io_attempted: *physical_io_attempted,
            },
            InitializationOwnership::Blocked { reason, .. } => {
                CredentialInitializationStatus::Blocked { reason: *reason }
            }
            InitializationOwnership::Completed => CredentialInitializationStatus::Completed,
            InitializationOwnership::PolicyFault(error) => {
                CredentialInitializationStatus::PolicyFault { error: *error }
            }
        }
    }

    /// Forward one strictly increasing connection epoch to the retained policy.
    pub fn pairing_connected(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
    ) -> Option<Result<Option<WindowClosed>, ConnectionRefusal>> {
        Some(self.pairing.as_mut()?.connected(now, connection))
    }

    /// Forward a disconnect while retaining any admitted physical operation.
    pub fn pairing_disconnected(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
    ) -> Option<PolicyEvent> {
        Some(self.pairing.as_mut()?.disconnected(now, connection))
    }

    /// Forward one already-debounced physical-presence sample.
    pub fn pairing_observe_button(
        &mut self,
        now: MonotonicMillis,
        level: ActiveLowButton,
    ) -> Option<ButtonEffect> {
        Some(self.pairing.as_mut()?.observe_button(now, level))
    }

    /// Confirm that the bearer granted the requested exclusive connection.
    pub fn pairing_exclusive_acquired(
        &mut self,
        now: MonotonicMillis,
        effect: AcquirePairingExclusive,
    ) -> Option<ExclusiveAcquireOutcome> {
        Some(self.pairing.as_mut()?.exclusive_acquired(now, effect))
    }

    /// Poll the retained physical-presence window deadline.
    pub fn pairing_poll_timeout(&mut self, now: MonotonicMillis) -> Option<PolicyEvent> {
        Some(self.pairing.as_mut()?.poll_timeout(now))
    }

    /// Admit explicit initialization while retaining the single-use permit.
    ///
    /// `identity_ready` is a trusted admission-time assertion. The caller must
    /// supply a fresh identity preflight again to [`Self::drive_initialization`].
    pub fn request_initialization(
        &mut self,
        now: MonotonicMillis,
        connection: ConnectionId,
        identity_ready: bool,
    ) -> Result<InitializationAccepted, InitializationRequestRefusal> {
        let media = match &self.initialization {
            InitializationOwnership::Eligible(media) => Some(*media),
            _ => None,
        };
        let policy = self
            .pairing
            .as_mut()
            .ok_or(InitializationRequestRefusal::PairingUnavailable)?;
        let permit = policy
            .initialize(
                now,
                connection,
                InitializationFacts::new(identity_ready, media),
            )
            .map_err(InitializationRequestRefusal::Policy)?;
        let admitted = permit.media();
        self.initialization = InitializationOwnership::InFlight {
            permit,
            physical_io_attempted: false,
        };
        Ok(InitializationAccepted { media: admitted })
    }

    /// Reclassify and synchronously advance one admitted initialization.
    ///
    /// The admitted capability is retained across identity-not-ready, backend,
    /// and exact-readback ambiguity. A binding mismatch, backward trajectory,
    /// noncanonical media, or noncanonical completed authority blocks this boot
    /// without releasing the capability. The mounted revision-1 owner is
    /// installed before policy completion is consumed.
    pub fn drive_initialization<A>(
        &mut self,
        access: &mut A,
        identity_ready: bool,
    ) -> InitializationDriveOutcome<A::Error>
    where
        A: BoundCredentialStoreAccess,
    {
        let current = mem::replace(
            &mut self.initialization,
            InitializationOwnership::Unavailable,
        );
        let (permit, physical_io_attempted) = match current {
            InitializationOwnership::InFlight {
                permit,
                physical_io_attempted,
            } => (permit, physical_io_attempted),
            InitializationOwnership::Blocked { permit, reason } => {
                self.initialization = InitializationOwnership::Blocked { permit, reason };
                return InitializationDriveOutcome::Blocked(reason);
            }
            other => {
                self.initialization = other;
                return InitializationDriveOutcome::NotInFlight(self.initialization_status());
            }
        };

        if !identity_ready {
            self.initialization = InitializationOwnership::InFlight {
                permit,
                physical_io_attempted,
            };
            return InitializationDriveOutcome::Retry(InitializationRetry::IdentityNotReady);
        }

        let actual = access.credential_store_binding();
        if actual != self.binding {
            return self.block(
                permit,
                InitializationBlockReason::AccessBindingMismatch {
                    expected: self.binding,
                    actual,
                },
            );
        }

        let observed = match classify_empty_provision_media(access) {
            Ok(observed) => observed,
            Err(CredentialStoreMountError::Backend(error)) => {
                self.initialization = InitializationOwnership::InFlight {
                    permit,
                    physical_io_attempted,
                };
                return InitializationDriveOutcome::Retry(InitializationRetry::Backend(error));
            }
            Err(CredentialStoreMountError::Binding(error)) => {
                return self.block(permit, InitializationBlockReason::StoreBinding(error));
            }
            Err(CredentialStoreMountError::Fault(fault)) => {
                return self.block(permit, InitializationBlockReason::MediaFault(fault));
            }
        };
        let admitted = permit.media();
        if !trajectory_is_forward(admitted, physical_io_attempted, observed) {
            return self.block(
                permit,
                InitializationBlockReason::MediaTrajectory {
                    admitted,
                    observed,
                    physical_io_attempted,
                },
            );
        }

        let result = if observed == EmptyProvisionMediaClassification::CommittedEmptyRevision1 {
            mount(access)
        } else {
            recover_empty_provision(access)
        };
        let mounted = match result {
            Ok(mounted) => mounted,
            Err(CredentialStoreMountError::Backend(error)) => {
                self.initialization = InitializationOwnership::InFlight {
                    permit,
                    physical_io_attempted: true,
                };
                return InitializationDriveOutcome::Retry(InitializationRetry::Backend(error));
            }
            Err(CredentialStoreMountError::Binding(error)) => {
                return self.block(permit, InitializationBlockReason::StoreBinding(error));
            }
            Err(CredentialStoreMountError::Fault(
                fault @ CredentialStoreFault::ReadbackMismatch { .. },
            )) => {
                self.initialization = InitializationOwnership::InFlight {
                    permit,
                    physical_io_attempted: true,
                };
                return InitializationDriveOutcome::Retry(InitializationRetry::Readback(fault));
            }
            Err(CredentialStoreMountError::Fault(fault)) => {
                return self.block(permit, InitializationBlockReason::MediaFault(fault));
            }
        };

        if !is_canonical_empty_revision_one(&mounted, self.binding) {
            self.mounted = Some(mounted);
            return self.block(
                permit,
                InitializationBlockReason::NonCanonicalMountedAuthority,
            );
        }

        self.mounted = Some(mounted);
        self.boot_state = CredentialBootState::Ready;
        let finish = self
            .pairing
            .as_mut()
            .expect("an admitted permit has one resident policy owner")
            .finish_initialization(permit);
        match finish {
            Ok(()) => {
                self.initialization = InitializationOwnership::Completed;
                InitializationDriveOutcome::Completed
            }
            Err(error) => {
                self.initialization = InitializationOwnership::PolicyFault(error);
                InitializationDriveOutcome::NotInFlight(self.initialization_status())
            }
        }
    }

    fn block<E>(
        &mut self,
        permit: InitializationPermit,
        reason: InitializationBlockReason,
    ) -> InitializationDriveOutcome<E> {
        self.initialization = InitializationOwnership::Blocked { permit, reason };
        InitializationDriveOutcome::Blocked(reason)
    }
}

fn pairing_policy_for_boot(
    state: CredentialBootState,
    mounted: Option<&MountedCredentialStore>,
) -> Option<PairingPolicy> {
    let pending = match state {
        CredentialBootState::UninitializedErased
        | CredentialBootState::InitializationInterrupted => PendingState::None,
        CredentialBootState::Ready => {
            let authority = mounted?.publishable_authority()?;
            match authority.pending_credential().ok()? {
                None => PendingState::None,
                Some(pending) => {
                    PendingState::One(PendingRef::new(pending.id(), pending.generation()).ok()?)
                }
            }
        }
        CredentialBootState::AuthenticationOnly { .. }
        | CredentialBootState::Blocked { .. }
        | CredentialBootState::Corrupt { .. }
        | CredentialBootState::Backend { .. } => return None,
    };
    Some(PairingPolicy::new(pending))
}

const fn trajectory_is_forward(
    admitted: InitializableMedia,
    physical_io_attempted: bool,
    observed: EmptyProvisionMediaClassification,
) -> bool {
    matches!(
        (admitted, physical_io_attempted, observed),
        (
            InitializableMedia::ExactlyErased,
            false,
            EmptyProvisionMediaClassification::ExactlyErased,
        ) | (
            InitializableMedia::ExactlyErased,
            true,
            EmptyProvisionMediaClassification::ExactlyErased
                | EmptyProvisionMediaClassification::RecoverableInterrupted
                | EmptyProvisionMediaClassification::CommittedEmptyRevision1,
        ) | (
            InitializableMedia::RecoverableInterrupted,
            false,
            EmptyProvisionMediaClassification::RecoverableInterrupted,
        ) | (
            InitializableMedia::RecoverableInterrupted,
            true,
            EmptyProvisionMediaClassification::RecoverableInterrupted
                | EmptyProvisionMediaClassification::CommittedEmptyRevision1,
        )
    )
}

fn is_canonical_empty_revision_one(
    mounted: &MountedCredentialStore,
    binding: CredentialStoreBinding,
) -> bool {
    mounted.binding() == binding
        && mounted.revision().get() == 1
        && mounted.recovery() == CredentialStoreRecovery::Clean
        && mounted
            .publishable_authority()
            .is_some_and(|authority| authority.record_count() == 0)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::mem::size_of;

    use embedded_storage::nor_flash::{
        ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
        check_erase, check_read, check_write,
    };
    use reticulum_device_api_credential_store::{
        BoundCredentialStore, CredentialStoreDeviceId, PARTITION_SIZE, PHYSICAL_FORMAT_VERSION,
        mount, recover_empty_provision,
    };
    use reticulum_device_api_pairing_policy::{
        BUTTON_HOLD_MILLIS, CloseReason, PAIRING_WINDOW_MILLIS, RequestRefusal,
    };
    use std::vec;
    use std::vec::Vec;

    use super::*;
    use crate::credential_boot::{CredentialBootState, boot_credentials};

    const ABSOLUTE_OFFSET: usize = 0x52_0000;
    const SECTOR_SIZE: usize = PARTITION_SIZE / 2;
    const CONNECTION_VALUE: u64 = 1;
    const BUTTON_PRESS_MILLIS: u64 = 10;
    const WINDOW_OPEN_MILLIS: u64 = BUTTON_PRESS_MILLIS + BUTTON_HOLD_MILLIS;
    const REQUEST_MILLIS: u64 = WINDOW_OPEN_MILLIS + 1;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FakeError {
        Injected,
        Bounds,
        Alignment,
        IllegalProgramming,
    }

    impl NorFlashError for FakeError {
        fn kind(&self) -> NorFlashErrorKind {
            match self {
                Self::Bounds => NorFlashErrorKind::OutOfBounds,
                Self::Alignment => NorFlashErrorKind::NotAligned,
                Self::Injected | Self::IllegalProgramming => NorFlashErrorKind::Other,
            }
        }
    }

    #[derive(Clone, Copy)]
    enum WriteFault {
        Partial(usize),
        LostReply,
    }

    struct FakeNor {
        bytes: Vec<u8>,
        reads: usize,
        writes: usize,
        erases: usize,
        fail_next_read: bool,
        fail_next_write: Option<WriteFault>,
    }

    impl FakeNor {
        fn erased() -> Self {
            Self {
                bytes: vec![0xff; PARTITION_SIZE],
                reads: 0,
                writes: 0,
                erases: 0,
                fail_next_read: false,
                fail_next_write: None,
            }
        }

        fn reset_io(&mut self) {
            self.reads = 0;
            self.writes = 0;
            self.erases = 0;
            self.fail_next_read = false;
            self.fail_next_write = None;
        }

        fn range(&self, offset: u32, len: usize) -> Result<core::ops::Range<usize>, FakeError> {
            let start = usize::try_from(offset).map_err(|_| FakeError::Bounds)?;
            let end = start.checked_add(len).ok_or(FakeError::Bounds)?;
            if end > self.bytes.len() {
                return Err(FakeError::Bounds);
            }
            Ok(start..end)
        }

        fn program(&mut self, offset: usize, bytes: &[u8]) -> Result<(), FakeError> {
            let target = &mut self.bytes[offset..offset + bytes.len()];
            if target
                .iter()
                .zip(bytes)
                .any(|(current, next)| current & next != *next)
            {
                return Err(FakeError::IllegalProgramming);
            }
            for (current, next) in target.iter_mut().zip(bytes) {
                *current &= *next;
            }
            Ok(())
        }
    }

    impl ErrorType for FakeNor {
        type Error = FakeError;
    }

    impl ReadNorFlash for FakeNor {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            check_read(self, offset, bytes.len()).map_err(map_check)?;
            let range = self.range(offset, bytes.len())?;
            self.reads += 1;
            if core::mem::take(&mut self.fail_next_read) {
                return Err(FakeError::Injected);
            }
            bytes.copy_from_slice(&self.bytes[range]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.bytes.len()
        }
    }

    impl NorFlash for FakeNor {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = SECTOR_SIZE;

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            check_write(self, offset, bytes.len()).map_err(map_check)?;
            let range = self.range(offset, bytes.len())?;
            self.writes += 1;
            match self.fail_next_write.take() {
                Some(WriteFault::Partial(cut)) => {
                    let cut = cut.min(bytes.len());
                    self.program(range.start, &bytes[..cut])?;
                    Err(FakeError::Injected)
                }
                Some(WriteFault::LostReply) => {
                    self.program(range.start, bytes)?;
                    Err(FakeError::Injected)
                }
                None => self.program(range.start, bytes),
            }
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            check_erase(self, from, to).map_err(map_check)?;
            let len = usize::try_from(to - from).map_err(|_| FakeError::Bounds)?;
            let range = self.range(from, len)?;
            self.erases += 1;
            self.bytes[range].fill(0xff);
            Ok(())
        }
    }

    impl MultiwriteNorFlash for FakeNor {}

    fn map_check(kind: NorFlashErrorKind) -> FakeError {
        match kind {
            NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
            NorFlashErrorKind::NotAligned => FakeError::Alignment,
            _ => FakeError::Injected,
        }
    }

    const fn binding(device_byte: u8) -> CredentialStoreBinding {
        CredentialStoreBinding::new(
            CredentialStoreDeviceId::new([device_byte; 16]),
            ABSOLUTE_OFFSET,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        )
    }

    fn bound(
        flash: FakeNor,
        store_binding: CredentialStoreBinding,
    ) -> BoundCredentialStore<FakeNor> {
        BoundCredentialStore::new(flash, store_binding)
    }

    fn time(value: u64) -> MonotonicMillis {
        MonotonicMillis::new(value)
    }

    fn connection() -> ConnectionId {
        ConnectionId::new(CONNECTION_VALUE).expect("test connection is nonzero")
    }

    fn open_pairing_window(runtime: &mut CredentialRuntime) {
        assert_eq!(
            runtime.pairing_connected(time(0), connection()),
            Some(Ok(None))
        );
        assert!(matches!(
            runtime.pairing_observe_button(time(0), ActiveLowButton::High),
            Some(ButtonEffect::None)
        ));
        assert!(matches!(
            runtime.pairing_observe_button(time(BUTTON_PRESS_MILLIS), ActiveLowButton::Low),
            Some(ButtonEffect::None)
        ));
        let acquire =
            match runtime.pairing_observe_button(time(WINDOW_OPEN_MILLIS), ActiveLowButton::Low) {
                Some(ButtonEffect::AcquirePairingExclusive(acquire)) => acquire,
                _ => panic!("continuous hold did not request pairing exclusivity"),
            };
        assert!(matches!(
            runtime.pairing_exclusive_acquired(time(WINDOW_OPEN_MILLIS), acquire),
            Some(ExclusiveAcquireOutcome::Opened(opened))
                if opened.connection() == connection()
        ));
    }

    fn admit_initialization(runtime: &mut CredentialRuntime, expected: InitializableMedia) {
        open_pairing_window(runtime);
        let accepted = runtime
            .request_initialization(time(REQUEST_MILLIS), connection(), true)
            .unwrap_or_else(|_| panic!("eligible initialization was refused"));
        assert_eq!(accepted.media(), expected);
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::InFlight {
                media: expected,
                physical_io_attempted: false,
            }
        );
    }

    fn erased_runtime() -> (CredentialRuntime, BoundCredentialStore<FakeNor>) {
        let store_binding = binding(0x51);
        let mut access = bound(FakeNor::erased(), store_binding);
        let boot = boot_credentials(&mut access);
        assert_eq!(boot.state(), CredentialBootState::UninitializedErased);
        access.backend_mut().reset_io();
        (CredentialRuntime::from_boot(boot, store_binding), access)
    }

    fn interrupted_runtime() -> (CredentialRuntime, BoundCredentialStore<FakeNor>) {
        let store_binding = binding(0x52);
        let mut seed = bound(FakeNor::erased(), store_binding);
        seed.backend_mut().fail_next_write = Some(WriteFault::Partial(37));
        assert!(matches!(
            recover_empty_provision(&mut seed),
            Err(CredentialStoreMountError::Backend(FakeError::Injected))
        ));
        let mut access = bound(seed.into_backend(), store_binding);
        access.backend_mut().reset_io();
        let boot = boot_credentials(&mut access);
        assert_eq!(boot.state(), CredentialBootState::InitializationInterrupted);
        access.backend_mut().reset_io();
        (CredentialRuntime::from_boot(boot, store_binding), access)
    }

    fn committed_bytes(store_binding: CredentialStoreBinding) -> Vec<u8> {
        let mut access = bound(FakeNor::erased(), store_binding);
        let mounted = recover_empty_provision(&mut access)
            .unwrap_or_else(|_| panic!("test revision-1 provisioning failed"));
        assert_eq!(mounted.revision().get(), 1);
        access.into_backend().bytes
    }

    #[test]
    fn erased_media_requires_presence_then_establishes_only_empty_revision_one() {
        let (mut runtime, mut access) = erased_runtime();
        assert_eq!(
            runtime.credential_boot_state(),
            CredentialBootState::UninitializedErased
        );
        assert_eq!(runtime.revision(), None);
        assert!(!runtime.authority_publishable());
        assert!(!runtime.mutation_eligible());
        assert!(runtime.pairing_policy_available());
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::Eligible {
                media: InitializableMedia::ExactlyErased,
            }
        );

        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Completed
        ));
        assert!(access.backend().reads > 0);
        assert!(access.backend().writes > 0);
        assert_eq!(access.backend().erases, 0);
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::Completed
        );
        assert_eq!(runtime.credential_boot_state(), CredentialBootState::Ready);
        assert_eq!(runtime.revision(), Some(1));
        assert!(runtime.authority_publishable());
        assert!(runtime.mutation_eligible());

        let mounted = mount(&mut access).unwrap_or_else(|_| panic!("revision 1 did not remount"));
        let authority = mounted
            .publishable_authority()
            .expect("completed empty authority must be publishable");
        assert_eq!(
            authority.record_count(),
            0,
            "initialization implicitly ran Begin"
        );
    }

    #[test]
    fn identity_not_ready_retries_without_store_io_or_losing_the_permit() {
        let (mut runtime, mut access) = erased_runtime();
        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        access.backend_mut().reset_io();

        assert!(matches!(
            runtime.drive_initialization(&mut access, false),
            InitializationDriveOutcome::Retry(InitializationRetry::IdentityNotReady)
        ));
        assert_eq!(access.backend().reads, 0);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::InFlight {
                media: InitializableMedia::ExactlyErased,
                physical_io_attempted: false,
            }
        );
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Completed
        ));
    }

    #[test]
    fn classifier_backend_ambiguity_retains_unattempted_ownership() {
        let (mut runtime, mut access) = erased_runtime();
        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        access.backend_mut().reset_io();
        access.backend_mut().fail_next_read = true;

        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Retry(InitializationRetry::Backend(FakeError::Injected))
        ));
        assert_eq!(access.backend().reads, 1);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::InFlight {
                media: InitializableMedia::ExactlyErased,
                physical_io_attempted: false,
            }
        );
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Completed
        ));
    }

    #[test]
    fn partial_write_and_lost_reply_reconcile_under_the_same_permit() {
        for fault in [WriteFault::Partial(37), WriteFault::LostReply] {
            let (mut runtime, mut access) = erased_runtime();
            admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
            access.backend_mut().reset_io();
            access.backend_mut().fail_next_write = Some(fault);

            assert!(matches!(
                runtime.drive_initialization(&mut access, true),
                InitializationDriveOutcome::Retry(InitializationRetry::Backend(
                    FakeError::Injected
                ))
            ));
            assert_eq!(access.backend().writes, 1);
            assert_eq!(
                runtime.initialization_status(),
                CredentialInitializationStatus::InFlight {
                    media: InitializableMedia::ExactlyErased,
                    physical_io_attempted: true,
                }
            );
            assert!(matches!(
                runtime.drive_initialization(&mut access, true),
                InitializationDriveOutcome::Completed
            ));
            assert_eq!(runtime.revision(), Some(1));
        }
    }

    #[test]
    fn interrupted_boot_trajectory_can_finish_initialization() {
        let (mut runtime, mut access) = interrupted_runtime();
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::Eligible {
                media: InitializableMedia::RecoverableInterrupted,
            }
        );
        admit_initialization(&mut runtime, InitializableMedia::RecoverableInterrupted);
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Completed
        ));
        assert_eq!(runtime.revision(), Some(1));
        assert!(runtime.authority_publishable());
        assert!(runtime.mutation_eligible());
    }

    #[test]
    fn interrupted_boot_cannot_move_backward_to_erased_media() {
        let (mut runtime, mut access) = interrupted_runtime();
        admit_initialization(&mut runtime, InitializableMedia::RecoverableInterrupted);
        access.backend_mut().bytes.fill(0xff);
        access.backend_mut().reset_io();

        let expected = InitializationBlockReason::MediaTrajectory {
            admitted: InitializableMedia::RecoverableInterrupted,
            observed: EmptyProvisionMediaClassification::ExactlyErased,
            physical_io_attempted: false,
        };
        assert!(matches!(
            runtime.drive_initialization(&mut access, true),
            InitializationDriveOutcome::Blocked(reason) if reason == expected
        ));
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::Blocked { reason: expected }
        );
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
    }

    #[test]
    fn off_trajectory_and_preexisting_commit_are_first_drive_contradictions() {
        for (observed, bytes) in [
            (EmptyProvisionMediaClassification::NotRecoverable, {
                let mut bytes = vec![0xff; PARTITION_SIZE];
                bytes[SECTOR_SIZE] = 0xfe;
                bytes
            }),
            (
                EmptyProvisionMediaClassification::CommittedEmptyRevision1,
                committed_bytes(binding(0x51)),
            ),
        ] {
            let (mut runtime, mut access) = erased_runtime();
            admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
            access.backend_mut().bytes = bytes;
            access.backend_mut().reset_io();
            let expected = InitializationBlockReason::MediaTrajectory {
                admitted: InitializableMedia::ExactlyErased,
                observed,
                physical_io_attempted: false,
            };

            assert!(matches!(
                runtime.drive_initialization(&mut access, true),
                InitializationDriveOutcome::Blocked(reason) if reason == expected
            ));
            assert_eq!(access.backend().writes, 0);
            assert_eq!(access.backend().erases, 0);
        }
    }

    #[test]
    fn wrong_operation_binding_blocks_before_any_store_io() {
        let (mut runtime, _) = erased_runtime();
        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        let expected_binding = runtime.binding();
        let actual_binding = binding(0x99);
        let mut wrong_access = bound(FakeNor::erased(), actual_binding);
        let expected = InitializationBlockReason::AccessBindingMismatch {
            expected: expected_binding,
            actual: actual_binding,
        };

        assert!(matches!(
            runtime.drive_initialization(&mut wrong_access, true),
            InitializationDriveOutcome::Blocked(reason) if reason == expected
        ));
        assert_eq!(wrong_access.backend().reads, 0);
        assert_eq!(wrong_access.backend().writes, 0);
        assert_eq!(wrong_access.backend().erases, 0);
        assert!(matches!(
            runtime.drive_initialization(&mut wrong_access, true),
            InitializationDriveOutcome::Blocked(reason) if reason == expected
        ));
        assert_eq!(wrong_access.backend().reads, 0);
    }

    #[test]
    fn disconnect_and_timeout_retain_in_flight_ownership_until_finish() {
        for disconnect in [true, false] {
            let (mut runtime, mut access) = erased_runtime();
            admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
            let event = if disconnect {
                runtime.pairing_disconnected(time(REQUEST_MILLIS + 1), connection())
            } else {
                runtime.pairing_poll_timeout(time(WINDOW_OPEN_MILLIS + PAIRING_WINDOW_MILLIS))
            };
            assert!(matches!(
                event,
                Some(PolicyEvent::Closed(closed))
                    if closed.reason()
                        == if disconnect { CloseReason::Disconnect } else { CloseReason::Timeout }
            ));
            assert_eq!(
                runtime.initialization_status(),
                CredentialInitializationStatus::InFlight {
                    media: InitializableMedia::ExactlyErased,
                    physical_io_attempted: false,
                }
            );
            assert!(matches!(
                runtime.drive_initialization(&mut access, true),
                InitializationDriveOutcome::Completed
            ));
        }
    }

    #[test]
    fn repeated_admission_is_refused_without_replacing_owned_initialization() {
        let (mut runtime, access) = erased_runtime();
        admit_initialization(&mut runtime, InitializableMedia::ExactlyErased);
        let refusal = runtime
            .request_initialization(time(REQUEST_MILLIS + 1), connection(), true)
            .expect_err("second initialization was unexpectedly admitted");
        assert!(matches!(
            refusal,
            InitializationRequestRefusal::Policy(refused)
                if refused.reason() == RequestRefusal::OperationInFlight
        ));
        assert_eq!(
            runtime.initialization_status(),
            CredentialInitializationStatus::InFlight {
                media: InitializableMedia::ExactlyErased,
                physical_io_attempted: false,
            }
        );
        assert_eq!(access.backend().reads, 0);
        assert_eq!(access.backend().writes, 0);
    }

    #[test]
    fn resident_runtime_stays_within_its_fixed_ram_ceiling() {
        assert!(size_of::<CredentialRuntime>() <= MAXIMUM_CREDENTIAL_RUNTIME_BYTES);
    }
}
