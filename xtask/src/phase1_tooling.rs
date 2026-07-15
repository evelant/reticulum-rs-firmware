use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use crate::phase1_source;

pub(crate) const HOST_RUST_VERSION: &str = "rustc 1.97.0 (2d8144b78 2026-07-07)";
pub(crate) const ESP_RUST_VERSION: &str = "rustc 1.95.0-nightly (95e5bda86 2026-04-15) (1.95.0.0)";
pub(crate) const ESP_CARGO_VERSION: &str = "cargo 1.95.0-nightly (f2d3ce0bd 2026-03-21) (1.95.0.0)";
pub(crate) const ESPFLASH_VERSION: &str = "espflash 4.5.0";
pub(crate) const XTENSA_GCC_VERSION: &str =
    "xtensa-esp-elf-gcc (crosstool-NG esp-15.2.0_20250920) 15.2.0";
pub(crate) const XTENSA_SIZE_VERSION: &str = "GNU size (crosstool-NG esp-15.2.0_20250920) 2.45";
pub(crate) const XTENSA_NM_VERSION: &str = "GNU nm (crosstool-NG esp-15.2.0_20250920) 2.45";
pub(crate) const XTENSA_READELF_VERSION: &str =
    "GNU readelf (crosstool-NG esp-15.2.0_20250920) 2.45";
pub(crate) const XTENSA_OBJDUMP_VERSION: &str =
    "GNU objdump (crosstool-NG esp-15.2.0_20250920) 2.45";
pub(crate) const XTENSA_STRINGS_VERSION: &str =
    "GNU strings (crosstool-NG esp-15.2.0_20250920) 2.45";

/// The only values copied from the invoking process into qualification build
/// commands. Everything else is supplied explicitly after `Command::env_clear`.
pub(crate) const AMBIENT_ENVIRONMENT_ALLOWLIST: &[&str] = &["PATH", "RUSTUP_HOME"];
pub(crate) const EXPLICIT_BUILD_ENVIRONMENT_NAMES: &[&str] = &[
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "SOURCE_DATE_EPOCH",
    "TMPDIR",
];
pub(crate) const DETERMINISTIC_ENVIRONMENT_POLICY: &str = "env-clear;ambient=PATH,RUSTUP_HOME;tmpdir=build-root;source-date-epoch=git-commit-seconds-times-1000000;remap=build-root-and-rustup-home-v1";
pub(crate) const BUILD_ROOT_REMAP: &str = "/reticulum-phase1/build";
pub(crate) const RUSTUP_HOME_REMAP: &str = "/reticulum-phase1/rustup";
pub(crate) const SOURCE_DATE_EPOCH_SCALE: u64 = 1_000_000;

pub(crate) const PINNED_TOOL_VERSIONS: &[(&str, &str)] = &[
    ("host_rust", HOST_RUST_VERSION),
    ("esp_rust", ESP_RUST_VERSION),
    ("esp_cargo", ESP_CARGO_VERSION),
    ("espflash", ESPFLASH_VERSION),
    ("xtensa_gcc", XTENSA_GCC_VERSION),
    ("xtensa_size", XTENSA_SIZE_VERSION),
    ("xtensa_nm", XTENSA_NM_VERSION),
    ("xtensa_readelf", XTENSA_READELF_VERSION),
    ("xtensa_objdump", XTENSA_OBJDUMP_VERSION),
    ("xtensa_strings", XTENSA_STRINGS_VERSION),
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QualificationEnvironment {
    path: String,
    rustup_home: PathBuf,
}

pub(crate) struct CargoBuildContext<'a> {
    build_root: &'a Path,
    cargo_home: &'a Path,
    source_date_epoch_microseconds: &'a str,
    qualification_environment: &'a QualificationEnvironment,
}

impl<'a> CargoBuildContext<'a> {
    pub(crate) fn new(
        build_root: &'a Path,
        cargo_home: &'a Path,
        source_date_epoch_microseconds: &'a str,
        qualification_environment: &'a QualificationEnvironment,
    ) -> Result<Self, String> {
        validate_source_date_epoch_microseconds(source_date_epoch_microseconds)?;
        if !build_root.join("tmp").is_dir() {
            return Err(format!(
                "qualification build context lacks controlled TMPDIR under {}",
                build_root.display()
            ));
        }
        if !cargo_home.is_dir() {
            return Err(format!(
                "qualification build context lacks isolated Cargo home {}",
                cargo_home.display()
            ));
        }
        Ok(Self {
            build_root,
            cargo_home,
            source_date_epoch_microseconds,
            qualification_environment,
        })
    }

