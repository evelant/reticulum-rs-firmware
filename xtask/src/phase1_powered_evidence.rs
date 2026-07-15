use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};

use reticulum_phase1_rx_local_data::generator::{
    self as boot_local_generator, Inputs as BootLocalInputs,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{phase1_closure, phase1_hil, phase1_source};

const SCHEMA: &str = "reticulum.phase1-rx-powered-evidence.v2";
const OPERATOR_SCHEMA: &str = "reticulum.phase1-rx-powered-evidence.operator.v2";
const SCENARIO_SCHEMA: &str = "reticulum.phase1-rx-powered-evidence.scenario.v2";
const MANIFEST_FILE: &str = "powered-evidence.json";
const OPERATOR_FILE: &str = "records/operator.json";
const SCENARIO_DIRECTORY: &str = "records/scenarios";
const CAPTURE_DIRECTORY: &str = "captures";
const INCOMPLETE_FILE: &str = "powered-evidence.incomplete";
const SEALED_FILE: &str = "powered-evidence.sealed";
const INVENTORY_FILE: &str = "artifacts.sha256";
const INVENTORY_TEMP_FILE: &str = ".artifacts.sha256.tmp";
const FINALIZE_LOCK_SUFFIX: &str = ".phase1-powered-evidence-finalize.lock";
const INCOMPLETE_CONTENT: &str = "reticulum.phase1-rx-powered-evidence.v2\nstatus=incomplete\n";

const NORMAL_MANIFEST_FILE: &str = "artifact-preparation.json";
const CLOSURE_MANIFEST_FILE: &str = "closure-artifact-preparation.json";
const PEER_FIRMWARE_VERSION: &str = "1.86";
const PEER_FIRMWARE_REVISION: &str = "9b39b6ce5962007fafefc22034082f354eff3374";
const PEER_FIRMWARE_ROOT_TREE: &str = "12f583c5f0fd8ae83c59a391267f0fe9ce184d86";
const PEER_FIRMWARE_VERSION_BYTES_HEX: &str = "0156";
const PEER_CORPUS_FILE: &str = "interop/vectors/rnode-hil-v1.json";
const PEER_TOOL_FILE: &str = "interop/python/rnode_hil.py";
const BOOT_LOCAL_GENERATOR_FILE: &str = boot_local_generator::SOURCE_PATH;
const BOOT_LOCAL_GENERATOR_SHA256_V2: &str =
    "ea0ba30bd562b19e95d4648c3c65c8c31a48db8c6df7bc3a5bc659d6f45122fe";
const PEER_MANIFEST_FILE: &str = "peer-manifest.json";
const PEER_TRANSCRIPT_FILE: &str = "peer-transcript.jsonl";
const BOOT_LOCAL_CORPUS_FILE: &str = "boot-local-data.json";
const BOOT_LOCAL_DESTINATION_NAME: &str = "reticulum-rs-firmware.heltec-tracker-v2.lab-rx";
const NORMAL_TARGET_MODE: &str = "lab-rx";
const BACKPRESSURE_TARGET_MODE: &str = "lab-rx-backpressure-hil";
const RETURNED_FAULT_ONE_BOOT_TARGET_MODE: &str =
    "lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=one-boot";
const RETURNED_FAULT_REPEAT_TARGET_MODE: &str =
    "lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=repeat-until-quarantine";
const KISS_FEND: u8 = 0xc0;
const KISS_FESC: u8 = 0xdb;
const KISS_TFEND: u8 = 0xdc;
const KISS_TFESC: u8 = 0xdd;
const CMD_DATA: u8 = 0x00;
const CMD_FREQUENCY: u8 = 0x01;
const CMD_BANDWIDTH: u8 = 0x02;
const CMD_TXPOWER: u8 = 0x03;
const CMD_SF: u8 = 0x04;
const CMD_CR: u8 = 0x05;
const CMD_RADIO_STATE: u8 = 0x06;
const CMD_DETECT: u8 = 0x08;
const CMD_IMPLICIT: u8 = 0x09;
const CMD_ST_ALOCK: u8 = 0x0b;
const CMD_LT_ALOCK: u8 = 0x0c;
const CMD_PROMISC: u8 = 0x0e;
const CMD_READY: u8 = 0x0f;
const CMD_STAT_PHYPRM: u8 = 0x26;
const CMD_BOARD: u8 = 0x47;
const CMD_PLATFORM: u8 = 0x48;
const CMD_MCU: u8 = 0x49;
const CMD_FW_VERSION: u8 = 0x50;

const COMMON_CHECKS: &[&str] = &[
    "artifact-mode-and-readback-bound",
    "independent-rf-observer-present",
    "logic-analyzer-capture-present",
    "no-prohibited-sx1262-tx-command",
    "no-tracker-originated-rf",
    "serial-capture-present",
];

#[derive(Clone, Copy, Debug)]
struct ScenarioDefinition {
    id: &'static str,
    title: &'static str,
    artifact_ids: &'static [&'static str],
    checks: &'static [&'static str],
}

