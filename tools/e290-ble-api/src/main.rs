use std::{
    env,
    fs::{File, symlink_metadata},
    io::{self, Read, Write},
    net::SocketAddr,
    num::NonZeroU32,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api_ble::{
    GATT_PROFILE_MAJOR, GATT_PROFILE_MINOR, MAXIMUM_ATT_VALUE_BYTES, RX_UUID, SERVICE_UUID, TX_UUID,
};
use reticulum_device_client::{
    ActivatedCredential, ClientConfig, ClientSessionProfile, DeviceClient,
};
use reticulum_e290_ble_api::{
    BleConfig, BleTransport, BrowserBleTransport, BrowserBridgeConfig, SelectedPeripheral,
    TransportStats,
};
use serde_json::{Value, json};

const EVIDENCE_SCHEMA: &str = "reticulum.e290_ble_qualification.v1";

fn main() -> ExitCode {
    match run() {
        Ok(evidence) => {
            println!(
                "{}",
                serde_json::to_string(&evidence).expect("evidence is serializable")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::to_string(&json!({
                    "schema": EVIDENCE_SCHEMA,
                    "status": "fail",
                    "error": error,
                }))
                .expect("failure evidence is serializable")
            );
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Value, String> {
    let options = Options::parse(env::args().skip(1))?;
    if options.help {
        print_help();
        return Ok(json!({
            "schema": EVIDENCE_SCHEMA,
            "status": "help",
        }));
    }
    let credential_path = options
        .credential
        .as_deref()
        .ok_or_else(|| "--credential <path> is required".to_owned())?;
    let credential = read_credential(credential_path)?;
    let expected_device_id = hex::encode(credential.device_id().as_bytes());

    let started = Instant::now();
    let (transport, selected, host_adapter) = connect_transport(&options)?;
    eprintln!(
        "selected BLE peripheral {} ({})",
        selected.id,
        selected.local_name.as_deref().unwrap_or("unnamed")
    );
    let client_config = ClientConfig::new(
        options
            .operation_timeout
            .saturating_mul(3)
            .max(Duration::from_secs(15)),
        options.operation_timeout,
        512,
        16 * 1024 * 1024,
    );
    let mut client = DeviceClient::connect_with_profile(
        transport,
        credential,
        &mut HostRng,
        client_config,
        ClientSessionProfile::BleGattQualification,
    )
    .map_err(|error| format!("suite-3 BLE authentication failed: {error}"))?;
    let authenticated_ms = elapsed_millis(started);
    let device_id = hex::encode(client.device_id().as_bytes());
    let session_id = hex::encode(client.session_id().as_bytes());
    if device_id != expected_device_id {
        return Err("authenticated client returned an unexpected device ID".to_owned());
    }
    let identity = client
        .identity_summary()
        .map_err(|error| format!("authenticated identity.summary failed: {error}"))?;
    let primary_destination = hex::encode(identity.primary_destination().0);
    let lxmf_delivery_destination = identity
        .lxmf_delivery_destination()
        .map(|destination| hex::encode(destination.0));
    let transport = client.into_transport();
    let stats = transport.stats();
    let evidence = json!({
        "schema": EVIDENCE_SCHEMA,
        "status": "pass",
        "transport": "ble_gatt",
        "host_adapter": host_adapter,
        "profile": {
            "gatt_major": GATT_PROFILE_MAJOR,
            "gatt_minor": GATT_PROFILE_MINOR,
            "session_suite": 3,
            "service_uuid": SERVICE_UUID,
            "rx_uuid": RX_UUID,
            "tx_uuid": TX_UUID,
            "maximum_fragment_bytes": MAXIMUM_ATT_VALUE_BYTES,
            "write_type": "with_response",
            "tx_delivery": "indication",
        },
        "peripheral": {
            "id": selected.id,
            "local_name": selected.local_name,
            "rssi_dbm": selected.rssi,
        },
        "authentication": {
            "device_id": device_id,
            "session_id": session_id,
            "completed_ms": authenticated_ms,
        },
        "identity": {
            "primary_destination": primary_destination,
            "lxmf_delivery_destination": lxmf_delivery_destination,
        },
        "traffic": {
            "tx_fragments": stats.tx_fragments,
            "tx_bytes": stats.tx_bytes,
            "rx_indications": stats.rx_indications,
            "rx_bytes": stats.rx_bytes,
        },
        "elapsed_ms": elapsed_millis(started),
    });
    transport.publish_proof(&evidence)?;
    // The proof is complete once the browser has acknowledged this exact
    // evidence. A later best-effort teardown fault must not make the browser
    // display PASS while the CLI emits contradictory failure evidence.
    if let Err(error) = transport.disconnect() {
        eprintln!("post-proof BLE disconnect warning: {error}");
    }
    Ok(evidence)
}

enum HostTransport {
    Native(BleTransport),
    Browser(BrowserBleTransport),
}

impl HostTransport {
    fn stats(&self) -> TransportStats {
        match self {
            Self::Native(transport) => transport.stats(),
            Self::Browser(transport) => transport.stats(),
        }
    }

    fn publish_proof(&self, evidence: &Value) -> Result<(), String> {
        match self {
            Self::Native(_) => Ok(()),
            Self::Browser(transport) => transport.publish_proof(evidence),
        }
    }

    fn disconnect(self) -> Result<TransportStats, String> {
        match self {
            Self::Native(transport) => transport.disconnect(),
            Self::Browser(transport) => transport.disconnect(),
        }
    }
}

impl Read for HostTransport {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Native(transport) => transport.read(output),
            Self::Browser(transport) => transport.read(output),
        }
    }
}

impl Write for HostTransport {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        match self {
            Self::Native(transport) => transport.write(input),
            Self::Browser(transport) => transport.write(input),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Native(transport) => transport.flush(),
            Self::Browser(transport) => transport.flush(),
        }
    }
}

fn connect_transport(
    options: &Options,
) -> Result<(HostTransport, SelectedPeripheral, &'static str), String> {
    if options.browser {
        if options.peripheral_id.is_some() || options.name_suffix.is_some() {
            return Err(
                "--peripheral-id and --name-suffix apply only to the native adapter; the browser chooser selects the E290"
                    .to_owned(),
            );
        }
        let config = BrowserBridgeConfig {
            bind_address: options.browser_bind,
            connect_timeout: options.browser_connect_timeout,
            operation_timeout: options.operation_timeout,
            io_slice: Duration::from_millis(50),
        };
        let (transport, selected) = BrowserBleTransport::connect(config, |url| {
            eprintln!("open {url} in current Chrome or Edge, then click “Choose E290 and connect”");
        })?;
        return Ok((HostTransport::Browser(transport), selected, "web_bluetooth"));
    }

    let config = BleConfig {
        peripheral_id: options.peripheral_id.clone(),
        name_suffix: options.name_suffix.clone(),
        scan_timeout: options.scan_timeout,
        operation_timeout: options.operation_timeout,
        io_slice: Duration::from_millis(50),
    };
    let (transport, selected) = BleTransport::connect(config).map_err(|error| {
        format!(
            "{error}; if the launching process lacks Bluetooth permission, retry with --browser"
        )
    })?;
    Ok((HostTransport::Native(transport), selected, "corebluetooth"))
}

#[derive(Debug)]
struct Options {
    credential: Option<PathBuf>,
    peripheral_id: Option<String>,
    name_suffix: Option<String>,
    scan_timeout: Duration,
    operation_timeout: Duration,
    browser: bool,
    browser_bind: SocketAddr,
    browser_connect_timeout: Duration,
    help: bool,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self {
            credential: None,
            peripheral_id: None,
            name_suffix: None,
            scan_timeout: Duration::from_secs(8),
            operation_timeout: Duration::from_secs(5),
            browser: false,
            browser_bind: "127.0.0.1:8329"
                .parse()
                .expect("default browser bridge address is valid"),
            browser_connect_timeout: Duration::from_secs(5 * 60),
            help: false,
        };
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--credential" => {
                    options.credential = Some(PathBuf::from(required_value(
                        &mut arguments,
                        "--credential",
                    )?));
                }
                "--peripheral-id" => {
                    options.peripheral_id =
                        Some(required_value(&mut arguments, "--peripheral-id")?);
                }
                "--name-suffix" => {
                    options.name_suffix = Some(required_value(&mut arguments, "--name-suffix")?);
                }
                "--scan-timeout-ms" => {
                    options.scan_timeout =
                        parse_duration(&required_value(&mut arguments, "--scan-timeout-ms")?)?;
                }
                "--operation-timeout-ms" => {
                    options.operation_timeout =
                        parse_duration(&required_value(&mut arguments, "--operation-timeout-ms")?)?;
                }
                "--browser" => options.browser = true,
                "--browser-bind" => {
                    let value = required_value(&mut arguments, "--browser-bind")?;
                    options.browser_bind = value
                        .parse()
                        .map_err(|_| format!("{value:?} is not an IP socket address"))?;
                }
                "--browser-connect-timeout-ms" => {
                    options.browser_connect_timeout = parse_duration(&required_value(
                        &mut arguments,
                        "--browser-connect-timeout-ms",
                    )?)?;
                }
                "-h" | "--help" => options.help = true,
                unknown => return Err(format!("unknown argument {unknown}")),
            }
        }
        Ok(options)
    }
}

