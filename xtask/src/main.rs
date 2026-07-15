use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

const HOST_RUST: &str = "rustc 1.97.0";
const ESP_RUST_FINGERPRINT: &str = "1.95.0.0";
const ESPUP_VERSION: &str = "espup 0.17.1";
const ESPFLASH_VERSION: &str = "espflash 4.5.0";

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("doctor") if args.next().is_none() => doctor(),
        Some("build-tracker") if args.next().is_none() => build_tracker(),
        Some("check-rns-vectors") if args.next().is_none() => check_rns_vectors(),
        Some("graph-policy") if args.next().is_none() => graph_policy(),
        _ => {
            eprintln!(
                "usage: cargo run -p xtask -- \
                 <doctor|build-tracker|check-rns-vectors|graph-policy>"
            );
            ExitCode::from(2)
        }
    }
}

fn check_rns_vectors() -> ExitCode {
    let root = workspace_root();
    let python = env::var_os("PYTHON").unwrap_or_else(|| "python3".into());
    let script = root.join("interop/python/generate_rns_vectors.py");

    let vector_status = Command::new(&python)
        .current_dir(&root)
        .arg(script)
        .arg("--check")
        .status();
    match vector_status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!("Python vector check exited with {status}");
            eprintln!(
                "hint: set PYTHON to an environment containing \
                 interop/python/requirements-rns-1.3.8.txt"
            );
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("could not run Python vector generator: {error}");
            return ExitCode::FAILURE;
        }
    }

    run_inherited_at(
        "cargo",
        &["run", "--locked", "-p", "reticulum-conformance-rete"],
        &root,
    )
}

fn doctor() -> ExitCode {
    let checks: &[(&str, &str, &[&str], &str)] = &[
        ("host Rust", "rustc", &["--version"], HOST_RUST),
        (
            "ESP Rust",
            "rustc",
            &["+esp", "--version"],
            ESP_RUST_FINGERPRINT,
        ),
        ("espup", "espup", &["--version"], ESPUP_VERSION),
        ("espflash", "espflash", &["--version"], ESPFLASH_VERSION),
        (
            "Xtensa GCC",
            "xtensa-esp32s3-elf-gcc",
            &["--version"],
            "xtensa-esp-elf",
        ),
    ];

    let mut failed = false;
    for (label, program, args, expected) in checks {
        match capture(program, *args) {
            Ok(output) if output.contains(expected) => {
                println!("ok: {label}: {}", first_line(&output));
            }
            Ok(output) => {
                eprintln!(
                    "error: {label}: expected {expected:?}, got {:?}",
                    first_line(&output)
                );
                failed = true;
            }
            Err(error) => {
                eprintln!("error: {label}: {error}");
                failed = true;
            }
        }
    }

    if failed {
        eprintln!("hint: install the pinned toolchain with espup, then source ~/export-esp.sh");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn build_tracker() -> ExitCode {
    if capture("xtensa-esp32s3-elf-gcc", ["--version"]).is_err() {
        eprintln!("Xtensa GCC is not on PATH; source ~/export-esp.sh first");
        return ExitCode::FAILURE;
    }

    run_inherited(
        "cargo",
        &[
            "+esp",
            "build",
            "--locked",
            "--release",
            "-p",
            "reticulum-heltec-tracker-v2",
            "--target",
            "xtensa-esp32s3-none-elf",
        ],
    )
}

fn graph_policy() -> ExitCode {
    let root = workspace_root();
    let product = match capture(
        "cargo",
        ["tree", "--locked", "-p", "reticulum-heltec-tracker-v2"],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect product graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let comparison = match capture(
        "cargo",
        ["tree", "--locked", "-p", "reticulum-rns-leviculum"],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect comparison graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = false;
    for forbidden in ["leviculum-core", "rete-lxmf", "lxmf-rs"] {
        if product.contains(forbidden) {
            eprintln!("error: product graph contains forbidden {forbidden}");
            failed = true;
        }
    }
    for forbidden in ["rete-core", "rete-stack", "rete-transport", "rete-lxmf"] {
        if comparison.contains(forbidden) {
            eprintln!("error: Leviculum comparison graph contains forbidden {forbidden}");
            failed = true;
        }
    }

    let candidate = reticulum_rns_rete::metadata();
    let resolved = capture_stdout_at(
        "cargo",
        ["metadata", "--locked", "--format-version", "1"],
        &root,
    )
    .and_then(|json| {
        validate_resolved_rete_pin(&json, candidate.source, candidate.revision)
            .map_err(|error| format!("Rete pin/report mismatch: {error}"))
    });
    if let Err(error) = resolved {
        eprintln!("error: {error}");
        failed = true;
    }

    if failed {
        ExitCode::FAILURE
    } else {
        println!(
            "ok: Rete product and Leviculum comparison graphs are isolated; \
             resolved Rete packages match reported source/revision"
        );
        ExitCode::SUCCESS
    }
}

fn validate_resolved_rete_pin(
    metadata_json: &str,
    reported_source: &str,
    reported_revision: &str,
) -> Result<(), String> {
    if reported_revision.len() != 40
        || !reported_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!(
            "reported revision {:?} is not a full Git object ID",
            reported_revision
        ));
    }

    let repository = reported_source
        .strip_suffix(".git")
        .unwrap_or(reported_source);
    let expected_source = format!(
        "git+{repository}.git?rev={revision}#{revision}",
        revision = reported_revision
    );
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;

    for name in ["rete-core", "rete-stack", "rete-transport"] {
        let matching = packages
            .iter()
            .filter(|package| package["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(format!(
                "expected exactly one resolved {name} package, found {}",
                matching.len()
            ));
        }
        let source = matching[0]["source"]
            .as_str()
            .ok_or_else(|| format!("resolved {name} package has no Git source"))?;
        if source != expected_source {
            return Err(format!(
                "resolved {name} source {source:?} does not match report-derived {expected_source:?}"
            ));
        }
    }

    Ok(())
}

fn capture<I, S>(program: &str, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program)
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("could not run {program}: {error}"))?;

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(combined)
    } else {
        Err(format!(
            "{program} exited with {}: {}",
            output.status,
            first_line(&combined)
        ))
    }
}