const SCENARIOS: &[ScenarioDefinition] = &[
    ScenarioDefinition {
        id: "cold-boot-and-silence",
        title: "Cold boot and silence",
        artifact_ids: &["normal"],
        checks: &[
            "inert-pin-order",
            "profile-and-radio-constants",
            "two-heartbeats",
            "heap-stable",
            "stack-guard-and-scan-valid",
        ],
    },
    ScenarioDefinition {
        id: "single-physical-frame",
        title: "Single physical frame",
        artifact_ids: &["normal"],
        checks: &[
            "all-corpus-cases-run",
            "physical-counters-and-lengths",
            "rssi-and-snr-recorded",
            "rete-dispositions-recorded",
            "raw-packet-digests-match",
        ],
    },
    ScenarioDefinition {
        id: "split-packet",
        title: "Split packet",
        artifact_ids: &["normal"],
        checks: &[
            "all-corpus-cases-run",
            "first-half-pending",
            "reassembled-lengths-match",
            "conservative-phy-metadata",
            "raw-packet-digests-match",
        ],
    },
    ScenarioDefinition {
        id: "fragment-expiry-and-replacement",
        title: "Fragment expiry and replacement",
        artifact_ids: &["normal"],
        checks: &[
            "all-corpus-cases-run",
            "pending-expired-delta",
            "pending-replaced-delta",
            "no-cross-packet-splice",
        ],
    },
    ScenarioDefinition {
        id: "physical-over-rns-boundary",
        title: "Physical-over-RNS boundary",
        artifact_ids: &["normal"],
        checks: &[
            "all-corpus-cases-run",
            "hardware-mtu-frames-observed",
            "packets-too-long-delta",
            "no-rete-ingress-for-oversize",
        ],
    },
    ScenarioDefinition {
        id: "malformed-and-semantic-rejection",
        title: "Malformed and semantic rejection",
        artifact_ids: &["normal"],
        checks: &[
            "all-corpus-cases-run",
            "duplicate-and-reorder-rejected",
            "announce-dedup-correct",
            "boot-local-data-processed",
            "all-output-actions-suppressed",
        ],
    },
    ScenarioDefinition {
        id: "bounded-backpressure",
        title: "Bounded backpressure",
        artifact_ids: &["backpressure"],
        checks: &[
            "feature-bound-corpus-run",
            "configured-stall-seven-seconds",
            "offered-three-queued-two-dropped-one",
            "expiry-before-queued-service",
            "queued-frames-rejected-by-watermark",
            "no-completed-packet-or-rete-ingress",
        ],
    },
    ScenarioDefinition {
        id: "returned-radio-fault",
        title: "Returned radio fault and reset quarantine",
        artifact_ids: &[
            "returned-fault-one-boot",
            "returned-fault-repeat-until-quarantine",
        ],
        checks: &[
            "one-boot-fault-trace",
            "retained-write-failure-fails-closed",
            "repeat-policy-three-fault-quarantine",
            "healthy-lease-qualified",
            "supervisor-watchdog-combined-streak",
            "cold-power-cycle-behavior-recorded",
        ],
    },
    ScenarioDefinition {
        id: "corrupt-and-torn-retained-journal",
        title: "Corrupt and torn retained journal",
        artifact_ids: &[
            "reset-journal-corrupt-slot0-word4",
            "reset-journal-torn-write9",
        ],
        checks: &[
            "corrupt-selector-two-boot-run",
            "torn-selector-two-boot-run",
            "corrupt-or-torn-detected",
            "quarantine-before-peripheral-construction",
        ],
    },
    ScenarioDefinition {
        id: "electrical-matrix",
        title: "Electrical regulator and RX-gain matrix",
        artifact_ids: &[
            "electrical-ldo-unboosted",
            "electrical-ldo-boosted",
            "electrical-dcdc-unboosted",
            "electrical-dcdc-boosted",
        ],
        checks: &[
            "all-four-selections-measured",
            "calibrated-current-measurement",
            "safety-pin-timing-measured",
            "more-than-one-board-sample",
            "no-single-sample-policy-change",
        ],
    },
    ScenarioDefinition {
        id: "receive-soak-24h",
        title: "24-hour receive soak",
        artifact_ids: &["normal"],
        checks: &[
            "continuous-duration-at-least-24h",
            "gap-free-observer-index",
            "mixed-valid-and-hostile-traffic",
            "heap-stable",
            "stack-headroom-stable",
            "rete-maintenance-continues",
        ],
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
enum Cli {
    Init {
        normal_pressure_bundle: PathBuf,
        closure_bundle: PathBuf,
        output: PathBuf,
    },
    Finalize {
        evidence: PathBuf,
    },
    Verify {
        evidence: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ResultStatus {
    Pass,
    Fail,
    NotRun,
}

impl ResultStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotRun => "not-run",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EvidenceFileBinding {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EvidenceArtifactBinding {
    id: String,
    mode: String,
    elf: EvidenceFileBinding,
    flash_image: EvidenceFileBinding,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EvidenceBundleBinding {
    kind: String,
    schema: String,
    canonical_path: String,
    manifest_file: String,
    manifest_sha256: String,
    git_commit: String,
    git_root_tree: String,
    profile_environment: BTreeMap<String, String>,
    artifacts: Vec<EvidenceArtifactBinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EvidenceManifest {
    schema: String,
    created_unix_seconds: u64,
    normal_pressure_bundle: EvidenceBundleBinding,
    closure_bundle: EvidenceBundleBinding,
    required_scenarios: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct OperatorRecord {
    schema: String,
    operator: String,
    started_utc: Option<String>,
    finished_utc: Option<String>,
    board_revision: String,
    rf_variant_hz: String,
    board_sample_ids: Vec<String>,
    tracker_usb_identity: String,
    peer_id: String,
    peer_usb_identity: String,
    peer_firmware_version: String,
    peer_firmware_revision: String,
    peer_firmware_image_path: String,
    peer_firmware_sha256: String,
    peer_firmware_source_path: String,
    peer_firmware_source_sha256: String,
    peer_corpus_path: String,
    peer_corpus_sha256: String,
    peer_tool_path: String,
    peer_tool_sha256: String,
    peer_conducted_power_dbm: Option<i16>,
    peer_short_airtime_limit_basis_points: Option<u16>,
    peer_long_airtime_limit_basis_points: Option<u16>,
    peer_effective_short_airtime_limit_basis_points: Option<u16>,
    peer_effective_long_airtime_limit_basis_points: Option<u16>,
    peer_reported_preamble_symbols: Option<u16>,
    transmit_authorization: String,
    observer_id: String,
    observer_bandwidth_hz: Option<u64>,
    observer_noise_floor_dbm: Option<f64>,
    observer_detection_threshold_dbm: Option<f64>,
    observer_attribution_setup: String,
    logic_analyzer: String,
    rf_observer: String,
    current_instrument: String,
    antenna_or_load: String,
    pa_cps_probe: String,
    region_basis: String,
    notes: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PeerManifest {
    schema: u8,
    status: String,
    started_utc: String,
    finished_utc: String,
    corpus: String,
    corpus_sha256: String,
    tool: String,
    tool_sha256: String,
    scenario: serde_json::Value,
    serial_port: String,
    target_artifact_mode: String,
    profile: PeerProfile,
    receiver_fragment_timeout_us: u64,
    receiver_maximum_frame_airtime_us: u64,
    peer_preamble_extension_us: u64,
    post_enqueue_observation_ms: u64,
    region_basis: String,
    antenna_or_load_attached: bool,
    fresh_peer_reset_acknowledged: bool,
    fresh_tracker_boot_acknowledged: bool,
    independent_rf_observer_required: bool,
    runtime: PeerRuntime,
    enqueued_steps: usize,
    device: PeerDevice,
    peer_physical_timing: PeerPhysicalTiming,
    error: serde_json::Value,
    transcript_sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PeerProfile {
    frequency_hz: u64,
    bandwidth_hz: u64,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    tx_power_dbm: i16,
    expected_peer_preamble_symbols: u16,
    receiver_preamble_symbols: u16,
    short_airtime_limit_basis_points: u16,
    long_airtime_limit_basis_points: u16,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PeerRuntime {
    python_implementation: String,
    python_version: String,
    pyserial_version: String,
    serial: PeerSerial,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PeerSerial {
    baudrate: u64,
    bytesize: u8,
    parity: String,
    stopbits: u8,
    timeout_seconds: f64,
    write_timeout_seconds: f64,
    xonxoff: bool,
    rtscts: bool,
    dsrdtr: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PeerDevice {
    firmware_version: String,
    firmware_version_bytes_hex: String,
    board: u8,
    platform: u8,
    mcu: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PeerPhysicalTiming {
    symbol_time_us: u64,
    symbol_rate: u64,
    preamble_symbols: u16,
    preamble_time_ms: u64,
    csma_slot_ms: u64,
    difs_ms: u64,
    effective_short_airtime_limit_basis_points: u16,
    effective_long_airtime_limit_basis_points: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PeerTranscriptEntry {
    sequence: u64,
    utc: String,
    monotonic_ns: u64,
    direction: String,
    command: u8,
    payload_hex: String,
    wire_hex: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PeerFrame {
    command: u8,
    payload: Vec<u8>,
}

struct ProjectPeerSources {
    corpus: Vec<u8>,
    tool: Vec<u8>,
    boot_local_generator: Vec<u8>,
}

struct PeerValidationContext<'a> {
    evidence: &'a Path,
    operator: &'a OperatorRecord,
    profile_environment: &'a BTreeMap<String, String>,
    pinned_corpus: &'a serde_json::Value,
    pinned_corpus_bytes: &'a [u8],
    pinned_scenarios: &'a BTreeMap<String, serde_json::Value>,
    boot_local_generator_sha256: &'a str,
}

struct PeerExchangeContract {
    exchanges: Vec<(PeerFrame, PeerFrame)>,
    data: Vec<Vec<u8>>,
    physical: PeerFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PeerCorpusKind {
    Pinned,
    BootLocal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PeerRunExpectation {
    name: &'static str,
    count: usize,
    target_mode: &'static str,
    artifact_id: &'static str,
    binding_check: &'static str,
    corpus_kind: PeerCorpusKind,
}

const SINGLE_PEER_RUNS: &[PeerRunExpectation] = &[
    ordinary_peer_run("raw-header-only", "all-corpus-cases-run"),
    ordinary_peer_run("raw-single-1", "all-corpus-cases-run"),
    ordinary_peer_run("raw-single-253", "all-corpus-cases-run"),
    ordinary_peer_run("raw-single-254", "all-corpus-cases-run"),
];
const SPLIT_PEER_RUNS: &[PeerRunExpectation] = &[
    ordinary_peer_run("rnode-split-255", "all-corpus-cases-run"),
    ordinary_peer_run("rnode-split-256", "all-corpus-cases-run"),
    ordinary_peer_run("rnode-split-499", "all-corpus-cases-run"),
    ordinary_peer_run("rnode-exact-500", "all-corpus-cases-run"),
];
const EXPIRY_PEER_RUNS: &[PeerRunExpectation] = &[
    ordinary_peer_run("raw-orphan-split", "all-corpus-cases-run"),
    ordinary_peer_run("raw-split-replacement", "all-corpus-cases-run"),
    ordinary_peer_run("raw-nonsplit-discards-pending", "all-corpus-cases-run"),
];
const BOUNDARY_PEER_RUNS: &[PeerRunExpectation] = &[ordinary_peer_run(
    "rnode-501-through-508",
    "all-corpus-cases-run",
)];
const MALFORMED_PEER_RUNS: &[PeerRunExpectation] = &[
    ordinary_peer_run("raw-duplicate-first-half", "all-corpus-cases-run"),
    ordinary_peer_run("raw-reordered-same-sequence", "all-corpus-cases-run"),
    ordinary_peer_run("released-python-announce", "all-corpus-cases-run"),
    ordinary_peer_run("released-python-announce-duplicate", "all-corpus-cases-run"),
    ordinary_peer_run("rnode-exact-500", "all-output-actions-suppressed"),
    PeerRunExpectation {
        name: "boot-local-data",
        count: 1,
        target_mode: NORMAL_TARGET_MODE,
        artifact_id: "normal",
        binding_check: "all-corpus-cases-run",
        corpus_kind: PeerCorpusKind::BootLocal,
    },
];
const BACKPRESSURE_PEER_RUNS: &[PeerRunExpectation] = &[PeerRunExpectation {
    name: "raw-backpressure-four-frame",
    count: 1,
    target_mode: BACKPRESSURE_TARGET_MODE,
    artifact_id: "backpressure",
    binding_check: "feature-bound-corpus-run",
    corpus_kind: PeerCorpusKind::Pinned,
}];
const RETURNED_FAULT_PEER_RUNS: &[PeerRunExpectation] = &[
    PeerRunExpectation {
        name: "raw-returned-fault-trigger",
        count: 1,
        target_mode: RETURNED_FAULT_ONE_BOOT_TARGET_MODE,
        artifact_id: "returned-fault-one-boot",
        binding_check: "one-boot-fault-trace",
        corpus_kind: PeerCorpusKind::Pinned,
    },
    PeerRunExpectation {
        name: "raw-returned-fault-repeat-until-quarantine",
        count: 3,
        target_mode: RETURNED_FAULT_REPEAT_TARGET_MODE,
        artifact_id: "returned-fault-repeat-until-quarantine",
        binding_check: "repeat-policy-three-fault-quarantine",
        corpus_kind: PeerCorpusKind::Pinned,
    },
];
const SOAK_MINIMUM_PEER_RUNS: &[PeerRunExpectation] = &[
    ordinary_peer_run("raw-single-1", "mixed-valid-and-hostile-traffic"),
    ordinary_peer_run("rnode-split-256", "mixed-valid-and-hostile-traffic"),
    ordinary_peer_run(
        "raw-duplicate-first-half",
        "mixed-valid-and-hostile-traffic",
    ),
];

const fn ordinary_peer_run(name: &'static str, binding_check: &'static str) -> PeerRunExpectation {
    PeerRunExpectation {
        name,
        count: 1,
        target_mode: NORMAL_TARGET_MODE,
        artifact_id: "normal",
        binding_check,
        corpus_kind: PeerCorpusKind::Pinned,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactUse {
    artifact_id: String,
    declared_mode: String,
    observed_mode: String,
    flash_readback_path: String,
    flash_readback_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum CheckObservation {
    ElapsedSeconds {
        seconds: u64,
    },
    ConfiguredStallMicroseconds {
        microseconds: u64,
    },
    BackpressureCounters {
        offered_during_stall: u64,
        queued_during_stall: u64,
        dropped_during_stall: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CheckRecord {
    status: ResultStatus,
    evidence_files: Vec<String>,
    observation: Option<CheckObservation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ScenarioRecord {
    schema: String,
    scenario_id: String,
    title: String,
    status: ResultStatus,
    started_utc: Option<String>,
    finished_utc: Option<String>,
    reason: String,
    board_sample_ids: Vec<String>,
    artifact_uses: Vec<ArtifactUse>,
    evidence_files: Vec<String>,
    serial_capture_files: Vec<String>,
    peer_capture_files: Vec<String>,
    logic_analyzer_capture_files: Vec<String>,
    rf_observer_capture_files: Vec<String>,
    current_measurement_files: Vec<String>,
    checks: BTreeMap<String, CheckRecord>,
}

pub(crate) fn run(args: Vec<String>, root: &Path) -> ExitCode {
    let result = match parse_cli(args) {
        Ok(Cli::Init {
            normal_pressure_bundle,
            closure_bundle,
            output,
        }) => init(root, &normal_pressure_bundle, &closure_bundle, &output),
        Ok(Cli::Finalize { evidence }) => finalize(root, &evidence),
        Ok(Cli::Verify { evidence }) => verify(root, &evidence),
        Err(error) => Err(error),
    };
    match result {
        Ok(status) => {
            println!("qualification_status={}", status.as_str());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}

fn usage() -> &'static str {
    "usage:\n  cargo run --locked -p xtask -- phase1-rx-powered-evidence init --normal-pressure-bundle <directory> --closure-bundle <directory> --output <absent-directory>\n  cargo run --locked -p xtask -- phase1-rx-powered-evidence finalize --evidence <directory>\n  cargo run --locked -p xtask -- phase1-rx-powered-evidence verify --evidence <directory>\n\nThis command never accepts a serial port and never invokes a hardware operation."
}

fn parse_cli(args: Vec<String>) -> Result<Cli, String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err("missing phase1-rx-powered-evidence subcommand".to_owned());
    };
    let flags = parse_flags(&args[1..])?;
    match subcommand {
        "init" => {
            require_exact_flags(
                &flags,
                &["--closure-bundle", "--normal-pressure-bundle", "--output"],
            )?;
            Ok(Cli::Init {
                normal_pressure_bundle: PathBuf::from(&flags["--normal-pressure-bundle"]),
                closure_bundle: PathBuf::from(&flags["--closure-bundle"]),
                output: PathBuf::from(&flags["--output"]),
            })
        }
        "finalize" => {
            require_exact_flags(&flags, &["--evidence"])?;
            Ok(Cli::Finalize {
                evidence: PathBuf::from(&flags["--evidence"]),
            })
        }
        "verify" => {
            require_exact_flags(&flags, &["--evidence"])?;
            Ok(Cli::Verify {
                evidence: PathBuf::from(&flags["--evidence"]),
            })
        }
        _ => Err(format!(
            "unknown phase1-rx-powered-evidence subcommand {subcommand:?}"
        )),
    }
}

fn parse_flags(args: &[String]) -> Result<BTreeMap<String, String>, String> {
    if !args.len().is_multiple_of(2) {
        return Err("every option requires exactly one value".to_owned());
    }
    let mut flags = BTreeMap::new();
    for pair in args.chunks_exact(2) {
        if !pair[0].starts_with("--") {
            return Err(format!("unexpected positional argument {:?}", pair[0]));
        }
        if pair[1].is_empty() || pair[1].starts_with("--") {
            return Err(format!("option {} requires a non-empty value", pair[0]));
        }
        if flags.insert(pair[0].clone(), pair[1].clone()).is_some() {
            return Err(format!("duplicate option {}", pair[0]));
        }
    }
    Ok(flags)
}

fn require_exact_flags(flags: &BTreeMap<String, String>, expected: &[&str]) -> Result<(), String> {
    let actual = flags.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "wrong options: expected {}, got {}",
            expected.into_iter().collect::<Vec<_>>().join(", "),
            actual.into_iter().collect::<Vec<_>>().join(", ")
        ))
    }
}

fn init(
    root: &Path,
    normal_pressure_arg: &Path,
    closure_arg: &Path,
    output_arg: &Path,
) -> Result<ResultStatus, String> {
    let normal_pressure_path = canonical_bundle_path(root, normal_pressure_arg)?;
    let closure_path = canonical_bundle_path(root, closure_arg)?;
    if normal_pressure_path == closure_path {
        return Err("normal/pressure and closure bundles must be distinct".to_owned());
    }

    let normal_binding = phase1_hil::verified_bundle_binding(root, &normal_pressure_path)?;
    let closure_binding = phase1_closure::verified_bundle_binding(root, &closure_path)?;
    if normal_binding.git_commit != closure_binding.git_commit {
        return Err(format!(
            "bundle Git commits differ: normal/pressure={}, closure={}",
            normal_binding.git_commit, closure_binding.git_commit
        ));
    }
    if normal_binding.git_root_tree != closure_binding.git_root_tree {
        return Err(format!(
            "bundle Git root trees differ: normal/pressure={}, closure={}",
            normal_binding.git_root_tree, closure_binding.git_root_tree
        ));
    }
    if normal_binding.profile_environment != closure_binding.profile_environment {
        return Err("bundle radio profiles differ".to_owned());
    }

    let output = resolve_absent_output(root, output_arg)?;
    if output.starts_with(&normal_pressure_path)
        || output.starts_with(&closure_path)
        || normal_pressure_path.starts_with(&output)
        || closure_path.starts_with(&output)
    {
        return Err("powered-evidence output must not overlap either immutable bundle".to_owned());
    }

    let manifest = EvidenceManifest {
        schema: SCHEMA.to_owned(),
        created_unix_seconds: unix_seconds_now()?,
        normal_pressure_bundle: evidence_bundle_binding(
            "normal-pressure",
            &normal_pressure_path,
            NORMAL_MANIFEST_FILE,
            normal_binding,
        )?,
        closure_bundle: evidence_bundle_binding(
            "closure",
            &closure_path,
            CLOSURE_MANIFEST_FILE,
            closure_binding,
        )?,
        required_scenarios: SCENARIOS
            .iter()
            .map(|scenario| scenario.id.to_owned())
            .collect(),
    };

    let parent = output.parent().ok_or_else(|| {
        format!(
            "powered-evidence output has no parent: {}",
            output.display()
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "could not create powered-evidence parent {}: {error}",
            parent.display()
        )
    })?;
    fs::create_dir(&output).map_err(|error| {
        format!(
            "could not create powered-evidence output {}: {error}",
            output.display()
        )
    })?;
    let created = fs::canonicalize(&output).map_err(|error| {
        format!(
            "could not canonicalize created powered-evidence output {}: {error}",
            output.display()
        )
    })?;
    if created != output {
        return Err(format!(
            "powered-evidence output changed through a filesystem alias: expected {}, found {}",
            output.display(),
            created.display()
        ));
    }

    write_new(&output.join(INCOMPLETE_FILE), INCOMPLETE_CONTENT.as_bytes())?;
    fs::create_dir(output.join("records"))
        .map_err(|error| format!("could not create records directory: {error}"))?;
    fs::create_dir(output.join(SCENARIO_DIRECTORY))
        .map_err(|error| format!("could not create scenario-record directory: {error}"))?;
    fs::create_dir(output.join(CAPTURE_DIRECTORY))
        .map_err(|error| format!("could not create capture directory: {error}"))?;

    write_json_new(&output.join(MANIFEST_FILE), &manifest)?;
    write_json_new(&output.join(OPERATOR_FILE), &operator_template())?;
    for scenario in SCENARIOS {
        write_json_new(
            &output.join(scenario_record_path(scenario.id)),
            &scenario_template(scenario),
        )?;
    }
    validate_evidence_tree(&output, Lifecycle::Incomplete, false)?;
    println!(
        "initialized incomplete Phase-1 powered-evidence directory {}",
        output.display()
    );
    Ok(ResultStatus::NotRun)
}

fn finalize(root: &Path, evidence_arg: &Path) -> Result<ResultStatus, String> {
    let evidence = resolve_existing_evidence(root, evidence_arg)?;
    let _finalize_lock = FinalizeLock::acquire(&evidence)?;
    match prepare_finalize_lifecycle(&evidence)? {
        FinalizeLifecycle::Sealed => {
            let verified = verify_at(root, &evidence)?;
            println!(
                "Phase-1 powered evidence was already sealed; verified {}",
                evidence.display()
            );
            return Ok(verified);
        }
        FinalizeLifecycle::Incomplete => {}
    }

    validate_powered_evidence_payload(root, &evidence)?;
    install_inventory_atomically(&evidence)?;
    validate_evidence_tree(&evidence, Lifecycle::Incomplete, true)?;
    verify_inventory(&evidence)?;

    let status = validate_powered_evidence_payload(root, &evidence)?;

    validate_evidence_tree(&evidence, Lifecycle::Incomplete, true)?;
    verify_inventory(&evidence)?;

    let sealed = sealed_content(&evidence, status)?;
    stage_seal_in_incomplete_marker(&evidence, sealed.as_bytes())?;
    commit_staged_seal(&evidence, sealed.as_bytes())?;
    let verified = verify_at(root, &evidence)?;
    println!("sealed Phase-1 powered evidence {}", evidence.display());
    Ok(verified)
}

fn verify(root: &Path, evidence_arg: &Path) -> Result<ResultStatus, String> {
    let evidence = resolve_existing_evidence(root, evidence_arg)?;
    verify_at(root, &evidence)
}

fn verify_at(root: &Path, evidence: &Path) -> Result<ResultStatus, String> {
    validate_evidence_tree(evidence, Lifecycle::Sealed, true)?;
    verify_inventory(evidence)?;
    let status = validate_powered_evidence_payload(root, evidence)?;
    let expected_sealed = sealed_content(evidence, status)?;
    let actual_sealed = fs::read_to_string(evidence.join(SEALED_FILE))
        .map_err(|error| format!("could not read powered-evidence seal: {error}"))?;
    if actual_sealed != expected_sealed {
        return Err("powered-evidence seal does not match the verified records".to_owned());
    }
    validate_evidence_tree(evidence, Lifecycle::Sealed, true)?;
    verify_inventory(evidence)?;
    println!(
        "ok: verified sealed Phase-1 powered evidence {}",
        evidence.display()
    );
    Ok(status)
}

fn validate_powered_evidence_payload(root: &Path, evidence: &Path) -> Result<ResultStatus, String> {
    let manifest = verify_manifest_and_bundles(root, evidence)?;
    let (operator, records, status) = load_and_validate_records(evidence, &manifest)?;
    validate_operator(
        &operator,
        status,
        records
            .iter()
            .any(|record| record.status == ResultStatus::Pass),
        records.iter().any(|record| {
            record.status == ResultStatus::Pass && scenario_requires_peer(&record.scenario_id)
        }),
    )?;
    validate_peer_provenance(evidence, &manifest, &operator, &records)?;
    validate_record_board_ids(&operator, &records)?;
    Ok(status)
}

fn canonical_bundle_path(root: &Path, argument: &Path) -> Result<PathBuf, String> {
    let path = absolute_from(root, argument);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("could not inspect bundle {}: {error}", path.display()))?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "bundle is not a real directory: {}",
            path.display()
        ));
    }
    fs::canonicalize(&path)
        .map_err(|error| format!("could not canonicalize bundle {}: {error}", path.display()))
}

fn resolve_absent_output(root: &Path, argument: &Path) -> Result<PathBuf, String> {
    let output = absolute_from(root, argument);
    phase1_source::ensure_output_location_does_not_dirty_source(root, &output)?;
    if fs::symlink_metadata(&output).is_ok() {
        return Err(format!(
            "powered-evidence output already exists and will not be overwritten: {}",
            output.display()
        ));
    }
    resolve_nonexistent_path(&output)
}

fn resolve_nonexistent_path(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path;
    let mut suffix = Vec::new();
    while fs::symlink_metadata(existing).is_err() {
        let name = existing
            .file_name()
            .ok_or_else(|| format!("could not find an existing ancestor for {}", path.display()))?;
        suffix.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| format!("path has no existing ancestor: {}", path.display()))?;
    }
    let metadata = fs::metadata(existing)
        .map_err(|error| format!("could not inspect {}: {error}", existing.display()))?;
    if !metadata.is_dir() {
        return Err(format!(
            "powered-evidence output ancestor is not a directory: {}",
            existing.display()
        ));
    }
    let mut resolved = fs::canonicalize(existing)
        .map_err(|error| format!("could not canonicalize {}: {error}", existing.display()))?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn resolve_existing_evidence(root: &Path, argument: &Path) -> Result<PathBuf, String> {
    let path = absolute_from(root, argument);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        format!(
            "could not inspect powered-evidence directory {}: {error}",
            path.display()
        )
    })?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "powered-evidence path is not a real directory: {}",
            path.display()
        ));
    }
    fs::canonicalize(&path).map_err(|error| {
        format!(
            "could not canonicalize powered-evidence directory {}: {error}",
            path.display()
        )
    })
}

fn evidence_bundle_binding(
    kind: &str,
    path: &Path,
    manifest_file: &str,
    binding: phase1_hil::VerifiedBundleBinding,
) -> Result<EvidenceBundleBinding, String> {
    let canonical_path = path
        .to_str()
        .ok_or_else(|| format!("bundle path is not UTF-8: {}", path.display()))?
        .to_owned();
    let artifacts = binding
        .artifacts
        .into_iter()
        .map(|artifact| EvidenceArtifactBinding {
            id: artifact.id,
            mode: artifact.mode,
            elf: EvidenceFileBinding {
                path: artifact.elf.path,
                sha256: artifact.elf.sha256,
                bytes: artifact.elf.bytes,
            },
            flash_image: EvidenceFileBinding {
                path: artifact.flash_image.path,
                sha256: artifact.flash_image.sha256,
                bytes: artifact.flash_image.bytes,
            },
        })
        .collect();
    Ok(EvidenceBundleBinding {
        kind: kind.to_owned(),
        schema: binding.schema,
        canonical_path,
        manifest_file: manifest_file.to_owned(),
        manifest_sha256: sha256_file(&path.join(manifest_file))?,
        git_commit: binding.git_commit,
        git_root_tree: binding.git_root_tree,
        profile_environment: binding.profile_environment,
        artifacts,
    })
}

fn verify_manifest_and_bundles(root: &Path, evidence: &Path) -> Result<EvidenceManifest, String> {
    let manifest: EvidenceManifest = read_json(&evidence.join(MANIFEST_FILE))?;
    if manifest.schema != SCHEMA {
        return Err(format!(
            "unsupported powered-evidence schema {:?}",
            manifest.schema
        ));
    }
    if manifest.created_unix_seconds == 0 {
        return Err("powered-evidence creation time must be non-zero".to_owned());
    }
    let expected_scenarios = SCENARIOS
        .iter()
        .map(|scenario| scenario.id.to_owned())
        .collect::<Vec<_>>();
    if manifest.required_scenarios != expected_scenarios {
        return Err("powered-evidence manifest scenario inventory changed".to_owned());
    }

    let normal_path = canonical_manifest_bundle_path(&manifest.normal_pressure_bundle)?;
    let closure_path = canonical_manifest_bundle_path(&manifest.closure_bundle)?;
    if normal_path == closure_path {
        return Err("powered-evidence manifest aliases both source bundles".to_owned());
    }
    if evidence.starts_with(&normal_path)
        || evidence.starts_with(&closure_path)
        || normal_path.starts_with(evidence)
        || closure_path.starts_with(evidence)
    {
        return Err("powered-evidence directory overlaps an immutable bundle".to_owned());
    }
    let normal = evidence_bundle_binding(
        "normal-pressure",
        &normal_path,
        NORMAL_MANIFEST_FILE,
        phase1_hil::verified_bundle_binding(root, &normal_path)?,
    )?;
    let closure = evidence_bundle_binding(
        "closure",
        &closure_path,
        CLOSURE_MANIFEST_FILE,
        phase1_closure::verified_bundle_binding(root, &closure_path)?,
    )?;
    if manifest.normal_pressure_bundle != normal || manifest.closure_bundle != closure {
        return Err(
            "powered-evidence bundle binding no longer matches its source bundle".to_owned(),
        );
    }
    if normal.git_commit != closure.git_commit {
        return Err("powered-evidence bundles record different Git commits".to_owned());
    }
    if normal.git_root_tree != closure.git_root_tree {
        return Err("powered-evidence bundles record different Git root trees".to_owned());
    }
    if normal.profile_environment != closure.profile_environment {
        return Err("powered-evidence bundles record different radio profiles".to_owned());
    }
    Ok(manifest)
}

fn canonical_manifest_bundle_path(binding: &EvidenceBundleBinding) -> Result<PathBuf, String> {
    let path = PathBuf::from(&binding.canonical_path);
    if !path.is_absolute() {
        return Err(format!(
            "bundle binding path is not absolute: {:?}",
            binding.canonical_path
        ));
    }
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("could not inspect bound bundle {}: {error}", path.display()))?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "bound bundle is not a real directory: {}",
            path.display()
        ));
    }
    let canonical = fs::canonicalize(&path)
        .map_err(|error| format!("could not canonicalize bound bundle: {error}"))?;
    if canonical != path {
        return Err(format!(
            "bound bundle path is not canonical: {}",
            path.display()
        ));
    }
    Ok(path)
}

fn operator_template() -> OperatorRecord {
    OperatorRecord {
        schema: OPERATOR_SCHEMA.to_owned(),
        operator: String::new(),
        started_utc: None,
        finished_utc: None,
        board_revision: "2.3".to_owned(),
        rf_variant_hz: "863000000-928000000".to_owned(),
        board_sample_ids: Vec::new(),
        tracker_usb_identity: String::new(),
        peer_id: String::new(),
        peer_usb_identity: String::new(),
        peer_firmware_version: PEER_FIRMWARE_VERSION.to_owned(),
        peer_firmware_revision: PEER_FIRMWARE_REVISION.to_owned(),
        peer_firmware_image_path: String::new(),
        peer_firmware_sha256: String::new(),
        peer_firmware_source_path: String::new(),
        peer_firmware_source_sha256: String::new(),
        peer_corpus_path: String::new(),
        peer_corpus_sha256: String::new(),
        peer_tool_path: String::new(),
        peer_tool_sha256: String::new(),
        peer_conducted_power_dbm: None,
        peer_short_airtime_limit_basis_points: None,
        peer_long_airtime_limit_basis_points: None,
        peer_effective_short_airtime_limit_basis_points: None,
        peer_effective_long_airtime_limit_basis_points: None,
        peer_reported_preamble_symbols: None,
        transmit_authorization: String::new(),
        observer_id: String::new(),
        observer_bandwidth_hz: None,
        observer_noise_floor_dbm: None,
        observer_detection_threshold_dbm: None,
        observer_attribution_setup: String::new(),
        logic_analyzer: String::new(),
        rf_observer: String::new(),
        current_instrument: String::new(),
        antenna_or_load: String::new(),
        pa_cps_probe: "not-qualified".to_owned(),
        region_basis: String::new(),
        notes: String::new(),
    }
}

fn scenario_template(definition: &ScenarioDefinition) -> ScenarioRecord {
    ScenarioRecord {
        schema: SCENARIO_SCHEMA.to_owned(),
        scenario_id: definition.id.to_owned(),
        title: definition.title.to_owned(),
        status: ResultStatus::NotRun,
        started_utc: None,
        finished_utc: None,
        reason: "not run".to_owned(),
        board_sample_ids: Vec::new(),
        artifact_uses: Vec::new(),
        evidence_files: Vec::new(),
        serial_capture_files: Vec::new(),
        peer_capture_files: Vec::new(),
        logic_analyzer_capture_files: Vec::new(),
        rf_observer_capture_files: Vec::new(),
        current_measurement_files: Vec::new(),
        checks: expected_checks(definition)
            .into_iter()
            .map(|check| {
                (
                    check,
                    CheckRecord {
                        status: ResultStatus::NotRun,
                        evidence_files: Vec::new(),
                        observation: None,
                    },
                )
            })
            .collect(),
    }
}

fn expected_checks(definition: &ScenarioDefinition) -> BTreeSet<String> {
    COMMON_CHECKS
        .iter()
        .chain(definition.checks.iter())
        .map(|check| (*check).to_owned())
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EvidenceRole {
    ArtifactReadback,
    Serial,
    Peer,
    LogicAnalyzer,
    RfObserver,
    CurrentMeasurement,
}

const ARTIFACT_AND_SERIAL_ROLES: &[EvidenceRole] =
    &[EvidenceRole::ArtifactReadback, EvidenceRole::Serial];
const SERIAL_ROLE: &[EvidenceRole] = &[EvidenceRole::Serial];
const SERIAL_AND_PEER_ROLES: &[EvidenceRole] = &[EvidenceRole::Serial, EvidenceRole::Peer];
const SERIAL_AND_LOGIC_ANALYZER_ROLES: &[EvidenceRole] =
    &[EvidenceRole::Serial, EvidenceRole::LogicAnalyzer];
const SERIAL_PEER_AND_LOGIC_ANALYZER_ROLES: &[EvidenceRole] = &[
    EvidenceRole::Serial,
    EvidenceRole::Peer,
    EvidenceRole::LogicAnalyzer,
];
const LOGIC_ANALYZER_ROLE: &[EvidenceRole] = &[EvidenceRole::LogicAnalyzer];
const RF_OBSERVER_ROLE: &[EvidenceRole] = &[EvidenceRole::RfObserver];
const CURRENT_MEASUREMENT_ROLE: &[EvidenceRole] = &[EvidenceRole::CurrentMeasurement];
const CURRENT_AND_LOGIC_ANALYZER_ROLES: &[EvidenceRole] = &[
    EvidenceRole::CurrentMeasurement,
    EvidenceRole::LogicAnalyzer,
];
const SOAK_DURATION_ROLES: &[EvidenceRole] = &[EvidenceRole::Serial, EvidenceRole::RfObserver];
const NO_REQUIRED_ROLES: &[EvidenceRole] = &[];

fn required_evidence_roles(check_id: &str) -> &'static [EvidenceRole] {
    match check_id {
        "artifact-mode-and-readback-bound" => ARTIFACT_AND_SERIAL_ROLES,
        "independent-rf-observer-present" | "no-tracker-originated-rf" => RF_OBSERVER_ROLE,
        "logic-analyzer-capture-present" | "no-prohibited-sx1262-tx-command" => LOGIC_ANALYZER_ROLE,
        "serial-capture-present"
        | "profile-and-radio-constants"
        | "two-heartbeats"
        | "heap-stable"
        | "stack-guard-and-scan-valid"
        | "rssi-and-snr-recorded"
        | "rete-dispositions-recorded"
        | "conservative-phy-metadata"
        | "all-output-actions-suppressed"
        | "configured-stall-seven-seconds"
        | "healthy-lease-qualified"
        | "corrupt-or-torn-detected"
        | "stack-headroom-stable"
        | "rete-maintenance-continues" => SERIAL_ROLE,
        "all-corpus-cases-run"
        | "physical-counters-and-lengths"
        | "raw-packet-digests-match"
        | "first-half-pending"
        | "reassembled-lengths-match"
        | "pending-expired-delta"
        | "pending-replaced-delta"
        | "no-cross-packet-splice"
        | "hardware-mtu-frames-observed"
        | "packets-too-long-delta"
        | "no-rete-ingress-for-oversize"
        | "duplicate-and-reorder-rejected"
        | "announce-dedup-correct"
        | "boot-local-data-processed"
        | "feature-bound-corpus-run"
        | "offered-three-queued-two-dropped-one"
        | "expiry-before-queued-service"
        | "queued-frames-rejected-by-watermark"
        | "no-completed-packet-or-rete-ingress"
        | "mixed-valid-and-hostile-traffic" => SERIAL_AND_PEER_ROLES,
        "inert-pin-order" | "safety-pin-timing-measured" => LOGIC_ANALYZER_ROLE,
        "one-boot-fault-trace" | "repeat-policy-three-fault-quarantine" => {
            SERIAL_PEER_AND_LOGIC_ANALYZER_ROLES
        }
        "retained-write-failure-fails-closed"
        | "supervisor-watchdog-combined-streak"
        | "cold-power-cycle-behavior-recorded"
        | "corrupt-selector-two-boot-run"
        | "torn-selector-two-boot-run"
        | "quarantine-before-peripheral-construction" => SERIAL_AND_LOGIC_ANALYZER_ROLES,
        "all-four-selections-measured" => CURRENT_AND_LOGIC_ANALYZER_ROLES,
        "calibrated-current-measurement"
        | "more-than-one-board-sample"
        | "no-single-sample-policy-change" => CURRENT_MEASUREMENT_ROLE,
        "continuous-duration-at-least-24h" => SOAK_DURATION_ROLES,
        "gap-free-observer-index" => RF_OBSERVER_ROLE,
        _ => NO_REQUIRED_ROLES,
    }
}

fn evidence_has_role(record: &ScenarioRecord, path: &str, role: EvidenceRole) -> bool {
    match role {
        EvidenceRole::ArtifactReadback => record
            .artifact_uses
            .iter()
            .any(|artifact| artifact.flash_readback_path == path),
        EvidenceRole::Serial => record
            .serial_capture_files
            .iter()
            .any(|value| value == path),
        EvidenceRole::Peer => record.peer_capture_files.iter().any(|value| value == path),
        EvidenceRole::LogicAnalyzer => record
            .logic_analyzer_capture_files
            .iter()
            .any(|value| value == path),
        EvidenceRole::RfObserver => record
            .rf_observer_capture_files
            .iter()
            .any(|value| value == path),
        EvidenceRole::CurrentMeasurement => record
            .current_measurement_files
            .iter()
            .any(|value| value == path),
    }
}

fn evidence_is_classified(record: &ScenarioRecord, path: &str) -> bool {
    [
        EvidenceRole::ArtifactReadback,
        EvidenceRole::Serial,
        EvidenceRole::Peer,
        EvidenceRole::LogicAnalyzer,
        EvidenceRole::RfObserver,
        EvidenceRole::CurrentMeasurement,
    ]
    .into_iter()
    .any(|role| evidence_has_role(record, path, role))
}

fn validate_check_records(
    definition: &ScenarioDefinition,
    record: &ScenarioRecord,
) -> Result<(), String> {
    for (check_id, check) in &record.checks {
        ensure_unique_nonempty(
            &check.evidence_files,
            &format!("{} check {check_id} evidence paths", definition.id),
        )?;
        for path in &check.evidence_files {
            if !record.evidence_files.contains(path) {
                return Err(format!(
                    "scenario {} check {check_id} references evidence {path:?} absent from the scenario inventory",
                    definition.id
                ));
            }
            if !evidence_is_classified(record, path) {
                return Err(format!(
                    "scenario {} check {check_id} references unclassified evidence {path:?}",
                    definition.id
                ));
            }
        }

        match check.status {
            ResultStatus::Pass => {
                if check.evidence_files.is_empty() {
                    return Err(format!(
                        "passing scenario {} check {check_id} requires bound evidence",
                        definition.id
                    ));
                }
                for role in required_evidence_roles(check_id) {
                    if !check
                        .evidence_files
                        .iter()
                        .any(|path| evidence_has_role(record, path, *role))
                    {
                        return Err(format!(
                            "passing scenario {} check {check_id} lacks required {role:?} evidence",
                            definition.id
                        ));
                    }
                }
                if check_id == "artifact-mode-and-readback-bound" {
                    for artifact in &record.artifact_uses {
                        if !check.evidence_files.contains(&artifact.flash_readback_path) {
                            return Err(format!(
                                "passing scenario {} artifact check does not bind readback for {:?}",
                                definition.id, artifact.artifact_id
                            ));
                        }
                    }
                }
            }
            ResultStatus::Fail => {
                if check.evidence_files.is_empty() {
                    return Err(format!(
                        "failed scenario {} check {check_id} requires bound evidence; use not-run when no capture exists",
                        definition.id
                    ));
                }
            }
            ResultStatus::NotRun => {
                if !check.evidence_files.is_empty() || check.observation.is_some() {
                    return Err(format!(
                        "not-run scenario {} check {check_id} contains partial evidence",
                        definition.id
                    ));
                }
            }
        }
        validate_check_observation(definition.id, check_id, check)?;
    }
    Ok(())
}

fn validate_check_observation(
    scenario_id: &str,
    check_id: &str,
    check: &CheckRecord,
) -> Result<(), String> {
    match (check_id, check.observation.as_ref()) {
        (
            "continuous-duration-at-least-24h",
            Some(CheckObservation::ElapsedSeconds { seconds }),
        ) => {
            if check.status == ResultStatus::Pass && *seconds < 24 * 60 * 60 {
                return Err(format!(
                    "passing scenario {scenario_id} 24-hour soak recorded only {seconds} seconds"
                ));
            }
        }
        (
            "configured-stall-seven-seconds",
            Some(CheckObservation::ConfiguredStallMicroseconds { microseconds }),
        ) => {
            if check.status == ResultStatus::Pass && *microseconds != 7_000_000 {
                return Err(format!(
                    "passing scenario {scenario_id} configured stall is {microseconds} microseconds instead of 7000000"
                ));
            }
        }
        (
            "offered-three-queued-two-dropped-one",
            Some(CheckObservation::BackpressureCounters {
                offered_during_stall,
                queued_during_stall,
                dropped_during_stall,
            }),
        ) => {
            if check.status == ResultStatus::Pass
                && (
                    *offered_during_stall,
                    *queued_during_stall,
                    *dropped_during_stall,
                ) != (3, 2, 1)
            {
                return Err(format!(
                    "passing scenario {scenario_id} backpressure counters must be exactly offered=3 queued=2 dropped=1"
                ));
            }
        }
        (
            "continuous-duration-at-least-24h"
            | "configured-stall-seven-seconds"
            | "offered-three-queued-two-dropped-one",
            None,
        ) if check.status != ResultStatus::Pass => {}
        (
            "continuous-duration-at-least-24h"
            | "configured-stall-seven-seconds"
            | "offered-three-queued-two-dropped-one",
            None,
        ) => {
            return Err(format!(
                "passing scenario {scenario_id} check {check_id} requires a machine-readable observation"
            ));
        }
        (
            "continuous-duration-at-least-24h"
            | "configured-stall-seven-seconds"
            | "offered-three-queued-two-dropped-one",
            Some(_),
        ) => {
            return Err(format!(
                "scenario {scenario_id} check {check_id} has the wrong observation kind"
            ));
        }
        (_, Some(_)) => {
            return Err(format!(
                "scenario {scenario_id} check {check_id} does not accept a machine-readable observation"
            ));
        }
        (_, None) => {}
    }
    Ok(())
}

fn load_and_validate_records(
    evidence: &Path,
    manifest: &EvidenceManifest,
) -> Result<(OperatorRecord, Vec<ScenarioRecord>, ResultStatus), String> {
    let operator: OperatorRecord = read_json(&evidence.join(OPERATOR_FILE))?;
    let catalog = artifact_catalog(manifest)?;
    let mut records = Vec::with_capacity(SCENARIOS.len());
    for definition in SCENARIOS {
        let record: ScenarioRecord =
            read_json(&evidence.join(scenario_record_path(definition.id)))?;
        validate_scenario_record(evidence, definition, &record, &catalog)?;
        records.push(record);
    }
    let status = if records
        .iter()
        .any(|record| record.status == ResultStatus::Fail)
    {
        ResultStatus::Fail
    } else if records
        .iter()
        .all(|record| record.status == ResultStatus::Pass)
    {
        ResultStatus::Pass
    } else {
        ResultStatus::NotRun
    };
    Ok((operator, records, status))
}

fn artifact_catalog(
    manifest: &EvidenceManifest,
) -> Result<BTreeMap<String, EvidenceArtifactBinding>, String> {
    let mut catalog = BTreeMap::new();
    for artifact in manifest
        .normal_pressure_bundle
        .artifacts
        .iter()
        .chain(manifest.closure_bundle.artifacts.iter())
    {
        if catalog
            .insert(artifact.id.clone(), artifact.clone())
            .is_some()
        {
            return Err(format!("duplicate bound artifact id {:?}", artifact.id));
        }
    }
    let expected = SCENARIOS
        .iter()
        .flat_map(|scenario| scenario.artifact_ids.iter().copied())
        .collect::<BTreeSet<_>>();
    let actual = catalog.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if actual != expected {
        return Err("powered-evidence artifact inventory is incomplete or unexpected".to_owned());
    }
    Ok(catalog)
}

fn validate_scenario_record(
    evidence: &Path,
    definition: &ScenarioDefinition,
    record: &ScenarioRecord,
    catalog: &BTreeMap<String, EvidenceArtifactBinding>,
) -> Result<(), String> {
    if record.schema != SCENARIO_SCHEMA
        || record.scenario_id != definition.id
        || record.title != definition.title
    {
        return Err(format!(
            "scenario record identity changed for {}",
            definition.id
        ));
    }
    let check_names = record.checks.keys().cloned().collect::<BTreeSet<_>>();
    if check_names != expected_checks(definition) {
        return Err(format!(
            "scenario {} does not contain the exact required check inventory",
            definition.id
        ));
    }
    if record.reason.trim().is_empty() || record.reason.trim() != record.reason {
        return Err(format!(
            "scenario {} requires a reason without surrounding whitespace",
            definition.id
        ));
    }
    if record.status != ResultStatus::NotRun && record.reason == "not run" {
        return Err(format!(
            "scenario {} must replace the template reason before recording {}",
            definition.id,
            record.status.as_str()
        ));
    }
    ensure_unique_nonempty(
        &record.board_sample_ids,
        &format!("{} board sample ids", definition.id),
    )?;
    ensure_unique_nonempty(
        &record.evidence_files,
        &format!("{} evidence paths", definition.id),
    )?;
    for (paths, name) in [
        (&record.serial_capture_files, "serial capture paths"),
        (&record.peer_capture_files, "peer capture paths"),
        (
            &record.logic_analyzer_capture_files,
            "logic-analyzer capture paths",
        ),
        (
            &record.rf_observer_capture_files,
            "RF-observer capture paths",
        ),
        (
            &record.current_measurement_files,
            "current-measurement paths",
        ),
    ] {
        ensure_unique_nonempty(paths, &format!("{} {name}", definition.id))?;
        for path in paths {
            if !record.evidence_files.contains(path) {
                return Err(format!(
                    "scenario {} typed {name} entry {path:?} is absent from evidence_files",
                    definition.id
                ));
            }
        }
    }

    for path in &record.evidence_files {
        validate_capture_file(evidence, path, record.status != ResultStatus::NotRun)?;
    }
    let mut used = BTreeSet::new();
    for artifact_use in &record.artifact_uses {
        if !used.insert(artifact_use.artifact_id.as_str()) {
            return Err(format!(
                "scenario {} repeats artifact {:?}",
                definition.id, artifact_use.artifact_id
            ));
        }
        validate_artifact_use(evidence, record, artifact_use, catalog)?;
    }
    validate_check_records(definition, record)?;

    match record.status {
        ResultStatus::Pass => {
            validate_time_pair(
                record.started_utc.as_deref(),
                record.finished_utc.as_deref(),
                definition.id,
            )?;
            if record.board_sample_ids.is_empty() || record.evidence_files.is_empty() {
                return Err(format!(
                    "passing scenario {} requires board and capture evidence",
                    definition.id
                ));
            }
            if record.serial_capture_files.is_empty()
                || record.logic_analyzer_capture_files.is_empty()
                || record.rf_observer_capture_files.is_empty()
            {
                return Err(format!(
                    "passing scenario {} requires typed serial, analyzer and observer captures",
                    definition.id
                ));
            }
            if scenario_requires_peer(definition.id) && record.peer_capture_files.is_empty() {
                return Err(format!(
                    "passing scenario {} requires typed peer manifest/transcript evidence",
                    definition.id
                ));
            }
            if matches!(
                definition.id,
                "cold-boot-and-silence" | "electrical-matrix" | "receive-soak-24h"
            ) && record.current_measurement_files.is_empty()
            {
                return Err(format!(
                    "passing scenario {} requires typed current-measurement evidence",
                    definition.id
                ));
            }
            let expected_artifacts = definition
                .artifact_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            if used != expected_artifacts {
                return Err(format!(
                    "passing scenario {} does not bind every required artifact",
                    definition.id
                ));
            }
            if record
                .checks
                .values()
                .any(|check| check.status != ResultStatus::Pass)
            {
                return Err(format!(
                    "partial checks cannot make scenario {} pass",
                    definition.id
                ));
            }
        }
        ResultStatus::Fail => {
            validate_time_pair(
                record.started_utc.as_deref(),
                record.finished_utc.as_deref(),
                definition.id,
            )?;
            if record.board_sample_ids.is_empty() {
                return Err(format!(
                    "failed scenario {} must identify the board sample",
                    definition.id
                ));
            }
            if !record
                .checks
                .values()
                .any(|check| check.status == ResultStatus::Fail)
            {
                return Err(format!(
                    "failed scenario {} must identify at least one failed check",
                    definition.id
                ));
            }
        }
        ResultStatus::NotRun => {
            if record.started_utc.is_some()
                || record.finished_utc.is_some()
                || !record.board_sample_ids.is_empty()
                || !record.artifact_uses.is_empty()
                || !record.evidence_files.is_empty()
                || !record.serial_capture_files.is_empty()
                || !record.peer_capture_files.is_empty()
                || !record.logic_analyzer_capture_files.is_empty()
                || !record.rf_observer_capture_files.is_empty()
                || !record.current_measurement_files.is_empty()
                || record
                    .checks
                    .values()
                    .any(|check| check.status != ResultStatus::NotRun)
            {
                return Err(format!(
                    "not-run scenario {} contains partial run evidence",
                    definition.id
                ));
            }
        }
    }
    Ok(())
}

fn validate_artifact_use(
    evidence: &Path,
    record: &ScenarioRecord,
    artifact_use: &ArtifactUse,
    catalog: &BTreeMap<String, EvidenceArtifactBinding>,
) -> Result<(), String> {
    let artifact = catalog.get(&artifact_use.artifact_id).ok_or_else(|| {
        format!(
            "scenario {} uses unknown artifact {:?}",
            record.scenario_id, artifact_use.artifact_id
        )
    })?;
    if artifact_use.declared_mode != artifact.mode || artifact_use.observed_mode != artifact.mode {
        return Err(format!(
            "scenario {} artifact mode does not match bound mode {:?}",
            record.scenario_id, artifact.mode
        ));
    }
    if !record
        .evidence_files
        .contains(&artifact_use.flash_readback_path)
    {
        return Err(format!(
            "scenario {} readback is not listed as evidence",
            record.scenario_id
        ));
    }
    validate_sha256(&artifact_use.flash_readback_sha256)?;
    let path = resolve_capture_path(evidence, &artifact_use.flash_readback_path)?;
    let actual = sha256_file(&path)?;
    if actual != artifact_use.flash_readback_sha256 || actual != artifact.flash_image.sha256 {
        return Err(format!(
            "scenario {} flash readback does not match the bound image",
            record.scenario_id
        ));
    }
    Ok(())
}

fn validate_operator(
    operator: &OperatorRecord,
    status: ResultStatus,
    any_scenario_passes: bool,
    any_peer_scenario_passes: bool,
) -> Result<(), String> {
    if operator.schema != OPERATOR_SCHEMA {
        return Err(format!(
            "unsupported powered-evidence operator schema {:?}",
            operator.schema
        ));
    }
    validate_time_pair(
        operator.started_utc.as_deref(),
        operator.finished_utc.as_deref(),
        "operator record",
    )?;
    require_nonempty(&operator.operator, "operator")?;
    if operator.board_revision != "2.3" {
        return Err("operator record must identify Tracker board revision 2.3".to_owned());
    }
    if operator.rf_variant_hz != "863000000-928000000" {
        return Err("operator record must identify the 863-928 MHz RF variant".to_owned());
    }
    ensure_unique_nonempty(&operator.board_sample_ids, "operator board sample ids")?;
    if operator.board_sample_ids.is_empty() {
        return Err("operator record must identify at least one board sample".to_owned());
    }
    if operator.peer_firmware_version != PEER_FIRMWARE_VERSION
        || operator.peer_firmware_revision != PEER_FIRMWARE_REVISION
    {
        return Err("operator record does not bind the pinned RNode firmware".to_owned());
    }
    if any_scenario_passes {
        for (value, name) in [
            (&operator.tracker_usb_identity, "tracker USB identity"),
            (&operator.observer_id, "observer id"),
            (
                &operator.observer_attribution_setup,
                "observer attribution setup",
            ),
            (&operator.logic_analyzer, "logic analyzer"),
            (&operator.rf_observer, "RF observer"),
            (&operator.current_instrument, "current instrument"),
            (&operator.antenna_or_load, "antenna or load"),
            (&operator.pa_cps_probe, "PA_CPS probe"),
            (&operator.region_basis, "region basis"),
            (&operator.notes, "operator notes"),
        ] {
            require_nonempty(value, name)?;
        }
        if operator.observer_bandwidth_hz == Some(0)
            || operator.observer_bandwidth_hz.is_none()
            || operator.observer_noise_floor_dbm.is_none()
            || operator.observer_detection_threshold_dbm.is_none()
        {
            return Err("passing qualification requires calibrated observer values".to_owned());
        }
        if operator.pa_cps_probe == "not-qualified" {
            return Err("passing scenario requires an actual PA_CPS probe record".to_owned());
        }
    }
    if any_peer_scenario_passes {
        for (value, name) in [
            (&operator.peer_id, "peer id"),
            (&operator.peer_usb_identity, "peer USB identity"),
            (
                &operator.peer_firmware_image_path,
                "preserved peer firmware image path",
            ),
            (
                &operator.peer_firmware_source_path,
                "preserved peer firmware source path",
            ),
            (&operator.peer_corpus_path, "preserved peer corpus path"),
            (&operator.peer_tool_path, "preserved peer tool path"),
            (&operator.transmit_authorization, "transmit authorization"),
        ] {
            require_nonempty(value, name)?;
        }
        for digest in [
            &operator.peer_firmware_sha256,
            &operator.peer_firmware_source_sha256,
            &operator.peer_corpus_sha256,
            &operator.peer_tool_sha256,
        ] {
            validate_sha256(digest)?;
        }
        validate_peer_rf_authorization(operator)?;
    }
    if status == ResultStatus::Pass && operator.board_sample_ids.len() < 2 {
        return Err("passing qualification requires more than one board sample".to_owned());
    }
    Ok(())
}

fn validate_peer_rf_authorization(operator: &OperatorRecord) -> Result<(), String> {
    let power = operator
        .peer_conducted_power_dbm
        .ok_or_else(|| "passing peer scenario requires conducted TX power".to_owned())?;
    let short = operator
        .peer_short_airtime_limit_basis_points
        .ok_or_else(|| "passing peer scenario requires a short airtime limit".to_owned())?;
    let long = operator
        .peer_long_airtime_limit_basis_points
        .ok_or_else(|| "passing peer scenario requires a long airtime limit".to_owned())?;
    let effective_short = operator
        .peer_effective_short_airtime_limit_basis_points
        .ok_or_else(|| {
            "passing peer scenario requires an effective short airtime limit".to_owned()
        })?;
    let effective_long = operator
        .peer_effective_long_airtime_limit_basis_points
        .ok_or_else(|| {
            "passing peer scenario requires an effective long airtime limit".to_owned()
        })?;
    if !(0..=37).contains(&power)
        || short >= 10_000
        || long >= 10_000
        || effective_short > short
        || effective_long > long
        || short - effective_short > 1
        || long - effective_long > 1
    {
        return Err(
            "peer airtime limits must be basis points and effective echoes may be at most one point lower"
                .to_owned(),
        );
    }
    if operator
        .peer_reported_preamble_symbols
        .is_none_or(|symbols| symbols == 0)
    {
        return Err("passing peer scenario requires a non-zero reported preamble".to_owned());
    }
    Ok(())
}

fn validate_peer_provenance(
    evidence: &Path,
    evidence_manifest: &EvidenceManifest,
    operator: &OperatorRecord,
    records: &[ScenarioRecord],
) -> Result<(), String> {
    let passing_peer_records = records
        .iter()
        .filter(|record| {
            record.status == ResultStatus::Pass && scenario_requires_peer(&record.scenario_id)
        })
        .collect::<Vec<_>>();
    if passing_peer_records.is_empty() {
        return Ok(());
    }

    let project_sources = read_project_peer_sources(&evidence_manifest.normal_pressure_bundle)?;
    validate_boot_local_generator_source(&project_sources.boot_local_generator)?;
    validate_global_peer_evidence_reuse(evidence, &passing_peer_records)?;
    validate_operator_peer_artifacts(
        evidence,
        operator,
        &project_sources.corpus,
        &project_sources.tool,
    )?;
    let pinned_corpus: serde_json::Value = serde_json::from_slice(&project_sources.corpus)
        .map_err(|error| format!("could not parse archived project-owned peer corpus: {error}"))?;
    let pinned_scenarios = validate_pinned_peer_corpus(&pinned_corpus)?;
    let boot_local_generator_sha256 = sha256_bytes(&project_sources.boot_local_generator);
    let profile_environment = &evidence_manifest.normal_pressure_bundle.profile_environment;
    let context = PeerValidationContext {
        evidence,
        operator,
        profile_environment,
        pinned_corpus: &pinned_corpus,
        pinned_corpus_bytes: &project_sources.corpus,
        pinned_scenarios: &pinned_scenarios,
        boot_local_generator_sha256: &boot_local_generator_sha256,
    };
    for record in passing_peer_records {
        validate_passing_peer_record(record, &context)?;
    }
    Ok(())
}

fn validate_boot_local_generator_source(bytes: &[u8]) -> Result<(), String> {
    if bytes == boot_local_generator::SOURCE_BYTES
        && sha256_bytes(bytes) == BOOT_LOCAL_GENERATOR_SHA256_V2
    {
        Ok(())
    } else {
        Err(
            "bound boot-local generator source differs from this powered-evidence schema; bump the schema before changing its algorithm"
                .to_owned(),
        )
    }
}

#[derive(Clone, Debug)]
struct GlobalPeerEvidenceUse {
    powered_scenario_id: String,
    peer_scenario_name: String,
    path: String,
}

fn validate_global_peer_evidence_reuse(
    evidence: &Path,
    records: &[&ScenarioRecord],
) -> Result<(), String> {
    let mut manifest_digests = BTreeMap::<String, GlobalPeerEvidenceUse>::new();
    let mut transcript_digests = BTreeMap::<String, GlobalPeerEvidenceUse>::new();
    for record in records {
        for manifest_path in record
            .peer_capture_files
            .iter()
            .filter(|path| capture_file_name(path) == Some(PEER_MANIFEST_FILE))
        {
            let manifest_file = resolve_capture_path(evidence, manifest_path)?;
            let manifest: PeerManifest = read_json(&manifest_file)?;
            let peer_scenario_name = peer_scenario_name(&manifest.scenario)?.to_owned();
            let current = GlobalPeerEvidenceUse {
                powered_scenario_id: record.scenario_id.clone(),
                peer_scenario_name,
                path: manifest_path.clone(),
            };
            record_global_peer_digest(
                &mut manifest_digests,
                sha256_file(&manifest_file)?,
                &current,
                "manifest",
            )?;
            let transcript_path = sibling_capture_path(manifest_path, PEER_TRANSCRIPT_FILE)?;
            let transcript_file = resolve_capture_path(evidence, &transcript_path)?;
            let transcript_use = GlobalPeerEvidenceUse {
                path: transcript_path,
                ..current
            };
            record_global_peer_digest(
                &mut transcript_digests,
                sha256_file(&transcript_file)?,
                &transcript_use,
                "transcript",
            )?;
        }
    }
    Ok(())
}

fn record_global_peer_digest(
    observed: &mut BTreeMap<String, GlobalPeerEvidenceUse>,
    digest: String,
    current: &GlobalPeerEvidenceUse,
    kind: &str,
) -> Result<(), String> {
    let Some(previous) = observed.get(&digest) else {
        observed.insert(digest, current.clone());
        return Ok(());
    };
    let split_and_malformed =
        BTreeSet::from([
            previous.powered_scenario_id.as_str(),
            current.powered_scenario_id.as_str(),
        ]) == BTreeSet::from(["malformed-and-semantic-rejection", "split-packet"]);
    if split_and_malformed
        && previous.peer_scenario_name == "rnode-exact-500"
        && current.peer_scenario_name == "rnode-exact-500"
        && previous.path == current.path
    {
        return Ok(());
    }
    Err(format!(
        "peer {kind} digest {digest} is reused by powered scenarios {:?} and {:?}",
        previous.powered_scenario_id, current.powered_scenario_id
    ))
}

fn validate_operator_peer_artifacts(
    evidence: &Path,
    operator: &OperatorRecord,
    project_corpus: &[u8],
    project_tool: &[u8],
) -> Result<(), String> {
    validate_operator_peer_artifact_hashes(evidence, operator, project_corpus, project_tool)?;
    let peer_source = resolve_capture_path(evidence, &operator.peer_firmware_source_path)?;
    verify_peer_firmware_source_bundle(
        &peer_source,
        PEER_FIRMWARE_REVISION,
        PEER_FIRMWARE_ROOT_TREE,
    )
}

fn validate_operator_peer_artifact_hashes(
    evidence: &Path,
    operator: &OperatorRecord,
    project_corpus: &[u8],
    project_tool: &[u8],
) -> Result<(), String> {
    let project_corpus_sha256 = sha256_bytes(project_corpus);
    let project_tool_sha256 = sha256_bytes(project_tool);
    let bindings = [
        (
            operator.peer_firmware_image_path.as_str(),
            operator.peer_firmware_sha256.as_str(),
            None,
            "peer firmware image",
        ),
        (
            operator.peer_firmware_source_path.as_str(),
            operator.peer_firmware_source_sha256.as_str(),
            None,
            "peer firmware source",
        ),
        (
            operator.peer_corpus_path.as_str(),
            operator.peer_corpus_sha256.as_str(),
            Some(project_corpus_sha256.as_str()),
            "peer corpus copy",
        ),
        (
            operator.peer_tool_path.as_str(),
            operator.peer_tool_sha256.as_str(),
            Some(project_tool_sha256.as_str()),
            "peer tool copy",
        ),
    ];
    let mut paths = BTreeSet::new();
    for (relative, recorded_sha256, project_sha256, label) in bindings {
        if !paths.insert(relative) {
            return Err(format!(
                "operator peer artifact paths must be distinct; repeated {relative:?}"
            ));
        }
        validate_capture_file(evidence, relative, true)?;
        validate_sha256(recorded_sha256)?;
        let actual_sha256 = sha256_file(&resolve_capture_path(evidence, relative)?)?;
        if actual_sha256 != recorded_sha256 {
            return Err(format!(
                "preserved {label} digest does not match the operator record"
            ));
        }
        if project_sha256.is_some_and(|expected| actual_sha256 != expected) {
            return Err(format!(
                "preserved {label} does not match the project-owned pinned file"
            ));
        }
    }
    Ok(())
}

fn verify_peer_firmware_source_bundle(
    bundle: &Path,
    expected_revision: &str,
    expected_tree: &str,
) -> Result<(), String> {
    validate_git_object_id(expected_revision, "peer firmware revision")?;
    validate_git_object_id(expected_tree, "peer firmware root tree")?;
    let bundle = fs::canonicalize(bundle).map_err(|error| {
        format!(
            "could not canonicalize preserved peer source bundle {}: {error}",
            bundle.display()
        )
    })?;
    let temporary = PeerBundleRepository::new()?;
    let repository = temporary.path.join("repository.git");
    let output = safe_git_command()
        .args(["clone", "--bare", "--no-hardlinks", "--"])
        .arg(&bundle)
        .arg(&repository)
        .output()
        .map_err(|error| format!("could not clone preserved peer source bundle: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "preserved peer firmware source is not a self-contained Git bundle: {}",
            first_output_line(&output.stderr)
        ));
    }
    let repository = fs::canonicalize(&repository).map_err(|error| {
        format!(
            "could not canonicalize cloned peer source repository {}: {error}",
            repository.display()
        )
    })?;
    run_git_in_repository(
        &repository,
        &["fsck", "--strict", "--full", "--no-reflogs"],
        "validate preserved peer source objects",
    )?;
    run_git_in_repository(
        &repository,
        &["cat-file", "-e", &format!("{expected_revision}^{{commit}}")],
        "locate the pinned peer firmware commit",
    )?;
    let reachable = capture_git_in_repository(
        &repository,
        &["rev-list", "--all"],
        "enumerate preserved peer source history",
    )?;
    if !reachable.lines().any(|line| line == expected_revision) {
        return Err(format!(
            "pinned peer firmware revision {expected_revision} is not reachable from the preserved bundle refs"
        ));
    }
    let tree = capture_git_in_repository(
        &repository,
        &["show", "-s", "--format=%T", expected_revision],
        "inspect the pinned peer firmware tree",
    )?;
    if tree.trim() != expected_tree {
        return Err(format!(
            "pinned peer firmware revision has tree {:?} instead of {:?}",
            tree.trim(),
            expected_tree
        ));
    }
    Ok(())
}

fn safe_git_command() -> Command {
    let path = std::env::var_os("PATH");
    let mut command = Command::new("git");
    command
        .env_clear()
        .envs(path.map(|value| ("PATH", value)))
        .env("HOME", "/dev/null")
        .env("XDG_CONFIG_HOME", "/dev/null")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "--no-replace-objects",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "core.hooksPath=/dev/null",
        ]);
    command
}

fn run_git_in_repository(
    repository: &Path,
    arguments: &[&str],
    action: &str,
) -> Result<(), String> {
    let output = safe_git_command()
        .arg("--git-dir")
        .arg(repository)
        .args(arguments)
        .output()
        .map_err(|error| format!("could not {action}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "could not {action}: {}",
            first_output_line(&output.stderr)
        ))
    }
}

fn capture_git_in_repository(
    repository: &Path,
    arguments: &[&str],
    action: &str,
) -> Result<String, String> {
    let output = safe_git_command()
        .arg("--git-dir")
        .arg(repository)
        .args(arguments)
        .output()
        .map_err(|error| format!("could not {action}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not {action}: {}",
            first_output_line(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("{action} output was not UTF-8: {error}"))
}

fn first_output_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("unknown Git error")
        .to_owned()
}

fn validate_git_object_id(value: &str, label: &str) -> Result<(), String> {
    if value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!(
            "{label} is not a canonical SHA-1 object id: {value:?}"
        ))
    }
}

struct PeerBundleRepository {
    path: PathBuf,
}

impl PeerBundleRepository {
    fn new() -> Result<Self, String> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system time is before Unix epoch: {error}"))?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "reticulum-peer-bundle-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).map_err(|error| {
            format!(
                "could not create peer bundle verification directory {}: {error}",
                path.display()
            )
        })?;
        Ok(Self { path })
    }
}

