//! Executable entrypoint for the USB-connected LXMF appliance client.

#![forbid(unsafe_code)]

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use reticulum_lxmf_chat_service::{
    ApplianceConfig, SerialConnectorConfig, WebConfig, serve_web, start_appliance,
};

#[derive(Debug)]
struct Options {
    usb_serial: String,
    credential: PathBuf,
    database: PathBuf,
    explicit_port: Option<String>,
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
    let serial = SerialConnectorConfig::new(&options.usb_serial, options.credential)?
        .with_explicit_port(options.explicit_port);
    let appliance = start_appliance(ApplianceConfig::new(options.database, serial))
        .map_err(|error| error.to_string())?;
    let web = serve_web(appliance.clone(), WebConfig::new(options.http_port)).await?;
    println!("Reticulum LXMF appliance: {}", web.url());
    println!("Press Ctrl-C to stop.");
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("could not wait for Ctrl-C: {error}"))?;
    // Stop both owners together: the actor dropping its revision sender lets
    // long-lived SSE responses finish, while the web server stops accepting
    // new commands during actor shutdown.
    let (web_result, appliance_result) =
        tokio::join!(web.shutdown(), appliance.shutdown_and_wait());
    web_result?;
    appliance_result.map_err(|error| error.to_string())
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut usb_serial = None;
    let mut credential = None;
    let mut database = None;
    let mut explicit_port = None;
    let mut http_port = 0_u16;
    let mut index = 0_usize;
    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        let value = args
            .get(index)
            .ok_or_else(|| format!("{flag} requires a value\n{}", usage()))?;
        match flag {
            "--usb-serial" => usb_serial = Some(value.clone()),
            "--credential" => credential = Some(PathBuf::from(value)),
            "--database" => database = Some(PathBuf::from(value)),
            "--port" => explicit_port = Some(value.clone()),
            "--http-port" => {
                http_port = value
                    .parse::<u16>()
                    .map_err(|_| "--http-port must be between 0 and 65535".to_owned())?;
            }
            _ => return Err(format!("unknown option {flag}\n{}", usage())),
        }
        index += 1;
    }
    Ok(Options {
        usb_serial: usb_serial.ok_or_else(|| "--usb-serial is required".to_owned())?,
        credential: credential.ok_or_else(|| "--credential is required".to_owned())?,
        database: database.ok_or_else(|| "--database is required".to_owned())?,
        explicit_port,
        http_port,
    })
}

fn usage() -> &'static str {
    concat!(
        "usage: reticulum-lxmf-chat-service \\\n",
        "       --usb-serial <12-hex-or-colon-serial> \\\n",
        "       --credential <active-state-path> \\\n",
        "       --database <sqlite-path> \\\n",
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
        assert_eq!(options.credential, PathBuf::from("a.key"));
        assert!(!usage().contains("\n+"));
    }
}
