//! Executable entrypoint for the PRNS-backed appliance web service.

#![forbid(unsafe_code)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use reticulum_appliance_native::{NativePrnsManagementIdentity, NativePrnsNode, PrnsConnector};
use reticulum_appliance_service::{ApplianceConfig, WebConfig, serve_web, start_appliance};

const PROFILE_DIRECTORY: &str = "profiles";
const CHAT_DATABASE_FILE: &str = "chat.sqlite3";

#[derive(Debug, Eq, PartialEq)]
struct Options {
    state_root: PathBuf,
    management_destination: [u8; 16],
    management_destination_hex: String,
    enroll: bool,
    http_port: u16,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if matches!(args.as_slice(), [flag] if flag == "--help" || flag == "-h") {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    match parse(&args) {
        Ok(options) => match run(options).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(options: Options) -> Result<(), String> {
    let state_root = prepare_state_root(&options.state_root)?;
    let storage_directory = state_root
        .to_str()
        .ok_or_else(|| "--state-root must be valid UTF-8".to_owned())?
        .to_owned();
    let prns = NativePrnsNode::start(storage_directory).map_err(|error| error.to_string())?;

    if options.enroll {
        enroll_exact_management_application(&prns, &options.management_destination_hex).await?;
    }

    let database = profile_database(&state_root, &options.management_destination_hex);
    if let Some(parent) = database.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create appliance profile directory: {error}"))?;
    }
    let connector = PrnsConnector::new(Arc::clone(&prns), Some(options.management_destination));
    let appliance = start_appliance(ApplianceConfig::new(database), connector)
        .map_err(|error| error.to_string())?;
    let web = serve_web(appliance.clone(), WebConfig::new(options.http_port)).await?;
    let identity = prns
        .snapshot()
        .identity_hash
        .unwrap_or_else(|| "unavailable".to_owned());
    println!("Reticulum appliance: {}", web.url());
    println!("Client identity: {identity}");
    println!(
        "Management destination: {}",
        options.management_destination_hex
    );
    println!("Press Ctrl-C to stop.");
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("could not wait for Ctrl-C: {error}"))?;
    let (web_result, appliance_result) =
        tokio::join!(web.shutdown(), appliance.shutdown_and_wait());
    web_result?;
    appliance_result.map_err(|error| error.to_string())?;
    prns.close().map_err(|error| error.to_string())
}

async fn enroll_exact_management_application(
    prns: &NativePrnsNode,
    management_destination: &str,
) -> Result<(), String> {
    let public = prns
        .public_management_identity(management_destination.to_owned())
        .await
        .map_err(|error| format!("could not verify public management application: {error}"))?;
    validate_management_identity(&public, management_destination)?;
    prns.enroll_management(management_destination.to_owned())
        .await
        .map_err(|error| format!("management enrollment failed: {error}"))?;
    let authorized = prns
        .management_identity(management_destination.to_owned())
        .await
        .map_err(|error| format!("authorized management verification failed: {error}"))?;
    validate_management_identity(&authorized, management_destination)
}

fn validate_management_identity(
    identity: &NativePrnsManagementIdentity,
    expected_destination: &str,
) -> Result<(), String> {
    if identity.management_destination != expected_destination {
        return Err("management application returned a different primary destination".to_owned());
    }
    if identity.lxmf_destination.is_none() {
        return Err("management application did not publish an LXMF destination".to_owned());
    }
    Ok(())
}

fn prepare_state_root(path: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(path)
        .map_err(|error| format!("could not create --state-root: {error}"))?;
    let root = path
        .canonicalize()
        .map_err(|error| format!("could not resolve --state-root: {error}"))?;
    if !root.is_dir() {
        return Err("--state-root must name a directory".to_owned());
    }
    Ok(root)
}

fn profile_database(state_root: &Path, management_destination: &str) -> PathBuf {
    state_root
        .join(PROFILE_DIRECTORY)
        .join(management_destination)
        .join(CHAT_DATABASE_FILE)
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut state_root = None;
    let mut management_destination = None;
    let mut enroll = false;
    let mut http_port = None;
    let mut index = 0_usize;
    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        if flag == "--enroll" {
            if enroll {
                return Err("--enroll may be specified only once".to_owned());
            }
            enroll = true;
            continue;
        }
        if !matches!(
            flag,
            "--state-root" | "--management-destination" | "--http-port"
        ) {
            return Err(format!("unknown option {flag}\n{}", usage()));
        }
        let value = args
            .get(index)
            .ok_or_else(|| format!("{flag} requires a value\n{}", usage()))?;
        if value.starts_with("--") {
            return Err(format!("{flag} requires a value\n{}", usage()));
        }
        match flag {
            "--state-root" => set_once(&mut state_root, PathBuf::from(value), flag)?,
            "--management-destination" => {
                set_once(&mut management_destination, parse_destination(value)?, flag)?;
            }
            "--http-port" => {
                let parsed = value
                    .parse::<u16>()
                    .map_err(|_| "--http-port must be between 0 and 65535".to_owned())?;
                set_once(&mut http_port, parsed, flag)?;
            }
            _ => unreachable!("recognized value-taking option"),
        }
        index += 1;
    }
    let state_root = state_root.ok_or_else(|| "--state-root is required".to_owned())?;
    let management_destination =
        management_destination.ok_or_else(|| "--management-destination is required".to_owned())?;
    Ok(Options {
        state_root,
        management_destination,
        management_destination_hex: hex::encode(management_destination),
        enroll,
        http_port: http_port.unwrap_or(0),
    })
}

fn parse_destination(value: &str) -> Result<[u8; 16], String> {
    let mut destination = [0; 16];
    hex::decode_to_slice(value, &mut destination).map_err(|_| {
        "--management-destination must contain exactly 32 hexadecimal digits".to_owned()
    })?;
    Ok(destination)
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("{flag} may be specified only once"));
    }
    Ok(())
}

fn usage() -> &'static str {
    "usage: reticulum-appliance-service \\
       --state-root <private-directory> \\
       --management-destination <32-hex-destination> \\
       [--enroll] [--http-port <0..65535>]"
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESTINATION: &str = "00112233445566778899aabbccddeeff";

    #[test]
    fn one_state_root_selects_an_exact_reticulum_application() {
        let parsed = parse(&[
            "--state-root".to_owned(),
            "/tmp/reticulum-client".to_owned(),
            "--management-destination".to_owned(),
            DESTINATION.to_uppercase(),
            "--enroll".to_owned(),
            "--http-port".to_owned(),
            "43123".to_owned(),
        ])
        .unwrap();
        assert_eq!(parsed.state_root, PathBuf::from("/tmp/reticulum-client"));
        assert_eq!(
            parsed.management_destination,
            parse_destination(DESTINATION).unwrap()
        );
        assert_eq!(parsed.management_destination_hex, DESTINATION);
        assert!(parsed.enroll);
        assert_eq!(parsed.http_port, 43123);
    }

    #[test]
    fn each_management_application_gets_app_private_state_without_a_partition_scheme() {
        assert_eq!(
            profile_database(Path::new("/private/state"), DESTINATION),
            PathBuf::from(format!(
                "/private/state/{PROFILE_DIRECTORY}/{DESTINATION}/{CHAT_DATABASE_FILE}"
            ))
        );
    }

    #[test]
    fn destination_is_exact_and_required() {
        assert!(parse_destination(DESTINATION).is_ok());
        assert!(parse_destination("0011").is_err());
        assert!(
            parse(&[
                "--state-root".to_owned(),
                "/tmp/reticulum-client".to_owned(),
            ])
            .is_err()
        );
    }
}