impl Drop for PeerBundleRepository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn read_project_peer_sources(bundle: &EvidenceBundleBinding) -> Result<ProjectPeerSources, String> {
    let archive = Path::new(&bundle.canonical_path).join("source.tar");
    let mut requested = BTreeMap::from([
        (PEER_CORPUS_FILE.to_owned(), None),
        (PEER_TOOL_FILE.to_owned(), None),
        (BOOT_LOCAL_GENERATOR_FILE.to_owned(), None),
    ]);
    let file = File::open(&archive).map_err(|error| {
        format!(
            "could not open verified bundle source archive {}: {error}",
            archive.display()
        )
    })?;
    let mut tar = tar::Archive::new(file);
    let entries = tar.entries().map_err(|error| {
        format!(
            "could not enumerate verified bundle source archive {}: {error}",
            archive.display()
        )
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|error| {
            format!(
                "could not read verified bundle source archive {}: {error}",
                archive.display()
            )
        })?;
        let path = entry
            .path()
            .map_err(|error| format!("could not decode archived source path: {error}"))?;
        let path = relative_path_text(&path)?;
        let Some(slot) = requested.get_mut(&path) else {
            continue;
        };
        if slot.is_some() || !entry.header().entry_type().is_file() {
            return Err(format!(
                "verified bundle source archive has an invalid or duplicate {path:?} entry"
            ));
        }
        let size = entry
            .header()
            .size()
            .map_err(|error| format!("could not read archived {path:?} size: {error}"))?;
        if size > 4 * 1024 * 1024 {
            return Err(format!(
                "archived project-owned file {path:?} is unexpectedly large"
            ));
        }
        let mut bytes = Vec::with_capacity(size as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| format!("could not read archived {path:?}: {error}"))?;
        *slot = Some(bytes);
    }
    let corpus = requested
        .remove(PEER_CORPUS_FILE)
        .flatten()
        .ok_or_else(|| format!("verified bundle source archive lacks {PEER_CORPUS_FILE}"))?;
    let tool = requested
        .remove(PEER_TOOL_FILE)
        .flatten()
        .ok_or_else(|| format!("verified bundle source archive lacks {PEER_TOOL_FILE}"))?;
    let generator = requested
        .remove(BOOT_LOCAL_GENERATOR_FILE)
        .flatten()
        .ok_or_else(|| {
            format!("verified bundle source archive lacks {BOOT_LOCAL_GENERATOR_FILE}")
        })?;
    Ok(ProjectPeerSources {
        corpus,
        tool,
        boot_local_generator: generator,
    })
}

