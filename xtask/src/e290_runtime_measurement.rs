//! Decode the fixed E290 runtime-measurement HIL evidence ABI and inspect its
//! linked stack bounds.

use object::{
    Architecture, BinaryFormat, Endianness, Object, ObjectKind, ObjectSection, ObjectSymbol,
    SectionKind, SymbolSection,
};
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

const WORD_COUNT: usize = 64;
const BYTE_SIZE: usize = WORD_COUNT * size_of::<u32>();
const MAGIC: u32 = u32::from_le_bytes(*b"RTME");
const VERSION: u32 = 1;
const UNOBSERVED_MINIMUM: u32 = u32::MAX;

const PROOF_WORD_COUNT: usize = 48;
const PROOF_BYTE_SIZE: usize = PROOF_WORD_COUNT * size_of::<u32>();
const PROOF_MAGIC: u32 = u32::from_le_bytes(*b"RPTE");
const PROOF_VERSION: u32 = 1;
const PROOF_TRACE_EVIDENCE_SYMBOL_FRAGMENT: &str = "RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE";

const PROOF_FLAG_ACTIVE: u32 = 1 << 0;
const PROOF_FLAG_SATURATED: u32 = 1 << 1;
const PROOF_FLAG_GENERATED_TAG_PRESENT: u32 = 1 << 2;
const PROOF_FLAG_GENERATED_TAGS_CONSISTENT: u32 = 1 << 3;
const PROOF_FLAG_DELIVERED_TAG_PRESENT: u32 = 1 << 4;
const PROOF_FLAG_DELIVERED_TAGS_CONSISTENT: u32 = 1 << 5;
const PROOF_FLAG_TIMEOUT_TAG_PRESENT: u32 = 1 << 6;
const PROOF_FLAG_TIMEOUT_TAGS_CONSISTENT: u32 = 1 << 7;
const PROOF_FLAG_INBOX_COMMIT_IN_PROGRESS: u32 = 1 << 8;
const PROOF_FLAG_INBOX_COMMIT_ORDER_CONSISTENT: u32 = 1 << 9;
const PROOF_FLAG_INPUT_INCONSISTENT: u32 = 1 << 10;
const PROOF_KNOWN_FLAG_MASK: u32 = PROOF_FLAG_ACTIVE
    | PROOF_FLAG_SATURATED
    | PROOF_FLAG_GENERATED_TAG_PRESENT
    | PROOF_FLAG_GENERATED_TAGS_CONSISTENT
    | PROOF_FLAG_DELIVERED_TAG_PRESENT
    | PROOF_FLAG_DELIVERED_TAGS_CONSISTENT
    | PROOF_FLAG_TIMEOUT_TAG_PRESENT
    | PROOF_FLAG_TIMEOUT_TAGS_CONSISTENT
    | PROOF_FLAG_INBOX_COMMIT_IN_PROGRESS
    | PROOF_FLAG_INBOX_COMMIT_ORDER_CONSISTENT
    | PROOF_FLAG_INPUT_INCONSISTENT;

const PROOF_FLAG_NAMES: [(&str, u32); 11] = [
    ("flags.active", PROOF_FLAG_ACTIVE),
    ("flags.saturated", PROOF_FLAG_SATURATED),
    (
        "flags.generated_tag_present",
        PROOF_FLAG_GENERATED_TAG_PRESENT,
    ),
    (
        "flags.generated_tags_consistent",
        PROOF_FLAG_GENERATED_TAGS_CONSISTENT,
    ),
    (
        "flags.delivered_tag_present",
        PROOF_FLAG_DELIVERED_TAG_PRESENT,
    ),
    (
        "flags.delivered_tags_consistent",
        PROOF_FLAG_DELIVERED_TAGS_CONSISTENT,
    ),
    ("flags.timeout_tag_present", PROOF_FLAG_TIMEOUT_TAG_PRESENT),
    (
        "flags.timeout_tags_consistent",
        PROOF_FLAG_TIMEOUT_TAGS_CONSISTENT,
    ),
    (
        "flags.inbox_commit_in_progress",
        PROOF_FLAG_INBOX_COMMIT_IN_PROGRESS,
    ),
    (
        "flags.inbox_commit_order_consistent",
        PROOF_FLAG_INBOX_COMMIT_ORDER_CONSISTENT,
    ),
    ("flags.input_inconsistent", PROOF_FLAG_INPUT_INCONSISTENT),
];

const PROOF_WORD_NAMES: [&str; PROOF_WORD_COUNT] = [
    "snapshot_seq_begin",
    "magic",
    "version",
    "size_bytes",
    "flags.raw",
    "logical_rx.completed.count",
    "logical_rx.completed.last_ms",
    "ingress.enqueue.count",
    "ingress.enqueue.last_ms",
    "ingress.defer.count",
    "ingress.defer.last_ms",
    "ingress.fail.count",
    "ingress.fail.last_ms",
    "rns_ingress.count",
    "rns_ingress.last_ms",
    "proof.generated.count",
    "proof.generated.last_ms",
    "receipt.delivered.count",
    "receipt.delivered.last_ms",
    "receipt.timeout.count",
    "receipt.timeout.last_ms",
    "action.pressure.count",
    "action.pressure.last_ms",
    "correlation.fault.count",
    "correlation.fault.last_ms",
    "inbox.commit.count",
    "inbox.commit.last_start_ms",
    "inbox.commit.last_end_ms",
    "disposition.processed.count",
    "disposition.native_duplicate.count",
    "disposition.native_invalid.count",
    "disposition.no_observable_outcome.count",
    "disposition.rejected.count",
    "rns_ingress.last_disposition",
    "rns_ingress.last_wire_packet_type",
    "rns_ingress.last_emitted_packets",
    "rns_ingress.last_generated_proof_actions",
    "rns_ingress.last_delivered_receipt_terminals",
    "rns_ingress.last_timed_out_receipt_terminals",
    "tag.generated.low",
    "tag.generated.high",
    "tag.delivered.low",
    "tag.delivered.high",
    "tag.timeout.low",
    "tag.timeout.high",
    "radio_tx.confirmed_success.count",
    "radio_tx.not_confirmed_success.count",
    "snapshot_seq_end",
];

const PROOF_SNAPSHOT_SEQ_BEGIN_WORD: usize = 0;
const PROOF_MAGIC_WORD: usize = 1;
const PROOF_VERSION_WORD: usize = 2;
const PROOF_SIZE_WORD: usize = 3;
const PROOF_FLAGS_WORD: usize = 4;
const PROOF_LOGICAL_RX_COUNT_WORD: usize = 5;
const PROOF_LOGICAL_RX_LAST_MS_WORD: usize = 6;
const PROOF_INGRESS_ENQUEUE_COUNT_WORD: usize = 7;
const PROOF_INGRESS_ENQUEUE_LAST_MS_WORD: usize = 8;
const PROOF_INGRESS_DEFER_COUNT_WORD: usize = 9;
const PROOF_INGRESS_DEFER_LAST_MS_WORD: usize = 10;
const PROOF_INGRESS_FAIL_COUNT_WORD: usize = 11;
const PROOF_INGRESS_FAIL_LAST_MS_WORD: usize = 12;
const PROOF_RNS_INGRESS_COUNT_WORD: usize = 13;
const PROOF_RNS_INGRESS_LAST_MS_WORD: usize = 14;
const PROOF_GENERATED_COUNT_WORD: usize = 15;
const PROOF_GENERATED_LAST_MS_WORD: usize = 16;
const PROOF_DELIVERED_COUNT_WORD: usize = 17;
const PROOF_DELIVERED_LAST_MS_WORD: usize = 18;
const PROOF_TIMEOUT_COUNT_WORD: usize = 19;
const PROOF_TIMEOUT_LAST_MS_WORD: usize = 20;
const PROOF_ACTION_PRESSURE_COUNT_WORD: usize = 21;
const PROOF_ACTION_PRESSURE_LAST_MS_WORD: usize = 22;
const PROOF_CORRELATION_FAULT_COUNT_WORD: usize = 23;
const PROOF_CORRELATION_FAULT_LAST_MS_WORD: usize = 24;
const PROOF_INBOX_COMMIT_COUNT_WORD: usize = 25;
const PROOF_INBOX_COMMIT_START_MS_WORD: usize = 26;
const PROOF_INBOX_COMMIT_END_MS_WORD: usize = 27;
const PROOF_DISPOSITION_PROCESSED_WORD: usize = 28;
const PROOF_DISPOSITION_DUPLICATE_WORD: usize = 29;
const PROOF_DISPOSITION_INVALID_WORD: usize = 30;
const PROOF_DISPOSITION_NO_OUTCOME_WORD: usize = 31;
const PROOF_DISPOSITION_REJECTED_WORD: usize = 32;
const PROOF_LAST_DISPOSITION_WORD: usize = 33;
const PROOF_LAST_PACKET_TYPE_WORD: usize = 34;
const PROOF_LAST_EMITTED_PACKETS_WORD: usize = 35;
const PROOF_LAST_GENERATED_ACTIONS_WORD: usize = 36;
const PROOF_LAST_DELIVERED_TERMINALS_WORD: usize = 37;
const PROOF_LAST_TIMED_OUT_TERMINALS_WORD: usize = 38;
const PROOF_GENERATED_TAG_LOW_WORD: usize = 39;
const PROOF_GENERATED_TAG_HIGH_WORD: usize = 40;
const PROOF_DELIVERED_TAG_LOW_WORD: usize = 41;
const PROOF_DELIVERED_TAG_HIGH_WORD: usize = 42;
const PROOF_TIMEOUT_TAG_LOW_WORD: usize = 43;
const PROOF_TIMEOUT_TAG_HIGH_WORD: usize = 44;
const PROOF_RADIO_TX_CONFIRMED_SUCCESS_COUNT_WORD: usize = 45;
const PROOF_RADIO_TX_NOT_CONFIRMED_SUCCESS_COUNT_WORD: usize = 46;
const PROOF_SNAPSHOT_SEQ_END_WORD: usize = 47;

// The powered 2026-07-20 qualification observed 72,212 bytes of raw painted
// stack margin. RPTE v1 adds one exact 192-byte initialized internal-RAM
// object, and the linked stack boundary moves down by the same 192 bytes. Until
// a fresh powered trace supersedes it, carry the earlier watermark forward
// conservatively after that exact linked-RAM deduction. This does not turn the
// modified-word watermark into minimum-SP proof; the unchanged 52,752-byte
// compiler-frame ceiling leaves a derived 19,268-byte margin.
const PRIOR_QUALIFIED_RAW_STACK_MARGIN_BYTES: u64 = 72_212;
const PROOF_TRACE_LINKED_STACK_REDUCTION_BYTES: u64 = PROOF_BYTE_SIZE as u64;
const QUALIFIED_RAW_STACK_MARGIN_BYTES: u64 =
    PRIOR_QUALIFIED_RAW_STACK_MARGIN_BYTES - PROOF_TRACE_LINKED_STACK_REDUCTION_BYTES;
const MAXIMUM_STACK_FRAME_BYTES: u64 = 52_752;
const MINIMUM_CONSERVATIVE_STACK_MARGIN_BYTES: u64 =
    QUALIFIED_RAW_STACK_MARGIN_BYTES - MAXIMUM_STACK_FRAME_BYTES;
const MINIMUM_DEFAULT_USABLE_STACK_BYTES: u64 = 170_984;
const MINIMUM_HIL_USABLE_STACK_BYTES: u64 = 170_288;
const EXPECTED_STACK_GUARD_OFFSET_BYTES: u64 = 60;
const STACK_GUARD_WORD_BYTES: u64 = size_of::<u32>() as u64;

const FLAG_ACTIVE: u32 = 1 << 0;
const FLAG_STACK_INITIALIZED: u32 = 1 << 1;
const FLAG_HEAP_REGISTERED: u32 = 1 << 2;
const FLAG_COMPOSITION_READY: u32 = 1 << 3;
const FLAG_SCAN_VALID: u32 = 1 << 4;
const FLAG_GUARD_INTACT: u32 = 1 << 5;
const FLAG_SATURATED: u32 = 1 << 6;
const KNOWN_FLAG_MASK: u32 = FLAG_ACTIVE
    | FLAG_STACK_INITIALIZED
    | FLAG_HEAP_REGISTERED
    | FLAG_COMPOSITION_READY
    | FLAG_SCAN_VALID
    | FLAG_GUARD_INTACT
    | FLAG_SATURATED;

const FLAG_NAMES: [(&str, u32); 7] = [
    ("flags.active", FLAG_ACTIVE),
    ("flags.stack_initialized", FLAG_STACK_INITIALIZED),
    ("flags.heap_registered", FLAG_HEAP_REGISTERED),
    ("flags.composition_ready", FLAG_COMPOSITION_READY),
    ("flags.scan_valid", FLAG_SCAN_VALID),
    ("flags.guard_intact", FLAG_GUARD_INTACT),
    ("flags.saturated", FLAG_SATURATED),
];

/// Canonical version-1 word order and stable host-facing field names.
const WORD_NAMES: [&str; WORD_COUNT] = [
    "snapshot_seq_begin",
    "magic",
    "version",
    "size_bytes",
    "flags.raw",
    "init_error",
    "uptime_ms",
    "memory.psram_bytes",
    "memory.heap_total_bytes",
    "memory.heap_current_bytes",
    "memory.heap_maximum_bytes",
    "memory.heap_minimum_free_bytes",
    "memory.internal_heap_current_bytes",
    "memory.internal_heap_minimum_free_bytes",
    "memory.external_heap_current_bytes",
    "memory.external_heap_minimum_free_bytes",
    "stack.reserved_bytes",
    "stack.usable_bytes",
    "stack.painted_bytes",
    "stack.high_water_bytes",
    "stack.minimum_remaining_bytes",
    "stack.guard_offset_bytes",
    "composition_ready_us",
    "boot.credential_boot.last_us",
    "boot.credential_boot.max_us",
    "boot.identity_preflight.last_us",
    "boot.identity_preflight.max_us",
    "boot.journal_provision.last_us",
    "boot.journal_provision.max_us",
    "boot.announce_epoch.last_us",
    "boot.announce_epoch.max_us",
    "boot.identity_boot.last_us",
    "boot.identity_boot.max_us",
    "boot.journal_mount.last_us",
    "boot.journal_mount.max_us",
    "boot.inbox_mount.last_us",
    "boot.inbox_mount.max_us",
    "boot.radio_init.last_us",
    "boot.radio_init.max_us",
    "operation.inbound.count",
    "operation.inbound.max_us",
    "operation.authorized_frame.count",
    "operation.authorized_frame.max_us",
    "operation.submission.count",
    "operation.submission.max_us",
    "operation.api_dispatch.count",
    "operation.api_dispatch.max_us",
    "operation.rx.count",
    "operation.rx.max_us",
    "operation.rx.timeout_count",
    "operation.cad.count",
    "operation.cad.max_us",
    "operation.cad.timeout_count",
    "operation.tx.count",
    "operation.tx.max_us",
    "operation.tx.timeout_count",
    "scheduler.node_loop_gap_max_us",
    "scheduler.radio_loop_gap_max_us",
    "scheduler.measurement_lateness_max_us",
    "scheduler.measurement_work_max_us",
    "errors.unexpected_count",
    "allocation.count",
    "allocation.failed_count",
    "snapshot_seq_end",
];

