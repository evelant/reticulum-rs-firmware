//! Minimal persistent USB client for the first LXMF chat alpha.

#![forbid(unsafe_code)]

use std::{
    env,
    fs::File,
    num::NonZeroU32,
    path::PathBuf,
    process::ExitCode,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api as device_api;
use reticulum_device_client::{ActivatedCredential, BasicLxmfSend, ClientConfig, DeviceClient};
use reticulum_lxmf_chat_core::{
    AcceptanceIds, ChatStore, Contact, DestinationHash, EncodedPacketSha256, IdempotencyKey,
    InboundCommitOutcome, InboundMessage, MessageId, OutboxMaterial, PacketEvidence, ReconcileWork,
    SqliteChatStore, SubmissionFailure, SubmissionId, SubmissionState, TimelineDirection,
    UnixTimestampMillis,
};
use serialport::{ClearBuffer, SerialPort};

const BAUD_RATE: u32 = 115_200;
const IO_SLICE: Duration = Duration::from_millis(100);
const OPEN_SETTLE: Duration = Duration::from_millis(250);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Options {
    port: Option<String>,
    credential: Option<PathBuf>,
    database: Option<PathBuf>,
    timeout: Duration,
    command: Command,
}

#[derive(Clone, Debug)]
enum Command {
    Identity,
    ContactAdd {
        destination: DestinationHash,
        name: String,
    },
    Contacts,
    Send {
        destination: DestinationHash,
        title: Vec<u8>,
        content: Vec<u8>,
    },
    Sync,
    Reconcile,
    Timeline {
        destination: DestinationHash,
    },
}

struct HostRng;

impl RngCore for HostRng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0_u8; 4];
        self.fill_bytes(&mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0_u8; 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        self.try_fill_bytes(destination)
            .unwrap_or_else(|error| panic!("operating-system randomness failed: {error}"));
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        getrandom::fill(destination).map_err(|_| {
            NonZeroU32::new(rand_core::Error::CUSTOM_START)
                .expect("rand_core custom error base is nonzero")
                .into()
        })
    }
}