fn validate_pinned_peer_corpus(
    corpus: &serde_json::Value,
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    if corpus.get("schema") != Some(&serde_json::json!(3))
        || corpus.get("lane") != Some(&serde_json::json!("phase-1-rx-hil"))
        || corpus.get("protocol") != Some(&serde_json::json!("RNode LoRa framing"))
    {
        return Err("project-owned peer corpus identity changed".to_owned());
    }
    let expected_peer = serde_json::json!({
        "package": "RNode_Firmware",
        "repository": "https://github.com/markqvist/RNode_Firmware.git",
        "required_capability": "CMD_PROMISC 0x0e raw-frame transmit",
        "revision": PEER_FIRMWARE_REVISION,
        "version": PEER_FIRMWARE_VERSION
    });
    if corpus.get("peer") != Some(&expected_peer) {
        return Err("project-owned peer corpus no longer binds the pinned RNode peer".to_owned());
    }
    let scenarios = corpus
        .get("scenarios")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "project-owned peer corpus scenarios are not an array".to_owned())?;
    let mut indexed = BTreeMap::new();
    for scenario in scenarios {
        let name = peer_scenario_name(scenario)?;
        if indexed.insert(name.to_owned(), scenario.clone()).is_some() {
            return Err(format!(
                "project-owned peer corpus repeats scenario {name:?}"
            ));
        }
    }
    if indexed.is_empty() {
        return Err("project-owned peer corpus contains no scenarios".to_owned());
    }
    Ok(indexed)
}

fn validate_passing_peer_record(
    record: &ScenarioRecord,
    context: &PeerValidationContext<'_>,
) -> Result<(), String> {
    let PeerValidationContext {
        evidence,
        operator,
        profile_environment,
        pinned_corpus,
        pinned_corpus_bytes,
        pinned_scenarios,
        boot_local_generator_sha256,
    } = context;
    let expected_runs = expected_peer_runs(&record.scenario_id).ok_or_else(|| {
        format!(
            "passing peer scenario {} has no peer-run contract",
            record.scenario_id
        )
    })?;
    let manifest_paths = record
        .peer_capture_files
        .iter()
        .filter(|path| capture_file_name(path) == Some(PEER_MANIFEST_FILE))
        .cloned()
        .collect::<BTreeSet<_>>();
    if manifest_paths.is_empty() {
        return Err(format!(
            "passing peer scenario {} has no listed {PEER_MANIFEST_FILE}",
            record.scenario_id
        ));
    }
    if let Some(check) = record.checks.get("all-corpus-cases-run") {
        let bound = check
            .evidence_files
            .iter()
            .filter(|path| capture_file_name(path) == Some(PEER_MANIFEST_FILE))
            .cloned()
            .collect::<BTreeSet<_>>();
        if bound != manifest_paths {
            return Err(format!(
                "scenario {} all-corpus-cases-run must bind exactly every listed peer manifest",
                record.scenario_id
            ));
        }
    }

    let mut observed_counts = BTreeMap::<String, usize>::new();
    let mut paired_transcripts = BTreeSet::new();
    let mut transcript_digests = BTreeMap::<String, BTreeSet<String>>::new();
    let mut run_time_pairs = BTreeMap::<String, BTreeSet<(String, String)>>::new();
    for manifest_path in &manifest_paths {
        let manifest: PeerManifest = read_json(&resolve_capture_path(evidence, manifest_path)?)?;
        let scenario_name = peer_scenario_name(&manifest.scenario)?;
        validate_peer_run_interval(record, &manifest, manifest_path)?;
        let expectation = peer_run_expectation(
            &record.scenario_id,
            scenario_name,
            expected_runs,
            pinned_scenarios,
        )?;
        *observed_counts.entry(scenario_name.to_owned()).or_default() += 1;
        validate_manifest_capture_source(
            evidence,
            &manifest.tool,
            &operator.peer_tool_path,
            "peer tool",
        )?;

        let transcript_path = sibling_capture_path(manifest_path, PEER_TRANSCRIPT_FILE)?;
        if !record.peer_capture_files.contains(&transcript_path)
            || !record.evidence_files.contains(&transcript_path)
        {
            return Err(format!(
                "peer manifest {manifest_path:?} lacks its listed sibling transcript {transcript_path:?}"
            ));
        }
        validate_capture_file(evidence, &transcript_path, true)?;
        let transcript_sha256 = sha256_file(&resolve_capture_path(evidence, &transcript_path)?)?;
        if transcript_sha256 != manifest.transcript_sha256 {
            return Err(format!(
                "peer transcript digest does not match manifest {manifest_path:?}"
            ));
        }
        record_unique_peer_run(
            &mut transcript_digests,
            &mut run_time_pairs,
            scenario_name,
            &manifest,
        )?;
        validate_peer_transcript(
            &resolve_capture_path(evidence, &transcript_path)?,
            &manifest,
        )?;
        paired_transcripts.insert(transcript_path);

        let check = record
            .checks
            .get(expectation.binding_check)
            .ok_or_else(|| {
                format!(
                    "scenario {} lacks peer binding check {:?}",
                    record.scenario_id, expectation.binding_check
                )
            })?;
        if !check.evidence_files.contains(manifest_path) {
            return Err(format!(
                "peer manifest {manifest_path:?} is not bound to check {:?}",
                expectation.binding_check
            ));
        }
        if !record
            .artifact_uses
            .iter()
            .any(|artifact| artifact.artifact_id == expectation.artifact_id)
        {
            return Err(format!(
                "peer manifest {manifest_path:?} is not backed by artifact {:?}",
                expectation.artifact_id
            ));
        }

        validate_peer_manifest_contract(
            &manifest,
            manifest_path,
            scenario_name,
            expectation,
            operator,
            profile_environment,
        )?;
        match expectation.corpus_kind {
            PeerCorpusKind::Pinned => {
                let expected_scenario = pinned_scenarios.get(scenario_name).ok_or_else(|| {
                    format!("peer scenario {scenario_name:?} is absent from the pinned corpus")
                })?;
                if &manifest.scenario != expected_scenario {
                    return Err(format!(
                        "peer manifest {manifest_path:?} scenario differs from the pinned corpus"
                    ));
                }
                if manifest.corpus_sha256 != operator.peer_corpus_sha256 {
                    return Err(format!(
                        "peer manifest {manifest_path:?} does not bind the pinned corpus digest"
                    ));
                }
                validate_manifest_capture_source(
                    evidence,
                    &manifest.corpus,
                    &operator.peer_corpus_path,
                    "pinned peer corpus",
                )?;
            }
            PeerCorpusKind::BootLocal => validate_boot_local_corpus(
                evidence,
                record,
                &manifest,
                manifest_path,
                pinned_corpus,
                pinned_corpus_bytes,
                boot_local_generator_sha256,
            )?,
        }
    }

    let listed_transcripts = record
        .peer_capture_files
        .iter()
        .filter(|path| capture_file_name(path) == Some(PEER_TRANSCRIPT_FILE))
        .cloned()
        .collect::<BTreeSet<_>>();
    if listed_transcripts != paired_transcripts {
        return Err(format!(
            "scenario {} contains a missing or orphan peer transcript",
            record.scenario_id
        ));
    }
    validate_expected_peer_counts(&record.scenario_id, expected_runs, &observed_counts)
}

fn record_unique_peer_run(
    transcript_digests: &mut BTreeMap<String, BTreeSet<String>>,
    run_time_pairs: &mut BTreeMap<String, BTreeSet<(String, String)>>,
    scenario_name: &str,
    manifest: &PeerManifest,
) -> Result<(), String> {
    if !transcript_digests
        .entry(scenario_name.to_owned())
        .or_default()
        .insert(manifest.transcript_sha256.clone())
        || !run_time_pairs
            .entry(scenario_name.to_owned())
            .or_default()
            .insert((manifest.started_utc.clone(), manifest.finished_utc.clone()))
    {
        return Err(format!(
            "peer scenario {scenario_name:?} repeats a copied transcript or run timestamp pair"
        ));
    }
    Ok(())
}

fn validate_peer_run_interval(
    record: &ScenarioRecord,
    manifest: &PeerManifest,
    manifest_path: &str,
) -> Result<(), String> {
    let started = record.started_utc.as_deref().ok_or_else(|| {
        format!(
            "passing peer scenario {} lacks started_utc",
            record.scenario_id
        )
    })?;
    let finished = record.finished_utc.as_deref().ok_or_else(|| {
        format!(
            "passing peer scenario {} lacks finished_utc",
            record.scenario_id
        )
    })?;
    if manifest.started_utc.as_str() < started || manifest.finished_utc.as_str() > finished {
        return Err(format!(
            "peer manifest {manifest_path:?} interval falls outside powered scenario {}",
            record.scenario_id
        ));
    }
    Ok(())
}

fn expected_peer_runs(scenario_id: &str) -> Option<&'static [PeerRunExpectation]> {
    match scenario_id {
        "single-physical-frame" => Some(SINGLE_PEER_RUNS),
        "split-packet" => Some(SPLIT_PEER_RUNS),
        "fragment-expiry-and-replacement" => Some(EXPIRY_PEER_RUNS),
        "physical-over-rns-boundary" => Some(BOUNDARY_PEER_RUNS),
        "malformed-and-semantic-rejection" => Some(MALFORMED_PEER_RUNS),
        "bounded-backpressure" => Some(BACKPRESSURE_PEER_RUNS),
        "returned-radio-fault" => Some(RETURNED_FAULT_PEER_RUNS),
        "receive-soak-24h" => Some(SOAK_MINIMUM_PEER_RUNS),
        _ => None,
    }
}

fn peer_run_expectation(
    powered_scenario_id: &str,
    peer_scenario_name: &str,
    expected_runs: &'static [PeerRunExpectation],
    pinned_scenarios: &BTreeMap<String, serde_json::Value>,
) -> Result<PeerRunExpectation, String> {
    if let Some(expectation) = expected_runs
        .iter()
        .find(|expectation| expectation.name == peer_scenario_name)
    {
        return Ok(*expectation);
    }
    if powered_scenario_id == "receive-soak-24h" {
        let scenario = pinned_scenarios
            .get(peer_scenario_name)
            .ok_or_else(|| format!("soak uses unknown peer scenario {peer_scenario_name:?}"))?;
        if scenario.get("required_target_feature").is_some() {
            return Err(format!(
                "soak peer scenario {peer_scenario_name:?} is not an ordinary lab-rx stimulus"
            ));
        }
        return Ok(ordinary_peer_run("", "mixed-valid-and-hostile-traffic"));
    }
    Err(format!(
        "powered scenario {powered_scenario_id} contains unexpected peer scenario {peer_scenario_name:?}"
    ))
}

fn validate_expected_peer_counts(
    powered_scenario_id: &str,
    expected_runs: &[PeerRunExpectation],
    observed: &BTreeMap<String, usize>,
) -> Result<(), String> {
    for expectation in expected_runs {
        let count = observed.get(expectation.name).copied().unwrap_or_default();
        let count_is_valid = if powered_scenario_id == "receive-soak-24h" {
            count >= expectation.count
        } else {
            count == expectation.count
        };
        if !count_is_valid {
            return Err(format!(
                "powered scenario {powered_scenario_id} requires {} peer run(s) of {:?}, found {count}",
                expectation.count, expectation.name
            ));
        }
    }
    if powered_scenario_id != "receive-soak-24h" && observed.len() != expected_runs.len() {
        return Err(format!(
            "powered scenario {powered_scenario_id} contains an unexpected peer scenario set"
        ));
    }
    Ok(())
}

fn validate_peer_manifest_contract(
    manifest: &PeerManifest,
    manifest_path: &str,
    scenario_name: &str,
    expectation: PeerRunExpectation,
    operator: &OperatorRecord,
    profile_environment: &BTreeMap<String, String>,
) -> Result<(), String> {
    if manifest.schema != 1
        || manifest.status != "enqueued_not_rf_verified"
        || manifest.error != serde_json::Value::Null
    {
        return Err(format!(
            "peer manifest {manifest_path:?} is not a successful schema-1 enqueue record"
        ));
    }
    validate_time_pair(
        Some(&manifest.started_utc),
        Some(&manifest.finished_utc),
        &format!("peer manifest {manifest_path:?}"),
    )?;
    for (value, label) in [
        (&manifest.corpus, "corpus source path"),
        (&manifest.tool, "tool source path"),
        (&manifest.serial_port, "serial port"),
        (&manifest.region_basis, "region basis"),
    ] {
        require_nonempty(value, label)?;
    }
    if !Path::new(&manifest.corpus).is_absolute() || !Path::new(&manifest.tool).is_absolute() {
        return Err(format!(
            "peer manifest {manifest_path:?} corpus and tool source paths must be absolute"
        ));
    }
    validate_sha256(&manifest.corpus_sha256)?;
    validate_sha256(&manifest.tool_sha256)?;
    validate_sha256(&manifest.transcript_sha256)?;
    if manifest.tool_sha256 != operator.peer_tool_sha256 {
        return Err(format!(
            "peer manifest {manifest_path:?} does not bind the preserved peer tool"
        ));
    }
    if manifest.target_artifact_mode != expectation.target_mode {
        return Err(format!(
            "peer manifest {manifest_path:?} target mode {:?} does not match {:?}",
            manifest.target_artifact_mode, expectation.target_mode
        ));
    }
    if manifest.region_basis != operator.region_basis {
        return Err(format!(
            "peer manifest {manifest_path:?} region basis differs from the operator record"
        ));
    }
    let steps = manifest
        .scenario
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("peer scenario {scenario_name:?} steps are not an array"))?;
    if steps.is_empty() || manifest.enqueued_steps != steps.len() {
        return Err(format!(
            "peer manifest {manifest_path:?} enqueued step count does not match its scenario"
        ));
    }
    if manifest.post_enqueue_observation_ms != 2_000
        || !manifest.antenna_or_load_attached
        || !manifest.fresh_peer_reset_acknowledged
        || !manifest.fresh_tracker_boot_acknowledged
        || !manifest.independent_rf_observer_required
    {
        return Err(format!(
            "peer manifest {manifest_path:?} does not contain the required RF safety acknowledgments and observation window"
        ));
    }
    if manifest.device.firmware_version != PEER_FIRMWARE_VERSION
        || manifest.device.firmware_version_bytes_hex != PEER_FIRMWARE_VERSION_BYTES_HEX
    {
        return Err(format!(
            "peer manifest {manifest_path:?} does not report the pinned RNode firmware version"
        ));
    }
    validate_peer_runtime(&manifest.runtime, manifest_path)?;
    validate_peer_profile_and_timing(manifest, manifest_path, operator, profile_environment)
}

fn validate_peer_transcript(path: &Path, manifest: &PeerManifest) -> Result<(), String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read peer transcript {}: {error}", path.display()))?;
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(format!(
            "peer transcript {} must be non-empty newline-terminated JSONL",
            path.display()
        ));
    }
    let mut entries = Vec::new();
    for (index, line) in bytes[..bytes.len() - 1]
        .split(|byte| *byte == b'\n')
        .enumerate()
    {
        if line.is_empty() {
            return Err(format!(
                "peer transcript {} contains an empty JSONL record",
                path.display()
            ));
        }
        let entry: PeerTranscriptEntry = serde_json::from_slice(line).map_err(|error| {
            format!(
                "could not parse peer transcript {} line {}: {error}",
                path.display(),
                index + 1
            )
        })?;
        if entry.sequence != index as u64 {
            return Err(format!(
                "peer transcript {} sequence is not contiguous at line {}",
                path.display(),
                index + 1
            ));
        }
        validate_utc_timestamp(&entry.utc)?;
        if let Some(previous) = entries.last() {
            let previous: &PeerTranscriptEntry = previous;
            if entry.monotonic_ns < previous.monotonic_ns || entry.utc < previous.utc {
                return Err(format!(
                    "peer transcript {} timestamps are not monotonic",
                    path.display()
                ));
            }
        }
        if !matches!(entry.direction.as_str(), "host_to_peer" | "peer_to_host") {
            return Err(format!(
                "peer transcript {} has invalid direction {:?}",
                path.display(),
                entry.direction
            ));
        }
        let payload = decode_lower_hex(&entry.payload_hex, "peer transcript payload")?;
        let wire = decode_lower_hex(&entry.wire_hex, "peer transcript wire")?;
        if wire != kiss_wire(entry.command, &payload) {
            return Err(format!(
                "peer transcript {} line {} wire does not encode its command and payload",
                path.display(),
                index + 1
            ));
        }
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err(format!(
            "peer transcript {} contains no records",
            path.display()
        ));
    }
    if entries
        .first()
        .is_some_and(|entry| entry.utc < manifest.started_utc)
        || entries
            .last()
            .is_some_and(|entry| entry.utc > manifest.finished_utc)
    {
        return Err(format!(
            "peer transcript {} timestamps fall outside the manifest run interval",
            path.display()
        ));
    }

    let expected = peer_exchange_contract(manifest)?;
    validate_peer_transcript_sequence(
        path,
        &entries,
        &expected.exchanges,
        &expected.data,
        &expected.physical,
    )
}

fn validate_peer_transcript_sequence(
    path: &Path,
    entries: &[PeerTranscriptEntry],
    exchanges: &[(PeerFrame, PeerFrame)],
    data: &[Vec<u8>],
    physical: &PeerFrame,
) -> Result<(), String> {
    const RADIO_ON_EXCHANGE: usize = 14;
    let mut cursor = 0;
    let mut physical_reports = 0;
    for (index, (host, peer)) in exchanges.iter().enumerate().take(RADIO_ON_EXCHANGE + 1) {
        expect_transcript_frame(entries, &mut cursor, "host_to_peer", host, path)?;
        if index == RADIO_ON_EXCHANGE {
            physical_reports += consume_physical_reports(entries, &mut cursor, physical, path)?;
        }
        expect_transcript_frame(entries, &mut cursor, "peer_to_host", peer, path)?;
    }
    consume_ready_loop(
        entries,
        &mut cursor,
        physical,
        &mut physical_reports,
        true,
        path,
    )?;
    physical_reports += consume_physical_reports(entries, &mut cursor, physical, path)?;
    if physical_reports == 0 {
        return Err(format!(
            "peer transcript {} lacks the configured physical timing report",
            path.display()
        ));
    }
    for (host, peer) in exchanges.iter().skip(RADIO_ON_EXCHANGE + 1) {
        expect_transcript_frame(entries, &mut cursor, "host_to_peer", host, path)?;
        expect_transcript_frame(entries, &mut cursor, "peer_to_host", peer, path)?;
    }
    for payload in data {
        consume_ready_loop(
            entries,
            &mut cursor,
            physical,
            &mut physical_reports,
            false,
            path,
        )?;
        expect_transcript_frame(
            entries,
            &mut cursor,
            "host_to_peer",
            &PeerFrame {
                command: CMD_DATA,
                payload: payload.clone(),
            },
            path,
        )?;
    }
    consume_ready_loop(
        entries,
        &mut cursor,
        physical,
        &mut physical_reports,
        false,
        path,
    )?;
    if cursor != entries.len() {
        return Err(format!(
            "peer transcript {} contains unexpected or out-of-order frames after the tool contract",
            path.display()
        ));
    }
    Ok(())
}

fn consume_ready_loop(
    entries: &[PeerTranscriptEntry],
    cursor: &mut usize,
    physical: &PeerFrame,
    physical_reports: &mut usize,
    allow_physical: bool,
    path: &Path,
) -> Result<(), String> {
    loop {
        if allow_physical {
            *physical_reports += consume_physical_reports(entries, cursor, physical, path)?;
        }
        expect_transcript_frame(
            entries,
            cursor,
            "host_to_peer",
            &PeerFrame {
                command: CMD_READY,
                payload: vec![0],
            },
            path,
        )?;
        if allow_physical {
            *physical_reports += consume_physical_reports(entries, cursor, physical, path)?;
        }
        let response = entries.get(*cursor).ok_or_else(|| {
            format!(
                "peer transcript {} ends during a READY exchange",
                path.display()
            )
        })?;
        let frame = transcript_frame(response)?;
        if response.direction != "peer_to_host"
            || frame.command != CMD_READY
            || !matches!(frame.payload.as_slice(), [0] | [1])
        {
            return Err(format!(
                "peer transcript {} has an unsolicited or malformed READY response",
                path.display()
            ));
        }
        *cursor += 1;
        if frame.payload == [1] {
            return Ok(());
        }
    }
}

fn consume_physical_reports(
    entries: &[PeerTranscriptEntry],
    cursor: &mut usize,
    expected: &PeerFrame,
    path: &Path,
) -> Result<usize, String> {
    let mut count = 0;
    while let Some(entry) = entries.get(*cursor) {
        if entry.direction != "peer_to_host" || entry.command != CMD_STAT_PHYPRM {
            break;
        }
        if transcript_frame(entry)? != *expected {
            return Err(format!(
                "peer transcript {} physical timing report does not support the manifest",
                path.display()
            ));
        }
        *cursor += 1;
        count += 1;
    }
    Ok(count)
}

fn expect_transcript_frame(
    entries: &[PeerTranscriptEntry],
    cursor: &mut usize,
    direction: &str,
    expected: &PeerFrame,
    path: &Path,
) -> Result<(), String> {
    let entry = entries.get(*cursor).ok_or_else(|| {
        format!(
            "peer transcript {} ends before the expected {direction} command 0x{:02x}",
            path.display(),
            expected.command
        )
    })?;
    if entry.direction != direction || transcript_frame(entry)? != *expected {
        return Err(format!(
            "peer transcript {} has an out-of-order or mismatched frame at sequence {}",
            path.display(),
            entry.sequence
        ));
    }
    *cursor += 1;
    Ok(())
}

