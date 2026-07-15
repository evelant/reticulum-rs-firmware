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

    if failed {
        ExitCode::FAILURE
    } else {
        println!("ok: Rete product and Leviculum comparison graphs are isolated");
        ExitCode::SUCCESS
    }
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
}