fn required_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<String, String> {
    arguments
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{option} requires a value"))
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let millis = value
        .parse::<u64>()
        .map_err(|_| format!("{value:?} is not a millisecond count"))?;
    if millis == 0 {
        return Err("timeouts must be positive".to_owned());
    }
    Ok(Duration::from_millis(millis))
}

fn print_help() {
    eprintln!(
        "usage: reticulum-e290-ble-api --credential PATH [--peripheral-id ID | --name-suffix SUFFIX] [--scan-timeout-ms N] [--operation-timeout-ms N]\n       reticulum-e290-ble-api --browser --credential PATH [--browser-bind 127.0.0.1:PORT] [--browser-connect-timeout-ms N] [--operation-timeout-ms N]"
    );
}

fn read_credential(path: &Path) -> Result<ActivatedCredential, String> {
    let path_metadata = symlink_metadata(path)
        .map_err(|error| format!("could not inspect credential {}: {error}", path.display()))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err("credential must be a regular non-symlink file".to_owned());
    }
    enforce_owner_only(&path_metadata)?;
    let mut file = File::open(path)
        .map_err(|error| format!("could not open credential {}: {error}", path.display()))?;
    verify_open_file_identity(&path_metadata, &file)?;
    ActivatedCredential::read_from(&mut file)
        .map_err(|error| format!("could not load credential {}: {error}", path.display()))
}

#[cfg(unix)]
fn enforce_owner_only(metadata: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(
            "credential must not grant group or other permissions (use chmod 600)".to_owned(),
        );
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err("credential must be owned by the effective user".to_owned());
    }
    if metadata.nlink() != 1 {
        return Err("credential must have exactly one hard link".to_owned());
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_owner_only(_metadata: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn verify_open_file_identity(expected: &std::fs::Metadata, file: &File) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let opened = file
        .metadata()
        .map_err(|error| format!("could not recheck opened credential: {error}"))?;
    if expected.dev() != opened.dev() || expected.ino() != opened.ino() {
        return Err("credential changed while it was being opened".to_owned());
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_open_file_identity(_expected: &std::fs::Metadata, _file: &File) -> Result<(), String> {
    Ok(())
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

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