fn peer_exchange_contract(manifest: &PeerManifest) -> Result<PeerExchangeContract, String> {
    let profile = &manifest.profile;
    let frequency = u32::try_from(profile.frequency_hz)
        .map_err(|_| "peer frequency does not fit the RNode wire contract".to_owned())?
        .to_be_bytes()
        .to_vec();
    let bandwidth = u32::try_from(profile.bandwidth_hz)
        .map_err(|_| "peer bandwidth does not fit the RNode wire contract".to_owned())?
        .to_be_bytes()
        .to_vec();
    let power = u8::try_from(profile.tx_power_dbm)
        .map_err(|_| "peer TX power does not fit the RNode wire contract".to_owned())?;
    let short = profile
        .short_airtime_limit_basis_points
        .to_be_bytes()
        .to_vec();
    let long = profile
        .long_airtime_limit_basis_points
        .to_be_bytes()
        .to_vec();
    let effective_short = manifest
        .peer_physical_timing
        .effective_short_airtime_limit_basis_points
        .to_be_bytes()
        .to_vec();
    let effective_long = manifest
        .peer_physical_timing
        .effective_long_airtime_limit_basis_points
        .to_be_bytes()
        .to_vec();
    let mut exchanges = vec![
        exchange(CMD_DETECT, &[0x73], &[0x46]),
        exchange(CMD_FW_VERSION, &[0], &[1, 86]),
        exchange(CMD_BOARD, &[0], &[manifest.device.board]),
        exchange(CMD_PLATFORM, &[0], &[manifest.device.platform]),
        exchange(CMD_MCU, &[0], &[manifest.device.mcu]),
        exchange(CMD_RADIO_STATE, &[0], &[0]),
        exchange(CMD_FREQUENCY, &frequency, &frequency),
        exchange(CMD_BANDWIDTH, &bandwidth, &bandwidth),
        exchange(CMD_TXPOWER, &[power], &[power]),
        exchange(
            CMD_SF,
            &[profile.spreading_factor],
            &[profile.spreading_factor],
        ),
        exchange(
            CMD_CR,
            &[profile.coding_rate_denominator],
            &[profile.coding_rate_denominator],
        ),
        exchange(CMD_IMPLICIT, &[0], &[0]),
        exchange(CMD_ST_ALOCK, &short, &effective_short),
        exchange(CMD_LT_ALOCK, &long, &effective_long),
        exchange(CMD_RADIO_STATE, &[1], &[1]),
        exchange(CMD_FREQUENCY, &[0; 4], &frequency),
        exchange(CMD_BANDWIDTH, &[0; 4], &bandwidth),
        exchange(CMD_TXPOWER, &[0xff], &[power]),
        exchange(CMD_SF, &[0xff], &[profile.spreading_factor]),
        exchange(CMD_CR, &[0xff], &[profile.coding_rate_denominator]),
        exchange(CMD_IMPLICIT, &[0], &[0]),
        exchange(CMD_ST_ALOCK, &short, &effective_short),
        exchange(CMD_LT_ALOCK, &long, &effective_long),
        exchange(CMD_RADIO_STATE, &[0xff], &[1]),
    ];
    let steps = manifest
        .scenario
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "peer scenario steps are not an array".to_owned())?;
    let modes = steps
        .iter()
        .map(|step| {
            step.get("mode")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "peer scenario step is missing mode".to_owned())
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let promisc = if modes.len() == 1 && modes.contains("raw_lora_frame") {
        1
    } else if modes.len() == 1 && modes.contains("rnode_packet") {
        0
    } else {
        return Err("peer scenario does not have one supported RNode mode".to_owned());
    };
    exchanges.push(exchange(CMD_PROMISC, &[promisc], &[promisc]));
    let data = steps
        .iter()
        .map(|step| {
            let payload = step
                .get("payload_hex")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "peer scenario step is missing payload_hex".to_owned())?;
            decode_lower_hex(payload, "peer scenario payload")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let timing = &manifest.peer_physical_timing;
    let mut physical_payload = Vec::with_capacity(12);
    for value in [
        timing.symbol_time_us,
        timing.symbol_rate,
        u64::from(timing.preamble_symbols),
        timing.preamble_time_ms,
        timing.csma_slot_ms,
        timing.difs_ms,
    ] {
        physical_payload.extend_from_slice(
            &u16::try_from(value)
                .map_err(|_| "peer physical timing does not fit its 12-byte report".to_owned())?
                .to_be_bytes(),
        );
    }
    Ok(PeerExchangeContract {
        exchanges,
        data,
        physical: PeerFrame {
            command: CMD_STAT_PHYPRM,
            payload: physical_payload,
        },
    })
}

fn exchange(command: u8, request: &[u8], response: &[u8]) -> (PeerFrame, PeerFrame) {
    (
        PeerFrame {
            command,
            payload: request.to_vec(),
        },
        PeerFrame {
            command,
            payload: response.to_vec(),
        },
    )
}

fn transcript_frame(entry: &PeerTranscriptEntry) -> Result<PeerFrame, String> {
    Ok(PeerFrame {
        command: entry.command,
        payload: decode_lower_hex(&entry.payload_hex, "peer transcript payload")?,
    })
}

fn kiss_wire(command: u8, payload: &[u8]) -> Vec<u8> {
    let mut wire = vec![KISS_FEND, command];
    for byte in payload {
        match *byte {
            KISS_FEND => wire.extend_from_slice(&[KISS_FESC, KISS_TFEND]),
            KISS_FESC => wire.extend_from_slice(&[KISS_FESC, KISS_TFESC]),
            byte => wire.push(byte),
        }
    }
    wire.push(KISS_FEND);
    wire
}

fn decode_lower_hex(value: &str, label: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "{label} is not canonical lower-case even-length hex"
        ));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let nibble = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => unreachable!("validated lower-case hex"),
            };
            Ok((nibble(pair[0]) << 4) | nibble(pair[1]))
        })
        .collect()
}

fn validate_peer_runtime(runtime: &PeerRuntime, manifest_path: &str) -> Result<(), String> {
    let serial = &runtime.serial;
    if runtime.python_implementation != "CPython"
        || runtime.python_version != "3.13.7"
        || runtime.pyserial_version != "3.5"
        || serial.baudrate != 115_200
        || serial.bytesize != 8
        || serial.parity != "N"
        || serial.stopbits != 1
        || serial.timeout_seconds != 0.1
        || serial.write_timeout_seconds != 3.0
        || serial.xonxoff
        || serial.rtscts
        || serial.dsrdtr
    {
        return Err(format!(
            "peer manifest {manifest_path:?} runtime does not match the qualification tool contract"
        ));
    }
    Ok(())
}

fn validate_peer_profile_and_timing(
    manifest: &PeerManifest,
    manifest_path: &str,
    operator: &OperatorRecord,
    profile_environment: &BTreeMap<String, String>,
) -> Result<(), String> {
    let profile = &manifest.profile;
    let expected_frequency =
        profile_environment_value(profile_environment, "RETICULUM_LAB_RX_FREQUENCY_HZ")?;
    let expected_bandwidth =
        profile_environment_value(profile_environment, "RETICULUM_LAB_RX_BANDWIDTH_HZ")?;
    let expected_sf =
        profile_environment_value(profile_environment, "RETICULUM_LAB_RX_SPREADING_FACTOR")?;
    let expected_cr = profile_environment_value(
        profile_environment,
        "RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR",
    )?;
    let expected_receiver_preamble =
        profile_environment_value(profile_environment, "RETICULUM_LAB_RX_PREAMBLE_SYMBOLS")?;
    if profile.frequency_hz != expected_frequency
        || profile.bandwidth_hz != expected_bandwidth
        || u64::from(profile.spreading_factor) != expected_sf
        || u64::from(profile.coding_rate_denominator) != expected_cr
        || u64::from(profile.receiver_preamble_symbols) != expected_receiver_preamble
    {
        return Err(format!(
            "peer manifest {manifest_path:?} RF profile differs from the bound Tracker bundle"
        ));
    }
    if Some(profile.tx_power_dbm) != operator.peer_conducted_power_dbm
        || Some(profile.short_airtime_limit_basis_points)
            != operator.peer_short_airtime_limit_basis_points
        || Some(profile.long_airtime_limit_basis_points)
            != operator.peer_long_airtime_limit_basis_points
        || Some(profile.expected_peer_preamble_symbols) != operator.peer_reported_preamble_symbols
        || Some(
            manifest
                .peer_physical_timing
                .effective_short_airtime_limit_basis_points,
        ) != operator.peer_effective_short_airtime_limit_basis_points
        || Some(
            manifest
                .peer_physical_timing
                .effective_long_airtime_limit_basis_points,
        ) != operator.peer_effective_long_airtime_limit_basis_points
        || manifest.peer_physical_timing.preamble_symbols != profile.expected_peer_preamble_symbols
    {
        return Err(format!(
            "peer manifest {manifest_path:?} peer RF values differ from the operator record"
        ));
    }
    if manifest.peer_physical_timing.symbol_time_us == 0
        || manifest.peer_physical_timing.symbol_rate == 0
        || manifest.peer_physical_timing.preamble_time_ms == 0
        || manifest.peer_physical_timing.csma_slot_ms == 0
        || manifest.peer_physical_timing.difs_ms == 0
        || manifest.receiver_maximum_frame_airtime_us == 0
    {
        return Err(format!(
            "peer manifest {manifest_path:?} contains zero physical timing values"
        ));
    }
    let expected_fragment_timeout = manifest
        .receiver_maximum_frame_airtime_us
        .checked_mul(2)
        .and_then(|value| value.checked_add(5_000_000))
        .ok_or_else(|| format!("peer manifest {manifest_path:?} timing overflow"))?;
    if manifest.receiver_fragment_timeout_us != expected_fragment_timeout {
        return Err(format!(
            "peer manifest {manifest_path:?} fragment timeout does not match the Tracker contract"
        ));
    }
    if profile.spreading_factor >= 64 || profile.bandwidth_hz == 0 {
        return Err(format!(
            "peer manifest {manifest_path:?} cannot derive peer preamble timing"
        ));
    }
    let extra_symbols = profile
        .expected_peer_preamble_symbols
        .saturating_sub(profile.receiver_preamble_symbols);
    let numerator =
        u128::from(extra_symbols) * (1_u128 << profile.spreading_factor) * 1_000_000_u128;
    let extension = numerator.div_ceil(u128::from(profile.bandwidth_hz));
    if extension != u128::from(manifest.peer_preamble_extension_us) {
        return Err(format!(
            "peer manifest {manifest_path:?} preamble extension does not match its RF profile"
        ));
    }
    Ok(())
}

fn profile_environment_value(
    profile_environment: &BTreeMap<String, String>,
    name: &str,
) -> Result<u64, String> {
    profile_environment
        .get(name)
        .ok_or_else(|| format!("bound Tracker profile is missing {name}"))?
        .parse::<u64>()
        .map_err(|error| format!("bound Tracker profile {name} is not an integer: {error}"))
}

fn validate_boot_local_corpus(
    evidence: &Path,
    record: &ScenarioRecord,
    manifest: &PeerManifest,
    manifest_path: &str,
    pinned_corpus: &serde_json::Value,
    pinned_corpus_bytes: &[u8],
    generator_source_sha256: &str,
) -> Result<(), String> {
    let candidates = record
        .peer_capture_files
        .iter()
        .filter(|path| capture_file_name(path) == Some(BOOT_LOCAL_CORPUS_FILE))
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err(format!(
            "boot-local peer manifest {manifest_path:?} requires exactly one listed {BOOT_LOCAL_CORPUS_FILE}"
        ));
    }
    let relative = candidates[0];
    if !record.evidence_files.contains(relative) {
        return Err("boot-local corpus is absent from the scenario evidence inventory".to_owned());
    }
    for check_id in ["all-corpus-cases-run", "boot-local-data-processed"] {
        if !record
            .checks
            .get(check_id)
            .is_some_and(|check| check.evidence_files.contains(relative))
        {
            return Err(format!(
                "boot-local corpus is not bound to check {check_id:?}"
            ));
        }
    }
    validate_capture_file(evidence, relative, true)?;
    let captured_path = fs::canonicalize(resolve_capture_path(evidence, relative)?)
        .map_err(|error| format!("could not canonicalize boot-local corpus: {error}"))?;
    let manifest_corpus_path = fs::canonicalize(Path::new(&manifest.corpus)).map_err(|error| {
        format!(
            "could not canonicalize peer manifest boot-local corpus {:?}: {error}",
            manifest.corpus
        )
    })?;
    if captured_path != manifest_corpus_path {
        return Err(format!(
            "boot-local peer manifest {manifest_path:?} does not reference its preserved corpus"
        ));
    }
    let actual_sha256 = sha256_file(&captured_path)?;
    if actual_sha256 != manifest.corpus_sha256 {
        return Err(format!(
            "boot-local peer manifest {manifest_path:?} corpus digest does not match its preserved corpus"
        ));
    }
    let captured_bytes = fs::read(&captured_path)
        .map_err(|error| format!("could not read boot-local corpus bytes: {error}"))?;
    let corpus: serde_json::Value = serde_json::from_slice(&captured_bytes)
        .map_err(|error| format!("could not parse boot-local corpus: {error}"))?;
    if corpus.get("schema") != Some(&serde_json::json!(3))
        || corpus.get("lane") != Some(&serde_json::json!("phase-1-rx-hil"))
        || corpus.get("protocol") != Some(&serde_json::json!("RNode LoRa framing"))
        || corpus.get("peer") != pinned_corpus.get("peer")
        || corpus.get("wire_contract") != pinned_corpus.get("wire_contract")
    {
        return Err(
            "boot-local corpus does not preserve the pinned HIL corpus identity".to_owned(),
        );
    }
    let scenarios = corpus
        .get("scenarios")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "boot-local corpus scenarios are not an array".to_owned())?;
    if scenarios.as_slice() != [manifest.scenario.clone()]
        || peer_scenario_name(&manifest.scenario)? != "boot-local-data"
    {
        return Err(
            "boot-local corpus does not contain exactly the manifested scenario".to_owned(),
        );
    }
    let target = corpus
        .get("target_boot")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "boot-local corpus is missing target_boot".to_owned())?;
    validate_lower_hex_field(target, "public_key_hex", 128)?;
    validate_lower_hex_field(target, "destination_hash_hex", 32)?;
    if target
        .get("destination_name")
        .and_then(serde_json::Value::as_str)
        != Some(BOOT_LOCAL_DESTINATION_NAME)
    {
        return Err("boot-local corpus destination name changed".to_owned());
    }
    let public_key = decode_fixed_lower_hex::<64>(
        target["public_key_hex"]
            .as_str()
            .expect("validated target field"),
        "boot-local public key",
    )?;
    let destination_hash = decode_fixed_lower_hex::<16>(
        target["destination_hash_hex"]
            .as_str()
            .expect("validated target field"),
        "boot-local destination hash",
    )?;
    let expected = boot_local_generator::generate(
        public_key,
        destination_hash,
        BootLocalInputs {
            base_corpus: pinned_corpus_bytes,
            source_sha256: generator_source_sha256,
        },
    )?;
    if captured_bytes != expected.corpus_bytes {
        return Err(
            "boot-local corpus bytes do not exactly match regeneration for its recorded target"
                .to_owned(),
        );
    }
    Ok(())
}

fn decode_fixed_lower_hex<const N: usize>(value: &str, label: &str) -> Result<[u8; N], String> {
    decode_lower_hex(value, label)?
        .try_into()
        .map_err(|_: Vec<u8>| format!("{label} must contain exactly {N} bytes"))
}

fn validate_lower_hex_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    length: usize,
) -> Result<(), String> {
    let value = object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("boot-local target is missing {field}"))?;
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "boot-local target {field} is not canonical lower-case hex"
        ));
    }
    Ok(())
}

fn peer_scenario_name(scenario: &serde_json::Value) -> Result<&str, String> {
    scenario
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty() && name.trim() == *name)
        .ok_or_else(|| "peer scenario requires a canonical non-empty name".to_owned())
}

fn capture_file_name(path: &str) -> Option<&str> {
    Path::new(path).file_name()?.to_str()
}

fn validate_manifest_capture_source(
    evidence: &Path,
    manifest_source: &str,
    operator_capture: &str,
    label: &str,
) -> Result<(), String> {
    let expected = fs::canonicalize(resolve_capture_path(evidence, operator_capture)?)
        .map_err(|error| format!("could not canonicalize preserved {label}: {error}"))?;
    let actual = fs::canonicalize(Path::new(manifest_source)).map_err(|error| {
        format!("could not canonicalize peer manifest {label} path {manifest_source:?}: {error}")
    })?;
    if actual != expected {
        return Err(format!(
            "peer manifest {label} path does not name the operator's preserved capture"
        ));
    }
    Ok(())
}

fn sibling_capture_path(path: &str, file_name: &str) -> Result<String, String> {
    let parent = Path::new(path)
        .parent()
        .ok_or_else(|| format!("capture path has no parent: {path:?}"))?;
    relative_path_text(&parent.join(file_name))
}

fn scenario_requires_peer(id: &str) -> bool {
    !matches!(
        id,
        "cold-boot-and-silence" | "corrupt-and-torn-retained-journal" | "electrical-matrix"
    )
}

fn validate_record_board_ids(
    operator: &OperatorRecord,
    records: &[ScenarioRecord],
) -> Result<(), String> {
    let operator_ids = operator
        .board_sample_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for record in records {
        for board_id in &record.board_sample_ids {
            if !operator_ids.contains(board_id.as_str()) {
                return Err(format!(
                    "scenario {} references board sample {:?} absent from the operator record",
                    record.scenario_id, board_id
                ));
            }
        }
        if record.scenario_id == "electrical-matrix"
            && record.status == ResultStatus::Pass
            && record.board_sample_ids.len() < 2
        {
            return Err(
                "passing electrical-matrix evidence requires more than one board sample".to_owned(),
            );
        }
    }
    Ok(())
}

fn validate_capture_file(
    evidence: &Path,
    relative: &str,
    require_nonempty: bool,
) -> Result<(), String> {
    let path = resolve_capture_path(evidence, relative)?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("could not inspect evidence file {relative:?}: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err(format!("evidence path is not a regular file: {relative:?}"));
    }
    if require_nonempty && metadata.len() == 0 {
        return Err(format!("passing evidence file is empty: {relative:?}"));
    }
    Ok(())
}

fn resolve_capture_path(evidence: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if relative_path_text(path)? != relative || !relative.starts_with("captures/") {
        return Err(format!(
            "evidence path must be canonical and below captures/: {relative:?}"
        ));
    }
    Ok(evidence.join(path))
}

fn validate_time_pair(
    started: Option<&str>,
    finished: Option<&str>,
    context: &str,
) -> Result<(), String> {
    let started = started.ok_or_else(|| format!("{context} is missing started_utc"))?;
    let finished = finished.ok_or_else(|| format!("{context} is missing finished_utc"))?;
    validate_utc_timestamp(started)?;
    validate_utc_timestamp(finished)?;
    if finished < started {
        return Err(format!("{context} finishes before it starts"));
    }
    Ok(())
}

fn validate_utc_timestamp(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let separators = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'Z'),
    ];
    if bytes.len() != 20
        || separators
            .iter()
            .any(|(index, expected)| bytes[*index] != *expected)
        || bytes.iter().enumerate().any(|(index, byte)| {
            !separators.iter().any(|(slot, _)| *slot == index) && !byte.is_ascii_digit()
        })
    {
        return Err(format!(
            "timestamp must use canonical UTC YYYY-MM-DDTHH:MM:SSZ: {value:?}"
        ));
    }
    let number = |start: usize, end: usize| -> u32 {
        value[start..end]
            .parse()
            .expect("validated decimal timestamp")
    };
    let year = number(0, 4);
    let month = number(5, 7);
    let day = number(8, 10);
    let hour = number(11, 13);
    let minute = number(14, 16);
    let second = number(17, 19);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    };
    if year < 2020 || day == 0 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return Err(format!("timestamp fields are out of range: {value:?}"));
    }
    Ok(())
}

fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn require_nonempty(value: &str, name: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        Err(format!(
            "{name} must be non-empty without surrounding whitespace"
        ))
    } else {
        Ok(())
    }
}

fn ensure_unique_nonempty(values: &[String], name: &str) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    for value in values {
        require_nonempty(value, name)?;
        if !unique.insert(value) {
            return Err(format!("{name} contains duplicate value {value:?}"));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lifecycle {
    Incomplete,
    Sealed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinalizeLifecycle {
    Incomplete,
    Sealed,
}

struct FinalizeLock {
    file: File,
}

impl FinalizeLock {
    fn acquire(evidence: &Path) -> Result<Self, String> {
        let lock_path = finalize_lock_path(evidence)?;
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&lock_path).map_err(|inspect_error| {
                    format!(
                        "could not inspect existing finalize lock {} after {error}: {inspect_error}",
                        lock_path.display()
                    )
                })?;
                if !metadata.file_type().is_file() {
                    return Err(format!(
                        "finalize lock is not a real regular file: {}",
                        lock_path.display()
                    ));
                }
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&lock_path)
                    .map_err(|open_error| {
                        format!(
                            "could not open existing finalize lock {}: {open_error}",
                            lock_path.display()
                        )
                    })?
            }
            Err(error) => {
                return Err(format!(
                    "could not create finalize lock {}: {error}",
                    lock_path.display()
                ));
            }
        };
        if !file
            .metadata()
            .map_err(|error| {
                format!(
                    "could not inspect opened finalize lock {}: {error}",
                    lock_path.display()
                )
            })?
            .file_type()
            .is_file()
        {
            return Err(format!(
                "opened finalize lock is not a regular file: {}",
                lock_path.display()
            ));
        }
        file.try_lock().map_err(|error| {
            format!(
                "could not acquire finalize lock {}; another finalizer may be active: {error}",
                lock_path.display()
            )
        })?;
        Ok(Self { file })
    }
}

impl Drop for FinalizeLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn finalize_lock_path(evidence: &Path) -> Result<PathBuf, String> {
    let parent = evidence.parent().ok_or_else(|| {
        format!(
            "powered-evidence directory has no parent for finalize lock: {}",
            evidence.display()
        )
    })?;
    let file_name = evidence.file_name().ok_or_else(|| {
        format!(
            "powered-evidence directory has no name for finalize lock: {}",
            evidence.display()
        )
    })?;
    let mut lock_name = file_name.to_os_string();
    lock_name.push(FINALIZE_LOCK_SUFFIX);
    Ok(parent.join(lock_name))
}

fn prepare_finalize_lifecycle(evidence: &Path) -> Result<FinalizeLifecycle, String> {
    let incomplete = regular_marker_exists(&evidence.join(INCOMPLETE_FILE))?;
    let sealed = regular_marker_exists(&evidence.join(SEALED_FILE))?;
    match (incomplete, sealed) {
        (true, false) => {
            validate_incomplete_recovery_tree(evidence)?;
            recover_incomplete_finalize_metadata(evidence)?;
            overwrite_existing_regular_file(
                &evidence.join(INCOMPLETE_FILE),
                INCOMPLETE_CONTENT.as_bytes(),
            )?;
            sync_directory(evidence)?;
            validate_evidence_tree(evidence, Lifecycle::Incomplete, false)?;
            Ok(FinalizeLifecycle::Incomplete)
        }
        (false, true) => {
            validate_evidence_tree(evidence, Lifecycle::Sealed, true)?;
            Ok(FinalizeLifecycle::Sealed)
        }
        (true, true) => Err(format!(
            "powered evidence contains both {INCOMPLETE_FILE:?} and {SEALED_FILE:?}; refusing to guess its lifecycle"
        )),
        (false, false) => Err(format!(
            "powered evidence contains neither {INCOMPLETE_FILE:?} nor {SEALED_FILE:?}; refusing to guess its lifecycle"
        )),
    }
}

fn regular_marker_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(format!(
            "powered-evidence lifecycle marker is not a real regular file: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "could not inspect powered-evidence lifecycle marker {}: {error}",
            path.display()
        )),
    }
}