const SNAPSHOT_SEQ_BEGIN_WORD: usize = 0;
const MAGIC_WORD: usize = 1;
const VERSION_WORD: usize = 2;
const SIZE_WORD: usize = 3;
const FLAGS_WORD: usize = 4;
const INIT_ERROR_WORD: usize = 5;
const UPTIME_WORD: usize = 6;
const PSRAM_BYTES_WORD: usize = 7;
const HEAP_TOTAL_WORD: usize = 8;
const HEAP_CURRENT_WORD: usize = 9;
const HEAP_MAXIMUM_WORD: usize = 10;
const HEAP_MINIMUM_FREE_WORD: usize = 11;
const INTERNAL_HEAP_CURRENT_WORD: usize = 12;
const INTERNAL_HEAP_MINIMUM_FREE_WORD: usize = 13;
const EXTERNAL_HEAP_CURRENT_WORD: usize = 14;
const EXTERNAL_HEAP_MINIMUM_FREE_WORD: usize = 15;
const STACK_RESERVED_WORD: usize = 16;
const STACK_USABLE_WORD: usize = 17;
const STACK_PAINTED_WORD: usize = 18;
const STACK_HIGH_WATER_WORD: usize = 19;
const STACK_MINIMUM_REMAINING_WORD: usize = 20;
const STACK_GUARD_OFFSET_WORD: usize = 21;
const COMPOSITION_READY_US_WORD: usize = 22;
const BOOT_FIRST_WORD: usize = 23;
const BOOT_LAST_WORD: usize = 38;
const INBOUND_COUNT_WORD: usize = 39;
const INBOUND_MAXIMUM_WORD: usize = 40;
const AUTHORIZED_COUNT_WORD: usize = 41;
const AUTHORIZED_MAXIMUM_WORD: usize = 42;
const SUBMISSION_COUNT_WORD: usize = 43;
const SUBMISSION_MAXIMUM_WORD: usize = 44;
const API_COUNT_WORD: usize = 45;
const API_MAXIMUM_WORD: usize = 46;
const RX_COUNT_WORD: usize = 47;
const RX_MAXIMUM_WORD: usize = 48;
const RX_TIMEOUT_WORD: usize = 49;
const CAD_COUNT_WORD: usize = 50;
const CAD_MAXIMUM_WORD: usize = 51;
const CAD_TIMEOUT_WORD: usize = 52;
const TX_COUNT_WORD: usize = 53;
const TX_MAXIMUM_WORD: usize = 54;
const TX_TIMEOUT_WORD: usize = 55;
const NODE_LOOP_GAP_MAXIMUM_WORD: usize = 56;
const RADIO_LOOP_GAP_MAXIMUM_WORD: usize = 57;
const MEASUREMENT_LATENESS_MAXIMUM_WORD: usize = 58;
const MEASUREMENT_WORK_MAXIMUM_WORD: usize = 59;
const UNEXPECTED_ERROR_COUNT_WORD: usize = 60;
const ALLOCATION_COUNT_WORD: usize = 61;
const FAILED_ALLOCATION_COUNT_WORD: usize = 62;
const SNAPSHOT_SEQ_END_WORD: usize = 63;

#[derive(Debug, Eq, PartialEq)]
struct Options {
    input: PathBuf,
    json: bool,
}

#[derive(Debug, Eq, PartialEq)]
enum CommandOptions {
    Decode(Options),
    DecodeProofTrace(Options),
    InspectElf(ElfInspectionOptions),
}

#[derive(Debug, Eq, PartialEq)]
struct ElfInspectionOptions {
    default_elf: PathBuf,
    hil_elf: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StackSizeInventory {
    record_count: u64,
    maximum_frame_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StackLayout {
    reserved_bytes: u64,
    usable_bytes: u64,
    guard_offset_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ElfInspection {
    default_stack_sizes: StackSizeInventory,
    default_stack: StackLayout,
    default_proof_trace_symbol_count: u64,
    hil_stack_sizes: StackSizeInventory,
    hil_stack: StackLayout,
    hil_proof_trace_symbol_count: u64,
    hil_proof_trace_symbol_size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedEvidence {
    words: [u32; WORD_COUNT],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedProofTraceEvidence {
    words: [u32; PROOF_WORD_COUNT],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputValue {
    Number(u32),
    Bool(bool),
    Text(&'static str),
    Unobserved,
}

pub(crate) fn run(args: Vec<String>) -> ExitCode {
    let options = match parse_command_options(&args) {
        Ok(options) => options,
        Err(reason) => {
            eprintln!("error: {reason}");
            usage();
            return ExitCode::from(2);
        }
    };

    let result = match options {
        CommandOptions::Decode(options) => execute(&options),
        CommandOptions::DecodeProofTrace(options) => execute_proof_trace(&options),
        CommandOptions::InspectElf(options) => {
            inspect_elf_pair(&options).map(|value| value.render())
        }
    };
    match result {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("error: {reason}");
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "usage:\n  cargo run -p xtask -- e290-runtime-measurement decode \
         --input <256-byte-bin> [--json]\n  cargo run -p xtask -- \
         e290-runtime-measurement decode-proof-trace \
         --input <192-byte-bin> [--json]\n  cargo run -p xtask -- \
         e290-runtime-measurement inspect-elf --default-elf <path> \
         --hil-elf <path>"
    );
}

fn parse_command_options(args: &[String]) -> Result<CommandOptions, String> {
    match args.first().map(String::as_str) {
        Some("decode") => parse_options(args).map(CommandOptions::Decode),
        Some("decode-proof-trace") => {
            parse_proof_trace_options(args).map(CommandOptions::DecodeProofTrace)
        }
        Some("inspect-elf") => {
            parse_elf_inspection_options(&args[1..]).map(CommandOptions::InspectElf)
        }
        Some(value) => Err(format!("unknown subcommand {value}")),
        None => Err("decode or inspect-elf subcommand is required".to_owned()),
    }
}

fn parse_proof_trace_options(args: &[String]) -> Result<Options, String> {
    match args.first().map(String::as_str) {
        Some("decode-proof-trace") => {}
        Some(_) => return Err("subcommand must be decode-proof-trace".to_owned()),
        None => return Err("decode-proof-trace subcommand is required".to_owned()),
    }

    let mut input = None;
    let mut json = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--input" => {
                if input.is_some() {
                    return Err("--input may be supplied only once".to_owned());
                }
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| "--input requires a value".to_owned())?;
                if value.is_empty() {
                    return Err("--input must not be empty".to_owned());
                }
                input = Some(PathBuf::from(value));
                index += 2;
            }
            "--json" => {
                if json {
                    return Err("--json may be supplied only once".to_owned());
                }
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => return Err(format!("unknown option {value}")),
            value => return Err(format!("unexpected argument {value}")),
        }
    }

    Ok(Options {
        input: input.ok_or_else(|| "--input is required".to_owned())?,
        json,
    })
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    match args.first().map(String::as_str) {
        Some("decode") => {}
        Some(_) => return Err("subcommand must be decode".to_owned()),
        None => return Err("decode subcommand is required".to_owned()),
    }

    let mut input = None;
    let mut json = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--input" => {
                if input.is_some() {
                    return Err("--input may be supplied only once".to_owned());
                }
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| "--input requires a value".to_owned())?;
                if value.is_empty() {
                    return Err("--input must not be empty".to_owned());
                }
                input = Some(PathBuf::from(value));
                index += 2;
            }
            "--json" => {
                if json {
                    return Err("--json may be supplied only once".to_owned());
                }
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => return Err(format!("unknown option {value}")),
            value => return Err(format!("unexpected argument {value}")),
        }
    }

    Ok(Options {
        input: input.ok_or_else(|| "--input is required".to_owned())?,
        json,
    })
}

fn parse_elf_inspection_options(args: &[String]) -> Result<ElfInspectionOptions, String> {
    let mut default_elf = None;
    let mut hil_elf = None;
    let mut index = 0;
    while index < args.len() {
        let (slot, option) = match args[index].as_str() {
            "--default-elf" => (&mut default_elf, "--default-elf"),
            "--hil-elf" => (&mut hil_elf, "--hil-elf"),
            value if value.starts_with('-') => return Err(format!("unknown option {value}")),
            value => return Err(format!("unexpected argument {value}")),
        };
        if slot.is_some() {
            return Err(format!("{option} may be supplied only once"));
        }
        let value = args
            .get(index + 1)
            .filter(|value| !value.starts_with('-'))
            .ok_or_else(|| format!("{option} requires a value"))?;
        if value.is_empty() {
            return Err(format!("{option} must not be empty"));
        }
        *slot = Some(PathBuf::from(value));
        index += 2;
    }

    Ok(ElfInspectionOptions {
        default_elf: default_elf.ok_or_else(|| "--default-elf is required".to_owned())?,
        hil_elf: hil_elf.ok_or_else(|| "--hil-elf is required".to_owned())?,
    })
}

fn execute(options: &Options) -> Result<String, String> {
    let bytes = fs::read(&options.input).map_err(|error| {
        format!(
            "could not read --input {}: {error}",
            options.input.display()
        )
    })?;
    let evidence = DecodedEvidence::parse(&bytes)?;
    Ok(if options.json {
        evidence.render_json()
    } else {
        evidence.render_human()
    })
}

fn execute_proof_trace(options: &Options) -> Result<String, String> {
    let bytes = fs::read(&options.input).map_err(|error| {
        format!(
            "could not read --input {}: {error}",
            options.input.display()
        )
    })?;
    let evidence = DecodedProofTraceEvidence::parse(&bytes)?;
    Ok(if options.json {
        evidence.render_json()
    } else {
        evidence.render_human()
    })
}

fn inspect_elf_pair(options: &ElfInspectionOptions) -> Result<ElfInspection, String> {
    let (default_stack_sizes, default_stack) = inspect_elf(&options.default_elf, "default E290")?;
    let (hil_stack_sizes, hil_stack) = inspect_elf(&options.hil_elf, "runtime-measurement HIL")?;
    let (default_proof_trace_symbol_count, _) =
        inspect_proof_trace_symbol(&options.default_elf, "default E290", false)?;
    let (hil_proof_trace_symbol_count, hil_proof_trace_symbol_size_bytes) =
        inspect_proof_trace_symbol(&options.hil_elf, "runtime-measurement HIL", true)?;
    let inspection = ElfInspection {
        default_stack_sizes,
        default_stack,
        default_proof_trace_symbol_count,
        hil_stack_sizes,
        hil_stack,
        hil_proof_trace_symbol_count,
        hil_proof_trace_symbol_size_bytes,
    };
    inspection.validate()?;
    Ok(inspection)
}

fn inspect_elf(path: &Path, label: &str) -> Result<(StackSizeInventory, StackLayout), String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read {label} ELF {}: {error}", path.display()))?;
    let object = parse_xtensa_elf(&bytes, path, label)?;
    let stack_sizes = stack_size_inventory(&object, path, label)?;
    let stack_end = unique_symbol_address(&object, path, label, "_stack_end_cpu0")?;
    let stack_guard = unique_symbol_address(&object, path, label, "__stack_chk_guard")?;
    let stack_start = unique_symbol_address(&object, path, label, "_stack_start_cpu0")?;
    let stack = calculate_stack_layout(label, stack_end, stack_guard, stack_start)?;
    Ok((stack_sizes, stack))
}

fn inspect_proof_trace_symbol(
    path: &Path,
    label: &str,
    required: bool,
) -> Result<(u64, u64), String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read {label} ELF {}: {error}", path.display()))?;
    let object = parse_xtensa_elf(&bytes, path, label)?;
    let mut symbols = object.symbols().filter(|symbol| {
        symbol
            .name()
            .is_ok_and(|name| name.contains(PROOF_TRACE_EVIDENCE_SYMBOL_FRAGMENT))
            && symbol.section() != SymbolSection::Undefined
    });
    let first = symbols.next();
    let count = u64::from(first.is_some()) + symbols.count() as u64;
    if !required {
        if count != 0 {
            return Err(format!(
                "{label} ELF {} must exclude {PROOF_TRACE_EVIDENCE_SYMBOL_FRAGMENT}, found {count} defined symbols",
                path.display()
            ));
        }
        return Ok((0, 0));
    }
    if count != 1 {
        return Err(format!(
            "{label} ELF {} must contain exactly one defined {PROOF_TRACE_EVIDENCE_SYMBOL_FRAGMENT}, found {count}",
            path.display()
        ));
    }

    let symbol = first.expect("one required proof-trace symbol was counted");
    if symbol.size() != PROOF_BYTE_SIZE as u64 {
        return Err(format!(
            "{label} ELF {} proof-trace symbol must be exactly {PROOF_BYTE_SIZE} bytes, got {}",
            path.display(),
            symbol.size()
        ));
    }
    let section_index = match symbol.section() {
        SymbolSection::Section(index) => index,
        section => {
            return Err(format!(
                "{label} ELF {} proof-trace symbol must belong to one initialized data section, got {section:?}",
                path.display()
            ));
        }
    };
    let section = object.section_by_index(section_index).map_err(|error| {
        format!(
            "could not resolve {label} proof-trace section in {}: {error}",
            path.display()
        )
    })?;
    if section.kind() == SectionKind::UninitializedData {
        return Err(format!(
            "{label} ELF {} proof-trace symbol must be initialized, not BSS",
            path.display()
        ));
    }
    let offset = symbol
        .address()
        .checked_sub(section.address())
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            format!(
                "{label} ELF {} proof-trace symbol address is outside its section",
                path.display()
            )
        })?;
    let end = offset.checked_add(PROOF_BYTE_SIZE).ok_or_else(|| {
        format!(
            "{label} ELF {} proof-trace symbol range overflows",
            path.display()
        )
    })?;
    let section_data = section.data().map_err(|error| {
        format!(
            "could not read initialized {label} proof-trace section in {}: {error}",
            path.display()
        )
    })?;
    let initialized = section_data.get(offset..end).ok_or_else(|| {
        format!(
            "{label} ELF {} proof-trace symbol bytes are outside initialized section data",
            path.display()
        )
    })?;
    let evidence = DecodedProofTraceEvidence::parse(initialized).map_err(|error| {
        format!(
            "{label} ELF {} proof-trace symbol has invalid initialized ABI bytes: {error}",
            path.display()
        )
    })?;
    evidence.validate_empty_initializer().map_err(|error| {
        format!(
            "{label} ELF {} proof-trace symbol is not an empty initialized record: {error}",
            path.display()
        )
    })?;
    Ok((count, symbol.size()))
}

fn parse_xtensa_elf<'data>(
    bytes: &'data [u8],
    path: &Path,
    label: &str,
) -> Result<object::File<'data>, String> {
    let object = object::File::parse(bytes)
        .map_err(|error| format!("could not parse {label} ELF {}: {error}", path.display()))?;
    if object.format() != BinaryFormat::Elf || object.architecture() != Architecture::Xtensa {
        return Err(format!(
            "{label} input {} must be a linked Xtensa ELF, got {:?} {:?}",
            path.display(),
            object.format(),
            object.architecture()
        ));
    }
    if object.kind() != ObjectKind::Executable {
        return Err(format!(
            "{label} input {} must be a final ET_EXEC image, got {:?}",
            path.display(),
            object.kind()
        ));
    }
    if object.endianness() != Endianness::Little {
        return Err(format!(
            "{label} input {} must be little-endian",
            path.display()
        ));
    }
    if object.is_64() {
        return Err(format!(
            "{label} input {} must use 32-bit Xtensa addresses",
            path.display()
        ));
    }
    Ok(object)
}