fn capture_stdout_at<I, S>(program: &str, args: I, current_dir: &Path) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program)
        .current_dir(current_dir)
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("could not run {program}: {error}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{program} exited with {}: {}",
            output.status,
            first_line(&stderr)
        ))
    }
}

fn run_inherited(program: &str, args: &[&str]) -> ExitCode {
    run_inherited_at(program, args, Path::new("."))
}

fn run_inherited_at(program: &str, args: &[&str], current_dir: &Path) -> ExitCode {
    match Command::new(program)
        .current_dir(current_dir)
        .args(args)
        .status()
    {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("{program} exited with {status}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("could not run {program}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must live directly below the workspace root")
        .to_owned()
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_handles_empty_and_multiline_output() {
        assert_eq!(first_line(""), "");
        assert_eq!(first_line("one\ntwo"), "one");
    }

    #[test]
    fn workspace_root_contains_the_workspace_manifest() {
        assert!(workspace_root().join("Cargo.toml").is_file());
    }

    #[test]
    fn resolved_rete_pin_is_tied_to_reported_metadata() {
        let source = "https://github.com/example/rete";
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let expected = "git+https://github.com/example/rete.git?rev=\
                        0123456789abcdef0123456789abcdef01234567#\
                        0123456789abcdef0123456789abcdef01234567";
        let metadata = serde_json::json!({
            "packages": [
                { "name": "rete-core", "source": expected },
                { "name": "rete-stack", "source": expected },
                { "name": "rete-transport", "source": expected },
            ]
        });

        validate_resolved_rete_pin(&metadata.to_string(), source, revision).unwrap();

        let mut mismatched = metadata;
        mismatched["packages"][2]["source"] = serde_json::Value::String(
            "git+https://github.com/example/rete.git?rev=bad#bad".to_owned(),
        );
        assert!(validate_resolved_rete_pin(&mismatched.to_string(), source, revision).is_err());
    }
}