    pub(crate) fn environment(
        &self,
        selected: &BTreeMap<String, String>,
        target_dir: &Path,
        base_rustflags: &[&str],
    ) -> Result<BTreeMap<String, String>, String> {
        let mut environment = self.qualification_environment.base_environment();
        environment.extend(selected.clone());
        environment.insert(
            "CARGO_TARGET_DIR".to_owned(),
            target_dir.to_string_lossy().into_owned(),
        );
        environment.insert(
            "CARGO_HOME".to_owned(),
            self.cargo_home.to_string_lossy().into_owned(),
        );
        environment.insert(
            "SOURCE_DATE_EPOCH".to_owned(),
            self.source_date_epoch_microseconds.to_owned(),
        );
        environment.insert(
            "TMPDIR".to_owned(),
            self.build_root.join("tmp").to_string_lossy().into_owned(),
        );
        environment.insert(
            "CARGO_ENCODED_RUSTFLAGS".to_owned(),
            encoded_rustflags(
                base_rustflags,
                self.build_root,
                self.qualification_environment.rustup_home(),
            )?,
        );
        validate_cargo_build_environment(&environment)?;
        Ok(environment)
    }
}

impl QualificationEnvironment {
    pub(crate) fn capture() -> Result<Self, String> {
        let ambient = env::vars_os().collect::<BTreeMap<_, _>>();
        Self::from_ambient(&ambient)
    }

    fn from_ambient(
        ambient: &BTreeMap<std::ffi::OsString, std::ffi::OsString>,
    ) -> Result<Self, String> {
        let path = ambient
            .get(std::ffi::OsStr::new("PATH"))
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "qualification preparation requires a non-empty UTF-8 PATH".to_owned())?
            .to_owned();

        let rustup_home = if let Some(value) = ambient.get(std::ffi::OsStr::new("RUSTUP_HOME")) {
            let value = value.to_str().ok_or_else(|| {
                "qualification preparation requires a UTF-8 RUSTUP_HOME".to_owned()
            })?;
            if value.is_empty() {
                return Err("qualification preparation requires a non-empty RUSTUP_HOME".to_owned());
            }
            PathBuf::from(value)
        } else {
            let home = ambient
                .get(std::ffi::OsStr::new("HOME"))
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    "qualification preparation needs RUSTUP_HOME or HOME to locate the pinned ESP toolchain"
                        .to_owned()
                })?;
            Path::new(home).join(".rustup")
        };
        let rustup_home = fs::canonicalize(&rustup_home).map_err(|error| {
            format!(
                "could not canonicalize Rustup home {}: {error}",
                rustup_home.display()
            )
        })?;
        if !rustup_home.is_dir() {
            return Err(format!(
                "Rustup home is not a directory: {}",
                rustup_home.display()
            ));
        }
        Ok(Self { path, rustup_home })
    }

    pub(crate) fn base_environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("PATH".to_owned(), self.path.clone()),
            (
                "RUSTUP_HOME".to_owned(),
                self.rustup_home.to_string_lossy().into_owned(),
            ),
        ])
    }

    pub(crate) fn rustup_home(&self) -> &Path {
        &self.rustup_home
    }
}

pub(crate) fn create_controlled_tmpdir(build_root: &Path) -> Result<PathBuf, String> {
    let tmpdir = build_root.join("tmp");
    fs::create_dir(&tmpdir).map_err(|error| {
        format!(
            "could not create controlled build temporary directory {}: {error}",
            tmpdir.display()
        )
    })?;
    Ok(tmpdir)
}

pub(crate) fn encoded_rustflags(
    base_flags: &[&str],
    build_root: &Path,
    rustup_home: &Path,
) -> Result<String, String> {
    let build_root = fs::canonicalize(build_root).map_err(|error| {
        format!(
            "could not canonicalize qualification build root {}: {error}",
            build_root.display()
        )
    })?;
    let build_root = utf8_remap_source(&build_root, "qualification build root")?;
    let rustup_home = utf8_remap_source(rustup_home, "Rustup home")?;
    let mut flags = base_flags
        .iter()
        .map(|flag| (*flag).to_owned())
        .collect::<Vec<_>>();
    flags.extend([
        "--remap-path-prefix".to_owned(),
        format!("{build_root}={BUILD_ROOT_REMAP}"),
        "--remap-path-prefix".to_owned(),
        format!("{rustup_home}={RUSTUP_HOME_REMAP}"),
    ]);
    Ok(flags.join("\u{1f}"))
}

