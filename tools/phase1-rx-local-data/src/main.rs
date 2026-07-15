use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

use sha2::{Digest, Sha256};

use reticulum_phase1_rx_local_data::generate_embedded;

struct Arguments {
    target_public_key: [u8; 64],
    target_destination_hash: [u8; 16],
    output: PathBuf,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let arguments = parse_arguments(env::args_os().skip(1))?;
    let generated = generate_embedded(
        arguments.target_public_key,
        arguments.target_destination_hash,
    )?;
    write_new(&arguments.output, &generated.corpus_bytes)?;
    println!(
        "wrote {} (packet_len={} packet_sha256={})",
        arguments.output.display(),
        generated.packet.len(),
        hex::encode(Sha256::digest(&generated.packet))
    );
    Ok(())
}

fn parse_arguments(
    arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<Arguments, String> {
    let mut target_public_key = None;
    let mut target_destination_hash = None;
    let mut output = None;
    let mut arguments = arguments;

    while let Some(argument) = arguments.next() {
        let argument = argument
            .to_str()
            .ok_or_else(|| "arguments must be valid UTF-8".to_owned())?;
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value after {argument}"))?;
        match argument {
            "--target-public-key-hex" => {
                target_public_key = Some(parse_hex_array::<64>(&value, argument)?);
            }
            "--target-destination-hash-hex" => {
                target_destination_hash = Some(parse_hex_array::<16>(&value, argument)?);
            }
            "--output" => output = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown argument {argument}")),
        }
    }

    Ok(Arguments {
        target_public_key: target_public_key.ok_or_else(usage)?,
        target_destination_hash: target_destination_hash.ok_or_else(usage)?,
        output: output.ok_or_else(usage)?,
    })
}

fn usage() -> String {
    "usage: reticulum-phase1-rx-local-data \
     --target-public-key-hex <128 hex characters> \
     --target-destination-hash-hex <32 hex characters> \
     --output <new corpus path>"
        .to_owned()
}

fn parse_hex_array<const N: usize>(
    value: &std::ffi::OsStr,
    label: &str,
) -> Result<[u8; N], String> {
    let text = value
        .to_str()
        .ok_or_else(|| format!("{label} must be valid UTF-8"))?;
    let decoded = hex::decode(text).map_err(|error| format!("invalid {label}: {error}"))?;
    decoded
        .try_into()
        .map_err(|_: Vec<u8>| format!("{label} must contain exactly {N} bytes"))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("could not create new {}: {error}", path.display()))?;
    output
        .write_all(bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}
