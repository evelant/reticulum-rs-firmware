use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};

use object::{Object, ObjectSection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{phase1_image, phase1_source, phase1_tooling};

const SCHEMA: &str = "reticulum.phase1-rx-hil.artifacts.v2";
const COMPLETE_CONTENT: &str = "reticulum.phase1-rx-hil.artifacts.v2\n";
const INCOMPLETE_FILE: &str = "artifact-preparation.incomplete";
const COMPLETE_FILE: &str = "artifact-preparation.complete";
const MANIFEST_FILE: &str = "artifact-preparation.json";
const PREPARED_HASH_FILE: &str = "prepared-artifacts.sha256";

const ARTIFACT_RUSTFLAGS: &str = "-C link-arg=-nostartfiles -Z emit-stack-sizes";
const ARTIFACT_RUSTFLAG_ARGUMENTS: &[&str] =
    &["-C", "link-arg=-nostartfiles", "-Z", "emit-stack-sizes"];
const MAXIMUM_STACK_FRAME_BYTES: u64 = 49_152;

const PACKAGE: &str = "reticulum-heltec-tracker-v2";
const NORMAL_BINARY: &str = "reticulum-heltec-tracker-v2-lab-rx";
const BACKPRESSURE_BINARY: &str = "reticulum-heltec-tracker-v2-lab-rx-backpressure";
const TARGET: &str = "xtensa-esp32s3-none-elf";
const NORMAL_FEATURE: &str = "lab-rx";
const BACKPRESSURE_FEATURE: &str = "lab-rx-backpressure";
const STALL_ENV: &str = "RETICULUM_LAB_RX_BACKPRESSURE_STALL_US";

const PROFILE_ENV: &[(&str, ProfileValueKind)] = &[
    ("RETICULUM_LAB_RX_FREQUENCY_HZ", ProfileValueKind::Unsigned),
    (
        "RETICULUM_LAB_RX_SPREADING_FACTOR",
        ProfileValueKind::Unsigned,
    ),
    ("RETICULUM_LAB_RX_BANDWIDTH_HZ", ProfileValueKind::Unsigned),
    (
        "RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR",
        ProfileValueKind::Unsigned,
    ),
    (
        "RETICULUM_LAB_RX_PREAMBLE_SYMBOLS",
        ProfileValueKind::Unsigned,
    ),
    (
        "RETICULUM_LAB_RX_EXPLICIT_HEADER",
        ProfileValueKind::Boolean,
    ),
    ("RETICULUM_LAB_RX_CRC", ProfileValueKind::Boolean),
    ("RETICULUM_LAB_RX_IQ_INVERTED", ProfileValueKind::Boolean),
];

const PREPARED_FILES: &[&str] = &[
    "artifact-preparation.json",
    "backpressure-artifact/build.log",
    "backpressure-artifact/firmware.elf",
    "backpressure-artifact/firmware.sha256",
    "backpressure-artifact/flash-image-address.txt",
    "backpressure-artifact/flash-image-bytes.txt",
    "backpressure-artifact/flash-image.bin",
    "backpressure-artifact/flash-image.sha256",
    "backpressure-artifact/save-image.log",
    "build.log",
    "firmware.elf",
    "firmware.sha256",
    "flash-image-address.txt",
    "flash-image-bytes.txt",
    "flash-image.bin",
    "flash-image.sha256",
    "save-image.log",
    "source.sha256",
    "source.tar",
    "tool-and-source-versions.txt",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProfileValueKind {
    Unsigned,
    Boolean,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Cli {
    Prepare {
        output: PathBuf,
        backpressure_stall_us: u64,
    },
    Verify {
        bundle: PathBuf,
    },
    InspectElf {
        elf: PathBuf,
        mode: InspectionMode,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ArtifactMode {
    LabRx,
    LabRxBackpressureHil,
}

/// Static ELF contract selected by the separately named build artifact.
///
/// Keep this separate from [`ArtifactMode`]. `ArtifactMode` is serialized in
/// the normal/backpressure qualification-bundle v2 schema; adding closure
/// artifacts to it would silently widen that frozen schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InspectionMode {
    Normal,
    Backpressure,
    ElectricalLdoUnboosted,
    ElectricalLdoBoosted,
    ElectricalDcdcUnboosted,
    ElectricalDcdcBoosted,
    ReturnedFaultOneBoot,
    ReturnedFaultRepeatUntilQuarantine,
    ResetJournalCorrupt,
    ResetJournalTorn,
}

impl InspectionMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "lab-rx",
            Self::Backpressure => "lab-rx-backpressure-hil",
            Self::ElectricalLdoUnboosted => "lab-rx-electrical-hil-ldo-unboosted",
            Self::ElectricalLdoBoosted => "lab-rx-electrical-hil-ldo-boosted",
            Self::ElectricalDcdcUnboosted => "lab-rx-electrical-hil-dcdc-unboosted",
            Self::ElectricalDcdcBoosted => "lab-rx-electrical-hil-dcdc-boosted",
            Self::ReturnedFaultOneBoot => "lab-rx-returned-fault-hil-one-boot",
            Self::ReturnedFaultRepeatUntilQuarantine => {
                "lab-rx-returned-fault-hil-repeat-until-quarantine"
            }
            Self::ResetJournalCorrupt => "lab-rx-reset-journal-corrupt-hil",
            Self::ResetJournalTorn => "lab-rx-reset-journal-torn-hil",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "lab-rx" => Ok(Self::Normal),
            "lab-rx-backpressure-hil" => Ok(Self::Backpressure),
            "lab-rx-electrical-hil-ldo-unboosted" => Ok(Self::ElectricalLdoUnboosted),
            "lab-rx-electrical-hil-ldo-boosted" => Ok(Self::ElectricalLdoBoosted),
            "lab-rx-electrical-hil-dcdc-unboosted" => Ok(Self::ElectricalDcdcUnboosted),
            "lab-rx-electrical-hil-dcdc-boosted" => Ok(Self::ElectricalDcdcBoosted),
            "lab-rx-returned-fault-hil-one-boot" => Ok(Self::ReturnedFaultOneBoot),
            "lab-rx-returned-fault-hil-repeat-until-quarantine" => {
                Ok(Self::ReturnedFaultRepeatUntilQuarantine)
            }
            "lab-rx-reset-journal-corrupt-hil" => Ok(Self::ResetJournalCorrupt),
            "lab-rx-reset-journal-torn-hil" => Ok(Self::ResetJournalTorn),
            _ => Err(format!(
                "unsupported artifact inspection mode {value:?}; expected one of {}",
                Self::ALL
                    .iter()
                    .map(|mode| mode.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    const ALL: [Self; 10] = [
        Self::Normal,
        Self::Backpressure,
        Self::ElectricalLdoUnboosted,
        Self::ElectricalLdoBoosted,
        Self::ElectricalDcdcUnboosted,
        Self::ElectricalDcdcBoosted,
        Self::ReturnedFaultOneBoot,
        Self::ReturnedFaultRepeatUntilQuarantine,
        Self::ResetJournalCorrupt,
        Self::ResetJournalTorn,
    ];

    const fn is_full_stack(self) -> bool {
        !matches!(self, Self::ResetJournalCorrupt | Self::ResetJournalTorn)
    }

    const fn electrical_identity(self) -> Option<&'static str> {
        match self {
            Self::ElectricalLdoUnboosted => {
                Some("lab-rx-electrical-hil;regulator=ldo;rx_gain=unboosted")
            }
            Self::ElectricalLdoBoosted => {
                Some("lab-rx-electrical-hil;regulator=ldo;rx_gain=boosted")
            }
            Self::ElectricalDcdcUnboosted => {
                Some("lab-rx-electrical-hil;regulator=dcdc;rx_gain=unboosted")
            }
            Self::ElectricalDcdcBoosted => {
                Some("lab-rx-electrical-hil;regulator=dcdc;rx_gain=boosted")
            }
            _ => None,
        }
    }

    const fn returned_fault_identity(self) -> Option<&'static str> {
        match self {
            Self::ReturnedFaultOneBoot => Some(
                "lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=one-boot",
            ),
            Self::ReturnedFaultRepeatUntilQuarantine => Some(
                "lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=repeat-until-quarantine",
            ),
            _ => None,
        }
    }
}

impl From<ArtifactMode> for InspectionMode {
    fn from(mode: ArtifactMode) -> Self {
        match mode {
            ArtifactMode::LabRx => Self::Normal,
            ArtifactMode::LabRxBackpressureHil => Self::Backpressure,
        }
    }
}

impl ArtifactMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::LabRx => "lab-rx",
            Self::LabRxBackpressureHil => "lab-rx-backpressure-hil",
        }
    }

    const fn feature(self) -> &'static str {
        match self {
            Self::LabRx => NORMAL_FEATURE,
            Self::LabRxBackpressureHil => BACKPRESSURE_FEATURE,
        }
    }

    const fn binary(self) -> &'static str {
        match self {
            Self::LabRx => NORMAL_BINARY,
            Self::LabRxBackpressureHil => BACKPRESSURE_BINARY,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct FileRecord {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ElfSize {
    pub(crate) text: u64,
    pub(crate) data: u64,
    pub(crate) bss: u64,
    pub(crate) total: u64,
    pub(crate) maximum_stack_frame: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRecord {
    mode: ArtifactMode,
    feature: String,
    elf: FileRecord,
    flash_image: FileRecord,
    flash_image_address: u32,
    size: ElfSize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ReproducibilityRecord {
    canary_mode: ArtifactMode,
    independent_source_archive_extraction: bool,
    independent_target_directory: bool,
    independent_cargo_home: bool,
    elf_sha256: String,
    flash_image_sha256: String,
    byte_for_byte: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct BuildRecipe {
    normal_cargo_arguments: Vec<String>,
    backpressure_cargo_arguments: Vec<String>,
    chip: String,
    flash_size: String,
    flash_mode: String,
    flash_frequency: String,
    xtal_frequency: String,
    minimum_chip_revision: String,
    image_format: String,
    merged: bool,
    skip_padding: bool,
    espflash_config_policy: String,
    isolated_target_directories: bool,
    rustflags: String,
    encoded_rustflags: bool,
    environment_cleared: bool,
    ambient_environment_allowlist: Vec<String>,
    explicit_build_environment_names: Vec<String>,
    environment_policy: String,
    source_date_epoch_microseconds: String,
    build_root_remap: String,
    rustup_home_remap: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactManifest {
    schema: String,
    prepared_unix_seconds: u64,
    git_commit: String,
    git_root_tree: String,
    worktree_clean: bool,
    tools: BTreeMap<String, String>,
    profile_environment: BTreeMap<String, String>,
    backpressure_stall_us: u64,
    source_archive: FileRecord,
    build_recipe: BuildRecipe,
    reproducibility: ReproducibilityRecord,
    normal: ArtifactRecord,
    backpressure: ArtifactRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedFileBinding {
    pub(crate) path: String,
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedArtifactBinding {
    pub(crate) id: String,
    pub(crate) mode: String,
    pub(crate) elf: VerifiedFileBinding,
    pub(crate) flash_image: VerifiedFileBinding,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedBundleBinding {
    pub(crate) schema: String,
    pub(crate) git_commit: String,
    pub(crate) git_root_tree: String,
    pub(crate) profile_environment: BTreeMap<String, String>,
    pub(crate) artifacts: Vec<VerifiedArtifactBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandSpec {
    program: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

impl CommandSpec {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.env_clear().args(&self.args).envs(&self.env);
        command
    }

    fn rendered(&self) -> String {
        let mut rendered = self.program.clone();
        for argument in &self.args {
            rendered.push(' ');
            rendered.push_str(&format!("{argument:?}"));
        }
        rendered
    }
}

pub(crate) fn run(args: Vec<String>, root: &Path) -> ExitCode {
    let result = match parse_cli(args) {
        Ok(Cli::Prepare {
            output,
            backpressure_stall_us,
        }) => prepare(root, &output, backpressure_stall_us),
        Ok(Cli::Verify { bundle }) => verify_bundle(root, &bundle, true),
        Ok(Cli::InspectElf { elf, mode }) => inspect_elf(&elf, mode).map(|size| {
            println!(
                "ok: {} artifact {} text={} data={} bss={} total={} maximum_stack_frame={}",
                mode.as_str(),
                elf.display(),
                size.text,
                size.data,
                size.bss,
                size.total,
                size.maximum_stack_frame
            );
        }),
        Err(error) => Err(error),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}

fn usage() -> &'static str {
    "usage:\n  cargo run --locked -p xtask -- phase1-rx-hil-artifacts prepare --output <absent-directory> --backpressure-stall-us <u64>\n  cargo run --locked -p xtask -- phase1-rx-hil-artifacts verify --bundle <directory>\n  cargo run --locked -p xtask -- phase1-rx-hil-artifacts inspect-elf --mode <artifact-mode> --elf <path>\n\nartifact modes:\n  lab-rx\n  lab-rx-backpressure-hil\n  lab-rx-electrical-hil-{ldo|dcdc}-{unboosted|boosted}\n  lab-rx-returned-fault-hil-{one-boot|repeat-until-quarantine}\n  lab-rx-reset-journal-{corrupt|torn}-hil"
}

fn parse_cli(args: Vec<String>) -> Result<Cli, String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err("missing phase1-rx-hil-artifacts subcommand".to_owned());
    };
    let flags = parse_flags(&args[1..])?;
    match subcommand {
        "prepare" => {
            require_exact_flags(&flags, &["--output", "--backpressure-stall-us"])?;
            let stall_text = flags
                .get("--backpressure-stall-us")
                .expect("required flag checked");
            let backpressure_stall_us =
                parse_canonical_u64("--backpressure-stall-us", stall_text, false)?;
            Ok(Cli::Prepare {
                output: PathBuf::from(flags.get("--output").expect("required flag checked")),
                backpressure_stall_us,
            })
        }
        "verify" => {
            require_exact_flags(&flags, &["--bundle"])?;
            Ok(Cli::Verify {
                bundle: PathBuf::from(flags.get("--bundle").expect("required flag checked")),
            })
        }
        "inspect-elf" => {
            require_exact_flags(&flags, &["--elf", "--mode"])?;
            Ok(Cli::InspectElf {
                elf: PathBuf::from(flags.get("--elf").expect("required flag checked")),
                mode: InspectionMode::parse(flags.get("--mode").expect("required flag checked"))?,
            })
        }
        _ => Err(format!(
            "unknown phase1-rx-hil-artifacts subcommand {subcommand:?}"
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

fn parse_canonical_u64(name: &str, value: &str, allow_zero: bool) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{name} must be a canonical unsigned decimal integer"))?;
    if parsed.to_string() != value || (!allow_zero && parsed == 0) {
        return Err(format!(
            "{name} must be a canonical {}unsigned decimal integer",
            if allow_zero { "" } else { "non-zero " }
        ));
    }
    Ok(parsed)
}

fn prepare(root: &Path, output_arg: &Path, backpressure_stall_us: u64) -> Result<(), String> {
    let profile = capture_profile_environment()?;
    let source_identity = phase1_source::clean_source_identity(root)?;
    let git_commit = source_identity.commit.clone();
    let git_root_tree = source_identity.root_tree.clone();
    let source_date_epoch_microseconds =
        phase1_tooling::source_date_epoch_for_commit(root, &git_commit)?;
    let tools = collect_and_validate_tools(root)?;
    let qualification_environment = phase1_tooling::QualificationEnvironment::capture()?;

    let output = absolute_from(root, output_arg);
    ensure_output_location_does_not_dirty_source(root, &output)?;
    if output.exists() {
        return Err(format!(
            "qualification output already exists and will not be overwritten: {}",
            output.display()
        ));
    }
    let parent = output
        .parent()
        .ok_or_else(|| format!("output has no parent: {}", output.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "could not create qualification output parent {}: {error}",
            parent.display()
        )
    })?;
    fs::create_dir(&output)
        .map_err(|error| format!("could not create output {}: {error}", output.display()))?;
    write_new(
        &output.join(INCOMPLETE_FILE),
        b"preparation did not complete; this directory is not qualification evidence\n",
    )?;

    let backpressure_dir = output.join("backpressure-artifact");
    fs::create_dir(&backpressure_dir).map_err(|error| {
        format!(
            "could not create backpressure artifact directory {}: {error}",
            backpressure_dir.display()
        )
    })?;

    write_tool_versions(&output, &git_commit, &git_root_tree, &tools)?;
    phase1_source::create_source_archive(
        root,
        &output.join("source.tar"),
        &git_commit,
        &git_root_tree,
    )?;
    write_hash_sidecar(
        &output.join("source.tar"),
        &output.join("source.sha256"),
        "source.tar",
    )?;

    let build_root = TemporaryDirectory::below(&env::temp_dir(), "phase1-rx-hil-build")?;
    phase1_source::ensure_path_outside_workspace(root, build_root.path())?;
    let source_root = build_root.path().join("source");
    fs::create_dir(&source_root).map_err(|error| {
        format!(
            "could not create archived source build root {}: {error}",
            source_root.display()
        )
    })?;
    phase1_source::extract_source_archive(&output.join("source.tar"), &source_root)?;
    phase1_source::reject_ambient_ancestor_cargo_configs(&source_root)?;
    let cargo_home = build_root.path().join("cargo-home");
    fs::create_dir(&cargo_home).map_err(|error| {
        format!(
            "could not create isolated Cargo home {}: {error}",
            cargo_home.display()
        )
    })?;
    phase1_tooling::create_controlled_tmpdir(build_root.path())?;
    let build_context = phase1_tooling::CargoBuildContext::new(
        build_root.path(),
        &cargo_home,
        &source_date_epoch_microseconds,
        &qualification_environment,
    )?;
    let normal_target = build_root.path().join("normal");
    let backpressure_target = build_root.path().join("backpressure");
    let espflash_context = phase1_image::OfflineEspflashContext::create(build_root.path())?;

    let normal_elf = output.join("firmware.elf");
    let normal_image = output.join("flash-image.bin");
    let normal_build = build_spec(
        &profile,
        ArtifactMode::LabRx,
        None,
        &normal_target,
        &build_context,
    )?;
    validate_offline_artifact_command(&normal_build)?;
    run_logged(&normal_build, &source_root, &output.join("build.log"))?;
    copy_built_elf(&normal_target, ArtifactMode::LabRx, &normal_elf)?;
    let normal_save = save_image_spec(
        &normal_elf,
        &normal_image,
        &espflash_context,
        &qualification_environment,
    );
    validate_offline_artifact_command(&normal_save)?;
    run_logged(
        &normal_save,
        espflash_context.workdir(),
        &output.join("save-image.log"),
    )?;
    write_artifact_sidecars(&output, &normal_elf, &normal_image)?;

    let backpressure_elf = backpressure_dir.join("firmware.elf");
    let backpressure_image = backpressure_dir.join("flash-image.bin");
    let backpressure_build = build_spec(
        &profile,
        ArtifactMode::LabRxBackpressureHil,
        Some(backpressure_stall_us),
        &backpressure_target,
        &build_context,
    )?;
    validate_offline_artifact_command(&backpressure_build)?;
    run_logged(
        &backpressure_build,
        &source_root,
        &backpressure_dir.join("build.log"),
    )?;
    copy_built_elf(
        &backpressure_target,
        ArtifactMode::LabRxBackpressureHil,
        &backpressure_elf,
    )?;
    let backpressure_save = save_image_spec(
        &backpressure_elf,
        &backpressure_image,
        &espflash_context,
        &qualification_environment,
    );
    validate_offline_artifact_command(&backpressure_save)?;
    run_logged(
        &backpressure_save,
        espflash_context.workdir(),
        &backpressure_dir.join("save-image.log"),
    )?;
    write_artifact_sidecars(&backpressure_dir, &backpressure_elf, &backpressure_image)?;

    let normal_size = inspect_elf(&normal_elf, InspectionMode::Normal)?;
    let backpressure_size = inspect_elf(&backpressure_elf, InspectionMode::Backpressure)?;
    let normal = ArtifactRecord {
        mode: ArtifactMode::LabRx,
        feature: NORMAL_FEATURE.to_owned(),
        elf: file_record(&output, &normal_elf)?,
        flash_image: file_record(&output, &normal_image)?,
        flash_image_address: 0,
        size: normal_size,
    };
    let backpressure = ArtifactRecord {
        mode: ArtifactMode::LabRxBackpressureHil,
        feature: BACKPRESSURE_FEATURE.to_owned(),
        elf: file_record(&output, &backpressure_elf)?,
        flash_image: file_record(&output, &backpressure_image)?,
        flash_image_address: 0,
        size: backpressure_size,
    };
    ensure_artifacts_distinct(&normal, &backpressure)?;
    let reproducibility = independently_rebuild_and_compare(
        root,
        &output.join("source.tar"),
        &profile,
        &source_date_epoch_microseconds,
        &qualification_environment,
        &normal_elf,
        &normal_image,
    )?;
    phase1_source::ensure_source_identity_unchanged(root, &source_identity)?;

    let manifest = ArtifactManifest {
        schema: SCHEMA.to_owned(),
        prepared_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
            .as_secs(),
        git_commit,
        git_root_tree,
        worktree_clean: true,
        tools,
        profile_environment: profile,
        backpressure_stall_us,
        source_archive: file_record(&output, &output.join("source.tar"))?,
        build_recipe: BuildRecipe {
            normal_cargo_arguments: build_arguments(ArtifactMode::LabRx),
            backpressure_cargo_arguments: build_arguments(ArtifactMode::LabRxBackpressureHil),
            chip: phase1_image::CHIP.to_owned(),
            flash_size: phase1_image::FLASH_SIZE.to_owned(),
            flash_mode: phase1_image::FLASH_MODE.to_owned(),
            flash_frequency: phase1_image::FLASH_FREQUENCY.to_owned(),
            xtal_frequency: phase1_image::XTAL_FREQUENCY.to_owned(),
            minimum_chip_revision: phase1_image::MINIMUM_CHIP_REVISION.to_owned(),
            image_format: phase1_image::IMAGE_FORMAT.to_owned(),
            merged: true,
            skip_padding: true,
            espflash_config_policy: phase1_image::CONFIG_POLICY.to_owned(),
            isolated_target_directories: true,
            rustflags: ARTIFACT_RUSTFLAGS.to_owned(),
            encoded_rustflags: true,
            environment_cleared: true,
            ambient_environment_allowlist: phase1_tooling::AMBIENT_ENVIRONMENT_ALLOWLIST
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            explicit_build_environment_names: phase1_tooling::EXPLICIT_BUILD_ENVIRONMENT_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            environment_policy: phase1_tooling::DETERMINISTIC_ENVIRONMENT_POLICY.to_owned(),
            source_date_epoch_microseconds,
            build_root_remap: phase1_tooling::BUILD_ROOT_REMAP.to_owned(),
            rustup_home_remap: phase1_tooling::RUSTUP_HOME_REMAP.to_owned(),
        },
        reproducibility,
        normal,
        backpressure,
    };
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("could not serialize artifact manifest: {error}"))?;
    manifest_bytes.push(b'\n');
    write_new(&output.join(MANIFEST_FILE), &manifest_bytes)?;
    write_prepared_hashes(&output)?;

    verify_bundle(root, &output, false)?;
    phase1_source::ensure_source_identity_unchanged(
        root,
        &phase1_source::SourceIdentity {
            commit: manifest.git_commit.clone(),
            root_tree: manifest.git_root_tree.clone(),
        },
    )?;
    write_new(&output.join(COMPLETE_FILE), COMPLETE_CONTENT.as_bytes())?;
    fs::remove_file(output.join(INCOMPLETE_FILE)).map_err(|error| {
        format!(
            "could not remove incomplete marker in {}: {error}",
            output.display()
        )
    })?;

    println!(
        "ok: prepared and verified Phase-1 artifacts without hardware operations in {}",
        output.display()
    );
    println!(
        "next: follow docs/phase-1-rx-hil.md for explicit flash, readback, monitoring and powered evidence"
    );
    Ok(())
}

fn capture_profile_environment() -> Result<BTreeMap<String, String>, String> {
    let mut profile = BTreeMap::new();
    for (name, kind) in PROFILE_ENV {
        let value = env::var(name)
            .map_err(|_| format!("explicit lab RX configuration is missing {name}"))?;
        if value.trim() != value || value.is_empty() {
            return Err(format!(
                "{name} must be non-empty and contain no surrounding whitespace"
            ));
        }
        match kind {
            ProfileValueKind::Unsigned => {
                parse_canonical_u64(name, &value, true)?;
            }
            ProfileValueKind::Boolean if value != "0" && value != "1" => {
                return Err(format!("{name} must be exactly 0 or 1"));
            }
            ProfileValueKind::Boolean => {}
        }
        profile.insert((*name).to_owned(), value);
    }
    Ok(profile)
}

fn validate_git_commit(commit: &str) -> Result<(), String> {
    if commit.len() == 40
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!(
            "git rev-parse returned a non-canonical commit: {commit:?}"
        ))
    }
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

fn ensure_output_location_does_not_dirty_source(root: &Path, output: &Path) -> Result<(), String> {
    phase1_source::ensure_output_location_does_not_dirty_source(root, output)
}

fn collect_and_validate_tools(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let checks: &[(&str, &str, &[&str], Option<&str>)] = &[
        ("git", "git", &["--version"], None),
        (
            "host_rust",
            "rustc",
            &["--version"],
            Some(phase1_tooling::HOST_RUST_VERSION),
        ),
        (
            "esp_rust",
            "rustc",
            &["+esp", "--version"],
            Some(phase1_tooling::ESP_RUST_VERSION),
        ),
        (
            "esp_cargo",
            "cargo",
            &["+esp", "--version"],
            Some(phase1_tooling::ESP_CARGO_VERSION),
        ),
        (
            "espflash",
            "espflash",
            &["--version"],
            Some(phase1_tooling::ESPFLASH_VERSION),
        ),
        (
            "xtensa_gcc",
            "xtensa-esp32s3-elf-gcc",
            &["--version"],
            Some(phase1_tooling::XTENSA_GCC_VERSION),
        ),
        (
            "xtensa_size",
            "xtensa-esp32s3-elf-size",
            &["--version"],
            Some(phase1_tooling::XTENSA_SIZE_VERSION),
        ),
        (
            "xtensa_nm",
            "xtensa-esp32s3-elf-nm",
            &["--version"],
            Some(phase1_tooling::XTENSA_NM_VERSION),
        ),
        (
            "xtensa_readelf",
            "xtensa-esp32s3-elf-readelf",
            &["--version"],
            Some(phase1_tooling::XTENSA_READELF_VERSION),
        ),
        (
            "xtensa_objdump",
            "xtensa-esp32s3-elf-objdump",
            &["--version"],
            Some(phase1_tooling::XTENSA_OBJDUMP_VERSION),
        ),
        (
            "xtensa_strings",
            "xtensa-esp32s3-elf-strings",
            &["--version"],
            Some(phase1_tooling::XTENSA_STRINGS_VERSION),
        ),
    ];
    let mut tools = BTreeMap::new();
    for (label, program, args, expected) in checks {
        let version = if *label == "git" {
            phase1_source::git_version(root)?
        } else {
            let output = capture_stdout_at(program, args, root)?;
            output.lines().next().unwrap_or("").trim().to_owned()
        };
        if let Some(expected) = expected
            && version != *expected
        {
            return Err(format!(
                "{label} version mismatch: expected {expected:?}, got {version:?}"
            ));
        }
        tools.insert((*label).to_owned(), version);
    }
    phase1_tooling::validate_tool_inventory(&tools)?;
    Ok(tools)
}

fn write_tool_versions(
    output: &Path,
    commit: &str,
    root_tree: &str,
    tools: &BTreeMap<String, String>,
) -> Result<(), String> {
    let text = phase1_tooling::render_tool_inventory(commit, root_tree, tools)?;
    write_new(
        &output.join("tool-and-source-versions.txt"),
        text.as_bytes(),
    )
}

fn build_arguments(mode: ArtifactMode) -> Vec<String> {
    [
        "+esp",
        "build",
        "--locked",
        "--release",
        "-p",
        PACKAGE,
        "--bin",
        mode.binary(),
        "--no-default-features",
        "--features",
        mode.feature(),
        "--target",
        TARGET,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn build_spec(
    profile: &BTreeMap<String, String>,
    mode: ArtifactMode,
    stall_us: Option<u64>,
    target_dir: &Path,
    build_context: &phase1_tooling::CargoBuildContext<'_>,
) -> Result<CommandSpec, String> {
    let mut command_env =
        build_context.environment(profile, target_dir, ARTIFACT_RUSTFLAG_ARGUMENTS)?;
    match (mode, stall_us) {
        (ArtifactMode::LabRx, None) => {}
        (ArtifactMode::LabRxBackpressureHil, Some(stall_us)) => {
            command_env.insert(STALL_ENV.to_owned(), stall_us.to_string());
        }
        _ => unreachable!("artifact mode and stall must be paired"),
    }
    Ok(CommandSpec {
        program: "cargo".to_owned(),
        args: build_arguments(mode),
        env: command_env,
    })
}

fn save_image_spec(
    elf: &Path,
    image: &Path,
    context: &phase1_image::OfflineEspflashContext,
    qualification_environment: &phase1_tooling::QualificationEnvironment,
) -> CommandSpec {
    let mut environment = qualification_environment.base_environment();
    environment.extend(context.environment());
    CommandSpec {
        program: "espflash".to_owned(),
        args: phase1_image::save_image_arguments(elf, image),
        env: environment,
    }
}

fn validate_offline_artifact_command(spec: &CommandSpec) -> Result<(), String> {
    const PROHIBITED: &[&str] = &[
        "flash",
        "write-bin",
        "read-flash",
        "monitor",
        "list-ports",
        "board-info",
        "reset",
        "erase-flash",
        "erase-parts",
        "erase-region",
        "hold-in-reset",
        "checksum-md5",
    ];
    match spec.program.as_str() {
        "cargo"
            if spec.args.first().map(String::as_str) == Some("+esp")
                && spec.args.get(1).map(String::as_str) == Some("build") => {}
        "espflash" if spec.args.first().map(String::as_str) == Some("save-image") => {}
        _ => {
            return Err(format!(
                "artifact command is not in the non-RF allowlist: {}",
                spec.rendered()
            ));
        }
    }
    if spec.program == "espflash"
        && spec
            .args
            .iter()
            .any(|argument| PROHIBITED.contains(&argument.as_str()))
    {
        return Err(format!(
            "artifact command contains a hardware-affecting espflash operation: {}",
            spec.rendered()
        ));
    }
    if spec.args.iter().any(|argument| argument == "--port") {
        return Err(format!(
            "artifact command contains a serial-port argument: {}",
            spec.rendered()
        ));
    }
    Ok(())
}

fn run_logged(spec: &CommandSpec, root: &Path, log_path: &Path) -> Result<(), String> {
    let output = spec
        .command()
        .current_dir(root)
        .output()
        .map_err(|error| format!("could not run {}: {error}", spec.rendered()))?;
    let mut log = Vec::new();
    writeln!(&mut log, "command: {}", spec.rendered())
        .map_err(|error| format!("could not format command log: {error}"))?;
    writeln!(&mut log, "exit_status: {}", output.status)
        .map_err(|error| format!("could not format command log: {error}"))?;
    log.extend_from_slice(b"--- stdout ---\n");
    log.extend_from_slice(&output.stdout);
    if !output.stdout.ends_with(b"\n") {
        log.push(b'\n');
    }
    log.extend_from_slice(b"--- stderr ---\n");
    log.extend_from_slice(&output.stderr);
    if !output.stderr.ends_with(b"\n") {
        log.push(b'\n');
    }
    write_new(log_path, &log)?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "command failed with {}; retained log at {}",
            output.status,
            log_path.display()
        ))
    }
}

fn copy_built_elf(target_dir: &Path, mode: ArtifactMode, destination: &Path) -> Result<(), String> {
    let source = target_dir.join(TARGET).join("release").join(mode.binary());
    if !source.is_file() {
        return Err(format!(
            "Cargo did not produce expected ELF {}",
            source.display()
        ));
    }
    fs::copy(&source, destination).map_err(|error| {
        format!(
            "could not preserve ELF {} as {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn write_artifact_sidecars(directory: &Path, elf: &Path, image: &Path) -> Result<(), String> {
    write_hash_sidecar(elf, &directory.join("firmware.sha256"), "firmware.elf")?;
    write_hash_sidecar(
        image,
        &directory.join("flash-image.sha256"),
        "flash-image.bin",
    )?;
    write_new(&directory.join("flash-image-address.txt"), b"0x00000000\n")?;
    let bytes = fs::metadata(image)
        .map_err(|error| format!("could not stat {}: {error}", image.display()))?
        .len();
    write_new(
        &directory.join("flash-image-bytes.txt"),
        format!("{bytes}\n").as_bytes(),
    )
}

fn file_record(bundle: &Path, path: &Path) -> Result<FileRecord, String> {
    let relative = path.strip_prefix(bundle).map_err(|_| {
        format!(
            "artifact path {} is outside bundle {}",
            path.display(),
            bundle.display()
        )
    })?;
    let path_text = relative_path_text(relative)?;
    let metadata = fs::metadata(path)
        .map_err(|error| format!("could not stat artifact {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "artifact is not a regular file: {}",
            path.display()
        ));
    }
    Ok(FileRecord {
        path: path_text,
        sha256: sha256_file(path)?,
        bytes: metadata.len(),
    })
}

fn relative_path_text(path: &Path) -> Result<String, String> {
    let mut pieces = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(piece) => pieces.push(
                piece
                    .to_str()
                    .ok_or_else(|| format!("artifact path is not UTF-8: {}", path.display()))?,
            ),
            _ => {
                return Err(format!(
                    "artifact path must be a confined relative path: {}",
                    path.display()
                ));
            }
        }
    }
    if pieces.is_empty() {
        return Err("artifact path must not be empty".to_owned());
    }
    Ok(pieces.join("/"))
}

fn resolve_record_path(bundle: &Path, record: &FileRecord) -> Result<PathBuf, String> {
    let relative = Path::new(&record.path);
    if relative_path_text(relative)? != record.path {
        return Err(format!(
            "artifact manifest path is not canonical: {:?}",
            record.path
        ));
    }
    Ok(bundle.join(relative))
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

fn write_hash_sidecar(path: &Path, sidecar: &Path, recorded_name: &str) -> Result<(), String> {
    let digest = sha256_file(path)?;
    write_new(sidecar, format!("{digest}  {recorded_name}\n").as_bytes())
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

fn write_prepared_hashes(bundle: &Path) -> Result<(), String> {
    let mut text = String::new();
    for relative in PREPARED_FILES {
        let path = bundle.join(relative);
        let digest = sha256_file(&path)?;
        text.push_str(&format!("{digest}  {relative}\n"));
    }
    write_new(&bundle.join(PREPARED_HASH_FILE), text.as_bytes())
}

fn ensure_artifacts_distinct(
    normal: &ArtifactRecord,
    backpressure: &ArtifactRecord,
) -> Result<(), String> {
    if normal.mode == backpressure.mode
        || normal.feature == backpressure.feature
        || normal.elf.path == backpressure.elf.path
        || normal.flash_image.path == backpressure.flash_image.path
        || normal.elf.sha256 == backpressure.elf.sha256
        || normal.flash_image.sha256 == backpressure.flash_image.sha256
    {
        Err(
            "normal and backpressure artifacts are not distinct; shared Cargo output may have overwritten one mode"
                .to_owned(),
        )
    } else {
        Ok(())
    }
}

fn independently_rebuild_and_compare(
    workspace_root: &Path,
    source_archive: &Path,
    profile: &BTreeMap<String, String>,
    source_date_epoch_microseconds: &str,
    qualification_environment: &phase1_tooling::QualificationEnvironment,
    preserved_elf: &Path,
    preserved_image: &Path,
) -> Result<ReproducibilityRecord, String> {
    let rebuild = TemporaryDirectory::below(&env::temp_dir(), "phase1-rx-hil-repro")?;
    phase1_source::ensure_path_outside_workspace(workspace_root, rebuild.path())?;
    let source_root = rebuild.path().join("source");
    fs::create_dir(&source_root).map_err(|error| {
        format!(
            "could not create independent rebuild source root {}: {error}",
            source_root.display()
        )
    })?;
    phase1_source::extract_source_archive(source_archive, &source_root)?;
    phase1_source::reject_ambient_ancestor_cargo_configs(&source_root)?;
    let cargo_home = rebuild.path().join("cargo-home");
    fs::create_dir(&cargo_home).map_err(|error| {
        format!(
            "could not create independent rebuild Cargo home {}: {error}",
            cargo_home.display()
        )
    })?;
    phase1_tooling::create_controlled_tmpdir(rebuild.path())?;
    let build_context = phase1_tooling::CargoBuildContext::new(
        rebuild.path(),
        &cargo_home,
        source_date_epoch_microseconds,
        qualification_environment,
    )?;
    let target = rebuild.path().join("normal");
    let build = build_spec(profile, ArtifactMode::LabRx, None, &target, &build_context)?;
    validate_offline_artifact_command(&build)?;
    run_logged(&build, &source_root, &rebuild.path().join("build.log"))?;
    let rebuilt_elf = rebuild.path().join("firmware.elf");
    copy_built_elf(&target, ArtifactMode::LabRx, &rebuilt_elf)?;

    let espflash_context = phase1_image::OfflineEspflashContext::create(rebuild.path())?;
    let rebuilt_image = rebuild.path().join("flash-image.bin");
    let save = save_image_spec(
        &rebuilt_elf,
        &rebuilt_image,
        &espflash_context,
        qualification_environment,
    );
    validate_offline_artifact_command(&save)?;
    run_logged(
        &save,
        espflash_context.workdir(),
        &rebuild.path().join("save-image.log"),
    )?;

    for (label, preserved, rebuilt) in [
        ("ELF", preserved_elf, rebuilt_elf.as_path()),
        ("flash image", preserved_image, rebuilt_image.as_path()),
    ] {
        if fs::read(preserved).map_err(|error| {
            format!(
                "could not read preserved {label} {}: {error}",
                preserved.display()
            )
        })? != fs::read(rebuilt).map_err(|error| {
            format!(
                "could not read rebuilt {label} {}: {error}",
                rebuilt.display()
            )
        })? {
            return Err(format!(
                "independent normal-mode rebuild produced a different {label}; deterministic qualification failed"
            ));
        }
    }

    Ok(ReproducibilityRecord {
        canary_mode: ArtifactMode::LabRx,
        independent_source_archive_extraction: true,
        independent_target_directory: true,
        independent_cargo_home: true,
        elf_sha256: sha256_file(&rebuilt_elf)?,
        flash_image_sha256: sha256_file(&rebuilt_image)?,
        byte_for_byte: true,
    })
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn below(parent: &Path, label: &str) -> Result<Self, String> {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "could not create temporary directory parent {}: {error}",
                parent.display()
            )
        })?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
            .as_nanos();
        let path = parent.join(format!("{label}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).map_err(|error| {
            format!(
                "could not create temporary directory {}: {error}",
                path.display()
            )
        })?;
        // Keep every path passed to Cargo in the same canonical spelling used
        // by the Rust path-remap flags. On macOS, `/tmp` aliases
        // `/private/tmp`; mixing those spellings leaves nonce paths embedded
        // in the ELF and defeats the independent-build comparison.
        let path = fs::canonicalize(&path).map_err(|error| {
            format!(
                "could not canonicalize temporary directory {}: {error}",
                path.display()
            )
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            eprintln!(
                "warning: could not remove temporary directory {}: {error}",
                self.path.display()
            );
        }
    }
}

fn verify_bundle(root: &Path, bundle_arg: &Path, require_complete: bool) -> Result<(), String> {
    let current_tools = collect_and_validate_tools(root)?;
    let qualification_environment = phase1_tooling::QualificationEnvironment::capture()?;
    let bundle = absolute_from(root, bundle_arg);
    let metadata = fs::symlink_metadata(&bundle).map_err(|error| {
        format!(
            "could not inspect artifact bundle {}: {error}",
            bundle.display()
        )
    })?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "artifact bundle is not a directory: {}",
            bundle.display()
        ));
    }
    verify_completion_state(&bundle, require_complete)?;
    verify_bundle_tree(&bundle, require_complete)?;
    let manifest_path = bundle.join(MANIFEST_FILE);
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
        format!(
            "could not read artifact manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: ArtifactManifest = serde_json::from_slice(&manifest_bytes).map_err(|error| {
        format!(
            "could not parse artifact manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    validate_manifest_shape(&manifest)?;
    let expected_source_date_epoch =
        phase1_tooling::source_date_epoch_for_commit(root, &manifest.git_commit)?;
    if manifest.build_recipe.source_date_epoch_microseconds != expected_source_date_epoch {
        return Err(
            "artifact manifest SOURCE_DATE_EPOCH does not match the Git commit timestamp in microseconds"
                .to_owned(),
        );
    }
    verify_prepared_hashes(&bundle)?;
    verify_tool_versions_file(
        &bundle,
        &manifest.git_commit,
        &manifest.git_root_tree,
        &manifest.tools,
    )?;
    verify_file_record(&bundle, &manifest.source_archive)?;
    let source_path = resolve_record_path(&bundle, &manifest.source_archive)?;
    phase1_source::verify_source_archive(
        root,
        &source_path,
        &manifest.git_commit,
        &manifest.git_root_tree,
    )?;
    verify_hash_sidecar(&bundle.join("source.sha256"), &source_path, "source.tar")?;

    verify_artifact_record(&bundle, &manifest.normal, "", ArtifactMode::LabRx)?;
    verify_artifact_record(
        &bundle,
        &manifest.backpressure,
        "backpressure-artifact/",
        ArtifactMode::LabRxBackpressureHil,
    )?;
    ensure_artifacts_distinct(&manifest.normal, &manifest.backpressure)?;

    let normal_elf = resolve_record_path(&bundle, &manifest.normal.elf)?;
    let backpressure_elf = resolve_record_path(&bundle, &manifest.backpressure.elf)?;
    let normal_size = inspect_elf(&normal_elf, InspectionMode::Normal)?;
    let backpressure_size = inspect_elf(&backpressure_elf, InspectionMode::Backpressure)?;
    if normal_size != manifest.normal.size || backpressure_size != manifest.backpressure.size {
        return Err("ELF size metrics do not match artifact manifest".to_owned());
    }

    let current_espflash = current_tools
        .get("espflash")
        .ok_or_else(|| "current Phase-1 tool inventory does not record espflash".to_owned())?;
    let recorded_espflash = manifest
        .tools
        .get("espflash")
        .ok_or_else(|| "artifact manifest does not record espflash".to_owned())?;
    if current_espflash != phase1_tooling::ESPFLASH_VERSION
        || recorded_espflash != phase1_tooling::ESPFLASH_VERSION
    {
        return Err(format!(
            "host-only image verification requires recorded and current {:?}; recorded={recorded_espflash:?}, current={current_espflash:?}",
            phase1_tooling::ESPFLASH_VERSION
        ));
    }
    regenerate_and_compare_image(&bundle, &manifest.normal, &qualification_environment)?;
    regenerate_and_compare_image(&bundle, &manifest.backpressure, &qualification_environment)?;

    if require_complete {
        println!("ok: verified Phase-1 artifact bundle {}", bundle.display());
    }
    Ok(())
}

pub(crate) fn verified_bundle_binding(
    root: &Path,
    bundle: &Path,
) -> Result<VerifiedBundleBinding, String> {
    let bundle = absolute_from(root, bundle);
    let manifest_path = bundle.join(MANIFEST_FILE);
    let manifest_bytes_before = fs::read(&manifest_path).map_err(|error| {
        format!(
            "could not read verified artifact manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: ArtifactManifest =
        serde_json::from_slice(&manifest_bytes_before).map_err(|error| {
            format!(
                "could not parse verified artifact manifest {}: {error}",
                manifest_path.display()
            )
        })?;
    verify_bundle(root, &bundle, true)?;
    let manifest_bytes_after = fs::read(&manifest_path).map_err(|error| {
        format!(
            "could not reread verified artifact manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    if manifest_bytes_before != manifest_bytes_after {
        return Err("artifact manifest changed while it was being verified".to_owned());
    }
    let binding = |id: &str, record: ArtifactRecord| VerifiedArtifactBinding {
        id: id.to_owned(),
        mode: record.mode.as_str().to_owned(),
        elf: VerifiedFileBinding {
            path: record.elf.path,
            sha256: record.elf.sha256,
            bytes: record.elf.bytes,
        },
        flash_image: VerifiedFileBinding {
            path: record.flash_image.path,
            sha256: record.flash_image.sha256,
            bytes: record.flash_image.bytes,
        },
    };
    Ok(VerifiedBundleBinding {
        schema: manifest.schema,
        git_commit: manifest.git_commit,
        git_root_tree: manifest.git_root_tree,
        profile_environment: manifest.profile_environment,
        artifacts: vec![
            binding("normal", manifest.normal),
            binding("backpressure", manifest.backpressure),
        ],
    })
}

fn verify_tool_versions_file(
    bundle: &Path,
    commit: &str,
    root_tree: &str,
    tools: &BTreeMap<String, String>,
) -> Result<(), String> {
    let expected = phase1_tooling::render_tool_inventory(commit, root_tree, tools)?;
    let path = bundle.join("tool-and-source-versions.txt");
    let actual = fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if actual == expected {
        Ok(())
    } else {
        Err("tool-and-source version file does not match artifact manifest".to_owned())
    }
}

fn verify_completion_state(bundle: &Path, require_complete: bool) -> Result<(), String> {
    let incomplete = bundle.join(INCOMPLETE_FILE);
    let complete = bundle.join(COMPLETE_FILE);
    if require_complete {
        if incomplete.exists() {
            return Err(format!(
                "artifact bundle retains incomplete marker: {}",
                incomplete.display()
            ));
        }
        let marker = fs::read_to_string(&complete).map_err(|error| {
            format!(
                "artifact bundle lacks completion marker {}: {error}",
                complete.display()
            )
        })?;
        if marker != COMPLETE_CONTENT {
            return Err(format!(
                "artifact completion marker has unexpected content: {}",
                complete.display()
            ));
        }
    } else if !incomplete.is_file() || complete.exists() {
        return Err(
            "pre-completion verification requires exactly one incomplete marker".to_owned(),
        );
    }
    Ok(())
}

fn verify_bundle_tree(bundle: &Path, require_complete: bool) -> Result<(), String> {
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    collect_bundle_tree(bundle, bundle, &mut files, &mut directories)?;

    let mut expected_files = PREPARED_FILES
        .iter()
        .map(|relative| (*relative).to_owned())
        .collect::<BTreeSet<_>>();
    expected_files.insert(PREPARED_HASH_FILE.to_owned());
    expected_files.insert(
        if require_complete {
            COMPLETE_FILE
        } else {
            INCOMPLETE_FILE
        }
        .to_owned(),
    );
    if files != expected_files {
        let missing = expected_files
            .difference(&files)
            .cloned()
            .collect::<Vec<_>>();
        let unexpected = files
            .difference(&expected_files)
            .cloned()
            .collect::<Vec<_>>();
        return Err(format!(
            "artifact bundle file set changed: missing={missing:?} unexpected={unexpected:?}"
        ));
    }

    let expected_directories = BTreeSet::from(["backpressure-artifact".to_owned()]);
    if directories != expected_directories {
        let missing = expected_directories
            .difference(&directories)
            .cloned()
            .collect::<Vec<_>>();
        let unexpected = directories
            .difference(&expected_directories)
            .cloned()
            .collect::<Vec<_>>();
        return Err(format!(
            "artifact bundle directory set changed: missing={missing:?} unexpected={unexpected:?}"
        ));
    }
    Ok(())
}

fn collect_bundle_tree(
    bundle: &Path,
    directory: &Path,
    files: &mut BTreeSet<String>,
    directories: &mut BTreeSet<String>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|error| {
        format!(
            "could not list bundle directory {}: {error}",
            directory.display()
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "could not inspect entry in bundle directory {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            format!("could not inspect bundle path {}: {error}", path.display())
        })?;
        let relative = path.strip_prefix(bundle).map_err(|_| {
            format!(
                "bundle traversal escaped {} through {}",
                bundle.display(),
                path.display()
            )
        })?;
        let relative = relative_path_text(relative)?;
        if metadata.file_type().is_symlink() {
            return Err(format!("artifact bundle contains symlink {relative:?}"));
        }
        if metadata.file_type().is_dir() {
            directories.insert(relative);
            collect_bundle_tree(bundle, &path, files, directories)?;
        } else if metadata.file_type().is_file() {
            files.insert(relative);
        } else {
            return Err(format!(
                "artifact bundle contains non-file, non-directory path {relative:?}"
            ));
        }
    }
    Ok(())
}

fn validate_manifest_shape(manifest: &ArtifactManifest) -> Result<(), String> {
    if manifest.schema != SCHEMA {
        return Err(format!(
            "unsupported artifact manifest schema {:?}",
            manifest.schema
        ));
    }
    validate_git_commit(&manifest.git_commit)?;
    validate_git_commit(&manifest.git_root_tree)
        .map_err(|error| format!("artifact manifest root tree: {error}"))?;
    if !manifest.worktree_clean {
        return Err(
            "artifact manifest is not qualification-eligible: worktree_clean=false".to_owned(),
        );
    }
    phase1_tooling::validate_tool_inventory(&manifest.tools)?;
    validate_profile_map(&manifest.profile_environment)?;
    if manifest.backpressure_stall_us == 0 {
        return Err("artifact manifest records a zero backpressure stall".to_owned());
    }
    let recipe = &manifest.build_recipe;
    if recipe.normal_cargo_arguments != build_arguments(ArtifactMode::LabRx)
        || recipe.backpressure_cargo_arguments
            != build_arguments(ArtifactMode::LabRxBackpressureHil)
        || recipe.chip != phase1_image::CHIP
        || recipe.flash_size != phase1_image::FLASH_SIZE
        || recipe.flash_mode != phase1_image::FLASH_MODE
        || recipe.flash_frequency != phase1_image::FLASH_FREQUENCY
        || recipe.xtal_frequency != phase1_image::XTAL_FREQUENCY
        || recipe.minimum_chip_revision != phase1_image::MINIMUM_CHIP_REVISION
        || recipe.image_format != phase1_image::IMAGE_FORMAT
        || !recipe.merged
        || !recipe.skip_padding
        || recipe.espflash_config_policy != phase1_image::CONFIG_POLICY
        || !recipe.isolated_target_directories
        || recipe.rustflags != ARTIFACT_RUSTFLAGS
        || !recipe.encoded_rustflags
        || !recipe.environment_cleared
        || recipe.ambient_environment_allowlist
            != phase1_tooling::AMBIENT_ENVIRONMENT_ALLOWLIST
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        || recipe.explicit_build_environment_names
            != phase1_tooling::EXPLICIT_BUILD_ENVIRONMENT_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        || recipe.environment_policy != phase1_tooling::DETERMINISTIC_ENVIRONMENT_POLICY
        || phase1_tooling::validate_source_date_epoch_microseconds(
            &recipe.source_date_epoch_microseconds,
        )
        .is_err()
        || recipe.build_root_remap != phase1_tooling::BUILD_ROOT_REMAP
        || recipe.rustup_home_remap != phase1_tooling::RUSTUP_HOME_REMAP
    {
        return Err("artifact manifest build recipe is not the reviewed Phase-1 recipe".to_owned());
    }
    let reproducibility = &manifest.reproducibility;
    if reproducibility.canary_mode != ArtifactMode::LabRx
        || !reproducibility.independent_source_archive_extraction
        || !reproducibility.independent_target_directory
        || !reproducibility.independent_cargo_home
        || !reproducibility.byte_for_byte
        || reproducibility.elf_sha256 != manifest.normal.elf.sha256
        || reproducibility.flash_image_sha256 != manifest.normal.flash_image.sha256
    {
        return Err(
            "artifact manifest does not record the reviewed independent normal-mode byte comparison"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_profile_map(profile: &BTreeMap<String, String>) -> Result<(), String> {
    if profile.len() != PROFILE_ENV.len() {
        return Err("artifact manifest profile does not contain exactly eight fields".to_owned());
    }
    for (name, kind) in PROFILE_ENV {
        let value = profile
            .get(*name)
            .ok_or_else(|| format!("artifact manifest profile is missing {name}"))?;
        match kind {
            ProfileValueKind::Unsigned => {
                parse_canonical_u64(name, value, true)?;
            }
            ProfileValueKind::Boolean if value != "0" && value != "1" => {
                return Err(format!("artifact manifest {name} must be exactly 0 or 1"));
            }
            ProfileValueKind::Boolean => {}
        }
    }
    Ok(())
}

fn verify_prepared_hashes(bundle: &Path) -> Result<(), String> {
    let manifest_path = bundle.join(PREPARED_HASH_FILE);
    let text = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "could not read prepared artifact hash manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let mut records = BTreeMap::new();
    for (line_number, line) in text.lines().enumerate() {
        let (digest, relative) = line.split_once("  ").ok_or_else(|| {
            format!(
                "invalid prepared artifact hash line {} in {}",
                line_number + 1,
                manifest_path.display()
            )
        })?;
        validate_sha256(digest)?;
        if relative_path_text(Path::new(relative))? != relative {
            return Err(format!("non-canonical prepared artifact path {relative:?}"));
        }
        if records
            .insert(relative.to_owned(), digest.to_owned())
            .is_some()
        {
            return Err(format!("duplicate prepared artifact hash for {relative}"));
        }
    }
    let expected = PREPARED_FILES.iter().copied().collect::<BTreeSet<_>>();
    let actual = records.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if actual != expected {
        return Err("prepared artifact hash manifest has a missing or unexpected path".to_owned());
    }
    for (relative, expected_digest) in records {
        let actual_digest = sha256_file(&bundle.join(&relative))?;
        if actual_digest != expected_digest {
            return Err(format!("prepared artifact hash mismatch for {relative}"));
        }
    }
    Ok(())
}

fn verify_file_record(bundle: &Path, record: &FileRecord) -> Result<(), String> {
    validate_sha256(&record.sha256)?;
    let path = resolve_record_path(bundle, record)?;
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("could not stat artifact {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "artifact is not a regular file: {}",
            path.display()
        ));
    }
    if metadata.len() != record.bytes {
        return Err(format!(
            "artifact byte count mismatch for {}: expected {}, found {}",
            path.display(),
            record.bytes,
            metadata.len()
        ));
    }
    let digest = sha256_file(&path)?;
    if digest != record.sha256 {
        return Err(format!("artifact SHA-256 mismatch for {}", path.display()));
    }
    Ok(())
}

fn verify_artifact_record(
    bundle: &Path,
    record: &ArtifactRecord,
    prefix: &str,
    expected_mode: ArtifactMode,
) -> Result<(), String> {
    let expected_elf = format!("{prefix}firmware.elf");
    let expected_image = format!("{prefix}flash-image.bin");
    if record.mode != expected_mode
        || record.feature != expected_mode.feature()
        || record.elf.path != expected_elf
        || record.flash_image.path != expected_image
        || record.flash_image_address != 0
    {
        return Err(format!(
            "{} artifact manifest identity/path/address does not match the reviewed layout",
            expected_mode.as_str()
        ));
    }
    verify_file_record(bundle, &record.elf)?;
    verify_file_record(bundle, &record.flash_image)?;
    let directory = if prefix.is_empty() {
        bundle.to_owned()
    } else {
        bundle.join(prefix.trim_end_matches('/'))
    };
    let elf = resolve_record_path(bundle, &record.elf)?;
    let image = resolve_record_path(bundle, &record.flash_image)?;
    verify_hash_sidecar(&directory.join("firmware.sha256"), &elf, "firmware.elf")?;
    verify_hash_sidecar(
        &directory.join("flash-image.sha256"),
        &image,
        "flash-image.bin",
    )?;
    let address = fs::read_to_string(directory.join("flash-image-address.txt"))
        .map_err(|error| format!("could not read flash image address: {error}"))?;
    if address != "0x00000000\n" {
        return Err(format!(
            "{} flash image address is not exact address zero",
            expected_mode.as_str()
        ));
    }
    let bytes = fs::read_to_string(directory.join("flash-image-bytes.txt"))
        .map_err(|error| format!("could not read flash image byte count: {error}"))?;
    if bytes != format!("{}\n", record.flash_image.bytes) {
        return Err(format!(
            "{} flash image byte-count sidecar does not match manifest",
            expected_mode.as_str()
        ));
    }
    Ok(())
}

fn verify_hash_sidecar(sidecar: &Path, target: &Path, recorded_name: &str) -> Result<(), String> {
    let text = fs::read_to_string(sidecar)
        .map_err(|error| format!("could not read hash sidecar {}: {error}", sidecar.display()))?;
    let expected = format!("{}  {recorded_name}\n", sha256_file(target)?);
    if text == expected {
        Ok(())
    } else {
        Err(format!(
            "hash sidecar {} does not match {}",
            sidecar.display(),
            target.display()
        ))
    }
}

fn validate_sha256(value: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!("non-canonical SHA-256 digest {value:?}"))
    }
}

fn regenerate_and_compare_image(
    bundle: &Path,
    artifact: &ArtifactRecord,
    qualification_environment: &phase1_tooling::QualificationEnvironment,
) -> Result<(), String> {
    let temporary = TemporaryDirectory::below(&env::temp_dir(), "phase1-rx-image-verify")?;
    let espflash_context = phase1_image::OfflineEspflashContext::create(temporary.path())?;
    let elf = resolve_record_path(bundle, &artifact.elf)?;
    let preserved_image = resolve_record_path(bundle, &artifact.flash_image)?;
    let regenerated = temporary.path().join("flash-image.bin");
    let log = temporary.path().join("save-image.log");
    let command = save_image_spec(
        &elf,
        &regenerated,
        &espflash_context,
        qualification_environment,
    );
    validate_offline_artifact_command(&command)?;
    run_logged(&command, espflash_context.workdir(), &log)?;
    let preserved = fs::read(&preserved_image).map_err(|error| {
        format!(
            "could not read preserved image {}: {error}",
            preserved_image.display()
        )
    })?;
    let regenerated = fs::read(&regenerated)
        .map_err(|error| format!("could not read regenerated flash image: {error}"))?;
    if preserved != regenerated {
        return Err(format!(
            "offline regenerated image differs from preserved {} image",
            artifact.mode.as_str()
        ));
    }
    Ok(())
}

fn inspect_elf(path: &Path, mode: InspectionMode) -> Result<ElfSize, String> {
    if !path.is_file() {
        return Err(format!("ELF does not exist: {}", path.display()));
    }
    let path_text = path.to_string_lossy();
    let size_output = capture_stdout("xtensa-esp32s3-elf-size", &[path_text.as_ref()])?;
    let mut size = parse_size_output(&size_output)?;
    // Full lab images carry the reviewed startup-stack paint and compiler
    // frame inventory. The deliberately minimal retained-journal fixtures do
    // neither; zero records "not applicable" in their inspection result.
    size.maximum_stack_frame = if mode.is_full_stack() {
        inspect_stack_sizes(path)?
    } else {
        0
    };
    let (max_text, max_data, max_bss, max_total) = match mode {
        InspectionMode::Normal
        | InspectionMode::ElectricalLdoUnboosted
        | InspectionMode::ElectricalLdoBoosted
        | InspectionMode::ElectricalDcdcUnboosted
        | InspectionMode::ElectricalDcdcBoosted => (360_448, 12_288, 475_136, 840_000),
        InspectionMode::Backpressure
        | InspectionMode::ReturnedFaultOneBoot
        | InspectionMode::ReturnedFaultRepeatUntilQuarantine => (364_544, 12_288, 475_136, 845_000),
        InspectionMode::ResetJournalCorrupt | InspectionMode::ResetJournalTorn => {
            (65_536, 6_144, 419_840, 500_000)
        }
    };
    if size.text > max_text || size.data > max_data || size.bss > max_bss || size.total > max_total
    {
        return Err(format!(
            "{} artifact budget exceeded: text={}/{max_text} data={}/{max_data} bss={}/{max_bss} total={}/{max_total}",
            mode.as_str(),
            size.text,
            size.data,
            size.bss,
            size.total
        ));
    }
    if size.maximum_stack_frame > MAXIMUM_STACK_FRAME_BYTES {
        return Err(format!(
            "{} compiler-emitted stack frame exceeds reviewed {}-byte ceiling: {}",
            mode.as_str(),
            MAXIMUM_STACK_FRAME_BYTES,
            size.maximum_stack_frame
        ));
    }

    let strings = capture_stdout("xtensa-esp32s3-elf-strings", &[path_text.as_ref()])?;
    validate_mode_strings(&strings, mode)?;

    let sized_symbols = capture_stdout(
        "xtensa-esp32s3-elf-nm",
        &["-S", "--defined-only", path_text.as_ref()],
    )?;
    let sections = capture_stdout("xtensa-esp32s3-elf-readelf", &["-SW", path_text.as_ref()])?;
    let ordered_symbols = capture_stdout("xtensa-esp32s3-elf-nm", &["-n", path_text.as_ref()])?;
    let demangled_symbols = capture_stdout(
        "xtensa-esp32s3-elf-nm",
        &["--defined-only", "-C", path_text.as_ref()],
    )?;
    if mode.is_full_stack() {
        validate_full_stack_retained_symbols(&sized_symbols, mode)?;
        validate_retained_sections(&sections)?;
        validate_stack_guard_offset(&ordered_symbols)?;
        let disassembly =
            capture_stdout("xtensa-esp32s3-elf-objdump", &["-d", path_text.as_ref()])?;
        validate_zero_bss_hook(&disassembly)?;
    } else {
        validate_journal_retained_symbols(&sized_symbols)?;
        validate_journal_retained_sections(&sections)?;
        validate_default_zero_bss_hook(&ordered_symbols)?;
        validate_no_journal_runtime_ownership(&demangled_symbols)?;
    }
    validate_no_prohibited_tx_symbols(&demangled_symbols)?;
    Ok(size)
}

pub(crate) fn inspect_elf_by_name(path: &Path, mode: &str) -> Result<ElfSize, String> {
    inspect_elf(path, InspectionMode::parse(mode)?)
}

fn parse_size_output(output: &str) -> Result<ElfSize, String> {
    let line = output
        .lines()
        .find(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields.len() >= 4 && fields[..4].iter().all(|field| field.parse::<u64>().is_ok())
        })
        .ok_or_else(|| "could not parse GNU size output".to_owned())?;
    let fields = line.split_whitespace().collect::<Vec<_>>();
    Ok(ElfSize {
        text: fields[0]
            .parse()
            .map_err(|_| "invalid text size".to_owned())?,
        data: fields[1]
            .parse()
            .map_err(|_| "invalid data size".to_owned())?,
        bss: fields[2]
            .parse()
            .map_err(|_| "invalid BSS size".to_owned())?,
        total: fields[3]
            .parse()
            .map_err(|_| "invalid total size".to_owned())?,
        maximum_stack_frame: 0,
    })
}

fn inspect_stack_sizes(path: &Path) -> Result<u64, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read ELF {}: {error}", path.display()))?;
    let object = object::File::parse(bytes.as_slice())
        .map_err(|error| format!("could not parse ELF {}: {error}", path.display()))?;
    let section = object.section_by_name(".stack_sizes").ok_or_else(|| {
        format!(
            "ELF {} has no .stack_sizes section; build with RUSTFLAGS={ARTIFACT_RUSTFLAGS:?}",
            path.display()
        )
    })?;
    let data = section
        .data()
        .map_err(|error| format!("could not read .stack_sizes in {}: {error}", path.display()))?;
    let address_bytes = if object.is_64() { 8 } else { 4 };
    parse_stack_size_records(data, address_bytes)
}

fn parse_stack_size_records(data: &[u8], address_bytes: usize) -> Result<u64, String> {
    if data.is_empty() {
        return Err(".stack_sizes section is empty".to_owned());
    }
    if address_bytes != 4 && address_bytes != 8 {
        return Err(format!(
            "unsupported stack-size address width {address_bytes}"
        ));
    }
    let mut offset = 0;
    let mut maximum = 0;
    let mut records = 0_u64;
    while offset < data.len() {
        if data.len() - offset < address_bytes {
            return Err("truncated function address in .stack_sizes".to_owned());
        }
        offset += address_bytes;
        let (size, consumed) = decode_uleb128(&data[offset..])?;
        offset += consumed;
        maximum = maximum.max(size);
        records += 1;
    }
    if records == 0 {
        Err(".stack_sizes contains no records".to_owned())
    } else {
        Ok(maximum)
    }
}

fn decode_uleb128(bytes: &[u8]) -> Result<(u64, usize), String> {
    let mut value = 0_u64;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let shift = index * 7;
        if shift >= 64 || (index == 9 && byte > 1) {
            return Err("overflowing ULEB128 in .stack_sizes".to_owned());
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err("truncated ULEB128 in .stack_sizes".to_owned())
}

fn validate_mode_strings(strings: &str, mode: InspectionMode) -> Result<(), String> {
    const ESP_RTOS_MAIN_STACK_PATCH_ID: &str = "esp-rtos-0.3.0-cpu0-cpu1-main-stack-words-v2";
    const NORMAL_IDENTITY_MARKERS: &[&str] = &[
        "phase1 artifact identity: mode=",
        "phase1 lab-rx active: board=",
        "heltec-tracker-v2lab-rx",
    ];
    const PRESSURE_MARKERS: &[&str] = &[
        "lab-rx-backpressure-hil",
        "first_awaiting_continuation",
        "phase1 backpressure HIL triggered:",
        "phase1 backpressure HIL completed:",
    ];
    const ELECTRICAL_IDENTITIES: &[&str] = &[
        "lab-rx-electrical-hil;regulator=ldo;rx_gain=unboosted",
        "lab-rx-electrical-hil;regulator=ldo;rx_gain=boosted",
        "lab-rx-electrical-hil;regulator=dcdc;rx_gain=unboosted",
        "lab-rx-electrical-hil;regulator=dcdc;rx_gain=boosted",
    ];
    const RETURNED_IDENTITIES: &[&str] = &[
        "lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=one-boot",
        "lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=repeat-until-quarantine",
    ];
    const RETURNED_MARKERS: &[&str] = &[
        "phase1 returned-fault HIL evidence:",
        "get_irq_status_rejected_before_spi=",
        "set_rx_forwarded=",
        "fired=",
    ];
    const JOURNAL_COMMON_MARKERS: &[&str] = &[
        "phase1 reset-journal HIL artifact identity:",
        "phase1 reset-journal HIL baseline:",
        "phase1 reset-journal HIL triggered:",
        "phase1 reset-journal HIL expected quarantine:",
        "reason=CorruptOrTornJournal history=None",
        "radio_constructed=false spi_constructed=false executor_timer_constructed=false supervisor_watchdog=off",
        "rf_state=reset_low_fem_low",
    ];

    if mode.is_full_stack() {
        require_strings(
            strings,
            mode,
            &[
                "action=immediate_rf_inert_quarantine",
                ESP_RTOS_MAIN_STACK_PATCH_ID,
            ],
        )?;
    }

    match mode {
        InspectionMode::Normal => {
            require_strings(strings, mode, NORMAL_IDENTITY_MARKERS)?;
            reject_strings(strings, mode, PRESSURE_MARKERS)?;
            reject_strings(strings, mode, ELECTRICAL_IDENTITIES)?;
            reject_strings(strings, mode, RETURNED_IDENTITIES)?;
            reject_strings(strings, mode, RETURNED_MARKERS)?;
            reject_strings(
                strings,
                mode,
                &[
                    "lab-rx-reset-journal-corrupt-hil",
                    "lab-rx-reset-journal-torn-hil",
                ],
            )?;
        }
        InspectionMode::Backpressure => {
            require_strings(strings, mode, PRESSURE_MARKERS)?;
            reject_strings(strings, mode, ELECTRICAL_IDENTITIES)?;
            reject_strings(strings, mode, RETURNED_IDENTITIES)?;
            reject_strings(strings, mode, RETURNED_MARKERS)?;
        }
        InspectionMode::ElectricalLdoUnboosted
        | InspectionMode::ElectricalLdoBoosted
        | InspectionMode::ElectricalDcdcUnboosted
        | InspectionMode::ElectricalDcdcBoosted => {
            let expected = mode
                .electrical_identity()
                .expect("electrical mode has an identity");
            require_strings(strings, mode, &[expected])?;
            for identity in ELECTRICAL_IDENTITIES {
                if *identity != expected {
                    reject_strings(strings, mode, &[*identity])?;
                }
            }
            reject_strings(strings, mode, PRESSURE_MARKERS)?;
            reject_strings(strings, mode, RETURNED_IDENTITIES)?;
            reject_strings(strings, mode, RETURNED_MARKERS)?;
        }
        InspectionMode::ReturnedFaultOneBoot
        | InspectionMode::ReturnedFaultRepeatUntilQuarantine => {
            let expected = mode
                .returned_fault_identity()
                .expect("returned-fault mode has an identity");
            require_strings(strings, mode, &[expected])?;
            require_strings(strings, mode, RETURNED_MARKERS)?;
            for identity in RETURNED_IDENTITIES {
                if *identity != expected {
                    reject_strings(strings, mode, &[*identity])?;
                }
            }
            reject_strings(strings, mode, PRESSURE_MARKERS)?;
            reject_strings(strings, mode, ELECTRICAL_IDENTITIES)?;
        }
        InspectionMode::ResetJournalCorrupt => {
            require_strings(strings, mode, JOURNAL_COMMON_MARKERS)?;
            require_strings(
                strings,
                mode,
                &[
                    "lab-rx-reset-journal-corrupt-hil",
                    "trigger=corrupt-word",
                    "slot=",
                    "word=",
                    "xor_mask=0x",
                ],
            )?;
            reject_strings(strings, mode, &["lab-rx-reset-journal-torn-hil"])?;
            reject_strings(strings, mode, PRESSURE_MARKERS)?;
            reject_strings(strings, mode, ELECTRICAL_IDENTITIES)?;
            reject_strings(strings, mode, RETURNED_IDENTITIES)?;
        }
        InspectionMode::ResetJournalTorn => {
            require_strings(strings, mode, JOURNAL_COMMON_MARKERS)?;
            require_strings(
                strings,
                mode,
                &[
                    "lab-rx-reset-journal-torn-hil",
                    "trigger=torn-write",
                    "write_ordinal=",
                ],
            )?;
            reject_strings(strings, mode, &["lab-rx-reset-journal-corrupt-hil"])?;
            reject_strings(strings, mode, PRESSURE_MARKERS)?;
            reject_strings(strings, mode, ELECTRICAL_IDENTITIES)?;
            reject_strings(strings, mode, RETURNED_IDENTITIES)?;
        }
    }
    Ok(())
}

fn require_strings(strings: &str, mode: InspectionMode, markers: &[&str]) -> Result<(), String> {
    for marker in markers {
        if !strings.contains(marker) {
            return Err(format!(
                "{} ELF lacks required evidence marker {marker:?}",
                mode.as_str()
            ));
        }
    }
    Ok(())
}

fn reject_strings(strings: &str, mode: InspectionMode, markers: &[&str]) -> Result<(), String> {
    for marker in markers {
        if strings.contains(marker) {
            return Err(format!(
                "{} ELF contains forbidden cross-mode marker {marker:?}",
                mode.as_str()
            ));
        }
    }
    Ok(())
}

fn validate_full_stack_retained_symbols(symbols: &str, mode: InspectionMode) -> Result<(), String> {
    require_sized_symbol(symbols, "00000048", "RESET_QUARANTINE_JOURNAL")?;
    require_sized_symbol(symbols, "00000020", "RETICULUM_STACK_WATERMARK_MARKER")?;
    require_sized_symbol(symbols, "0000004a", "__zero_bss")?;
    if matches!(
        mode,
        InspectionMode::ReturnedFaultOneBoot | InspectionMode::ReturnedFaultRepeatUntilQuarantine
    ) {
        require_sized_symbol(symbols, "00000001", "RETICULUM_RETURNED_FAULT_EVIDENCE")?;
    } else {
        reject_symbol_suffix(symbols, "RETICULUM_RETURNED_FAULT_EVIDENCE")?;
    }
    Ok(())
}

fn validate_journal_retained_symbols(symbols: &str) -> Result<(), String> {
    require_sized_symbol(symbols, "00000048", "RESET_QUARANTINE_JOURNAL")?;
    reject_symbol_suffix(symbols, "RETICULUM_STACK_WATERMARK_MARKER")?;
    reject_symbol_suffix(symbols, "RETICULUM_RETURNED_FAULT_EVIDENCE")
}

fn require_sized_symbol(symbols: &str, size: &str, suffix: &str) -> Result<(), String> {
    let found = symbols
        .lines()
        .filter(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields.len() >= 4
                && fields[1].eq_ignore_ascii_case(size)
                && fields.last().is_some_and(|name| name.ends_with(suffix))
        })
        .count();
    if found == 1 {
        Ok(())
    } else {
        Err(format!(
            "ELF must contain exactly one {size}-byte symbol ending in {suffix}, found {found}"
        ))
    }
}

fn reject_symbol_suffix(symbols: &str, suffix: &str) -> Result<(), String> {
    if symbols.lines().any(|line| {
        line.split_whitespace()
            .last()
            .is_some_and(|name| name.ends_with(suffix))
    }) {
        Err(format!(
            "ELF unexpectedly retains symbol ending in {suffix}"
        ))
    } else {
        Ok(())
    }
}

fn validate_retained_sections(sections: &str) -> Result<(), String> {
    let rtc = parse_section_contract(sections, ".rtc_fast.persistent")?;
    if rtc
        != (
            "NOBITS".to_owned(),
            "600fe000".to_owned(),
            "000048".to_owned(),
        )
    {
        return Err(format!("reset journal section contract changed: {rtc:?}"));
    }
    let noinit = parse_section_contract(sections, ".noinit")?;
    if noinit.0 != "NOBITS" || noinit.2 != "000020" {
        return Err(format!("stack marker section contract changed: {noinit:?}"));
    }
    Ok(())
}

fn validate_journal_retained_sections(sections: &str) -> Result<(), String> {
    let rtc = parse_section_contract(sections, ".rtc_fast.persistent")?;
    if rtc
        != (
            "NOBITS".to_owned(),
            "600fe000".to_owned(),
            "000048".to_owned(),
        )
    {
        return Err(format!("reset journal section contract changed: {rtc:?}"));
    }
    let noinit = parse_section_contract(sections, ".noinit")?;
    if noinit.0 != "NOBITS" || noinit.2 != "000000" {
        return Err(format!(
            "RF-inert journal artifact unexpectedly owns noinit state: {noinit:?}"
        ));
    }
    Ok(())
}

fn parse_section_contract(sections: &str, name: &str) -> Result<(String, String, String), String> {
    for line in sections.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if let Some(index) = fields.iter().position(|field| *field == name)
            && fields.len() > index + 4
        {
            return Ok((
                fields[index + 1].to_owned(),
                fields[index + 2].to_ascii_lowercase(),
                fields[index + 4].to_ascii_lowercase(),
            ));
        }
    }
    Err(format!("ELF lacks required section {name}"))
}

fn validate_stack_guard_offset(symbols: &str) -> Result<(), String> {
    let bottom = parse_symbol_address(symbols, "_stack_end_cpu0")?;
    let guard = parse_symbol_address(symbols, "__stack_chk_guard")?;
    if guard.checked_sub(bottom) == Some(60) {
        Ok(())
    } else {
        Err(format!(
            "stack guard offset changed: guard=0x{guard:x}, bottom=0x{bottom:x}"
        ))
    }
}

fn validate_default_zero_bss_hook(symbols: &str) -> Result<(), String> {
    let zero_bss = parse_symbol_address(symbols, "__zero_bss")?;
    let default_hook = parse_symbol_address(symbols, "default_mem_hook")?;
    if zero_bss == default_hook {
        Ok(())
    } else {
        Err(format!(
            "RF-inert journal artifact replaced the runtime BSS hook: __zero_bss=0x{zero_bss:x} default_mem_hook=0x{default_hook:x}"
        ))
    }
}

fn parse_symbol_address(symbols: &str, name: &str) -> Result<u64, String> {
    for line in symbols.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.last().copied() == Some(name) {
            return u64::from_str_radix(fields[0], 16)
                .map_err(|_| format!("invalid address for symbol {name}"));
        }
    }
    Err(format!("ELF lacks symbol {name}"))
}

fn validate_zero_bss_hook(disassembly: &str) -> Result<(), String> {
    let mut lines = disassembly
        .lines()
        .skip_while(|line| !line.contains("<__zero_bss>:"));
    let Some(first) = lines.next() else {
        return Err("objdump lacks __zero_bss disassembly".to_owned());
    };
    let mut block = vec![first];
    for line in lines {
        if line.trim().is_empty() || (line.contains('<') && line.ends_with(":")) {
            break;
        }
        block.push(line);
    }
    let normalized = block
        .iter()
        .flat_map(|line| line.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ");
    for required in ["entry a1, 0", "movi.n a2, 1", "retw.n"] {
        if !normalized.contains(required) {
            return Err(format!(
                "__zero_bss lacks reviewed instruction {required:?}"
            ));
        }
    }
    if block.iter().any(|line| {
        line.split_whitespace()
            .any(|word| word.starts_with("call") && !word.starts_with("callback"))
    }) {
        return Err("__zero_bss is no longer a leaf function".to_owned());
    }
    Ok(())
}

fn validate_no_prohibited_tx_symbols(symbols: &str) -> Result<(), String> {
    for line in symbols.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 3 || fields[1].eq_ignore_ascii_case("a") {
            continue;
        }
        let name = fields[2..].join(" ");
        if prohibited_tx_symbol(&name) {
            return Err(format!("ELF retains prohibited TX symbol name: {name}"));
        }
    }
    Ok(())
}

fn validate_no_journal_runtime_ownership(symbols: &str) -> Result<(), String> {
    const PROHIBITED: &[&str] = &[
        "TrackerRxRadio",
        "RxOnlySpiDevice",
        "ReturnedFaultSpiDevice",
        "embedded_hal_bus::spi::ExclusiveDevice",
        "esp_hal::spi::master::Spi",
        "lora_phy::",
        "embassy_executor::",
        "embassy_time::",
        "esp_rtos::",
        "esp_hal::timer::timg::TimerGroup",
        "esp_hal::timer::timg::Wdt",
    ];
    for line in symbols.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 3 || fields[1].eq_ignore_ascii_case("a") {
            continue;
        }
        let name = fields[2..].join(" ");
        if let Some(prohibited) = PROHIBITED.iter().find(|item| name.contains(**item)) {
            return Err(format!(
                "RF-inert journal ELF retains prohibited runtime owner {prohibited:?}: {name}"
            ));
        }
    }
    Ok(())
}

fn prohibited_tx_symbol(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    (lower.contains("lora<") && symbol_component(&lower, "tx"))
        || symbol_component(&lower, "do_tx")
        || symbol_component(&lower, "prepare_for_tx")
        || (lower.contains("continuous") && (lower.contains("wave") || lower.contains("preamble")))
        || lower.contains("settxcontinuouswave")
        || lower.contains("settxcontinuouspreamble")
        || lower.contains("writebuffer")
        || (lower.contains("reteinterface") && symbol_component(&lower, "send"))
        || symbol_component(&lower, "send_data")
        || symbol_component(&lower, "send_link")
        || symbol_component(&lower, "send_link_data")
        || symbol_component(&lower, "send_link_message")
        || symbol_component(&lower, "send_channel")
        || symbol_component(&lower, "send_channel_data")
        || symbol_component(&lower, "send_channel_message")
        || symbol_component(&lower, "initiate_link")
}

fn symbol_component(name: &str, component: &str) -> bool {
    name == component
        || name.ends_with(&format!("::{component}"))
        || name.contains(&format!("::{component}::"))
        || name.contains(&format!("::{component}<"))
        || name.contains(&format!("::{component}("))
}

fn capture_stdout(program: &str, args: &[&str]) -> Result<String, String> {
    capture_stdout_at(program, args, Path::new("."))
}

fn capture_stdout_at(program: &str, args: &[&str], root: &Path) -> Result<String, String> {
    let output = Command::new(program)
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{program} exited with {}: {}",
            output.status,
            stderr.lines().next().unwrap_or("unknown error")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn temporary_build_root_uses_its_canonical_spelling() {
        let temporary =
            TemporaryDirectory::below(&env::temp_dir(), "phase1-hil-canonical").unwrap();
        assert_eq!(
            temporary.path(),
            fs::canonicalize(temporary.path()).unwrap()
        );
    }

    fn test_profile() -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "RETICULUM_LAB_RX_FREQUENCY_HZ".to_owned(),
                "915000000".to_owned(),
            ),
            (
                "RETICULUM_LAB_RX_SPREADING_FACTOR".to_owned(),
                "7".to_owned(),
            ),
            (
                "RETICULUM_LAB_RX_BANDWIDTH_HZ".to_owned(),
                "125000".to_owned(),
            ),
            (
                "RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR".to_owned(),
                "5".to_owned(),
            ),
            (
                "RETICULUM_LAB_RX_PREAMBLE_SYMBOLS".to_owned(),
                "18".to_owned(),
            ),
            (
                "RETICULUM_LAB_RX_EXPLICIT_HEADER".to_owned(),
                "1".to_owned(),
            ),
            ("RETICULUM_LAB_RX_CRC".to_owned(), "1".to_owned()),
            ("RETICULUM_LAB_RX_IQ_INVERTED".to_owned(), "0".to_owned()),
        ])
    }

    fn dummy_file(path: &str, digest_byte: char) -> FileRecord {
        FileRecord {
            path: path.to_owned(),
            sha256: std::iter::repeat_n(digest_byte, 64).collect(),
            bytes: 1,
        }
    }

    fn dummy_artifact(mode: ArtifactMode, digest_byte: char) -> ArtifactRecord {
        let prefix = if mode == ArtifactMode::LabRx {
            ""
        } else {
            "backpressure-artifact/"
        };
        ArtifactRecord {
            mode,
            feature: mode.feature().to_owned(),
            elf: dummy_file(&format!("{prefix}firmware.elf"), digest_byte),
            flash_image: dummy_file(
                &format!("{prefix}flash-image.bin"),
                char::from_u32(u32::from(digest_byte) + 1).unwrap(),
            ),
            flash_image_address: 0,
            size: ElfSize {
                text: 1,
                data: 1,
                bss: 1,
                total: 3,
                maximum_stack_frame: 1,
            },
        }
    }

    #[test]
    fn parser_accepts_only_explicit_prepare_verify_and_inspect_forms() {
        assert_eq!(
            parse_cli(strings(&[
                "prepare",
                "--output",
                "artifacts/hil/run",
                "--backpressure-stall-us",
                "7000000",
            ]))
            .unwrap(),
            Cli::Prepare {
                output: PathBuf::from("artifacts/hil/run"),
                backpressure_stall_us: 7_000_000,
            }
        );
        assert_eq!(
            parse_cli(strings(&["verify", "--bundle", "bundle"])).unwrap(),
            Cli::Verify {
                bundle: PathBuf::from("bundle")
            }
        );
        assert_eq!(
            parse_cli(strings(&[
                "inspect-elf",
                "--mode",
                "lab-rx-backpressure-hil",
                "--elf",
                "pressure.elf",
            ]))
            .unwrap(),
            Cli::InspectElf {
                elf: PathBuf::from("pressure.elf"),
                mode: InspectionMode::Backpressure,
            }
        );
        assert_eq!(
            parse_cli(strings(&[
                "inspect-elf",
                "--mode",
                "lab-rx-electrical-hil-dcdc-boosted",
                "--elf",
                "electrical.elf",
            ]))
            .unwrap(),
            Cli::InspectElf {
                elf: PathBuf::from("electrical.elf"),
                mode: InspectionMode::ElectricalDcdcBoosted,
            }
        );
    }

    #[test]
    fn parser_rejects_defaults_duplicates_unknowns_and_noncanonical_stalls() {
        for args in [
            strings(&["prepare", "--output", "out"]),
            strings(&[
                "prepare",
                "--output",
                "one",
                "--output",
                "two",
                "--backpressure-stall-us",
                "7000000",
            ]),
            strings(&[
                "prepare",
                "--output",
                "out",
                "--backpressure-stall-us",
                "07000000",
            ]),
            strings(&["prepare", "--output", "out", "--backpressure-stall-us", "0"]),
            strings(&["verify", "--bundle", "out", "--force", "yes"]),
            strings(&["send", "--port", "/dev/ttyUSB0"]),
        ] {
            assert!(parse_cli(args).is_err());
        }
        for mode in InspectionMode::ALL {
            assert_eq!(InspectionMode::parse(mode.as_str()).unwrap(), mode);
        }
        assert!(InspectionMode::parse("lab-rx-electrical-hil-default").is_err());
    }

    #[test]
    fn planned_artifact_commands_are_build_or_host_only_save_only() {
        let temporary = TemporaryDirectory::below(&env::temp_dir(), "hil-command-test").unwrap();
        phase1_tooling::create_controlled_tmpdir(temporary.path()).unwrap();
        let espflash_context =
            phase1_image::OfflineEspflashContext::create(temporary.path()).unwrap();
        let qualification_environment =
            phase1_tooling::QualificationEnvironment::capture().unwrap();
        let profile = test_profile();
        let cargo_home = temporary.path().join("isolated-cargo-home");
        let normal_target = temporary.path().join("normal-target");
        let pressure_target = temporary.path().join("pressure-target");
        fs::create_dir(&cargo_home).unwrap();
        let build_context = phase1_tooling::CargoBuildContext::new(
            temporary.path(),
            &cargo_home,
            "1000000",
            &qualification_environment,
        )
        .unwrap();
        let normal = build_spec(
            &profile,
            ArtifactMode::LabRx,
            None,
            &normal_target,
            &build_context,
        )
        .unwrap();
        let pressure = build_spec(
            &profile,
            ArtifactMode::LabRxBackpressureHil,
            Some(7_000_000),
            &pressure_target,
            &build_context,
        )
        .unwrap();
        let normal_save = save_image_spec(
            Path::new("normal.elf"),
            Path::new("normal.bin"),
            &espflash_context,
            &qualification_environment,
        );
        let pressure_save = save_image_spec(
            Path::new("pressure.elf"),
            Path::new("pressure.bin"),
            &espflash_context,
            &qualification_environment,
        );
        for command in [&normal, &pressure, &normal_save, &pressure_save] {
            validate_offline_artifact_command(command).unwrap();
            assert!(!command.args.iter().any(|argument| {
                matches!(
                    argument.as_str(),
                    "flash" | "write-bin" | "read-flash" | "monitor" | "--port"
                )
            }));
        }
        assert_eq!(normal.args[7], NORMAL_BINARY);
        assert_eq!(pressure.args[7], BACKPRESSURE_BINARY);
        assert_ne!(
            normal.env["CARGO_TARGET_DIR"],
            pressure.env["CARGO_TARGET_DIR"]
        );
        assert!(!normal.env.contains_key(STALL_ENV));
        assert_eq!(pressure.env[STALL_ENV], "7000000");
        assert_eq!(normal.env["CARGO_HOME"], cargo_home.display().to_string());
        assert_eq!(pressure.env["CARGO_HOME"], cargo_home.display().to_string());
        assert_eq!(normal.env["SOURCE_DATE_EPOCH"], "1000000");
        assert!(!normal.env.contains_key("RUSTC_WRAPPER"));
        assert!(!normal.env.contains_key("GITHUB_TOKEN"));
        assert!(!normal.env.keys().any(|name| name.starts_with("ESP_")));
        assert!(normal.env["CARGO_ENCODED_RUSTFLAGS"].contains("emit-stack-sizes"));
        assert!(normal.env["CARGO_ENCODED_RUSTFLAGS"].contains(phase1_tooling::BUILD_ROOT_REMAP));
        assert!(normal.env["CARGO_ENCODED_RUSTFLAGS"].contains(phase1_tooling::RUSTUP_HOME_REMAP));
        assert_eq!(normal_save.args[0], "save-image");
        assert_eq!(normal_save.env["ESPFLASH_SKIP_UPDATE_CHECK"], "true");
        for name in ["ESPFLASH_PORT", "ESPFLASH_BAUD", "MONITOR_BAUD"] {
            assert!(!normal_save.env.contains_key(name));
        }
        assert!(normal_save.env.contains_key("HOME"));
        assert!(normal_save.env.contains_key("XDG_CONFIG_HOME"));
        assert!(normal_save.env.contains_key("TMPDIR"));
    }

    #[test]
    fn hardware_affecting_espflash_commands_are_not_allowed() {
        let command = CommandSpec {
            program: "espflash".to_owned(),
            args: strings(&["write-bin", "0", "firmware.bin"]),
            env: BTreeMap::new(),
        };
        assert!(validate_offline_artifact_command(&command).is_err());
    }

    #[test]
    fn command_specs_start_from_an_empty_process_environment() {
        let command = CommandSpec {
            program: "/usr/bin/env".to_owned(),
            args: Vec::new(),
            env: BTreeMap::from([("QUALIFICATION_ONLY".to_owned(), "reviewed".to_owned())]),
        };
        let output = command.command().output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "QUALIFICATION_ONLY=reviewed\n"
        );
    }

    #[test]
    fn normal_and_pressure_records_must_have_distinct_modes_paths_and_bytes() {
        let normal = dummy_artifact(ArtifactMode::LabRx, '1');
        let pressure = dummy_artifact(ArtifactMode::LabRxBackpressureHil, '3');
        ensure_artifacts_distinct(&normal, &pressure).unwrap();

        let mut collision = pressure.clone();
        collision.elf.sha256.clone_from(&normal.elf.sha256);
        assert!(ensure_artifacts_distinct(&normal, &collision).is_err());

        let mut path_collision = pressure;
        path_collision.elf.path.clone_from(&normal.elf.path);
        assert!(ensure_artifacts_distinct(&normal, &path_collision).is_err());
    }

    #[test]
    fn manifest_paths_are_confined_and_canonical() {
        for rejected in ["", "/absolute", "../escape", "one/../escape", "./one"] {
            let record = FileRecord {
                path: rejected.to_owned(),
                sha256: "0".repeat(64),
                bytes: 0,
            };
            assert!(resolve_record_path(Path::new("bundle"), &record).is_err());
        }
        let record = FileRecord {
            path: "backpressure-artifact/firmware.elf".to_owned(),
            sha256: "0".repeat(64),
            bytes: 0,
        };
        assert_eq!(
            resolve_record_path(Path::new("bundle"), &record).unwrap(),
            PathBuf::from("bundle/backpressure-artifact/firmware.elf")
        );
    }

    #[test]
    fn in_workspace_output_must_be_git_ignored() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        ensure_output_location_does_not_dirty_source(
            root,
            &root.join("artifacts/hil/phase1-rx/unit-test"),
        )
        .unwrap();
        assert!(
            ensure_output_location_does_not_dirty_source(
                root,
                &root.join("docs/qualification-output")
            )
            .is_err()
        );
        ensure_output_location_does_not_dirty_source(
            root,
            &env::temp_dir().join("phase1-rx-external-output"),
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn external_looking_symlink_parent_cannot_bypass_workspace_ignore_policy() {
        use std::os::unix::fs::symlink;

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let temporary =
            TemporaryDirectory::below(&env::temp_dir(), "phase1-output-link-test").unwrap();
        let unignored_alias = temporary.path().join("unignored");
        symlink(root.join("docs"), &unignored_alias).unwrap();
        assert!(
            ensure_output_location_does_not_dirty_source(
                root,
                &unignored_alias.join("qualification-output")
            )
            .is_err()
        );

        let ignored_alias = temporary.path().join("ignored");
        symlink(root.join("target"), &ignored_alias).unwrap();
        ensure_output_location_does_not_dirty_source(
            root,
            &ignored_alias.join("phase1-output-link-test"),
        )
        .unwrap();
    }

    #[test]
    fn file_record_verification_detects_byte_and_digest_tampering() {
        let temp = TemporaryDirectory::below(&env::temp_dir(), "phase1-hil-file-test").unwrap();
        let path = temp.path().join("firmware.elf");
        fs::write(&path, b"original").unwrap();
        let record = file_record(temp.path(), &path).unwrap();
        verify_file_record(temp.path(), &record).unwrap();
        fs::write(&path, b"tampered").unwrap();
        assert!(verify_file_record(temp.path(), &record).is_err());
    }

    fn populate_expected_bundle_files(bundle: &Path, incomplete: bool) {
        fs::create_dir(bundle.join("backpressure-artifact")).unwrap();
        for relative in PREPARED_FILES {
            let path = bundle.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, relative.as_bytes()).unwrap();
        }
        write_prepared_hashes(bundle).unwrap();
        fs::write(
            bundle.join(if incomplete {
                INCOMPLETE_FILE
            } else {
                COMPLETE_FILE
            }),
            if incomplete {
                b"incomplete\n".as_slice()
            } else {
                COMPLETE_CONTENT.as_bytes()
            },
        )
        .unwrap();
    }

    #[test]
    fn prepared_hash_manifest_detects_any_preserved_file_change() {
        let temp = TemporaryDirectory::below(&env::temp_dir(), "phase1-hil-hash-test").unwrap();
        populate_expected_bundle_files(temp.path(), true);
        verify_prepared_hashes(temp.path()).unwrap();
        fs::write(temp.path().join("firmware.elf"), b"changed").unwrap();
        assert!(verify_prepared_hashes(temp.path()).is_err());
    }

    #[test]
    fn exact_bundle_tree_rejects_missing_and_extra_paths() {
        let temp = TemporaryDirectory::below(&env::temp_dir(), "phase1-hil-tree-test").unwrap();
        populate_expected_bundle_files(temp.path(), true);
        verify_bundle_tree(temp.path(), false).unwrap();

        fs::write(temp.path().join("unexpected.log"), b"extra").unwrap();
        assert!(verify_bundle_tree(temp.path(), false).is_err());
        fs::remove_file(temp.path().join("unexpected.log")).unwrap();

        fs::create_dir(temp.path().join("unexpected-directory")).unwrap();
        assert!(verify_bundle_tree(temp.path(), false).is_err());
        fs::remove_dir(temp.path().join("unexpected-directory")).unwrap();

        fs::remove_file(temp.path().join("firmware.sha256")).unwrap();
        assert!(verify_bundle_tree(temp.path(), false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn exact_bundle_tree_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let temp =
            TemporaryDirectory::below(&env::temp_dir(), "phase1-hil-tree-link-test").unwrap();
        populate_expected_bundle_files(temp.path(), false);
        verify_bundle_tree(temp.path(), true).unwrap();

        fs::remove_file(temp.path().join("source.sha256")).unwrap();
        symlink("source.tar", temp.path().join("source.sha256")).unwrap();
        assert!(verify_bundle_tree(temp.path(), true).is_err());
    }

    #[test]
    fn completion_state_is_fail_closed() {
        let temp = TemporaryDirectory::below(&env::temp_dir(), "phase1-hil-marker-test").unwrap();
        fs::write(temp.path().join(INCOMPLETE_FILE), b"incomplete\n").unwrap();
        verify_completion_state(temp.path(), false).unwrap();
        assert!(verify_completion_state(temp.path(), true).is_err());
        fs::write(temp.path().join(COMPLETE_FILE), COMPLETE_CONTENT).unwrap();
        assert!(verify_completion_state(temp.path(), false).is_err());
        assert!(verify_completion_state(temp.path(), true).is_err());
        fs::remove_file(temp.path().join(INCOMPLETE_FILE)).unwrap();
        verify_completion_state(temp.path(), true).unwrap();
    }

    #[test]
    fn size_and_stack_size_parsers_enforce_exact_records() {
        let size =
            parse_size_output("text data bss dec hex filename\n10 20 30 60 3c fw\n").unwrap();
        assert_eq!(size.text, 10);
        assert_eq!(size.total, 60);

        let mut records = Vec::new();
        records.extend_from_slice(&0x4201_0000_u32.to_le_bytes());
        records.push(0x20);
        records.extend_from_slice(&0x4201_0100_u32.to_le_bytes());
        records.extend_from_slice(&[0xa0, 0xf5, 0x02]); // 47,776
        assert_eq!(parse_stack_size_records(&records, 4).unwrap(), 47_776);
        assert!(parse_stack_size_records(&records[..records.len() - 1], 4).is_err());
        assert!(decode_uleb128(&[0x80]).is_err());
    }

    #[test]
    fn stack_frame_ceiling_is_deliberately_48_kib() {
        assert_eq!(MAXIMUM_STACK_FRAME_BYTES, 48 * 1024);
    }

    #[test]
    fn elf_contract_parsers_cover_sections_symbols_hook_modes_and_tx_names() {
        let sections = "[ 1] .rtc_fast.persistent NOBITS 600fe000 001000 000048 00 WA 0 0 4\n[ 2] .noinit NOBITS 3fce0000 002000 000020 00 WA 0 0 4\n";
        validate_retained_sections(sections).unwrap();
        let symbols = "600fe000 00000048 B crate::RESET_QUARANTINE_JOURNAL\n3fce0000 00000020 B RETICULUM_STACK_WATERMARK_MARKER\n42000000 0000004a T __zero_bss\n";
        validate_full_stack_retained_symbols(symbols, InspectionMode::Normal).unwrap();
        let ordered = "3fc00000 B _stack_end_cpu0\n3fc0003c B __stack_chk_guard\n";
        validate_stack_guard_offset(ordered).unwrap();
        let hook =
            "42000000 <__zero_bss>:\n  entry a1, 0\n  movi.n a2, 1\n  retw.n\n\n4200004a <next>:\n";
        validate_zero_bss_hook(hook).unwrap();
        let runtime_patch = "esp-rtos-0.3.0-cpu0-cpu1-main-stack-words-v2";
        let normal = format!(
            "action=immediate_rf_inert_quarantine {runtime_patch} phase1 artifact identity: mode= phase1 lab-rx active: board= heltec-tracker-v2lab-rx"
        );
        validate_mode_strings(&normal, InspectionMode::Normal).unwrap();
        assert!(validate_mode_strings(&normal, InspectionMode::Backpressure).is_err());
        let pressure = format!(
            "action=immediate_rf_inert_quarantine {runtime_patch} lab-rx-backpressure-hil first_awaiting_continuation phase1 backpressure HIL triggered: phase1 backpressure HIL completed:"
        );
        validate_mode_strings(&pressure, InspectionMode::Backpressure).unwrap();
        assert!(validate_mode_strings(&pressure, InspectionMode::Normal).is_err());
        let electrical = format!(
            "action=immediate_rf_inert_quarantine {runtime_patch} lab-rx-electrical-hil;regulator=dcdc;rx_gain=boosted"
        );
        validate_mode_strings(&electrical, InspectionMode::ElectricalDcdcBoosted).unwrap();
        assert!(validate_mode_strings(&electrical, InspectionMode::ElectricalLdoBoosted).is_err());
        assert!(validate_mode_strings(&electrical, InspectionMode::Normal).is_err());
        let returned = format!(
            "action=immediate_rf_inert_quarantine {runtime_patch} lab-rx-returned-fault-hil;trigger=get-irq-status-after-set-rx;policy=repeat-until-quarantine phase1 returned-fault HIL evidence: get_irq_status_rejected_before_spi= set_rx_forwarded= fired="
        );
        validate_mode_strings(
            &returned,
            InspectionMode::ReturnedFaultRepeatUntilQuarantine,
        )
        .unwrap();
        let journal = "phase1 reset-journal HIL artifact identity: phase1 reset-journal HIL baseline: phase1 reset-journal HIL triggered: phase1 reset-journal HIL expected quarantine: reason=CorruptOrTornJournal history=None radio_constructed=false spi_constructed=false executor_timer_constructed=false supervisor_watchdog=off rf_state=reset_low_fem_low lab-rx-reset-journal-corrupt-hil trigger=corrupt-word slot= word= xor_mask=0x";
        validate_mode_strings(journal, InspectionMode::ResetJournalCorrupt).unwrap();
        let journal_sections = "[ 1] .rtc_fast.persistent NOBITS 600fe000 001000 000048 00 WA 0 0 4\n[ 2] .noinit NOBITS 3fce0000 002000 000000 00 WA 0 0 4\n";
        validate_journal_retained_sections(journal_sections).unwrap();
        let journal_symbols =
            "600fe000 00000048 B crate::RESET_QUARANTINE_JOURNAL\n42000000 T __zero_bss\n";
        validate_journal_retained_symbols(journal_symbols).unwrap();
        validate_default_zero_bss_hook("42000000 T __zero_bss\n42000000 T default_mem_hook\n")
            .unwrap();
        validate_no_journal_runtime_ownership("42000000 T journal::main").unwrap();
        assert!(
            validate_no_journal_runtime_ownership("42000000 T esp_hal::spi::master::Spi::new")
                .is_err()
        );
        assert!(prohibited_tx_symbol("lora_phy::LoRa<SPI>::prepare_for_tx"));
        assert!(prohibited_tx_symbol("rete::ReteInterface::send"));
        assert!(!prohibited_tx_symbol("reticulum::receive_only::ingest"));
    }

    #[test]
    fn profile_manifest_must_have_exact_fields_and_canonical_values() {
        let profile = test_profile();
        validate_profile_map(&profile).unwrap();
        let mut missing = profile.clone();
        missing.remove("RETICULUM_LAB_RX_CRC");
        assert!(validate_profile_map(&missing).is_err());
        let mut noncanonical = profile;
        noncanonical.insert(
            "RETICULUM_LAB_RX_FREQUENCY_HZ".to_owned(),
            "0915000000".to_owned(),
        );
        assert!(validate_profile_map(&noncanonical).is_err());
    }
}