fn recover_incomplete_finalize_metadata(evidence: &Path) -> Result<(), String> {
    let mut removed = false;
    for file_name in [INVENTORY_TEMP_FILE, INVENTORY_FILE] {
        let path = evidence.join(file_name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                fs::remove_file(&path).map_err(|error| {
                    format!(
                        "could not remove retryable finalize metadata {}: {error}",
                        path.display()
                    )
                })?;
                removed = true;
            }
            Ok(_) => {
                return Err(format!(
                    "retryable finalize metadata is not a real regular file: {}",
                    path.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect retryable finalize metadata {}: {error}",
                    path.display()
                ));
            }
        }
    }
    if removed {
        sync_directory(evidence)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InventoryExpectation {
    Absent,
    Present,
    OptionalForRecovery,
}

fn validate_evidence_tree(
    evidence: &Path,
    lifecycle: Lifecycle,
    require_inventory: bool,
) -> Result<(), String> {
    validate_evidence_tree_shape(
        evidence,
        lifecycle,
        if require_inventory {
            InventoryExpectation::Present
        } else {
            InventoryExpectation::Absent
        },
    )?;
    if lifecycle == Lifecycle::Incomplete {
        let marker = fs::read(evidence.join(INCOMPLETE_FILE)).map_err(|error| {
            format!(
                "could not read powered-evidence incomplete marker {}: {error}",
                evidence.join(INCOMPLETE_FILE).display()
            )
        })?;
        if marker != INCOMPLETE_CONTENT.as_bytes() {
            return Err("powered-evidence incomplete marker has unexpected content".to_owned());
        }
    }
    Ok(())
}

fn validate_incomplete_recovery_tree(evidence: &Path) -> Result<(), String> {
    validate_evidence_tree_shape(
        evidence,
        Lifecycle::Incomplete,
        InventoryExpectation::OptionalForRecovery,
    )
}

fn validate_evidence_tree_shape(
    evidence: &Path,
    lifecycle: Lifecycle,
    inventory: InventoryExpectation,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(evidence).map_err(|error| {
        format!(
            "could not inspect powered-evidence root {}: {error}",
            evidence.display()
        )
    })?;
    if !metadata.file_type().is_dir() {
        return Err("powered-evidence root must be a real directory".to_owned());
    }
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    collect_evidence_tree(evidence, evidence, &mut files, &mut directories)?;

    let expected_directories = BTreeSet::from([
        "captures".to_owned(),
        "records".to_owned(),
        "records/scenarios".to_owned(),
    ]);
    for directory in &directories {
        if !expected_directories.contains(directory) && !directory.starts_with("captures/") {
            return Err(format!(
                "powered-evidence contains unexpected directory {directory:?}"
            ));
        }
    }
    for required in expected_directories {
        if !directories.contains(&required) {
            return Err(format!(
                "powered-evidence is missing required directory {required:?}"
            ));
        }
    }

    let mut required_files = BTreeSet::from([MANIFEST_FILE.to_owned(), OPERATOR_FILE.to_owned()]);
    for scenario in SCENARIOS {
        required_files.insert(scenario_record_path(scenario.id));
    }
    required_files.insert(match lifecycle {
        Lifecycle::Incomplete => INCOMPLETE_FILE.to_owned(),
        Lifecycle::Sealed => SEALED_FILE.to_owned(),
    });
    if inventory == InventoryExpectation::Present {
        required_files.insert(INVENTORY_FILE.to_owned());
    }
    for required in &required_files {
        if !files.contains(required) {
            return Err(format!(
                "powered-evidence is missing required file {required:?}"
            ));
        }
    }

    let mut allowed_files = required_files;
    if inventory == InventoryExpectation::OptionalForRecovery {
        allowed_files.insert(INVENTORY_FILE.to_owned());
        allowed_files.insert(INVENTORY_TEMP_FILE.to_owned());
    }
    for file in &files {
        if !allowed_files.contains(file) && !file.starts_with("captures/") {
            return Err(format!(
                "powered-evidence contains unexpected file {file:?}"
            ));
        }
    }
    let forbidden_marker = match lifecycle {
        Lifecycle::Incomplete => SEALED_FILE,
        Lifecycle::Sealed => INCOMPLETE_FILE,
    };
    if files.contains(forbidden_marker) {
        return Err(format!(
            "powered-evidence contains conflicting marker {forbidden_marker:?}"
        ));
    }
    Ok(())
}

fn collect_evidence_tree(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<String>,
    directories: &mut BTreeSet<String>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("could not list {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| {
            format!(
                "could not inspect entry in {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("evidence traversal escaped through {}", path.display()))?;
        let relative = relative_path_text(relative)?;
        if metadata.file_type().is_symlink() {
            return Err(format!("powered-evidence contains symlink {relative:?}"));
        }
        if metadata.file_type().is_dir() {
            directories.insert(relative);
            collect_evidence_tree(root, &path, files, directories)?;
        } else if metadata.file_type().is_file() {
            files.insert(relative);
        } else {
            return Err(format!(
                "powered-evidence contains special file {relative:?}"
            ));
        }
    }
    Ok(())
}

fn inventory_files(evidence: &Path) -> Result<BTreeSet<String>, String> {
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    collect_evidence_tree(evidence, evidence, &mut files, &mut directories)?;
    files.remove(INVENTORY_FILE);
    files.remove(INVENTORY_TEMP_FILE);
    files.remove(INCOMPLETE_FILE);
    files.remove(SEALED_FILE);
    Ok(files)
}

fn render_inventory(evidence: &Path) -> Result<Vec<u8>, String> {
    let mut text = String::new();
    for relative in inventory_files(evidence)? {
        let digest = sha256_file(&evidence.join(&relative))?;
        text.push_str(&format!("{digest}  {relative}\n"));
    }
    Ok(text.into_bytes())
}

fn install_inventory_atomically(evidence: &Path) -> Result<(), String> {
    ensure_path_absent(&evidence.join(INVENTORY_FILE), "final inventory")?;
    ensure_path_absent(&evidence.join(INVENTORY_TEMP_FILE), "temporary inventory")?;
    let inventory = render_inventory(evidence)?;
    let temporary = evidence.join(INVENTORY_TEMP_FILE);
    let installed = evidence.join(INVENTORY_FILE);
    write_new(&temporary, &inventory)?;
    sync_directory(evidence)?;
    fs::rename(&temporary, &installed).map_err(|error| {
        format!(
            "could not atomically install inventory {} from {}: {error}",
            installed.display(),
            temporary.display()
        )
    })?;
    sync_directory(evidence)
}

fn ensure_path_absent(path: &Path, description: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not inspect {description} {}: {error}",
            path.display()
        )),
        Ok(_) => Err(format!(
            "{description} already exists at {}; finalize recovery must run first",
            path.display()
        )),
    }
}

fn verify_inventory(evidence: &Path) -> Result<(), String> {
    let path = evidence.join(INVENTORY_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let expected_files = inventory_files(evidence)?;
    let mut recorded = BTreeMap::new();
    let mut previous: Option<&str> = None;
    for line in text.lines() {
        let Some((digest, relative)) = line.split_once("  ") else {
            return Err("invalid powered-evidence inventory line".to_owned());
        };
        validate_sha256(digest)?;
        if relative_path_text(Path::new(relative))? != relative {
            return Err(format!("non-canonical inventory path {relative:?}"));
        }
        if previous.is_some_and(|previous| previous >= relative) {
            return Err("powered-evidence inventory is not strictly sorted".to_owned());
        }
        previous = Some(relative);
        if recorded
            .insert(relative.to_owned(), digest.to_owned())
            .is_some()
        {
            return Err(format!("duplicate inventory path {relative:?}"));
        }
    }
    if !text.is_empty() && !text.ends_with('\n') {
        return Err("powered-evidence inventory lacks final newline".to_owned());
    }
    if recorded.keys().cloned().collect::<BTreeSet<_>>() != expected_files {
        return Err("powered-evidence inventory has missing or unexpected paths".to_owned());
    }
    for (relative, digest) in recorded {
        if sha256_file(&evidence.join(&relative))? != digest {
            return Err(format!("powered-evidence digest mismatch for {relative}"));
        }
    }
    Ok(())
}

fn sealed_content(evidence: &Path, status: ResultStatus) -> Result<String, String> {
    Ok(format!(
        "{SCHEMA}\nqualification_status={}\nmanifest_sha256={}\ninventory_sha256={}\n",
        status.as_str(),
        sha256_file(&evidence.join(MANIFEST_FILE))?,
        sha256_file(&evidence.join(INVENTORY_FILE))?,
    ))
}

fn stage_seal_in_incomplete_marker(evidence: &Path, sealed: &[u8]) -> Result<(), String> {
    overwrite_existing_regular_file(&evidence.join(INCOMPLETE_FILE), sealed)?;
    sync_directory(evidence)
}

fn commit_staged_seal(evidence: &Path, expected: &[u8]) -> Result<(), String> {
    let incomplete = evidence.join(INCOMPLETE_FILE);
    let sealed = evidence.join(SEALED_FILE);
    ensure_path_absent(&sealed, "sealed lifecycle marker")?;
    let marker = fs::read(&incomplete).map_err(|error| {
        format!(
            "could not read staged powered-evidence seal {}: {error}",
            incomplete.display()
        )
    })?;
    if marker != expected {
        return Err("staged powered-evidence seal changed before commit".to_owned());
    }
    let metadata = fs::symlink_metadata(&incomplete).map_err(|error| {
        format!(
            "could not inspect staged powered-evidence seal {}: {error}",
            incomplete.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "staged powered-evidence seal is not a real regular file: {}",
            incomplete.display()
        ));
    }
    fs::rename(&incomplete, &sealed).map_err(|error| {
        format!(
            "could not atomically commit powered-evidence seal {} from {}: {error}",
            sealed.display(),
            incomplete.display()
        )
    })?;
    sync_directory(evidence)
}

fn overwrite_existing_regular_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
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

fn scenario_record_path(id: &str) -> String {
    format!("{SCENARIO_DIRECTORY}/{id}.json")
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

fn relative_path_text(path: &Path) -> Result<String, String> {
    let mut pieces = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(piece) => pieces.push(
                piece
                    .to_str()
                    .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))?,
            ),
            _ => {
                return Err(format!(
                    "path must be a confined canonical relative path: {}",
                    path.display()
                ));
            }
        }
    }
    if pieces.is_empty() {
        return Err("relative path must not be empty".to_owned());
    }
    Ok(pieces.join("/"))
}

fn unix_seconds_now() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| format!("system time is before Unix epoch: {error}"))
}

