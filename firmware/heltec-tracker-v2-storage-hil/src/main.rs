//! Dedicated RF-inert physical-journal HIL for Heltec Wireless Tracker V2.3.
//!
//! This binary owns no executor and cannot construct a radio, LoRa PHY, or RNS
//! stack. Its only persistent device is the validated raw `retlog` partition.

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware in an unsafe state"
)]
#![deny(clippy::large_stack_frames)]

mod fixture;

use embedded_storage::nor_flash::ReadNorFlash;
use esp_backtrace as _;
use esp_bootloader_esp_idf::partitions::{PARTITION_TABLE_MAX_LEN, read_partition_table};
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
};
use esp_storage::FlashStorage;
use log::info;
use reticulum_nor_flash_region::PartitionNorFlash;
use reticulum_storage_journal::{
    AppendOutcome, JournalError, MountedJournal, append, compact, format_erased, mount,
};
use reticulum_storage_model::JournalEntry;

const REQUIRED_FLASH_BYTES: usize = 8 * 1024 * 1024;
const RETLOG_OFFSET: u32 = 0x0067_0000;
const RETLOG_LEN: u32 = 0x0010_0000;
const RETLOG_END: u32 = RETLOG_OFFSET + RETLOG_LEN;
const DATA_PARTITION_TYPE: u8 = 0x01;
const UNDEFINED_DATA_SUBTYPE: u8 = 0x06;
const CAPTURE_GUARD_MS: u32 = 5_000;
const RESET_LOG_FLUSH_MS: u32 = 100;
const RETLOG_LABEL: [u8; 16] = [
    b'r', b'e', b't', b'l', b'o', b'g', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const HEARTBEAT_PERIOD_MS: u32 = 30_000;

const _: () = assert!(CAPTURE_GUARD_MS >= 3_000);

esp_bootloader_esp_idf::esp_app_desc!();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetlogPartition {
    offset: u32,
    len: u32,
}

#[allow(
    clippy::large_stack_frames,
    reason = "the one-shot HIL boot owns the validated partition-table buffer"
)]
#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Establish the electrical interlock before logger setup, flash access, or
    // any other HIL work. SX1262 DIO2 directly owns the KCT8103L CPS net;
    // exposed GPIO46 is a separate header pin and is deliberately untouched.
    let _sx1262_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let _fem_power = Output::new(peripherals.GPIO7, Level::Low, OutputConfig::default());
    let _fem_csd = Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());
    let _fem_ctx = Output::new(peripherals.GPIO5, Level::Low, OutputConfig::default());
    let _sx1262_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());
    let _vext = Output::new(peripherals.GPIO3, Level::Low, OutputConfig::default());
    let _battery_divider = Output::new(peripherals.GPIO2, Level::Low, OutputConfig::default());

    esp_println::logger::init_logger_from_env();
    let base_mac = esp_hal::efuse::base_mac_address();
    let reset_reason = esp_hal::system::reset_reason();
    info!(
        "storage-hil stage=boot status=PASS base_mac={} reset_reason={reset_reason:?} rf_inert=true",
        base_mac,
    );
    info!(
        "storage-hil stage=rf-interlock status=PASS sx1262_reset=low fem_power=low fem_csd=low fem_ctx=low sx1262_nss=high vext=low battery_divider=low"
    );

    // Opening the ESP32-S3 native USB CDC device can itself reset the chip.
    // Give an armed recorder enough time to report that it is open and issue
    // one identity-qualified JTAG reset before this boot can access `retlog`.
    // The counted post-reset segment can then prove one coherent journal run
    // even if opening the recorder began an uncounted activation.
    info!(
        "storage-hil stage=capture-guard status=ARMED duration_ms={CAPTURE_GUARD_MS} retlog_access=false flash_mutation=false"
    );
    let capture_guard = Delay::new();
    capture_guard.delay_millis(CAPTURE_GUARD_MS);
    info!(
        "storage-hil stage=capture-guard status=COMPLETE duration_ms={CAPTURE_GUARD_MS} retlog_access=false flash_mutation=false"
    );

    if esp_hal::efuse::flash_encryption() {
        panic!("storage-hil stage=preflight status=FAIL reason=flash-encryption-enabled");
    }

    // Construct the sole FlashStorage instance. Keep its default fail-closed
    // multicore strategy; this synchronous HIL never starts the second core.
    let mut flash = FlashStorage::new(peripherals.FLASH);
    let capacity = ReadNorFlash::capacity(&flash);
    if capacity != REQUIRED_FLASH_BYTES {
        panic!(
            "storage-hil stage=preflight status=FAIL reason=flash-capacity expected={} actual={}",
            REQUIRED_FLASH_BYTES, capacity
        );
    }

    let retlog = validated_retlog_partition(&mut flash);
    info!(
        "storage-hil stage=preflight status=PASS flash_bytes={} flash_encryption=false retlog_offset=0x{:08x} retlog_len=0x{:08x} retlog_plaintext=true retlog_writable=true",
        capacity, retlog.offset, retlog.len
    );

    let mut region = PartitionNorFlash::new(&mut flash, retlog.offset, retlog.len);
    info!(
        "storage-hil stage=raw-region status=PASS write_calls={} erase_calls={}",
        region.write_calls(),
        region.erase_calls()
    );
    run_physical_journal_hil(&mut region)
}

