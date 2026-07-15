//! Retained reset-storm quarantine policy.
//!
//! This module is independent of any MCU, reset primitive or clock. A target
//! supplies word-addressable retained storage and maps its reset reason into
//! [`RetainedBootReason`]. The journal uses two complete slots. Every update
//! poisons the destination slot first, writes and verifies its payload, and
//! commits the slot last. On a retained boot, an empty, corrupt, torn,
//! ambiguous or one-slot-only journal is a quarantine condition.

/// Consecutive fault resets that latch the target in quarantine.
pub const RESET_STORM_QUARANTINE_THRESHOLD: u32 = 3;

/// Words in one retained journal slot.
pub const RESET_QUARANTINE_SLOT_WORDS: usize = 9;

/// Words required for both retained journal slots.
pub const RESET_QUARANTINE_JOURNAL_WORDS: usize = RESET_QUARANTINE_SLOT_WORDS * 2;

const SLOT_MAGIC: u32 = 0x5251_4a31;
const SLOT_SCHEMA: u32 = 1;
const STATE_HEALTHY: u32 = 0x4845_414c;
const STATE_FAULT_PENDING: u32 = 0x5045_4e44;
const WRITE_IN_PROGRESS: u32 = 0x5752_4954;
const COMMIT_MAGIC: u32 = 0x434f_4d54;

const MAGIC_WORD: usize = 0;
const SCHEMA_WORD: usize = 1;
const GENERATION_WORD: usize = 2;
const STREAK_WORD: usize = 3;
const TOTAL_WORD: usize = 4;
const STATE_WORD: usize = 5;
const CHECKSUM_WORD: usize = 6;
const CHECKSUM_INVERSE_WORD: usize = 7;
const COMMIT_WORD: usize = 8;

/// Word-addressable storage retained across the target's digital-core resets.
///
/// `write_barrier` must prevent the compiler and target from moving the final
/// commit store ahead of preceding payload stores. Storage implementations
/// must make aligned word stores indivisible with respect to reset.
pub trait ResetQuarantineStorage {
    /// Read one word from `0..RESET_QUARANTINE_JOURNAL_WORDS`.
    fn read_word(&self, index: usize) -> u32;

    /// Write one aligned word in `0..RESET_QUARANTINE_JOURNAL_WORDS`.
    fn write_word(&mut self, index: usize, value: u32);

    /// Complete all earlier stores before a later journal phase begins.
    fn write_barrier(&mut self);
}

/// Reset classifications relevant to a retained boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedBootReason {
    /// Retention was initialized by a true power-on reset.
    ChipPowerOn,
    /// The target's explicit digital-core software-reset primitive ran.
    CoreSoftwareReset,
    /// The target's configured supervisor watchdog reset the digital core.
    SupervisorWatchdogReset,
    /// Any other reset for which retention is expected to survive.
    OtherRetainedReset,
}

/// Valid state recovered from the newest retained slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResetFaultHistory {
    generation: u32,
    consecutive_fault_resets: u32,
    total_fault_resets: u32,
    radio_fault_pending_reset: bool,
}

impl ResetFaultHistory {
    /// Monotonic journal generation since the last true power cycle.
    pub const fn generation(self) -> u32 {
        self.generation
    }

    /// Returned-radio faults and supervisor-watchdog resets since the last
    /// completed healthy lease.
    pub const fn consecutive_fault_resets(self) -> u32 {
        self.consecutive_fault_resets
    }

    /// Saturating fault-reset count since the last true power cycle.
    pub const fn total_fault_resets(self) -> u32 {
        self.total_fault_resets
    }

    /// Whether the latest radio fault has not yet been correlated with reset.
    pub const fn radio_fault_pending_reset(self) -> bool {
        self.radio_fault_pending_reset
    }
}

