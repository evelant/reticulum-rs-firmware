use std::fs::{File, symlink_metadata};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use rand_core::{CryptoRng, RngCore};
use reticulum_device_client::{ActivatedCredential, ClientConfig, DeviceClient};
use reticulum_lxmf_chat_app::DeviceClientSession;
use serialport::{ClearBuffer, SerialPortInfo, SerialPortType};

use crate::runtime::{ConnectedSession, Connector};

const BAUD_RATE: u32 = 115_200;
const DEFAULT_IO_SLICE: Duration = Duration::from_millis(100);
const DEFAULT_OPEN_SETTLE: Duration = Duration::from_millis(250);
const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Normalize a 48-bit USB serial expressed with optional `:` or `-`
/// separators into twelve uppercase hexadecimal characters.
pub fn normalize_usb_serial(value: &str) -> Option<String> {
    let mut normalized = String::with_capacity(12);
    for character in value.chars() {
        if character == ':' || character == '-' {
            continue;
        }
        if !character.is_ascii_hexdigit() || normalized.len() == 12 {
            return None;
        }
        normalized.push(character.to_ascii_uppercase());
    }
    (normalized.len() == 12).then_some(normalized)
}

/// Stable USB discovery and activated-credential policy.
#[derive(Clone, Debug)]
pub struct SerialConnectorConfig {
    usb_serial: String,
    credential: PathBuf,
    explicit_port: Option<String>,
    io_slice: Duration,
    open_settle: Duration,
    operation_timeout: Duration,
}

impl SerialConnectorConfig {
    /// Bind discovery to one exact native-USB descriptor serial.
    pub fn new(usb_serial: &str, credential: PathBuf) -> Result<Self, String> {
        let usb_serial = normalize_usb_serial(usb_serial).ok_or_else(|| {
            "USB serial must contain exactly twelve hexadecimal digits".to_owned()
        })?;
        Ok(Self {
            usb_serial,
            credential,
            explicit_port: None,
            io_slice: DEFAULT_IO_SLICE,
            open_settle: DEFAULT_OPEN_SETTLE,
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
        })
    }

    /// Override discovery with one explicit serial path while retaining the
    /// configured descriptor serial as the expected board identity label.
    pub fn with_explicit_port(mut self, port: Option<String>) -> Self {
        self.explicit_port = port;
        self
    }

    /// Canonical twelve-digit USB serial used for discovery.
    pub fn usb_serial(&self) -> &str {
        &self.usb_serial
    }

    /// Activated credential path reloaded for every handshake attempt.
    pub fn credential(&self) -> &Path {
        &self.credential
    }

    /// Optional operator-selected serial path.
    pub fn explicit_port(&self) -> Option<&str> {
        self.explicit_port.as_deref()
    }
}

pub(crate) struct SerialConnector {
    config: SerialConnectorConfig,
}

impl SerialConnector {
    pub(crate) const fn new(config: SerialConnectorConfig) -> Self {
        Self { config }
    }
}

impl Connector for SerialConnector {
    fn connect(&mut self) -> Result<ConnectedSession, String> {
        let selected = self.select_port()?;
        let credential = read_credential(&self.config.credential)?;
        let mut port = serialport::new(&selected.port_name, BAUD_RATE)
            .timeout(self.config.io_slice)
            .open()
            .map_err(|error| format!("could not open {}: {error}", selected.port_name))?;
        port.write_data_terminal_ready(true)
            .map_err(|error| format!("could not assert DTR on {}: {error}", selected.port_name))?;
        port.write_request_to_send(false)
            .map_err(|error| format!("could not clear RTS on {}: {error}", selected.port_name))?;
        thread::sleep(self.config.open_settle);
        port.clear(ClearBuffer::Input).map_err(|error| {
            format!(
                "could not clear stale input on {}: {error}",
                selected.port_name
            )
        })?;

        let client = DeviceClient::connect(
            port,
            credential,
            &mut HostRng,
            ClientConfig::new(
                self.config.operation_timeout,
                self.config.operation_timeout,
                512,
                16 * 1024 * 1024,
            ),
        )
        .map_err(|error| format!("could not authenticate {}: {error}", selected.port_name))?;
        Ok(ConnectedSession {
            session: Box::new(DeviceClientSession::new(client)),
            port_name: selected.port_name,
            usb_serial: self.config.usb_serial.clone(),
        })
    }
}

impl SerialConnector {
    fn select_port(&self) -> Result<SerialPortInfo, String> {
        let ports = serialport::available_ports()
            .map_err(|error| format!("could not enumerate serial ports: {error}"))?;
        select_matching_port(
            ports,
            &self.config.usb_serial,
            self.config.explicit_port.as_deref(),
        )
    }
}

