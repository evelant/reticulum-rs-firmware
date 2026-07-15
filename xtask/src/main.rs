use std::{
    env,
    ffi::OsStr,
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
        Some("graph-policy") if args.next().is_none() => graph_policy(),
        _ => {
            eprintln!("usage: cargo run -p xtask -- <doctor|build-tracker|graph-policy>");
            ExitCode::from(2)
        }
    }
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
    match Command::new(program).args(args).status() {
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
}
