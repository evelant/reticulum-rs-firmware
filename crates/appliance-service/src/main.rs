//! Executable entrypoint for the BLE-backed appliance web service.

#![forbid(unsafe_code)]

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use reticulum_appliance_service::{
    ApplianceConfig, BleConnector, BleConnectorConfig, ProfileRoot, WebConfig, normalize_eui48,
    parse_eui48, serve_web, start_appliance,
};

#[derive(Debug, Eq, PartialEq)]
struct Options {
    eui48: String,
    storage: StorageOptions,
    peripheral_id: Option<String>,
    http_port: u16,
}

#[derive(Debug, Eq, PartialEq)]
enum StorageOptions {
    Explicit {
        credential: PathBuf,
        database: PathBuf,
    },
    ProfileRoot(PathBuf),
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
    let (credential, database) = match options.storage {
        StorageOptions::Explicit {
            credential,
            database,
        } => (credential, database),
        StorageOptions::ProfileRoot(path) => {
            let profile = ProfileRoot::open(path)?.device(&options.eui48)?;
            profile.prepare_database()?;
            (profile.credential_path(), profile.database_path())
        }
    };
    let expected_eui48 =
        parse_eui48(&options.eui48).expect("the command-line parser retains one normalized EUI-48");
    let connector = BleConnector::new(
        BleConnectorConfig::new(expected_eui48, credential)
            .with_peripheral_id(options.peripheral_id),
    );
    let appliance = start_appliance(ApplianceConfig::new(database), connector)
        .map_err(|error| error.to_string())?;
    let web = serve_web(appliance.clone(), WebConfig::new(options.http_port)).await?;
    println!("Reticulum appliance: {}", web.url());
    println!("Press Ctrl-C to stop.");
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("could not wait for Ctrl-C: {error}"))?;
    let (web_result, appliance_result) =
        tokio::join!(web.shutdown(), appliance.shutdown_and_wait());
    web_result?;
    appliance_result.map_err(|error| error.to_string())
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut eui48 = None;
    let mut credential = None;
    let mut database = None;
    let mut profile_root = None;
    let mut peripheral_id = None;
    let mut http_port = None;
    let mut index = 0_usize;
    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        if !matches!(
            flag,
            "--eui48"
                | "--credential"
                | "--database"
                | "--profile-root"
                | "--ble-peripheral-id"
                | "--http-port"
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
            "--eui48" => set_once(&mut eui48, value.clone(), flag)?,
            "--credential" => set_once(&mut credential, PathBuf::from(value), flag)?,
            "--database" => set_once(&mut database, PathBuf::from(value), flag)?,
            "--profile-root" => set_once(&mut profile_root, PathBuf::from(value), flag)?,
            "--ble-peripheral-id" => set_once(&mut peripheral_id, value.clone(), flag)?,
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
    let storage = match (profile_root, credential, database) {
        (Some(profile_root), None, None) => StorageOptions::ProfileRoot(profile_root),
        (None, Some(credential), Some(database)) => StorageOptions::Explicit {
            credential,
            database,
        },
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            return Err(
                "--profile-root cannot be combined with --credential or --database".to_owned(),
            );
        }
        (None, _, _) => {
            return Err(
                "either --profile-root or both --credential and --database are required".to_owned(),
            );
        }
    };
    let eui48 = normalize_eui48(
        eui48
            .as_deref()
            .ok_or_else(|| "--eui48 is required".to_owned())?,
    )
    .ok_or_else(|| "--eui48 must contain exactly twelve hexadecimal digits".to_owned())?;
    if peripheral_id
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err("--ble-peripheral-id must not be empty".to_owned());
    }
    Ok(Options {
        eui48,
        storage,
        peripheral_id,
        http_port: http_port.unwrap_or(0),
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("{flag} may be specified only once"));
    }
    Ok(())
}

fn usage() -> &'static str {
    "usage: reticulum-appliance-service \\
       --eui48 <12-hex-board-eui48> \\
       --profile-root <private-directory> \\
       [--ble-peripheral-id <platform-id>] [--http-port <0..65535>]\n\
     or: reticulum-appliance-service \\
       --eui48 <12-hex-board-eui48> \\
       --credential <activated-credential> --database <sqlite-path> \\
       [--ble-peripheral-id <platform-id>] [--http-port <0..65535>]"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_options_are_ble_only_and_bearer_neutral() {
        let parsed = parse(&[
            "--eui48".to_owned(),
            "ac:a7:04:e1:3e:88".to_owned(),
            "--profile-root".to_owned(),
            "/tmp/profiles".to_owned(),
            "--ble-peripheral-id".to_owned(),
            "peripheral-a".to_owned(),
            "--http-port".to_owned(),
            "43123".to_owned(),
        ])
        .unwrap();
        assert_eq!(parsed.eui48, "ACA704E13E88");
        assert_eq!(
            parsed.storage,
            StorageOptions::ProfileRoot(PathBuf::from("/tmp/profiles"))
        );
        assert_eq!(parsed.peripheral_id.as_deref(), Some("peripheral-a"));
        assert_eq!(parsed.http_port, 43123);
    }

    #[test]
    fn explicit_storage_requires_both_paths() {
        let base = ["--eui48".to_owned(), "ACA704E13E88".to_owned()];
        assert!(parse(&base).is_err());
        assert!(
            parse(&[
                base[0].clone(),
                base[1].clone(),
                "--credential".to_owned(),
                "/tmp/key".to_owned(),
            ])
            .is_err()
        );
        assert!(
            parse(&[
                base[0].clone(),
                base[1].clone(),
                "--credential".to_owned(),
                "/tmp/key".to_owned(),
                "--database".to_owned(),
                "/tmp/chat.sqlite3".to_owned(),
            ])
            .is_ok()
        );
    }
}