fn stack_size_inventory(
    object: &object::File<'_>,
    path: &Path,
    label: &str,
) -> Result<StackSizeInventory, String> {
    let mut sections = object
        .sections()
        .filter(|section| section.name().is_ok_and(|name| name == ".stack_sizes"));
    let section = sections.next().ok_or_else(|| {
        format!(
            "{label} ELF {} has no .stack_sizes section; build the final linked image with RUSTFLAGS='-C link-arg=-nostartfiles -Z emit-stack-sizes'",
            path.display()
        )
    })?;
    if sections.next().is_some() {
        return Err(format!(
            "{label} ELF {} has more than one .stack_sizes section",
            path.display()
        ));
    }
    if section.relocations().next().is_some() {
        return Err(format!(
            "{label} ELF {} retains relocations in .stack_sizes; inspect only the final linked image",
            path.display()
        ));
    }
    let data = section.data().map_err(|error| {
        format!(
            "could not read {label} .stack_sizes section in {}: {error}",
            path.display()
        )
    })?;
    parse_stack_size_records(data, size_of::<u32>()).map_err(|error| {
        format!(
            "invalid {label} .stack_sizes in {}: {error}",
            path.display()
        )
    })
}

fn parse_stack_size_records(
    data: &[u8],
    address_bytes: usize,
) -> Result<StackSizeInventory, String> {
    if data.is_empty() {
        return Err("section is empty".to_owned());
    }
    if address_bytes != 4 && address_bytes != 8 {
        return Err(format!(
            "unsupported function-address width {address_bytes}"
        ));
    }

    let mut offset = 0;
    let mut record_count = 0_u64;
    let mut maximum_frame_bytes = 0_u64;
    while offset < data.len() {
        if data.len() - offset < address_bytes {
            return Err("truncated function address".to_owned());
        }
        offset += address_bytes;
        let (frame_bytes, consumed) = decode_uleb128(&data[offset..])?;
        offset += consumed;
        record_count += 1;
        maximum_frame_bytes = maximum_frame_bytes.max(frame_bytes);
    }

    Ok(StackSizeInventory {
        record_count,
        maximum_frame_bytes,
    })
}

