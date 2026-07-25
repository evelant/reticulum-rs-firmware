//! Decode the fixed E290 runtime-measurement HIL evidence ABI and inspect its
//! linked stack bounds.

use object::{
    Architecture, BinaryFormat, Endianness, Object, ObjectKind, ObjectSection, ObjectSymbol,
    SectionKind, SymbolKind, SymbolSection,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    env,
    ffi::OsString,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::Write as _,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
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
const LXMF_WORD_COUNT: usize = 24;
const LXMF_BYTE_SIZE: usize = LXMF_WORD_COUNT * size_of::<u32>();
const LXMF_MAGIC: u32 = u32::from_le_bytes(*b"LXTE");
const LXMF_VERSION: u32 = 1;
const MEASUREMENT_EVIDENCE_SYMBOL_FRAGMENT: &str = "RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE";
const PROOF_TRACE_EVIDENCE_SYMBOL_FRAGMENT: &str = "RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE";
const LXMF_TRACE_EVIDENCE_SYMBOL_FRAGMENT: &str = "RETICULUM_RUNTIME_LXMF_TRACE_EVIDENCE";

const CHECKPOINT_BYTE_SIZE: usize = BYTE_SIZE + PROOF_BYTE_SIZE + LXMF_BYTE_SIZE;
const CHECKPOINT_SCHEMA: &str = "reticulum.e290-runtime-checkpoint.v2";
const CAPTURE_SCHEMA: &str = "reticulum.e290-runtime-checkpoint-capture.v2";
const CAPTURE_INCOMPLETE_FILE: &str = "checkpoint.incomplete";
const CAPTURE_COMPLETE_FILE: &str = "checkpoint.complete";
const CAPTURE_RAW_FILE: &str = "checkpoint.bin";
const CAPTURE_RUNTIME_FILE: &str = "runtime.bin";
const CAPTURE_PROOF_FILE: &str = "proof-trace.bin";
const CAPTURE_LXMF_FILE: &str = "lxmf-trace.bin";
const CAPTURE_HUMAN_FILE: &str = "checkpoint.txt";
const CAPTURE_JSON_FILE: &str = "checkpoint.json";
const CAPTURE_MANIFEST_FILE: &str = "manifest.json";
const PROBE_LAUNCH_DIRECTORY: &str = "probe-launch";
const PROBE_CWD_DIRECTORY: &str = "probe-launch/cwd";
const PROBE_HOME_DIRECTORY: &str = "probe-launch/home";
const PROBE_CONFIG_FILE: &str = "probe-launch/cwd/.probe-rs.toml";
const PROBE_CONFIG_NAMES: [&str; 4] = [
    ".probe-rs.toml",
    ".probe-rs.json",
    ".probe-rs.yaml",
    ".probe-rs.yml",
];
const CAPTURE_INCOMPLETE_CONTENT: &str =
    "reticulum.e290-runtime-checkpoint-capture.v2\nstatus=incomplete\n";
const EMPTY_PROBE_CONFIG: &[u8] = b"";
const DEFAULT_PROBE_RS: &str = "probe-rs";
const E290_PROBE_VID_PID: &str = "303a:1001";
const PROBE_FAILURE_GUIDANCE: &str = "target halt/resume state is uncertain; abandon this checkpoint and trial, then recover or restart the board manually; capture-checkpoint will not reset or resume it";

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

const LXMF_FLAG_ACTIVE: u32 = 1 << 0;
const LXMF_FLAG_SATURATED: u32 = 1 << 1;
const LXMF_FLAG_LAST_COMMIT_PRESENT: u32 = 1 << 2;
const LXMF_FLAG_LAST_COMMIT_NEW: u32 = 1 << 3;
const LXMF_FLAG_LAST_COMMIT_ALREADY_DURABLE: u32 = 1 << 4;
const LXMF_FLAG_PROOF_TAG_PRESENT: u32 = 1 << 5;
const LXMF_FLAG_ORDERING_VIOLATION: u32 = 1 << 6;
const LXMF_FLAG_INPUT_INCONSISTENT: u32 = 1 << 7;
const LXMF_KNOWN_FLAG_MASK: u32 = LXMF_FLAG_ACTIVE
    | LXMF_FLAG_SATURATED
    | LXMF_FLAG_LAST_COMMIT_PRESENT
    | LXMF_FLAG_LAST_COMMIT_NEW
    | LXMF_FLAG_LAST_COMMIT_ALREADY_DURABLE
    | LXMF_FLAG_PROOF_TAG_PRESENT
    | LXMF_FLAG_ORDERING_VIOLATION
    | LXMF_FLAG_INPUT_INCONSISTENT;

const LXMF_FLAG_NAMES: [(&str, u32); 8] = [
    ("flags.active", LXMF_FLAG_ACTIVE),
    ("flags.saturated", LXMF_FLAG_SATURATED),
    ("flags.last_commit_present", LXMF_FLAG_LAST_COMMIT_PRESENT),
    ("flags.last_commit_new", LXMF_FLAG_LAST_COMMIT_NEW),
    (
        "flags.last_commit_already_durable",
        LXMF_FLAG_LAST_COMMIT_ALREADY_DURABLE,
    ),
    ("flags.proof_tag_present", LXMF_FLAG_PROOF_TAG_PRESENT),
    ("flags.ordering_violation", LXMF_FLAG_ORDERING_VIOLATION),
    ("flags.input_inconsistent", LXMF_FLAG_INPUT_INCONSISTENT),
];

const LXMF_WORD_NAMES: [&str; LXMF_WORD_COUNT] = [
    "snapshot_seq_begin",
    "magic",
    "version",
    "size_bytes",
    "flags.raw",
    "durable.new.count",
    "durable.already_durable.count",
    "proof.ready.count",
    "proof.released.count",
    "proof.ordinary_handoff.count",
    "ordering.violation.count",
    "last_message_id.word0",
    "last_message_id.word1",
    "last_message_id.word2",
    "last_message_id.word3",
    "last_message_id.word4",
    "last_message_id.word5",
    "last_message_id.word6",
    "last_message_id.word7",
    "last_durable_handle.low",
    "last_durable_handle.high",
    "last_proof_tag.low",
    "last_proof_tag.high",
    "snapshot_seq_end",
];

const LXMF_SNAPSHOT_SEQ_BEGIN_WORD: usize = 0;
const LXMF_MAGIC_WORD: usize = 1;
const LXMF_VERSION_WORD: usize = 2;
const LXMF_SIZE_WORD: usize = 3;
const LXMF_FLAGS_WORD: usize = 4;
const LXMF_DURABLE_NEW_COUNT_WORD: usize = 5;
const LXMF_DURABLE_ALREADY_COUNT_WORD: usize = 6;
const LXMF_PROOF_READY_COUNT_WORD: usize = 7;
const LXMF_PROOF_RELEASED_COUNT_WORD: usize = 8;
const LXMF_PROOF_HANDOFF_COUNT_WORD: usize = 9;
const LXMF_ORDERING_VIOLATION_COUNT_WORD: usize = 10;
const LXMF_LAST_MESSAGE_ID_FIRST_WORD: usize = 11;
const LXMF_LAST_MESSAGE_ID_LAST_WORD: usize = 18;
const LXMF_LAST_HANDLE_LOW_WORD: usize = 19;
const LXMF_LAST_HANDLE_HIGH_WORD: usize = 20;
const LXMF_LAST_PROOF_TAG_LOW_WORD: usize = 21;
const LXMF_LAST_PROOF_TAG_HIGH_WORD: usize = 22;
const LXMF_SNAPSHOT_SEQ_END_WORD: usize = 23;

// The powered 2026-07-20 qualification observed 72,212 bytes of raw painted
// stack margin. RPTE v1 adds one exact 192-byte initialized internal-RAM
// object, and the linked stack boundary moves down by the same 192 bytes. The
// source accumulated another exact 3,544 bytes of linked internal-RAM growth
// through the interface-lifecycle tranche. The application-event ownership
// tranche moved both stack boundaries down by a further exact 2,408 bytes. The
// later retained-proof and mounted durable-LXMF composition moves the final
// post-offload default profile down another exact 2,632 bytes; the independently
// gated HIL profile moves down 2,640 bytes. The historical policy calculation
// carries the default-profile deduction. Most volatile LXMF state is now
// explicitly in PSRAM; these deltas are measured linked layout, not the size of
// that external state.
// That linked-only carry-forward was 63,436 bytes. The 2026-07-21 Stage 5 HIL
// supersedes it with a lower observed 57,716-byte raw painted margin, so policy
// must fail closed to the powered value. This still does not turn a modified-
// word watermark into minimum-SP proof. The exact E290 pair reports a
// 53,664-byte largest frame under the historical 53,680-byte ceiling, leaving
// a deliberately pessimistic 4,036-byte margin for that retained artifact.
// Current-source policy gates named cumulative paths against the final linked
// stack instead of treating that artifact-specific single-frame value as a
// product-feature ceiling. This remains an E290 internal-stack measurement;
// PSRAM does not back the executor stack.
const PRIOR_QUALIFIED_RAW_STACK_MARGIN_BYTES: u64 = 72_212;
const PROOF_TRACE_LINKED_STACK_REDUCTION_BYTES: u64 = PROOF_BYTE_SIZE as u64;
const POST_PROOF_LINKED_STACK_REDUCTION_BYTES: u64 = 3_544;
const APPLICATION_EVENT_LINKED_STACK_REDUCTION_BYTES: u64 = 2_408;
const DURABLE_LXMF_DEFAULT_POLICY_REDUCTION_BYTES: u64 = 2_632;
const PRE_STAGE5_CARRIED_RAW_STACK_MARGIN_BYTES: u64 = PRIOR_QUALIFIED_RAW_STACK_MARGIN_BYTES
    - PROOF_TRACE_LINKED_STACK_REDUCTION_BYTES
    - POST_PROOF_LINKED_STACK_REDUCTION_BYTES
    - APPLICATION_EVENT_LINKED_STACK_REDUCTION_BYTES
    - DURABLE_LXMF_DEFAULT_POLICY_REDUCTION_BYTES;
const PRE_BOOTSTRAP_QUALIFIED_RAW_STACK_MARGIN_BYTES: u64 = 57_716;
const BOOTSTRAP_ANNOUNCE_SCHEDULE_LINKED_STACK_REDUCTION_BYTES: u64 = 16;
const QUALIFIED_RAW_STACK_MARGIN_BYTES: u64 = PRE_BOOTSTRAP_QUALIFIED_RAW_STACK_MARGIN_BYTES
    - BOOTSTRAP_ANNOUNCE_SCHEDULE_LINKED_STACK_REDUCTION_BYTES;
const HISTORICAL_QUALIFIED_MAXIMUM_STACK_FRAME_BYTES: u64 = 53_680;
const HISTORICAL_MINIMUM_CONSERVATIVE_STACK_MARGIN_BYTES: u64 =
    QUALIFIED_RAW_STACK_MARGIN_BYTES - HISTORICAL_QUALIFIED_MAXIMUM_STACK_FRAME_BYTES;
// The emitted storage-path frames below start at the async task body and stop
// at the local esp-storage wrapper. Keep explicit room for the small executor
// poll wrapper, the ROM flash implementation, and interrupt entry that the
// selected path or ELF's `.stack_sizes` section cannot describe.
const STORAGE_PATH_STACK_RESERVE_BYTES: u64 = 4_096;
const STARTUP_STACK_COMPONENT_COUNT: usize = 2;
const PRE_USB_MOUNT_STACK_COMPONENT_COUNT: usize = 9;
const LIVE_APPEND_STACK_COMPONENT_COUNT: usize = 9;
const LIVE_COMPACT_STACK_COMPONENT_COUNT: usize = 10;
// The bounded bootstrap announce scheduler adds sixteen linked bytes to both
// profiles. LXTE remains one exact 96-byte initialized internal-RAM object only
// in HIL, preserving the profile-to-profile difference.
const MINIMUM_DEFAULT_USABLE_STACK_BYTES: u64 = 162_376;
const MINIMUM_HIL_USABLE_STACK_BYTES: u64 = 161_576;
const EXPECTED_STACK_GUARD_OFFSET_BYTES: u64 = 60;
const STACK_GUARD_WORD_BYTES: u64 = size_of::<u32>() as u64;

#[derive(Clone, Copy)]
struct StackSymbolSelector {
    output_name: &'static str,
    required_fragments: &'static [&'static str],
    rejected_fragments: &'static [&'static str],
}

const STARTUP_STACK_SELECTORS: [StackSymbolSelector; STARTUP_STACK_COMPONENT_COUNT] = [
    StackSymbolSelector {
        output_name: "product_main_poll",
        required_fragments: &["___product_main_task_inner_function"],
        rejected_fragments: &["UninitCell", "TaskStorage", "HEAP"],
    },
    StackSymbolSelector {
        output_name: "node_core_new",
        required_fragments: &["reticulum_node_core", "NodeCore", "3new"],
        rejected_fragments: &[],
    },
];