impl CryptoRng for HostRng {}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if is_help_request(&args) {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    match parse(args).and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn is_help_request(args: &[String]) -> bool {
    matches!(args, [flag] if flag == "--help" || flag == "-h")
}

fn run(options: Options) -> Result<(), String> {
    match options.command.clone() {
        Command::Identity => {
            let mut client = connect(&options)?;
            let identity = client.identity_summary().map_err(client_error)?;
            println!(
                "primary={} lxmf_delivery={}",
                hex::encode(identity.primary_destination().0),
                identity
                    .lxmf_delivery_destination()
                    .map_or_else(|| "none".to_owned(), |value| hex::encode(value.0))
            );
        }
        Command::ContactAdd { destination, name } => {
            let mut store = open_store(&options)?;
            let outcome = store
                .upsert_contact(Contact::new(destination, name))
                .map_err(store_error)?;
            println!(
                "contact={} outcome={outcome:?}",
                hex::encode(destination.as_bytes())
            );
        }
        Command::Contacts => {
            let store = open_store(&options)?;
            for contact in store.contacts().map_err(store_error)? {
                println!(
                    "destination={} name={:?}",
                    hex::encode(contact.destination().as_bytes()),
                    contact.display_name()
                );
            }
        }
        Command::Send {
            destination,
            title,
            content,
        } => {
            let timestamp = now_timestamp()?;
            let mut key = [0_u8; 16];
            getrandom::fill(&mut key)
                .map_err(|error| format!("could not generate an idempotency key: {error}"))?;
            let material = OutboxMaterial::new(
                destination,
                timestamp,
                IdempotencyKey::new(key),
                title,
                content,
            );
            let mut store = open_store(&options)?;
            let committed = store
                .commit_outbound(material.clone())
                .map_err(store_error)?;
            let outbox_id = committed.outbox_id();
            let mut client = connect(&options)?;
            let acceptance = submit_material(&mut client, &material)?;
            store
                .record_acceptance(outbox_id, acceptance)
                .map_err(store_error)?;
            refresh_status(&mut store, &mut client, acceptance)?;
            println!(
                "outbox_id={} submission_id={} message_id={} outcome={committed:?}",
                outbox_id.get(),
                acceptance.submission_id().get(),
                hex::encode(acceptance.message_id().as_bytes())
            );
        }
        Command::Sync => {
            let mut store = open_store(&options)?;
            let mut client = connect(&options)?;
            let reconciliation = reconcile(&mut store, &mut client)?;
            let mut inserted = 0_usize;
            let mut duplicates = 0_usize;
            for summary in client.lxmf_list().map_err(client_error)? {
                let message = client.lxmf_read_summary(summary).map_err(client_error)?;
                let view = message.view().map_err(|error| error.to_string())?;
                let timestamp = inbound_timestamp(view.payload().timestamp())?;
                let inbound = InboundMessage::new(
                    MessageId::new(*summary.message_id()),
                    DestinationHash::new(summary.destination().0),
                    DestinationHash::new(summary.source().0),
                    timestamp,
                    view.payload().title().as_bytes().to_vec(),
                    view.payload().content().as_bytes().to_vec(),
                );
                match store.commit_inbound(inbound).map_err(store_error)? {
                    InboundCommitOutcome::Inserted => inserted += 1,
                    InboundCommitOutcome::Duplicate => duplicates += 1,
                }
            }
            println!(
                "reconciled={reconciliation} inbox_inserted={inserted} inbox_duplicates={duplicates}"
            );
        }
        Command::Reconcile => {
            let mut store = open_store(&options)?;
            let mut client = connect(&options)?;
            let completed = reconcile(&mut store, &mut client)?;
            println!("reconciled={completed}");
        }
        Command::Timeline { destination } => {
            let store = open_store(&options)?;
            for entry in store
                .conversation_timeline(destination)
                .map_err(store_error)?
            {
                let direction = match entry.direction() {
                    TimelineDirection::Inbound => "in",
                    TimelineDirection::Outbound => "out",
                };
                println!(
                    "timestamp_ms={} direction={direction} message_id={} outbox_id={} status={:?} title={} content={}",
                    entry.timestamp().get(),
                    entry
                        .message_id()
                        .map_or_else(|| "none".to_owned(), |id| hex::encode(id.as_bytes())),
                    entry
                        .outbox_id()
                        .map_or_else(|| "none".to_owned(), |id| id.get().to_string()),
                    entry.outbox_status(),
                    display_bytes(entry.title()),
                    display_bytes(entry.content())
                );
            }
        }
    }
    Ok(())
}

fn open_store(options: &Options) -> Result<SqliteChatStore, String> {
    let path = options
        .database
        .as_deref()
        .ok_or_else(|| "this command requires --database <path>".to_owned())?;
    SqliteChatStore::open(path).map_err(store_error)
}

fn connect(options: &Options) -> Result<DeviceClient<Box<dyn SerialPort>>, String> {
    let port_name = options
        .port
        .as_deref()
        .ok_or_else(|| "this command requires --port <serial-path>".to_owned())?;
    let credential_path = options
        .credential
        .as_deref()
        .ok_or_else(|| "this command requires --credential <active-state-path>".to_owned())?;
    let mut credential_file = File::open(credential_path).map_err(|error| {
        format!(
            "could not open credential {}: {error}",
            credential_path.display()
        )
    })?;
    let credential = ActivatedCredential::read_from(&mut credential_file).map_err(|error| {
        format!(
            "could not load credential {}: {error}",
            credential_path.display()
        )
    })?;

    let mut port = serialport::new(port_name, BAUD_RATE)
        .timeout(IO_SLICE)
        .open()
        .map_err(|error| format!("could not open {port_name}: {error}"))?;
    port.write_data_terminal_ready(true)
        .map_err(|error| format!("could not assert DTR on {port_name}: {error}"))?;
    port.write_request_to_send(false)
        .map_err(|error| format!("could not clear RTS on {port_name}: {error}"))?;
    thread::sleep(OPEN_SETTLE);
    port.clear(ClearBuffer::Input)
        .map_err(|error| format!("could not clear stale input on {port_name}: {error}"))?;

    DeviceClient::connect(
        port,
        credential,
        &mut HostRng,
        ClientConfig::new(options.timeout, options.timeout, 512, 16 * 1024 * 1024),
    )
    .map_err(client_error)
}

fn reconcile(
    store: &mut SqliteChatStore,
    client: &mut DeviceClient<Box<dyn SerialPort>>,
) -> Result<usize, String> {
    let work = store.reconcile().map_err(store_error)?;
    let count = work.len();
    for item in work {
        match item {
            ReconcileWork::Submit {
                outbox_id,
                material,
            } => {
                let acceptance = submit_material(client, &material)?;
                store
                    .record_acceptance(outbox_id, acceptance)
                    .map_err(store_error)?;
                refresh_status(store, client, acceptance)?;
            }
            ReconcileWork::RefreshStatus { acceptance, .. } => {
                refresh_status(store, client, acceptance)?;
            }
        }
    }
    Ok(count)
}

fn submit_material(
    client: &mut DeviceClient<Box<dyn SerialPort>>,
    material: &OutboxMaterial,
) -> Result<AcceptanceIds, String> {
    let accepted = client
        .lxmf_basic_send(BasicLxmfSend::new(
            device_api::DestinationHash(*material.destination().as_bytes()),
            material.timestamp().get(),
            material.title(),
            material.content(),
            device_api::IdempotencyKey(*material.idempotency_key().as_bytes()),
        ))
        .map_err(client_error)?;
    let submission_id = SubmissionId::new(accepted.id.0)
        .map_err(|error| format!("device returned an invalid submission ID: {error}"))?;
    Ok(AcceptanceIds::new(
        submission_id,
        MessageId::new(*accepted.message_id()),
    ))
}

fn refresh_status(
    store: &mut SqliteChatStore,
    client: &mut DeviceClient<Box<dyn SerialPort>>,
    acceptance: AcceptanceIds,
) -> Result<(), String> {
    let status = client
        .submission_status(device_api::SubmissionId(acceptance.submission_id().get()))
        .map_err(client_error)?;
    let state = map_submission_state(status.state)?;
    store
        .project_submission_status(acceptance.submission_id(), state)
        .map_err(store_error)?;
    Ok(())
}

fn map_submission_state(state: device_api::SubmissionState) -> Result<SubmissionState, String> {
    let evidence = |details: device_api::PreparedPacketDetails| {
        PacketEvidence::new(
            details.packet_len,
            EncodedPacketSha256::new(*details.encoded_packet_sha256.as_bytes()),
        )
        .map_err(|error| error.to_string())
    };
    match state {
        device_api::SubmissionState::Queued => Ok(SubmissionState::Queued),
        device_api::SubmissionState::Preparing => Ok(SubmissionState::Preparing),
        device_api::SubmissionState::AwaitingDelivery(details) => {
            Ok(SubmissionState::AwaitingDelivery(evidence(details)?))
        }
        device_api::SubmissionState::Delivered(details) => {
            Ok(SubmissionState::Delivered(evidence(details)?))
        }
        device_api::SubmissionState::Failed(failure) => {
            let failure = match failure {
                device_api::SubmissionFailure::NoPath => SubmissionFailure::NoPath,
                device_api::SubmissionFailure::DeliveryTimeout => {
                    SubmissionFailure::DeliveryTimeout
                }
                device_api::SubmissionFailure::Rejected => SubmissionFailure::DownstreamRejection,
                device_api::SubmissionFailure::Internal => SubmissionFailure::Internal,
            };
            Ok(SubmissionState::Failed(failure))
        }
        device_api::SubmissionState::Cancelled => Ok(SubmissionState::Cancelled),
    }
}

fn now_timestamp() -> Result<UnixTimestampMillis, String> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("host wall clock is before Unix epoch: {error}"))?
        .as_millis();
    let milliseconds = u64::try_from(milliseconds)
        .map_err(|_| "host wall clock does not fit LXMF timestamp range".to_owned())?;
    UnixTimestampMillis::new(milliseconds).map_err(|error| error.to_string())
}