fn utf8_remap_source<'a>(path: &'a Path, label: &str) -> Result<&'a str, String> {
    let value = path
        .to_str()
        .ok_or_else(|| format!("{label} must be UTF-8 for Rust path remapping"))?;
    if value.contains(['=', '\u{1f}']) {
        return Err(format!(
            "{label} contains a character unsupported by the deterministic Rust path-remap policy"
        ));
    }
    Ok(value)
}

pub(crate) fn source_date_epoch_microseconds(commit_seconds: &str) -> Result<String, String> {
    if commit_seconds.is_empty()
        || (commit_seconds.len() > 1 && commit_seconds.starts_with('0'))
        || !commit_seconds.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(format!(
            "git commit timestamp is not canonical unsigned seconds: {commit_seconds:?}"
        ));
    }
    let seconds = commit_seconds
        .parse::<u64>()
        .map_err(|error| format!("could not parse git commit timestamp: {error}"))?;
    seconds
        .checked_mul(SOURCE_DATE_EPOCH_SCALE)
        .map(|value| value.to_string())
        .ok_or_else(|| "git commit timestamp overflows SOURCE_DATE_EPOCH microseconds".to_owned())
}

pub(crate) fn validate_source_date_epoch_microseconds(value: &str) -> Result<(), String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|error| format!("SOURCE_DATE_EPOCH is not canonical microseconds: {error}"))?;
    if parsed == 0 || parsed % SOURCE_DATE_EPOCH_SCALE != 0 || parsed.to_string() != value {
        return Err(format!(
            "SOURCE_DATE_EPOCH is not positive canonical whole commit seconds expressed in microseconds: {value:?}"
        ));
    }
    Ok(())
}

pub(crate) fn source_date_epoch_for_commit(root: &Path, commit: &str) -> Result<String, String> {
    let timestamp = phase1_source::capture_git_stdout(
        root,
        ["show", "-s", "--format=%ct", commit],
        "read source commit timestamp",
    )?;
    source_date_epoch_microseconds(timestamp.trim())
}