/// Fail-closed reason returned before target radio construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResetQuarantineReason {
    /// The consecutive fault-reset threshold was reached.
    FaultStreak,
    /// A pending radio fault was followed by something other than `CoreSw`.
    PendingResetReasonMismatch,
    /// Both slots were empty on a boot whose retention should have survived.
    MissingJournal,
    /// Exactly one of the two required retained slots was valid.
    DegradedJournal,
    /// A slot was corrupt or retained an interrupted-write marker.
    CorruptOrTornJournal,
    /// Two equally current slots disagreed or generation could not advance.
    AmbiguousJournal,
    /// A retained write did not read back as the record being committed.
    JournalWriteFailed,
}

/// Startup decision made while the target RF interlock is still asserted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResetQuarantineDecision {
    /// Startup may proceed to construct the receive-only radio.
    Run(ResetFaultHistory),
    /// Startup must remain RF-inert and must not construct the radio.
    Quarantine {
        /// Why retained state did not authorize radio construction.
        reason: ResetQuarantineReason,
        /// Latest valid history, when one could be recovered unambiguously.
        history: Option<ResetFaultHistory>,
    },
}

/// Failure to durably update an already valid retained journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResetQuarantineWriteError {
    /// Retained state was not a complete, unambiguous two-slot journal.
    InvalidJournal(ResetQuarantineReason),
    /// The next generation cannot be represented without ambiguity.
    GenerationExhausted,
    /// A retained counter cannot be advanced without wrapping.
    CounterExhausted,
    /// The completed transaction did not read back exactly.
    VerificationFailed,
    /// A new fault was attempted while an earlier fault still awaited reset.
    FaultAlreadyPending,
}

/// Result of completing the target's documented healthy lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthyLeaseCommit {
    /// No fault streak existed, so no retained write was necessary.
    AlreadyClear(ResetFaultHistory),
    /// The retained fault streak was durably cleared.
    Cleared(ResetFaultHistory),
}

/// Initialize or validate retained state and decide whether startup may run.
///
/// A true power-on reset deliberately replaces both slots with a clean
/// baseline. On every retained reset, both slots must be valid. A pending radio
/// fault is acknowledged only when the hardware reports the exact software
/// reset primitive used by the fault path; acknowledging never increments the
/// streak a second time. A supervisor-watchdog boot without a pending returned
/// radio fault is itself counted transactionally before startup is authorized.
pub fn prepare_reset_quarantine_boot(
    storage: &mut impl ResetQuarantineStorage,
    reset_reason: RetainedBootReason,
) -> ResetQuarantineDecision {
    if reset_reason == RetainedBootReason::ChipPowerOn {
        return match initialize_both_slots(storage) {
            Ok(history) => ResetQuarantineDecision::Run(history),
            Err(reason) => ResetQuarantineDecision::Quarantine {
                reason,
                history: None,
            },
        };
    }

    let loaded = match load_retained(storage) {
        Ok(loaded) => loaded,
        Err(reason) => {
            return ResetQuarantineDecision::Quarantine {
                reason,
                history: None,
            };
        }
    };

    let history = if loaded.record.state == RecordState::FaultPending {
        if reset_reason != RetainedBootReason::CoreSoftwareReset {
            return ResetQuarantineDecision::Quarantine {
                reason: ResetQuarantineReason::PendingResetReasonMismatch,
                history: Some(loaded.record.history()),
            };
        }

        let acknowledged = Record {
            state: RecordState::Healthy,
            ..loaded.record
        };
        match commit_next(storage, loaded, acknowledged) {
            Ok(record) => record.history(),
            Err(error) => {
                return ResetQuarantineDecision::Quarantine {
                    reason: write_error_reason(error),
                    history: Some(loaded.record.history()),
                };
            }
        }
    } else if reset_reason == RetainedBootReason::SupervisorWatchdogReset {
        let Some(streak) = loaded.record.streak.checked_add(1) else {
            poison_target(storage, loaded.target_slot());
            return ResetQuarantineDecision::Quarantine {
                reason: ResetQuarantineReason::AmbiguousJournal,
                history: Some(loaded.record.history()),
            };
        };
        let watchdog_fault = Record {
            generation: loaded.record.generation,
            streak,
            total: loaded.record.total.saturating_add(1),
            state: RecordState::Healthy,
        };
        match commit_next(storage, loaded, watchdog_fault) {
            Ok(record) => record.history(),
            Err(error) => {
                return ResetQuarantineDecision::Quarantine {
                    reason: write_error_reason(error),
                    history: Some(loaded.record.history()),
                };
            }
        }
    } else {
        loaded.record.history()
    };

    if history.consecutive_fault_resets >= RESET_STORM_QUARANTINE_THRESHOLD {
        ResetQuarantineDecision::Quarantine {
            reason: ResetQuarantineReason::FaultStreak,
            history: Some(history),
        }
    } else {
        ResetQuarantineDecision::Run(history)
    }
}