fn validated_retlog_partition(flash: &mut FlashStorage<'_>) -> RetlogPartition {
    let mut bytes = [0_u8; PARTITION_TABLE_MAX_LEN];
    let table = read_partition_table(flash, &mut bytes).unwrap_or_else(|error| {
        panic!("storage-hil stage=partition-table status=FAIL reason=parse-or-md5 error={error:?}")
    });

    let mut named_entries = 0_u8;
    let mut found = None;
    for entry in table.iter() {
        let named_retlog = entry.label() == RETLOG_LABEL;
        if named_retlog {
            named_entries = named_entries
                .checked_add(1)
                .unwrap_or_else(|| panic!("storage-hil partition label count overflow"));
        }
        let entry_end = entry.offset().checked_add(entry.len()).unwrap_or_else(|| {
            panic!(
                "storage-hil stage=partition-table status=FAIL reason=entry-range-overflow offset=0x{:08x} len=0x{:08x}",
                entry.offset(),
                entry.len()
            )
        });
        let overlaps_retlog = entry.offset() < RETLOG_END && RETLOG_OFFSET < entry_end;
        if overlaps_retlog && !named_retlog {
            panic!(
                "storage-hil stage=partition-table status=FAIL reason=retlog-overlap offset=0x{:08x} len=0x{:08x}",
                entry.offset(),
                entry.len()
            );
        }
        if named_retlog
            && entry.raw_type() == DATA_PARTITION_TYPE
            && entry.raw_subtype() == UNDEFINED_DATA_SUBTYPE
            && entry.offset() == RETLOG_OFFSET
            && entry.len() == RETLOG_LEN
            && !entry.is_read_only()
            && !entry.is_encrypted()
            && entry.flags() == 0
        {
            found = Some(RetlogPartition {
                offset: entry.offset(),
                len: entry.len(),
            });
        }
    }

    if named_entries != 1 {
        panic!(
            "storage-hil stage=partition-table status=FAIL reason=retlog-cardinality expected=1 actual={named_entries}"
        );
    }
    found.unwrap_or_else(|| {
        panic!(
            "storage-hil stage=partition-table status=FAIL reason=retlog-shape expected_type=data expected_subtype=undefined expected_offset=0x{RETLOG_OFFSET:08x} expected_len=0x{RETLOG_LEN:08x} expected_flags=0"
        )
    })
}