fn validate_sha256(value: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!("invalid canonical SHA-256 digest {value:?}"))
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("could not open {} for hashing: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("could not hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read JSON record {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not parse JSON record {}: {error}", path.display()))
}

fn write_json_new<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("could not serialize {}: {error}", path.display()))?;
    bytes.push(b'\n');
    write_new(path, &bytes)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn scenario(id: &str) -> &'static ScenarioDefinition {
        SCENARIOS
            .iter()
            .find(|definition| definition.id == id)
            .unwrap()
    }

    fn completely_bound_pass_record(definition: &ScenarioDefinition) -> ScenarioRecord {
        let serial = "captures/test/serial.log".to_owned();
        let peer = "captures/test/peer.json".to_owned();
        let logic = "captures/test/analyzer.bin".to_owned();
        let rf = "captures/test/rf-observer.bin".to_owned();
        let current = "captures/test/current.csv".to_owned();
        let mut record = scenario_template(definition);
        record.status = ResultStatus::Pass;
        record.serial_capture_files = vec![serial.clone()];
        record.peer_capture_files = vec![peer.clone()];
        record.logic_analyzer_capture_files = vec![logic.clone()];
        record.rf_observer_capture_files = vec![rf.clone()];
        record.current_measurement_files = vec![current.clone()];
        record.evidence_files = vec![
            serial.clone(),
            peer.clone(),
            logic.clone(),
            rf.clone(),
            current.clone(),
        ];
        for artifact_id in definition.artifact_ids {
            let readback = format!("captures/test/{artifact_id}-readback.bin");
            record.evidence_files.push(readback.clone());
            record.artifact_uses.push(ArtifactUse {
                artifact_id: (*artifact_id).to_owned(),
                declared_mode: "test-mode".to_owned(),
                observed_mode: "test-mode".to_owned(),
                flash_readback_path: readback,
                flash_readback_sha256: "0".repeat(64),
            });
        }
        let all_evidence = record.evidence_files.clone();
        for (check_id, check) in &mut record.checks {
            check.status = ResultStatus::Pass;
            check.evidence_files = all_evidence.clone();
            check.observation = match check_id.as_str() {
                "continuous-duration-at-least-24h" => {
                    Some(CheckObservation::ElapsedSeconds { seconds: 86_400 })
                }
                "configured-stall-seven-seconds" => {
                    Some(CheckObservation::ConfiguredStallMicroseconds {
                        microseconds: 7_000_000,
                    })
                }
                "offered-three-queued-two-dropped-one" => {
                    Some(CheckObservation::BackpressureCounters {
                        offered_during_stall: 3,
                        queued_during_stall: 2,
                        dropped_during_stall: 1,
                    })
                }
                _ => None,
            };
        }
        record
    }

    struct PeerFixture {
        temporary: TestDirectory,
        operator: OperatorRecord,
        record: ScenarioRecord,
        profile_environment: BTreeMap<String, String>,
        pinned_corpus: serde_json::Value,
        pinned_corpus_bytes: Vec<u8>,
        pinned_scenarios: BTreeMap<String, serde_json::Value>,
    }

    impl PeerFixture {
        fn single(prefix: &str) -> Self {
            let temporary = TestDirectory::new(prefix);
            fs::create_dir_all(temporary.path().join("captures/peer")).unwrap();
            fs::create_dir_all(temporary.path().join("captures/operator")).unwrap();
            let workspace = crate::workspace_root();
            let corpus_bytes = fs::read(workspace.join(PEER_CORPUS_FILE)).unwrap();
            let tool_bytes = fs::read(workspace.join(PEER_TOOL_FILE)).unwrap();
            let preserved_corpus = temporary.path().join("captures/operator/rnode-hil-v1.json");
            let preserved_tool = temporary.path().join("captures/operator/rnode_hil.py");
            fs::write(&preserved_corpus, &corpus_bytes).unwrap();
            fs::write(&preserved_tool, &tool_bytes).unwrap();
            let pinned_corpus: serde_json::Value = serde_json::from_slice(&corpus_bytes).unwrap();
            let pinned_scenarios = validate_pinned_peer_corpus(&pinned_corpus).unwrap();

            let mut operator = operator_template();
            operator.peer_corpus_path = "captures/operator/rnode-hil-v1.json".to_owned();
            operator.peer_corpus_sha256 = sha256_bytes(&corpus_bytes);
            operator.peer_tool_path = "captures/operator/rnode_hil.py".to_owned();
            operator.peer_tool_sha256 = sha256_bytes(&tool_bytes);
            operator.peer_conducted_power_dbm = Some(10);
            operator.peer_short_airtime_limit_basis_points = Some(500);
            operator.peer_long_airtime_limit_basis_points = Some(1_000);
            operator.peer_effective_short_airtime_limit_basis_points = Some(499);
            operator.peer_effective_long_airtime_limit_basis_points = Some(999);
            operator.peer_reported_preamble_symbols = Some(24);
            operator.region_basis = "test authorization".to_owned();

            let profile_environment = BTreeMap::from([
                (
                    "RETICULUM_LAB_RX_FREQUENCY_HZ".to_owned(),
                    "915000000".to_owned(),
                ),
                (
                    "RETICULUM_LAB_RX_BANDWIDTH_HZ".to_owned(),
                    "125000".to_owned(),
                ),
                (
                    "RETICULUM_LAB_RX_SPREADING_FACTOR".to_owned(),
                    "7".to_owned(),
                ),
                (
                    "RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR".to_owned(),
                    "5".to_owned(),
                ),
                (
                    "RETICULUM_LAB_RX_PREAMBLE_SYMBOLS".to_owned(),
                    "18".to_owned(),
                ),
            ]);
            let mut record = scenario_template(scenario("single-physical-frame"));
            record.status = ResultStatus::Pass;
            record.started_utc = Some("2026-07-15T11:59:00Z".to_owned());
            record.finished_utc = Some("2026-07-15T12:02:00Z".to_owned());
            record.artifact_uses.push(ArtifactUse {
                artifact_id: "normal".to_owned(),
                declared_mode: NORMAL_TARGET_MODE.to_owned(),
                observed_mode: NORMAL_TARGET_MODE.to_owned(),
                flash_readback_path: "captures/readback.bin".to_owned(),
                flash_readback_sha256: "0".repeat(64),
            });

            let mut manifest_paths = Vec::new();
            for expectation in SINGLE_PEER_RUNS {
                let directory = temporary
                    .path()
                    .join("captures/peer")
                    .join(expectation.name);
                fs::create_dir_all(&directory).unwrap();
                let mut manifest = peer_manifest_fixture(
                    pinned_scenarios.get(expectation.name).unwrap().clone(),
                    &operator,
                    &profile_environment,
                    preserved_corpus.clone(),
                    preserved_tool.clone(),
                );
                let transcript = peer_transcript_fixture(&manifest);
                manifest.transcript_sha256 = sha256_bytes(&transcript);
                fs::write(directory.join(PEER_TRANSCRIPT_FILE), &transcript).unwrap();
                fs::write(
                    directory.join(PEER_MANIFEST_FILE),
                    serde_json::to_vec_pretty(&manifest).unwrap(),
                )
                .unwrap();
                let manifest_path =
                    format!("captures/peer/{}/{PEER_MANIFEST_FILE}", expectation.name);
                let transcript_path =
                    format!("captures/peer/{}/{PEER_TRANSCRIPT_FILE}", expectation.name);
                record.peer_capture_files.push(manifest_path.clone());
                record.peer_capture_files.push(transcript_path.clone());
                record.evidence_files.push(manifest_path.clone());
                record.evidence_files.push(transcript_path);
                manifest_paths.push(manifest_path);
            }
            record
                .checks
                .get_mut("all-corpus-cases-run")
                .unwrap()
                .evidence_files = manifest_paths;

            Self {
                temporary,
                operator,
                record,
                profile_environment,
                pinned_corpus,
                pinned_corpus_bytes: corpus_bytes,
                pinned_scenarios,
            }
        }

        fn manifest_path(&self, scenario_name: &str) -> String {
            format!("captures/peer/{scenario_name}/{PEER_MANIFEST_FILE}")
        }

        fn read_manifest(&self, scenario_name: &str) -> PeerManifest {
            read_json(
                &resolve_capture_path(self.temporary.path(), &self.manifest_path(scenario_name))
                    .unwrap(),
            )
            .unwrap()
        }

        fn write_manifest(&self, scenario_name: &str, manifest: &PeerManifest) {
            fs::write(
                resolve_capture_path(self.temporary.path(), &self.manifest_path(scenario_name))
                    .unwrap(),
                serde_json::to_vec_pretty(manifest).unwrap(),
            )
            .unwrap();
        }

        fn validate(&self) -> Result<(), String> {
            validate_passing_peer_record(
                &self.record,
                &PeerValidationContext {
                    evidence: self.temporary.path(),
                    operator: &self.operator,
                    profile_environment: &self.profile_environment,
                    pinned_corpus: &self.pinned_corpus,
                    pinned_corpus_bytes: &self.pinned_corpus_bytes,
                    pinned_scenarios: &self.pinned_scenarios,
                    boot_local_generator_sha256: "unused-for-pinned-corpus",
                },
            )
        }
    }

    fn peer_manifest_fixture(
        scenario: serde_json::Value,
        operator: &OperatorRecord,
        profile_environment: &BTreeMap<String, String>,
        corpus_path: PathBuf,
        tool_path: PathBuf,
    ) -> PeerManifest {
        PeerManifest {
            schema: 1,
            status: "enqueued_not_rf_verified".to_owned(),
            started_utc: "2026-07-15T12:00:00Z".to_owned(),
            finished_utc: "2026-07-15T12:01:00Z".to_owned(),
            corpus: corpus_path.to_str().unwrap().to_owned(),
            corpus_sha256: operator.peer_corpus_sha256.clone(),
            tool: tool_path.to_str().unwrap().to_owned(),
            tool_sha256: operator.peer_tool_sha256.clone(),
            enqueued_steps: scenario["steps"].as_array().unwrap().len(),
            scenario,
            serial_port: "/dev/test-rnode".to_owned(),
            target_artifact_mode: NORMAL_TARGET_MODE.to_owned(),
            profile: PeerProfile {
                frequency_hz: profile_environment_value(
                    profile_environment,
                    "RETICULUM_LAB_RX_FREQUENCY_HZ",
                )
                .unwrap(),
                bandwidth_hz: profile_environment_value(
                    profile_environment,
                    "RETICULUM_LAB_RX_BANDWIDTH_HZ",
                )
                .unwrap(),
                spreading_factor: 7,
                coding_rate_denominator: 5,
                tx_power_dbm: operator.peer_conducted_power_dbm.unwrap(),
                expected_peer_preamble_symbols: operator.peer_reported_preamble_symbols.unwrap(),
                receiver_preamble_symbols: 18,
                short_airtime_limit_basis_points: operator
                    .peer_short_airtime_limit_basis_points
                    .unwrap(),
                long_airtime_limit_basis_points: operator
                    .peer_long_airtime_limit_basis_points
                    .unwrap(),
            },
            receiver_fragment_timeout_us: 7_000_000,
            receiver_maximum_frame_airtime_us: 1_000_000,
            peer_preamble_extension_us: 6_144,
            post_enqueue_observation_ms: 2_000,
            region_basis: operator.region_basis.clone(),
            antenna_or_load_attached: true,
            fresh_peer_reset_acknowledged: true,
            fresh_tracker_boot_acknowledged: true,
            independent_rf_observer_required: true,
            runtime: PeerRuntime {
                python_implementation: "CPython".to_owned(),
                python_version: "3.13.7".to_owned(),
                pyserial_version: "3.5".to_owned(),
                serial: PeerSerial {
                    baudrate: 115_200,
                    bytesize: 8,
                    parity: "N".to_owned(),
                    stopbits: 1,
                    timeout_seconds: 0.1,
                    write_timeout_seconds: 3.0,
                    xonxoff: false,
                    rtscts: false,
                    dsrdtr: false,
                },
            },
            device: PeerDevice {
                firmware_version: PEER_FIRMWARE_VERSION.to_owned(),
                firmware_version_bytes_hex: PEER_FIRMWARE_VERSION_BYTES_HEX.to_owned(),
                board: 1,
                platform: 1,
                mcu: 1,
            },
            peer_physical_timing: PeerPhysicalTiming {
                symbol_time_us: 1_024,
                symbol_rate: 976,
                preamble_symbols: 24,
                preamble_time_ms: 24,
                csma_slot_ms: 12,
                difs_ms: 24,
                effective_short_airtime_limit_basis_points: operator
                    .peer_effective_short_airtime_limit_basis_points
                    .unwrap(),
                effective_long_airtime_limit_basis_points: operator
                    .peer_effective_long_airtime_limit_basis_points
                    .unwrap(),
            },
            error: serde_json::Value::Null,
            transcript_sha256: "0".repeat(64),
        }
    }

    fn peer_transcript_fixture(manifest: &PeerManifest) -> Vec<u8> {
        let contract = peer_exchange_contract(manifest).unwrap();
        let mut frames = Vec::<(String, PeerFrame)>::new();
        for (index, (host, peer)) in contract.exchanges.into_iter().enumerate() {
            frames.push(("host_to_peer".to_owned(), host));
            if index == 14 {
                frames.push(("peer_to_host".to_owned(), contract.physical.clone()));
            }
            frames.push(("peer_to_host".to_owned(), peer));
            if index == 14 {
                frames.push((
                    "host_to_peer".to_owned(),
                    PeerFrame {
                        command: CMD_READY,
                        payload: vec![0],
                    },
                ));
                frames.push((
                    "peer_to_host".to_owned(),
                    PeerFrame {
                        command: CMD_READY,
                        payload: vec![1],
                    },
                ));
            }
        }
        let ready_host = PeerFrame {
            command: CMD_READY,
            payload: vec![0],
        };
        let ready_peer = PeerFrame {
            command: CMD_READY,
            payload: vec![1],
        };
        for payload in contract.data {
            frames.push(("host_to_peer".to_owned(), ready_host.clone()));
            frames.push(("peer_to_host".to_owned(), ready_peer.clone()));
            frames.push((
                "host_to_peer".to_owned(),
                PeerFrame {
                    command: CMD_DATA,
                    payload,
                },
            ));
        }
        frames.push(("host_to_peer".to_owned(), ready_host));
        frames.push(("peer_to_host".to_owned(), ready_peer));
        let mut output = Vec::new();
        for (sequence, (direction, frame)) in frames.into_iter().enumerate() {
            let entry = PeerTranscriptEntry {
                sequence: sequence as u64,
                utc: format!("2026-07-15T12:00:{:02}Z", sequence.min(59)),
                monotonic_ns: sequence as u64 + 1,
                direction,
                command: frame.command,
                payload_hex: lower_hex(&frame.payload),
                wire_hex: lower_hex(&kiss_wire(frame.command, &frame.payload)),
            };
            output.extend_from_slice(&serde_json::to_vec(&entry).unwrap());
            output.push(b'\n');
        }
        output
    }

    fn read_peer_transcript_entries(
        fixture: &PeerFixture,
        scenario_name: &str,
    ) -> Vec<PeerTranscriptEntry> {
        let path = fixture.temporary.path().join(format!(
            "captures/peer/{scenario_name}/{PEER_TRANSCRIPT_FILE}"
        ));
        fs::read(path)
            .unwrap()
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect()
    }

    fn write_peer_transcript_entries(
        fixture: &PeerFixture,
        scenario_name: &str,
        entries: &[PeerTranscriptEntry],
    ) {
        let mut bytes = Vec::new();
        for entry in entries {
            bytes.extend_from_slice(&serde_json::to_vec(entry).unwrap());
            bytes.push(b'\n');
        }
        let path = fixture.temporary.path().join(format!(
            "captures/peer/{scenario_name}/{PEER_TRANSCRIPT_FILE}"
        ));
        fs::write(path, &bytes).unwrap();
        let mut manifest = fixture.read_manifest(scenario_name);
        manifest.transcript_sha256 = sha256_bytes(&bytes);
        fixture.write_manifest(scenario_name, &manifest);
    }

    fn lower_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn append_tar_file(builder: &mut tar::Builder<File>, path: &str, bytes: &[u8]) {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, path, bytes).unwrap();
    }

    fn write_project_source_archive(path: &Path, corpus: &[u8], tool: &[u8], generator: &[u8]) {
        let mut builder = tar::Builder::new(File::create(path).unwrap());
        append_tar_file(&mut builder, PEER_CORPUS_FILE, corpus);
        append_tar_file(&mut builder, PEER_TOOL_FILE, tool);
        append_tar_file(&mut builder, BOOT_LOCAL_GENERATOR_FILE, generator);
        builder.finish().unwrap();
    }

    fn write_forged_peer_source_tar(path: &Path, revision: &str) {
        let mut builder = tar::Builder::new(File::create(path).unwrap());
        let record = format!("52 comment={revision}\n");
        assert_eq!(record.len(), 52);
        let mut header = tar::Header::new_ustar();
        header.set_size(record.len() as u64);
        header.set_mode(0o666);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::XGlobalHeader);
        header.set_cksum();
        builder
            .append_data(&mut header, "pax_global_header", record.as_bytes())
            .unwrap();
        append_tar_file(&mut builder, "README", b"pinned peer source\n");
        builder.finish().unwrap();
    }

    struct TestGitBundle {
        bundle: PathBuf,
        repository: PathBuf,
        revision: String,
        tree: String,
    }

    fn create_test_git_bundle(root: &Path, source: &[u8]) -> TestGitBundle {
        let repository = root.join("source-repository");
        fs::create_dir_all(&repository).unwrap();
        let run = |arguments: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(arguments)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                arguments,
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        run(&["init", "--quiet"]);
        fs::write(repository.join("firmware.cpp"), source).unwrap();
        run(&["add", "firmware.cpp"]);
        run(&[
            "-c",
            "user.name=Qualification Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "source",
        ]);
        let revision = run(&["rev-parse", "HEAD"]);
        let tree = run(&["show", "-s", "--format=%T", "HEAD"]);
        run(&["tag", "1.86"]);
        let bundle = root.join("peer-source.bundle");
        let output = Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["bundle", "create"])
            .arg(&bundle)
            .arg("refs/tags/1.86")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git bundle: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        TestGitBundle {
            bundle,
            repository,
            revision,
            tree,
        }
    }

    #[test]
    fn complete_peer_manifest_set_and_transcript_bindings_validate() {
        PeerFixture::single("pe-peer-valid").validate().unwrap();
    }

    #[test]
    fn failed_peer_manifest_status_is_rejected() {
        let fixture = PeerFixture::single("pe-peer-failed-status");
        let mut manifest = fixture.read_manifest("raw-header-only");
        manifest.status = "failed_after_enqueue".to_owned();
        fixture.write_manifest("raw-header-only", &manifest);
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn missing_or_unlisted_peer_transcript_is_rejected() {
        let mut unlisted = PeerFixture::single("pe-peer-unlisted-transcript");
        let transcript = "captures/peer/raw-header-only/peer-transcript.jsonl";
        unlisted
            .record
            .peer_capture_files
            .retain(|path| path != transcript);
        unlisted
            .record
            .evidence_files
            .retain(|path| path != transcript);
        assert!(unlisted.validate().is_err());

        let missing = PeerFixture::single("pe-peer-missing-transcript");
        fs::remove_file(missing.temporary.path().join(transcript)).unwrap();
        assert!(missing.validate().is_err());
    }

    #[test]
    fn peer_transcript_digest_mismatch_is_rejected() {
        let fixture = PeerFixture::single("pe-peer-transcript-digest");
        let mut manifest = fixture.read_manifest("raw-header-only");
        manifest.transcript_sha256 = "a".repeat(64);
        fixture.write_manifest("raw-header-only", &manifest);
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn wrong_peer_scenario_is_rejected() {
        let fixture = PeerFixture::single("pe-peer-wrong-scenario");
        let mut manifest = fixture.read_manifest("raw-header-only");
        manifest.scenario = fixture.pinned_scenarios["rnode-split-255"].clone();
        manifest.enqueued_steps = manifest.scenario["steps"].as_array().unwrap().len();
        fixture.write_manifest("raw-header-only", &manifest);
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn wrong_peer_target_mode_is_rejected() {
        let fixture = PeerFixture::single("pe-peer-wrong-mode");
        let mut manifest = fixture.read_manifest("raw-header-only");
        manifest.target_artifact_mode = BACKPRESSURE_TARGET_MODE.to_owned();
        fixture.write_manifest("raw-header-only", &manifest);
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn wrong_peer_rf_profile_is_rejected() {
        let fixture = PeerFixture::single("pe-peer-wrong-profile");
        let mut manifest = fixture.read_manifest("raw-header-only");
        manifest.profile.frequency_hz += 1;
        fixture.write_manifest("raw-header-only", &manifest);
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn peer_manifest_sources_must_name_operator_capture_copies() {
        let tool_fixture = PeerFixture::single("pe-peer-tool-source-path");
        let mut manifest = tool_fixture.read_manifest("raw-header-only");
        manifest.tool = crate::workspace_root()
            .join(PEER_TOOL_FILE)
            .to_str()
            .unwrap()
            .to_owned();
        tool_fixture.write_manifest("raw-header-only", &manifest);
        assert!(tool_fixture.validate().is_err());

        let corpus_fixture = PeerFixture::single("pe-peer-corpus-source-path");
        let mut manifest = corpus_fixture.read_manifest("raw-header-only");
        manifest.corpus = crate::workspace_root()
            .join(PEER_CORPUS_FILE)
            .to_str()
            .unwrap()
            .to_owned();
        corpus_fixture.write_manifest("raw-header-only", &manifest);
        assert!(corpus_fixture.validate().is_err());
    }

    #[test]
    fn self_consistent_transcript_with_wrong_data_payload_is_rejected() {
        let fixture = PeerFixture::single("pe-peer-wrong-transcript-data");
        let transcript_path = fixture
            .temporary
            .path()
            .join("captures/peer/raw-header-only/peer-transcript.jsonl");
        let bytes = fs::read(&transcript_path).unwrap();
        let mut entries = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<PeerTranscriptEntry>(line).unwrap())
            .collect::<Vec<_>>();
        let data = entries
            .iter_mut()
            .find(|entry| entry.direction == "host_to_peer" && entry.command == CMD_DATA)
            .unwrap();
        let mut payload = decode_lower_hex(&data.payload_hex, "test payload").unwrap();
        payload[0] ^= 1;
        data.payload_hex = lower_hex(&payload);
        data.wire_hex = lower_hex(&kiss_wire(CMD_DATA, &payload));
        let mut rewritten = Vec::new();
        for entry in entries {
            rewritten.extend_from_slice(&serde_json::to_vec(&entry).unwrap());
            rewritten.push(b'\n');
        }
        fs::write(&transcript_path, &rewritten).unwrap();
        let mut manifest = fixture.read_manifest("raw-header-only");
        manifest.transcript_sha256 = sha256_bytes(&rewritten);
        fixture.write_manifest("raw-header-only", &manifest);
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn self_consistent_transcript_with_reordered_request_and_reply_is_rejected() {
        let fixture = PeerFixture::single("pe-peer-reordered-request-reply");
        let mut entries = read_peer_transcript_entries(&fixture, "raw-header-only");
        let (first, rest) = entries.split_at_mut(1);
        let second = &mut rest[0];
        std::mem::swap(&mut first[0].direction, &mut second.direction);
        std::mem::swap(&mut first[0].command, &mut second.command);
        std::mem::swap(&mut first[0].payload_hex, &mut second.payload_hex);
        std::mem::swap(&mut first[0].wire_hex, &mut second.wire_hex);
        write_peer_transcript_entries(&fixture, "raw-header-only", &entries);
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn self_consistent_transcript_with_unsolicited_ready_is_rejected() {
        let fixture = PeerFixture::single("pe-peer-unsolicited-ready");
        let mut entries = read_peer_transcript_entries(&fixture, "raw-header-only");
        let ready_request = entries
            .iter()
            .position(|entry| entry.direction == "host_to_peer" && entry.command == CMD_READY)
            .unwrap();
        let mut unsolicited = entries[ready_request + 1].clone();
        unsolicited.direction = "peer_to_host".to_owned();
        entries.insert(ready_request, unsolicited);
        for (sequence, entry) in entries.iter_mut().enumerate() {
            entry.sequence = sequence as u64;
            entry.monotonic_ns = sequence as u64 + 1;
            entry.utc = format!("2026-07-15T12:00:{:02}Z", sequence.min(59));
        }
        write_peer_transcript_entries(&fixture, "raw-header-only", &entries);
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn peer_manifest_interval_must_be_inside_powered_scenario() {
        let fixture = PeerFixture::single("pe-peer-run-interval");
        let mut manifest = fixture.read_manifest("raw-header-only");
        manifest.started_utc = "2026-07-15T11:58:59Z".to_owned();
        fixture.write_manifest("raw-header-only", &manifest);
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn copied_repeated_peer_run_identity_is_rejected() {
        let fixture = PeerFixture::single("pe-peer-copied-repeat");
        let manifest = fixture.read_manifest("raw-header-only");
        let mut digests = BTreeMap::new();
        let mut times = BTreeMap::new();
        record_unique_peer_run(&mut digests, &mut times, "repeat", &manifest).unwrap();
        assert!(record_unique_peer_run(&mut digests, &mut times, "repeat", &manifest).is_err());

        let mut same_interval = manifest.clone();
        same_interval.transcript_sha256 = "b".repeat(64);
        assert!(
            record_unique_peer_run(&mut digests, &mut times, "repeat-2", &same_interval).is_ok()
        );
        assert!(
            record_unique_peer_run(&mut digests, &mut times, "repeat", &same_interval).is_err()
        );
    }

    #[test]
    fn cross_record_peer_digest_reuse_has_only_the_rnode_exact_exception() {
        let first = GlobalPeerEvidenceUse {
            powered_scenario_id: "single-physical-frame".to_owned(),
            peer_scenario_name: "raw-single-1".to_owned(),
            path: "captures/peer/run/peer-manifest.json".to_owned(),
        };
        let second = GlobalPeerEvidenceUse {
            powered_scenario_id: "receive-soak-24h".to_owned(),
            ..first.clone()
        };
        let mut observed = BTreeMap::new();
        record_global_peer_digest(&mut observed, "a".repeat(64), &first, "manifest").unwrap();
        assert!(
            record_global_peer_digest(&mut observed, "a".repeat(64), &second, "manifest").is_err()
        );

        let split = GlobalPeerEvidenceUse {
            powered_scenario_id: "split-packet".to_owned(),
            peer_scenario_name: "rnode-exact-500".to_owned(),
            path: "captures/peer/rnode-exact-500/peer-manifest.json".to_owned(),
        };
        let malformed = GlobalPeerEvidenceUse {
            powered_scenario_id: "malformed-and-semantic-rejection".to_owned(),
            ..split.clone()
        };
        let mut observed = BTreeMap::new();
        record_global_peer_digest(&mut observed, "b".repeat(64), &split, "manifest").unwrap();
        record_global_peer_digest(&mut observed, "b".repeat(64), &malformed, "manifest").unwrap();
    }

    #[test]
    fn boot_local_generator_source_is_schema_frozen() {
        validate_boot_local_generator_source(boot_local_generator::SOURCE_BYTES).unwrap();
        assert!(validate_boot_local_generator_source(b"different generator").is_err());
    }

    #[test]
    fn boot_local_corpus_is_regenerated_byte_for_byte() {
        let temporary = TestDirectory::new("pe-boot-local-regeneration");
        fs::create_dir_all(temporary.path().join("captures/boot-local-data")).unwrap();
        fs::create_dir_all(temporary.path().join("captures/operator")).unwrap();
        let workspace = crate::workspace_root();
        let pinned_bytes = fs::read(workspace.join(PEER_CORPUS_FILE)).unwrap();
        let pinned_corpus: serde_json::Value = serde_json::from_slice(&pinned_bytes).unwrap();
        let receiver =
            reticulum_rns_rete::Identity::from_seed(b"boot local verifier target").unwrap();
        let public_key = receiver.public_key();
        let endpoint = reticulum_rns_rete::InitialEmbeddedNode::new(
            reticulum_rns_rete::Identity::from_seed(b"boot local verifier target").unwrap(),
            "reticulum-rs-firmware",
            &["heltec-tracker-v2", "lab-rx"],
            reticulum_rns_rete::EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        let destination_hash: [u8; 16] = *endpoint.destination_hash().as_bytes();
        let generator_sha256 = sha256_bytes(boot_local_generator::SOURCE_BYTES);
        let generated = boot_local_generator::generate(
            public_key,
            destination_hash,
            BootLocalInputs {
                base_corpus: &pinned_bytes,
                source_sha256: &generator_sha256,
            },
        )
        .unwrap();
        let corpus_relative = "captures/boot-local-data/boot-local-data.json";
        let corpus_path = temporary.path().join(corpus_relative);
        fs::write(&corpus_path, &generated.corpus_bytes).unwrap();
        let corpus: serde_json::Value = serde_json::from_slice(&generated.corpus_bytes).unwrap();

        let mut operator = operator_template();
        operator.peer_corpus_sha256 = sha256_bytes(&pinned_bytes);
        operator.peer_tool_sha256 = "0".repeat(64);
        operator.peer_conducted_power_dbm = Some(10);
        operator.peer_short_airtime_limit_basis_points = Some(500);
        operator.peer_long_airtime_limit_basis_points = Some(1_000);
        operator.peer_effective_short_airtime_limit_basis_points = Some(499);
        operator.peer_effective_long_airtime_limit_basis_points = Some(999);
        operator.peer_reported_preamble_symbols = Some(24);
        operator.region_basis = "test authorization".to_owned();
        let profile = BTreeMap::from([
            (
                "RETICULUM_LAB_RX_FREQUENCY_HZ".to_owned(),
                "915000000".to_owned(),
            ),
            (
                "RETICULUM_LAB_RX_BANDWIDTH_HZ".to_owned(),
                "125000".to_owned(),
            ),
            (
                "RETICULUM_LAB_RX_SPREADING_FACTOR".to_owned(),
                "7".to_owned(),
            ),
            (
                "RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR".to_owned(),
                "5".to_owned(),
            ),
            (
                "RETICULUM_LAB_RX_PREAMBLE_SYMBOLS".to_owned(),
                "18".to_owned(),
            ),
        ]);
        let mut manifest = peer_manifest_fixture(
            corpus["scenarios"][0].clone(),
            &operator,
            &profile,
            corpus_path.clone(),
            temporary.path().join("captures/operator/rnode_hil.py"),
        );
        manifest.corpus_sha256 = sha256_bytes(&generated.corpus_bytes);
        let mut record = scenario_template(scenario("malformed-and-semantic-rejection"));
        record.peer_capture_files.push(corpus_relative.to_owned());
        record.evidence_files.push(corpus_relative.to_owned());
        for check in ["all-corpus-cases-run", "boot-local-data-processed"] {
            record
                .checks
                .get_mut(check)
                .unwrap()
                .evidence_files
                .push(corpus_relative.to_owned());
        }
        validate_boot_local_corpus(
            temporary.path(),
            &record,
            &manifest,
            "captures/peer/boot-local-data/peer-manifest.json",
            &pinned_corpus,
            &pinned_bytes,
            &generator_sha256,
        )
        .unwrap();

        let semantically_equal_bytes = serde_json::to_vec(&corpus).unwrap();
        assert_ne!(semantically_equal_bytes, generated.corpus_bytes);
        fs::write(&corpus_path, &semantically_equal_bytes).unwrap();
        manifest.corpus_sha256 = sha256_bytes(&semantically_equal_bytes);
        assert!(
            validate_boot_local_corpus(
                temporary.path(),
                &record,
                &manifest,
                "captures/peer/boot-local-data/peer-manifest.json",
                &pinned_corpus,
                &pinned_bytes,
                &generator_sha256,
            )
            .is_err()
        );

        fs::write(&corpus_path, &generated.corpus_bytes).unwrap();
        manifest.corpus_sha256 = sha256_bytes(&generated.corpus_bytes);

        let mut forged = corpus;
        let payload = forged["scenarios"][0]["steps"][0]["payload_hex"]
            .as_str()
            .unwrap();
        let mut replacement = decode_lower_hex(payload, "test boot-local payload").unwrap();
        replacement[0] ^= 1;
        forged["scenarios"][0]["steps"][0]["payload_hex"] =
            serde_json::Value::String(lower_hex(&replacement));
        manifest.scenario = forged["scenarios"][0].clone();
        let mut forged_bytes = serde_json::to_vec_pretty(&forged).unwrap();
        forged_bytes.push(b'\n');
        fs::write(&corpus_path, &forged_bytes).unwrap();
        manifest.corpus_sha256 = sha256_bytes(&forged_bytes);
        assert!(
            validate_boot_local_corpus(
                temporary.path(),
                &record,
                &manifest,
                "captures/peer/boot-local-data/peer-manifest.json",
                &pinned_corpus,
                &pinned_bytes,
                &generator_sha256,
            )
            .is_err()
        );
    }

    #[test]
    fn incomplete_expected_peer_scenario_set_is_rejected() {
        let mut fixture = PeerFixture::single("pe-peer-incomplete-set");
        let manifest = fixture.manifest_path("raw-single-254");
        let transcript = "captures/peer/raw-single-254/peer-transcript.jsonl";
        fixture
            .record
            .peer_capture_files
            .retain(|path| path != &manifest && path != transcript);
        fixture
            .record
            .evidence_files
            .retain(|path| path != &manifest && path != transcript);
        fixture
            .record
            .checks
            .get_mut("all-corpus-cases-run")
            .unwrap()
            .evidence_files
            .retain(|path| path != &manifest);
        assert!(fixture.validate().is_err());
    }

    #[test]
    fn unrelated_self_consistent_peer_corpus_and_tool_copies_are_rejected() {
        let temporary = TestDirectory::new("pe-peer-operator-artifacts");
        fs::create_dir_all(temporary.path().join("captures/operator")).unwrap();
        let workspace = crate::workspace_root();
        let corpus = fs::read(workspace.join(PEER_CORPUS_FILE)).unwrap();
        let tool = fs::read(workspace.join(PEER_TOOL_FILE)).unwrap();
        let image_path = "captures/operator/peer-firmware.bin";
        let source_path = "captures/operator/peer-source.bundle";
        let corpus_path = "captures/operator/rnode-hil-v1.json";
        let tool_path = "captures/operator/rnode_hil.py";
        fs::write(temporary.path().join(image_path), b"peer firmware image").unwrap();
        fs::write(temporary.path().join(source_path), b"peer source proof").unwrap();
        fs::write(temporary.path().join(corpus_path), &corpus).unwrap();
        fs::write(temporary.path().join(tool_path), &tool).unwrap();

        let mut operator = operator_template();
        operator.peer_firmware_image_path = image_path.to_owned();
        operator.peer_firmware_sha256 = sha256_file(&temporary.path().join(image_path)).unwrap();
        operator.peer_firmware_source_path = source_path.to_owned();
        operator.peer_firmware_source_sha256 =
            sha256_file(&temporary.path().join(source_path)).unwrap();
        operator.peer_corpus_path = corpus_path.to_owned();
        operator.peer_corpus_sha256 = sha256_bytes(&corpus);
        operator.peer_tool_path = tool_path.to_owned();
        operator.peer_tool_sha256 = sha256_bytes(&tool);
        validate_operator_peer_artifact_hashes(temporary.path(), &operator, &corpus, &tool)
            .unwrap();

        let unrelated_corpus = br#"{"schema":3,"scenarios":[]}"#;
        fs::write(temporary.path().join(corpus_path), unrelated_corpus).unwrap();
        operator.peer_corpus_sha256 = sha256_bytes(unrelated_corpus);
        assert!(
            validate_operator_peer_artifact_hashes(temporary.path(), &operator, &corpus, &tool)
                .is_err()
        );

        fs::write(temporary.path().join(corpus_path), &corpus).unwrap();
        operator.peer_corpus_sha256 = sha256_bytes(&corpus);
        let unrelated_tool = b"def main():\n    return 0\n";
        fs::write(temporary.path().join(tool_path), unrelated_tool).unwrap();
        operator.peer_tool_sha256 = sha256_bytes(unrelated_tool);
        assert!(
            validate_operator_peer_artifact_hashes(temporary.path(), &operator, &corpus, &tool)
                .is_err()
        );
    }

    #[test]
    fn peer_firmware_source_bundle_is_self_contained_and_object_verified() {
        let temporary = TestDirectory::new("pe-peer-source-bundle");
        fs::create_dir(temporary.path()).unwrap();
        let fixture = create_test_git_bundle(temporary.path(), b"pinned source\n");
        verify_peer_firmware_source_bundle(&fixture.bundle, &fixture.revision, &fixture.tree)
            .unwrap();
        assert!(
            verify_peer_firmware_source_bundle(&fixture.bundle, &"1".repeat(40), &fixture.tree,)
                .is_err()
        );
        assert!(
            verify_peer_firmware_source_bundle(
                &fixture.bundle,
                &fixture.revision,
                &"2".repeat(40),
            )
            .is_err()
        );
    }

    #[test]
    fn ambient_git_redirection_cannot_borrow_expected_repository_objects() {
        const CHILD_MODE: &str = "RETICULUM_PEER_GIT_POISON_CHILD_MODE";
        const CHILD_BUNDLE: &str = "RETICULUM_PEER_GIT_POISON_BUNDLE";
        const CHILD_REVISION: &str = "RETICULUM_PEER_GIT_POISON_REVISION";
        const CHILD_TREE: &str = "RETICULUM_PEER_GIT_POISON_TREE";
        const TEST_NAME: &str = concat!(
            "phase1_powered_evidence::tests::",
            "ambient_git_redirection_cannot_borrow_expected_repository_objects"
        );
        const POISONED_ENVIRONMENT: &[&str] = &[
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_REPLACE_REF_BASE",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_GLOBAL",
            "HOME",
            "XDG_CONFIG_HOME",
        ];

        if let Ok(mode) = std::env::var(CHILD_MODE) {
            let bundle = PathBuf::from(std::env::var_os(CHILD_BUNDLE).unwrap());
            let revision = std::env::var(CHILD_REVISION).unwrap();
            let tree = std::env::var(CHILD_TREE).unwrap();
            let result = verify_peer_firmware_source_bundle(&bundle, &revision, &tree);
            match mode.as_str() {
                "reject-redirected-unrelated" => assert!(result.is_err()),
                "accept-valid-despite-all-poison" => result.unwrap(),
                _ => panic!("unknown poisoned Git child mode {mode:?}"),
            }
            return;
        }

        let temporary = TestDirectory::new("pe-peer-git-env-poison");
        let official_root = temporary.path().join("official");
        let unrelated_root = temporary.path().join("unrelated");
        fs::create_dir_all(&official_root).unwrap();
        fs::create_dir_all(&unrelated_root).unwrap();
        let official = create_test_git_bundle(&official_root, b"official pinned source\n");
        let unrelated = create_test_git_bundle(&unrelated_root, b"unrelated source\n");
        assert_ne!(official.revision, unrelated.revision);

        let poison_home = temporary.path().join("poison-home");
        let poison_xdg = temporary.path().join("poison-xdg");
        let poison_hooks = temporary.path().join("poison-hooks");
        fs::create_dir_all(poison_xdg.join("git")).unwrap();
        fs::create_dir_all(&poison_hooks).unwrap();
        let poison_config = format!(
            "[protocol \"file\"]\n\tallow = never\n[core]\n\thooksPath = {}\n",
            poison_hooks.display()
        );
        fs::create_dir_all(&poison_home).unwrap();
        fs::write(poison_home.join(".gitconfig"), &poison_config).unwrap();
        fs::write(poison_xdg.join("git/config"), &poison_config).unwrap();
        let poison_system = temporary.path().join("poison-system.gitconfig");
        let poison_global = temporary.path().join("poison-global.gitconfig");
        fs::write(&poison_system, &poison_config).unwrap();
        fs::write(&poison_global, &poison_config).unwrap();

        let child = |mode: &str, bundle: &Path, redirect: &TestGitBundle, all_poison: bool| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.arg("--exact").arg(TEST_NAME).arg("--nocapture");
            for variable in POISONED_ENVIRONMENT {
                command.env_remove(variable);
            }
            command
                .env(CHILD_MODE, mode)
                .env(CHILD_BUNDLE, bundle)
                .env(CHILD_REVISION, &official.revision)
                .env(CHILD_TREE, &official.tree)
                .env("GIT_DIR", redirect.repository.join(".git"))
                .env("GIT_WORK_TREE", &redirect.repository);
            if all_poison {
                command
                    .env("GIT_COMMON_DIR", redirect.repository.join(".git"))
                    .env(
                        "GIT_OBJECT_DIRECTORY",
                        redirect.repository.join(".git/objects"),
                    )
                    .env(
                        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                        redirect.repository.join(".git/objects"),
                    )
                    .env("GIT_NAMESPACE", "poison")
                    .env("GIT_REPLACE_REF_BASE", "refs/replace/poison")
                    .env("GIT_CONFIG_COUNT", "1")
                    .env("GIT_CONFIG_KEY_0", "protocol.file.allow")
                    .env("GIT_CONFIG_VALUE_0", "never")
                    .env("GIT_CONFIG_PARAMETERS", "'protocol.file.allow'='never'")
                    .env("GIT_CONFIG_NOSYSTEM", "0")
                    .env("GIT_CONFIG_SYSTEM", &poison_system)
                    .env("GIT_CONFIG_GLOBAL", &poison_global)
                    .env("HOME", &poison_home)
                    .env("XDG_CONFIG_HOME", &poison_xdg);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "poisoned Git child {mode} failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };

        child(
            "reject-redirected-unrelated",
            &unrelated.bundle,
            &official,
            false,
        );
        child(
            "accept-valid-despite-all-poison",
            &official.bundle,
            &unrelated,
            true,
        );
    }

    #[test]
    fn forged_pax_commit_header_is_not_peer_source_proof() {
        let temporary = TestDirectory::new("pe-peer-forged-source-tar");
        fs::create_dir(temporary.path()).unwrap();
        let forged = temporary.path().join("forged-source.tar");
        write_forged_peer_source_tar(&forged, PEER_FIRMWARE_REVISION);
        assert!(
            verify_peer_firmware_source_bundle(
                &forged,
                PEER_FIRMWARE_REVISION,
                PEER_FIRMWARE_ROOT_TREE,
            )
            .is_err()
        );
    }

    #[test]
    fn project_peer_sources_are_read_from_the_bound_archive_not_workspace() {
        let temporary = TestDirectory::new("pe-archived-project-peer-sources");
        fs::create_dir_all(temporary.path().join("bundle")).unwrap();
        fs::create_dir_all(temporary.path().join("workspace/interop/vectors")).unwrap();
        fs::create_dir_all(temporary.path().join("workspace/interop/python")).unwrap();
        let archived_corpus = b"archived corpus bytes";
        let archived_tool = b"archived tool bytes";
        let archived_generator = b"archived generator bytes";
        write_project_source_archive(
            &temporary.path().join("bundle/source.tar"),
            archived_corpus,
            archived_tool,
            archived_generator,
        );
        fs::write(
            temporary.path().join("workspace").join(PEER_CORPUS_FILE),
            b"drifted workspace corpus",
        )
        .unwrap();
        fs::write(
            temporary.path().join("workspace").join(PEER_TOOL_FILE),
            b"drifted workspace tool",
        )
        .unwrap();
        let binding = EvidenceBundleBinding {
            kind: "normal-pressure".to_owned(),
            schema: "test".to_owned(),
            canonical_path: temporary.path().join("bundle").to_str().unwrap().to_owned(),
            manifest_file: "manifest.json".to_owned(),
            manifest_sha256: "0".repeat(64),
            git_commit: "0".repeat(40),
            git_root_tree: "1".repeat(40),
            profile_environment: BTreeMap::new(),
            artifacts: Vec::new(),
        };
        let sources = read_project_peer_sources(&binding).unwrap();
        assert_eq!(sources.corpus, archived_corpus);
        assert_eq!(sources.tool, archived_tool);
        assert_eq!(sources.boot_local_generator, archived_generator);
    }

    #[test]
    fn parser_accepts_only_hardware_inert_command_shapes() {
        assert!(matches!(
            parse_cli(strings(&[
                "init",
                "--normal-pressure-bundle",
                "normal",
                "--closure-bundle",
                "closure",
                "--output",
                "evidence",
            ]))
            .unwrap(),
            Cli::Init { .. }
        ));
        assert_eq!(
            parse_cli(strings(&["finalize", "--evidence", "evidence"])).unwrap(),
            Cli::Finalize {
                evidence: PathBuf::from("evidence")
            }
        );
        assert_eq!(
            parse_cli(strings(&["verify", "--evidence", "evidence"])).unwrap(),
            Cli::Verify {
                evidence: PathBuf::from("evidence")
            }
        );
        for rejected in [
            strings(&["verify", "--evidence", "evidence", "--port", "/dev/tty0"]),
            strings(&["flash", "--evidence", "evidence"]),
            strings(&["finalize", "--evidence", "evidence", "--force", "yes"]),
            strings(&["init", "--output", "evidence"]),
        ] {
            assert!(parse_cli(rejected).is_err());
        }
    }

    #[test]
    fn scenario_template_is_honestly_not_run_and_partial_pass_is_rejected() {
        let definition = &SCENARIOS[0];
        let template = scenario_template(definition);
        assert_eq!(template.status, ResultStatus::NotRun);
        assert!(
            template
                .checks
                .values()
                .all(|check| check.status == ResultStatus::NotRun
                    && check.evidence_files.is_empty()
                    && check.observation.is_none())
        );

        let mut partial = template;
        partial.status = ResultStatus::Pass;
        partial.started_utc = Some("2026-07-15T12:00:00Z".to_owned());
        partial.finished_utc = Some("2026-07-15T12:01:00Z".to_owned());
        partial.board_sample_ids = strings(&["tracker-a"]);
        assert!(
            validate_scenario_record(
                Path::new("/does-not-matter"),
                definition,
                &partial,
                &BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn passing_checks_require_classified_scenario_evidence() {
        let definition = scenario("cold-boot-and-silence");
        let mut record = completely_bound_pass_record(definition);
        validate_check_records(definition, &record).unwrap();

        record
            .checks
            .get_mut("heap-stable")
            .unwrap()
            .evidence_files
            .clear();
        assert!(validate_check_records(definition, &record).is_err());

        let mut record = completely_bound_pass_record(definition);
        record.checks.get_mut("heap-stable").unwrap().evidence_files =
            strings(&["captures/test/missing.log"]);
        assert!(validate_check_records(definition, &record).is_err());

        let mut record = completely_bound_pass_record(definition);
        record
            .evidence_files
            .push("captures/test/generic.log".to_owned());
        record.checks.get_mut("heap-stable").unwrap().evidence_files =
            strings(&["captures/test/generic.log"]);
        assert!(validate_check_records(definition, &record).is_err());
    }

    #[test]
    fn common_checks_require_matching_typed_capture_roles() {
        let definition = scenario("cold-boot-and-silence");
        let mut record = completely_bound_pass_record(definition);
        record
            .checks
            .get_mut("no-prohibited-sx1262-tx-command")
            .unwrap()
            .evidence_files = record.serial_capture_files.clone();
        assert!(validate_check_records(definition, &record).is_err());

        record
            .checks
            .get_mut("no-prohibited-sx1262-tx-command")
            .unwrap()
            .evidence_files = record.logic_analyzer_capture_files.clone();
        validate_check_records(definition, &record).unwrap();

        let mut record = completely_bound_pass_record(definition);
        let last_readback = record.artifact_uses[0].flash_readback_path.clone();
        record
            .checks
            .get_mut("artifact-mode-and-readback-bound")
            .unwrap()
            .evidence_files
            .retain(|path| path != &last_readback);
        assert!(validate_check_records(definition, &record).is_err());
    }

    #[test]
    fn every_generated_check_has_an_explicit_evidence_role_policy() {
        for definition in SCENARIOS {
            for check_id in expected_checks(definition) {
                assert!(
                    !required_evidence_roles(&check_id).is_empty(),
                    "scenario {} check {check_id} lacks an evidence-role policy",
                    definition.id
                );
            }
        }
    }

    #[test]
    fn backpressure_counters_bind_both_serial_and_peer_evidence() {
        let definition = scenario("bounded-backpressure");
        let mut record = completely_bound_pass_record(definition);
        record
            .checks
            .get_mut("offered-three-queued-two-dropped-one")
            .unwrap()
            .evidence_files = record.serial_capture_files.clone();
        assert!(validate_check_records(definition, &record).is_err());

        let mut evidence = record.serial_capture_files.clone();
        evidence.extend(record.peer_capture_files.clone());
        record
            .checks
            .get_mut("offered-three-queued-two-dropped-one")
            .unwrap()
            .evidence_files = evidence;
        validate_check_records(definition, &record).unwrap();
    }

    #[test]
    fn electrical_checks_bind_current_and_analyzer_evidence() {
        let definition = scenario("electrical-matrix");
        let mut record = completely_bound_pass_record(definition);

        record
            .checks
            .get_mut("calibrated-current-measurement")
            .unwrap()
            .evidence_files = record.logic_analyzer_capture_files.clone();
        assert!(validate_check_records(definition, &record).is_err());
        record
            .checks
            .get_mut("calibrated-current-measurement")
            .unwrap()
            .evidence_files = record.current_measurement_files.clone();

        record
            .checks
            .get_mut("safety-pin-timing-measured")
            .unwrap()
            .evidence_files = record.current_measurement_files.clone();
        assert!(validate_check_records(definition, &record).is_err());
        record
            .checks
            .get_mut("safety-pin-timing-measured")
            .unwrap()
            .evidence_files = record.logic_analyzer_capture_files.clone();

        record
            .checks
            .get_mut("all-four-selections-measured")
            .unwrap()
            .evidence_files = record.current_measurement_files.clone();
        assert!(validate_check_records(definition, &record).is_err());
        let mut evidence = record.current_measurement_files.clone();
        evidence.extend(record.logic_analyzer_capture_files.clone());
        record
            .checks
            .get_mut("all-four-selections-measured")
            .unwrap()
            .evidence_files = evidence;
        validate_check_records(definition, &record).unwrap();
    }

    #[test]
    fn not_run_checks_reject_partial_bindings_and_observations() {
        let definition = scenario("cold-boot-and-silence");
        let mut record = scenario_template(definition);
        record
            .evidence_files
            .push("captures/test/serial.log".to_owned());
        record
            .serial_capture_files
            .push("captures/test/serial.log".to_owned());
        record
            .checks
            .get_mut("heap-stable")
            .unwrap()
            .evidence_files
            .push("captures/test/serial.log".to_owned());
        assert!(validate_check_records(definition, &record).is_err());

        let mut record = scenario_template(definition);
        record.checks.get_mut("heap-stable").unwrap().observation =
            Some(CheckObservation::ElapsedSeconds { seconds: 1 });
        assert!(validate_check_records(definition, &record).is_err());
    }

    #[test]
    fn failed_checks_require_classified_nonempty_capture_bindings() {
        let definition = scenario("cold-boot-and-silence");
        let mut record = completely_bound_pass_record(definition);
        let check = record.checks.get_mut("heap-stable").unwrap();
        check.status = ResultStatus::Fail;
        check.evidence_files.clear();
        assert!(validate_check_records(definition, &record).is_err());

        record.checks.get_mut("heap-stable").unwrap().evidence_files =
            record.serial_capture_files.clone();
        validate_check_records(definition, &record).unwrap();

        let temporary = TestDirectory::new("pe-empty-failed-capture");
        populate_template_tree(temporary.path());
        let relative = "captures/failed/serial.log";
        fs::create_dir(temporary.path().join("captures/failed")).unwrap();
        fs::write(temporary.path().join(relative), b"").unwrap();
        assert!(validate_capture_file(temporary.path(), relative, true).is_err());
    }

    #[test]
    fn soak_pass_requires_at_least_24_hours() {
        let definition = scenario("receive-soak-24h");
        let mut record = completely_bound_pass_record(definition);
        let check = record
            .checks
            .get_mut("continuous-duration-at-least-24h")
            .unwrap();
        check.observation = Some(CheckObservation::ElapsedSeconds { seconds: 86_399 });
        assert!(validate_check_records(definition, &record).is_err());

        record
            .checks
            .get_mut("continuous-duration-at-least-24h")
            .unwrap()
            .observation = Some(CheckObservation::ElapsedSeconds { seconds: 86_400 });
        validate_check_records(definition, &record).unwrap();
    }

    #[test]
    fn backpressure_pass_requires_exact_stall_and_counter_observations() {
        let definition = scenario("bounded-backpressure");
        let mut record = completely_bound_pass_record(definition);
        validate_check_records(definition, &record).unwrap();

        record
            .checks
            .get_mut("configured-stall-seven-seconds")
            .unwrap()
            .observation = Some(CheckObservation::ConfiguredStallMicroseconds {
            microseconds: 6_999_999,
        });
        assert!(validate_check_records(definition, &record).is_err());

        for counters in [(2, 2, 1), (3, 1, 1), (3, 2, 0)] {
            let mut record = completely_bound_pass_record(definition);
            record
                .checks
                .get_mut("offered-three-queued-two-dropped-one")
                .unwrap()
                .observation = Some(CheckObservation::BackpressureCounters {
                offered_during_stall: counters.0,
                queued_during_stall: counters.1,
                dropped_during_stall: counters.2,
            });
            assert!(validate_check_records(definition, &record).is_err());
        }
    }

    #[test]
    fn observations_are_rejected_on_checks_without_machine_policy() {
        let definition = scenario("cold-boot-and-silence");
        let mut record = completely_bound_pass_record(definition);
        record.checks.get_mut("heap-stable").unwrap().observation =
            Some(CheckObservation::ElapsedSeconds { seconds: 86_400 });
        assert!(validate_check_records(definition, &record).is_err());
    }

    #[test]
    fn scenario_boards_must_belong_to_operator_and_electrical_pass_needs_two() {
        let mut operator = operator_template();
        operator.board_sample_ids = strings(&["tracker-a", "tracker-b"]);
        let mut electrical = scenario_template(
            SCENARIOS
                .iter()
                .find(|scenario| scenario.id == "electrical-matrix")
                .unwrap(),
        );
        electrical.status = ResultStatus::Pass;
        electrical.board_sample_ids = strings(&["tracker-a"]);
        assert!(validate_record_board_ids(&operator, &[electrical.clone()]).is_err());

        electrical.board_sample_ids = strings(&["tracker-a", "tracker-b"]);
        validate_record_board_ids(&operator, &[electrical.clone()]).unwrap();

        electrical.board_sample_ids = strings(&["tracker-a", "tracker-c"]);
        assert!(validate_record_board_ids(&operator, &[electrical]).is_err());
    }

    #[test]
    fn timestamp_and_confined_path_validation_are_strict() {
        validate_utc_timestamp("2026-07-15T12:34:56Z").unwrap();
        validate_utc_timestamp("2024-02-29T12:34:56Z").unwrap();
        validate_utc_timestamp("2400-02-29T12:34:56Z").unwrap();
        for value in [
            "2026-07-15 12:34:56Z",
            "2026-13-15T12:34:56Z",
            "2026-07-15T25:34:56Z",
            "2026-07-15T12:34:56+00:00",
            "2026-02-29T12:34:56Z",
            "2024-02-30T12:34:56Z",
            "2026-04-31T12:34:56Z",
            "2100-02-29T12:34:56Z",
        ] {
            assert!(validate_utc_timestamp(value).is_err());
        }
        assert_eq!(
            relative_path_text(Path::new("captures/run/serial.log")).unwrap(),
            "captures/run/serial.log"
        );
        for value in ["../outside", "captures/../outside", "/absolute", "."] {
            assert!(relative_path_text(Path::new(value)).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn evidence_tree_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let temporary = TestDirectory::new("powered-evidence-symlink");
        populate_template_tree(temporary.path());
        symlink(
            temporary.path().join(MANIFEST_FILE),
            temporary.path().join(CAPTURE_DIRECTORY).join("alias"),
        )
        .unwrap();
        assert!(validate_evidence_tree(temporary.path(), Lifecycle::Incomplete, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn evidence_tree_rejects_special_files() {
        use std::os::unix::net::UnixListener;

        let temporary = TestDirectory::new("pe-special");
        populate_template_tree(temporary.path());
        let socket = temporary.path().join(CAPTURE_DIRECTORY).join("s");
        let _listener = UnixListener::bind(socket).unwrap();
        assert!(validate_evidence_tree(temporary.path(), Lifecycle::Incomplete, false).is_err());
    }

    #[test]
    fn inventory_is_sorted_exact_and_detects_tampering() {
        let temporary = TestDirectory::new("powered-evidence-inventory");
        populate_template_tree(temporary.path());
        let capture = temporary.path().join(CAPTURE_DIRECTORY).join("serial.log");
        fs::write(&capture, b"serial evidence\n").unwrap();
        install_inventory_atomically(temporary.path()).unwrap();
        validate_evidence_tree(temporary.path(), Lifecycle::Incomplete, true).unwrap();
        verify_inventory(temporary.path()).unwrap();
        let inventory = fs::read_to_string(temporary.path().join(INVENTORY_FILE)).unwrap();
        let paths = inventory
            .lines()
            .map(|line| line.split_once("  ").unwrap().1)
            .collect::<Vec<_>>();
        assert!(paths.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(!paths.contains(&INVENTORY_FILE));
        assert!(!paths.contains(&INCOMPLETE_FILE));

        fs::write(capture, b"tampered\n").unwrap();
        assert!(verify_inventory(temporary.path()).is_err());
    }

    #[test]
    fn inventory_rejects_unrecorded_extra_capture() {
        let temporary = TestDirectory::new("powered-evidence-extra");
        populate_template_tree(temporary.path());
        install_inventory_atomically(temporary.path()).unwrap();
        fs::write(
            temporary.path().join(CAPTURE_DIRECTORY).join("late.log"),
            b"late\n",
        )
        .unwrap();
        assert!(verify_inventory(temporary.path()).is_err());
    }

    #[test]
    fn finalize_lock_is_single_writer_and_persistent_outside_evidence() {
        let temporary = TestDirectory::new("pe-finalize-lock");
        populate_template_tree(temporary.path());
        let lock_path = finalize_lock_path(temporary.path()).unwrap();
        assert_eq!(lock_path.parent(), temporary.path().parent());
        assert_ne!(lock_path.parent(), Some(temporary.path()));

        let first = FinalizeLock::acquire(temporary.path()).unwrap();
        assert!(lock_path.is_file());
        assert!(FinalizeLock::acquire(temporary.path()).is_err());
        drop(first);

        let second = FinalizeLock::acquire(temporary.path()).unwrap();
        assert!(lock_path.is_file());
        drop(second);
        assert!(lock_path.is_file());
    }

    #[test]
    fn interrupted_inventory_and_staged_seal_are_retryable() {
        let temporary = TestDirectory::new("pe-finalize-retry");
        populate_template_tree(temporary.path());
        let root = temporary.path();
        fs::write(root.join(INVENTORY_TEMP_FILE), b"partial inventory").unwrap();
        fs::write(root.join(INVENTORY_FILE), b"stale inventory").unwrap();
        fs::write(root.join(INCOMPLETE_FILE), b"partially staged seal").unwrap();

        assert!(validate_evidence_tree(root, Lifecycle::Incomplete, false).is_err());
        assert_eq!(
            prepare_finalize_lifecycle(root).unwrap(),
            FinalizeLifecycle::Incomplete
        );
        assert!(!root.join(INVENTORY_TEMP_FILE).exists());
        assert!(!root.join(INVENTORY_FILE).exists());
        assert_eq!(
            fs::read(root.join(INCOMPLETE_FILE)).unwrap(),
            INCOMPLETE_CONTENT.as_bytes()
        );
        validate_evidence_tree(root, Lifecycle::Incomplete, false).unwrap();

        install_inventory_atomically(root).unwrap();
        stage_seal_in_incomplete_marker(root, b"staged seal\n").unwrap();
        assert!(root.join(INCOMPLETE_FILE).is_file());
        assert!(!root.join(SEALED_FILE).exists());
        assert_eq!(
            prepare_finalize_lifecycle(root).unwrap(),
            FinalizeLifecycle::Incomplete
        );
        assert!(!root.join(INVENTORY_FILE).exists());
        assert_eq!(
            fs::read(root.join(INCOMPLETE_FILE)).unwrap(),
            INCOMPLETE_CONTENT.as_bytes()
        );
    }

    #[test]
    fn marker_rename_is_the_seal_commit_and_sealed_state_is_idempotent() {
        let temporary = TestDirectory::new("pe-seal-commit");
        populate_template_tree(temporary.path());
        let root = temporary.path();
        install_inventory_atomically(root).unwrap();
        let seal = b"complete seal\n";

        stage_seal_in_incomplete_marker(root, seal).unwrap();
        assert!(root.join(INCOMPLETE_FILE).is_file());
        assert!(!root.join(SEALED_FILE).exists());
        commit_staged_seal(root, seal).unwrap();

        assert!(!root.join(INCOMPLETE_FILE).exists());
        assert_eq!(fs::read(root.join(SEALED_FILE)).unwrap(), seal);
        validate_evidence_tree(root, Lifecycle::Sealed, true).unwrap();
        verify_inventory(root).unwrap();
        assert_eq!(
            prepare_finalize_lifecycle(root).unwrap(),
            FinalizeLifecycle::Sealed
        );
        assert_eq!(
            prepare_finalize_lifecycle(root).unwrap(),
            FinalizeLifecycle::Sealed
        );
    }

    #[test]
    fn ambiguous_or_unknown_finalize_state_fails_closed() {
        let both = TestDirectory::new("pe-both-markers");
        populate_template_tree(both.path());
        fs::write(both.path().join(SEALED_FILE), b"seal\n").unwrap();
        assert!(prepare_finalize_lifecycle(both.path()).is_err());
        assert!(both.path().join(INCOMPLETE_FILE).exists());
        assert!(both.path().join(SEALED_FILE).exists());

        let neither = TestDirectory::new("pe-neither-marker");
        populate_template_tree(neither.path());
        fs::remove_file(neither.path().join(INCOMPLETE_FILE)).unwrap();
        assert!(prepare_finalize_lifecycle(neither.path()).is_err());

        let unknown = TestDirectory::new("pe-unknown-finalize-file");
        populate_template_tree(unknown.path());
        fs::write(unknown.path().join(INVENTORY_FILE), b"retryable inventory").unwrap();
        fs::write(unknown.path().join("unexpected"), b"must remain").unwrap();
        let marker_before = fs::read(unknown.path().join(INCOMPLETE_FILE)).unwrap();
        let inventory_before = fs::read(unknown.path().join(INVENTORY_FILE)).unwrap();
        assert!(prepare_finalize_lifecycle(unknown.path()).is_err());
        assert!(unknown.path().join("unexpected").exists());
        assert_eq!(
            fs::read(unknown.path().join(INCOMPLETE_FILE)).unwrap(),
            marker_before
        );
        assert_eq!(
            fs::read(unknown.path().join(INVENTORY_FILE)).unwrap(),
            inventory_before
        );
    }

    #[cfg(unix)]
    #[test]
    fn finalize_recovery_rejects_symlinked_machine_metadata() {
        use std::os::unix::fs::symlink;

        let temporary = TestDirectory::new("pe-finalize-metadata-link");
        populate_template_tree(temporary.path());
        symlink(
            temporary.path().join(INCOMPLETE_FILE),
            temporary.path().join(INVENTORY_TEMP_FILE),
        )
        .unwrap();
        assert!(prepare_finalize_lifecycle(temporary.path()).is_err());
        assert!(
            fs::symlink_metadata(temporary.path().join(INVENTORY_TEMP_FILE))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    fn populate_template_tree(root: &Path) {
        fs::create_dir(root).unwrap();
        fs::create_dir(root.join("records")).unwrap();
        fs::create_dir(root.join(SCENARIO_DIRECTORY)).unwrap();
        fs::create_dir(root.join(CAPTURE_DIRECTORY)).unwrap();
        fs::write(root.join(INCOMPLETE_FILE), INCOMPLETE_CONTENT).unwrap();
        fs::write(root.join(MANIFEST_FILE), b"{}\n").unwrap();
        fs::write(root.join(OPERATOR_FILE), b"{}\n").unwrap();
        for scenario in SCENARIOS {
            fs::write(root.join(scenario_record_path(scenario.id)), b"{}\n").unwrap();
        }
    }

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "{prefix}-{}-{}",
                std::process::id(),
                unix_seconds_now().unwrap()
            ));
            let _ = fs::remove_dir_all(&path);
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
            if let Ok(lock_path) = finalize_lock_path(&self.path) {
                let _ = fs::remove_file(lock_path);
            }
        }
    }
}
