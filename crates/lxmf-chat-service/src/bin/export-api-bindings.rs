use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const GENERATED_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../clients/appliance/src/generated/api.ts"
);

fn main() -> ExitCode {
    match run(env::args().skip(1)) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: impl Iterator<Item = String>) -> Result<String, String> {
    let arguments = arguments.collect::<Vec<_>>();
    let check = match arguments.as_slice() {
        [] => false,
        [argument] if argument == "--check" => true,
        _ => {
            return Err(
                "usage: cargo run -p reticulum-lxmf-chat-service --bin export-api-bindings -- [--check]"
                    .to_owned(),
            );
        }
    };

    let destination = Path::new(GENERATED_PATH);
    let generated = reticulum_lxmf_chat_service::render_api_bindings();
    if check {
        let committed = fs::read_to_string(destination).map_err(|error| {
            format!(
                "generated bindings are missing at {}: {error}",
                destination.display()
            )
        })?;
        if committed != generated {
            return Err(
                "generated bindings are stale; run `cargo run --locked -p reticulum-lxmf-chat-service --bin export-api-bindings`"
                    .to_owned(),
            );
        }
        return Ok(format!(
            "TypeScript bindings are current: {}",
            destination.display()
        ));
    }

    let parent = destination
        .parent()
        .ok_or_else(|| "generated binding path has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "could not create generated binding directory {}: {error}",
            parent.display()
        )
    })?;
    let temporary = temporary_path(destination);
    fs::write(&temporary, generated).map_err(|error| {
        format!(
            "could not write temporary bindings {}: {error}",
            temporary.display()
        )
    })?;
    fs::rename(&temporary, destination).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!(
            "could not install generated bindings at {}: {error}",
            destination.display()
        )
    })?;
    Ok(format!(
        "generated TypeScript bindings: {}",
        destination.display()
    ))
}

fn temporary_path(destination: &Path) -> PathBuf {
    let mut path = destination.as_os_str().to_owned();
    path.push(".tmp");
    PathBuf::from(path)
}
