//! Executable entrypoint for the USB-connected LXMF appliance client.

#![forbid(unsafe_code)]

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use reticulum_lxmf_chat_service::{
    ApplianceConfig, OnboardingConfig, OnboardingHandle, ProfileRoot, SerialConnectionGate,
    SerialConnector, SerialConnectorConfig, WebConfig, discover_usb_serials,
    serve_web_with_onboarding, start_appliance, start_onboarding,
};

#[derive(Debug)]
struct Options {
    usb_serial: String,
    storage: StorageOptions,
    explicit_port: Option<String>,
    http_port: u16,
}

#[derive(Debug)]
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
    if matches!(args.as_slice(), [flag] if flag == "--discover") {
        return match discover_usb_serials() {
            Ok(serials) => {
                for serial in serials {
                    println!("{serial}");
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        };
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
    let (credential, database, connection_gate, onboarding) = match options.storage {
        StorageOptions::Explicit {
            credential,
            database,
        } => (credential, database, None, None),
        StorageOptions::ProfileRoot(path) => {
            let profile = ProfileRoot::open(path)?.device(&options.usb_serial)?;
            profile.prepare_database()?;
            let gate = SerialConnectionGate::new();
            let onboarding = start_onboarding(
                OnboardingConfig::new(profile.clone(), gate.clone())
                    .with_explicit_port(options.explicit_port.clone()),
            )
            .map_err(|error| error.to_string())?;
            (
                profile.credential_path(),
                profile.database_path(),
                Some(gate),
                Some(onboarding),
            )
        }
    };
    let mut serial = SerialConnectorConfig::new(&options.usb_serial, credential)?
        .with_explicit_port(options.explicit_port);
    if let Some(gate) = connection_gate {
        serial = serial.with_connection_gate(gate);
    }
    let appliance = start_appliance(ApplianceConfig::new(database), SerialConnector::new(serial))
        .map_err(|error| error.to_string())?;
    let web = serve_web_with_onboarding(
        appliance.clone(),
        onboarding.clone(),
        WebConfig::new(options.http_port),
    )
    .await?;
    println!("Reticulum LXMF appliance: {}", web.url());
    println!("Press Ctrl-C to stop.");
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("could not wait for Ctrl-C: {error}"))?;
    // Stop both owners together: the actor dropping its revision sender lets
    // long-lived SSE responses finish, while the web server stops accepting
    // new commands during actor shutdown.
    let onboarding_shutdown = shutdown_onboarding(onboarding);
    let (web_result, appliance_result, onboarding_result) = tokio::join!(
        web.shutdown(),
        appliance.shutdown_and_wait(),
        onboarding_shutdown
    );
    web_result?;
    appliance_result.map_err(|error| error.to_string())?;
    onboarding_result
}

async fn shutdown_onboarding(onboarding: Option<OnboardingHandle>) -> Result<(), String> {
    match onboarding {
        Some(onboarding) => onboarding
            .shutdown_and_wait()
            .await
            .map_err(|error| error.to_string()),
        None => Ok(()),
    }
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut usb_serial = None;
    let mut credential = None;
    let mut database = None;
    let mut profile_root = None;
    let mut explicit_port = None;
    let mut http_port = None;
    let mut index = 0_usize;
    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        let value = args
            .get(index)
            .ok_or_else(|| format!("{flag} requires a value\n{}", usage()))?;
        if value.starts_with("--") {
            return Err(format!("{flag} requires a value\n{}", usage()));
        }
        match flag {
            "--usb-serial" => set_once(&mut usb_serial, value.clone(), flag)?,
            "--credential" => set_once(&mut credential, PathBuf::from(value), flag)?,
            "--database" => set_once(&mut database, PathBuf::from(value), flag)?,
            "--profile-root" => set_once(&mut profile_root, PathBuf::from(value), flag)?,
            "--port" => set_once(&mut explicit_port, value.clone(), flag)?,
            "--http-port" => {
                let parsed = value
                    .parse::<u16>()
                    .map_err(|_| "--http-port must be between 0 and 65535".to_owned())?;
                set_once(&mut http_port, parsed, flag)?;
            }
            _ => return Err(format!("unknown option {flag}\n{}", usage())),
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
    Ok(Options {
        usb_serial: usb_serial.ok_or_else(|| "--usb-serial is required".to_owned())?,
        storage,
        explicit_port,
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
    concat!(
        "usage:\n",
        "  reticulum-lxmf-chat-service --discover\n",
        "  reticulum-lxmf-chat-service \\\n",
        "       --usb-serial <12-hex-or-colon-serial> \\\n",
        "       --profile-root <private-data-directory> \\\n",
        "       [--port <serial-path>] [--http-port <0..65535>]\n",
        "  reticulum-lxmf-chat-service \\\n",
        "       --usb-serial <12-hex-or-colon-serial> \\\n",
        "       --credential <active-state-path> --database <sqlite-path> \\\n",
        "       [--port <serial-path>] [--http-port <0..65535>]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_stable_registration_inputs() {
        assert!(parse(&[]).is_err());
        let options = parse(&[
            "--usb-serial".to_owned(),
            "AC:A7:04:E1:3E:88".to_owned(),
            "--credential".to_owned(),
            "a.key".to_owned(),
            "--database".to_owned(),
            "chat.sqlite3".to_owned(),
            "--http-port".to_owned(),
            "8080".to_owned(),
        ])
        .unwrap();
        assert_eq!(options.http_port, 8080);
        assert!(matches!(
            options.storage,
            StorageOptions::Explicit { credential, database }
                if credential == std::path::Path::new("a.key")
                    && database == std::path::Path::new("chat.sqlite3")
        ));
        assert!(!usage().contains("\n+"));
    }

    #[test]
    fn parser_accepts_managed_profile_root_without_secret_paths() {
        let options = parse(&[
            "--usb-serial".to_owned(),
            "AC:A7:04:E1:3E:88".to_owned(),
            "--profile-root".to_owned(),
            "/private/profile".to_owned(),
        ])
        .unwrap();
        assert!(matches!(
            options.storage,
            StorageOptions::ProfileRoot(path) if path == std::path::Path::new("/private/profile")
        ));
    }

    #[test]
    fn parser_rejects_mixed_or_incomplete_storage_modes() {
        let base = ["--usb-serial".to_owned(), "AC:A7:04:E1:3E:88".to_owned()];
        assert!(parse(&base).is_err());
        assert!(
            parse(&[
                base[0].clone(),
                base[1].clone(),
                "--credential".to_owned(),
                "a.key".to_owned(),
            ])
            .is_err()
        );
        assert!(
            parse(&[
                base[0].clone(),
                base[1].clone(),
                "--profile-root".to_owned(),
                "profiles".to_owned(),
                "--database".to_owned(),
                "chat.sqlite3".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn parser_rejects_duplicate_options_and_missing_values() {
        assert!(
            parse(&[
                "--usb-serial".to_owned(),
                "001122334455".to_owned(),
                "--usb-serial".to_owned(),
                "aabbccddeeff".to_owned(),
                "--profile-root".to_owned(),
                "/private/profile".to_owned(),
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "--usb-serial".to_owned(),
                "--profile-root".to_owned(),
                "/private/profile".to_owned(),
            ])
            .is_err()
        );
    }
}