/// Durably count a returned radio fault before logging or software reset.
///
/// The destination slot's interrupted-write marker is the first store. Once
/// this returns `Ok`, a following `CoreSw` boot can correlate and acknowledge
/// the pending marker without incrementing the streak again. Once the poison
/// store sticks, an interrupted later write makes the next retained boot
/// quarantine. A returned error does not prove that the first store reached
/// retained memory, so the caller must fail closed in the current boot instead
/// of resetting on the assumption that the old journal was invalidated.
pub fn record_radio_fault_before_reset(
    storage: &mut impl ResetQuarantineStorage,
) -> Result<ResetFaultHistory, ResetQuarantineWriteError> {
    let loaded = load_retained(storage).map_err(ResetQuarantineWriteError::InvalidJournal)?;
    if loaded.record.state == RecordState::FaultPending {
        poison_target(storage, loaded.target_slot());
        return Err(ResetQuarantineWriteError::FaultAlreadyPending);
    }
    let Some(streak) = loaded.record.streak.checked_add(1) else {
        poison_target(storage, loaded.target_slot());
        return Err(ResetQuarantineWriteError::CounterExhausted);
    };
    let pending = Record {
        generation: loaded.record.generation,
        streak,
        total: loaded.record.total.saturating_add(1),
        state: RecordState::FaultPending,
    };
    commit_next(storage, loaded, pending).map(|record| record.history())
}

