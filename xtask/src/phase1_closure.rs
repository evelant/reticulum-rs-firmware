use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{phase1_hil, phase1_image, phase1_source, phase1_tooling};

const SCHEMA: &str = "reticulum.phase1-rx-closure-artifacts.v2";
const INCOMPLETE_FILE: &str = "closure-artifact-preparation.incomplete";
const COMPLETE_FILE: &str = "closure-artifact-preparation.complete";
const COMPLETE_CONTENT: &str = "reticulum.phase1-rx-closure-artifacts.v2\n";
const MANIFEST_FILE: &str = "closure-artifact-preparation.json";
const PREPARED_HASH_FILE: &str = "closure-prepared-artifacts.sha256";

const FULL_STACK_RUSTFLAGS: &str = "-C link-arg=-nostartfiles -Z emit-stack-sizes";
const FULL_STACK_RUSTFLAG_ARGUMENTS: &[&str] =
    &["-C", "link-arg=-nostartfiles", "-Z", "emit-stack-sizes"];
const PACKAGE: &str = "reticulum-heltec-tracker-v2";
const TARGET: &str = "xtensa-esp32s3-none-elf";

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProfileValueKind {
    Unsigned,
    Boolean,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Cli {
    Prepare {
        output: PathBuf,
        journal: JournalParameters,
    },
    Verify {
        bundle: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ClosureArtifactId {
    ElectricalLdoUnboosted,
    ElectricalLdoBoosted,
    ElectricalDcdcUnboosted,
    ElectricalDcdcBoosted,
    ReturnedFaultOneBoot,
    ReturnedFaultRepeatUntilQuarantine,
    ResetJournalCorrupt,
    ResetJournalTorn,
}

impl ClosureArtifactId {
    const ALL: [Self; 8] = [
        Self::ElectricalLdoUnboosted,
        Self::ElectricalLdoBoosted,
        Self::ElectricalDcdcUnboosted,
        Self::ElectricalDcdcBoosted,
        Self::ReturnedFaultOneBoot,
        Self::ReturnedFaultRepeatUntilQuarantine,
        Self::ResetJournalCorrupt,
        Self::ResetJournalTorn,
    ];

    const fn slug(self) -> &'static str {
        match self {
            Self::ElectricalLdoUnboosted => "electrical-ldo-unboosted",
            Self::ElectricalLdoBoosted => "electrical-ldo-boosted",
            Self::ElectricalDcdcUnboosted => "electrical-dcdc-unboosted",
            Self::ElectricalDcdcBoosted => "electrical-dcdc-boosted",
            Self::ReturnedFaultOneBoot => "returned-fault-one-boot",
            Self::ReturnedFaultRepeatUntilQuarantine => "returned-fault-repeat-until-quarantine",
            Self::ResetJournalCorrupt => "reset-journal-corrupt-slot0-word4",
            Self::ResetJournalTorn => "reset-journal-torn-write9",
        }
    }

    const fn inspection_mode(self) -> &'static str {
        match self {
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

    const fn feature(self) -> &'static str {
        match self {
            Self::ElectricalLdoUnboosted
            | Self::ElectricalLdoBoosted
            | Self::ElectricalDcdcUnboosted
            | Self::ElectricalDcdcBoosted => "lab-rx-electrical-hil",
            Self::ReturnedFaultOneBoot | Self::ReturnedFaultRepeatUntilQuarantine => {
                "lab-rx-returned-fault-hil"
            }
            Self::ResetJournalCorrupt => "lab-rx-reset-journal-corrupt-hil",
            Self::ResetJournalTorn => "lab-rx-reset-journal-torn-hil",
        }
    }

    const fn binary(self) -> &'static str {
        match self {
            Self::ElectricalLdoUnboosted
            | Self::ElectricalLdoBoosted
            | Self::ElectricalDcdcUnboosted
            | Self::ElectricalDcdcBoosted => "reticulum-heltec-tracker-v2-lab-rx-electrical-hil",
            Self::ReturnedFaultOneBoot | Self::ReturnedFaultRepeatUntilQuarantine => {
                "reticulum-heltec-tracker-v2-lab-rx-returned-fault-hil"
            }
            Self::ResetJournalCorrupt => "reticulum-heltec-tracker-v2-lab-rx-reset-journal-corrupt",
            Self::ResetJournalTorn => "reticulum-heltec-tracker-v2-lab-rx-reset-journal-torn",
        }
    }

    const fn full_stack(self) -> bool {
        !matches!(self, Self::ResetJournalCorrupt | Self::ResetJournalTorn)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalParameters {
    corrupt_slot: u8,
    corrupt_word: u8,
    torn_write_ordinal: u8,
}

impl JournalParameters {
    const REVIEWED: Self = Self {
        corrupt_slot: 0,
        corrupt_word: 4,
        torn_write_ordinal: 9,
    };
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
struct ElfSize {
    text: u64,
    data: u64,
    bss: u64,
    total: u64,
    maximum_stack_frame: u64,
}

impl From<phase1_hil::ElfSize> for ElfSize {
    fn from(size: phase1_hil::ElfSize) -> Self {
        Self {
            text: size.text,
            data: size.data,
            bss: size.bss,
            total: size.total,
            maximum_stack_frame: size.maximum_stack_frame,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRecord {
    id: ClosureArtifactId,
    feature: String,
    binary: String,
    inspection_mode: String,
    build_environment: BTreeMap<String, String>,
    cargo_arguments: Vec<String>,
    elf: FileRecord,
    flash_image: FileRecord,
    flash_image_address: u32,
    size: ElfSize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ReproducibilityRecord {
    canary: ClosureArtifactId,
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
    dedicated_target_directory: bool,
    copy_after_each_build: bool,
    full_stack_rustflags: String,
    journal_rustflags: Option<String>,
    encoded_rustflags: bool,
    environment_cleared: bool,
    ambient_environment_allowlist: Vec<String>,
    explicit_build_environment_names: Vec<String>,
    environment_policy: String,
    source_date_epoch_microseconds: String,
    build_root_remap: String,
    rustup_home_remap: String,
    build_from_source_archive: bool,
    source_parent_outside_workspace: bool,
    isolated_cargo_home: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ClosureManifest {
    schema: String,
    prepared_unix_seconds: u64,
    git_commit: String,
    git_root_tree: String,
    worktree_clean: bool,
    tools: BTreeMap<String, String>,
    profile_environment: BTreeMap<String, String>,
    journal: JournalParameters,
    source_archive: FileRecord,
    build_recipe: BuildRecipe,
    reproducibility: ReproducibilityRecord,
    artifacts: Vec<ArtifactRecord>,
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
        Ok(Cli::Prepare { output, journal }) => prepare(root, &output, journal),
        Ok(Cli::Verify { bundle }) => verify_bundle(root, &bundle, true),
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
    "usage:\n  cargo run --locked -p xtask -- phase1-rx-closure-artifacts prepare --output <absent-directory> --journal-corrupt-slot 0 --journal-corrupt-word 4 --journal-torn-write-ordinal 9\n  cargo run --locked -p xtask -- phase1-rx-closure-artifacts verify --bundle <directory>"
}

fn parse_cli(args: Vec<String>) -> Result<Cli, String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err("missing phase1-rx-closure-artifacts subcommand".to_owned());
    };
    let flags = parse_flags(&args[1..])?;
    match subcommand {
        "prepare" => {
            require_exact_flags(
                &flags,
                &[
                    "--output",
                    "--journal-corrupt-slot",
                    "--journal-corrupt-word",
                    "--journal-torn-write-ordinal",
                ],
            )?;
            let journal = JournalParameters {
                corrupt_slot: parse_canonical_u8(
                    "--journal-corrupt-slot",
                    flags
                        .get("--journal-corrupt-slot")
                        .expect("required flag checked"),
                )?,
                corrupt_word: parse_canonical_u8(
                    "--journal-corrupt-word",
                    flags
                        .get("--journal-corrupt-word")
                        .expect("required flag checked"),
                )?,
                torn_write_ordinal: parse_canonical_u8(
                    "--journal-torn-write-ordinal",
                    flags
                        .get("--journal-torn-write-ordinal")
                        .expect("required flag checked"),
                )?,
            };
            validate_reviewed_journal_parameters(journal)?;
            Ok(Cli::Prepare {
                output: PathBuf::from(flags.get("--output").expect("required flag checked")),
                journal,
            })
        }
        "verify" => {
            require_exact_flags(&flags, &["--bundle"])?;
            Ok(Cli::Verify {
                bundle: PathBuf::from(flags.get("--bundle").expect("required flag checked")),
            })
        }
        _ => Err(format!(
            "unknown phase1-rx-closure-artifacts subcommand {subcommand:?}"
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

fn parse_canonical_u8(name: &str, value: &str) -> Result<u8, String> {
    let parsed = value
        .parse::<u8>()
        .map_err(|_| format!("{name} must be a canonical unsigned decimal u8"))?;
    if parsed.to_string() != value {
        return Err(format!("{name} must be a canonical unsigned decimal u8"));
    }
    Ok(parsed)
}

fn validate_reviewed_journal_parameters(journal: JournalParameters) -> Result<(), String> {
    if journal == JournalParameters::REVIEWED {
        Ok(())
    } else {
        Err(format!(
            "the Phase-1 closure bundle requires the reviewed journal selection slot=0 word=4 torn_write_ordinal=9, got slot={} word={} torn_write_ordinal={}",
            journal.corrupt_slot, journal.corrupt_word, journal.torn_write_ordinal
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

fn expected_artifact_environment(
    id: ClosureArtifactId,
    profile: &BTreeMap<String, String>,
    journal: JournalParameters,
) -> BTreeMap<String, String> {
    let mut selected = if id.full_stack() {
        profile.clone()
    } else {
        BTreeMap::new()
    };
    match id {
        ClosureArtifactId::ElectricalLdoUnboosted => {
            selected.insert("RETICULUM_LAB_RX_REGULATOR".to_owned(), "ldo".to_owned());
            selected.insert("RETICULUM_LAB_RX_GAIN".to_owned(), "unboosted".to_owned());
        }
        ClosureArtifactId::ElectricalLdoBoosted => {
            selected.insert("RETICULUM_LAB_RX_REGULATOR".to_owned(), "ldo".to_owned());
            selected.insert("RETICULUM_LAB_RX_GAIN".to_owned(), "boosted".to_owned());
        }
        ClosureArtifactId::ElectricalDcdcUnboosted => {
            selected.insert("RETICULUM_LAB_RX_REGULATOR".to_owned(), "dcdc".to_owned());
            selected.insert("RETICULUM_LAB_RX_GAIN".to_owned(), "unboosted".to_owned());
        }
        ClosureArtifactId::ElectricalDcdcBoosted => {
            selected.insert("RETICULUM_LAB_RX_REGULATOR".to_owned(), "dcdc".to_owned());
            selected.insert("RETICULUM_LAB_RX_GAIN".to_owned(), "boosted".to_owned());
        }
        ClosureArtifactId::ReturnedFaultOneBoot => {
            selected.insert(
                "RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER".to_owned(),
                "get-irq-status-after-set-rx".to_owned(),
            );
            selected.insert(
                "RETICULUM_LAB_RX_RETURNED_FAULT_POLICY".to_owned(),
                "one-boot".to_owned(),
            );
        }
        ClosureArtifactId::ReturnedFaultRepeatUntilQuarantine => {
            selected.insert(
                "RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER".to_owned(),
                "get-irq-status-after-set-rx".to_owned(),
            );
            selected.insert(
                "RETICULUM_LAB_RX_RETURNED_FAULT_POLICY".to_owned(),
                "repeat-until-quarantine".to_owned(),
            );
        }
        ClosureArtifactId::ResetJournalCorrupt => {
            selected.insert(
                "RETICULUM_LAB_RX_RESET_JOURNAL_SLOT".to_owned(),
                journal.corrupt_slot.to_string(),
            );
            selected.insert(
                "RETICULUM_LAB_RX_RESET_JOURNAL_WORD".to_owned(),
                journal.corrupt_word.to_string(),
            );
        }
        ClosureArtifactId::ResetJournalTorn => {
            selected.insert(
                "RETICULUM_LAB_RX_RESET_JOURNAL_WRITE_ORDINAL".to_owned(),
                journal.torn_write_ordinal.to_string(),
            );
        }
    }
    selected
}

fn build_arguments(id: ClosureArtifactId) -> Vec<String> {
    [
        "+esp",
        "build",
        "--locked",
        "--release",
        "-p",
        PACKAGE,
        "--bin",
        id.binary(),
        "--no-default-features",
        "--features",
        id.feature(),
        "--target",
        TARGET,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn build_spec(
    id: ClosureArtifactId,
    selected: &BTreeMap<String, String>,
    target_dir: &Path,
    build_context: &phase1_tooling::CargoBuildContext<'_>,
) -> Result<CommandSpec, String> {
    let base_flags = if id.full_stack() {
        FULL_STACK_RUSTFLAG_ARGUMENTS
    } else {
        &[]
    };
    let command_env = build_context.environment(selected, target_dir, base_flags)?;
    Ok(CommandSpec {
        program: "cargo".to_owned(),
        args: build_arguments(id),
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

fn prepare(root: &Path, output_arg: &Path, journal: JournalParameters) -> Result<(), String> {
    validate_reviewed_journal_parameters(journal)?;
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
    fs::create_dir(output.join("artifacts")).map_err(|error| {
        format!(
            "could not create closure artifact root {}: {error}",
            output.join("artifacts").display()
        )
    })?;
    for id in ClosureArtifactId::ALL {
        fs::create_dir(output.join("artifacts").join(id.slug())).map_err(|error| {
            format!(
                "could not create closure artifact directory {}: {error}",
                output.join("artifacts").join(id.slug()).display()
            )
        })?;
    }

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

    let temporary = TemporaryDirectory::below(&env::temp_dir(), "phase1-rx-closure")?;
    phase1_source::ensure_path_outside_workspace(root, temporary.path())?;
    let source_root = temporary.path().join("source");
    fs::create_dir(&source_root).map_err(|error| {
        format!(
            "could not create archived source build root {}: {error}",
            source_root.display()
        )
    })?;
    phase1_source::extract_source_archive(&output.join("source.tar"), &source_root)?;
    phase1_source::reject_ambient_ancestor_cargo_configs(&source_root)?;
    let target_dir = temporary.path().join("build-target");
    let cargo_home = temporary.path().join("cargo-home");
    fs::create_dir(&cargo_home).map_err(|error| {
        format!(
            "could not create isolated Cargo home {}: {error}",
            cargo_home.display()
        )
    })?;
    phase1_tooling::create_controlled_tmpdir(temporary.path())?;
    let build_context = phase1_tooling::CargoBuildContext::new(
        temporary.path(),
        &cargo_home,
        &source_date_epoch_microseconds,
        &qualification_environment,
    )?;
    let espflash_context = phase1_image::OfflineEspflashContext::create(temporary.path())?;

    let mut artifacts = Vec::with_capacity(ClosureArtifactId::ALL.len());
    for id in ClosureArtifactId::ALL {
        let directory = output.join("artifacts").join(id.slug());
        let selected = expected_artifact_environment(id, &profile, journal);
        let build = build_spec(id, &selected, &target_dir, &build_context)?;
        validate_offline_artifact_command(&build)?;
        run_logged(&build, &source_root, &directory.join("build.log"))?;

        let elf = directory.join("firmware.elf");
        copy_built_elf(&target_dir, id, &elf)?;
        let image = directory.join("flash-image.bin");
        let save = save_image_spec(&elf, &image, &espflash_context, &qualification_environment);
        validate_offline_artifact_command(&save)?;
        run_logged(
            &save,
            espflash_context.workdir(),
            &directory.join("save-image.log"),
        )?;
        write_artifact_sidecars(&directory, &elf, &image)?;
        let size = phase1_hil::inspect_elf_by_name(&elf, id.inspection_mode())?.into();
        artifacts.push(ArtifactRecord {
            id,
            feature: id.feature().to_owned(),
            binary: id.binary().to_owned(),
            inspection_mode: id.inspection_mode().to_owned(),
            build_environment: selected,
            cargo_arguments: build_arguments(id),
            elf: file_record(&output, &elf)?,
            flash_image: file_record(&output, &image)?,
            flash_image_address: 0,
            size,
        });
    }
    ensure_artifacts_distinct(&artifacts)?;
    let reproducibility = independently_rebuild_and_compare(
        root,
        &output.join("source.tar"),
        &profile,
        journal,
        &source_date_epoch_microseconds,
        &qualification_environment,
        &output,
    )?;
    phase1_source::ensure_source_identity_unchanged(root, &source_identity)?;

    let manifest = ClosureManifest {
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
        journal,
        source_archive: file_record(&output, &output.join("source.tar"))?,
        build_recipe: reviewed_build_recipe(&source_date_epoch_microseconds),
        reproducibility,
        artifacts,
    };
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("could not serialize closure artifact manifest: {error}"))?;
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
        "ok: prepared and verified eight Phase-1 closure artifacts without hardware operations in {}",
        output.display()
    );
    println!(
        "next: follow docs/phase-1-rx-hil.md for explicit flash, powered measurements and retained-reset evidence"
    );
    Ok(())
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

fn copy_built_elf(
    target_dir: &Path,
    id: ClosureArtifactId,
    destination: &Path,
) -> Result<(), String> {
    let source = target_dir.join(TARGET).join("release").join(id.binary());
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
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not stat artifact {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
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

fn expected_prepared_files() -> BTreeSet<String> {
    let mut files = BTreeSet::from([
        MANIFEST_FILE.to_owned(),
        "source.sha256".to_owned(),
        "source.tar".to_owned(),
        "tool-and-source-versions.txt".to_owned(),
    ]);
    for id in ClosureArtifactId::ALL {
        let prefix = format!("artifacts/{}/", id.slug());
        for name in [
            "build.log",
            "firmware.elf",
            "firmware.sha256",
            "flash-image-address.txt",
            "flash-image-bytes.txt",
            "flash-image.bin",
            "flash-image.sha256",
            "save-image.log",
        ] {
            files.insert(format!("{prefix}{name}"));
        }
    }
    files
}

fn expected_directories() -> BTreeSet<String> {
    let mut directories = BTreeSet::from(["artifacts".to_owned()]);
    for id in ClosureArtifactId::ALL {
        directories.insert(format!("artifacts/{}", id.slug()));
    }
    directories
}

fn write_prepared_hashes(bundle: &Path) -> Result<(), String> {
    let mut text = String::new();
    for relative in expected_prepared_files() {
        let digest = sha256_file(&bundle.join(&relative))?;
        text.push_str(&format!("{digest}  {relative}\n"));
    }
    write_new(&bundle.join(PREPARED_HASH_FILE), text.as_bytes())
}

fn ensure_artifacts_distinct(artifacts: &[ArtifactRecord]) -> Result<(), String> {
    if artifacts.len() != ClosureArtifactId::ALL.len() {
        return Err(format!(
            "closure bundle must contain exactly eight artifact records, found {}",
            artifacts.len()
        ));
    }
    let ids = artifacts
        .iter()
        .map(|artifact| artifact.id)
        .collect::<BTreeSet<_>>();
    let elf_paths = artifacts
        .iter()
        .map(|artifact| artifact.elf.path.as_str())
        .collect::<BTreeSet<_>>();
    let image_paths = artifacts
        .iter()
        .map(|artifact| artifact.flash_image.path.as_str())
        .collect::<BTreeSet<_>>();
    let elf_hashes = artifacts
        .iter()
        .map(|artifact| artifact.elf.sha256.as_str())
        .collect::<BTreeSet<_>>();
    let image_hashes = artifacts
        .iter()
        .map(|artifact| artifact.flash_image.sha256.as_str())
        .collect::<BTreeSet<_>>();
    let expected = ClosureArtifactId::ALL.len();
    if ids.len() == expected
        && elf_paths.len() == expected
        && image_paths.len() == expected
        && elf_hashes.len() == expected
        && image_hashes.len() == expected
    {
        Ok(())
    } else {
        Err(format!(
            "closure artifacts are not pairwise distinct: ids={} elf_paths={} image_paths={} elf_hashes={} image_hashes={} expected={expected}",
            ids.len(),
            elf_paths.len(),
            image_paths.len(),
            elf_hashes.len(),
            image_hashes.len()
        ))
    }
}

fn independently_rebuild_and_compare(
    workspace_root: &Path,
    source_archive: &Path,
    profile: &BTreeMap<String, String>,
    journal: JournalParameters,
    source_date_epoch_microseconds: &str,
    qualification_environment: &phase1_tooling::QualificationEnvironment,
    preserved_bundle: &Path,
) -> Result<ReproducibilityRecord, String> {
    let canary = ClosureArtifactId::ElectricalLdoUnboosted;
    let rebuild = TemporaryDirectory::below(&env::temp_dir(), "phase1-rx-closure-repro")?;
    phase1_source::ensure_path_outside_workspace(workspace_root, rebuild.path())?;
    let source_root = rebuild.path().join("source");
    fs::create_dir(&source_root).map_err(|error| {
        format!(
            "could not create closure reproducibility source root {}: {error}",
            source_root.display()
        )
    })?;
    phase1_source::extract_source_archive(source_archive, &source_root)?;
    phase1_source::reject_ambient_ancestor_cargo_configs(&source_root)?;
    let cargo_home = rebuild.path().join("cargo-home");
    fs::create_dir(&cargo_home).map_err(|error| {
        format!(
            "could not create closure reproducibility Cargo home {}: {error}",
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
    let selected = expected_artifact_environment(canary, profile, journal);
    let target = rebuild.path().join("build-target");
    let build = build_spec(canary, &selected, &target, &build_context)?;
    validate_offline_artifact_command(&build)?;
    run_logged(&build, &source_root, &rebuild.path().join("build.log"))?;
    let rebuilt_elf = rebuild.path().join("firmware.elf");
    copy_built_elf(&target, canary, &rebuilt_elf)?;

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

    let preserved_directory = preserved_bundle.join("artifacts").join(canary.slug());
    for (label, preserved, rebuilt) in [
        (
            "ELF",
            preserved_directory.join("firmware.elf"),
            rebuilt_elf.as_path(),
        ),
        (
            "flash image",
            preserved_directory.join("flash-image.bin"),
            rebuilt_image.as_path(),
        ),
    ] {
        if fs::read(&preserved).map_err(|error| {
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
                "independent {} rebuild produced a different {label}; deterministic qualification failed",
                canary.slug()
            ));
        }
    }

    Ok(ReproducibilityRecord {
        canary,
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
            "could not inspect closure artifact bundle {}: {error}",
            bundle.display()
        )
    })?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "closure artifact bundle is not a directory: {}",
            bundle.display()
        ));
    }
    verify_completion_state(&bundle, require_complete)?;
    verify_bundle_tree(&bundle, require_complete)?;

    let manifest_path = bundle.join(MANIFEST_FILE);
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
        format!(
            "could not read closure artifact manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: ClosureManifest = serde_json::from_slice(&manifest_bytes).map_err(|error| {
        format!(
            "could not parse closure artifact manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    validate_manifest_shape(&manifest)?;
    let expected_source_date_epoch =
        phase1_tooling::source_date_epoch_for_commit(root, &manifest.git_commit)?;
    if manifest.build_recipe.source_date_epoch_microseconds != expected_source_date_epoch {
        return Err(
            "closure manifest SOURCE_DATE_EPOCH does not match the Git commit timestamp in microseconds"
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

    for (record, id) in manifest.artifacts.iter().zip(ClosureArtifactId::ALL) {
        verify_artifact_record(&bundle, record, id, &manifest)?;
        let elf = resolve_record_path(&bundle, &record.elf)?;
        let size: ElfSize = phase1_hil::inspect_elf_by_name(&elf, id.inspection_mode())?.into();
        if size != record.size {
            return Err(format!(
                "{} ELF size metrics do not match closure manifest",
                id.slug()
            ));
        }
    }
    ensure_artifacts_distinct(&manifest.artifacts)?;

    let current_espflash = current_tools
        .get("espflash")
        .ok_or_else(|| "current Phase-1 tool inventory does not record espflash".to_owned())?;
    let recorded_espflash = manifest
        .tools
        .get("espflash")
        .ok_or_else(|| "closure manifest does not record espflash".to_owned())?;
    if current_espflash != phase1_tooling::ESPFLASH_VERSION
        || recorded_espflash != phase1_tooling::ESPFLASH_VERSION
    {
        return Err(format!(
            "host-only image verification requires recorded and current {:?}; recorded={recorded_espflash:?}, current={current_espflash:?}",
            phase1_tooling::ESPFLASH_VERSION
        ));
    }
    for artifact in &manifest.artifacts {
        regenerate_and_compare_image(&bundle, artifact, &qualification_environment)?;
    }

    if require_complete {
        println!(
            "ok: verified Phase-1 closure artifact bundle {}",
            bundle.display()
        );
    }
    Ok(())
}

pub(crate) fn verified_bundle_binding(
    root: &Path,
    bundle: &Path,
) -> Result<phase1_hil::VerifiedBundleBinding, String> {
    let bundle = absolute_from(root, bundle);
    let manifest_path = bundle.join(MANIFEST_FILE);
    let manifest_bytes_before = fs::read(&manifest_path).map_err(|error| {
        format!(
            "could not read verified closure manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: ClosureManifest =
        serde_json::from_slice(&manifest_bytes_before).map_err(|error| {
            format!(
                "could not parse verified closure manifest {}: {error}",
                manifest_path.display()
            )
        })?;
    verify_bundle(root, &bundle, true)?;
    let manifest_bytes_after = fs::read(&manifest_path).map_err(|error| {
        format!(
            "could not reread verified closure manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    if manifest_bytes_before != manifest_bytes_after {
        return Err("closure manifest changed while it was being verified".to_owned());
    }
    let artifacts = manifest
        .artifacts
        .into_iter()
        .map(|record| phase1_hil::VerifiedArtifactBinding {
            id: record.id.slug().to_owned(),
            mode: record.inspection_mode,
            elf: phase1_hil::VerifiedFileBinding {
                path: record.elf.path,
                sha256: record.elf.sha256,
                bytes: record.elf.bytes,
            },
            flash_image: phase1_hil::VerifiedFileBinding {
                path: record.flash_image.path,
                sha256: record.flash_image.sha256,
                bytes: record.flash_image.bytes,
            },
        })
        .collect();
    Ok(phase1_hil::VerifiedBundleBinding {
        schema: manifest.schema,
        git_commit: manifest.git_commit,
        git_root_tree: manifest.git_root_tree,
        profile_environment: manifest.profile_environment,
        artifacts,
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
        Err("closure tool-and-source version file does not match manifest".to_owned())
    }
}

fn verify_completion_state(bundle: &Path, require_complete: bool) -> Result<(), String> {
    let incomplete = bundle.join(INCOMPLETE_FILE);
    let complete = bundle.join(COMPLETE_FILE);
    if require_complete {
        if incomplete.exists() {
            return Err(format!(
                "closure artifact bundle retains incomplete marker: {}",
                incomplete.display()
            ));
        }
        let marker = fs::read_to_string(&complete).map_err(|error| {
            format!(
                "closure artifact bundle lacks completion marker {}: {error}",
                complete.display()
            )
        })?;
        if marker != COMPLETE_CONTENT {
            return Err(format!(
                "closure artifact completion marker has unexpected content: {}",
                complete.display()
            ));
        }
    } else if !incomplete.is_file() || complete.exists() {
        return Err(
            "pre-completion closure verification requires exactly one incomplete marker".to_owned(),
        );
    }
    Ok(())
}

fn verify_bundle_tree(bundle: &Path, require_complete: bool) -> Result<(), String> {
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    collect_bundle_tree(bundle, bundle, &mut files, &mut directories)?;

    let mut expected_files = expected_prepared_files();
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
            "closure bundle file set changed: missing={missing:?} unexpected={unexpected:?}"
        ));
    }
    let expected_directories = expected_directories();
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
            "closure bundle directory set changed: missing={missing:?} unexpected={unexpected:?}"
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
            return Err(format!("closure bundle contains symlink {relative:?}"));
        }
        if metadata.file_type().is_dir() {
            directories.insert(relative);
            collect_bundle_tree(bundle, &path, files, directories)?;
        } else if metadata.file_type().is_file() {
            files.insert(relative);
        } else {
            return Err(format!(
                "closure bundle contains non-file, non-directory path {relative:?}"
            ));
        }
    }
    Ok(())
}

fn validate_manifest_shape(manifest: &ClosureManifest) -> Result<(), String> {
    if manifest.schema != SCHEMA {
        return Err(format!(
            "unsupported closure artifact manifest schema {:?}",
            manifest.schema
        ));
    }
    validate_git_commit(&manifest.git_commit)?;
    validate_git_commit(&manifest.git_root_tree)
        .map_err(|error| format!("closure manifest root tree: {error}"))?;
    if !manifest.worktree_clean {
        return Err(
            "closure artifact manifest is not qualification-eligible: worktree_clean=false"
                .to_owned(),
        );
    }
    if manifest.prepared_unix_seconds == 0 {
        return Err("closure artifact manifest has an invalid preparation time".to_owned());
    }
    validate_profile_map(&manifest.profile_environment)?;
    validate_reviewed_journal_parameters(manifest.journal)?;
    if manifest.source_archive.path != "source.tar" {
        return Err("closure source archive path is not exactly source.tar".to_owned());
    }
    validate_tools_map(&manifest.tools)?;
    validate_build_recipe(&manifest.build_recipe)?;
    if manifest.artifacts.len() != ClosureArtifactId::ALL.len() {
        return Err(format!(
            "closure manifest must contain exactly eight artifact records, found {}",
            manifest.artifacts.len()
        ));
    }
    for (record, expected) in manifest.artifacts.iter().zip(ClosureArtifactId::ALL) {
        if record.id != expected {
            return Err(format!(
                "closure artifact order/identity changed: expected {}, found {}",
                expected.slug(),
                record.id.slug()
            ));
        }
        validate_artifact_record_shape(record, expected, manifest)?;
    }
    let canary = &manifest.artifacts[0];
    let reproducibility = &manifest.reproducibility;
    if reproducibility.canary != ClosureArtifactId::ElectricalLdoUnboosted
        || !reproducibility.independent_source_archive_extraction
        || !reproducibility.independent_target_directory
        || !reproducibility.independent_cargo_home
        || !reproducibility.byte_for_byte
        || reproducibility.elf_sha256 != canary.elf.sha256
        || reproducibility.flash_image_sha256 != canary.flash_image.sha256
    {
        return Err(
            "closure manifest does not record the reviewed independent canary byte comparison"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_profile_map(profile: &BTreeMap<String, String>) -> Result<(), String> {
    if profile.len() != PROFILE_ENV.len() {
        return Err("closure manifest profile does not contain exactly eight fields".to_owned());
    }
    for (name, kind) in PROFILE_ENV {
        let value = profile
            .get(*name)
            .ok_or_else(|| format!("closure manifest profile is missing {name}"))?;
        match kind {
            ProfileValueKind::Unsigned => {
                parse_canonical_u64(name, value, true)?;
            }
            ProfileValueKind::Boolean if value != "0" && value != "1" => {
                return Err(format!("closure manifest {name} must be exactly 0 or 1"));
            }
            ProfileValueKind::Boolean => {}
        }
    }
    Ok(())
}

fn validate_tools_map(tools: &BTreeMap<String, String>) -> Result<(), String> {
    phase1_tooling::validate_tool_inventory(tools)
}

fn validate_build_recipe(recipe: &BuildRecipe) -> Result<(), String> {
    if phase1_tooling::validate_source_date_epoch_microseconds(
        &recipe.source_date_epoch_microseconds,
    )
    .is_ok()
        && recipe == &reviewed_build_recipe(&recipe.source_date_epoch_microseconds)
    {
        Ok(())
    } else {
        Err("closure manifest build recipe is not the reviewed recipe".to_owned())
    }
}

fn reviewed_build_recipe(source_date_epoch_microseconds: &str) -> BuildRecipe {
    BuildRecipe {
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
        dedicated_target_directory: true,
        copy_after_each_build: true,
        full_stack_rustflags: FULL_STACK_RUSTFLAGS.to_owned(),
        journal_rustflags: None,
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
        source_date_epoch_microseconds: source_date_epoch_microseconds.to_owned(),
        build_root_remap: phase1_tooling::BUILD_ROOT_REMAP.to_owned(),
        rustup_home_remap: phase1_tooling::RUSTUP_HOME_REMAP.to_owned(),
        build_from_source_archive: true,
        source_parent_outside_workspace: true,
        isolated_cargo_home: true,
    }
}

fn verify_prepared_hashes(bundle: &Path) -> Result<(), String> {
    let manifest_path = bundle.join(PREPARED_HASH_FILE);
    let text = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "could not read closure artifact hash manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let mut records = BTreeMap::new();
    for (line_number, line) in text.lines().enumerate() {
        let (digest, relative) = line.split_once("  ").ok_or_else(|| {
            format!(
                "invalid closure artifact hash line {} in {}",
                line_number + 1,
                manifest_path.display()
            )
        })?;
        validate_sha256(digest)?;
        if relative_path_text(Path::new(relative))? != relative {
            return Err(format!("non-canonical closure artifact path {relative:?}"));
        }
        if records
            .insert(relative.to_owned(), digest.to_owned())
            .is_some()
        {
            return Err(format!("duplicate closure artifact hash for {relative}"));
        }
    }
    let expected = expected_prepared_files();
    let actual = records.keys().cloned().collect::<BTreeSet<_>>();
    if actual != expected {
        return Err("closure artifact hash manifest has a missing or unexpected path".to_owned());
    }
    for (relative, expected_digest) in records {
        let actual_digest = sha256_file(&bundle.join(&relative))?;
        if actual_digest != expected_digest {
            return Err(format!("closure artifact hash mismatch for {relative}"));
        }
    }
    Ok(())
}

fn verify_file_record(bundle: &Path, record: &FileRecord) -> Result<(), String> {
    validate_sha256(&record.sha256)?;
    let path = resolve_record_path(bundle, record)?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("could not stat artifact {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
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
    expected: ClosureArtifactId,
    manifest: &ClosureManifest,
) -> Result<(), String> {
    validate_artifact_record_shape(record, expected, manifest)?;
    verify_file_record(bundle, &record.elf)?;
    verify_file_record(bundle, &record.flash_image)?;
    let directory = bundle.join("artifacts").join(expected.slug());
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
            expected.slug()
        ));
    }
    let bytes = fs::read_to_string(directory.join("flash-image-bytes.txt"))
        .map_err(|error| format!("could not read flash image byte count: {error}"))?;
    if bytes != format!("{}\n", record.flash_image.bytes) {
        return Err(format!(
            "{} flash image byte-count sidecar does not match manifest",
            expected.slug()
        ));
    }
    Ok(())
}

fn validate_artifact_record_shape(
    record: &ArtifactRecord,
    expected: ClosureArtifactId,
    manifest: &ClosureManifest,
) -> Result<(), String> {
    let prefix = format!("artifacts/{}/", expected.slug());
    if record.id != expected
        || record.feature != expected.feature()
        || record.binary != expected.binary()
        || record.inspection_mode != expected.inspection_mode()
        || record.build_environment
            != expected_artifact_environment(
                expected,
                &manifest.profile_environment,
                manifest.journal,
            )
        || record.cargo_arguments != build_arguments(expected)
        || record.elf.path != format!("{prefix}firmware.elf")
        || record.flash_image.path != format!("{prefix}flash-image.bin")
        || record.flash_image_address != 0
    {
        return Err(format!(
            "{} manifest identity/environment/recipe/path/address does not match the reviewed closure layout",
            expected.slug()
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
    let temporary = TemporaryDirectory::below(&env::temp_dir(), "phase1-rx-closure-image")?;
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
    if preserved == regenerated {
        Ok(())
    } else {
        Err(format!(
            "offline regenerated image differs from preserved {} image",
            artifact.id.slug()
        ))
    }
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
            TemporaryDirectory::below(&env::temp_dir(), "phase1-closure-canonical").unwrap();
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

    fn test_tools() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("git".to_owned(), "git version test".to_owned()),
            (
                "host_rust".to_owned(),
                phase1_tooling::HOST_RUST_VERSION.to_owned(),
            ),
            (
                "esp_rust".to_owned(),
                phase1_tooling::ESP_RUST_VERSION.to_owned(),
            ),
            (
                "esp_cargo".to_owned(),
                phase1_tooling::ESP_CARGO_VERSION.to_owned(),
            ),
            (
                "espflash".to_owned(),
                phase1_tooling::ESPFLASH_VERSION.to_owned(),
            ),
            (
                "xtensa_gcc".to_owned(),
                phase1_tooling::XTENSA_GCC_VERSION.to_owned(),
            ),
            (
                "xtensa_size".to_owned(),
                phase1_tooling::XTENSA_SIZE_VERSION.to_owned(),
            ),
            (
                "xtensa_nm".to_owned(),
                phase1_tooling::XTENSA_NM_VERSION.to_owned(),
            ),
            (
                "xtensa_readelf".to_owned(),
                phase1_tooling::XTENSA_READELF_VERSION.to_owned(),
            ),
            (
                "xtensa_objdump".to_owned(),
                phase1_tooling::XTENSA_OBJDUMP_VERSION.to_owned(),
            ),
            (
                "xtensa_strings".to_owned(),
                phase1_tooling::XTENSA_STRINGS_VERSION.to_owned(),
            ),
        ])
    }

    fn dummy_file(path: String, digest: char) -> FileRecord {
        FileRecord {
            path,
            sha256: std::iter::repeat_n(digest, 64).collect(),
            bytes: 1,
        }
    }

    fn dummy_manifest() -> ClosureManifest {
        let profile = test_profile();
        let journal = JournalParameters::REVIEWED;
        let elf_digests = ['0', '1', '2', '3', '4', '5', '6', '7'];
        let image_digests = ['8', '9', 'a', 'b', 'c', 'd', 'e', 'f'];
        let artifacts = ClosureArtifactId::ALL
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                let prefix = format!("artifacts/{}/", id.slug());
                ArtifactRecord {
                    id,
                    feature: id.feature().to_owned(),
                    binary: id.binary().to_owned(),
                    inspection_mode: id.inspection_mode().to_owned(),
                    build_environment: expected_artifact_environment(id, &profile, journal),
                    cargo_arguments: build_arguments(id),
                    elf: dummy_file(format!("{prefix}firmware.elf"), elf_digests[index]),
                    flash_image: dummy_file(
                        format!("{prefix}flash-image.bin"),
                        image_digests[index],
                    ),
                    flash_image_address: 0,
                    size: ElfSize {
                        text: 1,
                        data: 1,
                        bss: 1,
                        total: 3,
                        maximum_stack_frame: u64::from(id.full_stack()),
                    },
                }
            })
            .collect();
        ClosureManifest {
            schema: SCHEMA.to_owned(),
            prepared_unix_seconds: 1,
            git_commit: "0".repeat(40),
            git_root_tree: "1".repeat(40),
            worktree_clean: true,
            tools: test_tools(),
            profile_environment: profile,
            journal,
            source_archive: dummy_file("source.tar".to_owned(), 'f'),
            build_recipe: reviewed_build_recipe("1000000"),
            reproducibility: ReproducibilityRecord {
                canary: ClosureArtifactId::ElectricalLdoUnboosted,
                independent_source_archive_extraction: true,
                independent_target_directory: true,
                independent_cargo_home: true,
                elf_sha256: "0".repeat(64),
                flash_image_sha256: "8".repeat(64),
                byte_for_byte: true,
            },
            artifacts,
        }
    }

    #[test]
    fn parser_requires_all_reviewed_journal_parameters_without_defaults() {
        assert_eq!(
            parse_cli(strings(&[
                "prepare",
                "--output",
                "artifacts/closure/run",
                "--journal-corrupt-slot",
                "0",
                "--journal-corrupt-word",
                "4",
                "--journal-torn-write-ordinal",
                "9",
            ]))
            .unwrap(),
            Cli::Prepare {
                output: PathBuf::from("artifacts/closure/run"),
                journal: JournalParameters::REVIEWED,
            }
        );
        assert_eq!(
            parse_cli(strings(&["verify", "--bundle", "bundle"])).unwrap(),
            Cli::Verify {
                bundle: PathBuf::from("bundle")
            }
        );
        for rejected in [
            strings(&["prepare", "--output", "out"]),
            strings(&[
                "prepare",
                "--output",
                "out",
                "--journal-corrupt-slot",
                "00",
                "--journal-corrupt-word",
                "4",
                "--journal-torn-write-ordinal",
                "9",
            ]),
            strings(&[
                "prepare",
                "--output",
                "out",
                "--journal-corrupt-slot",
                "0",
                "--journal-corrupt-word",
                "3",
                "--journal-torn-write-ordinal",
                "9",
            ]),
            strings(&["verify", "--bundle", "one", "--bundle", "two"]),
            strings(&["flash", "--port", "/dev/ttyUSB0"]),
        ] {
            assert!(parse_cli(rejected).is_err());
        }
    }

    #[test]
    fn all_planned_commands_are_host_only_and_clear_ambient_build_overrides() {
        let temporary =
            TemporaryDirectory::below(&env::temp_dir(), "closure-command-test").unwrap();
        phase1_tooling::create_controlled_tmpdir(temporary.path()).unwrap();
        let espflash_context =
            phase1_image::OfflineEspflashContext::create(temporary.path()).unwrap();
        let qualification_environment =
            phase1_tooling::QualificationEnvironment::capture().unwrap();
        let profile = test_profile();
        let target = temporary.path().join("dedicated-target");
        let cargo_home = temporary.path().join("isolated-cargo-home");
        fs::create_dir(&cargo_home).unwrap();
        let build_context = phase1_tooling::CargoBuildContext::new(
            temporary.path(),
            &cargo_home,
            "1000000",
            &qualification_environment,
        )
        .unwrap();
        for id in ClosureArtifactId::ALL {
            let selected = expected_artifact_environment(id, &profile, JournalParameters::REVIEWED);
            let build = build_spec(id, &selected, &target, &build_context).unwrap();
            validate_offline_artifact_command(&build).unwrap();
            assert_eq!(
                build.env.get("CARGO_HOME"),
                Some(&cargo_home.display().to_string())
            );
            assert_eq!(
                build.env.get("CARGO_TARGET_DIR"),
                Some(&target.display().to_string())
            );
            assert_eq!(build.env["SOURCE_DATE_EPOCH"], "1000000");
            assert!(!build.env.contains_key("GITHUB_TOKEN"));
            assert!(!build.env.keys().any(|name| name.starts_with("ESP_")));
            assert!(!build.env.contains_key("RUSTC_WRAPPER"));
            let encoded = &build.env["CARGO_ENCODED_RUSTFLAGS"];
            assert!(encoded.contains(phase1_tooling::BUILD_ROOT_REMAP));
            assert!(encoded.contains(phase1_tooling::RUSTUP_HOME_REMAP));
            if id.full_stack() {
                assert!(encoded.contains("emit-stack-sizes"));
            } else {
                assert!(!encoded.contains("emit-stack-sizes"));
                for (name, _) in PROFILE_ENV {
                    assert!(!build.env.contains_key(*name));
                }
            }
        }
        let save = save_image_spec(
            Path::new("firmware.elf"),
            Path::new("image.bin"),
            &espflash_context,
            &qualification_environment,
        );
        validate_offline_artifact_command(&save).unwrap();
        for name in ["ESPFLASH_PORT", "ESPFLASH_BAUD", "MONITOR_BAUD"] {
            assert!(!save.env.contains_key(name));
        }
        assert_eq!(save.env["ESPFLASH_SKIP_UPDATE_CHECK"], "true");
        assert!(save.env.contains_key("HOME"));
        assert!(save.env.contains_key("XDG_CONFIG_HOME"));
        assert!(save.env.contains_key("TMPDIR"));

        let hardware = CommandSpec {
            program: "espflash".to_owned(),
            args: strings(&["flash", "firmware.elf"]),
            env: BTreeMap::new(),
        };
        assert!(validate_offline_artifact_command(&hardware).is_err());
    }

    #[test]
    fn manifest_is_exact_and_rejects_cross_mode_relabeling_or_env_tampering() {
        let manifest = dummy_manifest();
        validate_manifest_shape(&manifest).unwrap();
        ensure_artifacts_distinct(&manifest.artifacts).unwrap();

        let mut relabeled = manifest.clone();
        relabeled.artifacts[0].inspection_mode = ClosureArtifactId::ElectricalDcdcBoosted
            .inspection_mode()
            .to_owned();
        assert!(validate_manifest_shape(&relabeled).is_err());

        let mut changed_env = manifest.clone();
        changed_env.artifacts[4].build_environment.insert(
            "RETICULUM_LAB_RX_RETURNED_FAULT_POLICY".to_owned(),
            "repeat-until-quarantine".to_owned(),
        );
        assert!(validate_manifest_shape(&changed_env).is_err());

        let mut recipe = manifest;
        recipe.build_recipe.isolated_cargo_home = false;
        assert!(validate_manifest_shape(&recipe).is_err());

        let mut reproducibility = dummy_manifest();
        reproducibility.reproducibility.byte_for_byte = false;
        assert!(validate_manifest_shape(&reproducibility).is_err());
    }

    #[test]
    fn artifact_distinctness_rejects_hash_path_and_identity_collisions() {
        let manifest = dummy_manifest();
        ensure_artifacts_distinct(&manifest.artifacts).unwrap();
        let mut collision = manifest.artifacts.clone();
        let digest = collision[0].elf.sha256.clone();
        collision[1].elf.sha256 = digest;
        assert!(ensure_artifacts_distinct(&collision).is_err());
        let mut collision = manifest.artifacts.clone();
        let path = collision[0].flash_image.path.clone();
        collision[1].flash_image.path = path;
        assert!(ensure_artifacts_distinct(&collision).is_err());
        let mut collision = manifest.artifacts;
        collision[1].id = collision[0].id;
        assert!(ensure_artifacts_distinct(&collision).is_err());
    }

    #[test]
    fn confined_file_records_detect_byte_and_digest_tampering() {
        let temporary = TemporaryDirectory::below(&env::temp_dir(), "closure-file-test").unwrap();
        let path = temporary.path().join("firmware.elf");
        fs::write(&path, b"original").unwrap();
        let record = file_record(temporary.path(), &path).unwrap();
        verify_file_record(temporary.path(), &record).unwrap();
        fs::write(&path, b"tampered").unwrap();
        assert!(verify_file_record(temporary.path(), &record).is_err());
        for rejected in ["", "/absolute", "../escape", "one/../escape", "./one"] {
            let record = FileRecord {
                path: rejected.to_owned(),
                sha256: "0".repeat(64),
                bytes: 0,
            };
            assert!(resolve_record_path(temporary.path(), &record).is_err());
        }
    }

    fn populate_expected_bundle_files(bundle: &Path, incomplete: bool) {
        for directory in expected_directories() {
            fs::create_dir_all(bundle.join(directory)).unwrap();
        }
        for relative in expected_prepared_files() {
            fs::write(bundle.join(&relative), relative.as_bytes()).unwrap();
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
    fn hash_manifest_and_exact_tree_reject_tampering_and_extras() {
        let temporary = TemporaryDirectory::below(&env::temp_dir(), "closure-tree-test").unwrap();
        populate_expected_bundle_files(temporary.path(), true);
        verify_prepared_hashes(temporary.path()).unwrap();
        verify_bundle_tree(temporary.path(), false).unwrap();

        fs::write(temporary.path().join("artifacts/unexpected"), b"extra").unwrap();
        assert!(verify_bundle_tree(temporary.path(), false).is_err());
        fs::remove_file(temporary.path().join("artifacts/unexpected")).unwrap();
        fs::write(temporary.path().join("source.tar"), b"changed").unwrap();
        assert!(verify_prepared_hashes(temporary.path()).is_err());
    }

    #[test]
    fn completion_markers_are_fail_closed() {
        let temporary = TemporaryDirectory::below(&env::temp_dir(), "closure-marker-test").unwrap();
        fs::write(temporary.path().join(INCOMPLETE_FILE), b"incomplete\n").unwrap();
        verify_completion_state(temporary.path(), false).unwrap();
        assert!(verify_completion_state(temporary.path(), true).is_err());
        fs::write(temporary.path().join(COMPLETE_FILE), COMPLETE_CONTENT).unwrap();
        assert!(verify_completion_state(temporary.path(), false).is_err());
        assert!(verify_completion_state(temporary.path(), true).is_err());
        fs::remove_file(temporary.path().join(INCOMPLETE_FILE)).unwrap();
        verify_completion_state(temporary.path(), true).unwrap();
    }
}