pub(crate) fn validate_cargo_build_environment(
    environment: &BTreeMap<String, String>,
) -> Result<(), String> {
    if let Some(name) = environment
        .keys()
        .find(|name| name.as_str() == "GITHUB_TOKEN" || name.starts_with("ESP_"))
    {
        return Err(format!(
            "forbidden ambient credential/ESP override reached qualification build environment: {name}"
        ));
    }
    for required in AMBIENT_ENVIRONMENT_ALLOWLIST
        .iter()
        .chain(EXPLICIT_BUILD_ENVIRONMENT_NAMES.iter())
    {
        if !environment.contains_key(*required) {
            return Err(format!(
                "qualification build environment is missing required explicit field {required}"
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_tool_inventory(tools: &BTreeMap<String, String>) -> Result<(), String> {
    let expected = std::iter::once("git")
        .chain(PINNED_TOOL_VERSIONS.iter().map(|(label, _)| *label))
        .collect::<BTreeSet<_>>();
    let actual = tools.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if actual != expected || tools.get("git").is_none_or(String::is_empty) {
        return Err("Phase-1 tool inventory is missing, empty or unexpected".to_owned());
    }
    for (label, expected_version) in PINNED_TOOL_VERSIONS {
        let actual_version = tools.get(*label).map(String::as_str);
        if actual_version != Some(*expected_version) {
            return Err(format!(
                "Phase-1 pinned {label} fingerprint changed: expected {expected_version:?}, found {actual_version:?}"
            ));
        }
    }
    Ok(())
}

pub(crate) fn render_tool_inventory(
    commit: &str,
    root_tree: &str,
    tools: &BTreeMap<String, String>,
) -> Result<String, String> {
    validate_tool_inventory(tools)?;
    let mut text = format!(
        "git_commit={commit}\ngit_root_tree={root_tree}\nsource_git_environment_policy={}\nworktree_clean=true\n",
        phase1_source::SOURCE_GIT_ENVIRONMENT_POLICY
    );
    for (name, version) in tools {
        text.push_str(name);
        text.push('=');
        text.push_str(version);
        text.push('\n');
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsString, time::SystemTime};

    fn test_directory(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "phase1-tooling-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn exact_inventory() -> BTreeMap<String, String> {
        std::iter::once(("git".to_owned(), "git version test".to_owned()))
            .chain(
                PINNED_TOOL_VERSIONS
                    .iter()
                    .map(|(label, version)| ((*label).to_owned(), (*version).to_owned())),
            )
            .collect()
    }

    #[test]
    fn tool_inventory_requires_every_exact_full_fingerprint() {
        let inventory = exact_inventory();
        validate_tool_inventory(&inventory).unwrap();

        let mut shortened = inventory.clone();
        shortened.insert("host_rust".to_owned(), "rustc 1.97.0".to_owned());
        assert!(validate_tool_inventory(&shortened).is_err());

        let mut embedded = inventory.clone();
        embedded.insert(
            "host_rust".to_owned(),
            format!("wrapper output: {HOST_RUST_VERSION}"),
        );
        assert!(validate_tool_inventory(&embedded).is_err());

        let mut extra = inventory;
        extra.insert("unexpected".to_owned(), "tool".to_owned());
        assert!(validate_tool_inventory(&extra).is_err());
    }

    #[test]
    fn hostile_ambient_environment_is_reduced_to_the_two_field_allowlist() {
        let temporary = test_directory("hostile-env");
        let rustup = temporary.join("rustup");
        fs::create_dir(&rustup).unwrap();
        let ambient = BTreeMap::from([
            (OsString::from("PATH"), OsString::from("/reviewed/tools")),
            (OsString::from("RUSTUP_HOME"), rustup.as_os_str().to_owned()),
            (OsString::from("TMPDIR"), OsString::from("/hostile/tmp")),
            (OsString::from("GITHUB_TOKEN"), OsString::from("secret")),
            (
                OsString::from("ESP_IDF_PATH"),
                OsString::from("/hostile/idf"),
            ),
            (OsString::from("ESP_HOSTED"), OsString::from("1")),
            (OsString::from("RUSTC_WRAPPER"), OsString::from("evil")),
            (
                OsString::from("CARGO_HOME"),
                OsString::from("/hostile/cargo"),
            ),
        ]);
        let captured = QualificationEnvironment::from_ambient(&ambient).unwrap();
        assert_eq!(
            captured
                .base_environment()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            AMBIENT_ENVIRONMENT_ALLOWLIST
        );
        assert_eq!(captured.base_environment()["PATH"], "/reviewed/tools");
        assert_eq!(captured.rustup_home(), fs::canonicalize(rustup).unwrap());
        fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn deterministic_timestamp_uses_esp_bootloader_microseconds_and_rejects_ambiguity() {
        assert_eq!(
            source_date_epoch_microseconds("1721000000").unwrap(),
            "1721000000000000"
        );
        for invalid in ["", "01", "-1", "1.0", "18446744073710"] {
            assert!(source_date_epoch_microseconds(invalid).is_err());
        }
    }

    #[test]
    fn encoded_flags_remap_nonce_roots_to_stable_virtual_paths() {
        let temporary = test_directory("path-remap");
        let rustup = temporary.join("rustup");
        fs::create_dir(&rustup).unwrap();
        let build = temporary.join("nonce-build");
        fs::create_dir(&build).unwrap();
        let flags = encoded_rustflags(&["-Z", "emit-stack-sizes"], &build, &rustup).unwrap();
        let fields = flags.split('\u{1f}').collect::<Vec<_>>();
        assert_eq!(&fields[..2], &["-Z", "emit-stack-sizes"]);
        assert_eq!(fields[2], "--remap-path-prefix");
        assert!(fields[3].ends_with(&format!("={BUILD_ROOT_REMAP}")));
        assert_eq!(fields[4], "--remap-path-prefix");
        assert!(fields[5].ends_with(&format!("={RUSTUP_HOME_REMAP}")));
        assert!(!flags.contains("nonce-build=/reticulum-phase1/build/nonce-build"));
        fs::remove_dir_all(temporary).unwrap();
    }
}