fn run_physical_journal_hil<F>(flash: &mut PartitionNorFlash<'_, F>) -> !
where
    F: embedded_storage::nor_flash::MultiwriteNorFlash,
    F::Error: embedded_storage::nor_flash::NorFlashError,
{
    let records = fixture::records();
    let conflict = fixture::same_key_conflict();
    if records[fixture::RECORD_COUNT - 1] == conflict {
        panic!("storage-hil deterministic same-key conflict fixture is not contradictory");
    }
    let first_id = fixture::submission_id();
    let initial = match mount::<1, _>(flash, first_id) {
        Ok(mounted) => mounted,
        Err(JournalError::UnformattedErased) => {
            let state = format_erased(flash).unwrap_or_else(|error| {
                panic!("storage-hil stage=format status=FAIL error={error:?}")
            });
            info!(
                "storage-hil stage=format status=PASS bank={:?} generation={} records={} write_calls={} erase_calls={}",
                state.bank(),
                state.generation(),
                state.committed_records(),
                flash.write_calls(),
                flash.erase_calls()
            );
            mount::<1, _>(flash, first_id).unwrap_or_else(|error| {
                panic!("storage-hil stage=post-format-mount status=FAIL error={error:?}")
            })
        }
        Err(error) => panic!("storage-hil stage=mount status=FAIL error={error:?}"),
    };
    let initial_state = initial.state();
    info!(
        "storage-hil stage=mount status=PASS bank={:?} generation={} records={} consumed_slots={} accepted={} compaction_pending={} write_calls={} erase_calls={}",
        initial_state.bank(),
        initial_state.generation(),
        initial_state.committed_records(),
        initial_state.consumed_slots(),
        initial_state.accepted_submissions(),
        initial_state.compaction_pending(),
        flash.write_calls(),
        flash.erase_calls()
    );

    match initial_state.generation() {
        1 => run_generation_one(flash, initial, records, conflict),
        2 => run_generation_two(flash, initial, records),
        generation => panic!(
            "storage-hil stage=mount status=FAIL reason=unexpected-generation generation={generation}"
        ),
    }
}

fn run_generation_one<F>(
    flash: &mut PartitionNorFlash<'_, F>,
    initial: MountedJournal<1>,
    records: [JournalEntry; fixture::RECORD_COUNT],
    conflict: JournalEntry,
) -> !
where
    F: embedded_storage::nor_flash::MultiwriteNorFlash,
    F::Error: embedded_storage::nor_flash::NorFlashError,
{
    let first_id = fixture::submission_id();
    if !initial.state().compaction_pending() {
        for (index, record) in records.iter().copied().enumerate() {
            let outcome = append::<1, _>(flash, first_id, record).unwrap_or_else(|error| {
                panic!("storage-hil stage=seed status=FAIL record_index={index} error={error:?}")
            });
            let state = match outcome {
                AppendOutcome::Appended(state) | AppendOutcome::AlreadyEquivalent(state) => state,
            };
            info!(
                "storage-hil stage=seed status=PASS record_index={} records={} consumed_slots={} write_calls={} erase_calls={}",
                index,
                state.committed_records(),
                state.consumed_slots(),
                flash.write_calls(),
                flash.erase_calls()
            );
        }
        let seeded = mount::<1, _>(flash, first_id).unwrap_or_else(|error| {
            panic!("storage-hil stage=seed-replay status=FAIL error={error:?}")
        });
        verify_fixture(&seeded, &records);

        let writes_before_retry = flash.write_calls();
        let erases_before_retry = flash.erase_calls();
        if !matches!(
            append::<1, _>(flash, first_id, records[fixture::RECORD_COUNT - 1]),
            Ok(AppendOutcome::AlreadyEquivalent(_))
        ) || flash.write_calls() != writes_before_retry
            || flash.erase_calls() != erases_before_retry
        {
            panic!("storage-hil stage=exact-retry status=FAIL reason=write-or-outcome");
        }
        info!(
            "storage-hil stage=exact-retry status=PASS write_calls={} erase_calls={}",
            flash.write_calls(),
            flash.erase_calls()
        );

        let writes_before_conflict = flash.write_calls();
        let erases_before_conflict = flash.erase_calls();
        if !matches!(
            append::<1, _>(flash, first_id, conflict),
            Err(JournalError::LogicalConflict)
        ) || flash.write_calls() != writes_before_conflict
            || flash.erase_calls() != erases_before_conflict
        {
            panic!("storage-hil stage=logical-conflict status=FAIL reason=write-or-outcome");
        }
        info!(
            "storage-hil stage=logical-conflict status=PASS write_calls={} erase_calls={}",
            flash.write_calls(),
            flash.erase_calls()
        );
    }

    let erases_before_compact = flash.erase_calls();
    let compacted = compact::<1, _>(flash, first_id)
        .unwrap_or_else(|error| panic!("storage-hil stage=compact status=FAIL error={error:?}"));
    let expected_erases = erases_before_compact
        .checked_add(3)
        .unwrap_or_else(|| panic!("storage-hil erase-call counter overflow"));
    if compacted.generation() != 2
        || compacted.committed_records() != fixture::RECORD_COUNT
        || compacted.compaction_pending()
        || flash.erase_calls() != expected_erases
    {
        panic!(
            "storage-hil stage=compact status=FAIL reason=post-state generation={} records={} pending={} erase_delta={} expected_erase_delta=3",
            compacted.generation(),
            compacted.committed_records(),
            compacted.compaction_pending(),
            flash.erase_calls().saturating_sub(erases_before_compact)
        );
    }
    info!(
        "storage-hil stage=compact status=PASS bank={:?} generation={} records={} consumed_slots={} write_calls={} erase_calls={}",
        compacted.bank(),
        compacted.generation(),
        compacted.committed_records(),
        compacted.consumed_slots(),
        flash.write_calls(),
        flash.erase_calls()
    );
    info!(
        "storage-hil stage=software-reset status=ARMED reason=post-compaction source_generation=1 target_generation=2 delay_ms=250 rf_inert=true"
    );
    let delay = Delay::new();
    delay.delay_millis(250);
    info!(
        "storage-hil stage=software-reset status=ISSUED reason=post-compaction source_generation=1 target_generation=2 flush_ms={RESET_LOG_FLUSH_MS} rf_inert=true"
    );
    delay.delay_millis(RESET_LOG_FLUSH_MS);
    esp_hal::system::software_reset()
}

fn run_generation_two<F>(
    flash: &mut PartitionNorFlash<'_, F>,
    initial: MountedJournal<1>,
    records: [JournalEntry; fixture::RECORD_COUNT],
) -> !
where
    F: embedded_storage::nor_flash::MultiwriteNorFlash,
    F::Error: embedded_storage::nor_flash::NorFlashError,
{
    verify_fixture(&initial, &records);
    let initial_state = initial.state();
    if initial_state.compaction_pending() {
        let erases_before_retirement = flash.erase_calls();
        let retired = compact::<1, _>(flash, fixture::submission_id()).unwrap_or_else(|error| {
            panic!("storage-hil stage=manifest-retirement status=FAIL error={error:?}")
        });
        let expected_erases = erases_before_retirement
            .checked_add(1)
            .unwrap_or_else(|| panic!("storage-hil erase-call counter overflow"));
        if retired.bank() != initial_state.bank()
            || retired.generation() != 2
            || retired.committed_records() != fixture::RECORD_COUNT
            || retired.compaction_pending()
            || flash.erase_calls() != expected_erases
        {
            panic!(
                "storage-hil stage=manifest-retirement status=FAIL reason=post-state bank={:?} generation={} records={} pending={} erase_delta={} expected_erase_delta=1",
                retired.bank(),
                retired.generation(),
                retired.committed_records(),
                retired.compaction_pending(),
                flash.erase_calls().saturating_sub(erases_before_retirement)
            );
        }
        let replayed = mount::<1, _>(flash, fixture::submission_id()).unwrap_or_else(|error| {
            panic!("storage-hil stage=post-retirement-replay status=FAIL error={error:?}")
        });
        verify_fixture(&replayed, &records);
        info!(
            "storage-hil stage=manifest-retirement status=PASS bank={:?} generation=2 records={} erase_delta=1 write_calls={} erase_calls={} rf_inert=true",
            retired.bank(),
            retired.committed_records(),
            flash.write_calls(),
            flash.erase_calls()
        );
        info!(
            "storage-hil stage=software-reset status=ARMED reason=post-manifest-retirement source_generation=2 target_generation=2 delay_ms=250 rf_inert=true"
        );
        let delay = Delay::new();
        delay.delay_millis(250);
        info!(
            "storage-hil stage=software-reset status=ISSUED reason=post-manifest-retirement source_generation=2 target_generation=2 flush_ms={RESET_LOG_FLUSH_MS} rf_inert=true"
        );
        delay.delay_millis(RESET_LOG_FLUSH_MS);
        esp_hal::system::software_reset()
    }

    info!(
        "storage-hil stage=final-replay status=PASS bank={:?} generation=2 records={} accepted={} write_calls={} erase_calls={} rf_inert=true",
        initial_state.bank(),
        initial_state.committed_records(),
        initial_state.accepted_submissions(),
        flash.write_calls(),
        flash.erase_calls()
    );
    hold_pass(flash)
}

fn verify_fixture(mounted: &MountedJournal<1>, records: &[JournalEntry; fixture::RECORD_COUNT]) {
    let state = mounted.state();
    if state.committed_records() != fixture::RECORD_COUNT || state.accepted_submissions() != 1 {
        panic!(
            "storage-hil stage=semantic-replay status=FAIL reason=count records={} accepted={}",
            state.committed_records(),
            state.accepted_submissions()
        );
    }
    let indexed = mounted
        .index()
        .get(fixture::submission_id())
        .unwrap_or_else(|| panic!("storage-hil stage=semantic-replay status=FAIL reason=missing"));
    let JournalEntry::Accepted(expected_accepted) = records[0] else {
        panic!("storage-hil first fixture record must be an acceptance")
    };
    if indexed.accepted().authorization() != expected_accepted.authorization() {
        panic!("storage-hil stage=semantic-replay status=FAIL reason=authorization-provenance");
    }
    let JournalEntry::StateTransition(expected) = records[fixture::RECORD_COUNT - 1] else {
        panic!("storage-hil final fixture record must be a transition")
    };
    if indexed.revision() != expected.revision() || indexed.state() != expected.state() {
        panic!(
            "storage-hil stage=semantic-replay status=FAIL reason=final-state revision={} expected_revision={} state={:?} expected_state={:?}",
            indexed.revision(),
            expected.revision(),
            indexed.state(),
            expected.state()
        );
    }
    info!(
        "storage-hil stage=semantic-replay status=PASS revision={} state={:?}",
        indexed.revision(),
        indexed.state()
    );
}

fn hold_pass<F>(flash: &PartitionNorFlash<'_, F>) -> ! {
    let delay = Delay::new();
    loop {
        delay.delay_millis(HEARTBEAT_PERIOD_MS);
        info!(
            "storage-hil heartbeat stage=complete status=PASS generation=2 write_calls={} erase_calls={} rf_inert=true",
            flash.write_calls(),
            flash.erase_calls()
        );
    }
}