fn inbound_timestamp(seconds: f64) -> Result<UnixTimestampMillis, String> {
    let milliseconds = seconds * 1_000.0;
    if !milliseconds.is_finite() || milliseconds < 1.0 || milliseconds > u64::MAX as f64 {
        return Err(format!(
            "inbound LXMF timestamp {seconds:?} is outside host range"
        ));
    }
    UnixTimestampMillis::new(milliseconds.round() as u64).map_err(|error| error.to_string())
}

fn display_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => format!("{text:?}"),
        Err(_) => format!("hex:{}", hex::encode(bytes)),
    }
}

fn client_error(error: impl std::fmt::Display) -> String {
    format!("device client failed: {error}")
}

fn store_error(error: impl std::fmt::Display) -> String {
    format!("chat database failed: {error}")
}

fn parse(args: Vec<String>) -> Result<Options, String> {
    let mut port = None;
    let mut credential = None;
    let mut database = None;
    let mut timeout = DEFAULT_TIMEOUT;
    let mut index = 0_usize;
    while index < args.len() && args[index].starts_with("--") {
        let flag = args[index].as_str();
        index += 1;
        let value = args
            .get(index)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--port" => port = Some(value.clone()),
            "--credential" => credential = Some(PathBuf::from(value)),
            "--database" => database = Some(PathBuf::from(value)),
            "--timeout-ms" => {
                let milliseconds = value
                    .parse::<u64>()
                    .map_err(|_| "--timeout-ms must be an unsigned integer".to_owned())?;
                if milliseconds == 0 {
                    return Err("--timeout-ms must be nonzero".to_owned());
                }
                timeout = Duration::from_millis(milliseconds);
            }
            _ => return Err(format!("unknown option {flag}\n{}", usage())),
        }
        index += 1;
    }
    let name = args.get(index).ok_or_else(|| usage().to_owned())?.as_str();
    index += 1;
    let command_args = &args[index..];
    let command = parse_command(name, command_args)?;
    Ok(Options {
        port,
        credential,
        database,
        timeout,
        command,
    })
}