fn select_matching_port(
    ports: Vec<SerialPortInfo>,
    usb_serial: &str,
    explicit_port: Option<&str>,
) -> Result<SerialPortInfo, String> {
    if let Some(explicit) = explicit_port {
        let selected = ports
            .into_iter()
            .find(|port| port.port_name == explicit)
            .ok_or_else(|| format!("configured serial port {explicit} is not present"))?;
        if port_matches_usb_serial(&selected, usb_serial) {
            return Ok(selected);
        }
        return Err(format!(
            "configured serial port {explicit} does not report USB serial {usb_serial}"
        ));
    }

    let matching: Vec<_> = ports
        .into_iter()
        .filter(|port| port_matches_usb_serial(port, usb_serial))
        .collect();
    if matching.is_empty() {
        return Err(format!("USB serial {usb_serial} is not currently present"));
    }
    if matching.len() == 1 {
        return Ok(matching
            .into_iter()
            .next()
            .expect("one matching port exists"));
    }

    // macOS publishes a callout (`/dev/cu.*`) and dial-in (`/dev/tty.*`)
    // path for the same CDC data interface. They carry identical USB
    // metadata and are not distinct devices; the callout endpoint avoids
    // waiting for carrier detection.
    let mut callouts = matching
        .iter()
        .filter(|port| port.port_name.starts_with("/dev/cu."));
    if let Some(selected) = callouts.next().cloned()
        && callouts.next().is_none()
    {
        return Ok(selected);
    }
    Err(format!(
        "more than one serial interface reports USB serial {usb_serial}"
    ))
}

fn port_matches_usb_serial(port: &SerialPortInfo, usb_serial: &str) -> bool {
    match &port.port_type {
        SerialPortType::UsbPort(info) => info
            .serial_number
            .as_deref()
            .and_then(normalize_usb_serial)
            .is_some_and(|serial| serial == usb_serial),
        SerialPortType::BluetoothPort | SerialPortType::PciPort | SerialPortType::Unknown => false,
    }
}

fn read_credential(path: &Path) -> Result<ActivatedCredential, String> {
    let path_metadata = symlink_metadata(path)
        .map_err(|error| format!("could not inspect credential {}: {error}", path.display()))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(format!(
            "credential {} must be a regular non-symlink file",
            path.display()
        ));
    }
    enforce_owner_only(path, &path_metadata)?;
    let mut file = File::open(path)
        .map_err(|error| format!("could not open credential {}: {error}", path.display()))?;
    verify_open_file_identity(path, &path_metadata, &file)?;
    ActivatedCredential::read_from(&mut file)
        .map_err(|error| format!("could not load credential {}: {error}", path.display()))
}

#[cfg(unix)]
fn enforce_owner_only(path: &Path, metadata: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "credential {} must not grant group or other permissions (use chmod 600)",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_owner_only(_path: &Path, _metadata: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn verify_open_file_identity(
    path: &Path,
    expected: &std::fs::Metadata,
    file: &File,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let opened = file
        .metadata()
        .map_err(|error| format!("could not recheck credential {}: {error}", path.display()))?;
    if expected.dev() != opened.dev() || expected.ino() != opened.ino() {
        return Err(format!(
            "credential {} changed while it was being opened",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_open_file_identity(
    _path: &Path,
    _expected: &std::fs::Metadata,
    _file: &File,
) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::UsbPortInfo;

    fn usb_port(path: &str, serial: &str) -> SerialPortInfo {
        SerialPortInfo {
            port_name: path.to_owned(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid: 0x303a,
                pid: 0x1001,
                serial_number: Some(serial.to_owned()),
                manufacturer: None,
                product: None,
            }),
        }
    }

    #[test]
    fn usb_serial_normalization_is_strict_and_separator_agnostic() {
        assert_eq!(
            normalize_usb_serial("AC:A7:04:E1:3E:88").as_deref(),
            Some("ACA704E13E88")
        );
        assert_eq!(
            normalize_usb_serial("ac-a7-04-e1-3e-88").as_deref(),
            Some("ACA704E13E88")
        );
        assert_eq!(normalize_usb_serial("ACA704E13E88x"), None);
        assert_eq!(normalize_usb_serial("ACA704E13E8"), None);
    }

    #[test]
    fn discovery_collapses_one_macos_callout_and_dial_in_pair() {
        let selected = select_matching_port(
            vec![
                usb_port("/dev/tty.usbmodem1101", "AC:A7:04:E1:3E:88"),
                usb_port("/dev/cu.usbmodem1101", "AC:A7:04:E1:3E:88"),
                usb_port("/dev/cu.usbmodem101", "AC:A7:04:E1:3F:88"),
            ],
            "ACA704E13E88",
            None,
        )
        .expect("one physical board should resolve to its callout endpoint");

        assert_eq!(selected.port_name, "/dev/cu.usbmodem1101");
    }

    #[test]
    fn discovery_rejects_ambiguous_physical_interfaces() {
        let error = select_matching_port(
            vec![
                usb_port("/dev/cu.usbmodem1101", "AC:A7:04:E1:3E:88"),
                usb_port("/dev/cu.usbmodem1103", "AC:A7:04:E1:3E:88"),
            ],
            "ACA704E13E88",
            None,
        )
        .expect_err("two callout endpoints must remain ambiguous");

        assert!(error.contains("more than one serial interface"));
    }

    #[test]
    fn explicit_port_must_report_the_configured_usb_serial() {
        let error = select_matching_port(
            vec![usb_port("/dev/cu.usbmodem101", "AC:A7:04:E1:3F:88")],
            "ACA704E13E88",
            Some("/dev/cu.usbmodem101"),
        )
        .expect_err("an explicit path must not bypass stable board identity");

        assert!(error.contains("does not report USB serial ACA704E13E88"));
    }
}