fn decode_uleb128(bytes: &[u8]) -> Result<(u64, usize), String> {
    let mut value = 0_u64;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let shift = index * 7;
        if shift >= u64::BITS as usize || (index == 9 && byte > 1) {
            return Err("overflowing ULEB128 frame size".to_owned());
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err("truncated ULEB128 frame size".to_owned())
}

fn unique_symbol_address(
    object: &object::File<'_>,
    path: &Path,
    label: &str,
    name: &str,
) -> Result<u64, String> {
    let mut matches = object
        .symbols()
        .filter(|symbol| symbol.name().is_ok_and(|value| value == name));
    let symbol = matches.next().ok_or_else(|| {
        format!(
            "{label} ELF {} has no defined {name} symbol",
            path.display()
        )
    })?;
    if matches.next().is_some() {
        return Err(format!(
            "{label} ELF {} has more than one defined {name} symbol",
            path.display()
        ));
    }
    if symbol.section() != SymbolSection::Absolute {
        return Err(format!(
            "{label} ELF {} {name} must be one absolute linker symbol, got {:?}",
            path.display(),
            symbol.section()
        ));
    }
    Ok(symbol.address())
}

fn calculate_stack_layout(
    label: &str,
    stack_end: u64,
    stack_guard: u64,
    stack_start: u64,
) -> Result<StackLayout, String> {
    if stack_guard < stack_end {
        return Err(format!(
            "{label} stack guard 0x{stack_guard:x} is below stack end 0x{stack_end:x}"
        ));
    }
    let usable_start = stack_guard
        .checked_add(STACK_GUARD_WORD_BYTES)
        .ok_or_else(|| format!("{label} stack guard address overflows"))?;
    if stack_start < usable_start {
        return Err(format!(
            "{label} stack start 0x{stack_start:x} is below the end of its guard word 0x{usable_start:x}"
        ));
    }
    Ok(StackLayout {
        reserved_bytes: stack_start - stack_end,
        usable_bytes: stack_start - usable_start,
        guard_offset_bytes: stack_guard - stack_end,
    })
}

impl ElfInspection {
    fn validate(&self) -> Result<(), String> {
        if self.default_proof_trace_symbol_count != 0 {
            return Err(format!(
                "default E290 must exclude proof-trace evidence, found {} symbols",
                self.default_proof_trace_symbol_count
            ));
        }
        if self.hil_proof_trace_symbol_count != 1
            || self.hil_proof_trace_symbol_size_bytes != PROOF_BYTE_SIZE as u64
        {
            return Err(format!(
                "runtime-measurement HIL must contain one initialized {PROOF_BYTE_SIZE}-byte proof-trace symbol, got count={} size={}",
                self.hil_proof_trace_symbol_count, self.hil_proof_trace_symbol_size_bytes
            ));
        }
        for (label, inventory) in [
            ("default E290", self.default_stack_sizes),
            ("runtime-measurement HIL", self.hil_stack_sizes),
        ] {
            if inventory.record_count == 0 {
                return Err(format!("{label} .stack_sizes contains no records"));
            }
            if inventory.maximum_frame_bytes > MAXIMUM_STACK_FRAME_BYTES {
                return Err(format!(
                    "{label} maximum compiler-emitted frame {} exceeds the reviewed {}-byte ceiling",
                    inventory.maximum_frame_bytes, MAXIMUM_STACK_FRAME_BYTES
                ));
            }
        }
        for (label, stack, minimum_usable) in [
            (
                "default E290",
                self.default_stack,
                MINIMUM_DEFAULT_USABLE_STACK_BYTES,
            ),
            (
                "runtime-measurement HIL",
                self.hil_stack,
                MINIMUM_HIL_USABLE_STACK_BYTES,
            ),
        ] {
            if stack.guard_offset_bytes != EXPECTED_STACK_GUARD_OFFSET_BYTES {
                return Err(format!(
                    "{label} stack guard offset {} differs from the reviewed {} bytes",
                    stack.guard_offset_bytes, EXPECTED_STACK_GUARD_OFFSET_BYTES
                ));
            }
            if stack.usable_bytes < minimum_usable {
                return Err(format!(
                    "{label} usable stack {} is below the reviewed {minimum_usable}-byte floor",
                    stack.usable_bytes
                ));
            }
        }
        Ok(())
    }

    fn render(self) -> String {
        let worst_frame = self
            .default_stack_sizes
            .maximum_frame_bytes
            .max(self.hil_stack_sizes.maximum_frame_bytes);
        let conservative_margin = QUALIFIED_RAW_STACK_MARGIN_BYTES.saturating_sub(worst_frame);
        format!(
            "default.stack_size_records={}\ndefault.maximum_frame_bytes={}\ndefault.stack_reserved_bytes={}\ndefault.stack_usable_bytes={}\ndefault.stack_guard_offset_bytes={}\ndefault.proof_trace_symbol_count={}\nhil.stack_size_records={}\nhil.maximum_frame_bytes={}\nhil.stack_reserved_bytes={}\nhil.stack_usable_bytes={}\nhil.stack_guard_offset_bytes={}\nhil.proof_trace_symbol_count={}\nhil.proof_trace_symbol_size_bytes={}\npolicy.maximum_frame_bytes={}\npolicy.minimum_default_usable_stack_bytes={}\npolicy.minimum_hil_usable_stack_bytes={}\npolicy.expected_stack_guard_offset_bytes={}\nqualification.raw_painted_margin_bytes={}\nqualification.conservative_margin_bytes={}",
            self.default_stack_sizes.record_count,
            self.default_stack_sizes.maximum_frame_bytes,
            self.default_stack.reserved_bytes,
            self.default_stack.usable_bytes,
            self.default_stack.guard_offset_bytes,
            self.default_proof_trace_symbol_count,
            self.hil_stack_sizes.record_count,
            self.hil_stack_sizes.maximum_frame_bytes,
            self.hil_stack.reserved_bytes,
            self.hil_stack.usable_bytes,
            self.hil_stack.guard_offset_bytes,
            self.hil_proof_trace_symbol_count,
            self.hil_proof_trace_symbol_size_bytes,
            MAXIMUM_STACK_FRAME_BYTES,
            MINIMUM_DEFAULT_USABLE_STACK_BYTES,
            MINIMUM_HIL_USABLE_STACK_BYTES,
            EXPECTED_STACK_GUARD_OFFSET_BYTES,
            QUALIFIED_RAW_STACK_MARGIN_BYTES,
            conservative_margin,
        )
    }
}

impl DecodedEvidence {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != BYTE_SIZE {
            return Err(format!(
                "input must be exactly {BYTE_SIZE} bytes, got {}",
                bytes.len()
            ));
        }

        let mut words = [0_u32; WORD_COUNT];
        for (word, bytes) in words.iter_mut().zip(bytes.chunks_exact(size_of::<u32>())) {
            *word = u32::from_le_bytes(
                bytes
                    .try_into()
                    .expect("an exact four-byte chunk must convert to one ABI word"),
            );
        }
        let evidence = Self { words };
        evidence.validate()?;
        Ok(evidence)
    }

    fn validate(&self) -> Result<(), String> {
        let snapshot_seq_begin = self.words[SNAPSHOT_SEQ_BEGIN_WORD];
        let snapshot_seq_end = self.words[SNAPSHOT_SEQ_END_WORD];
        if snapshot_seq_begin != snapshot_seq_end {
            return Err(format!(
                "snapshot sequence markers must match, got begin={snapshot_seq_begin} end={snapshot_seq_end}"
            ));
        }
        if snapshot_seq_begin & 1 != 0 {
            return Err(format!(
                "snapshot sequence markers must be even, got {snapshot_seq_begin}"
            ));
        }

        if self.words[MAGIC_WORD] != MAGIC {
            return Err(format!(
                "magic must be RTME, got 0x{:08x}",
                self.words[MAGIC_WORD]
            ));
        }
        if self.words[VERSION_WORD] != VERSION {
            return Err(format!(
                "version must be {VERSION}, got {}",
                self.words[VERSION_WORD]
            ));
        }
        if self.words[SIZE_WORD] != BYTE_SIZE as u32 {
            return Err(format!(
                "size_bytes must be {BYTE_SIZE}, got {}",
                self.words[SIZE_WORD]
            ));
        }

        let flags = self.words[FLAGS_WORD];
        let unknown_flags = flags & !KNOWN_FLAG_MASK;
        if unknown_flags != 0 {
            return Err(format!(
                "flags.raw contains unknown bits 0x{unknown_flags:08x}"
            ));
        }
        if flags & FLAG_ACTIVE == 0 {
            return Err("flags.active must be true".to_owned());
        }

        self.validate_heap(flags)?;
        self.validate_stack(flags)?;
        if flags & FLAG_COMPOSITION_READY == 0 && self.words[COMPOSITION_READY_US_WORD] != 0 {
            return Err(
                "composition_ready_us must be zero until flags.composition_ready is true"
                    .to_owned(),
            );
        }

        for last_word in (BOOT_FIRST_WORD..=BOOT_LAST_WORD).step_by(2) {
            self.validate_last_maximum(last_word, last_word + 1)?;
        }
        self.validate_counted_operation(INBOUND_COUNT_WORD, None, INBOUND_MAXIMUM_WORD)?;
        self.validate_counted_operation(AUTHORIZED_COUNT_WORD, None, AUTHORIZED_MAXIMUM_WORD)?;
        self.validate_counted_operation(SUBMISSION_COUNT_WORD, None, SUBMISSION_MAXIMUM_WORD)?;
        self.validate_counted_operation(API_COUNT_WORD, None, API_MAXIMUM_WORD)?;
        self.validate_counted_operation(RX_COUNT_WORD, None, RX_MAXIMUM_WORD)?;
        self.validate_counted_operation(CAD_COUNT_WORD, None, CAD_MAXIMUM_WORD)?;
        self.validate_counted_operation(TX_COUNT_WORD, None, TX_MAXIMUM_WORD)?;
        self.validate_timeout(RX_COUNT_WORD, RX_TIMEOUT_WORD)?;
        self.validate_timeout(CAD_COUNT_WORD, CAD_TIMEOUT_WORD)?;
        self.validate_timeout(TX_COUNT_WORD, TX_TIMEOUT_WORD)?;
        if self.words[FAILED_ALLOCATION_COUNT_WORD] > self.words[ALLOCATION_COUNT_WORD] {
            return Err(format!(
                "{} must not exceed {}",
                WORD_NAMES[FAILED_ALLOCATION_COUNT_WORD], WORD_NAMES[ALLOCATION_COUNT_WORD]
            ));
        }
        Ok(())
    }

    fn validate_heap(&self, flags: u32) -> Result<(), String> {
        let registered = flags & FLAG_HEAP_REGISTERED != 0;
        for minimum_word in [
            HEAP_MINIMUM_FREE_WORD,
            INTERNAL_HEAP_MINIMUM_FREE_WORD,
            EXTERNAL_HEAP_MINIMUM_FREE_WORD,
        ] {
            self.validate_minimum_sentinel(minimum_word, registered, "flags.heap_registered")?;
        }

        if !registered {
            for word in [
                HEAP_TOTAL_WORD,
                HEAP_CURRENT_WORD,
                HEAP_MAXIMUM_WORD,
                INTERNAL_HEAP_CURRENT_WORD,
                EXTERNAL_HEAP_CURRENT_WORD,
            ] {
                if self.words[word] != 0 {
                    return Err(format!(
                        "{} must be zero until flags.heap_registered is true",
                        WORD_NAMES[word]
                    ));
                }
            }
            return Ok(());
        }

        let total = self.words[HEAP_TOTAL_WORD];
        let current = self.words[HEAP_CURRENT_WORD];
        let maximum = self.words[HEAP_MAXIMUM_WORD];
        let minimum_free = self.words[HEAP_MINIMUM_FREE_WORD];
        if total == 0 {
            return Err(
                "memory.heap_total_bytes must be nonzero when heap is registered".to_owned(),
            );
        }
        if current > maximum {
            return Err(
                "memory.heap_current_bytes must not exceed memory.heap_maximum_bytes".to_owned(),
            );
        }
        if maximum > total {
            return Err(
                "memory.heap_maximum_bytes must not exceed memory.heap_total_bytes".to_owned(),
            );
        }
        if minimum_free > total.saturating_sub(current) {
            return Err(
                "memory.heap_minimum_free_bytes must not exceed current free heap".to_owned(),
            );
        }
        for word in [
            INTERNAL_HEAP_CURRENT_WORD,
            INTERNAL_HEAP_MINIMUM_FREE_WORD,
            EXTERNAL_HEAP_CURRENT_WORD,
            EXTERNAL_HEAP_MINIMUM_FREE_WORD,
        ] {
            if self.words[word] > total {
                return Err(format!(
                    "{} must not exceed memory.heap_total_bytes",
                    WORD_NAMES[word]
                ));
            }
        }

        let psram = self.words[PSRAM_BYTES_WORD];
        for word in [EXTERNAL_HEAP_CURRENT_WORD, EXTERNAL_HEAP_MINIMUM_FREE_WORD] {
            if self.words[word] > psram {
                return Err(format!(
                    "{} must not exceed memory.psram_bytes",
                    WORD_NAMES[word]
                ));
            }
        }
        Ok(())
    }

    fn validate_stack(&self, flags: u32) -> Result<(), String> {
        let initialized = flags & FLAG_STACK_INITIALIZED != 0;
        self.validate_minimum_sentinel(
            STACK_MINIMUM_REMAINING_WORD,
            initialized,
            "flags.stack_initialized",
        )?;
        if !initialized {
            if flags & (FLAG_SCAN_VALID | FLAG_GUARD_INTACT) != 0 {
                return Err(
                    "stack validity flags require flags.stack_initialized to be true".to_owned(),
                );
            }
            for word in [
                STACK_RESERVED_WORD,
                STACK_USABLE_WORD,
                STACK_PAINTED_WORD,
                STACK_HIGH_WATER_WORD,
                STACK_GUARD_OFFSET_WORD,
            ] {
                if self.words[word] != 0 {
                    return Err(format!(
                        "{} must be zero until flags.stack_initialized is true",
                        WORD_NAMES[word]
                    ));
                }
            }
            return Ok(());
        }

        let reserved = self.words[STACK_RESERVED_WORD];
        let usable = self.words[STACK_USABLE_WORD];
        let painted = self.words[STACK_PAINTED_WORD];
        let high_water = self.words[STACK_HIGH_WATER_WORD];
        let minimum_remaining = self.words[STACK_MINIMUM_REMAINING_WORD];
        let guard_offset = self.words[STACK_GUARD_OFFSET_WORD];
        for (word, name) in WORD_NAMES
            .iter()
            .enumerate()
            .take(STACK_GUARD_OFFSET_WORD + 1)
            .skip(STACK_RESERVED_WORD)
        {
            if self.words[word] & 3 != 0 {
                return Err(format!("{name} must be four-byte aligned"));
            }
        }
        if reserved == 0 {
            return Err(
                "stack.reserved_bytes must be nonzero when stack is initialized".to_owned(),
            );
        }
        let guard_end = guard_offset
            .checked_add(size_of::<u32>() as u32)
            .ok_or_else(|| "stack.guard_offset_bytes overflows the reservation".to_owned())?;
        if guard_end > reserved {
            return Err("stack guard must fit inside stack.reserved_bytes".to_owned());
        }
        if usable != reserved - guard_end {
            return Err("stack.usable_bytes must equal reserved bytes above the guard".to_owned());
        }
        if painted > reserved.saturating_sub(size_of::<u32>() as u32) {
            return Err("stack.painted_bytes exceeds the non-guard reservation".to_owned());
        }
        if high_water > reserved {
            return Err("stack.high_water_bytes exceeds stack.reserved_bytes".to_owned());
        }
        if minimum_remaining != usable.saturating_sub(high_water) {
            return Err(
                "stack.minimum_remaining_bytes does not match the high-water bound".to_owned(),
            );
        }
        Ok(())
    }

    fn validate_minimum_sentinel(
        &self,
        word: usize,
        registered: bool,
        registration_flag: &str,
    ) -> Result<(), String> {
        let unobserved = self.words[word] == UNOBSERVED_MINIMUM;
        if registered == unobserved {
            let state = if registered { "observed" } else { "unobserved" };
            return Err(format!(
                "{} must be {state} when {registration_flag} is {}",
                WORD_NAMES[word], registered
            ));
        }
        Ok(())
    }

    fn validate_last_maximum(&self, last_word: usize, maximum_word: usize) -> Result<(), String> {
        if self.words[last_word] > self.words[maximum_word] {
            return Err(format!(
                "{} must not exceed {}",
                WORD_NAMES[last_word], WORD_NAMES[maximum_word]
            ));
        }
        Ok(())
    }

    fn validate_counted_operation(
        &self,
        count_word: usize,
        last_word: Option<usize>,
        maximum_word: usize,
    ) -> Result<(), String> {
        if let Some(last_word) = last_word {
            self.validate_last_maximum(last_word, maximum_word)?;
        }
        if self.words[count_word] == 0
            && (self.words[maximum_word] != 0
                || last_word.is_some_and(|word| self.words[word] != 0))
        {
            return Err(format!(
                "{} timing requires a nonzero {}",
                WORD_NAMES[maximum_word], WORD_NAMES[count_word]
            ));
        }
        Ok(())
    }

    fn validate_timeout(&self, count_word: usize, timeout_word: usize) -> Result<(), String> {
        if self.words[timeout_word] > self.words[count_word] {
            return Err(format!(
                "{} must not exceed {}",
                WORD_NAMES[timeout_word], WORD_NAMES[count_word]
            ));
        }
        Ok(())
    }

    fn render_human(&self) -> String {
        let mut output = String::new();
        self.for_each_output(|name, value| {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(name);
            output.push('=');
            match value {
                OutputValue::Number(value) => {
                    let _ = write!(output, "{value}");
                }
                OutputValue::Bool(value) => output.push_str(if value { "true" } else { "false" }),
                OutputValue::Text(value) => output.push_str(value),
                OutputValue::Unobserved => output.push_str("unobserved"),
            }
        });
        output
    }

    fn render_json(&self) -> String {
        let mut output = String::from("{");
        let mut first = true;
        self.for_each_output(|name, value| {
            if !first {
                output.push(',');
            }
            first = false;
            output.push('"');
            output.push_str(name);
            output.push_str("\":");
            match value {
                OutputValue::Number(value) => {
                    let _ = write!(output, "{value}");
                }
                OutputValue::Bool(value) => output.push_str(if value { "true" } else { "false" }),
                OutputValue::Text(value) => {
                    output.push('"');
                    output.push_str(value);
                    output.push('"');
                }
                OutputValue::Unobserved => output.push_str("null"),
            }
        });
        output.push('}');
        output
    }

    fn for_each_output(&self, mut emit: impl FnMut(&'static str, OutputValue)) {
        for (word, name) in WORD_NAMES.iter().copied().enumerate() {
            let value = if word == MAGIC_WORD {
                OutputValue::Text("RTME")
            } else if is_minimum_word(word) && self.words[word] == UNOBSERVED_MINIMUM {
                OutputValue::Unobserved
            } else {
                OutputValue::Number(self.words[word])
            };
            emit(name, value);
            if word == FLAGS_WORD {
                for (flag_name, flag) in FLAG_NAMES {
                    emit(
                        flag_name,
                        OutputValue::Bool(self.words[FLAGS_WORD] & flag != 0),
                    );
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProofOutputValue {
    Number(u32),
    WideNumber(u64),
    Bool(bool),
    Text(&'static str),
    Unobserved,
}

impl DecodedProofTraceEvidence {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != PROOF_BYTE_SIZE {
            return Err(format!(
                "proof-trace input must be exactly {PROOF_BYTE_SIZE} bytes, got {}",
                bytes.len()
            ));
        }

        let mut words = [0_u32; PROOF_WORD_COUNT];
        for (word, bytes) in words.iter_mut().zip(bytes.chunks_exact(size_of::<u32>())) {
            *word = u32::from_le_bytes(
                bytes
                    .try_into()
                    .expect("an exact four-byte chunk must convert to one ABI word"),
            );
        }
        let evidence = Self { words };
        evidence.validate()?;
        Ok(evidence)
    }

    fn validate(&self) -> Result<(), String> {
        let begin = self.words[PROOF_SNAPSHOT_SEQ_BEGIN_WORD];
        let end = self.words[PROOF_SNAPSHOT_SEQ_END_WORD];
        if begin != end {
            return Err(format!(
                "proof-trace snapshot sequence markers must match, got begin={begin} end={end}"
            ));
        }
        if begin & 1 != 0 {
            return Err(format!(
                "proof-trace snapshot sequence markers must be even, got {begin}"
            ));
        }
        if self.words[PROOF_MAGIC_WORD] != PROOF_MAGIC {
            return Err(format!(
                "proof-trace magic must be RPTE, got 0x{:08x}",
                self.words[PROOF_MAGIC_WORD]
            ));
        }
        if self.words[PROOF_VERSION_WORD] != PROOF_VERSION {
            return Err(format!(
                "proof-trace version must be {PROOF_VERSION}, got {}",
                self.words[PROOF_VERSION_WORD]
            ));
        }
        if self.words[PROOF_SIZE_WORD] != PROOF_BYTE_SIZE as u32 {
            return Err(format!(
                "proof-trace size_bytes must be {PROOF_BYTE_SIZE}, got {}",
                self.words[PROOF_SIZE_WORD]
            ));
        }

        let flags = self.words[PROOF_FLAGS_WORD];
        let unknown_flags = flags & !PROOF_KNOWN_FLAG_MASK;
        if unknown_flags != 0 {
            return Err(format!(
                "proof-trace flags.raw contains unknown bits 0x{unknown_flags:08x}"
            ));
        }
        if flags & PROOF_FLAG_ACTIVE == 0 {
            return Err("proof-trace flags.active must be true".to_owned());
        }

        for (count, last) in [
            (PROOF_LOGICAL_RX_COUNT_WORD, PROOF_LOGICAL_RX_LAST_MS_WORD),
            (
                PROOF_INGRESS_ENQUEUE_COUNT_WORD,
                PROOF_INGRESS_ENQUEUE_LAST_MS_WORD,
            ),
            (
                PROOF_INGRESS_DEFER_COUNT_WORD,
                PROOF_INGRESS_DEFER_LAST_MS_WORD,
            ),
            (
                PROOF_INGRESS_FAIL_COUNT_WORD,
                PROOF_INGRESS_FAIL_LAST_MS_WORD,
            ),
            (PROOF_RNS_INGRESS_COUNT_WORD, PROOF_RNS_INGRESS_LAST_MS_WORD),
            (PROOF_GENERATED_COUNT_WORD, PROOF_GENERATED_LAST_MS_WORD),
            (PROOF_DELIVERED_COUNT_WORD, PROOF_DELIVERED_LAST_MS_WORD),
            (PROOF_TIMEOUT_COUNT_WORD, PROOF_TIMEOUT_LAST_MS_WORD),
            (
                PROOF_ACTION_PRESSURE_COUNT_WORD,
                PROOF_ACTION_PRESSURE_LAST_MS_WORD,
            ),
            (
                PROOF_CORRELATION_FAULT_COUNT_WORD,
                PROOF_CORRELATION_FAULT_LAST_MS_WORD,
            ),
        ] {
            self.validate_count_timestamp(count, last)?;
        }
        self.validate_ingress()?;
        self.validate_commit(flags)?;
        self.validate_tag(
            flags,
            PROOF_GENERATED_COUNT_WORD,
            PROOF_GENERATED_TAG_LOW_WORD,
            PROOF_GENERATED_TAG_HIGH_WORD,
            PROOF_FLAG_GENERATED_TAG_PRESENT,
            PROOF_FLAG_GENERATED_TAGS_CONSISTENT,
            "generated",
        )?;
        self.validate_tag(
            flags,
            PROOF_DELIVERED_COUNT_WORD,
            PROOF_DELIVERED_TAG_LOW_WORD,
            PROOF_DELIVERED_TAG_HIGH_WORD,
            PROOF_FLAG_DELIVERED_TAG_PRESENT,
            PROOF_FLAG_DELIVERED_TAGS_CONSISTENT,
            "delivered",
        )?;
        self.validate_tag(
            flags,
            PROOF_TIMEOUT_COUNT_WORD,
            PROOF_TIMEOUT_TAG_LOW_WORD,
            PROOF_TIMEOUT_TAG_HIGH_WORD,
            PROOF_FLAG_TIMEOUT_TAG_PRESENT,
            PROOF_FLAG_TIMEOUT_TAGS_CONSISTENT,
            "timeout",
        )?;

        Ok(())
    }

    fn validate_empty_initializer(&self) -> Result<(), String> {
        for (word, name) in PROOF_WORD_NAMES.iter().copied().enumerate() {
            let expected = match word {
                PROOF_MAGIC_WORD => PROOF_MAGIC,
                PROOF_VERSION_WORD => PROOF_VERSION,
                PROOF_SIZE_WORD => PROOF_BYTE_SIZE as u32,
                PROOF_FLAGS_WORD => PROOF_FLAG_ACTIVE | PROOF_FLAG_INBOX_COMMIT_ORDER_CONSISTENT,
                _ => 0,
            };
            if self.words[word] != expected {
                return Err(format!(
                    "{name} must initialize to {expected}, got {}",
                    self.words[word]
                ));
            }
        }
        Ok(())
    }

    fn validate_count_timestamp(&self, count: usize, last: usize) -> Result<(), String> {
        if self.words[count] == 0 && self.words[last] != 0 {
            return Err(format!(
                "{} requires a nonzero {}",
                PROOF_WORD_NAMES[last], PROOF_WORD_NAMES[count]
            ));
        }
        Ok(())
    }

    fn validate_ingress(&self) -> Result<(), String> {
        let ingress_count = self.words[PROOF_RNS_INGRESS_COUNT_WORD];
        let dispositions = [
            PROOF_DISPOSITION_PROCESSED_WORD,
            PROOF_DISPOSITION_DUPLICATE_WORD,
            PROOF_DISPOSITION_INVALID_WORD,
            PROOF_DISPOSITION_NO_OUTCOME_WORD,
            PROOF_DISPOSITION_REJECTED_WORD,
        ];
        let disposition_sum = dispositions
            .into_iter()
            .map(|word| u64::from(self.words[word]))
            .sum::<u64>();
        let ingress_aggregate_saturated = ingress_count == u32::MAX
            || dispositions
                .into_iter()
                .any(|word| self.words[word] == u32::MAX);
        if !ingress_aggregate_saturated && disposition_sum != u64::from(ingress_count) {
            return Err(format!(
                "proof-trace disposition counts sum to {disposition_sum}, expected rns_ingress.count={ingress_count}"
            ));
        }

        let last_disposition = self.words[PROOF_LAST_DISPOSITION_WORD];
        let last_packet_type = self.words[PROOF_LAST_PACKET_TYPE_WORD];
        if ingress_count == 0 {
            for word in [
                PROOF_LAST_DISPOSITION_WORD,
                PROOF_LAST_PACKET_TYPE_WORD,
                PROOF_LAST_EMITTED_PACKETS_WORD,
                PROOF_LAST_GENERATED_ACTIONS_WORD,
                PROOF_LAST_DELIVERED_TERMINALS_WORD,
                PROOF_LAST_TIMED_OUT_TERMINALS_WORD,
            ] {
                if self.words[word] != 0 {
                    return Err(format!(
                        "{} must be zero until rns_ingress.count is nonzero",
                        PROOF_WORD_NAMES[word]
                    ));
                }
            }
        } else if !(1..=5).contains(&last_disposition) {
            return Err(format!(
                "rns_ingress.last_disposition must be in 1..=5, got {last_disposition}"
            ));
        }
        if last_packet_type > 4 {
            return Err(format!(
                "rns_ingress.last_wire_packet_type must be in 0..=4, got {last_packet_type}"
            ));
        }
        for (last_word, total_word) in [
            (
                PROOF_LAST_GENERATED_ACTIONS_WORD,
                PROOF_GENERATED_COUNT_WORD,
            ),
            (
                PROOF_LAST_DELIVERED_TERMINALS_WORD,
                PROOF_DELIVERED_COUNT_WORD,
            ),
        ] {
            if self.words[total_word] != u32::MAX && self.words[last_word] > self.words[total_word]
            {
                return Err(format!(
                    "{} must not exceed {}",
                    PROOF_WORD_NAMES[last_word], PROOF_WORD_NAMES[total_word]
                ));
            }
        }
        Ok(())
    }

    fn validate_commit(&self, flags: u32) -> Result<(), String> {
        let count = self.words[PROOF_INBOX_COMMIT_COUNT_WORD];
        if count == 0 {
            for word in [
                PROOF_INBOX_COMMIT_START_MS_WORD,
                PROOF_INBOX_COMMIT_END_MS_WORD,
            ] {
                if self.words[word] != 0 {
                    return Err(format!(
                        "{} requires a nonzero inbox.commit.count",
                        PROOF_WORD_NAMES[word]
                    ));
                }
            }
            if flags & PROOF_FLAG_INBOX_COMMIT_IN_PROGRESS != 0 {
                return Err(
                    "flags.inbox_commit_in_progress requires a nonzero inbox.commit.count"
                        .to_owned(),
                );
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_tag(
        &self,
        flags: u32,
        count_word: usize,
        low_word: usize,
        high_word: usize,
        present_flag: u32,
        consistency_flag: u32,
        label: &str,
    ) -> Result<(), String> {
        let count = self.words[count_word];
        let present = flags & present_flag != 0;
        let consistent = flags & consistency_flag != 0;
        if !present && (self.words[low_word] != 0 || self.words[high_word] != 0) {
            return Err(format!(
                "proof-trace {label} tag words must be zero when its presence flag is false"
            ));
        }
        if count == 0 && (present || consistent) {
            return Err(format!(
                "proof-trace {label} tag flags require a nonzero {}",
                PROOF_WORD_NAMES[count_word]
            ));
        }
        Ok(())
    }

    fn render_human(&self) -> String {
        let mut output = String::new();
        self.for_each_output(|name, value| {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(name);
            output.push('=');
            match value {
                ProofOutputValue::Number(value) => {
                    let _ = write!(output, "{value}");
                }
                ProofOutputValue::WideNumber(value) => {
                    let _ = write!(output, "0x{value:016x}");
                }
                ProofOutputValue::Bool(value) => {
                    output.push_str(if value { "true" } else { "false" });
                }
                ProofOutputValue::Text(value) => output.push_str(value),
                ProofOutputValue::Unobserved => output.push_str("unobserved"),
            }
        });
        output
    }

    fn render_json(&self) -> String {
        let mut output = String::from("{");
        let mut first = true;
        self.for_each_output(|name, value| {
            if !first {
                output.push(',');
            }
            first = false;
            output.push('"');
            output.push_str(name);
            output.push_str("\":");
            match value {
                ProofOutputValue::Number(value) => {
                    let _ = write!(output, "{value}");
                }
                ProofOutputValue::WideNumber(value) => {
                    let _ = write!(output, "\"0x{value:016x}\"");
                }
                ProofOutputValue::Bool(value) => {
                    output.push_str(if value { "true" } else { "false" });
                }
                ProofOutputValue::Text(value) => {
                    output.push('"');
                    output.push_str(value);
                    output.push('"');
                }
                ProofOutputValue::Unobserved => output.push_str("null"),
            }
        });
        output.push('}');
        output
    }

    fn for_each_output(&self, mut emit: impl FnMut(&'static str, ProofOutputValue)) {
        for (word, name) in PROOF_WORD_NAMES.iter().copied().enumerate() {
            let value = match word {
                PROOF_MAGIC_WORD => ProofOutputValue::Text("RPTE"),
                PROOF_LAST_DISPOSITION_WORD => self.last_disposition_value(),
                PROOF_LAST_PACKET_TYPE_WORD => self.last_packet_type_value(),
                _ => ProofOutputValue::Number(self.words[word]),
            };
            emit(name, value);
            if word == PROOF_FLAGS_WORD {
                for (flag_name, flag) in PROOF_FLAG_NAMES {
                    emit(
                        flag_name,
                        ProofOutputValue::Bool(self.words[PROOF_FLAGS_WORD] & flag != 0),
                    );
                }
            }
        }
        for (name, low, high, present) in [
            (
                "tag.generated",
                PROOF_GENERATED_TAG_LOW_WORD,
                PROOF_GENERATED_TAG_HIGH_WORD,
                PROOF_FLAG_GENERATED_TAG_PRESENT,
            ),
            (
                "tag.delivered",
                PROOF_DELIVERED_TAG_LOW_WORD,
                PROOF_DELIVERED_TAG_HIGH_WORD,
                PROOF_FLAG_DELIVERED_TAG_PRESENT,
            ),
            (
                "tag.timeout",
                PROOF_TIMEOUT_TAG_LOW_WORD,
                PROOF_TIMEOUT_TAG_HIGH_WORD,
                PROOF_FLAG_TIMEOUT_TAG_PRESENT,
            ),
        ] {
            let value = if self.words[PROOF_FLAGS_WORD] & present == 0 {
                ProofOutputValue::Unobserved
            } else {
                ProofOutputValue::WideNumber(
                    u64::from(self.words[low]) | (u64::from(self.words[high]) << 32),
                )
            };
            emit(name, value);
        }
    }

    fn last_disposition_value(&self) -> ProofOutputValue {
        match self.words[PROOF_LAST_DISPOSITION_WORD] {
            0 => ProofOutputValue::Unobserved,
            1 => ProofOutputValue::Text("processed"),
            2 => ProofOutputValue::Text("native_duplicate"),
            3 => ProofOutputValue::Text("native_invalid"),
            4 => ProofOutputValue::Text("no_observable_outcome"),
            5 => ProofOutputValue::Text("rejected"),
            _ => unreachable!("validation rejects unknown dispositions"),
        }
    }

    fn last_packet_type_value(&self) -> ProofOutputValue {
        match self.words[PROOF_LAST_PACKET_TYPE_WORD] {
            0 if self.words[PROOF_RNS_INGRESS_COUNT_WORD] == 0 => ProofOutputValue::Unobserved,
            0 => ProofOutputValue::Text("unparsed"),
            1 => ProofOutputValue::Text("data"),
            2 => ProofOutputValue::Text("announce"),
            3 => ProofOutputValue::Text("link_request"),
            4 => ProofOutputValue::Text("proof"),
            _ => unreachable!("validation rejects unknown packet types"),
        }
    }
}

const fn is_minimum_word(word: usize) -> bool {
    matches!(
        word,
        HEAP_MINIMUM_FREE_WORD
            | INTERNAL_HEAP_MINIMUM_FREE_WORD
            | EXTERNAL_HEAP_MINIMUM_FREE_WORD
            | STACK_MINIMUM_REMAINING_WORD
    )
}

const _: () = {
    assert!(PROOF_TRACE_LINKED_STACK_REDUCTION_BYTES == 192);
    assert!(QUALIFIED_RAW_STACK_MARGIN_BYTES == 72_020);
    assert!(MAXIMUM_STACK_FRAME_BYTES == 52_752);
    assert!(MINIMUM_CONSERVATIVE_STACK_MARGIN_BYTES == 19_268);
    assert!(
        MAXIMUM_STACK_FRAME_BYTES + MINIMUM_CONSERVATIVE_STACK_MARGIN_BYTES
            == QUALIFIED_RAW_STACK_MARGIN_BYTES
    );
    assert!(BYTE_SIZE == 256);
    assert!(WORD_NAMES.len() == WORD_COUNT);
    assert!(SNAPSHOT_SEQ_BEGIN_WORD == 0);
    assert!(MAGIC_WORD == SNAPSHOT_SEQ_BEGIN_WORD + 1);
    assert!(VERSION_WORD == MAGIC_WORD + 1);
    assert!(SIZE_WORD == VERSION_WORD + 1);
    assert!(FLAGS_WORD == SIZE_WORD + 1);
    assert!(INIT_ERROR_WORD == FLAGS_WORD + 1);
    assert!(UPTIME_WORD == INIT_ERROR_WORD + 1);
    assert!(NODE_LOOP_GAP_MAXIMUM_WORD == TX_TIMEOUT_WORD + 1);
    assert!(RADIO_LOOP_GAP_MAXIMUM_WORD == NODE_LOOP_GAP_MAXIMUM_WORD + 1);
    assert!(MEASUREMENT_LATENESS_MAXIMUM_WORD == RADIO_LOOP_GAP_MAXIMUM_WORD + 1);
    assert!(MEASUREMENT_WORK_MAXIMUM_WORD == MEASUREMENT_LATENESS_MAXIMUM_WORD + 1);
    assert!(UNEXPECTED_ERROR_COUNT_WORD == MEASUREMENT_WORK_MAXIMUM_WORD + 1);
    assert!(ALLOCATION_COUNT_WORD == UNEXPECTED_ERROR_COUNT_WORD + 1);
    assert!(FAILED_ALLOCATION_COUNT_WORD + 1 == SNAPSHOT_SEQ_END_WORD);
    assert!(SNAPSHOT_SEQ_END_WORD + 1 == WORD_COUNT);
};

const _: () = {
    assert!(PROOF_BYTE_SIZE == 192);
    assert!(PROOF_WORD_NAMES.len() == PROOF_WORD_COUNT);
    assert!(PROOF_SNAPSHOT_SEQ_BEGIN_WORD == 0);
    assert!(PROOF_MAGIC_WORD == PROOF_SNAPSHOT_SEQ_BEGIN_WORD + 1);
    assert!(PROOF_VERSION_WORD == PROOF_MAGIC_WORD + 1);
    assert!(PROOF_SIZE_WORD == PROOF_VERSION_WORD + 1);
    assert!(PROOF_FLAGS_WORD == PROOF_SIZE_WORD + 1);
    assert!(PROOF_LOGICAL_RX_COUNT_WORD == PROOF_FLAGS_WORD + 1);
    assert!(PROOF_LOGICAL_RX_LAST_MS_WORD == PROOF_LOGICAL_RX_COUNT_WORD + 1);
    assert!(PROOF_INGRESS_ENQUEUE_COUNT_WORD == PROOF_LOGICAL_RX_LAST_MS_WORD + 1);
    assert!(PROOF_INGRESS_ENQUEUE_LAST_MS_WORD == PROOF_INGRESS_ENQUEUE_COUNT_WORD + 1);
    assert!(PROOF_INGRESS_DEFER_COUNT_WORD == PROOF_INGRESS_ENQUEUE_LAST_MS_WORD + 1);
    assert!(PROOF_INGRESS_DEFER_LAST_MS_WORD == PROOF_INGRESS_DEFER_COUNT_WORD + 1);
    assert!(PROOF_INGRESS_FAIL_COUNT_WORD == PROOF_INGRESS_DEFER_LAST_MS_WORD + 1);
    assert!(PROOF_INGRESS_FAIL_LAST_MS_WORD == PROOF_INGRESS_FAIL_COUNT_WORD + 1);
    assert!(PROOF_RNS_INGRESS_COUNT_WORD == PROOF_INGRESS_FAIL_LAST_MS_WORD + 1);
    assert!(PROOF_RNS_INGRESS_LAST_MS_WORD == PROOF_RNS_INGRESS_COUNT_WORD + 1);
    assert!(PROOF_GENERATED_COUNT_WORD == PROOF_RNS_INGRESS_LAST_MS_WORD + 1);
    assert!(PROOF_GENERATED_LAST_MS_WORD == PROOF_GENERATED_COUNT_WORD + 1);
    assert!(PROOF_DELIVERED_COUNT_WORD == PROOF_GENERATED_LAST_MS_WORD + 1);
    assert!(PROOF_DELIVERED_LAST_MS_WORD == PROOF_DELIVERED_COUNT_WORD + 1);
    assert!(PROOF_TIMEOUT_COUNT_WORD == PROOF_DELIVERED_LAST_MS_WORD + 1);
    assert!(PROOF_TIMEOUT_LAST_MS_WORD == PROOF_TIMEOUT_COUNT_WORD + 1);
    assert!(PROOF_ACTION_PRESSURE_COUNT_WORD == PROOF_TIMEOUT_LAST_MS_WORD + 1);
    assert!(PROOF_ACTION_PRESSURE_LAST_MS_WORD == PROOF_ACTION_PRESSURE_COUNT_WORD + 1);
    assert!(PROOF_CORRELATION_FAULT_COUNT_WORD == PROOF_ACTION_PRESSURE_LAST_MS_WORD + 1);
    assert!(PROOF_CORRELATION_FAULT_LAST_MS_WORD == PROOF_CORRELATION_FAULT_COUNT_WORD + 1);
    assert!(PROOF_INBOX_COMMIT_COUNT_WORD == PROOF_CORRELATION_FAULT_LAST_MS_WORD + 1);
    assert!(PROOF_INBOX_COMMIT_START_MS_WORD == PROOF_INBOX_COMMIT_COUNT_WORD + 1);
    assert!(PROOF_INBOX_COMMIT_END_MS_WORD == PROOF_INBOX_COMMIT_START_MS_WORD + 1);
    assert!(PROOF_DISPOSITION_PROCESSED_WORD == PROOF_INBOX_COMMIT_END_MS_WORD + 1);
    assert!(PROOF_DISPOSITION_REJECTED_WORD == PROOF_DISPOSITION_PROCESSED_WORD + 4);
    assert!(PROOF_LAST_DISPOSITION_WORD == PROOF_DISPOSITION_REJECTED_WORD + 1);
    assert!(PROOF_LAST_PACKET_TYPE_WORD == PROOF_LAST_DISPOSITION_WORD + 1);
    assert!(PROOF_LAST_EMITTED_PACKETS_WORD == PROOF_LAST_PACKET_TYPE_WORD + 1);
    assert!(PROOF_LAST_GENERATED_ACTIONS_WORD == PROOF_LAST_EMITTED_PACKETS_WORD + 1);
    assert!(PROOF_LAST_DELIVERED_TERMINALS_WORD == PROOF_LAST_GENERATED_ACTIONS_WORD + 1);
    assert!(PROOF_LAST_TIMED_OUT_TERMINALS_WORD == PROOF_LAST_DELIVERED_TERMINALS_WORD + 1);
    assert!(PROOF_GENERATED_TAG_LOW_WORD == PROOF_LAST_TIMED_OUT_TERMINALS_WORD + 1);
    assert!(PROOF_GENERATED_TAG_HIGH_WORD == PROOF_GENERATED_TAG_LOW_WORD + 1);
    assert!(PROOF_DELIVERED_TAG_LOW_WORD == PROOF_GENERATED_TAG_HIGH_WORD + 1);
    assert!(PROOF_DELIVERED_TAG_HIGH_WORD == PROOF_DELIVERED_TAG_LOW_WORD + 1);
    assert!(PROOF_TIMEOUT_TAG_LOW_WORD == PROOF_DELIVERED_TAG_HIGH_WORD + 1);
    assert!(PROOF_TIMEOUT_TAG_HIGH_WORD == PROOF_TIMEOUT_TAG_LOW_WORD + 1);
    assert!(PROOF_RADIO_TX_CONFIRMED_SUCCESS_COUNT_WORD == PROOF_TIMEOUT_TAG_HIGH_WORD + 1);
    assert!(
        PROOF_RADIO_TX_NOT_CONFIRMED_SUCCESS_COUNT_WORD
            == PROOF_RADIO_TX_CONFIRMED_SUCCESS_COUNT_WORD + 1
    );
    assert!(PROOF_SNAPSHOT_SEQ_END_WORD == PROOF_RADIO_TX_NOT_CONFIRMED_SUCCESS_COUNT_WORD + 1);
    assert!(PROOF_SNAPSHOT_SEQ_END_WORD + 1 == PROOF_WORD_COUNT);
};

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_heltec_vision_master_e290_node::runtime_measurement::{
        BootPhase, HeapSnapshot, OperationKind, RuntimeMeasurementEvidence,
        RuntimeProofTraceEvidence, RuntimeProofTraceIngressDisposition,
        RuntimeProofTraceIngressMetadata, RuntimeProofTracePacketType, StackSnapshot,
    };
    use std::{
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
    };

    #[cfg(not(target_endian = "little"))]
    compile_error!("the E290 runtime-measurement producer contract requires a little-endian host");

    static NEXT_TEMP_INPUT: AtomicU64 = AtomicU64::new(0);

    struct TempInput {
        path: PathBuf,
    }

    impl TempInput {
        fn new(bytes: &[u8]) -> Self {
            let sequence = NEXT_TEMP_INPUT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "reticulum-e290-rtme-{}-{sequence}.bin",
                std::process::id()
            ));
            let _ = fs::remove_file(&path);
            fs::write(&path, bytes).expect("temporary evidence input must be writable");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempInput {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn append_stack_size_record(bytes: &mut Vec<u8>, address: u32, mut frame_bytes: u64) {
        bytes.extend_from_slice(&address.to_le_bytes());
        loop {
            let mut byte = (frame_bytes & 0x7f) as u8;
            frame_bytes >>= 7;
            if frame_bytes != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if frame_bytes == 0 {
                return;
            }
        }
    }

    fn minimal_words() -> [u32; WORD_COUNT] {
        let mut words = [0_u32; WORD_COUNT];
        words[MAGIC_WORD] = MAGIC;
        words[VERSION_WORD] = VERSION;
        words[SIZE_WORD] = BYTE_SIZE as u32;
        words[FLAGS_WORD] = FLAG_ACTIVE;
        words[SNAPSHOT_SEQ_BEGIN_WORD] = 0;
        words[HEAP_MINIMUM_FREE_WORD] = UNOBSERVED_MINIMUM;
        words[INTERNAL_HEAP_MINIMUM_FREE_WORD] = UNOBSERVED_MINIMUM;
        words[EXTERNAL_HEAP_MINIMUM_FREE_WORD] = UNOBSERVED_MINIMUM;
        words[STACK_MINIMUM_REMAINING_WORD] = UNOBSERVED_MINIMUM;
        words[SNAPSHOT_SEQ_END_WORD] = 0;
        words
    }

    fn populated_words() -> [u32; WORD_COUNT] {
        let mut words = minimal_words();
        words[FLAGS_WORD] = FLAG_ACTIVE
            | FLAG_STACK_INITIALIZED
            | FLAG_HEAP_REGISTERED
            | FLAG_COMPOSITION_READY
            | FLAG_SCAN_VALID
            | FLAG_GUARD_INTACT;
        words[SNAPSHOT_SEQ_BEGIN_WORD] = 200;
        words[UPTIME_WORD] = 120_000;
        words[PSRAM_BYTES_WORD] = 8_388_608;
        words[HEAP_TOTAL_WORD] = 1_114_112;
        words[HEAP_CURRENT_WORD] = 1_000;
        words[HEAP_MAXIMUM_WORD] = 2_000;
        words[HEAP_MINIMUM_FREE_WORD] = 1_112_112;
        words[INTERNAL_HEAP_CURRENT_WORD] = 400;
        words[INTERNAL_HEAP_MINIMUM_FREE_WORD] = 60_000;
        words[EXTERNAL_HEAP_CURRENT_WORD] = 600;
        words[EXTERNAL_HEAP_MINIMUM_FREE_WORD] = 1_040_000;
        words[STACK_RESERVED_WORD] = 327_680;
        words[STACK_USABLE_WORD] = 327_676;
        words[STACK_PAINTED_WORD] = 320_000;
        words[STACK_HIGH_WATER_WORD] = 12_000;
        words[STACK_MINIMUM_REMAINING_WORD] = 315_676;
        words[STACK_GUARD_OFFSET_WORD] = 0;
        words[COMPOSITION_READY_US_WORD] = 800_000;
        for (phase, last_word) in (BOOT_FIRST_WORD..=BOOT_LAST_WORD).step_by(2).enumerate() {
            words[last_word] = 10 + phase as u32;
            words[last_word + 1] = 20 + phase as u32;
        }
        words[INBOUND_COUNT_WORD] = 3;
        words[INBOUND_MAXIMUM_WORD] = 500;
        words[AUTHORIZED_COUNT_WORD] = 2;
        words[AUTHORIZED_MAXIMUM_WORD] = 30;
        words[SUBMISSION_COUNT_WORD] = 4;
        words[SUBMISSION_MAXIMUM_WORD] = 100;
        words[API_COUNT_WORD] = 5;
        words[API_MAXIMUM_WORD] = 50;
        words[RX_COUNT_WORD] = 10;
        words[RX_MAXIMUM_WORD] = 1_500_000;
        words[RX_TIMEOUT_WORD] = 1;
        words[CAD_COUNT_WORD] = 20;
        words[CAD_MAXIMUM_WORD] = 100_000;
        words[CAD_TIMEOUT_WORD] = 2;
        words[TX_COUNT_WORD] = 4;
        words[TX_MAXIMUM_WORD] = 900_000;
        words[TX_TIMEOUT_WORD] = 0;
        words[NODE_LOOP_GAP_MAXIMUM_WORD] = 2_000;
        words[RADIO_LOOP_GAP_MAXIMUM_WORD] = 3_000;
        words[MEASUREMENT_LATENESS_MAXIMUM_WORD] = 500;
        words[MEASUREMENT_WORK_MAXIMUM_WORD] = 200;
        words[UNEXPECTED_ERROR_COUNT_WORD] = 1;
        words[ALLOCATION_COUNT_WORD] = 100;
        words[FAILED_ALLOCATION_COUNT_WORD] = 2;
        words[SNAPSHOT_SEQ_END_WORD] = 200;
        words
    }

    fn encode(words: [u32; WORD_COUNT]) -> [u8; BYTE_SIZE] {
        let mut bytes = [0_u8; BYTE_SIZE];
        for (word, destination) in words
            .into_iter()
            .zip(bytes.chunks_exact_mut(size_of::<u32>()))
        {
            destination.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    fn parse_words(words: [u32; WORD_COUNT]) -> Result<DecodedEvidence, String> {
        DecodedEvidence::parse(&encode(words))
    }

    fn producer_bytes(evidence: &RuntimeMeasurementEvidence) -> [u8; BYTE_SIZE] {
        assert_eq!(
            size_of::<RuntimeMeasurementEvidence>(),
            BYTE_SIZE,
            "producer ABI size changed"
        );
        let mut bytes = [0_u8; BYTE_SIZE];
        // SAFETY: the producer's compile-time ABI assertions guarantee an exact
        // repr(C) sequence of 64 initialized four-byte u32/AtomicU32 fields with
        // no padding. This test owns `evidence`, and no writer can mutate it
        // while the stable snapshot is copied.
        let source = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(evidence).cast::<u8>(),
                size_of::<RuntimeMeasurementEvidence>(),
            )
        };
        bytes.copy_from_slice(source);
        bytes
    }

    fn assert_invalid(mut words: [u32; WORD_COUNT], word: usize, value: u32, expected: &str) {
        words[word] = value;
        let error = parse_words(words).expect_err("invalid evidence was accepted");
        assert!(
            error.contains(expected),
            "{error:?} does not contain {expected:?}"
        );
    }

    #[test]
    fn firmware_producer_layout_decodes_to_all_64_semantic_fields() {
        let evidence = RuntimeMeasurementEvidence::new();
        evidence.record_initialization_error(101);
        evidence.record_uptime_ms(102);
        evidence.record_psram_bytes(8_388_608);
        evidence.record_heap_snapshot(HeapSnapshot {
            total_bytes: 8_454_144,
            current_bytes: 1_010,
            maximum_bytes: 2_020,
            free_bytes: 8_452_124,
            internal_current_bytes: 303,
            internal_free_bytes: 64_501,
            external_current_bytes: 707,
            external_free_bytes: 8_300_009,
        });
        evidence.record_stack_snapshot(StackSnapshot {
            reserved_bytes: 32_768,
            usable_bytes: 32_000,
            painted_bytes: 31_744,
            high_water_bytes: 5_004,
            remaining_bytes: 26_996,
            guard_offset_bytes: 764,
            scan_valid: true,
            guard_intact: true,
        });
        evidence.record_composition_ready(106_006);

        for (index, phase) in [
            BootPhase::CredentialBoot,
            BootPhase::IdentityPreflight,
            BootPhase::JournalProvision,
            BootPhase::AnnounceEpoch,
            BootPhase::IdentityBoot,
            BootPhase::JournalMount,
            BootPhase::InboxMount,
            BootPhase::RadioInit,
        ]
        .into_iter()
        .enumerate()
        {
            let offset = index as u64 * 100;
            evidence.record_boot_phase(phase, 20_001 + offset);
            evidence.record_boot_phase(phase, 10_001 + offset);
        }

        for (index, operation) in [
            OperationKind::Inbound,
            OperationKind::AuthorizedFrame,
            OperationKind::Submission,
            OperationKind::ApiDispatch,
            OperationKind::Receive,
            OperationKind::Cad,
            OperationKind::Transmit,
        ]
        .into_iter()
        .enumerate()
        {
            evidence.record_operation(operation, 30_001 + index as u64 * 100);
            for elapsed_us in 1..=index {
                evidence.record_operation(operation, elapsed_us as u64 + 1);
            }
        }
        evidence.record_radio_timeout(OperationKind::Receive);
        evidence.record_radio_timeout(OperationKind::Cad);
        evidence.record_radio_timeout(OperationKind::Cad);
        evidence.record_radio_timeout(OperationKind::Transmit);
        evidence.record_radio_timeout(OperationKind::Transmit);
        evidence.record_radio_timeout(OperationKind::Transmit);
        evidence.record_node_loop_gap(40_001);
        evidence.record_radio_loop_gap(40_101);
        evidence.record_measurement_lateness(40_201);
        evidence.record_measurement_work(40_301);
        for _ in 0..4 {
            evidence.record_unexpected_error();
        }
        for success in [true, false, true, false, true, true] {
            evidence.record_allocation(success);
        }

        let decoded = DecodedEvidence::parse(&producer_bytes(&evidence))
            .expect("firmware-produced evidence must satisfy the host decoder");
        let expected_fields = [
            ("snapshot_seq_begin", 140),
            ("magic", MAGIC),
            ("version", 1),
            ("size_bytes", 256),
            ("flags.raw", 63),
            ("init_error", 101),
            ("uptime_ms", 102),
            ("memory.psram_bytes", 8_388_608),
            ("memory.heap_total_bytes", 8_454_144),
            ("memory.heap_current_bytes", 1_010),
            ("memory.heap_maximum_bytes", 2_020),
            ("memory.heap_minimum_free_bytes", 8_452_124),
            ("memory.internal_heap_current_bytes", 303),
            ("memory.internal_heap_minimum_free_bytes", 64_501),
            ("memory.external_heap_current_bytes", 707),
            ("memory.external_heap_minimum_free_bytes", 8_300_009),
            ("stack.reserved_bytes", 32_768),
            ("stack.usable_bytes", 32_000),
            ("stack.painted_bytes", 31_744),
            ("stack.high_water_bytes", 5_004),
            ("stack.minimum_remaining_bytes", 26_996),
            ("stack.guard_offset_bytes", 764),
            ("composition_ready_us", 106_006),
            ("boot.credential_boot.last_us", 10_001),
            ("boot.credential_boot.max_us", 20_001),
            ("boot.identity_preflight.last_us", 10_101),
            ("boot.identity_preflight.max_us", 20_101),
            ("boot.journal_provision.last_us", 10_201),
            ("boot.journal_provision.max_us", 20_201),
            ("boot.announce_epoch.last_us", 10_301),
            ("boot.announce_epoch.max_us", 20_301),
            ("boot.identity_boot.last_us", 10_401),
            ("boot.identity_boot.max_us", 20_401),
            ("boot.journal_mount.last_us", 10_501),
            ("boot.journal_mount.max_us", 20_501),
            ("boot.inbox_mount.last_us", 10_601),
            ("boot.inbox_mount.max_us", 20_601),
            ("boot.radio_init.last_us", 10_701),
            ("boot.radio_init.max_us", 20_701),
            ("operation.inbound.count", 1),
            ("operation.inbound.max_us", 30_001),
            ("operation.authorized_frame.count", 2),
            ("operation.authorized_frame.max_us", 30_101),
            ("operation.submission.count", 3),
            ("operation.submission.max_us", 30_201),
            ("operation.api_dispatch.count", 4),
            ("operation.api_dispatch.max_us", 30_301),
            ("operation.rx.count", 5),
            ("operation.rx.max_us", 30_401),
            ("operation.rx.timeout_count", 1),
            ("operation.cad.count", 6),
            ("operation.cad.max_us", 30_501),
            ("operation.cad.timeout_count", 2),
            ("operation.tx.count", 7),
            ("operation.tx.max_us", 30_601),
            ("operation.tx.timeout_count", 3),
            ("scheduler.node_loop_gap_max_us", 40_001),
            ("scheduler.radio_loop_gap_max_us", 40_101),
            ("scheduler.measurement_lateness_max_us", 40_201),
            ("scheduler.measurement_work_max_us", 40_301),
            ("errors.unexpected_count", 4),
            ("allocation.count", 6),
            ("allocation.failed_count", 2),
            ("snapshot_seq_end", 140),
        ];

        assert_eq!(expected_fields.len(), WORD_COUNT);
        for (word, (expected_name, expected_value)) in expected_fields.into_iter().enumerate() {
            assert_eq!(WORD_NAMES[word], expected_name, "decoder word {word}");
            assert_eq!(decoded.words[word], expected_value, "{expected_name}");
        }
    }

    #[test]
    fn cli_accepts_exact_decode_forms_in_either_flag_order() {
        assert_eq!(
            parse_options(&strings(&["decode", "--input", "capture.bin"])),
            Ok(Options {
                input: PathBuf::from("capture.bin"),
                json: false,
            })
        );
        assert_eq!(
            parse_options(&strings(&["decode", "--json", "--input", "capture.bin"])),
            Ok(Options {
                input: PathBuf::from("capture.bin"),
                json: true,
            })
        );
        assert_eq!(
            parse_options(&strings(&["decode", "--input", "capture.bin", "--json"])),
            Ok(Options {
                input: PathBuf::from("capture.bin"),
                json: true,
            })
        );
    }

    #[test]
    fn top_level_cli_accepts_exact_elf_inspection_forms() {
        let expected = CommandOptions::InspectElf(ElfInspectionOptions {
            default_elf: PathBuf::from("default.elf"),
            hil_elf: PathBuf::from("hil.elf"),
        });
        assert_eq!(
            parse_command_options(&strings(&[
                "inspect-elf",
                "--default-elf",
                "default.elf",
                "--hil-elf",
                "hil.elf",
            ])),
            Ok(expected)
        );
        assert_eq!(
            parse_command_options(&strings(&[
                "inspect-elf",
                "--hil-elf",
                "hil.elf",
                "--default-elf",
                "default.elf",
            ])),
            Ok(CommandOptions::InspectElf(ElfInspectionOptions {
                default_elf: PathBuf::from("default.elf"),
                hil_elf: PathBuf::from("hil.elf"),
            }))
        );
        assert!(matches!(
            parse_command_options(&strings(&["decode", "--input", "capture.bin"])),
            Ok(CommandOptions::Decode(_))
        ));
    }

    #[test]
    fn elf_inspection_cli_rejects_incomplete_duplicate_and_unknown_arguments() {
        for (args, expected) in [
            (strings(&[]), "decode or inspect-elf subcommand is required"),
            (strings(&["inspect"]), "unknown subcommand inspect"),
            (
                strings(&["inspect-elf", "--hil-elf", "hil.elf"]),
                "--default-elf is required",
            ),
            (
                strings(&["inspect-elf", "--default-elf", "default.elf"]),
                "--hil-elf is required",
            ),
            (
                strings(&["inspect-elf", "--default-elf"]),
                "--default-elf requires a value",
            ),
            (
                strings(&["inspect-elf", "--default-elf", "a", "--default-elf", "b"]),
                "--default-elf may be supplied only once",
            ),
            (
                strings(&["inspect-elf", "--default-elf=a", "--hil-elf", "b"]),
                "unknown option --default-elf=a",
            ),
            (
                strings(&["inspect-elf", "default.elf"]),
                "unexpected argument default.elf",
            ),
        ] {
            let error = parse_command_options(&args).expect_err("invalid CLI was accepted");
            assert_eq!(error, expected);
        }
    }

    #[test]
    fn stack_size_records_decode_exact_maximum_and_reject_malformed_sections() {
        let mut data = Vec::new();
        append_stack_size_record(&mut data, 0x4037_0000, 0);
        append_stack_size_record(&mut data, 0x4037_1000, 52_752);
        append_stack_size_record(&mut data, 0x4037_2000, 42_960);
        assert_eq!(
            parse_stack_size_records(&data, 4),
            Ok(StackSizeInventory {
                record_count: 3,
                maximum_frame_bytes: 52_752,
            })
        );

        for (data, width, expected) in [
            (Vec::new(), 4, "section is empty"),
            (vec![0; 5], 3, "unsupported function-address width"),
            (vec![0; 3], 4, "truncated function address"),
            (vec![0, 0, 0, 0, 0x80], 4, "truncated ULEB128"),
            (
                vec![
                    0, 0, 0, 0, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02,
                ],
                4,
                "overflowing ULEB128",
            ),
        ] {
            let error = parse_stack_size_records(&data, width)
                .expect_err("malformed .stack_sizes was accepted");
            assert!(
                error.contains(expected),
                "{error:?} does not contain {expected:?}"
            );
        }
    }

    #[test]
    fn stack_layout_uses_linker_guard_word_and_rejects_wrong_order() {
        assert_eq!(
            calculate_stack_layout(
                "runtime-measurement HIL",
                0x3fcb_1cd0,
                0x3fcb_1d0c,
                0x3fcd_b700,
            ),
            Ok(StackLayout {
                reserved_bytes: 170_544,
                usable_bytes: 170_480,
                guard_offset_bytes: 60,
            })
        );
        assert!(
            calculate_stack_layout("fixture", 100, 99, 200)
                .unwrap_err()
                .contains("below stack end")
        );
        assert!(
            calculate_stack_layout("fixture", 100, 160, 163)
                .unwrap_err()
                .contains("below the end of its guard word")
        );
        assert!(
            calculate_stack_layout("fixture", 100, u64::MAX, u64::MAX)
                .unwrap_err()
                .contains("overflows")
        );
    }

    #[test]
    fn elf_policy_accepts_reviewed_bounds_and_rejects_frame_or_stack_regressions() {
        let reviewed = ElfInspection {
            default_stack_sizes: StackSizeInventory {
                record_count: 1_025,
                maximum_frame_bytes: 52_752,
            },
            default_stack: StackLayout {
                reserved_bytes: 171_048,
                usable_bytes: 170_984,
                guard_offset_bytes: 60,
            },
            default_proof_trace_symbol_count: 0,
            hil_stack_sizes: StackSizeInventory {
                record_count: 1_025,
                maximum_frame_bytes: 52_752,
            },
            hil_stack: StackLayout {
                reserved_bytes: 170_352,
                usable_bytes: 170_288,
                guard_offset_bytes: 60,
            },
            hil_proof_trace_symbol_count: 1,
            hil_proof_trace_symbol_size_bytes: 192,
        };
        reviewed.validate().unwrap();
        let output = reviewed.render();
        assert!(output.contains("default.maximum_frame_bytes=52752\n"));
        assert!(output.contains("default.stack_usable_bytes=170984\n"));
        assert!(output.contains("default.proof_trace_symbol_count=0\n"));
        assert!(output.contains("hil.stack_usable_bytes=170288\n"));
        assert!(output.contains("hil.proof_trace_symbol_count=1\n"));
        assert!(output.contains("hil.proof_trace_symbol_size_bytes=192\n"));
        assert!(output.ends_with("qualification.conservative_margin_bytes=19268"));

        let mut regressed = reviewed;
        regressed.default_proof_trace_symbol_count = 1;
        assert!(regressed.validate().unwrap_err().contains("must exclude"));

        let mut regressed = reviewed;
        regressed.hil_proof_trace_symbol_count = 0;
        assert!(
            regressed
                .validate()
                .unwrap_err()
                .contains("one initialized")
        );

        let mut regressed = reviewed;
        regressed.hil_proof_trace_symbol_size_bytes = 191;
        assert!(regressed.validate().unwrap_err().contains("size=191"));

        let mut regressed = reviewed;
        regressed.default_stack_sizes.maximum_frame_bytes += 1;
        assert!(regressed.validate().unwrap_err().contains("frame 52753"));

        let mut regressed = reviewed;
        regressed.hil_stack_sizes.maximum_frame_bytes += 1;
        assert!(regressed.validate().unwrap_err().contains("frame 52753"));

        let mut regressed = reviewed;
        regressed.default_stack.usable_bytes -= 1;
        assert!(
            regressed
                .validate()
                .unwrap_err()
                .contains("usable stack 170983")
        );

        let mut regressed = reviewed;
        regressed.hil_stack.usable_bytes -= 1;
        assert!(
            regressed
                .validate()
                .unwrap_err()
                .contains("usable stack 170287")
        );

        let mut regressed = reviewed;
        regressed.hil_stack.guard_offset_bytes += 4;
        assert!(
            regressed
                .validate()
                .unwrap_err()
                .contains("guard offset 64")
        );

        let mut empty = reviewed;
        empty.default_stack_sizes.record_count = 0;
        assert!(
            empty
                .validate()
                .unwrap_err()
                .contains("contains no records")
        );
    }

    #[test]
    fn cli_rejects_missing_duplicate_joined_unknown_and_positional_arguments() {
        for (args, expected) in [
            (strings(&[]), "decode subcommand is required"),
            (strings(&["inspect"]), "subcommand must be decode"),
            (strings(&["decode"]), "--input is required"),
            (strings(&["decode", "--input"]), "--input requires a value"),
            (
                strings(&["decode", "--input", "--json"]),
                "--input requires a value",
            ),
            (
                strings(&["decode", "--input=a.bin"]),
                "unknown option --input=a.bin",
            ),
            (
                strings(&["decode", "--json=true"]),
                "unknown option --json=true",
            ),
            (
                strings(&["decode", "--input", "a", "--input", "b"]),
                "--input may be supplied only once",
            ),
            (
                strings(&["decode", "--input", "a", "--json", "--json"]),
                "--json may be supplied only once",
            ),
            (
                strings(&["decode", "--input", "a", "--wat"]),
                "unknown option --wat",
            ),
            (
                strings(&["decode", "--input", "a", "extra"]),
                "unexpected argument extra",
            ),
        ] {
            let error = parse_options(&args).expect_err("invalid CLI was accepted");
            assert_eq!(error, expected);
        }
    }

    #[test]
    fn parse_requires_exact_size_and_decodes_every_word_little_endian() {
        for length in [0, BYTE_SIZE - 1, BYTE_SIZE + 1] {
            let error = DecodedEvidence::parse(&vec![0_u8; length])
                .expect_err("wrong-size input was accepted");
            assert_eq!(
                error,
                format!("input must be exactly {BYTE_SIZE} bytes, got {length}")
            );
        }

        let words = populated_words();
        let evidence = parse_words(words).unwrap();
        assert_eq!(evidence.words, words);
        assert_eq!(
            &encode(words)[UPTIME_WORD * 4..(UPTIME_WORD + 1) * 4],
            &120_000_u32.to_le_bytes()
        );
    }

    #[test]
    fn header_validation_rejects_wrong_identity_flags_and_unstable_snapshot() {
        let words = populated_words();
        assert_invalid(words, MAGIC_WORD, u32::from_le_bytes(*b"NOPE"), "magic");
        assert_invalid(words, VERSION_WORD, 2, "version must be 1");
        assert_invalid(words, SIZE_WORD, 255, "size_bytes must be 256");
        assert_invalid(
            words,
            FLAGS_WORD,
            words[FLAGS_WORD] | (1 << 31),
            "unknown bits",
        );
        assert_invalid(
            words,
            FLAGS_WORD,
            words[FLAGS_WORD] & !FLAG_ACTIVE,
            "flags.active",
        );
        assert_invalid(words, SNAPSHOT_SEQ_BEGIN_WORD, 201, "markers must match");
        assert_invalid(words, SNAPSHOT_SEQ_END_WORD, 202, "markers must match");
        let mut odd = words;
        odd[SNAPSHOT_SEQ_BEGIN_WORD] = 201;
        odd[SNAPSHOT_SEQ_END_WORD] = 201;
        let error = parse_words(odd).unwrap_err();
        assert!(error.contains("markers must be even"), "{error}");

        let mut torn_header = words;
        torn_header[SNAPSHOT_SEQ_BEGIN_WORD] = 198;
        torn_header[MAGIC_WORD] = u32::from_le_bytes(*b"NOPE");
        let error = parse_words(torn_header).unwrap_err();
        assert!(error.contains("markers must match"), "{error}");
    }

    #[test]
    fn registration_flags_and_minimum_sentinels_must_agree() {
        let words = minimal_words();
        assert_invalid(words, HEAP_MINIMUM_FREE_WORD, 100, "must be unobserved");
        assert_invalid(
            words,
            STACK_MINIMUM_REMAINING_WORD,
            100,
            "must be unobserved",
        );

        let words = populated_words();
        assert_invalid(
            words,
            HEAP_MINIMUM_FREE_WORD,
            UNOBSERVED_MINIMUM,
            "must be observed",
        );
        assert_invalid(
            words,
            INTERNAL_HEAP_MINIMUM_FREE_WORD,
            UNOBSERVED_MINIMUM,
            "must be observed",
        );
        assert_invalid(
            words,
            EXTERNAL_HEAP_MINIMUM_FREE_WORD,
            UNOBSERVED_MINIMUM,
            "must be observed",
        );
        assert_invalid(
            words,
            STACK_MINIMUM_REMAINING_WORD,
            UNOBSERVED_MINIMUM,
            "must be observed",
        );

        let mut words = minimal_words();
        words[FLAGS_WORD] |= FLAG_SCAN_VALID;
        let error = parse_words(words).unwrap_err();
        assert!(error.contains("validity flags"), "{error}");
    }

    #[test]
    fn heap_validation_rejects_impossible_aggregate_and_regional_values() {
        let words = populated_words();
        assert_invalid(words, HEAP_TOTAL_WORD, 0, "must be nonzero");
        assert_invalid(
            words,
            HEAP_CURRENT_WORD,
            words[HEAP_MAXIMUM_WORD] + 1,
            "current_bytes must not exceed",
        );
        assert_invalid(
            words,
            HEAP_MAXIMUM_WORD,
            words[HEAP_TOTAL_WORD] + 1,
            "maximum_bytes must not exceed",
        );
        assert_invalid(
            words,
            HEAP_MINIMUM_FREE_WORD,
            words[HEAP_TOTAL_WORD],
            "must not exceed current free heap",
        );
        assert_invalid(
            words,
            INTERNAL_HEAP_CURRENT_WORD,
            words[HEAP_TOTAL_WORD] + 1,
            "must not exceed memory.heap_total_bytes",
        );
        assert_invalid(
            words,
            PSRAM_BYTES_WORD,
            words[EXTERNAL_HEAP_MINIMUM_FREE_WORD] - 1,
            "must not exceed memory.psram_bytes",
        );
    }

    #[test]
    fn stack_validation_rejects_alignment_bounds_and_watermark_contradictions() {
        let words = populated_words();
        assert_invalid(words, STACK_RESERVED_WORD, 0, "must be nonzero");
        assert_invalid(
            words,
            STACK_PAINTED_WORD,
            words[STACK_PAINTED_WORD] + 1,
            "four-byte aligned",
        );
        assert_invalid(
            words,
            STACK_GUARD_OFFSET_WORD,
            words[STACK_RESERVED_WORD],
            "guard must fit",
        );
        assert_invalid(
            words,
            STACK_USABLE_WORD,
            words[STACK_USABLE_WORD] - 4,
            "must equal reserved bytes above the guard",
        );
        assert_invalid(
            words,
            STACK_PAINTED_WORD,
            words[STACK_RESERVED_WORD],
            "exceeds the non-guard reservation",
        );
        assert_invalid(
            words,
            STACK_HIGH_WATER_WORD,
            words[STACK_RESERVED_WORD] + 4,
            "exceeds stack.reserved_bytes",
        );
        assert_invalid(
            words,
            STACK_MINIMUM_REMAINING_WORD,
            words[STACK_MINIMUM_REMAINING_WORD] - 4,
            "does not match the high-water bound",
        );
    }

    #[test]
    fn boot_operation_timeout_and_allocation_invariants_are_enforced() {
        let words = populated_words();
        assert_invalid(
            words,
            BOOT_FIRST_WORD,
            words[BOOT_FIRST_WORD + 1] + 1,
            "boot.credential_boot.last_us",
        );
        assert_invalid(words, INBOUND_COUNT_WORD, 0, "timing requires a nonzero");
        assert_invalid(
            words,
            RX_TIMEOUT_WORD,
            words[RX_COUNT_WORD] + 1,
            "operation.rx.timeout_count",
        );
        assert_invalid(
            words,
            CAD_TIMEOUT_WORD,
            words[CAD_COUNT_WORD] + 1,
            "operation.cad.timeout_count",
        );
        assert_invalid(
            words,
            TX_TIMEOUT_WORD,
            words[TX_COUNT_WORD] + 1,
            "operation.tx.timeout_count",
        );
        assert_invalid(
            words,
            FAILED_ALLOCATION_COUNT_WORD,
            words[ALLOCATION_COUNT_WORD] + 1,
            "allocation.failed_count",
        );
    }

    #[test]
    fn human_output_is_deterministic_complete_and_key_value_only() {
        let evidence = parse_words(populated_words()).unwrap();
        let output = evidence.render_human();
        assert_eq!(output, evidence.render_human());
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), WORD_COUNT + FLAG_NAMES.len());
        assert_eq!(lines[0], "snapshot_seq_begin=200");
        assert_eq!(lines[1], "magic=RTME");
        assert_eq!(lines[2], "version=1");
        assert_eq!(lines[3], "size_bytes=256");
        assert_eq!(
            lines[4],
            format!("flags.raw={}", populated_words()[FLAGS_WORD])
        );
        assert_eq!(lines[5], "flags.active=true");
        assert_eq!(lines[11], "flags.saturated=false");
        assert!(lines.contains(&"scheduler.measurement_work_max_us=200"));
        assert_eq!(lines.last(), Some(&"snapshot_seq_end=200"));
        assert!(!output.contains("sample_count="));
        assert!(!output.contains("operation.inbound.last_us="));
        assert!(lines.iter().all(|line| line.split_once('=').is_some()));
        for name in WORD_NAMES {
            assert_eq!(
                lines
                    .iter()
                    .filter(|line| line.starts_with(&format!("{name}=")))
                    .count(),
                1,
                "field {name} was missing or duplicated"
            );
        }

        let minimal = parse_words(minimal_words()).unwrap().render_human();
        assert!(minimal.contains("memory.heap_minimum_free_bytes=unobserved\n"));
        assert!(minimal.contains("stack.minimum_remaining_bytes=unobserved\n"));
    }

    #[test]
    fn json_output_is_valid_deterministic_complete_and_typed() {
        let evidence = parse_words(populated_words()).unwrap();
        let output = evidence.render_json();
        assert_eq!(output, evidence.render_json());
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), WORD_COUNT + FLAG_NAMES.len());
        assert_eq!(object["magic"], "RTME");
        assert_eq!(object["version"], 1);
        assert_eq!(object["flags.active"], true);
        assert_eq!(object["flags.saturated"], false);
        assert_eq!(object["boot.radio_init.max_us"], 27);
        assert_eq!(object["operation.rx.timeout_count"], 1);
        assert_eq!(object["scheduler.measurement_work_max_us"], 200);
        assert_eq!(object["allocation.failed_count"], 2);
        assert_eq!(object["snapshot_seq_begin"], 200);
        assert_eq!(object["snapshot_seq_end"], 200);

        let minimal = parse_words(minimal_words()).unwrap().render_json();
        let minimal: serde_json::Value = serde_json::from_str(&minimal).unwrap();
        assert!(minimal["memory.heap_minimum_free_bytes"].is_null());
        assert!(minimal["stack.minimum_remaining_bytes"].is_null());
    }

    #[test]
    fn execute_reads_one_binary_and_selects_requested_output_format() {
        let input = TempInput::new(&encode(populated_words()));
        let human = execute(&Options {
            input: input.path().to_owned(),
            json: false,
        })
        .unwrap();
        assert!(human.starts_with("snapshot_seq_begin=200\nmagic=RTME\nversion=1\n"));

        let json = execute(&Options {
            input: input.path().to_owned(),
            json: true,
        })
        .unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());

        let missing = input.path().with_extension("missing");
        let error = execute(&Options {
            input: missing,
            json: false,
        })
        .unwrap_err();
        assert!(error.starts_with("could not read --input"), "{error}");
    }

    fn proof_minimal_words() -> [u32; PROOF_WORD_COUNT] {
        let mut words = [0_u32; PROOF_WORD_COUNT];
        words[PROOF_MAGIC_WORD] = PROOF_MAGIC;
        words[PROOF_VERSION_WORD] = PROOF_VERSION;
        words[PROOF_SIZE_WORD] = PROOF_BYTE_SIZE as u32;
        words[PROOF_FLAGS_WORD] = PROOF_FLAG_ACTIVE | PROOF_FLAG_INBOX_COMMIT_ORDER_CONSISTENT;
        words
    }

    fn encode_proof(words: [u32; PROOF_WORD_COUNT]) -> [u8; PROOF_BYTE_SIZE] {
        let mut bytes = [0_u8; PROOF_BYTE_SIZE];
        for (word, destination) in words
            .into_iter()
            .zip(bytes.chunks_exact_mut(size_of::<u32>()))
        {
            destination.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    fn proof_producer_bytes(evidence: &RuntimeProofTraceEvidence) -> [u8; PROOF_BYTE_SIZE] {
        assert_eq!(
            size_of::<RuntimeProofTraceEvidence>(),
            PROOF_BYTE_SIZE,
            "proof producer ABI size changed"
        );
        let mut bytes = [0_u8; PROOF_BYTE_SIZE];
        // SAFETY: the firmware module's compile-time assertions guarantee an
        // exact repr(C) sequence of initialized four-byte fields with no
        // padding. This test owns `evidence`, and no writer can mutate it while
        // the matching-even snapshot is copied.
        let source = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(evidence).cast::<u8>(),
                size_of::<RuntimeProofTraceEvidence>(),
            )
        };
        bytes.copy_from_slice(source);
        bytes
    }

    fn populated_proof_evidence() -> RuntimeProofTraceEvidence {
        let evidence = RuntimeProofTraceEvidence::new();
        evidence.record_logical_rx_completed(10);
        evidence.record_ingress_enqueued(11);
        evidence.record_ingress_deferred(12);
        evidence.record_ingress_failed(13);
        evidence.record_rns_ingress(
            14,
            RuntimeProofTraceIngressDisposition::Processed,
            RuntimeProofTraceIngressMetadata {
                wire_packet_type: RuntimeProofTracePacketType::Proof,
                emitted_packets: 2,
                generated_proof_actions: 3,
                delivered_receipt_terminals: 4,
                timed_out_receipt_terminals: 0,
                generated_proof_tag: Some(0x1122_3344_5566_7788),
                delivered_receipt_tag: Some(0x8877_6655_4433_2211),
                generated_proof_tags_consistent: true,
                delivered_receipt_tags_consistent: false,
                counts_saturated: false,
            },
        );
        for (at, disposition) in [
            (15, RuntimeProofTraceIngressDisposition::NativeDuplicate),
            (16, RuntimeProofTraceIngressDisposition::NativeInvalid),
            (17, RuntimeProofTraceIngressDisposition::NoObservableOutcome),
            (18, RuntimeProofTraceIngressDisposition::Rejected),
        ] {
            evidence.record_rns_ingress(
                at,
                disposition,
                RuntimeProofTraceIngressMetadata::default(),
            );
        }
        evidence.record_receipt_timeouts(19, 2, Some(0xaabb_ccdd_eeff_0011), true);
        evidence.record_action_pressure(20);
        evidence.record_correlation_fault(21);
        evidence.record_inbox_commit_started(22);
        evidence.record_inbox_commit_finished(23);
        evidence.record_radio_tx_confirmed_success();
        evidence.record_radio_tx_not_confirmed_success();
        evidence
    }

    #[test]
    fn proof_decoder_requires_exact_stable_versioned_192_byte_abi() {
        for length in [0, PROOF_BYTE_SIZE - 1, PROOF_BYTE_SIZE + 1] {
            let error = DecodedProofTraceEvidence::parse(&vec![0; length])
                .expect_err("wrong proof-trace size was accepted");
            assert_eq!(
                error,
                format!("proof-trace input must be exactly {PROOF_BYTE_SIZE} bytes, got {length}")
            );
        }

        let words = proof_minimal_words();
        let empty = DecodedProofTraceEvidence::parse(&encode_proof(words)).unwrap();
        assert_eq!(empty.words, words);
        empty
            .validate_empty_initializer()
            .expect("canonical linked initializer must be empty");

        let mut runtime_observation = words;
        runtime_observation[PROOF_RADIO_TX_CONFIRMED_SUCCESS_COUNT_WORD] = 1;
        let runtime_observation =
            DecodedProofTraceEvidence::parse(&encode_proof(runtime_observation))
                .expect("a nonempty runtime TX observation must decode");
        assert!(
            runtime_observation
                .validate_empty_initializer()
                .unwrap_err()
                .contains("radio_tx.confirmed_success.count must initialize to 0")
        );
        for (word, value, expected) in [
            (PROOF_MAGIC_WORD, u32::from_le_bytes(*b"NOPE"), "magic"),
            (PROOF_VERSION_WORD, 2, "version"),
            (PROOF_SIZE_WORD, 128, "size_bytes"),
            (PROOF_FLAGS_WORD, 0, "flags.active"),
            (
                PROOF_FLAGS_WORD,
                PROOF_FLAG_ACTIVE | PROOF_FLAG_INBOX_COMMIT_ORDER_CONSISTENT | (1 << 31),
                "unknown bits",
            ),
        ] {
            let mut malformed = words;
            malformed[word] = value;
            let error = DecodedProofTraceEvidence::parse(&encode_proof(malformed))
                .expect_err("malformed proof-trace ABI was accepted");
            assert!(error.contains(expected), "{error:?}");
        }

        let mut unstable = words;
        unstable[PROOF_SNAPSHOT_SEQ_BEGIN_WORD] = 1;
        unstable[PROOF_SNAPSHOT_SEQ_END_WORD] = 1;
        assert!(
            DecodedProofTraceEvidence::parse(&encode_proof(unstable))
                .unwrap_err()
                .contains("must be even")
        );
        unstable[PROOF_SNAPSHOT_SEQ_BEGIN_WORD] = 2;
        assert!(
            DecodedProofTraceEvidence::parse(&encode_proof(unstable))
                .unwrap_err()
                .contains("must match")
        );
    }

    #[test]
    fn firmware_proof_producer_layout_decodes_all_words_and_wide_tags() {
        let evidence = populated_proof_evidence();
        let decoded = DecodedProofTraceEvidence::parse(&proof_producer_bytes(&evidence))
            .expect("firmware proof trace must satisfy its host decoder");
        assert_eq!(decoded.words.len(), PROOF_WORD_COUNT);
        assert_eq!(decoded.words[PROOF_SNAPSHOT_SEQ_BEGIN_WORD], 32);
        assert_eq!(decoded.words[PROOF_SNAPSHOT_SEQ_END_WORD], 32);
        assert_eq!(decoded.words[PROOF_LOGICAL_RX_COUNT_WORD], 1);
        assert_eq!(decoded.words[PROOF_RNS_INGRESS_COUNT_WORD], 5);
        assert_eq!(decoded.words[PROOF_GENERATED_COUNT_WORD], 3);
        assert_eq!(decoded.words[PROOF_DELIVERED_COUNT_WORD], 4);
        assert_eq!(decoded.words[PROOF_TIMEOUT_COUNT_WORD], 2);
        assert_eq!(decoded.words[PROOF_DISPOSITION_PROCESSED_WORD], 1);
        assert_eq!(decoded.words[PROOF_DISPOSITION_DUPLICATE_WORD], 1);
        assert_eq!(decoded.words[PROOF_DISPOSITION_INVALID_WORD], 1);
        assert_eq!(decoded.words[PROOF_DISPOSITION_NO_OUTCOME_WORD], 1);
        assert_eq!(decoded.words[PROOF_DISPOSITION_REJECTED_WORD], 1);
        assert_eq!(decoded.words[PROOF_LAST_DISPOSITION_WORD], 5);
        assert_eq!(decoded.words[PROOF_LAST_PACKET_TYPE_WORD], 0);
        assert_eq!(
            decoded.words[PROOF_RADIO_TX_CONFIRMED_SUCCESS_COUNT_WORD],
            1
        );
        assert_eq!(
            decoded.words[PROOF_RADIO_TX_NOT_CONFIRMED_SUCCESS_COUNT_WORD],
            1
        );

        let human = decoded.render_human();
        assert!(human.starts_with("snapshot_seq_begin=32\nmagic=RPTE\nversion=1\n"));
        assert!(human.contains("flags.generated_tag_present=true\n"));
        assert!(human.contains("flags.delivered_tags_consistent=false\n"));
        assert!(human.contains("rns_ingress.last_disposition=rejected\n"));
        assert!(human.contains("rns_ingress.last_wire_packet_type=unparsed\n"));
        assert!(human.contains(
            "radio_tx.confirmed_success.count=1\nradio_tx.not_confirmed_success.count=1\n"
        ));
        assert!(human.contains("tag.generated=0x1122334455667788\n"));
        assert!(human.contains("tag.delivered=0x8877665544332211\n"));
        assert!(human.ends_with("tag.timeout=0xaabbccddeeff0011"));

        let json: serde_json::Value = serde_json::from_str(&decoded.render_json()).unwrap();
        let object = json.as_object().unwrap();
        assert_eq!(object.len(), PROOF_WORD_COUNT + PROOF_FLAG_NAMES.len() + 3);
        assert_eq!(object["magic"], "RPTE");
        assert_eq!(object["tag.generated"], "0x1122334455667788");
        assert_eq!(object["tag.delivered"], "0x8877665544332211");
        assert_eq!(object["tag.timeout"], "0xaabbccddeeff0011");
        assert_eq!(object["radio_tx.confirmed_success.count"], 1);
        assert_eq!(object["radio_tx.not_confirmed_success.count"], 1);
    }

    #[test]
    fn proof_decoder_rejects_cross_field_corruption_but_accepts_saturation() {
        let producer = populated_proof_evidence();
        let decoded = DecodedProofTraceEvidence::parse(&proof_producer_bytes(&producer)).unwrap();
        let words = decoded.words;
        for (word, value, expected) in [
            (PROOF_DISPOSITION_REJECTED_WORD, 2, "disposition counts sum"),
            (PROOF_LAST_DISPOSITION_WORD, 6, "must be in 1..=5"),
            (PROOF_LAST_PACKET_TYPE_WORD, 5, "must be in 0..=4"),
            (
                PROOF_GENERATED_TAG_LOW_WORD,
                0,
                "generated tag words must be zero",
            ),
        ] {
            let mut malformed = words;
            malformed[word] = value;
            if word == PROOF_GENERATED_TAG_LOW_WORD {
                malformed[PROOF_FLAGS_WORD] &= !PROOF_FLAG_GENERATED_TAG_PRESENT;
            }
            let error = DecodedProofTraceEvidence::parse(&encode_proof(malformed))
                .expect_err("cross-field corruption was accepted");
            assert!(error.contains(expected), "{error:?}");
        }

        let mut saturated = words;
        saturated[PROOF_FLAGS_WORD] |= PROOF_FLAG_SATURATED;
        saturated[PROOF_RNS_INGRESS_COUNT_WORD] = u32::MAX;
        saturated[PROOF_DISPOSITION_PROCESSED_WORD] = u32::MAX;
        DecodedProofTraceEvidence::parse(&encode_proof(saturated))
            .expect("saturated disposition aggregates remain decodable");

        let mut unrelated_saturation = words;
        unrelated_saturation[PROOF_FLAGS_WORD] |= PROOF_FLAG_SATURATED;
        unrelated_saturation[PROOF_DISPOSITION_REJECTED_WORD] += 1;
        assert!(
            DecodedProofTraceEvidence::parse(&encode_proof(unrelated_saturation))
                .unwrap_err()
                .contains("disposition counts sum")
        );

        let mut unrelated_saturation = words;
        unrelated_saturation[PROOF_FLAGS_WORD] |= PROOF_FLAG_SATURATED;
        unrelated_saturation[PROOF_LAST_GENERATED_ACTIONS_WORD] =
            unrelated_saturation[PROOF_GENERATED_COUNT_WORD] + 1;
        assert!(
            DecodedProofTraceEvidence::parse(&encode_proof(unrelated_saturation))
                .unwrap_err()
                .contains("must not exceed")
        );
    }

    #[test]
    fn proof_trace_cli_parses_and_executes_human_and_json_forms() {
        let expected = Options {
            input: PathBuf::from("proof.bin"),
            json: true,
        };
        assert_eq!(
            parse_proof_trace_options(&strings(&[
                "decode-proof-trace",
                "--json",
                "--input",
                "proof.bin",
            ])),
            Ok(expected)
        );
        assert!(matches!(
            parse_command_options(&strings(&["decode-proof-trace", "--input", "proof.bin"])),
            Ok(CommandOptions::DecodeProofTrace(_))
        ));
        for (args, expected) in [
            (strings(&["decode-proof-trace"]), "--input is required"),
            (
                strings(&["decode-proof-trace", "--input", "a", "--input", "b"]),
                "--input may be supplied only once",
            ),
            (
                strings(&["decode-proof-trace", "--input", "a", "--wat"]),
                "unknown option --wat",
            ),
        ] {
            assert_eq!(parse_proof_trace_options(&args).unwrap_err(), expected);
        }

        let evidence = populated_proof_evidence();
        let input = TempInput::new(&proof_producer_bytes(&evidence));
        let human = execute_proof_trace(&Options {
            input: input.path().to_owned(),
            json: false,
        })
        .unwrap();
        assert!(human.contains("proof.generated.count=3\n"));
        let json = execute_proof_trace(&Options {
            input: input.path().to_owned(),
            json: true,
        })
        .unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
    }
}