/// Clear the consecutive fault-reset streak after the target's healthy lease.
///
/// Calling this function is the only retained-state transition that clears a
/// non-zero streak without a true power cycle. The policy intentionally has no
/// timer: the target must call it only after its documented continuous healthy
/// interval has elapsed following successful radio activation.
pub fn complete_healthy_radio_lease(
    storage: &mut impl ResetQuarantineStorage,
) -> Result<HealthyLeaseCommit, ResetQuarantineWriteError> {
    let loaded = load_retained(storage).map_err(ResetQuarantineWriteError::InvalidJournal)?;
    if loaded.record.state == RecordState::FaultPending {
        poison_target(storage, loaded.target_slot());
        return Err(ResetQuarantineWriteError::FaultAlreadyPending);
    }
    if loaded.record.streak == 0 {
        return Ok(HealthyLeaseCommit::AlreadyClear(loaded.record.history()));
    }
    let cleared = Record {
        generation: loaded.record.generation,
        streak: 0,
        total: loaded.record.total,
        state: RecordState::Healthy,
    };
    commit_next(storage, loaded, cleared)
        .map(|record| HealthyLeaseCommit::Cleared(record.history()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordState {
    Healthy,
    FaultPending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Record {
    generation: u32,
    streak: u32,
    total: u32,
    state: RecordState,
}

impl Record {
    const fn clean_baseline() -> Self {
        Self {
            generation: 1,
            streak: 0,
            total: 0,
            state: RecordState::Healthy,
        }
    }

    const fn history(self) -> ResetFaultHistory {
        ResetFaultHistory {
            generation: self.generation,
            consecutive_fault_resets: self.streak,
            total_fault_resets: self.total,
            radio_fault_pending_reset: matches!(self.state, RecordState::FaultPending),
        }
    }
}

#[derive(Clone, Copy)]
struct LoadedJournal {
    record: Record,
    newest_slot: usize,
}

impl LoadedJournal {
    const fn target_slot(self) -> usize {
        1 - self.newest_slot
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotState {
    Empty,
    Valid(Record),
    CorruptOrTorn,
}

fn initialize_both_slots(
    storage: &mut impl ResetQuarantineStorage,
) -> Result<ResetFaultHistory, ResetQuarantineReason> {
    // Poison both slots before either becomes valid. A reset during cold
    // initialization therefore cannot degrade into an apparently valid
    // one-slot retained journal.
    for slot in 0..2 {
        storage.write_word(slot_base(slot) + COMMIT_WORD, WRITE_IN_PROGRESS);
    }
    storage.write_barrier();

    let baseline = Record::clean_baseline();
    write_slot_after_poison(storage, 0, baseline);
    write_slot_after_poison(storage, 1, baseline);
    match load_retained(storage) {
        Ok(loaded) if loaded.record == baseline => Ok(baseline.history()),
        Ok(_) | Err(_) => Err(ResetQuarantineReason::JournalWriteFailed),
    }
}

fn load_retained(
    storage: &impl ResetQuarantineStorage,
) -> Result<LoadedJournal, ResetQuarantineReason> {
    let first = read_slot(storage, 0);
    let second = read_slot(storage, 1);
    match (first, second) {
        (SlotState::Empty, SlotState::Empty) => Err(ResetQuarantineReason::MissingJournal),
        (SlotState::Valid(_), SlotState::Empty) | (SlotState::Empty, SlotState::Valid(_)) => {
            Err(ResetQuarantineReason::DegradedJournal)
        }
        (SlotState::CorruptOrTorn, _) | (_, SlotState::CorruptOrTorn) => {
            Err(ResetQuarantineReason::CorruptOrTornJournal)
        }
        (SlotState::Valid(first), SlotState::Valid(second)) => {
            if first.generation == second.generation {
                if first == second {
                    Ok(LoadedJournal {
                        record: first,
                        newest_slot: 1,
                    })
                } else {
                    Err(ResetQuarantineReason::AmbiguousJournal)
                }
            } else if first.generation > second.generation {
                Ok(LoadedJournal {
                    record: first,
                    newest_slot: 0,
                })
            } else {
                Ok(LoadedJournal {
                    record: second,
                    newest_slot: 1,
                })
            }
        }
    }
}

fn commit_next(
    storage: &mut impl ResetQuarantineStorage,
    loaded: LoadedJournal,
    mut next: Record,
) -> Result<Record, ResetQuarantineWriteError> {
    let Some(generation) = loaded.record.generation.checked_add(1) else {
        poison_target(storage, loaded.target_slot());
        return Err(ResetQuarantineWriteError::GenerationExhausted);
    };
    next.generation = generation;
    let target = loaded.target_slot();
    poison_target(storage, target);
    write_slot_after_poison(storage, target, next);

    match load_retained(storage) {
        Ok(loaded) if loaded.record == next && loaded.newest_slot == target => Ok(next),
        Ok(_) | Err(_) => {
            // Do not try to repair the slot here. Leaving it poisoned or
            // corrupt makes the next retained boot fail closed.
            Err(ResetQuarantineWriteError::VerificationFailed)
        }
    }
}

fn poison_target(storage: &mut impl ResetQuarantineStorage, slot: usize) {
    storage.write_word(slot_base(slot) + COMMIT_WORD, WRITE_IN_PROGRESS);
    storage.write_barrier();
}

fn write_slot_after_poison(storage: &mut impl ResetQuarantineStorage, slot: usize, record: Record) {
    let base = slot_base(slot);
    let state = match record.state {
        RecordState::Healthy => STATE_HEALTHY,
        RecordState::FaultPending => STATE_FAULT_PENDING,
    };
    let payload = [
        SLOT_MAGIC,
        SLOT_SCHEMA,
        record.generation,
        record.streak,
        record.total,
        state,
    ];
    let checksum = checksum(&payload);
    for (offset, value) in payload.into_iter().enumerate() {
        storage.write_word(base + offset, value);
    }
    storage.write_word(base + CHECKSUM_WORD, checksum);
    storage.write_word(base + CHECKSUM_INVERSE_WORD, !checksum);
    storage.write_barrier();
    storage.write_word(
        base + COMMIT_WORD,
        commit_value(record.generation, checksum),
    );
    storage.write_barrier();
}

fn read_slot(storage: &impl ResetQuarantineStorage, slot: usize) -> SlotState {
    let base = slot_base(slot);
    let mut words = [0_u32; RESET_QUARANTINE_SLOT_WORDS];
    for (offset, word) in words.iter_mut().enumerate() {
        *word = storage.read_word(base + offset);
    }
    if words.iter().all(|word| *word == 0) {
        return SlotState::Empty;
    }
    if words[COMMIT_WORD] == WRITE_IN_PROGRESS
        || words[MAGIC_WORD] != SLOT_MAGIC
        || words[SCHEMA_WORD] != SLOT_SCHEMA
        || words[GENERATION_WORD] == 0
        || words[CHECKSUM_INVERSE_WORD] != !words[CHECKSUM_WORD]
    {
        return SlotState::CorruptOrTorn;
    }

    let state = match words[STATE_WORD] {
        STATE_HEALTHY => RecordState::Healthy,
        STATE_FAULT_PENDING => RecordState::FaultPending,
        _ => return SlotState::CorruptOrTorn,
    };
    let record = Record {
        generation: words[GENERATION_WORD],
        streak: words[STREAK_WORD],
        total: words[TOTAL_WORD],
        state,
    };
    if record.streak > record.total
        || (record.state == RecordState::FaultPending && record.streak == 0)
    {
        return SlotState::CorruptOrTorn;
    }
    let payload = [
        words[MAGIC_WORD],
        words[SCHEMA_WORD],
        words[GENERATION_WORD],
        words[STREAK_WORD],
        words[TOTAL_WORD],
        words[STATE_WORD],
    ];
    let expected_checksum = checksum(&payload);
    if words[CHECKSUM_WORD] != expected_checksum
        || words[COMMIT_WORD] != commit_value(record.generation, expected_checksum)
    {
        SlotState::CorruptOrTorn
    } else {
        SlotState::Valid(record)
    }
}

const fn slot_base(slot: usize) -> usize {
    slot * RESET_QUARANTINE_SLOT_WORDS
}

const fn commit_value(generation: u32, checksum: u32) -> u32 {
    COMMIT_MAGIC ^ generation.rotate_left(7) ^ checksum.rotate_left(19)
}

fn checksum(words: &[u32]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for word in words {
        for byte in word.to_le_bytes() {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = 0_u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
    }
    !crc
}

const fn write_error_reason(error: ResetQuarantineWriteError) -> ResetQuarantineReason {
    match error {
        ResetQuarantineWriteError::InvalidJournal(reason) => reason,
        ResetQuarantineWriteError::GenerationExhausted
        | ResetQuarantineWriteError::CounterExhausted
        | ResetQuarantineWriteError::FaultAlreadyPending => ResetQuarantineReason::AmbiguousJournal,
        ResetQuarantineWriteError::VerificationFailed => ResetQuarantineReason::JournalWriteFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    #[derive(Clone)]
    struct TestStorage {
        words: [u32; RESET_QUARANTINE_JOURNAL_WORDS],
        writes: std::vec::Vec<(usize, u32)>,
        barriers: usize,
        remaining_writes: Option<usize>,
    }

    impl TestStorage {
        fn empty() -> Self {
            Self {
                words: [0; RESET_QUARANTINE_JOURNAL_WORDS],
                writes: std::vec::Vec::new(),
                barriers: 0,
                remaining_writes: None,
            }
        }

        fn initialized() -> Self {
            let mut storage = Self::empty();
            assert!(matches!(
                prepare_reset_quarantine_boot(&mut storage, RetainedBootReason::ChipPowerOn),
                ResetQuarantineDecision::Run(_)
            ));
            storage.writes.clear();
            storage.barriers = 0;
            storage
        }
    }

    impl ResetQuarantineStorage for TestStorage {
        fn read_word(&self, index: usize) -> u32 {
            self.words[index]
        }

        fn write_word(&mut self, index: usize, value: u32) {
            self.writes.push((index, value));
            if self.remaining_writes.as_mut().is_none_or(|remaining| {
                if *remaining == 0 {
                    false
                } else {
                    *remaining -= 1;
                    true
                }
            }) {
                self.words[index] = value;
            }
        }

        fn write_barrier(&mut self) {
            self.barriers += 1;
        }
    }

    fn expect_run(decision: ResetQuarantineDecision) -> ResetFaultHistory {
        match decision {
            ResetQuarantineDecision::Run(history) => history,
            ResetQuarantineDecision::Quarantine { reason, .. } => {
                panic!("unexpected quarantine: {reason:?}")
            }
        }
    }

    fn fault_and_correlate(storage: &mut TestStorage) -> ResetQuarantineDecision {
        record_radio_fault_before_reset(storage).unwrap();
        prepare_reset_quarantine_boot(storage, RetainedBootReason::CoreSoftwareReset)
    }

    #[test]
    fn power_on_initializes_two_identical_valid_slots() {
        let mut storage = TestStorage::empty();
        let history = expect_run(prepare_reset_quarantine_boot(
            &mut storage,
            RetainedBootReason::ChipPowerOn,
        ));
        assert_eq!(history.consecutive_fault_resets(), 0);
        assert_eq!(history.total_fault_resets(), 0);
        assert!(!history.radio_fault_pending_reset());
        assert!(matches!(read_slot(&storage, 0), SlotState::Valid(_)));
        assert!(matches!(read_slot(&storage, 1), SlotState::Valid(_)));
    }

    #[test]
    fn fault_update_poison_is_first_and_commit_is_last() {
        let mut storage = TestStorage::initialized();
        let history = record_radio_fault_before_reset(&mut storage).unwrap();
        assert_eq!(history.consecutive_fault_resets(), 1);
        assert!(history.radio_fault_pending_reset());

        let target_commit = COMMIT_WORD;
        assert_eq!(storage.writes[0], (target_commit, WRITE_IN_PROGRESS));
        assert_eq!(storage.writes.last().unwrap().0, target_commit);
        assert_ne!(storage.writes.last().unwrap().1, WRITE_IN_PROGRESS);
        assert!(storage.barriers >= 3);
    }

    #[test]
    fn expected_software_reset_acknowledges_without_double_counting() {
        let mut storage = TestStorage::initialized();
        let pending = record_radio_fault_before_reset(&mut storage).unwrap();
        assert_eq!(pending.consecutive_fault_resets(), 1);
        assert_eq!(pending.total_fault_resets(), 1);

        let acknowledged = expect_run(prepare_reset_quarantine_boot(
            &mut storage,
            RetainedBootReason::CoreSoftwareReset,
        ));
        assert_eq!(acknowledged.consecutive_fault_resets(), 1);
        assert_eq!(acknowledged.total_fault_resets(), 1);
        assert!(!acknowledged.radio_fault_pending_reset());
    }

    #[test]
    fn third_correlated_fault_quarantines_before_another_run() {
        let mut storage = TestStorage::initialized();
        for expected in 1..RESET_STORM_QUARANTINE_THRESHOLD {
            let history = expect_run(fault_and_correlate(&mut storage));
            assert_eq!(history.consecutive_fault_resets(), expected);
        }
        let decision = fault_and_correlate(&mut storage);
        assert!(matches!(
            decision,
            ResetQuarantineDecision::Quarantine {
                reason: ResetQuarantineReason::FaultStreak,
                history: Some(history),
            } if history.consecutive_fault_resets() == RESET_STORM_QUARANTINE_THRESHOLD
                && !history.radio_fault_pending_reset()
        ));
    }

    #[test]
    fn supervisor_watchdog_boot_is_counted_before_startup() {
        let mut storage = TestStorage::initialized();
        let history = expect_run(prepare_reset_quarantine_boot(
            &mut storage,
            RetainedBootReason::SupervisorWatchdogReset,
        ));
        assert_eq!(history.consecutive_fault_resets(), 1);
        assert_eq!(history.total_fault_resets(), 1);
        assert!(!history.radio_fault_pending_reset());
        assert_eq!(storage.writes[0].1, WRITE_IN_PROGRESS);
        assert_ne!(storage.writes.last().unwrap().1, WRITE_IN_PROGRESS);
    }

    #[test]
    fn radio_and_watchdog_faults_share_one_quarantine_streak() {
        let mut storage = TestStorage::initialized();
        let radio_fault = expect_run(fault_and_correlate(&mut storage));
        assert_eq!(radio_fault.consecutive_fault_resets(), 1);

        let watchdog_fault = expect_run(prepare_reset_quarantine_boot(
            &mut storage,
            RetainedBootReason::SupervisorWatchdogReset,
        ));
        assert_eq!(watchdog_fault.consecutive_fault_resets(), 2);
        assert_eq!(watchdog_fault.total_fault_resets(), 2);

        assert!(matches!(
            prepare_reset_quarantine_boot(
                &mut storage,
                RetainedBootReason::SupervisorWatchdogReset,
            ),
            ResetQuarantineDecision::Quarantine {
                reason: ResetQuarantineReason::FaultStreak,
                history: Some(history),
            } if history.consecutive_fault_resets() == RESET_STORM_QUARANTINE_THRESHOLD
                && history.total_fault_resets() == RESET_STORM_QUARANTINE_THRESHOLD
                && !history.radio_fault_pending_reset()
        ));
    }

    #[test]
    fn pending_fault_with_any_other_reset_reason_quarantines() {
        let mut storage = TestStorage::initialized();
        record_radio_fault_before_reset(&mut storage).unwrap();
        assert!(matches!(
            prepare_reset_quarantine_boot(&mut storage, RetainedBootReason::OtherRetainedReset,),
            ResetQuarantineDecision::Quarantine {
                reason: ResetQuarantineReason::PendingResetReasonMismatch,
                history: Some(_),
            }
        ));
    }

    #[test]
    fn unrelated_software_reset_without_pending_marker_is_not_a_fault_reset() {
        let mut storage = TestStorage::initialized();
        let history = expect_run(prepare_reset_quarantine_boot(
            &mut storage,
            RetainedBootReason::CoreSoftwareReset,
        ));
        assert_eq!(history.consecutive_fault_resets(), 0);
        assert_eq!(history.total_fault_resets(), 0);
    }

    #[test]
    fn retained_boot_requires_both_slots() {
        let mut storage = TestStorage::initialized();
        storage.words[..RESET_QUARANTINE_SLOT_WORDS].fill(0);
        assert!(matches!(
            prepare_reset_quarantine_boot(&mut storage, RetainedBootReason::OtherRetainedReset,),
            ResetQuarantineDecision::Quarantine {
                reason: ResetQuarantineReason::DegradedJournal,
                ..
            }
        ));
    }

    #[test]
    fn corruption_of_even_the_older_slot_quarantines() {
        let mut storage = TestStorage::initialized();
        let _ = fault_and_correlate(&mut storage);
        let older_slot = 0;
        storage.words[slot_base(older_slot) + TOTAL_WORD] ^= 1;
        assert!(matches!(
            prepare_reset_quarantine_boot(&mut storage, RetainedBootReason::OtherRetainedReset,),
            ResetQuarantineDecision::Quarantine {
                reason: ResetQuarantineReason::CorruptOrTornJournal,
                ..
            }
        ));
    }

    #[test]
    fn every_single_retained_bit_corruption_fails_closed() {
        let baseline = TestStorage::initialized();
        for word in 0..RESET_QUARANTINE_JOURNAL_WORDS {
            for bit in 0..u32::BITS {
                let mut storage = baseline.clone();
                storage.words[word] ^= 1_u32 << bit;
                assert!(
                    matches!(
                        prepare_reset_quarantine_boot(
                            &mut storage,
                            RetainedBootReason::OtherRetainedReset,
                        ),
                        ResetQuarantineDecision::Quarantine { .. }
                    ),
                    "word {word} bit {bit} was not rejected",
                );
            }
        }
    }

    #[test]
    fn interrupted_power_on_initialization_cannot_create_a_runnable_journal() {
        // Two initial poison stores followed by nine stores per slot.
        for completed_writes in 0..20 {
            let mut storage = TestStorage::empty();
            storage.remaining_writes = Some(completed_writes);
            assert!(matches!(
                prepare_reset_quarantine_boot(&mut storage, RetainedBootReason::ChipPowerOn),
                ResetQuarantineDecision::Quarantine {
                    reason: ResetQuarantineReason::JournalWriteFailed,
                    ..
                }
            ));
            storage.remaining_writes = None;
            assert!(matches!(
                prepare_reset_quarantine_boot(&mut storage, RetainedBootReason::OtherRetainedReset,),
                ResetQuarantineDecision::Quarantine { .. }
            ));
        }
    }

    #[test]
    fn every_interrupted_fault_write_after_poison_fails_closed() {
        let baseline = TestStorage::initialized();
        // One poison store, eight payload/check words and one final commit.
        for completed_writes in 1..10 {
            let mut storage = baseline.clone();
            storage.remaining_writes = Some(completed_writes);
            assert_eq!(
                record_radio_fault_before_reset(&mut storage),
                Err(ResetQuarantineWriteError::VerificationFailed)
            );
            storage.remaining_writes = None;
            assert!(matches!(
                prepare_reset_quarantine_boot(&mut storage, RetainedBootReason::CoreSoftwareReset,),
                ResetQuarantineDecision::Quarantine { .. }
            ));
        }
    }

    #[test]
    fn rejected_initial_poison_can_leave_the_old_journal_valid() {
        let mut storage = TestStorage::initialized();
        storage.remaining_writes = Some(0);
        assert_eq!(
            record_radio_fault_before_reset(&mut storage),
            Err(ResetQuarantineWriteError::VerificationFailed)
        );

        storage.remaining_writes = None;
        let unchanged = expect_run(prepare_reset_quarantine_boot(
            &mut storage,
            RetainedBootReason::CoreSoftwareReset,
        ));
        assert_eq!(unchanged.consecutive_fault_resets(), 0);
        assert_eq!(unchanged.total_fault_resets(), 0);
        assert!(!unchanged.radio_fault_pending_reset());
    }

    #[test]
    fn every_interrupted_watchdog_boot_count_after_poison_fails_closed() {
        let baseline = TestStorage::initialized();
        // One poison store, eight payload/check words and one final commit.
        for completed_writes in 1..10 {
            let mut storage = baseline.clone();
            storage.remaining_writes = Some(completed_writes);
            assert!(matches!(
                prepare_reset_quarantine_boot(
                    &mut storage,
                    RetainedBootReason::SupervisorWatchdogReset,
                ),
                ResetQuarantineDecision::Quarantine {
                    reason: ResetQuarantineReason::JournalWriteFailed,
                    ..
                }
            ));
            storage.remaining_writes = None;
            assert!(matches!(
                prepare_reset_quarantine_boot(&mut storage, RetainedBootReason::OtherRetainedReset,),
                ResetQuarantineDecision::Quarantine { .. }
            ));
        }
    }

    #[test]
    fn healthy_lease_is_the_only_runtime_streak_clear() {
        let mut storage = TestStorage::initialized();
        let faulted = expect_run(fault_and_correlate(&mut storage));
        assert_eq!(faulted.consecutive_fault_resets(), 1);

        let cleared = complete_healthy_radio_lease(&mut storage).unwrap();
        let HealthyLeaseCommit::Cleared(history) = cleared else {
            panic!("expected a retained clear")
        };
        assert_eq!(history.consecutive_fault_resets(), 0);
        assert_eq!(history.total_fault_resets(), 1);
        let retained = expect_run(prepare_reset_quarantine_boot(
            &mut storage,
            RetainedBootReason::OtherRetainedReset,
        ));
        assert_eq!(retained.consecutive_fault_resets(), 0);
        assert_eq!(retained.total_fault_resets(), 1);
    }

    #[test]
    fn missing_retained_journal_is_not_treated_as_power_on() {
        let mut storage = TestStorage::empty();
        assert!(matches!(
            prepare_reset_quarantine_boot(&mut storage, RetainedBootReason::OtherRetainedReset,),
            ResetQuarantineDecision::Quarantine {
                reason: ResetQuarantineReason::MissingJournal,
                ..
            }
        ));
    }
}