fn parse_command(name: &str, args: &[String]) -> Result<Command, String> {
    match name {
        "identity" if args.is_empty() => Ok(Command::Identity),
        "contacts" if args.is_empty() => Ok(Command::Contacts),
        "sync" if args.is_empty() => Ok(Command::Sync),
        "reconcile" if args.is_empty() => Ok(Command::Reconcile),
        "contact-add" => {
            let fields = parse_named(args, &["--destination", "--name"])?;
            Ok(Command::ContactAdd {
                destination: parse_destination(fields[0])?,
                name: fields[1].to_owned(),
            })
        }
        "send" => {
            let fields = parse_named(args, &["--destination", "--title", "--content"])?;
            Ok(Command::Send {
                destination: parse_destination(fields[0])?,
                title: fields[1].as_bytes().to_vec(),
                content: fields[2].as_bytes().to_vec(),
            })
        }
        "timeline" => {
            let fields = parse_named(args, &["--destination"])?;
            Ok(Command::Timeline {
                destination: parse_destination(fields[0])?,
            })
        }
        _ => Err(format!("invalid command or arguments\n{}", usage())),
    }
}

fn parse_named<'a>(args: &'a [String], names: &[&str]) -> Result<Vec<&'a str>, String> {
    if args.len() != names.len() * 2 {
        return Err(format!("wrong command arguments\n{}", usage()));
    }
    let mut values = Vec::with_capacity(names.len());
    for name in names {
        let position = args
            .iter()
            .position(|candidate| candidate == name)
            .ok_or_else(|| format!("missing {name}"))?;
        let value = args
            .get(position + 1)
            .ok_or_else(|| format!("{name} requires a value"))?;
        if value.starts_with("--") {
            return Err(format!("{name} requires a value"));
        }
        values.push(value.as_str());
    }
    Ok(values)
}

fn parse_destination(value: &str) -> Result<DestinationHash, String> {
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(value, &mut bytes)
        .map_err(|_| "destination must be exactly 32 hexadecimal characters".to_owned())?;
    Ok(DestinationHash::new(bytes))
}

fn usage() -> &'static str {
    "usage: reticulum-lxmf-chat [--port <serial-path>] [--credential <active-key>] [--database <sqlite-path>] [--timeout-ms <u64>] <command>\n\
     commands:\n\
       identity\n\
       contact-add --destination <32-hex> --name <text>\n\
       contacts\n\
       send --destination <32-hex> --title <text> --content <text>\n\
       sync\n\
       reconcile\n\
       timeline --destination <32-hex>"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_parser_preserves_send_material() {
        let options = parse(vec![
            "--database".to_owned(),
            "/tmp/chat.db".to_owned(),
            "send".to_owned(),
            "--content".to_owned(),
            "hello".to_owned(),
            "--destination".to_owned(),
            "11".repeat(16),
            "--title".to_owned(),
            "greeting".to_owned(),
        ])
        .unwrap();
        let Command::Send {
            destination,
            title,
            content,
        } = options.command
        else {
            panic!("expected send command");
        };
        assert_eq!(destination.as_bytes(), &[0x11; 16]);
        assert_eq!(title, b"greeting");
        assert_eq!(content, b"hello");
    }

    #[test]
    fn inbound_timestamp_rounds_to_the_nearest_millisecond() {
        assert_eq!(inbound_timestamp(1.234_6).unwrap().get(), 1_235);
        assert!(inbound_timestamp(f64::NAN).is_err());
        assert!(inbound_timestamp(0.0).is_err());
    }

    #[test]
    fn binary_display_is_unambiguous() {
        assert_eq!(display_bytes(b"hello\n"), "\"hello\\n\"");
        assert_eq!(display_bytes(&[0xff, 0x00]), "hex:ff00");
    }

    #[test]
    fn local_commands_do_not_require_a_port_during_parse() {
        let options = parse(vec![
            "--database".to_owned(),
            "/tmp/chat.db".to_owned(),
            "contacts".to_owned(),
        ])
        .unwrap();
        assert!(matches!(options.command, Command::Contacts));
    }

    #[test]
    fn conventional_help_flags_are_recognized_before_option_parsing() {
        for flag in ["--help", "-h"] {
            assert!(is_help_request(&[flag.to_owned()]));
        }
        assert!(!is_help_request(&[]));
        assert!(!is_help_request(&[
            "--help".to_owned(),
            "identity".to_owned()
        ]));
        assert!(usage().contains("reticulum-lxmf-chat"));
    }

    #[test]
    fn destination_requires_exact_width() {
        assert!(parse_destination(&"ab".repeat(16)).is_ok());
        assert!(parse_destination(&"ab".repeat(15)).is_err());
        assert!(parse_destination(&"zz".repeat(16)).is_err());
    }
}