const PRE_USB_MOUNT_STACK_SELECTORS: [StackSymbolSelector; PRE_USB_MOUNT_STACK_COMPONENT_COUNT] = [
    // Rust v0 mangling retains these identifier fragments while crate hashes and
    // generic encodings vary. Every selector is required to resolve to one
    // distinct defined text address, so an inlining or topology change fails
    // closed for review instead of silently dropping a frame from the sum.
    StackSymbolSelector {
        output_name: "product_main_poll",
        required_fragments: &["___product_main_task_inner_function"],
        rejected_fragments: &["UninitCell", "TaskStorage", "HEAP"],
    },
    StackSymbolSelector {
        output_name: "mount_node_runtime",
        required_fragments: &["ProductFlashOwner", "mount_node_runtime"],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "submission_runtime_mount_into",
        required_fragments: &[
            "SubmissionRuntime",
            "mount_into",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "storage_actor_mount_into",
        required_fragments: &[
            "StorageActor",
            "mount_into",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "storage_journal_mount_into",
        required_fragments: &[
            "reticulum_storage_journal",
            "10mount_into",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &["scan_bank"],
    },
    StackSymbolSelector {
        output_name: "storage_journal_select_manifest",
        required_fragments: &[
            "reticulum_storage_journal",
            "select_manifest",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "storage_journal_read_manifest",
        required_fragments: &[
            "reticulum_storage_journal",
            "read_manifest",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "partition_nor_flash_read",
        required_fragments: &["PartitionNorFlash", "ReadNorFlash", "4read", "FlashStorage"],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "esp_storage_spiflash_read",
        required_fragments: &["esp_storage", "spiflash_read"],
        rejected_fragments: &[],
    },
];

const LIVE_APPEND_STACK_SELECTORS: [StackSymbolSelector; LIVE_APPEND_STACK_COMPONENT_COUNT] = [
    StackSymbolSelector {
        output_name: "node_task_poll",
        required_fragments: &["node_task", "___run_task_inner_function"],
        rejected_fragments: &["UninitCell", "TaskStorage", "HEAP"],
    },
    StackSymbolSelector {
        output_name: "authenticated_api_submission_accept",
        required_fragments: &["ProductAuthenticatedApiPort", "SubmissionPort", "6accept"],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "storage_actor_drive_pending",
        required_fragments: &[
            "StorageActor",
            "drive_pending",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "storage_actor_drive_append",
        required_fragments: &[
            "StorageActor",
            "drive_append",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "storage_journal_append_with_replay_scratch",
        required_fragments: &[
            "reticulum_storage_journal",
            "append_with_replay_scratch",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "storage_journal_select_manifest",
        required_fragments: &[
            "reticulum_storage_journal",
            "select_manifest",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "storage_journal_read_manifest",
        required_fragments: &[
            "reticulum_storage_journal",
            "read_manifest",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "partition_nor_flash_read",
        required_fragments: &["PartitionNorFlash", "ReadNorFlash", "4read", "FlashStorage"],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "esp_storage_spiflash_read",
        required_fragments: &["esp_storage", "spiflash_read"],
        rejected_fragments: &[],
    },
];

const LIVE_COMPACT_STACK_SELECTORS: [StackSymbolSelector; LIVE_COMPACT_STACK_COMPONENT_COUNT] = [
    LIVE_APPEND_STACK_SELECTORS[0],
    LIVE_APPEND_STACK_SELECTORS[1],
    LIVE_APPEND_STACK_SELECTORS[2],
    LIVE_APPEND_STACK_SELECTORS[3],
    StackSymbolSelector {
        output_name: "storage_journal_compact_with_replay_scratch",
        required_fragments: &[
            "reticulum_storage_journal",
            "compact_with_replay_scratch",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    StackSymbolSelector {
        output_name: "storage_journal_mount_state_with_scratch",
        required_fragments: &[
            "reticulum_storage_journal",
            "mount_state_with_scratch",
            "BoundJournal",
            "PartitionNorFlash",
            "FlashStorage",
        ],
        rejected_fragments: &[],
    },
    LIVE_APPEND_STACK_SELECTORS[5],
    LIVE_APPEND_STACK_SELECTORS[6],
    LIVE_APPEND_STACK_SELECTORS[7],
    LIVE_APPEND_STACK_SELECTORS[8],
];

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
    DecodeLxmfTrace(Options),
    DecodeCheckpoint(Options),
    InspectElf(ElfInspectionOptions),
    InspectStartupElf(StartupElfInspectionOptions),
    CaptureCheckpoint(CaptureCheckpointOptions),
}

#[derive(Debug, Eq, PartialEq)]
struct ElfInspectionOptions {
    default_elf: PathBuf,
    hil_elf: PathBuf,
}

#[derive(Debug, Eq, PartialEq)]
struct StartupElfInspectionOptions {
    elf: PathBuf,
}

#[derive(Debug, Eq, PartialEq)]
struct CaptureCheckpointOptions {
    hil_elf: PathBuf,
    usb_serial: String,
    output: PathBuf,
    probe_rs: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EvidenceSymbol {
    address: u64,
    size_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CheckpointLayout {
    runtime: EvidenceSymbol,
    proof_trace: EvidenceSymbol,
    lxmf_trace: EvidenceSymbol,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedCheckpointCapture {
    elf_path: PathBuf,
    elf_bytes: u64,
    elf_sha256: String,
    layout: CheckpointLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StackSizeInventory {
    record_count: u64,
    maximum_frame_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StackSizeRecord {
    function_address: u64,
    frame_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedStackSizes {
    inventory: StackSizeInventory,
    records: Vec<StackSizeRecord>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreUsbMountStack {
    frame_bytes: [u64; PRE_USB_MOUNT_STACK_COMPONENT_COUNT],
    cumulative_frame_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiveMutationStack<const COMPONENTS: usize> {
    frame_bytes: [u64; COMPONENTS],
    cumulative_frame_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StackLayout {
    reserved_bytes: u64,
    usable_bytes: u64,
    guard_offset_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DefinedSupervisorStatic {
    name: String,
    address: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StartupElfInspection {
    stack_sizes: StackSizeInventory,
    startup_stack: LiveMutationStack<STARTUP_STACK_COMPONENT_COUNT>,
    stack: StackLayout,
    supervisor_statics: Vec<DefinedSupervisorStatic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ElfInspection {
    default_stack_sizes: StackSizeInventory,
    default_pre_usb_mount_stack: PreUsbMountStack,
    default_live_append_stack: LiveMutationStack<LIVE_APPEND_STACK_COMPONENT_COUNT>,
    default_live_compact_stack: LiveMutationStack<LIVE_COMPACT_STACK_COMPONENT_COUNT>,
    default_stack: StackLayout,
    default_proof_trace_symbol_count: u64,
    default_lxmf_trace_symbol_count: u64,
    hil_stack_sizes: StackSizeInventory,
    hil_pre_usb_mount_stack: PreUsbMountStack,
    hil_live_append_stack: LiveMutationStack<LIVE_APPEND_STACK_COMPONENT_COUNT>,
    hil_live_compact_stack: LiveMutationStack<LIVE_COMPACT_STACK_COMPONENT_COUNT>,
    hil_stack: StackLayout,
    hil_proof_trace_symbol_count: u64,
    hil_proof_trace_symbol_size_bytes: u64,
    hil_lxmf_trace_symbol_count: u64,
    hil_lxmf_trace_symbol_size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedEvidence {
    words: [u32; WORD_COUNT],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedProofTraceEvidence {
    words: [u32; PROOF_WORD_COUNT],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedLxmfTraceEvidence {
    words: [u32; LXMF_WORD_COUNT],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TxPartitionDiagnostic {
    expected_count: u32,
    observed_count: u32,
    consistent: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedCheckpoint {
    runtime: DecodedEvidence,
    proof_trace: DecodedProofTraceEvidence,
    lxmf_trace: DecodedLxmfTraceEvidence,
    tx_partition: TxPartitionDiagnostic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProbeInvocation {
    program: PathBuf,
    arguments: Vec<OsString>,
    current_directory: PathBuf,
    home_directory: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProbeExit {
    success: bool,
    description: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CaptureFileBinding {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CaptureElfBinding {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CaptureSymbolBinding {
    address: String,
    size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CaptureLayoutBinding {
    runtime: CaptureSymbolBinding,
    proof_trace: CaptureSymbolBinding,
    lxmf_trace: CaptureSymbolBinding,
    contiguous_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CaptureProbeBinding {
    requested_program: String,
    executable: CaptureElfBinding,
    arguments: Vec<String>,
    launch: CaptureProbeLaunchBinding,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CaptureProbeLaunchBinding {
    environment_policy: String,
    environment_allowlist: Vec<String>,
    current_directory: String,
    home_directory: String,
    empty_config: CaptureFileBinding,
    executable_parent_config_policy: String,
    rejected_config_names: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedProbeLaunch {
    executable_path: PathBuf,
    executable_bytes: u64,
    executable_sha256: String,
    current_directory: PathBuf,
    home_directory: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CaptureManifest {
    schema: String,
    usb_serial: String,
    hil_elf: CaptureElfBinding,
    layout: CaptureLayoutBinding,
    probe: CaptureProbeBinding,
    files: Vec<CaptureFileBinding>,
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
        CommandOptions::DecodeLxmfTrace(options) => execute_lxmf_trace(&options),
        CommandOptions::DecodeCheckpoint(options) => execute_checkpoint(&options),
        CommandOptions::InspectElf(options) => {
            inspect_elf_pair(&options).map(|value| value.render())
        }
        CommandOptions::InspectStartupElf(options) => {
            inspect_startup_elf(&options).map(|value| value.render())
        }
        CommandOptions::CaptureCheckpoint(options) => execute_capture_checkpoint(&options),
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
         e290-runtime-measurement decode-lxmf-trace \
         --input <96-byte-bin> [--json]\n  cargo run -p xtask -- \
         e290-runtime-measurement decode-checkpoint \
         --input <544-byte-bin> [--json]\n  cargo run -p xtask -- \
         e290-runtime-measurement inspect-elf --default-elf <path> \
         --hil-elf <path>\n  cargo run -p xtask -- \
         e290-runtime-measurement inspect-startup-elf --elf <final-ELF>\n  cargo run -p xtask -- \
         e290-runtime-measurement capture-checkpoint --hil-elf <final-HIL-ELF> \
         --usb-serial <UPPERCASE-E290-USB-SERIAL> --output <absent-directory> \
         [--probe-rs <program>]\n\nThe capture command performs one debugger read only; it never resets, flashes, authenticates, or opens a serial port."
    );
}

fn parse_command_options(args: &[String]) -> Result<CommandOptions, String> {
    match args.first().map(String::as_str) {
        Some("decode") => parse_options(args).map(CommandOptions::Decode),
        Some("decode-proof-trace") => {
            parse_proof_trace_options(args).map(CommandOptions::DecodeProofTrace)
        }
        Some("decode-lxmf-trace") => {
            parse_lxmf_trace_options(args).map(CommandOptions::DecodeLxmfTrace)
        }
        Some("decode-checkpoint") => {
            parse_checkpoint_options(args).map(CommandOptions::DecodeCheckpoint)
        }
        Some("inspect-elf") => {
            parse_elf_inspection_options(&args[1..]).map(CommandOptions::InspectElf)
        }
        Some("inspect-startup-elf") => {
            parse_startup_elf_inspection_options(&args[1..]).map(CommandOptions::InspectStartupElf)
        }
        Some("capture-checkpoint") => {
            parse_capture_checkpoint_options(&args[1..]).map(CommandOptions::CaptureCheckpoint)
        }
        Some(value) => Err(format!("unknown subcommand {value}")),
        None => Err("e290-runtime-measurement subcommand is required".to_owned()),
    }
}

fn parse_checkpoint_options(args: &[String]) -> Result<Options, String> {
    match args.first().map(String::as_str) {
        Some("decode-checkpoint") => {}
        Some(_) => return Err("subcommand must be decode-checkpoint".to_owned()),
        None => return Err("decode-checkpoint subcommand is required".to_owned()),
    }
    parse_decode_input_options(&args[1..])
}

fn parse_proof_trace_options(args: &[String]) -> Result<Options, String> {
    match args.first().map(String::as_str) {
        Some("decode-proof-trace") => {}
        Some(_) => return Err("subcommand must be decode-proof-trace".to_owned()),
        None => return Err("decode-proof-trace subcommand is required".to_owned()),
    }

    parse_decode_input_options(&args[1..])
}

fn parse_lxmf_trace_options(args: &[String]) -> Result<Options, String> {
    match args.first().map(String::as_str) {
        Some("decode-lxmf-trace") => {}
        Some(_) => return Err("subcommand must be decode-lxmf-trace".to_owned()),
        None => return Err("decode-lxmf-trace subcommand is required".to_owned()),
    }

    parse_decode_input_options(&args[1..])
}

fn parse_decode_input_options(args: &[String]) -> Result<Options, String> {
    let mut input = None;
    let mut json = false;
    let mut index = 0;
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

    parse_decode_input_options(&args[1..])
}

fn parse_capture_checkpoint_options(args: &[String]) -> Result<CaptureCheckpointOptions, String> {
    let mut hil_elf = None;
    let mut usb_serial = None;
    let mut output = None;
    let mut probe_rs = None;
    let mut index = 0;
    while index < args.len() {
        let (slot, option) = match args[index].as_str() {
            "--hil-elf" => (&mut hil_elf, "--hil-elf"),
            "--usb-serial" => (&mut usb_serial, "--usb-serial"),
            "--output" => (&mut output, "--output"),
            "--probe-rs" => (&mut probe_rs, "--probe-rs"),
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
        *slot = Some(value.clone());
        index += 2;
    }

    let hil_elf = hil_elf.ok_or_else(|| "--hil-elf is required".to_owned())?;
    let usb_serial = usb_serial.ok_or_else(|| "--usb-serial is required".to_owned())?;
    let output = output.ok_or_else(|| "--output is required".to_owned())?;
    validate_e290_usb_serial(&usb_serial)?;
    Ok(CaptureCheckpointOptions {
        hil_elf: PathBuf::from(hil_elf),
        usb_serial,
        output: PathBuf::from(output),
        probe_rs: PathBuf::from(probe_rs.unwrap_or_else(|| DEFAULT_PROBE_RS.to_owned())),
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

fn parse_startup_elf_inspection_options(
    args: &[String],
) -> Result<StartupElfInspectionOptions, String> {
    let mut elf = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--elf" => {
                if elf.is_some() {
                    return Err("--elf may be supplied only once".to_owned());
                }
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| "--elf requires a value".to_owned())?;
                if value.is_empty() {
                    return Err("--elf must not be empty".to_owned());
                }
                elf = Some(PathBuf::from(value));
                index += 2;
            }
            value if value.starts_with('-') => return Err(format!("unknown option {value}")),
            value => return Err(format!("unexpected argument {value}")),
        }
    }
    Ok(StartupElfInspectionOptions {
        elf: elf.ok_or_else(|| "--elf is required".to_owned())?,
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

fn execute_lxmf_trace(options: &Options) -> Result<String, String> {
    let bytes = fs::read(&options.input).map_err(|error| {
        format!(
            "could not read --input {}: {error}",
            options.input.display()
        )
    })?;
    let evidence = DecodedLxmfTraceEvidence::parse(&bytes)?;
    Ok(if options.json {
        evidence.render_json()
    } else {
        evidence.render_human()
    })
}

fn execute_checkpoint(options: &Options) -> Result<String, String> {
    let bytes = fs::read(&options.input).map_err(|error| {
        format!(
            "could not read --input {}: {error}",
            options.input.display()
        )
    })?;
    let checkpoint = DecodedCheckpoint::parse(&bytes)?;
    Ok(if options.json {
        checkpoint.render_json()
    } else {
        checkpoint.render_human()
    })
}

fn execute_capture_checkpoint(options: &CaptureCheckpointOptions) -> Result<String, String> {
    validate_e290_usb_serial(&options.usb_serial)?;
    let prepared = inspect_checkpoint_capture_elf(&options.hil_elf)?;
    capture_checkpoint_with(options, &prepared, run_probe_command)
}

fn run_probe_command(invocation: &ProbeInvocation) -> Result<ProbeExit, String> {
    validate_probe_launch_isolation(invocation)?;
    let mut command = Command::new(&invocation.program);
    apply_sanitized_probe_launch(&mut command, invocation);
    let status = command.status().map_err(|error| {
        format!(
            "could not invoke probe-rs program {}: {error}; {PROBE_FAILURE_GUIDANCE}",
            invocation.program.display(),
        )
    })?;
    validate_probe_launch_isolation(invocation)
        .map_err(|error| format!("{error}; {PROBE_FAILURE_GUIDANCE}"))?;
    Ok(ProbeExit {
        success: status.success(),
        description: status.to_string(),
    })
}

fn apply_sanitized_probe_launch(command: &mut Command, invocation: &ProbeInvocation) {
    command
        .args(&invocation.arguments)
        .env_clear()
        .env("HOME", &invocation.home_directory)
        .current_dir(&invocation.current_directory);
}

fn validate_probe_launch_isolation(invocation: &ProbeInvocation) -> Result<(), String> {
    validate_exact_directory_entries(
        &invocation.current_directory,
        &[".probe-rs.toml"],
        "isolated probe working directory",
    )?;
    validate_exact_directory_entries(
        &invocation.home_directory,
        &[],
        "isolated probe HOME directory",
    )?;
    let config = invocation.current_directory.join(".probe-rs.toml");
    let metadata = fs::symlink_metadata(&config).map_err(|error| {
        format!(
            "could not inspect isolated probe-rs configuration {}: {error}",
            config.display()
        )
    })?;
    if !metadata.file_type().is_file() || metadata.len() != 0 {
        return Err(format!(
            "isolated probe-rs configuration must remain one empty regular file: {}",
            config.display()
        ));
    }
    for name in &PROBE_CONFIG_NAMES[1..] {
        ensure_absent(
            &invocation.current_directory.join(name),
            "alternate isolated probe-rs configuration",
        )?;
    }
    reject_probe_configs(&invocation.home_directory, "isolated probe HOME")?;
    let parent = invocation.program.parent().ok_or_else(|| {
        format!(
            "resolved probe-rs executable has no parent: {}",
            invocation.program.display()
        )
    })?;
    reject_probe_configs(parent, "resolved probe-rs executable parent")
}

fn validate_exact_directory_entries(
    directory: &Path,
    expected: &[&str],
    description: &str,
) -> Result<(), String> {
    let mut actual = fs::read_dir(directory)
        .map_err(|error| {
            format!(
                "could not list {description} {}: {error}",
                directory.display()
            )
        })?
        .map(|entry| {
            let entry = entry.map_err(|error| {
                format!(
                    "could not inspect entry in {description} {}: {error}",
                    directory.display()
                )
            })?;
            entry
                .file_name()
                .into_string()
                .map_err(|_| format!("{description} contains a non-UTF-8 entry"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    actual.sort();
    let mut expected = expected
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    expected.sort();
    if actual != expected {
        return Err(format!(
            "{description} must have exact entries {expected:?}, got {actual:?}: {}",
            directory.display()
        ));
    }
    Ok(())
}

fn capture_checkpoint_with(
    options: &CaptureCheckpointOptions,
    prepared: &PreparedCheckpointCapture,
    run_probe: impl FnOnce(&ProbeInvocation) -> Result<ProbeExit, String>,
) -> Result<String, String> {
    validate_e290_usb_serial(&options.usb_serial)?;
    prepared.layout.validate()?;
    let output = create_capture_output(&options.output)?;
    utf8_path(&output, "checkpoint output")?;
    utf8_path(&prepared.elf_path, "canonical HIL ELF")?;
    let probe_launch = prepare_probe_launch(&output, &options.probe_rs)?;
    let raw_path = output.join(CAPTURE_RAW_FILE);
    ensure_absent(&raw_path, "raw checkpoint output")?;

    let probe_selector = format!("{E290_PROBE_VID_PID}:{}", options.usb_serial);
    let capture_address = format!("0x{:x}", prepared.layout.lxmf_trace.address);
    let arguments = [
        OsString::from("read"),
        OsString::from("--chip"),
        OsString::from("esp32s3"),
        OsString::from("--protocol"),
        OsString::from("jtag"),
        OsString::from("--probe"),
        OsString::from(&probe_selector),
        OsString::from("--non-interactive"),
        OsString::from("--format"),
        OsString::from("binary"),
        OsString::from("--output"),
        raw_path.as_os_str().to_owned(),
        OsString::from("b8"),
        OsString::from(&capture_address),
        OsString::from(CHECKPOINT_BYTE_SIZE.to_string()),
    ]
    .to_vec();
    let invocation = ProbeInvocation {
        program: probe_launch.executable_path.clone(),
        arguments,
        current_directory: probe_launch.current_directory.clone(),
        home_directory: probe_launch.home_directory.clone(),
    };
    let probe_exit = match run_probe(&invocation) {
        Ok(status) => status,
        Err(error) => {
            retain_external_output_after_probe_failure(&raw_path)
                .map_err(|retention_error| format!("{error}; {retention_error}"))?;
            return Err(error);
        }
    };
    if !probe_exit.success {
        let error = format!(
            "probe-rs checkpoint read failed: {}; {PROBE_FAILURE_GUIDANCE}",
            probe_exit.description,
        );
        retain_external_output_after_probe_failure(&raw_path)
            .map_err(|retention_error| format!("{error}; {retention_error}"))?;
        return Err(error);
    }
    sync_external_output_if_regular(&raw_path)?;

    let raw = fs::read(&raw_path)
        .map_err(|error| format!("could not read {}: {error}", raw_path.display()))?;
    if raw.len() != CHECKPOINT_BYTE_SIZE {
        return Err(format!(
            "probe-rs checkpoint output must be exactly {CHECKPOINT_BYTE_SIZE} bytes, got {}",
            raw.len()
        ));
    }
    let checkpoint = DecodedCheckpoint::parse(&raw)?;
    let lxmf_trace = &raw[..LXMF_BYTE_SIZE];
    let runtime = &raw[LXMF_BYTE_SIZE..LXMF_BYTE_SIZE + BYTE_SIZE];
    let proof_trace = &raw[LXMF_BYTE_SIZE + BYTE_SIZE..];
    let human = format!("{}\n", checkpoint.render_human());
    let json = format!("{}\n", checkpoint.render_json());

    write_new_synced(&output.join(CAPTURE_RUNTIME_FILE), runtime)?;
    write_new_synced(&output.join(CAPTURE_PROOF_FILE), proof_trace)?;
    write_new_synced(&output.join(CAPTURE_LXMF_FILE), lxmf_trace)?;
    write_new_synced(&output.join(CAPTURE_HUMAN_FILE), human.as_bytes())?;
    write_new_synced(&output.join(CAPTURE_JSON_FILE), json.as_bytes())?;

    let files = [
        CAPTURE_RAW_FILE,
        CAPTURE_RUNTIME_FILE,
        CAPTURE_PROOF_FILE,
        CAPTURE_LXMF_FILE,
        CAPTURE_HUMAN_FILE,
        CAPTURE_JSON_FILE,
        PROBE_CONFIG_FILE,
    ]
    .into_iter()
    .map(|path| capture_file_binding(&output, path))
    .collect::<Result<Vec<_>, _>>()?;
    let requested_probe_program = utf8_path(&options.probe_rs, "--probe-rs")?.to_owned();
    let probe_arguments = invocation
        .arguments
        .iter()
        .map(|argument| {
            argument
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "probe-rs argument is not UTF-8".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let manifest = CaptureManifest {
        schema: CAPTURE_SCHEMA.to_owned(),
        usb_serial: options.usb_serial.clone(),
        hil_elf: CaptureElfBinding {
            path: utf8_path(&prepared.elf_path, "canonical HIL ELF")?.to_owned(),
            bytes: prepared.elf_bytes,
            sha256: prepared.elf_sha256.clone(),
        },
        layout: CaptureLayoutBinding {
            runtime: capture_symbol_binding(prepared.layout.runtime),
            proof_trace: capture_symbol_binding(prepared.layout.proof_trace),
            lxmf_trace: capture_symbol_binding(prepared.layout.lxmf_trace),
            contiguous_bytes: CHECKPOINT_BYTE_SIZE as u64,
        },
        probe: CaptureProbeBinding {
            requested_program: requested_probe_program,
            executable: CaptureElfBinding {
                path: utf8_path(
                    &probe_launch.executable_path,
                    "resolved probe-rs executable",
                )?
                .to_owned(),
                bytes: probe_launch.executable_bytes,
                sha256: probe_launch.executable_sha256,
            },
            arguments: probe_arguments,
            launch: CaptureProbeLaunchBinding {
                environment_policy: "clear-then-set-allowlist".to_owned(),
                environment_allowlist: vec!["HOME".to_owned()],
                current_directory: utf8_path(
                    &probe_launch.current_directory,
                    "isolated probe-rs working directory",
                )?
                .to_owned(),
                home_directory: utf8_path(
                    &probe_launch.home_directory,
                    "isolated probe-rs HOME directory",
                )?
                .to_owned(),
                empty_config: capture_file_binding(&output, PROBE_CONFIG_FILE)?,
                executable_parent_config_policy: "reject-any-known-default-config".to_owned(),
                rejected_config_names: PROBE_CONFIG_NAMES.map(str::to_owned).to_vec(),
            },
        },
        files,
    };
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("could not encode checkpoint manifest: {error}"))?;
    manifest_bytes.push(b'\n');
    write_new_synced(&output.join(CAPTURE_MANIFEST_FILE), &manifest_bytes)?;
    sync_directory(&output)?;

    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    let complete =
        format!("{CAPTURE_SCHEMA}\nstatus=complete\nmanifest_sha256={manifest_sha256}\n");
    stage_and_commit_capture_marker(&output, complete.as_bytes())?;
    Ok(format!("captured_checkpoint={}", output.display()))
}

fn capture_symbol_binding(symbol: EvidenceSymbol) -> CaptureSymbolBinding {
    CaptureSymbolBinding {
        address: format!("0x{:08x}", symbol.address),
        size_bytes: symbol.size_bytes,
    }
}

fn validate_e290_usb_serial(serial: &str) -> Result<(), String> {
    let bytes = serial.as_bytes();
    let valid = bytes.len() == 17
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 2 | 5 | 8 | 11 | 14) {
                *byte == b':'
            } else {
                byte.is_ascii_digit() || (b'A'..=b'F').contains(byte)
            }
        });
    if valid {
        Ok(())
    } else {
        Err(
            "--usb-serial must be exactly six uppercase hexadecimal octets separated by colons"
                .to_owned(),
        )
    }
}

#[cfg(unix)]
fn prepare_probe_launch(output: &Path, requested: &Path) -> Result<PreparedProbeLaunch, String> {
    let launch_directory = output.join(PROBE_LAUNCH_DIRECTORY);
    let current_directory = output.join(PROBE_CWD_DIRECTORY);
    let home_directory = output.join(PROBE_HOME_DIRECTORY);
    create_private_directory(&launch_directory, "probe launch")?;
    create_private_directory(&current_directory, "probe working")?;
    create_private_directory(&home_directory, "probe HOME")?;
    write_new_synced(&output.join(PROBE_CONFIG_FILE), EMPTY_PROBE_CONFIG)?;
    utf8_path(&current_directory, "isolated probe-rs working directory")?;
    utf8_path(&home_directory, "isolated probe-rs HOME directory")?;
    for name in &PROBE_CONFIG_NAMES[1..] {
        ensure_absent(
            &current_directory.join(name),
            "alternate isolated probe-rs configuration",
        )?;
    }
    reject_probe_configs(&home_directory, "isolated probe HOME")?;
    sync_directory(&current_directory)?;
    sync_directory(&home_directory)?;
    sync_directory(&launch_directory)?;
    sync_directory(output)?;

    let executable_path = resolve_probe_executable(requested)?;
    let parent = executable_path.parent().ok_or_else(|| {
        format!(
            "resolved probe-rs executable has no parent: {}",
            executable_path.display()
        )
    })?;
    reject_probe_configs(parent, "resolved probe-rs executable parent")?;
    let metadata = fs::symlink_metadata(&executable_path).map_err(|error| {
        format!(
            "could not inspect resolved probe-rs executable {}: {error}",
            executable_path.display()
        )
    })?;
    Ok(PreparedProbeLaunch {
        executable_path: executable_path.clone(),
        executable_bytes: metadata.len(),
        executable_sha256: sha256_file(&executable_path)?,
        current_directory,
        home_directory,
    })
}

#[cfg(not(unix))]
fn prepare_probe_launch(_output: &Path, _requested: &Path) -> Result<PreparedProbeLaunch, String> {
    Err("capture-checkpoint requires a Unix host to isolate probe-rs configuration".to_owned())
}

#[cfg(unix)]
fn create_private_directory(path: &Path, description: &str) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path).map_err(|error| {
        format!(
            "could not create {description} directory {}: {error}",
            path.display()
        )
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "could not set owner-only permissions on {description} directory {}: {error}",
            path.display()
        )
    })
}

#[cfg(unix)]
fn resolve_probe_executable(requested: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;

    let candidate = if requested.components().count() == 1
        && matches!(requested.components().next(), Some(Component::Normal(_)))
    {
        let path = env::var_os("PATH").ok_or_else(|| {
            format!(
                "cannot resolve probe-rs program {} because PATH is unset",
                requested.display()
            )
        })?;
        env::split_paths(&path)
            .map(|directory| directory.join(requested))
            .find(|candidate| {
                fs::symlink_metadata(candidate).is_ok_and(|metadata| {
                    metadata.file_type().is_file() || metadata.file_type().is_symlink()
                })
            })
            .ok_or_else(|| {
                format!(
                    "could not find probe-rs program {} on PATH",
                    requested.display()
                )
            })?
    } else if requested.is_absolute() {
        requested.to_owned()
    } else {
        env::current_dir()
            .map_err(|error| format!("could not resolve current directory: {error}"))?
            .join(requested)
    };
    let canonical = fs::canonicalize(&candidate).map_err(|error| {
        format!(
            "could not canonicalize probe-rs program {}: {error}",
            candidate.display()
        )
    })?;
    let metadata = fs::symlink_metadata(&canonical).map_err(|error| {
        format!(
            "could not inspect probe-rs program {}: {error}",
            canonical.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "probe-rs program must resolve to a regular file: {}",
            canonical.display()
        ));
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!(
            "probe-rs program is not executable: {}",
            canonical.display()
        ));
    }
    utf8_path(&canonical, "resolved probe-rs executable")?;
    Ok(canonical)
}

fn reject_probe_configs(directory: &Path, description: &str) -> Result<(), String> {
    for name in PROBE_CONFIG_NAMES {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect {description} configuration candidate {}: {error}",
                    path.display()
                ));
            }
            Ok(_) => {
                return Err(format!(
                    "refusing probe-rs launch because {description} contains default configuration {name}: {}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_capture_output(argument: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let output = resolve_absent_capture_output(argument)?;
    let parent = output.parent().ok_or_else(|| {
        format!(
            "checkpoint output has no parent directory: {}",
            output.display()
        )
    })?;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(&output).map_err(|error| {
        format!(
            "could not create checkpoint output directory {}: {error}",
            output.display()
        )
    })?;
    fs::set_permissions(&output, fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "could not set owner-only permissions on {}: {error}",
            output.display()
        )
    })?;
    write_new_synced(
        &output.join(CAPTURE_INCOMPLETE_FILE),
        CAPTURE_INCOMPLETE_CONTENT.as_bytes(),
    )?;
    sync_directory(&output)?;
    sync_directory(parent)?;
    Ok(output)
}

#[cfg(not(unix))]
fn create_capture_output(_argument: &Path) -> Result<PathBuf, String> {
    Err(
        "capture-checkpoint requires a Unix host to enforce owner-only evidence permissions"
            .to_owned(),
    )
}

fn resolve_absent_capture_output(argument: &Path) -> Result<PathBuf, String> {
    if argument.as_os_str().is_empty() {
        return Err("--output must not be empty".to_owned());
    }
    let absolute = if argument.is_absolute() {
        argument.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("could not resolve current directory: {error}"))?
            .join(argument)
    };
    for component in absolute.components() {
        if !matches!(
            component,
            Component::RootDir | Component::Prefix(_) | Component::Normal(_)
        ) {
            return Err(format!(
                "--output must not contain '.' or '..' components: {}",
                argument.display()
            ));
        }
    }
    let file_name = absolute.file_name().ok_or_else(|| {
        format!(
            "--output must name an absent directory: {}",
            argument.display()
        )
    })?;
    let parent = absolute
        .parent()
        .ok_or_else(|| format!("--output has no parent directory: {}", argument.display()))?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| {
        format!(
            "could not canonicalize --output parent {}: {error}",
            parent.display()
        )
    })?;
    let output = canonical_parent.join(file_name);
    ensure_absent(&output, "checkpoint output directory")?;
    Ok(output)
}

fn ensure_absent(path: &Path, description: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not inspect {description} {}: {error}",
            path.display()
        )),
        Ok(_) => Err(format!("{description} must be absent: {}", path.display())),
    }
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                format!(
                    "could not set owner-only permissions on {}: {error}",
                    path.display()
                )
            })?;
    }
    file.write_all(bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync {}: {error}", path.display()))
}

fn sync_external_output_if_regular(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "could not inspect probe-rs output {}: {error}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "probe-rs output must be a regular file: {}",
            path.display()
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("could not open probe-rs output {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                format!(
                    "could not set owner-only permissions on probe-rs output {}: {error}",
                    path.display()
                )
            })?;
    }
    file.sync_all()
        .map_err(|error| format!("could not sync probe-rs output {}: {error}", path.display()))?;
    let parent = path.parent().ok_or_else(|| {
        format!(
            "probe-rs output has no parent directory: {}",
            path.display()
        )
    })?;
    sync_directory(parent)
}

fn retain_external_output_after_probe_failure(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not inspect incomplete probe-rs output {}: {error}",
            path.display()
        )),
        Ok(_) => sync_external_output_if_regular(path).map_err(|error| {
            format!(
                "could not durably retain incomplete probe-rs output {}: {error}",
                path.display()
            )
        }),
    }
}

fn capture_file_binding(output: &Path, relative: &str) -> Result<CaptureFileBinding, String> {
    let path = output.join(relative);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("could not inspect capture file {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "capture path must be a regular file: {}",
            path.display()
        ));
    }
    Ok(CaptureFileBinding {
        path: relative.to_owned(),
        bytes: metadata.len(),
        sha256: sha256_file(&path)?,
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read {} for SHA-256: {error}", path.display()))?;
    Ok(sha256_bytes(&bytes))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn utf8_path<'a>(path: &'a Path, description: &str) -> Result<&'a str, String> {
    path.to_str()
        .ok_or_else(|| format!("{description} path is not UTF-8: {}", path.display()))
}

fn stage_and_commit_capture_marker(output: &Path, complete: &[u8]) -> Result<(), String> {
    let incomplete = output.join(CAPTURE_INCOMPLETE_FILE);
    let completed = output.join(CAPTURE_COMPLETE_FILE);
    ensure_absent(&completed, "completed checkpoint marker")?;
    overwrite_regular_synced(&incomplete, complete)?;
    sync_directory(output)?;
    fs::rename(&incomplete, &completed).map_err(|error| {
        format!(
            "could not atomically complete checkpoint marker {} from {}: {error}",
            completed.display(),
            incomplete.display()
        )
    })?;
    sync_directory(output)
}

fn overwrite_regular_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "refusing to overwrite non-regular file {}",
            path.display()
        ));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| format!("could not open {} for overwrite: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync {}: {error}", path.display()))
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "could not durably sync directory {}: {error}",
                path.display()
            )
        })
}

fn inspect_elf_pair(options: &ElfInspectionOptions) -> Result<ElfInspection, String> {
    let (
        default_stack_sizes,
        default_pre_usb_mount_stack,
        default_live_append_stack,
        default_live_compact_stack,
        default_stack,
    ) = inspect_elf(&options.default_elf, "default E290")?;
    let (
        hil_stack_sizes,
        hil_pre_usb_mount_stack,
        hil_live_append_stack,
        hil_live_compact_stack,
        hil_stack,
    ) = inspect_elf(&options.hil_elf, "runtime-measurement HIL")?;
    let (default_proof_trace_symbol_count, _) =
        inspect_proof_trace_symbol(&options.default_elf, "default E290", false)?;
    let (default_lxmf_trace_symbol_count, _) =
        inspect_lxmf_trace_symbol(&options.default_elf, "default E290", false)?;
    let (hil_proof_trace_symbol_count, hil_proof_trace_symbol_size_bytes) =
        inspect_proof_trace_symbol(&options.hil_elf, "runtime-measurement HIL", true)?;
    let (hil_lxmf_trace_symbol_count, hil_lxmf_trace_symbol_size_bytes) =
        inspect_lxmf_trace_symbol(&options.hil_elf, "runtime-measurement HIL", true)?;
    let inspection = ElfInspection {
        default_stack_sizes,
        default_pre_usb_mount_stack,
        default_live_append_stack,
        default_live_compact_stack,
        default_stack,
        default_proof_trace_symbol_count,
        default_lxmf_trace_symbol_count,
        hil_stack_sizes,
        hil_pre_usb_mount_stack,
        hil_live_append_stack,
        hil_live_compact_stack,
        hil_stack,
        hil_proof_trace_symbol_count,
        hil_proof_trace_symbol_size_bytes,
        hil_lxmf_trace_symbol_count,
        hil_lxmf_trace_symbol_size_bytes,
    };
    inspection.validate()?;
    Ok(inspection)
}

fn inspect_startup_elf(
    options: &StartupElfInspectionOptions,
) -> Result<StartupElfInspection, String> {
    const LABEL: &str = "startup E290";
    let bytes = fs::read(&options.elf).map_err(|error| {
        format!(
            "could not read {LABEL} ELF {}: {error}",
            options.elf.display()
        )
    })?;
    let object = parse_xtensa_elf(&bytes, &options.elf, LABEL)?;
    let stack_sizes = stack_size_records(&object, &options.elf, LABEL)?;
    let startup_stack = inspect_live_mutation_stack(
        &object,
        &stack_sizes.records,
        &options.elf,
        LABEL,
        "startup",
        &STARTUP_STACK_SELECTORS,
    )?;
    let stack_end = unique_symbol_address(&object, &options.elf, LABEL, "_stack_end_cpu0")?;
    let stack_guard = unique_symbol_address(&object, &options.elf, LABEL, "__stack_chk_guard")?;
    let stack_start = unique_symbol_address(&object, &options.elf, LABEL, "_stack_start_cpu0")?;
    let inspection = StartupElfInspection {
        stack_sizes: stack_sizes.inventory,
        startup_stack,
        stack: calculate_stack_layout(LABEL, stack_end, stack_guard, stack_start)?,
        supervisor_statics: defined_internal_supervisor_statics(&object, &options.elf, LABEL)?,
    };
    inspection.validate()?;
    Ok(inspection)
}

fn defined_internal_supervisor_statics(
    object: &object::File<'_>,
    path: &Path,
    label: &str,
) -> Result<Vec<DefinedSupervisorStatic>, String> {
    let mut statics = Vec::new();
    for symbol in object.symbols() {
        if symbol.kind() != SymbolKind::Data || symbol.section() == SymbolSection::Undefined {
            continue;
        }
        let Ok(name) = symbol.name() else {
            continue;
        };
        if !name.ends_with("SUPERVISOR") {
            continue;
        }
        let SymbolSection::Section(index) = symbol.section() else {
            continue;
        };
        let section = object.section_by_index(index).map_err(|error| {
            format!(
                "could not resolve {label} ELF {} section for supervisor static {name}: {error}",
                path.display()
            )
        })?;
        if !matches!(
            section.kind(),
            SectionKind::Data
                | SectionKind::UninitializedData
                | SectionKind::Tls
                | SectionKind::UninitializedTls
                | SectionKind::Common
        ) {
            continue;
        }
        statics.push(DefinedSupervisorStatic {
            name: name.to_owned(),
            address: symbol.address(),
        });
    }
    statics.sort_unstable_by(|left, right| {
        left.address
            .cmp(&right.address)
            .then_with(|| left.name.cmp(&right.name))
    });
    statics.dedup();
    Ok(statics)
}

fn inspect_checkpoint_capture_elf(path: &Path) -> Result<PreparedCheckpointCapture, String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("could not canonicalize HIL ELF {}: {error}", path.display()))?;
    let metadata = fs::symlink_metadata(&canonical).map_err(|error| {
        format!(
            "could not inspect canonical HIL ELF {}: {error}",
            canonical.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "HIL ELF must resolve to a regular file: {}",
            canonical.display()
        ));
    }
    let bytes = fs::read(&canonical)
        .map_err(|error| format!("could not read HIL ELF {}: {error}", canonical.display()))?;
    let object = parse_xtensa_elf(&bytes, &canonical, "runtime-measurement HIL")?;
    let runtime = inspect_checkpoint_symbol(
        &object,
        &canonical,
        MEASUREMENT_EVIDENCE_SYMBOL_FRAGMENT,
        "runtime-measurement",
        BYTE_SIZE,
        |initializer| DecodedEvidence::parse(initializer).map(|_| ()),
    )?;
    let proof_trace = inspect_checkpoint_symbol(
        &object,
        &canonical,
        PROOF_TRACE_EVIDENCE_SYMBOL_FRAGMENT,
        "proof-trace",
        PROOF_BYTE_SIZE,
        |initializer| {
            let evidence = DecodedProofTraceEvidence::parse(initializer)?;
            evidence.validate_empty_initializer()
        },
    )?;
    let lxmf_trace = inspect_checkpoint_symbol(
        &object,
        &canonical,
        LXMF_TRACE_EVIDENCE_SYMBOL_FRAGMENT,
        "durable-LXMF trace",
        LXMF_BYTE_SIZE,
        |initializer| {
            let evidence = DecodedLxmfTraceEvidence::parse(initializer)?;
            evidence.validate_empty_initializer()
        },
    )?;
    let layout = CheckpointLayout {
        runtime,
        proof_trace,
        lxmf_trace,
    };
    layout.validate()?;
    Ok(PreparedCheckpointCapture {
        elf_path: canonical,
        elf_bytes: u64::try_from(bytes.len())
            .map_err(|_| "HIL ELF byte length does not fit u64".to_owned())?,
        elf_sha256: sha256_bytes(&bytes),
        layout,
    })
}

fn inspect_checkpoint_symbol(
    object: &object::File<'_>,
    path: &Path,
    symbol_fragment: &str,
    description: &str,
    expected_size: usize,
    validate_initializer: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<EvidenceSymbol, String> {
    let mut symbols = object.symbols().filter(|symbol| {
        symbol
            .name()
            .is_ok_and(|name| name.contains(symbol_fragment))
            && symbol.section() != SymbolSection::Undefined
    });
    let symbol = symbols.next().ok_or_else(|| {
        format!(
            "runtime-measurement HIL ELF {} must contain exactly one defined {symbol_fragment}, found 0",
            path.display()
        )
    })?;
    if symbols.next().is_some() {
        return Err(format!(
            "runtime-measurement HIL ELF {} must contain exactly one defined {symbol_fragment}, found more than one",
            path.display()
        ));
    }
    if symbol.size() != expected_size as u64 {
        return Err(format!(
            "runtime-measurement HIL ELF {} {description} symbol must be exactly {expected_size} bytes, got {}",
            path.display(),
            symbol.size()
        ));
    }
    let section_index = match symbol.section() {
        SymbolSection::Section(index) => index,
        section => {
            return Err(format!(
                "runtime-measurement HIL ELF {} {description} symbol must belong to one initialized data section, got {section:?}",
                path.display()
            ));
        }
    };
    let section = object.section_by_index(section_index).map_err(|error| {
        format!(
            "could not resolve runtime-measurement HIL {description} section in {}: {error}",
            path.display()
        )
    })?;
    if section.kind() == SectionKind::UninitializedData {
        return Err(format!(
            "runtime-measurement HIL ELF {} {description} symbol must be initialized, not BSS",
            path.display()
        ));
    }
    let offset = symbol
        .address()
        .checked_sub(section.address())
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            format!(
                "runtime-measurement HIL ELF {} {description} symbol address is outside its section",
                path.display()
            )
        })?;
    let end = offset
        .checked_add(expected_size)
        .ok_or_else(|| format!("runtime-measurement HIL {description} symbol range overflows"))?;
    let section_data = section.data().map_err(|error| {
        format!(
            "could not read runtime-measurement HIL {description} section in {}: {error}",
            path.display()
        )
    })?;
    let initializer = section_data.get(offset..end).ok_or_else(|| {
        format!(
            "runtime-measurement HIL ELF {} {description} symbol bytes are outside initialized section data",
            path.display()
        )
    })?;
    validate_initializer(initializer).map_err(|error| {
        format!(
            "runtime-measurement HIL ELF {} {description} symbol has invalid initialized ABI bytes: {error}",
            path.display()
        )
    })?;
    Ok(EvidenceSymbol {
        address: symbol.address(),
        size_bytes: symbol.size(),
    })
}

impl CheckpointLayout {
    fn validate(self) -> Result<(), String> {
        if self.runtime.size_bytes != BYTE_SIZE as u64 {
            return Err(format!(
                "RTME symbol must be exactly {BYTE_SIZE} bytes, got {}",
                self.runtime.size_bytes
            ));
        }
        if self.proof_trace.size_bytes != PROOF_BYTE_SIZE as u64 {
            return Err(format!(
                "RPTE symbol must be exactly {PROOF_BYTE_SIZE} bytes, got {}",
                self.proof_trace.size_bytes
            ));
        }
        if self.lxmf_trace.size_bytes != LXMF_BYTE_SIZE as u64 {
            return Err(format!(
                "LXTE symbol must be exactly {LXMF_BYTE_SIZE} bytes, got {}",
                self.lxmf_trace.size_bytes
            ));
        }
        let expected_runtime_address =
            self.lxmf_trace
                .address
                .checked_add(LXMF_BYTE_SIZE as u64)
                .ok_or_else(|| "LXTE symbol address overflows before RTME".to_owned())?;
        if self.runtime.address != expected_runtime_address {
            return Err(format!(
                "RTME symbol must start exactly {LXMF_BYTE_SIZE} bytes after LXTE: LXTE=0x{:x}, expected RTME=0x{expected_runtime_address:x}, got RTME=0x{:x}",
                self.lxmf_trace.address, self.runtime.address
            ));
        }
        let expected_proof_address = self
            .runtime
            .address
            .checked_add(self.runtime.size_bytes)
            .ok_or_else(|| "RTME symbol address overflows before RPTE".to_owned())?;
        if self.proof_trace.address != expected_proof_address {
            return Err(format!(
                "RPTE symbol must start exactly {BYTE_SIZE} bytes after RTME: RTME=0x{:x}, expected RPTE=0x{expected_proof_address:x}, got RPTE=0x{:x}",
                self.runtime.address, self.proof_trace.address
            ));
        }
        let _ = self
            .proof_trace
            .address
            .checked_add(self.proof_trace.size_bytes)
            .ok_or_else(|| "contiguous LXTE/RTME/RPTE capture range overflows".to_owned())?;
        Ok(())
    }
}

fn inspect_elf(
    path: &Path,
    label: &str,
) -> Result<
    (
        StackSizeInventory,
        PreUsbMountStack,
        LiveMutationStack<LIVE_APPEND_STACK_COMPONENT_COUNT>,
        LiveMutationStack<LIVE_COMPACT_STACK_COMPONENT_COUNT>,
        StackLayout,
    ),
    String,
> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read {label} ELF {}: {error}", path.display()))?;
    let object = parse_xtensa_elf(&bytes, path, label)?;
    let stack_sizes = stack_size_records(&object, path, label)?;
    let pre_usb_mount_stack =
        inspect_pre_usb_mount_stack(&object, &stack_sizes.records, path, label)?;
    let live_append_stack = inspect_live_mutation_stack(
        &object,
        &stack_sizes.records,
        path,
        label,
        "live append",
        &LIVE_APPEND_STACK_SELECTORS,
    )?;
    let live_compact_stack = inspect_live_mutation_stack(
        &object,
        &stack_sizes.records,
        path,
        label,
        "live compact",
        &LIVE_COMPACT_STACK_SELECTORS,
    )?;
    let stack_end = unique_symbol_address(&object, path, label, "_stack_end_cpu0")?;
    let stack_guard = unique_symbol_address(&object, path, label, "__stack_chk_guard")?;
    let stack_start = unique_symbol_address(&object, path, label, "_stack_start_cpu0")?;
    let stack = calculate_stack_layout(label, stack_end, stack_guard, stack_start)?;
    Ok((
        stack_sizes.inventory,
        pre_usb_mount_stack,
        live_append_stack,
        live_compact_stack,
        stack,
    ))
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

fn inspect_lxmf_trace_symbol(
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
            .is_ok_and(|name| name.contains(LXMF_TRACE_EVIDENCE_SYMBOL_FRAGMENT))
            && symbol.section() != SymbolSection::Undefined
    });
    let first = symbols.next();
    let count = u64::from(first.is_some()) + symbols.count() as u64;
    if !required {
        if count != 0 {
            return Err(format!(
                "{label} ELF {} must exclude {LXMF_TRACE_EVIDENCE_SYMBOL_FRAGMENT}, found {count} defined symbols",
                path.display()
            ));
        }
        return Ok((0, 0));
    }
    if count != 1 {
        return Err(format!(
            "{label} ELF {} must contain exactly one defined {LXMF_TRACE_EVIDENCE_SYMBOL_FRAGMENT}, found {count}",
            path.display()
        ));
    }

    let symbol = first.expect("one required durable-LXMF trace symbol was counted");
    if symbol.size() != LXMF_BYTE_SIZE as u64 {
        return Err(format!(
            "{label} ELF {} durable-LXMF trace symbol must be exactly {LXMF_BYTE_SIZE} bytes, got {}",
            path.display(),
            symbol.size()
        ));
    }
    let section_index = match symbol.section() {
        SymbolSection::Section(index) => index,
        section => {
            return Err(format!(
                "{label} ELF {} durable-LXMF trace symbol must belong to one initialized data section, got {section:?}",
                path.display()
            ));
        }
    };
    let section = object.section_by_index(section_index).map_err(|error| {
        format!(
            "could not resolve {label} durable-LXMF trace section in {}: {error}",
            path.display()
        )
    })?;
    if section.kind() == SectionKind::UninitializedData {
        return Err(format!(
            "{label} ELF {} durable-LXMF trace symbol must be initialized, not BSS",
            path.display()
        ));
    }
    let offset = symbol
        .address()
        .checked_sub(section.address())
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            format!(
                "{label} ELF {} durable-LXMF trace symbol address is outside its section",
                path.display()
            )
        })?;
    let end = offset.checked_add(LXMF_BYTE_SIZE).ok_or_else(|| {
        format!(
            "{label} ELF {} durable-LXMF trace symbol range overflows",
            path.display()
        )
    })?;
    let section_data = section.data().map_err(|error| {
        format!(
            "could not read initialized {label} durable-LXMF trace section in {}: {error}",
            path.display()
        )
    })?;
    let initialized = section_data.get(offset..end).ok_or_else(|| {
        format!(
            "{label} ELF {} durable-LXMF trace symbol bytes are outside initialized section data",
            path.display()
        )
    })?;
    let evidence = DecodedLxmfTraceEvidence::parse(initialized).map_err(|error| {
        format!(
            "{label} ELF {} durable-LXMF trace symbol has invalid initialized ABI bytes: {error}",
            path.display()
        )
    })?;
    evidence.validate_empty_initializer().map_err(|error| {
        format!(
            "{label} ELF {} durable-LXMF trace symbol is not an empty initialized record: {error}",
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

fn stack_size_records(
    object: &object::File<'_>,
    path: &Path,
    label: &str,
) -> Result<ParsedStackSizes, String> {
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

fn parse_stack_size_records(data: &[u8], address_bytes: usize) -> Result<ParsedStackSizes, String> {
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
    let mut records = Vec::new();
    while offset < data.len() {
        if data.len() - offset < address_bytes {
            return Err("truncated function address".to_owned());
        }
        let address_end = offset + address_bytes;
        let function_address = data[offset..address_end]
            .iter()
            .copied()
            .enumerate()
            .fold(0_u64, |address, (byte, value)| {
                address | (u64::from(value) << (byte * u8::BITS as usize))
            });
        offset = address_end;
        let (frame_bytes, consumed) = decode_uleb128(&data[offset..])?;
        offset += consumed;
        if let Some(existing) = records.iter().find(|record: &&StackSizeRecord| {
            record.function_address == function_address && record.frame_bytes != frame_bytes
        }) {
            return Err(format!(
                "conflicting frame sizes at function address 0x{function_address:x}: {} and {frame_bytes}",
                existing.frame_bytes
            ));
        }
        records.push(StackSizeRecord {
            function_address,
            frame_bytes,
        });
        record_count += 1;
        maximum_frame_bytes = maximum_frame_bytes.max(frame_bytes);
    }

    Ok(ParsedStackSizes {
        inventory: StackSizeInventory {
            record_count,
            maximum_frame_bytes,
        },
        records,
    })
}

impl StackSymbolSelector {
    fn matches(self, name: &str) -> bool {
        self.required_fragments
            .iter()
            .all(|fragment| name.contains(fragment))
            && self
                .rejected_fragments
                .iter()
                .all(|fragment| !name.contains(fragment))
    }
}

impl PreUsbMountStack {
    fn from_frame_bytes(
        frame_bytes: [u64; PRE_USB_MOUNT_STACK_COMPONENT_COUNT],
    ) -> Result<Self, String> {
        let cumulative_frame_bytes =
            frame_bytes
                .iter()
                .copied()
                .try_fold(0_u64, |total, frame_bytes| {
                    total.checked_add(frame_bytes).ok_or_else(|| {
                        "pre-USB mount cumulative compiler-emitted frame size overflows".to_owned()
                    })
                })?;
        Ok(Self {
            frame_bytes,
            cumulative_frame_bytes,
        })
    }

    fn required_stack_bytes(self) -> Result<u64, String> {
        self.cumulative_frame_bytes
            .checked_add(STORAGE_PATH_STACK_RESERVE_BYTES)
            .ok_or_else(|| "pre-USB mount stack requirement overflows after reserve".to_owned())
    }

    fn render_into(self, prefix: &str, usable_stack_bytes: u64, output: &mut String) {
        for (selector, frame_bytes) in PRE_USB_MOUNT_STACK_SELECTORS.iter().zip(self.frame_bytes) {
            writeln!(
                output,
                "{prefix}.pre_usb_mount.{}_frame_bytes={frame_bytes}",
                selector.output_name
            )
            .expect("writing stack inspection to String cannot fail");
        }
        let required_stack_bytes = self
            .required_stack_bytes()
            .expect("validated pre-USB mount requirement cannot overflow");
        let raw_headroom_bytes = usable_stack_bytes
            .checked_sub(self.cumulative_frame_bytes)
            .expect("validated pre-USB mount chain must fit usable stack");
        let policy_headroom_bytes = usable_stack_bytes
            .checked_sub(required_stack_bytes)
            .expect("validated pre-USB mount chain and reserve must fit usable stack");
        writeln!(
            output,
            "{prefix}.pre_usb_mount.cumulative_frame_bytes={}",
            self.cumulative_frame_bytes
        )
        .expect("writing stack inspection to String cannot fail");
        writeln!(
            output,
            "{prefix}.pre_usb_mount.raw_headroom_bytes={raw_headroom_bytes}"
        )
        .expect("writing stack inspection to String cannot fail");
        writeln!(
            output,
            "{prefix}.pre_usb_mount.policy_headroom_bytes={policy_headroom_bytes}"
        )
        .expect("writing stack inspection to String cannot fail");
    }
}

impl<const COMPONENTS: usize> LiveMutationStack<COMPONENTS> {
    fn from_frame_bytes(path_name: &str, frame_bytes: [u64; COMPONENTS]) -> Result<Self, String> {
        let cumulative_frame_bytes =
            frame_bytes
                .iter()
                .copied()
                .try_fold(0_u64, |total, frame_bytes| {
                    total.checked_add(frame_bytes).ok_or_else(|| {
                        format!("{path_name} cumulative compiler-emitted frame size overflows")
                    })
                })?;
        Ok(Self {
            frame_bytes,
            cumulative_frame_bytes,
        })
    }

    fn required_stack_bytes(self, path_name: &str) -> Result<u64, String> {
        self.cumulative_frame_bytes
            .checked_add(STORAGE_PATH_STACK_RESERVE_BYTES)
            .ok_or_else(|| format!("{path_name} stack requirement overflows after reserve"))
    }

    fn render_into(
        self,
        profile_prefix: &str,
        output_prefix: &str,
        selectors: &[StackSymbolSelector; COMPONENTS],
        usable_stack_bytes: u64,
        output: &mut String,
    ) {
        for (selector, frame_bytes) in selectors.iter().zip(self.frame_bytes) {
            writeln!(
                output,
                "{profile_prefix}.{output_prefix}.{}_frame_bytes={frame_bytes}",
                selector.output_name
            )
            .expect("writing stack inspection to String cannot fail");
        }
        let required_stack_bytes = self
            .required_stack_bytes(output_prefix)
            .expect("validated live mutation stack requirement cannot overflow");
        let raw_headroom_bytes = usable_stack_bytes
            .checked_sub(self.cumulative_frame_bytes)
            .expect("validated live mutation chain must fit usable stack");
        let policy_headroom_bytes = usable_stack_bytes
            .checked_sub(required_stack_bytes)
            .expect("validated live mutation chain and reserve must fit usable stack");
        writeln!(
            output,
            "{profile_prefix}.{output_prefix}.cumulative_frame_bytes={}",
            self.cumulative_frame_bytes
        )
        .expect("writing stack inspection to String cannot fail");
        writeln!(
            output,
            "{profile_prefix}.{output_prefix}.raw_headroom_bytes={raw_headroom_bytes}"
        )
        .expect("writing stack inspection to String cannot fail");
        writeln!(
            output,
            "{profile_prefix}.{output_prefix}.policy_headroom_bytes={policy_headroom_bytes}"
        )
        .expect("writing stack inspection to String cannot fail");
    }
}

fn inspect_pre_usb_mount_stack(
    object: &object::File<'_>,
    records: &[StackSizeRecord],
    path: &Path,
    label: &str,
) -> Result<PreUsbMountStack, String> {
    let context = format!("{label} ELF {}", path.display());
    let mut frame_bytes = [0_u64; PRE_USB_MOUNT_STACK_COMPONENT_COUNT];
    for (index, selector) in PRE_USB_MOUNT_STACK_SELECTORS.iter().copied().enumerate() {
        let addresses = object.symbols().filter_map(|symbol| {
            if symbol.kind() != SymbolKind::Text || symbol.section() == SymbolSection::Undefined {
                return None;
            }
            let name = symbol.name().ok()?;
            selector.matches(name).then_some(symbol.address())
        });
        let address =
            unique_selected_stack_symbol_address(&context, "pre-USB mount", selector, addresses)?;
        frame_bytes[index] =
            stack_frame_bytes_at_address(&context, "pre-USB mount", selector, records, address)?;
    }
    PreUsbMountStack::from_frame_bytes(frame_bytes)
}

fn inspect_live_mutation_stack<const COMPONENTS: usize>(
    object: &object::File<'_>,
    records: &[StackSizeRecord],
    path: &Path,
    label: &str,
    path_name: &str,
    selectors: &[StackSymbolSelector; COMPONENTS],
) -> Result<LiveMutationStack<COMPONENTS>, String> {
    let context = format!("{label} ELF {}", path.display());
    let mut frame_bytes = [0_u64; COMPONENTS];
    for (index, selector) in selectors.iter().copied().enumerate() {
        let addresses = object.symbols().filter_map(|symbol| {
            if symbol.kind() != SymbolKind::Text || symbol.section() == SymbolSection::Undefined {
                return None;
            }
            let name = symbol.name().ok()?;
            selector.matches(name).then_some(symbol.address())
        });
        let address =
            unique_selected_stack_symbol_address(&context, path_name, selector, addresses)?;
        frame_bytes[index] =
            stack_frame_bytes_at_address(&context, path_name, selector, records, address)?;
    }
    LiveMutationStack::from_frame_bytes(path_name, frame_bytes)
}

fn unique_selected_stack_symbol_address(
    context: &str,
    path_name: &str,
    selector: StackSymbolSelector,
    addresses: impl IntoIterator<Item = u64>,
) -> Result<u64, String> {
    let mut addresses: Vec<u64> = addresses.into_iter().collect();
    addresses.sort_unstable();
    addresses.dedup();
    match addresses.as_slice() {
        [address] => Ok(*address),
        _ => Err(format!(
            "{context} must contain exactly one defined text symbol for {path_name} stack component {}, found {} distinct addresses",
            selector.output_name,
            addresses.len()
        )),
    }
}

fn stack_frame_bytes_at_address(
    context: &str,
    path_name: &str,
    selector: StackSymbolSelector,
    records: &[StackSizeRecord],
    address: u64,
) -> Result<u64, String> {
    let mut frames = records
        .iter()
        .filter(|record| record.function_address == address)
        .map(|record| record.frame_bytes);
    let frame_bytes = frames.next().ok_or_else(|| {
        format!(
            "{context} .stack_sizes has no record for {path_name} stack component {} at 0x{address:x}",
            selector.output_name
        )
    })?;
    if frames.any(|candidate| candidate != frame_bytes) {
        return Err(format!(
            "{context} .stack_sizes has conflicting records for {path_name} stack component {} at 0x{address:x}",
            selector.output_name
        ));
    }
    Ok(frame_bytes)
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

impl StartupElfInspection {
    fn validate(&self) -> Result<(), String> {
        if self.stack_sizes.record_count == 0 {
            return Err("startup E290 .stack_sizes contains no records".to_owned());
        }
        if self.stack.guard_offset_bytes != EXPECTED_STACK_GUARD_OFFSET_BYTES {
            return Err(format!(
                "startup E290 stack guard offset {} differs from the reviewed {} bytes",
                self.stack.guard_offset_bytes, EXPECTED_STACK_GUARD_OFFSET_BYTES
            ));
        }
        let selected_maximum_frame = self
            .startup_stack
            .frame_bytes
            .iter()
            .copied()
            .max()
            .expect("startup stack has two selected frames");
        if self.stack_sizes.maximum_frame_bytes != selected_maximum_frame {
            return Err(format!(
                "startup E290 largest compiler-emitted frame {} is not the audited product_main/NodeCore::new maximum {selected_maximum_frame}",
                self.stack_sizes.maximum_frame_bytes
            ));
        }
        if !self.supervisor_statics.is_empty() {
            let mut symbols = String::new();
            for (index, symbol) in self.supervisor_statics.iter().enumerate() {
                if index != 0 {
                    symbols.push_str(", ");
                }
                write!(symbols, "{}@0x{:x}", symbol.name, symbol.address)
                    .expect("writing supervisor symbol diagnostics to String cannot fail");
            }
            return Err(format!(
                "startup E290 must not retain a defined internal SUPERVISOR static symbol; found {}: {symbols}",
                self.supervisor_statics.len()
            ));
        }
        let required_stack_bytes = self.startup_stack.required_stack_bytes("startup")?;
        if required_stack_bytes > self.stack.usable_bytes {
            return Err(format!(
                "startup E290 product_main poll and NodeCore::new compiler-emitted frames total {} bytes plus the reviewed {STORAGE_PATH_STACK_RESERVE_BYTES}-byte ROM/interrupt reserve require {required_stack_bytes} bytes, exceeding the {}-byte usable CPU0 stack by {} bytes",
                self.startup_stack.cumulative_frame_bytes,
                self.stack.usable_bytes,
                required_stack_bytes - self.stack.usable_bytes,
            ));
        }
        Ok(())
    }

    fn render(&self) -> String {
        let required_stack_bytes = self
            .startup_stack
            .required_stack_bytes("startup")
            .expect("validated startup stack requirement cannot overflow");
        let raw_headroom_bytes = self
            .stack
            .usable_bytes
            .checked_sub(self.startup_stack.cumulative_frame_bytes)
            .expect("validated startup compiler frames must fit usable stack");
        let policy_headroom_bytes = self
            .stack
            .usable_bytes
            .checked_sub(required_stack_bytes)
            .expect("validated startup requirement must fit usable stack");
        format!(
            "startup.stack_size_records={}\nstartup.maximum_frame_bytes={}\nstartup.stack_reserved_bytes={}\nstartup.stack_usable_bytes={}\nstartup.stack_guard_offset_bytes={}\nstartup.supervisor_static_symbol_count={}\nstartup.product_main_poll_frame_bytes={}\nstartup.node_core_new_frame_bytes={}\nstartup.cumulative_frame_bytes={}\nstartup.reserve_bytes={}\nstartup.required_stack_bytes={required_stack_bytes}\nstartup.raw_headroom_bytes={raw_headroom_bytes}\nstartup.policy_headroom_bytes={policy_headroom_bytes}",
            self.stack_sizes.record_count,
            self.stack_sizes.maximum_frame_bytes,
            self.stack.reserved_bytes,
            self.stack.usable_bytes,
            self.stack.guard_offset_bytes,
            self.supervisor_statics.len(),
            self.startup_stack.frame_bytes[0],
            self.startup_stack.frame_bytes[1],
            self.startup_stack.cumulative_frame_bytes,
            STORAGE_PATH_STACK_RESERVE_BYTES,
        )
    }
}

impl ElfInspection {
    fn validate(&self) -> Result<(), String> {
        if self.default_proof_trace_symbol_count != 0 {
            return Err(format!(
                "default E290 must exclude proof-trace evidence, found {} symbols",
                self.default_proof_trace_symbol_count
            ));
        }
        if self.default_lxmf_trace_symbol_count != 0 {
            return Err(format!(
                "default E290 must exclude durable-LXMF trace evidence, found {} symbols",
                self.default_lxmf_trace_symbol_count
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
        if self.hil_lxmf_trace_symbol_count != 1
            || self.hil_lxmf_trace_symbol_size_bytes != LXMF_BYTE_SIZE as u64
        {
            return Err(format!(
                "runtime-measurement HIL must contain one initialized {LXMF_BYTE_SIZE}-byte durable-LXMF trace symbol, got count={} size={}",
                self.hil_lxmf_trace_symbol_count, self.hil_lxmf_trace_symbol_size_bytes
            ));
        }
        for (label, inventory) in [
            ("default E290", self.default_stack_sizes),
            ("runtime-measurement HIL", self.hil_stack_sizes),
        ] {
            if inventory.record_count == 0 {
                return Err(format!("{label} .stack_sizes contains no records"));
            }
        }
        for (label, stack, minimum_usable, pre_usb_mount_stack) in [
            (
                "default E290",
                self.default_stack,
                MINIMUM_DEFAULT_USABLE_STACK_BYTES,
                self.default_pre_usb_mount_stack,
            ),
            (
                "runtime-measurement HIL",
                self.hil_stack,
                MINIMUM_HIL_USABLE_STACK_BYTES,
                self.hil_pre_usb_mount_stack,
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
            let required_stack_bytes = pre_usb_mount_stack.required_stack_bytes()?;
            if required_stack_bytes > stack.usable_bytes {
                return Err(format!(
                    "{label} pre-USB mount compiler-emitted frames total {} bytes plus the reviewed {STORAGE_PATH_STACK_RESERVE_BYTES}-byte ROM/interrupt reserve require {required_stack_bytes} bytes, exceeding the {}-byte usable stack by {} bytes",
                    pre_usb_mount_stack.cumulative_frame_bytes,
                    stack.usable_bytes,
                    required_stack_bytes - stack.usable_bytes,
                ));
            }
            let (live_append_stack, live_compact_stack) = if label == "default E290" {
                (
                    self.default_live_append_stack,
                    self.default_live_compact_stack,
                )
            } else {
                (self.hil_live_append_stack, self.hil_live_compact_stack)
            };
            validate_live_mutation_stack(
                label,
                "live append",
                live_append_stack,
                stack.usable_bytes,
            )?;
            validate_live_mutation_stack(
                label,
                "live compact",
                live_compact_stack,
                stack.usable_bytes,
            )?;
        }
        Ok(())
    }

    fn render(self) -> String {
        let mut output = format!(
            "default.stack_size_records={}\ndefault.maximum_frame_bytes={}\ndefault.stack_reserved_bytes={}\ndefault.stack_usable_bytes={}\ndefault.stack_guard_offset_bytes={}\n",
            self.default_stack_sizes.record_count,
            self.default_stack_sizes.maximum_frame_bytes,
            self.default_stack.reserved_bytes,
            self.default_stack.usable_bytes,
            self.default_stack.guard_offset_bytes,
        );
        self.default_pre_usb_mount_stack.render_into(
            "default",
            self.default_stack.usable_bytes,
            &mut output,
        );
        self.default_live_append_stack.render_into(
            "default",
            "live_append",
            &LIVE_APPEND_STACK_SELECTORS,
            self.default_stack.usable_bytes,
            &mut output,
        );
        self.default_live_compact_stack.render_into(
            "default",
            "live_compact",
            &LIVE_COMPACT_STACK_SELECTORS,
            self.default_stack.usable_bytes,
            &mut output,
        );
        write!(
            output,
            "default.proof_trace_symbol_count={}\ndefault.lxmf_trace_symbol_count={}\nhil.stack_size_records={}\nhil.maximum_frame_bytes={}\nhil.stack_reserved_bytes={}\nhil.stack_usable_bytes={}\nhil.stack_guard_offset_bytes={}\n",
            self.default_proof_trace_symbol_count,
            self.default_lxmf_trace_symbol_count,
            self.hil_stack_sizes.record_count,
            self.hil_stack_sizes.maximum_frame_bytes,
            self.hil_stack.reserved_bytes,
            self.hil_stack.usable_bytes,
            self.hil_stack.guard_offset_bytes,
        )
        .expect("writing stack inspection to String cannot fail");
        self.hil_pre_usb_mount_stack
            .render_into("hil", self.hil_stack.usable_bytes, &mut output);
        self.hil_live_append_stack.render_into(
            "hil",
            "live_append",
            &LIVE_APPEND_STACK_SELECTORS,
            self.hil_stack.usable_bytes,
            &mut output,
        );
        self.hil_live_compact_stack.render_into(
            "hil",
            "live_compact",
            &LIVE_COMPACT_STACK_SELECTORS,
            self.hil_stack.usable_bytes,
            &mut output,
        );
        write!(
            output,
            "hil.proof_trace_symbol_count={}\nhil.proof_trace_symbol_size_bytes={}\nhil.lxmf_trace_symbol_count={}\nhil.lxmf_trace_symbol_size_bytes={}\npolicy.minimum_default_usable_stack_bytes={}\npolicy.minimum_hil_usable_stack_bytes={}\npolicy.expected_stack_guard_offset_bytes={}\npolicy.storage_path_stack_reserve_bytes={}\nqualification.raw_painted_margin_bytes={}\nqualification.historical_maximum_frame_bytes={}\nqualification.historical_conservative_margin_bytes={}",
            self.hil_proof_trace_symbol_count,
            self.hil_proof_trace_symbol_size_bytes,
            self.hil_lxmf_trace_symbol_count,
            self.hil_lxmf_trace_symbol_size_bytes,
            MINIMUM_DEFAULT_USABLE_STACK_BYTES,
            MINIMUM_HIL_USABLE_STACK_BYTES,
            EXPECTED_STACK_GUARD_OFFSET_BYTES,
            STORAGE_PATH_STACK_RESERVE_BYTES,
            QUALIFIED_RAW_STACK_MARGIN_BYTES,
            HISTORICAL_QUALIFIED_MAXIMUM_STACK_FRAME_BYTES,
            HISTORICAL_MINIMUM_CONSERVATIVE_STACK_MARGIN_BYTES,
        )
        .expect("writing stack inspection to String cannot fail");
        output
    }
}

fn validate_live_mutation_stack<const COMPONENTS: usize>(
    label: &str,
    path_name: &str,
    stack_path: LiveMutationStack<COMPONENTS>,
    usable_stack_bytes: u64,
) -> Result<(), String> {
    let required_stack_bytes = stack_path.required_stack_bytes(path_name)?;
    if required_stack_bytes > usable_stack_bytes {
        return Err(format!(
            "{label} {path_name} compiler-emitted frames total {} bytes plus the reviewed {STORAGE_PATH_STACK_RESERVE_BYTES}-byte ROM/interrupt reserve require {required_stack_bytes} bytes, exceeding the {usable_stack_bytes}-byte usable stack by {} bytes",
            stack_path.cumulative_frame_bytes,
            required_stack_bytes - usable_stack_bytes,
        ));
    }
    Ok(())
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

impl DecodedLxmfTraceEvidence {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != LXMF_BYTE_SIZE {
            return Err(format!(
                "durable-LXMF trace input must be exactly {LXMF_BYTE_SIZE} bytes, got {}",
                bytes.len()
            ));
        }
        let mut words = [0_u32; LXMF_WORD_COUNT];
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
        let begin = self.words[LXMF_SNAPSHOT_SEQ_BEGIN_WORD];
        let end = self.words[LXMF_SNAPSHOT_SEQ_END_WORD];
        if begin != end {
            return Err(format!(
                "durable-LXMF trace snapshot sequence markers must match, got begin={begin} end={end}"
            ));
        }
        if begin & 1 != 0 {
            return Err(format!(
                "durable-LXMF trace snapshot sequence markers must be even, got {begin}"
            ));
        }
        if self.words[LXMF_MAGIC_WORD] != LXMF_MAGIC {
            return Err(format!(
                "durable-LXMF trace magic must be LXTE, got 0x{:08x}",
                self.words[LXMF_MAGIC_WORD]
            ));
        }
        if self.words[LXMF_VERSION_WORD] != LXMF_VERSION {
            return Err(format!(
                "durable-LXMF trace version must be {LXMF_VERSION}, got {}",
                self.words[LXMF_VERSION_WORD]
            ));
        }
        if self.words[LXMF_SIZE_WORD] != LXMF_BYTE_SIZE as u32 {
            return Err(format!(
                "durable-LXMF trace size_bytes must be {LXMF_BYTE_SIZE}, got {}",
                self.words[LXMF_SIZE_WORD]
            ));
        }

        let flags = self.words[LXMF_FLAGS_WORD];
        let unknown_flags = flags & !LXMF_KNOWN_FLAG_MASK;
        if unknown_flags != 0 {
            return Err(format!(
                "durable-LXMF trace flags.raw contains unknown bits 0x{unknown_flags:08x}"
            ));
        }
        if flags & LXMF_FLAG_ACTIVE == 0 {
            return Err("durable-LXMF trace flags.active must be true".to_owned());
        }

        let has_commit = self.words[LXMF_DURABLE_NEW_COUNT_WORD] != 0
            || self.words[LXMF_DURABLE_ALREADY_COUNT_WORD] != 0;
        let commit_present = flags & LXMF_FLAG_LAST_COMMIT_PRESENT != 0;
        if has_commit != commit_present {
            return Err(
                "durable-LXMF trace last-commit presence must match durable counts".to_owned(),
            );
        }
        let last_new = flags & LXMF_FLAG_LAST_COMMIT_NEW != 0;
        let last_already = flags & LXMF_FLAG_LAST_COMMIT_ALREADY_DURABLE != 0;
        if commit_present && last_new == last_already {
            return Err(
                "durable-LXMF trace last commit must be exactly one of new or already_durable"
                    .to_owned(),
            );
        }
        if !commit_present && (last_new || last_already) {
            return Err(
                "durable-LXMF trace last-commit kind requires last_commit_present".to_owned(),
            );
        }
        let handle = self.wide(LXMF_LAST_HANDLE_LOW_WORD, LXMF_LAST_HANDLE_HIGH_WORD);
        if commit_present && handle == 0 && flags & LXMF_FLAG_INPUT_INCONSISTENT == 0 {
            return Err("durable-LXMF trace committed durable handle must be nonzero".to_owned());
        }
        if !commit_present
            && (handle != 0
                || self.words[LXMF_LAST_MESSAGE_ID_FIRST_WORD..=LXMF_LAST_MESSAGE_ID_LAST_WORD]
                    .iter()
                    .any(|word| *word != 0))
        {
            return Err(
                "durable-LXMF trace last-message fields require last_commit_present".to_owned(),
            );
        }

        let saturated = flags & LXMF_FLAG_SATURATED != 0;
        let ready = self.words[LXMF_PROOF_READY_COUNT_WORD];
        let released = self.words[LXMF_PROOF_RELEASED_COUNT_WORD];
        let handed_off = self.words[LXMF_PROOF_HANDOFF_COUNT_WORD];
        if !saturated {
            let durable = self.words[LXMF_DURABLE_NEW_COUNT_WORD]
                .saturating_add(self.words[LXMF_DURABLE_ALREADY_COUNT_WORD]);
            if ready > durable {
                return Err("durable-LXMF trace proof.ready.count exceeds durable count".to_owned());
            }
            if (released > ready || handed_off > released)
                && flags & LXMF_FLAG_ORDERING_VIOLATION == 0
            {
                return Err(
                    "durable-LXMF trace out-of-order proof counts require ordering_violation"
                        .to_owned(),
                );
            }
        }
        let violation_count = self.words[LXMF_ORDERING_VIOLATION_COUNT_WORD];
        if (violation_count != 0) != (flags & LXMF_FLAG_ORDERING_VIOLATION != 0) {
            return Err(
                "durable-LXMF trace ordering violation flag must match its count".to_owned(),
            );
        }

        let tag_present = flags & LXMF_FLAG_PROOF_TAG_PRESENT != 0;
        let tag = self.wide(LXMF_LAST_PROOF_TAG_LOW_WORD, LXMF_LAST_PROOF_TAG_HIGH_WORD);
        if !tag_present && tag != 0 {
            return Err("durable-LXMF trace proof tag words require proof_tag_present".to_owned());
        }
        if tag_present && released == 0 {
            return Err(
                "durable-LXMF trace proof_tag_present requires a released proof".to_owned(),
            );
        }
        if released != 0 && !tag_present && flags & LXMF_FLAG_INPUT_INCONSISTENT == 0 {
            return Err(
                "durable-LXMF trace missing released-proof tag requires input_inconsistent"
                    .to_owned(),
            );
        }
        Ok(())
    }

    fn validate_empty_initializer(&self) -> Result<(), String> {
        for (word, name) in LXMF_WORD_NAMES.iter().copied().enumerate() {
            let expected = match word {
                LXMF_MAGIC_WORD => LXMF_MAGIC,
                LXMF_VERSION_WORD => LXMF_VERSION,
                LXMF_SIZE_WORD => LXMF_BYTE_SIZE as u32,
                LXMF_FLAGS_WORD => LXMF_FLAG_ACTIVE,
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

    fn render_human(&self) -> String {
        let mut output = String::new();
        for (word, name) in LXMF_WORD_NAMES.iter().copied().enumerate() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(name);
            output.push('=');
            if word == LXMF_MAGIC_WORD {
                output.push_str("LXTE");
            } else {
                let _ = write!(output, "{}", self.words[word]);
            }
            if word == LXMF_FLAGS_WORD {
                for (flag_name, flag) in LXMF_FLAG_NAMES {
                    let _ = write!(
                        output,
                        "\n{flag_name}={}",
                        self.words[LXMF_FLAGS_WORD] & flag != 0
                    );
                }
            }
        }
        let _ = write!(
            output,
            "\nlast_commit_kind={}\nlast_message_id={}\nlast_durable_handle={}\nlast_proof_tag={}",
            self.last_commit_kind(),
            self.last_message_id(),
            self.optional_wide(
                LXMF_FLAG_LAST_COMMIT_PRESENT,
                LXMF_LAST_HANDLE_LOW_WORD,
                LXMF_LAST_HANDLE_HIGH_WORD,
            ),
            self.optional_wide(
                LXMF_FLAG_PROOF_TAG_PRESENT,
                LXMF_LAST_PROOF_TAG_LOW_WORD,
                LXMF_LAST_PROOF_TAG_HIGH_WORD,
            ),
        );
        output
    }

    fn render_json(&self) -> String {
        let mut output = String::from("{");
        let mut first = true;
        for (word, name) in LXMF_WORD_NAMES.iter().copied().enumerate() {
            if !first {
                output.push(',');
            }
            first = false;
            let _ = write!(output, "\"{name}\":");
            if word == LXMF_MAGIC_WORD {
                output.push_str("\"LXTE\"");
            } else {
                let _ = write!(output, "{}", self.words[word]);
            }
            if word == LXMF_FLAGS_WORD {
                for (flag_name, flag) in LXMF_FLAG_NAMES {
                    let _ = write!(
                        output,
                        ",\"{flag_name}\":{}",
                        self.words[LXMF_FLAGS_WORD] & flag != 0
                    );
                }
            }
        }
        let _ = write!(
            output,
            ",\"last_commit_kind\":\"{}\",\"last_message_id\":{},\"last_durable_handle\":{},\"last_proof_tag\":{}",
            self.last_commit_kind(),
            self.json_last_message_id(),
            self.json_optional_wide(
                LXMF_FLAG_LAST_COMMIT_PRESENT,
                LXMF_LAST_HANDLE_LOW_WORD,
                LXMF_LAST_HANDLE_HIGH_WORD,
            ),
            self.json_optional_wide(
                LXMF_FLAG_PROOF_TAG_PRESENT,
                LXMF_LAST_PROOF_TAG_LOW_WORD,
                LXMF_LAST_PROOF_TAG_HIGH_WORD,
            ),
        );
        output.push('}');
        output
    }

    fn last_commit_kind(&self) -> &'static str {
        let flags = self.words[LXMF_FLAGS_WORD];
        if flags & LXMF_FLAG_LAST_COMMIT_NEW != 0 {
            "new"
        } else if flags & LXMF_FLAG_LAST_COMMIT_ALREADY_DURABLE != 0 {
            "already_durable"
        } else {
            "unobserved"
        }
    }

    fn last_message_id(&self) -> String {
        if self.words[LXMF_FLAGS_WORD] & LXMF_FLAG_LAST_COMMIT_PRESENT == 0 {
            return "unobserved".to_owned();
        }
        let mut output = String::with_capacity(64);
        for word in &self.words[LXMF_LAST_MESSAGE_ID_FIRST_WORD..=LXMF_LAST_MESSAGE_ID_LAST_WORD] {
            for byte in word.to_le_bytes() {
                let _ = write!(output, "{byte:02x}");
            }
        }
        output
    }

    fn json_last_message_id(&self) -> String {
        if self.words[LXMF_FLAGS_WORD] & LXMF_FLAG_LAST_COMMIT_PRESENT == 0 {
            "null".to_owned()
        } else {
            format!("\"{}\"", self.last_message_id())
        }
    }

    fn wide(&self, low: usize, high: usize) -> u64 {
        u64::from(self.words[low]) | (u64::from(self.words[high]) << 32)
    }

    fn optional_wide(&self, flag: u32, low: usize, high: usize) -> String {
        if self.words[LXMF_FLAGS_WORD] & flag == 0 {
            "unobserved".to_owned()
        } else {
            format!("0x{:016x}", self.wide(low, high))
        }
    }

    fn json_optional_wide(&self, flag: u32, low: usize, high: usize) -> String {
        if self.words[LXMF_FLAGS_WORD] & flag == 0 {
            "null".to_owned()
        } else {
            format!("\"0x{:016x}\"", self.wide(low, high))
        }
    }
}

impl DecodedCheckpoint {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != CHECKPOINT_BYTE_SIZE {
            return Err(format!(
                "checkpoint input must be exactly {CHECKPOINT_BYTE_SIZE} bytes, got {}",
                bytes.len()
            ));
        }
        let lxmf_trace = DecodedLxmfTraceEvidence::parse(&bytes[..LXMF_BYTE_SIZE])
            .map_err(|error| format!("invalid checkpoint LXTE record: {error}"))?;
        let runtime = DecodedEvidence::parse(&bytes[LXMF_BYTE_SIZE..LXMF_BYTE_SIZE + BYTE_SIZE])
            .map_err(|error| format!("invalid checkpoint RTME record: {error}"))?;
        let proof_trace = DecodedProofTraceEvidence::parse(&bytes[LXMF_BYTE_SIZE + BYTE_SIZE..])
            .map_err(|error| format!("invalid checkpoint RPTE record: {error}"))?;
        let expected_count = runtime.words[TX_COUNT_WORD];
        let observed_count = proof_trace.words[PROOF_RADIO_TX_CONFIRMED_SUCCESS_COUNT_WORD]
            .saturating_add(proof_trace.words[PROOF_RADIO_TX_NOT_CONFIRMED_SUCCESS_COUNT_WORD]);
        let saturated = runtime.words[FLAGS_WORD] & FLAG_SATURATED != 0
            || proof_trace.words[PROOF_FLAGS_WORD] & PROOF_FLAG_SATURATED != 0;
        // RTME and RPTE each have a stable snapshot, but their sequence words
        // are independent. A debugger halt can therefore land after RPTE has
        // classified a TX and before the RTME operation guard completes. Keep
        // absolute equality as a typed diagnostic; later multi-checkpoint
        // qualification evaluates the invariant over stable deltas.
        let tx_partition = TxPartitionDiagnostic {
            expected_count,
            observed_count,
            consistent: (!saturated).then_some(expected_count == observed_count),
        };
        Ok(Self {
            runtime,
            proof_trace,
            lxmf_trace,
            tx_partition,
        })
    }

    fn render_human(&self) -> String {
        let consistent = match self.tx_partition.consistent {
            Some(true) => "true",
            Some(false) => "false",
            None => "unobserved-saturated",
        };
        let mut output = format!(
            "schema={CHECKPOINT_SCHEMA}\nsize_bytes={CHECKPOINT_BYTE_SIZE}\ntx_partition_consistent={consistent}\ntx_partition_expected_count={}\ntx_partition_observed_count={}",
            self.tx_partition.expected_count, self.tx_partition.observed_count
        );
        for line in self.runtime.render_human().lines() {
            let _ = write!(output, "\nrtme.{line}");
        }
        for line in self.proof_trace.render_human().lines() {
            let _ = write!(output, "\nrpte.{line}");
        }
        for line in self.lxmf_trace.render_human().lines() {
            let _ = write!(output, "\nlxte.{line}");
        }
        output
    }

    fn render_json(&self) -> String {
        let consistent = match self.tx_partition.consistent {
            Some(true) => "true",
            Some(false) => "false",
            None => "null",
        };
        format!(
            "{{\"schema\":\"{CHECKPOINT_SCHEMA}\",\"size_bytes\":{CHECKPOINT_BYTE_SIZE},\"tx_partition_consistent\":{consistent},\"tx_partition_expected_count\":{},\"tx_partition_observed_count\":{},\"rtme\":{},\"rpte\":{},\"lxte\":{}}}",
            self.tx_partition.expected_count,
            self.tx_partition.observed_count,
            self.runtime.render_json(),
            self.proof_trace.render_json(),
            self.lxmf_trace.render_json(),
        )
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
    assert!(POST_PROOF_LINKED_STACK_REDUCTION_BYTES == 3_544);
    assert!(APPLICATION_EVENT_LINKED_STACK_REDUCTION_BYTES == 2_408);
    assert!(DURABLE_LXMF_DEFAULT_POLICY_REDUCTION_BYTES == 2_632);
    assert!(LXMF_BYTE_SIZE == 96);
    assert!(CHECKPOINT_BYTE_SIZE == 544);
    assert!(PRE_STAGE5_CARRIED_RAW_STACK_MARGIN_BYTES == 63_436);
    assert!(QUALIFIED_RAW_STACK_MARGIN_BYTES == 57_700);
    assert!(HISTORICAL_QUALIFIED_MAXIMUM_STACK_FRAME_BYTES == 53_680);
    assert!(HISTORICAL_MINIMUM_CONSERVATIVE_STACK_MARGIN_BYTES == 4_020);
    assert!(STORAGE_PATH_STACK_RESERVE_BYTES == 4_096);
    assert!(PRE_USB_MOUNT_STACK_SELECTORS.len() == PRE_USB_MOUNT_STACK_COMPONENT_COUNT);
    assert!(LIVE_APPEND_STACK_SELECTORS.len() == LIVE_APPEND_STACK_COMPONENT_COUNT);
    assert!(LIVE_COMPACT_STACK_SELECTORS.len() == LIVE_COMPACT_STACK_COMPONENT_COUNT);
    assert!(
        HISTORICAL_QUALIFIED_MAXIMUM_STACK_FRAME_BYTES
            + HISTORICAL_MINIMUM_CONSERVATIVE_STACK_MARGIN_BYTES
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
    assert!(LXMF_WORD_NAMES.len() == LXMF_WORD_COUNT);
    assert!(LXMF_SNAPSHOT_SEQ_BEGIN_WORD == 0);
    assert!(LXMF_MAGIC_WORD == LXMF_SNAPSHOT_SEQ_BEGIN_WORD + 1);
    assert!(LXMF_VERSION_WORD == LXMF_MAGIC_WORD + 1);
    assert!(LXMF_SIZE_WORD == LXMF_VERSION_WORD + 1);
    assert!(LXMF_FLAGS_WORD == LXMF_SIZE_WORD + 1);
    assert!(LXMF_DURABLE_NEW_COUNT_WORD == LXMF_FLAGS_WORD + 1);
    assert!(LXMF_DURABLE_ALREADY_COUNT_WORD == LXMF_DURABLE_NEW_COUNT_WORD + 1);
    assert!(LXMF_PROOF_READY_COUNT_WORD == LXMF_DURABLE_ALREADY_COUNT_WORD + 1);
    assert!(LXMF_PROOF_RELEASED_COUNT_WORD == LXMF_PROOF_READY_COUNT_WORD + 1);
    assert!(LXMF_PROOF_HANDOFF_COUNT_WORD == LXMF_PROOF_RELEASED_COUNT_WORD + 1);
    assert!(LXMF_ORDERING_VIOLATION_COUNT_WORD == LXMF_PROOF_HANDOFF_COUNT_WORD + 1);
    assert!(LXMF_LAST_MESSAGE_ID_FIRST_WORD == LXMF_ORDERING_VIOLATION_COUNT_WORD + 1);
    assert!(LXMF_LAST_MESSAGE_ID_LAST_WORD == LXMF_LAST_MESSAGE_ID_FIRST_WORD + 7);
    assert!(LXMF_LAST_HANDLE_LOW_WORD == LXMF_LAST_MESSAGE_ID_LAST_WORD + 1);
    assert!(LXMF_LAST_HANDLE_HIGH_WORD == LXMF_LAST_HANDLE_LOW_WORD + 1);
    assert!(LXMF_LAST_PROOF_TAG_LOW_WORD == LXMF_LAST_HANDLE_HIGH_WORD + 1);
    assert!(LXMF_LAST_PROOF_TAG_HIGH_WORD == LXMF_LAST_PROOF_TAG_LOW_WORD + 1);
    assert!(LXMF_SNAPSHOT_SEQ_END_WORD == LXMF_LAST_PROOF_TAG_HIGH_WORD + 1);
    assert!(LXMF_SNAPSHOT_SEQ_END_WORD + 1 == LXMF_WORD_COUNT);
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
        BootPhase, HeapSnapshot, OperationKind, RuntimeLxmfCommitKind, RuntimeLxmfTraceEvidence,
        RuntimeMeasurementEvidence, RuntimeProofTraceEvidence, RuntimeProofTraceIngressDisposition,
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

    struct TempDirectory {
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

    impl TempDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEMP_INPUT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "reticulum-e290-rtme-{label}-{}-{sequence}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).expect("temporary test directory must be creatable");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
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
        assert_eq!(
            parse_command_options(&strings(&[
                "inspect-startup-elf",
                "--elf",
                "production-ble.elf",
            ])),
            Ok(CommandOptions::InspectStartupElf(
                StartupElfInspectionOptions {
                    elf: PathBuf::from("production-ble.elf"),
                }
            ))
        );
    }

    #[test]
    fn elf_inspection_cli_rejects_incomplete_duplicate_and_unknown_arguments() {
        for (args, expected) in [
            (
                strings(&[]),
                "e290-runtime-measurement subcommand is required",
            ),
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
            (strings(&["inspect-startup-elf"]), "--elf is required"),
            (
                strings(&["inspect-startup-elf", "--elf"]),
                "--elf requires a value",
            ),
            (
                strings(&[
                    "inspect-startup-elf",
                    "--elf",
                    "one.elf",
                    "--elf",
                    "two.elf",
                ]),
                "--elf may be supplied only once",
            ),
            (
                strings(&["inspect-startup-elf", "--elf=one.elf"]),
                "unknown option --elf=one.elf",
            ),
            (
                strings(&["inspect-startup-elf", "one.elf"]),
                "unexpected argument one.elf",
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
        append_stack_size_record(&mut data, 0x4037_1000, 53_680);
        append_stack_size_record(&mut data, 0x4037_2000, 42_960);
        assert_eq!(
            parse_stack_size_records(&data, 4),
            Ok(ParsedStackSizes {
                inventory: StackSizeInventory {
                    record_count: 3,
                    maximum_frame_bytes: 53_680,
                },
                records: vec![
                    StackSizeRecord {
                        function_address: 0x4037_0000,
                        frame_bytes: 0,
                    },
                    StackSizeRecord {
                        function_address: 0x4037_1000,
                        frame_bytes: 53_680,
                    },
                    StackSizeRecord {
                        function_address: 0x4037_2000,
                        frame_bytes: 42_960,
                    },
                ],
            })
        );

        let mut wide = 0x4201_2345_6789_abcd_u64.to_le_bytes().to_vec();
        wide.push(32);
        assert_eq!(
            parse_stack_size_records(&wide, 8).unwrap().records,
            vec![StackSizeRecord {
                function_address: 0x4201_2345_6789_abcd,
                frame_bytes: 32,
            }]
        );

        let mut conflicting = Vec::new();
        append_stack_size_record(&mut conflicting, 0x4200_1000, 32);
        append_stack_size_record(&mut conflicting, 0x4200_1000, 64);
        assert!(
            parse_stack_size_records(&conflicting, 4)
                .unwrap_err()
                .contains("conflicting frame sizes at function address 0x42001000")
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
    fn storage_path_selectors_and_frame_resolution_fail_closed() {
        for selector in STARTUP_STACK_SELECTORS
            .into_iter()
            .chain(PRE_USB_MOUNT_STACK_SELECTORS)
            .chain(LIVE_APPEND_STACK_SELECTORS)
            .chain(LIVE_COMPACT_STACK_SELECTORS)
        {
            let matching_name = selector.required_fragments.join("__");
            assert!(
                selector.matches(&matching_name),
                "selector {} rejected its required fragments",
                selector.output_name
            );
            for rejected in selector.rejected_fragments {
                assert!(
                    !selector.matches(&format!("{matching_name}__{rejected}")),
                    "selector {} accepted rejected fragment {rejected}",
                    selector.output_name
                );
            }
        }

        let selector = PRE_USB_MOUNT_STACK_SELECTORS[2];
        assert_eq!(
            unique_selected_stack_symbol_address(
                "fixture",
                "fixture path",
                selector,
                [0x4200_1000, 0x4200_1000]
            ),
            Ok(0x4200_1000)
        );
        assert!(
            unique_selected_stack_symbol_address("fixture", "fixture path", selector, [])
                .unwrap_err()
                .contains("found 0 distinct addresses")
        );
        assert!(
            unique_selected_stack_symbol_address(
                "fixture",
                "fixture path",
                selector,
                [0x4200_1000, 0x4200_2000]
            )
            .unwrap_err()
            .contains("found 2 distinct addresses")
        );

        let records = [StackSizeRecord {
            function_address: 0x4200_1000,
            frame_bytes: 1_072,
        }];
        assert_eq!(
            stack_frame_bytes_at_address(
                "fixture",
                "fixture path",
                selector,
                &records,
                0x4200_1000
            ),
            Ok(1_072)
        );
        assert!(
            stack_frame_bytes_at_address(
                "fixture",
                "fixture path",
                selector,
                &records,
                0x4200_2000
            )
            .unwrap_err()
            .contains("has no record")
        );
        assert!(
            stack_frame_bytes_at_address(
                "fixture",
                "fixture path",
                selector,
                &[
                    records[0],
                    StackSizeRecord {
                        function_address: 0x4200_1000,
                        frame_bytes: 1_073,
                    },
                ],
                0x4200_1000,
            )
            .unwrap_err()
            .contains("conflicting records")
        );

        assert!(
            PreUsbMountStack::from_frame_bytes([u64::MAX, 1, 0, 0, 0, 0, 0, 0, 0])
                .unwrap_err()
                .contains("cumulative compiler-emitted frame size overflows")
        );
        assert!(
            LiveMutationStack::from_frame_bytes("live append", [u64::MAX, 1, 0, 0, 0, 0, 0, 0, 0],)
                .unwrap_err()
                .contains("live append cumulative compiler-emitted frame size overflows")
        );
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
    fn startup_elf_policy_gates_the_cumulative_constructor_path_and_static_owner() {
        let reviewed = StartupElfInspection {
            stack_sizes: StackSizeInventory {
                record_count: 1_322,
                maximum_frame_bytes: 64_288,
            },
            startup_stack: LiveMutationStack::from_frame_bytes("startup", [62_016, 64_288])
                .unwrap(),
            stack: StackLayout {
                reserved_bytes: 149_320,
                usable_bytes: 149_256,
                guard_offset_bytes: 60,
            },
            supervisor_statics: Vec::new(),
        };
        reviewed.validate().unwrap();
        let output = reviewed.render();
        assert!(output.contains("startup.stack_size_records=1322\n"));
        assert!(output.contains("startup.maximum_frame_bytes=64288\n"));
        assert!(output.contains("startup.stack_reserved_bytes=149320\n"));
        assert!(output.contains("startup.stack_usable_bytes=149256\n"));
        assert!(output.contains("startup.stack_guard_offset_bytes=60\n"));
        assert!(output.contains("startup.supervisor_static_symbol_count=0\n"));
        assert!(output.contains("startup.product_main_poll_frame_bytes=62016\n"));
        assert!(output.contains("startup.node_core_new_frame_bytes=64288\n"));
        assert!(output.contains("startup.cumulative_frame_bytes=126304\n"));
        assert!(output.contains("startup.reserve_bytes=4096\n"));
        assert!(output.contains("startup.required_stack_bytes=130400\n"));
        assert!(output.contains("startup.raw_headroom_bytes=22952\n"));
        assert!(output.ends_with("startup.policy_headroom_bytes=18856"));

        // This command intentionally replaces the stale individual-frame
        // ceiling for the startup boundary with the measured cumulative path.
        assert!(
            reviewed.stack_sizes.maximum_frame_bytes
                > HISTORICAL_QUALIFIED_MAXIMUM_STACK_FRAME_BYTES
        );

        let mut regressed = reviewed.clone();
        regressed.stack_sizes.maximum_frame_bytes += 1;
        let error = regressed.validate().unwrap_err();
        assert!(error.contains("largest compiler-emitted frame 64289"));
        assert!(error.contains("audited product_main/NodeCore::new maximum 64288"));

        let mut regressed = reviewed.clone();
        regressed.stack.guard_offset_bytes = 64;
        assert!(
            regressed
                .validate()
                .unwrap_err()
                .contains("stack guard offset 64")
        );

        let mut regressed = reviewed.clone();
        regressed.supervisor_statics = vec![DefinedSupervisorStatic {
            name: "_RNvProduct10SUPERVISOR".to_owned(),
            address: 0x3fc9_8188,
        }];
        let error = regressed.validate().unwrap_err();
        assert!(error.contains("must not retain a defined internal SUPERVISOR static symbol"));
        assert!(error.contains("_RNvProduct10SUPERVISOR@0x3fc98188"));

        let mut regressed = reviewed.clone();
        regressed.stack.usable_bytes = 130_399;
        let error = regressed.validate().unwrap_err();
        assert!(error.contains("product_main poll and NodeCore::new"));
        assert!(error.contains("exceeding the 130399-byte usable CPU0 stack by 1 bytes"));

        let mut empty = reviewed;
        empty.stack_sizes.record_count = 0;
        assert!(
            empty
                .validate()
                .unwrap_err()
                .contains(".stack_sizes contains no records")
        );

        let not_an_elf = TempInput::new(b"not-an-elf");
        let error = inspect_startup_elf(&StartupElfInspectionOptions {
            elf: not_an_elf.path().to_owned(),
        })
        .expect_err("non-ELF startup artifact was accepted");
        assert!(error.contains("could not parse startup E290 ELF"));
    }

    #[test]
    fn elf_policy_accepts_reviewed_bounds_and_rejects_frame_or_stack_regressions() {
        let default_pre_usb_mount_stack = PreUsbMountStack::from_frame_bytes([
            35_984, 2_640, 80, 320, 912, 4_592, 4_368, 4_144, 32,
        ])
        .unwrap();
        let hil_pre_usb_mount_stack = PreUsbMountStack::from_frame_bytes([
            36_160, 2_640, 80, 320, 912, 4_592, 4_368, 4_144, 32,
        ])
        .unwrap();
        let default_live_append_stack = LiveMutationStack::from_frame_bytes(
            "live append",
            [33_632, 1_104, 2_224, 288, 2_064, 4_592, 4_368, 4_144, 32],
        )
        .unwrap();
        let hil_live_append_stack = LiveMutationStack::from_frame_bytes(
            "live append",
            [33_808, 1_104, 2_224, 288, 2_064, 4_592, 4_368, 4_144, 32],
        )
        .unwrap();
        let default_live_compact_stack = LiveMutationStack::from_frame_bytes(
            "live compact",
            [
                33_632, 1_104, 2_224, 288, 1_120, 832, 4_592, 4_368, 4_144, 32,
            ],
        )
        .unwrap();
        let hil_live_compact_stack = LiveMutationStack::from_frame_bytes(
            "live compact",
            [
                33_808, 1_104, 2_224, 288, 1_120, 832, 4_592, 4_368, 4_144, 32,
            ],
        )
        .unwrap();
        let reviewed = ElfInspection {
            default_stack_sizes: StackSizeInventory {
                record_count: 1_025,
                maximum_frame_bytes: 64_288,
            },
            default_pre_usb_mount_stack,
            default_live_append_stack,
            default_live_compact_stack,
            default_stack: StackLayout {
                reserved_bytes: 162_440,
                usable_bytes: 162_376,
                guard_offset_bytes: 60,
            },
            default_proof_trace_symbol_count: 0,
            default_lxmf_trace_symbol_count: 0,
            hil_stack_sizes: StackSizeInventory {
                record_count: 1_025,
                maximum_frame_bytes: 64_288,
            },
            hil_pre_usb_mount_stack,
            hil_live_append_stack,
            hil_live_compact_stack,
            hil_stack: StackLayout {
                reserved_bytes: 161_640,
                usable_bytes: 161_576,
                guard_offset_bytes: 60,
            },
            hil_proof_trace_symbol_count: 1,
            hil_proof_trace_symbol_size_bytes: 192,
            hil_lxmf_trace_symbol_count: 1,
            hil_lxmf_trace_symbol_size_bytes: 96,
        };
        reviewed.validate().unwrap();
        let output = reviewed.render();
        assert!(output.contains("default.maximum_frame_bytes=64288\n"));
        assert!(output.contains("default.stack_usable_bytes=162376\n"));
        assert!(
            output.contains("default.pre_usb_mount.submission_runtime_mount_into_frame_bytes=80\n")
        );
        assert!(output.contains("default.pre_usb_mount.cumulative_frame_bytes=53072\n"));
        assert!(output.contains("default.pre_usb_mount.policy_headroom_bytes=105208\n"));
        assert!(output.contains("default.live_append.cumulative_frame_bytes=52448\n"));
        assert!(output.contains("default.live_append.policy_headroom_bytes=105832\n"));
        assert!(output.contains("default.live_compact.cumulative_frame_bytes=52336\n"));
        assert!(output.contains("default.live_compact.policy_headroom_bytes=105944\n"));
        assert!(output.contains("default.proof_trace_symbol_count=0\n"));
        assert!(output.contains("default.lxmf_trace_symbol_count=0\n"));
        assert!(output.contains("hil.stack_usable_bytes=161576\n"));
        assert!(output.contains("hil.pre_usb_mount.cumulative_frame_bytes=53248\n"));
        assert!(output.contains("hil.pre_usb_mount.policy_headroom_bytes=104232\n"));
        assert!(output.contains("hil.live_append.cumulative_frame_bytes=52624\n"));
        assert!(output.contains("hil.live_append.policy_headroom_bytes=104856\n"));
        assert!(output.contains("hil.live_compact.cumulative_frame_bytes=52512\n"));
        assert!(output.contains("hil.live_compact.policy_headroom_bytes=104968\n"));
        assert!(output.contains("hil.proof_trace_symbol_count=1\n"));
        assert!(output.contains("hil.proof_trace_symbol_size_bytes=192\n"));
        assert!(output.contains("hil.lxmf_trace_symbol_count=1\n"));
        assert!(output.contains("hil.lxmf_trace_symbol_size_bytes=96\n"));
        assert!(output.contains("policy.storage_path_stack_reserve_bytes=4096\n"));
        assert!(output.contains("qualification.historical_maximum_frame_bytes=53680\n"));
        assert!(output.ends_with("qualification.historical_conservative_margin_bytes=4020"));

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
        regressed.default_lxmf_trace_symbol_count = 1;
        assert!(regressed.validate().unwrap_err().contains("must exclude"));

        let mut regressed = reviewed;
        regressed.hil_lxmf_trace_symbol_count = 0;
        assert!(
            regressed
                .validate()
                .unwrap_err()
                .contains("one initialized")
        );

        let mut regressed = reviewed;
        regressed.hil_lxmf_trace_symbol_size_bytes = 95;
        assert!(regressed.validate().unwrap_err().contains("size=95"));

        let mut regressed = reviewed;
        regressed.default_pre_usb_mount_stack =
            PreUsbMountStack::from_frame_bytes([53_680, 53_680, 53_680, 0, 0, 0, 0, 0, 0]).unwrap();
        let error = regressed.validate().unwrap_err();
        assert!(error.contains("default E290 pre-USB mount"));
        assert!(error.contains("exceeding the 162376-byte usable stack by 2760 bytes"));

        let mut regressed = reviewed;
        regressed.hil_pre_usb_mount_stack =
            PreUsbMountStack::from_frame_bytes([53_680, 53_680, 53_680, 0, 0, 0, 0, 0, 0]).unwrap();
        let error = regressed.validate().unwrap_err();
        assert!(error.contains("runtime-measurement HIL pre-USB mount"));
        assert!(error.contains("exceeding the 161576-byte usable stack by 3560 bytes"));

        let mut regressed = reviewed;
        regressed.default_live_append_stack = LiveMutationStack::from_frame_bytes(
            "live append",
            [53_680, 53_680, 53_680, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
        let error = regressed.validate().unwrap_err();
        assert!(error.contains("default E290 live append"));
        assert!(error.contains("exceeding the 162376-byte usable stack by 2760 bytes"));

        let mut regressed = reviewed;
        regressed.hil_live_compact_stack = LiveMutationStack::from_frame_bytes(
            "live compact",
            [53_680, 53_680, 53_680, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
        let error = regressed.validate().unwrap_err();
        assert!(error.contains("runtime-measurement HIL live compact"));
        assert!(error.contains("exceeding the 161576-byte usable stack by 3560 bytes"));

        let mut regressed = reviewed;
        regressed.default_stack.usable_bytes -= 1;
        assert!(
            regressed
                .validate()
                .unwrap_err()
                .contains("usable stack 162375")
        );

        let mut regressed = reviewed;
        regressed.hil_stack.usable_bytes -= 1;
        assert!(
            regressed
                .validate()
                .unwrap_err()
                .contains("usable stack 161575")
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

    fn lxmf_minimal_words() -> [u32; LXMF_WORD_COUNT] {
        let mut words = [0_u32; LXMF_WORD_COUNT];
        words[LXMF_MAGIC_WORD] = LXMF_MAGIC;
        words[LXMF_VERSION_WORD] = LXMF_VERSION;
        words[LXMF_SIZE_WORD] = LXMF_BYTE_SIZE as u32;
        words[LXMF_FLAGS_WORD] = LXMF_FLAG_ACTIVE;
        words
    }

    fn encode_lxmf(words: [u32; LXMF_WORD_COUNT]) -> [u8; LXMF_BYTE_SIZE] {
        let mut bytes = [0_u8; LXMF_BYTE_SIZE];
        for (word, destination) in words
            .into_iter()
            .zip(bytes.chunks_exact_mut(size_of::<u32>()))
        {
            destination.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    fn lxmf_producer_bytes(evidence: &RuntimeLxmfTraceEvidence) -> [u8; LXMF_BYTE_SIZE] {
        assert_eq!(
            size_of::<RuntimeLxmfTraceEvidence>(),
            LXMF_BYTE_SIZE,
            "durable-LXMF producer ABI size changed"
        );
        let mut bytes = [0_u8; LXMF_BYTE_SIZE];
        // SAFETY: the firmware module compile-time asserts an exact repr(C)
        // sequence of initialized four-byte fields with no padding. This test
        // owns `evidence`, so no writer can mutate the stable snapshot here.
        let source = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(evidence).cast::<u8>(),
                size_of::<RuntimeLxmfTraceEvidence>(),
            )
        };
        bytes.copy_from_slice(source);
        bytes
    }

    fn populated_lxmf_evidence() -> RuntimeLxmfTraceEvidence {
        let evidence = RuntimeLxmfTraceEvidence::new();
        let message_id = core::array::from_fn(|index| index as u8);
        evidence.record_durable(
            RuntimeLxmfCommitKind::New,
            &message_id,
            0x1122_3344_5566_7788,
            true,
        );
        evidence.record_proof_released(Some(0x8877_6655_4433_2211));
        evidence.record_proof_handed_off();
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

    #[test]
    fn lxmf_decoder_matches_firmware_abi_and_renders_decisive_lifecycle_evidence() {
        for length in [0, LXMF_BYTE_SIZE - 1, LXMF_BYTE_SIZE + 1] {
            let error = DecodedLxmfTraceEvidence::parse(&vec![0; length])
                .expect_err("wrong durable-LXMF trace size was accepted");
            assert_eq!(
                error,
                format!(
                    "durable-LXMF trace input must be exactly {LXMF_BYTE_SIZE} bytes, got {length}"
                )
            );
        }

        let empty = DecodedLxmfTraceEvidence::parse(&encode_lxmf(lxmf_minimal_words())).unwrap();
        empty.validate_empty_initializer().unwrap();

        let evidence = populated_lxmf_evidence();
        let decoded = DecodedLxmfTraceEvidence::parse(&lxmf_producer_bytes(&evidence))
            .expect("firmware durable-LXMF trace must satisfy its host decoder");
        assert_eq!(decoded.words[LXMF_DURABLE_NEW_COUNT_WORD], 1);
        assert_eq!(decoded.words[LXMF_PROOF_READY_COUNT_WORD], 1);
        assert_eq!(decoded.words[LXMF_PROOF_RELEASED_COUNT_WORD], 1);
        assert_eq!(decoded.words[LXMF_PROOF_HANDOFF_COUNT_WORD], 1);
        assert_eq!(decoded.words[LXMF_ORDERING_VIOLATION_COUNT_WORD], 0);

        let human = decoded.render_human();
        assert!(human.starts_with("snapshot_seq_begin=6\nmagic=LXTE\nversion=1\n"));
        assert!(human.contains("flags.last_commit_new=true\n"));
        assert!(human.contains("flags.proof_tag_present=true\n"));
        assert!(human.contains("last_commit_kind=new\n"));
        assert!(human.contains(
            "last_message_id=000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\n"
        ));
        assert!(human.contains("last_durable_handle=0x1122334455667788\n"));
        assert!(human.ends_with("last_proof_tag=0x8877665544332211"));

        let json: serde_json::Value = serde_json::from_str(&decoded.render_json()).unwrap();
        assert_eq!(json["magic"], "LXTE");
        assert_eq!(json["durable.new.count"], 1);
        assert_eq!(json["proof.ready.count"], 1);
        assert_eq!(json["proof.released.count"], 1);
        assert_eq!(json["proof.ordinary_handoff.count"], 1);
        assert_eq!(json["ordering.violation.count"], 0);
        assert_eq!(json["last_durable_handle"], "0x1122334455667788");
        assert_eq!(json["last_proof_tag"], "0x8877665544332211");
    }

    #[test]
    fn lxmf_trace_cli_parses_and_executes_human_and_json_forms() {
        let expected = Options {
            input: PathBuf::from("lxmf.bin"),
            json: true,
        };
        assert_eq!(
            parse_lxmf_trace_options(&strings(&[
                "decode-lxmf-trace",
                "--json",
                "--input",
                "lxmf.bin",
            ])),
            Ok(expected)
        );
        assert!(matches!(
            parse_command_options(&strings(&["decode-lxmf-trace", "--input", "lxmf.bin"])),
            Ok(CommandOptions::DecodeLxmfTrace(_))
        ));
        assert_eq!(
            parse_lxmf_trace_options(&strings(&["decode-lxmf-trace"])).unwrap_err(),
            "--input is required"
        );

        let input = TempInput::new(&lxmf_producer_bytes(&populated_lxmf_evidence()));
        let human = execute_lxmf_trace(&Options {
            input: input.path().to_owned(),
            json: false,
        })
        .unwrap();
        assert!(human.contains("proof.ordinary_handoff.count=1\n"));
        let json = execute_lxmf_trace(&Options {
            input: input.path().to_owned(),
            json: true,
        })
        .unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
    }

    fn checkpoint_fixture(
        runtime_tx_count: u32,
        confirmed_tx_count: u32,
        not_confirmed_tx_count: u32,
        runtime_saturated: bool,
        proof_saturated: bool,
    ) -> Vec<u8> {
        let mut runtime = populated_words();
        runtime[TX_COUNT_WORD] = runtime_tx_count;
        if runtime_saturated {
            runtime[FLAGS_WORD] |= FLAG_SATURATED;
        }
        let mut proof = proof_minimal_words();
        proof[PROOF_RADIO_TX_CONFIRMED_SUCCESS_COUNT_WORD] = confirmed_tx_count;
        proof[PROOF_RADIO_TX_NOT_CONFIRMED_SUCCESS_COUNT_WORD] = not_confirmed_tx_count;
        if proof_saturated {
            proof[PROOF_FLAGS_WORD] |= PROOF_FLAG_SATURATED;
        }
        let mut bytes = Vec::with_capacity(CHECKPOINT_BYTE_SIZE);
        bytes.extend_from_slice(&encode_lxmf(lxmf_minimal_words()));
        bytes.extend_from_slice(&encode(runtime));
        bytes.extend_from_slice(&encode_proof(proof));
        bytes
    }

    #[cfg(unix)]
    fn fake_executable(temporary: &TempDirectory, name: &str, contents: &[u8]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let directory = temporary.path().join(format!("{name}-bin"));
        fs::create_dir(&directory).unwrap();
        let path = directory.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::canonicalize(path).unwrap()
    }

    #[test]
    fn combined_checkpoint_requires_exact_typed_records_and_reports_tx_partition() {
        for length in [0, CHECKPOINT_BYTE_SIZE - 1, CHECKPOINT_BYTE_SIZE + 1] {
            let error = DecodedCheckpoint::parse(&vec![0; length])
                .expect_err("wrong-size checkpoint was accepted");
            assert_eq!(
                error,
                format!(
                    "checkpoint input must be exactly {CHECKPOINT_BYTE_SIZE} bytes, got {length}"
                )
            );
        }

        let checkpoint = DecodedCheckpoint::parse(&checkpoint_fixture(4, 3, 1, false, false))
            .expect("consistent combined checkpoint must decode");
        assert_eq!(
            checkpoint.tx_partition,
            TxPartitionDiagnostic {
                expected_count: 4,
                observed_count: 4,
                consistent: Some(true),
            }
        );

        let mismatch = DecodedCheckpoint::parse(&checkpoint_fixture(4, 2, 1, false, false))
            .expect("independently sequenced mismatch must remain valid evidence");
        assert_eq!(mismatch.tx_partition.consistent, Some(false));
        assert_eq!(mismatch.tx_partition.expected_count, 4);
        assert_eq!(mismatch.tx_partition.observed_count, 3);

        for (runtime_saturated, proof_saturated) in [(true, false), (false, true)] {
            let saturated = DecodedCheckpoint::parse(&checkpoint_fixture(
                4,
                u32::MAX,
                1,
                runtime_saturated,
                proof_saturated,
            ))
            .expect("saturated combined checkpoint must remain decodable");
            assert_eq!(saturated.tx_partition.consistent, None);
            assert_eq!(saturated.tx_partition.observed_count, u32::MAX);
        }

        let mut invalid_runtime = checkpoint_fixture(4, 3, 1, false, false);
        let runtime_magic = LXMF_BYTE_SIZE + MAGIC_WORD * 4;
        invalid_runtime[runtime_magic..runtime_magic + 4]
            .copy_from_slice(&u32::from_le_bytes(*b"NOPE").to_le_bytes());
        assert!(
            DecodedCheckpoint::parse(&invalid_runtime)
                .unwrap_err()
                .starts_with("invalid checkpoint RTME record:")
        );
        let mut invalid_proof = checkpoint_fixture(4, 3, 1, false, false);
        let proof_magic = LXMF_BYTE_SIZE + BYTE_SIZE + PROOF_MAGIC_WORD * 4;
        invalid_proof[proof_magic..proof_magic + 4]
            .copy_from_slice(&u32::from_le_bytes(*b"NOPE").to_le_bytes());
        assert!(
            DecodedCheckpoint::parse(&invalid_proof)
                .unwrap_err()
                .starts_with("invalid checkpoint RPTE record:")
        );
        let mut invalid_lxmf = checkpoint_fixture(4, 3, 1, false, false);
        let lxmf_magic = LXMF_MAGIC_WORD * 4;
        invalid_lxmf[lxmf_magic..lxmf_magic + 4]
            .copy_from_slice(&u32::from_le_bytes(*b"NOPE").to_le_bytes());
        assert!(
            DecodedCheckpoint::parse(&invalid_lxmf)
                .unwrap_err()
                .starts_with("invalid checkpoint LXTE record:")
        );
    }

    #[test]
    fn combined_checkpoint_cli_and_outputs_are_deterministic_and_typed() {
        let expected = Options {
            input: PathBuf::from("checkpoint.bin"),
            json: true,
        };
        assert_eq!(
            parse_checkpoint_options(&strings(&[
                "decode-checkpoint",
                "--json",
                "--input",
                "checkpoint.bin",
            ])),
            Ok(expected)
        );
        assert!(matches!(
            parse_command_options(&strings(&[
                "decode-checkpoint",
                "--input",
                "checkpoint.bin",
            ])),
            Ok(CommandOptions::DecodeCheckpoint(_))
        ));
        for (args, expected) in [
            (strings(&["decode-checkpoint"]), "--input is required"),
            (
                strings(&["decode-checkpoint", "--input", "a", "--input", "b"]),
                "--input may be supplied only once",
            ),
            (
                strings(&["decode-checkpoint", "--input", "a", "--wat"]),
                "unknown option --wat",
            ),
        ] {
            assert_eq!(parse_checkpoint_options(&args).unwrap_err(), expected);
        }

        let input = TempInput::new(&checkpoint_fixture(4, 2, 1, false, false));
        let human = execute_checkpoint(&Options {
            input: input.path().to_owned(),
            json: false,
        })
        .unwrap();
        assert!(human.starts_with(
            "schema=reticulum.e290-runtime-checkpoint.v2\nsize_bytes=544\ntx_partition_consistent=false\ntx_partition_expected_count=4\ntx_partition_observed_count=3\n"
        ));
        assert!(human.contains("rtme.operation.tx.count=4\n"));
        assert!(human.contains("rpte.radio_tx.confirmed_success.count=2\n"));
        assert!(human.contains("lxte.magic=LXTE\n"));

        let json = execute_checkpoint(&Options {
            input: input.path().to_owned(),
            json: true,
        })
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(json["schema"], CHECKPOINT_SCHEMA);
        assert_eq!(json["size_bytes"], 544);
        assert_eq!(json["tx_partition_consistent"], false);
        assert_eq!(json["tx_partition_expected_count"], 4);
        assert_eq!(json["tx_partition_observed_count"], 3);
        assert_eq!(json["rtme"]["magic"], "RTME");
        assert_eq!(json["rpte"]["magic"], "RPTE");
        assert_eq!(json["lxte"]["magic"], "LXTE");

        let saturated = DecodedCheckpoint::parse(&checkpoint_fixture(4, 3, 1, true, false))
            .unwrap()
            .render_json();
        let saturated: serde_json::Value = serde_json::from_str(&saturated).unwrap();
        assert!(saturated["tx_partition_consistent"].is_null());
    }

    #[test]
    fn capture_cli_requires_exact_uppercase_usb_serial_and_flags() {
        let expected = CaptureCheckpointOptions {
            hil_elf: PathBuf::from("hil.elf"),
            usb_serial: "AC:A7:04:E1:3E:88".to_owned(),
            output: PathBuf::from("capture"),
            probe_rs: PathBuf::from(DEFAULT_PROBE_RS),
        };
        assert_eq!(
            parse_capture_checkpoint_options(&strings(&[
                "--hil-elf",
                "hil.elf",
                "--usb-serial",
                "AC:A7:04:E1:3E:88",
                "--output",
                "capture",
            ])),
            Ok(expected)
        );
        assert_eq!(
            parse_capture_checkpoint_options(&strings(&[
                "--output",
                "capture",
                "--probe-rs",
                "/opt/probe-rs",
                "--usb-serial",
                "AC:A7:04:E1:3E:88",
                "--hil-elf",
                "hil.elf",
            ]))
            .unwrap()
            .probe_rs,
            PathBuf::from("/opt/probe-rs")
        );
        for serial in [
            "ac:a7:04:e1:3e:88",
            "AC-A7-04-E1-3E-88",
            "AC:A7:04:E1:3E",
            "AC:A7:04:E1:3G:88",
            " AC:A7:04:E1:3E:88",
        ] {
            let error = parse_capture_checkpoint_options(&strings(&[
                "--hil-elf",
                "hil.elf",
                "--usb-serial",
                serial,
                "--output",
                "capture",
            ]))
            .expect_err("invalid USB serial was accepted");
            assert!(
                error.contains("six uppercase hexadecimal octets"),
                "{error}"
            );
        }
        for (args, expected) in [
            (Vec::new(), "--hil-elf is required"),
            (
                strings(&["--hil-elf", "a", "--usb-serial", "AC:A7:04:E1:3E:88"]),
                "--output is required",
            ),
            (
                strings(&[
                    "--hil-elf",
                    "a",
                    "--hil-elf",
                    "b",
                    "--usb-serial",
                    "AC:A7:04:E1:3E:88",
                    "--output",
                    "capture",
                ]),
                "--hil-elf may be supplied only once",
            ),
            (strings(&["--wat", "value"]), "unknown option --wat"),
        ] {
            assert_eq!(
                parse_capture_checkpoint_options(&args).unwrap_err(),
                expected
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn sanitized_probe_launch_clears_hostile_environment_cwd_and_home_presets() {
        let temporary = TempDirectory::new("probe-sanitize");
        let hostile_cwd = temporary.path().join("hostile-cwd");
        let hostile_home = temporary.path().join("hostile-home");
        let isolated_cwd = temporary.path().join("isolated-cwd");
        let isolated_home = temporary.path().join("isolated-home");
        for directory in [&hostile_cwd, &hostile_home, &isolated_cwd, &isolated_home] {
            fs::create_dir(directory).unwrap();
        }
        fs::write(
            hostile_cwd.join(".probe-rs.toml"),
            b"connect_under_reset = true\n",
        )
        .unwrap();
        fs::write(
            hostile_home.join(".probe-rs.yaml"),
            b"connect_under_reset: true\n",
        )
        .unwrap();
        fs::write(isolated_cwd.join(".probe-rs.toml"), EMPTY_PROBE_CONFIG).unwrap();
        let fake_probe = fake_executable(
            &temporary,
            "sanitized-probe",
            br##"#!/bin/sh
if [ "${PROBE_RS_CONNECT_UNDER_RESET+x}" = x ]; then exit 91; fi
if [ "${PROBE_RS_CONFIG_PRESET+x}" = x ]; then exit 92; fi
if [ -s .probe-rs.toml ]; then exit 93; fi
if [ -e "$HOME/.probe-rs.toml" ] || [ -e "$HOME/.probe-rs.json" ] || [ -e "$HOME/.probe-rs.yaml" ] || [ -e "$HOME/.probe-rs.yml" ]; then exit 94; fi
printf '%s\n' "$HOME" > observed-home
pwd > observed-cwd
"##,
        );
        let invocation = ProbeInvocation {
            program: fake_probe,
            arguments: Vec::new(),
            current_directory: fs::canonicalize(&isolated_cwd).unwrap(),
            home_directory: fs::canonicalize(&isolated_home).unwrap(),
        };
        let mut command = Command::new(&invocation.program);
        command
            .env("PROBE_RS_CONNECT_UNDER_RESET", "1")
            .env("PROBE_RS_CONFIG_PRESET", "hostile")
            .env("HOME", &hostile_home)
            .current_dir(&hostile_cwd);
        apply_sanitized_probe_launch(&mut command, &invocation);
        let status = command.status().unwrap();
        assert_eq!(status.code(), Some(0));
        let observed_home = invocation.current_directory.join("observed-home");
        let observed_cwd = invocation.current_directory.join("observed-cwd");
        assert_eq!(
            fs::read_to_string(&observed_home).unwrap(),
            format!("{}\n", invocation.home_directory.display())
        );
        assert_eq!(
            fs::read_to_string(&observed_cwd).unwrap(),
            format!("{}\n", invocation.current_directory.display())
        );
        fs::remove_file(observed_home).unwrap();
        fs::remove_file(observed_cwd).unwrap();
        validate_probe_launch_isolation(&invocation).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn probe_process_launch_failure_requires_manual_trial_abandonment() {
        let temporary = TempDirectory::new("probe-launch-failure");
        let isolated_cwd = temporary.path().join("isolated-cwd");
        let isolated_home = temporary.path().join("isolated-home");
        fs::create_dir(&isolated_cwd).unwrap();
        fs::create_dir(&isolated_home).unwrap();
        fs::write(isolated_cwd.join(".probe-rs.toml"), EMPTY_PROBE_CONFIG).unwrap();
        let invocation = ProbeInvocation {
            program: temporary.path().join("missing-probe-rs"),
            arguments: Vec::new(),
            current_directory: fs::canonicalize(&isolated_cwd).unwrap(),
            home_directory: fs::canonicalize(&isolated_home).unwrap(),
        };

        let error = run_probe_command(&invocation)
            .expect_err("missing probe-rs executable unexpectedly launched");
        assert!(
            error.contains("could not invoke probe-rs program"),
            "{error}"
        );
        assert!(error.contains(PROBE_FAILURE_GUIDANCE), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn capture_rejects_executable_parent_default_config_before_probe_invocation() {
        let temporary = TempDirectory::new("probe-parent-config");
        let elf = temporary.path().join("hil.elf");
        fs::write(&elf, b"elf").unwrap();
        let prepared = PreparedCheckpointCapture {
            elf_path: fs::canonicalize(&elf).unwrap(),
            elf_bytes: 3,
            elf_sha256: sha256_bytes(b"elf"),
            layout: CheckpointLayout {
                runtime: EvidenceSymbol {
                    address: 0x3fca_1060,
                    size_bytes: BYTE_SIZE as u64,
                },
                proof_trace: EvidenceSymbol {
                    address: 0x3fca_1160,
                    size_bytes: PROOF_BYTE_SIZE as u64,
                },
                lxmf_trace: EvidenceSymbol {
                    address: 0x3fca_1000,
                    size_bytes: LXMF_BYTE_SIZE as u64,
                },
            },
        };
        let fake_probe = fake_executable(&temporary, "hostile-probe", b"#!/bin/sh\nexit 0\n");
        fs::write(
            fake_probe.parent().unwrap().join(".probe-rs.json"),
            br#"{"presets":{"hostile":["--connect-under-reset"]}}"#,
        )
        .unwrap();
        let output = fs::canonicalize(temporary.path()).unwrap().join("capture");
        let options = CaptureCheckpointOptions {
            hil_elf: elf,
            usb_serial: "AC:A7:04:E1:3F:88".to_owned(),
            output: output.clone(),
            probe_rs: fake_probe,
        };
        let mut invoked = false;
        let error = capture_checkpoint_with(&options, &prepared, |_| {
            invoked = true;
            Ok(ProbeExit {
                success: true,
                description: "must not run".to_owned(),
            })
        })
        .expect_err("executable-parent default config was accepted");
        assert!(!invoked);
        assert!(error.contains(".probe-rs.json"), "{error}");
        assert!(error.contains("executable parent"), "{error}");
        assert!(output.join(CAPTURE_INCOMPLETE_FILE).is_file());
        assert_eq!(
            fs::read(output.join(PROBE_CONFIG_FILE)).unwrap(),
            EMPTY_PROBE_CONFIG
        );
        assert!(!output.join(CAPTURE_RAW_FILE).exists());
        assert!(!output.join(CAPTURE_COMPLETE_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_probe_read_requires_manual_trial_abandonment_without_helper_reset() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TempDirectory::new("probe-nonzero");
        let elf = temporary.path().join("hil.elf");
        fs::write(&elf, b"elf").unwrap();
        let prepared = PreparedCheckpointCapture {
            elf_path: fs::canonicalize(&elf).unwrap(),
            elf_bytes: 3,
            elf_sha256: sha256_bytes(b"elf"),
            layout: CheckpointLayout {
                runtime: EvidenceSymbol {
                    address: 0x3fca_1060,
                    size_bytes: BYTE_SIZE as u64,
                },
                proof_trace: EvidenceSymbol {
                    address: 0x3fca_1160,
                    size_bytes: PROOF_BYTE_SIZE as u64,
                },
                lxmf_trace: EvidenceSymbol {
                    address: 0x3fca_1000,
                    size_bytes: LXMF_BYTE_SIZE as u64,
                },
            },
        };
        let fake_probe = fake_executable(&temporary, "failed-probe", b"#!/bin/sh\nexit 1\n");
        let output = fs::canonicalize(temporary.path()).unwrap().join("capture");
        let options = CaptureCheckpointOptions {
            hil_elf: elf,
            usb_serial: "AC:A7:04:E1:3F:88".to_owned(),
            output: output.clone(),
            probe_rs: fake_probe,
        };
        let mut invocations = 0;
        let error = capture_checkpoint_with(&options, &prepared, |invocation| {
            invocations += 1;
            assert!(
                !invocation
                    .arguments
                    .iter()
                    .any(|argument| argument == "--connect-under-reset" || argument == "reset")
            );
            fs::write(
                output.join(CAPTURE_RAW_FILE),
                vec![0_u8; CHECKPOINT_BYTE_SIZE / 2],
            )
            .unwrap();
            Ok(ProbeExit {
                success: false,
                description: "exit status: 1".to_owned(),
            })
        })
        .expect_err("nonzero probe read was accepted");
        assert_eq!(invocations, 1);
        assert!(error.contains("exit status: 1"), "{error}");
        assert!(error.contains(PROBE_FAILURE_GUIDANCE), "{error}");
        assert!(output.join(CAPTURE_INCOMPLETE_FILE).is_file());
        assert_eq!(
            fs::metadata(output.join(CAPTURE_RAW_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(!output.join(CAPTURE_COMPLETE_FILE).exists());
    }

    #[test]
    fn checkpoint_layout_requires_exact_adjacent_symbols_and_final_elf_parser_fails_closed() {
        let valid = CheckpointLayout {
            runtime: EvidenceSymbol {
                address: 0x3fca_1060,
                size_bytes: BYTE_SIZE as u64,
            },
            proof_trace: EvidenceSymbol {
                address: 0x3fca_1160,
                size_bytes: PROOF_BYTE_SIZE as u64,
            },
            lxmf_trace: EvidenceSymbol {
                address: 0x3fca_1000,
                size_bytes: LXMF_BYTE_SIZE as u64,
            },
        };
        valid.validate().unwrap();

        let mut invalid = valid;
        invalid.runtime.size_bytes -= 1;
        assert!(invalid.validate().unwrap_err().contains("RTME symbol"));
        let mut invalid = valid;
        invalid.proof_trace.size_bytes -= 1;
        assert!(invalid.validate().unwrap_err().contains("RPTE symbol"));
        let mut invalid = valid;
        invalid.proof_trace.address += 4;
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .contains("start exactly 256")
        );
        let mut invalid = valid;
        invalid.lxmf_trace.size_bytes -= 1;
        assert!(invalid.validate().unwrap_err().contains("LXTE symbol"));
        let mut invalid = valid;
        invalid.lxmf_trace.address += 4;
        assert!(invalid.validate().unwrap_err().contains("start exactly 96"));
        let overflow = CheckpointLayout {
            runtime: EvidenceSymbol {
                address: 0,
                size_bytes: BYTE_SIZE as u64,
            },
            proof_trace: EvidenceSymbol {
                address: 0,
                size_bytes: PROOF_BYTE_SIZE as u64,
            },
            lxmf_trace: EvidenceSymbol {
                address: u64::MAX - 50,
                size_bytes: LXMF_BYTE_SIZE as u64,
            },
        };
        assert!(overflow.validate().unwrap_err().contains("overflows"));

        let not_an_elf = TempInput::new(b"not-an-elf");
        let error = inspect_checkpoint_capture_elf(not_an_elf.path())
            .expect_err("non-ELF capture artifact was accepted");
        assert!(error.contains("could not parse runtime-measurement HIL ELF"));
    }

    #[cfg(unix)]
    #[test]
    fn capture_uses_one_exact_probe_read_and_seals_synced_hashed_outputs() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TempDirectory::new("capture-success");
        let elf = temporary.path().join("hil.elf");
        let elf_bytes = b"bound HIL ELF fixture";
        fs::write(&elf, elf_bytes).unwrap();
        let prepared = PreparedCheckpointCapture {
            elf_path: fs::canonicalize(&elf).unwrap(),
            elf_bytes: elf_bytes.len() as u64,
            elf_sha256: sha256_bytes(elf_bytes),
            layout: CheckpointLayout {
                runtime: EvidenceSymbol {
                    address: 0x3fca_1060,
                    size_bytes: BYTE_SIZE as u64,
                },
                proof_trace: EvidenceSymbol {
                    address: 0x3fca_1160,
                    size_bytes: PROOF_BYTE_SIZE as u64,
                },
                lxmf_trace: EvidenceSymbol {
                    address: 0x3fca_1000,
                    size_bytes: LXMF_BYTE_SIZE as u64,
                },
            },
        };
        let output = fs::canonicalize(temporary.path()).unwrap().join("capture");
        let fake_probe = fake_executable(&temporary, "fake-probe-rs", b"#!/bin/sh\nexit 0\n");
        let options = CaptureCheckpointOptions {
            hil_elf: elf,
            usb_serial: "AC:A7:04:E1:3E:88".to_owned(),
            output: output.clone(),
            probe_rs: fake_probe.clone(),
        };
        let checkpoint = checkpoint_fixture(4, 3, 1, false, false);
        let mut invocation_count = 0;
        let result = capture_checkpoint_with(&options, &prepared, |invocation| {
            invocation_count += 1;
            assert_eq!(invocation.program, fake_probe);
            assert_eq!(
                invocation.current_directory,
                output.join(PROBE_CWD_DIRECTORY)
            );
            assert_eq!(invocation.home_directory, output.join(PROBE_HOME_DIRECTORY));
            assert_eq!(
                fs::read(invocation.current_directory.join(".probe-rs.toml")).unwrap(),
                EMPTY_PROBE_CONFIG
            );
            let expected = vec![
                OsString::from("read"),
                OsString::from("--chip"),
                OsString::from("esp32s3"),
                OsString::from("--protocol"),
                OsString::from("jtag"),
                OsString::from("--probe"),
                OsString::from("303a:1001:AC:A7:04:E1:3E:88"),
                OsString::from("--non-interactive"),
                OsString::from("--format"),
                OsString::from("binary"),
                OsString::from("--output"),
                output.join(CAPTURE_RAW_FILE).into_os_string(),
                OsString::from("b8"),
                OsString::from("0x3fca1000"),
                OsString::from("544"),
            ];
            assert_eq!(invocation.arguments, expected);
            fs::write(output.join(CAPTURE_RAW_FILE), &checkpoint).unwrap();
            Ok(ProbeExit {
                success: true,
                description: "exit status: 0".to_owned(),
            })
        })
        .unwrap();
        assert_eq!(invocation_count, 1);
        assert_eq!(result, format!("captured_checkpoint={}", output.display()));
        assert!(!output.join(CAPTURE_INCOMPLETE_FILE).exists());
        assert!(output.join(CAPTURE_COMPLETE_FILE).is_file());
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::read(output.join(CAPTURE_RAW_FILE)).unwrap(), checkpoint);
        assert_eq!(
            fs::read(output.join(CAPTURE_RUNTIME_FILE)).unwrap(),
            checkpoint[LXMF_BYTE_SIZE..LXMF_BYTE_SIZE + BYTE_SIZE]
        );
        assert_eq!(
            fs::read(output.join(CAPTURE_PROOF_FILE)).unwrap(),
            checkpoint[LXMF_BYTE_SIZE + BYTE_SIZE..]
        );
        assert_eq!(
            fs::read(output.join(CAPTURE_LXMF_FILE)).unwrap(),
            checkpoint[..LXMF_BYTE_SIZE]
        );

        let decoded: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join(CAPTURE_JSON_FILE)).unwrap()).unwrap();
        assert_eq!(decoded["tx_partition_consistent"], true);
        let manifest_bytes = fs::read(output.join(CAPTURE_MANIFEST_FILE)).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
        assert_eq!(manifest["schema"], CAPTURE_SCHEMA);
        assert_eq!(manifest["usb_serial"], "AC:A7:04:E1:3E:88");
        assert_eq!(manifest["hil_elf"]["sha256"], sha256_bytes(elf_bytes));
        assert_eq!(manifest["layout"]["runtime"]["address"], "0x3fca1060");
        assert_eq!(manifest["layout"]["proof_trace"]["address"], "0x3fca1160");
        assert_eq!(manifest["layout"]["lxmf_trace"]["address"], "0x3fca1000");
        assert_eq!(manifest["layout"]["contiguous_bytes"], 544);
        assert_eq!(
            manifest["probe"]["executable"]["path"],
            fake_probe.to_str().unwrap()
        );
        assert_eq!(
            manifest["probe"]["launch"]["environment_policy"],
            "clear-then-set-allowlist"
        );
        assert_eq!(
            manifest["probe"]["launch"]["environment_allowlist"],
            serde_json::json!(["HOME"])
        );
        let manifest_current_directory = manifest["probe"]["launch"]["current_directory"]
            .as_str()
            .unwrap();
        let manifest_home_directory = manifest["probe"]["launch"]["home_directory"]
            .as_str()
            .unwrap();
        assert!(Path::new(manifest_current_directory).is_absolute());
        assert!(Path::new(manifest_home_directory).is_absolute());
        assert_eq!(
            Path::new(manifest_current_directory),
            output.join(PROBE_CWD_DIRECTORY)
        );
        assert_eq!(
            Path::new(manifest_home_directory),
            output.join(PROBE_HOME_DIRECTORY)
        );
        assert_eq!(
            manifest["probe"]["launch"]["empty_config"]["sha256"],
            sha256_bytes(EMPTY_PROBE_CONFIG)
        );
        assert_eq!(manifest["files"].as_array().unwrap().len(), 7);
        for binding in manifest["files"].as_array().unwrap() {
            let relative = binding["path"].as_str().unwrap();
            let bytes = fs::read(output.join(relative)).unwrap();
            assert_eq!(binding["bytes"], bytes.len() as u64);
            assert_eq!(binding["sha256"], sha256_bytes(&bytes));
        }
        let complete = fs::read_to_string(output.join(CAPTURE_COMPLETE_FILE)).unwrap();
        assert_eq!(
            complete,
            format!(
                "{CAPTURE_SCHEMA}\nstatus=complete\nmanifest_sha256={}\n",
                sha256_bytes(&manifest_bytes)
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_preserves_incomplete_raw_evidence_on_probe_or_size_failure() {
        let temporary = TempDirectory::new("capture-incomplete");
        let elf = temporary.path().join("hil.elf");
        fs::write(&elf, b"elf").unwrap();
        let prepared = PreparedCheckpointCapture {
            elf_path: fs::canonicalize(&elf).unwrap(),
            elf_bytes: 3,
            elf_sha256: sha256_bytes(b"elf"),
            layout: CheckpointLayout {
                runtime: EvidenceSymbol {
                    address: 0x3fca_1060,
                    size_bytes: BYTE_SIZE as u64,
                },
                proof_trace: EvidenceSymbol {
                    address: 0x3fca_1160,
                    size_bytes: PROOF_BYTE_SIZE as u64,
                },
                lxmf_trace: EvidenceSymbol {
                    address: 0x3fca_1000,
                    size_bytes: LXMF_BYTE_SIZE as u64,
                },
            },
        };
        let output = fs::canonicalize(temporary.path()).unwrap().join("capture");
        let fake_probe = fake_executable(&temporary, "fake-probe-rs", b"#!/bin/sh\nexit 0\n");
        let options = CaptureCheckpointOptions {
            hil_elf: elf,
            usb_serial: "AC:A7:04:E1:3F:88".to_owned(),
            output: output.clone(),
            probe_rs: fake_probe,
        };
        let error = capture_checkpoint_with(&options, &prepared, |invocation| {
            let raw = invocation
                .arguments
                .iter()
                .position(|argument| argument == "--output")
                .and_then(|index| invocation.arguments.get(index + 1))
                .map(PathBuf::from)
                .unwrap();
            fs::write(raw, vec![0_u8; CHECKPOINT_BYTE_SIZE - 1]).unwrap();
            Ok(ProbeExit {
                success: true,
                description: "exit status: 0".to_owned(),
            })
        })
        .expect_err("short debugger output was accepted");
        assert!(error.contains("exactly 544 bytes, got 543"), "{error}");
        assert!(output.join(CAPTURE_INCOMPLETE_FILE).is_file());
        assert!(!output.join(CAPTURE_COMPLETE_FILE).exists());
        assert_eq!(
            fs::metadata(output.join(CAPTURE_RAW_FILE)).unwrap().len(),
            (CHECKPOINT_BYTE_SIZE - 1) as u64
        );
        assert!(!output.join(CAPTURE_MANIFEST_FILE).exists());
    }
}
