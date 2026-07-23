use std::collections::BTreeSet;
use std::fs::{File, symlink_metadata};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
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
const ESPRESSIF_USB_VID: u16 = 0x303a;
const ESP32_USB_SERIAL_JTAG_PID: u16 = 0x1001;

/// Shared pause gate preventing authenticated and pre-authentication owners
/// from opening the same physical device concurrently.
#[derive(Clone, Debug, Default)]
pub struct SerialConnectionGate {
    inner: Arc<SerialConnectionGateInner>,
}

#[derive(Debug, Default)]
struct SerialConnectionGateInner {
    state: Mutex<SerialConnectionGateState>,
    drained: Condvar,
}

#[derive(Debug, Default)]
struct SerialConnectionGateState {
    paused: bool,
    active_leases: usize,
}

pub(crate) struct SerialConnectionLease {
    inner: Arc<SerialConnectionGateInner>,
}

pub(crate) struct SerialExclusiveConnectionLease {
    inner: Arc<SerialConnectionGateInner>,
}

impl SerialConnectionGate {
    /// Construct an initially open gate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire sole pre-authentication ownership after every ordinary lease has
    /// drained. Dropping the returned lease reopens the gate.
    pub(crate) fn acquire_exclusive(&self) -> SerialExclusiveConnectionLease {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("serial connection gate must not be poisoned");
        while state.paused {
            state = self
                .inner
                .drained
                .wait(state)
                .expect("serial connection gate must not be poisoned");
        }
        state.paused = true;
        while state.active_leases != 0 {
            state = self
                .inner
                .drained
                .wait(state)
                .expect("serial connection gate must not be poisoned");
        }
        SerialExclusiveConnectionLease {
            inner: self.inner.clone(),
        }
    }

    /// Whether a pre-authentication lifecycle currently owns the device.
    pub fn is_paused(&self) -> bool {
        self.inner
            .state
            .lock()
            .expect("serial connection gate must not be poisoned")
            .paused
    }

    fn acquire(&self) -> Result<SerialConnectionLease, String> {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("serial connection gate must not be poisoned");
        if state.paused {
            return Err("device onboarding currently owns the serial connection".to_owned());
        }
        state.active_leases = state
            .active_leases
            .checked_add(1)
            .expect("serial connection lease count cannot overflow");
        Ok(SerialConnectionLease {
            inner: self.inner.clone(),
        })
    }
}

impl Drop for SerialConnectionLease {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("serial connection gate must not be poisoned");
        state.active_leases = state
            .active_leases
            .checked_sub(1)
            .expect("a serial connection lease was released more than once");
        if state.active_leases == 0 {
            self.inner.drained.notify_all();
        }
    }
}

impl Drop for SerialExclusiveConnectionLease {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("serial connection gate must not be poisoned");
        assert!(
            state.paused && state.active_leases == 0,
            "exclusive serial lease invariant was violated"
        );
        state.paused = false;
        self.inner.drained.notify_all();
    }
}

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

/// Enumerate attached ESP32-S3 native USB Serial/JTAG devices by their stable
/// descriptor serial, collapsing operating-system aliases for one interface.
///
/// The generic Espressif VID/PID does not identify a particular board or prove
/// that compatible firmware is running. Callers must retain an exact user
/// selection and confirm the public device protocol after opening it.
pub fn discover_usb_serials() -> Result<Vec<String>, String> {
    let ports = serialport::available_ports()
        .map_err(|error| format!("could not enumerate serial ports: {error}"))?;
    Ok(discover_usb_serials_from(ports))
}

/// Resolve one exact descriptor serial to its current operating-system port.
///
/// An explicit path remains identity checked and cannot bypass the descriptor
/// serial. Callers should retain only the serial across re-enumeration.
pub fn resolve_usb_port(usb_serial: &str, explicit_port: Option<&str>) -> Result<String, String> {
    let usb_serial = normalize_usb_serial(usb_serial)
        .ok_or_else(|| "USB serial must contain exactly twelve hexadecimal digits".to_owned())?;
    let ports = serialport::available_ports()
        .map_err(|error| format!("could not enumerate serial ports: {error}"))?;
    select_matching_port(ports, &usb_serial, explicit_port).map(|port| port.port_name)
}

fn discover_usb_serials_from(ports: Vec<SerialPortInfo>) -> Vec<String> {
    ports
        .into_iter()
        .filter_map(|port| match port.port_type {
            SerialPortType::UsbPort(info)
                if info.vid == ESPRESSIF_USB_VID && info.pid == ESP32_USB_SERIAL_JTAG_PID =>
            {
                info.serial_number.as_deref().and_then(normalize_usb_serial)
            }
            SerialPortType::UsbPort(_)
            | SerialPortType::BluetoothPort
            | SerialPortType::PciPort
            | SerialPortType::Unknown => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Stable USB discovery and activated-credential policy.
#[derive(Clone)]
pub struct SerialConnectorConfig {
    usb_serial: String,
    credential: PathBuf,
    explicit_port: Option<String>,
    io_slice: Duration,
    open_settle: Duration,
    operation_timeout: Duration,
    connection_gate: Option<SerialConnectionGate>,
}

impl std::fmt::Debug for SerialConnectorConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SerialConnectorConfig")
            .field("usb_serial", &self.usb_serial)
            .field("credential", &"<redacted>")
            .field("explicit_port", &self.explicit_port)
            .field("io_slice", &self.io_slice)
            .field("open_settle", &self.open_settle)
            .field("operation_timeout", &self.operation_timeout)
            .field("connection_gate", &self.connection_gate)
            .finish()
    }
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
            connection_gate: None,
        })
    }

    /// Override discovery with one explicit serial path while retaining the
    /// configured descriptor serial as the expected board identity label.
    pub fn with_explicit_port(mut self, port: Option<String>) -> Self {
        self.explicit_port = port;
        self
    }

    /// Share a gate with a pre-authentication onboarding owner.
    pub fn with_connection_gate(mut self, gate: SerialConnectionGate) -> Self {
        self.connection_gate = Some(gate);
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
        let connection_lease = self
            .config
            .connection_gate
            .as_ref()
            .map(SerialConnectionGate::acquire)
            .transpose()?;
        let selected = self.select_port()?;
        let credential = read_credential(&self.config.credential)?;
        let mut port = serialport::new(&selected, BAUD_RATE)
            .timeout(self.config.io_slice)
            .open()
            .map_err(|error| format!("could not open {selected}: {error}"))?;
        port.write_data_terminal_ready(true)
            .map_err(|error| format!("could not assert DTR on {selected}: {error}"))?;
        port.write_request_to_send(false)
            .map_err(|error| format!("could not clear RTS on {selected}: {error}"))?;
        thread::sleep(self.config.open_settle);
        port.clear(ClearBuffer::Input)
            .map_err(|error| format!("could not clear stale input on {selected}: {error}"))?;

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
        .map_err(|error| format!("could not authenticate {selected}: {error}"))?;
        Ok(ConnectedSession {
            session: Box::new(DeviceClientSession::new(client)),
            port_name: selected,
            usb_serial: self.config.usb_serial.clone(),
            connection_lease,
        })
    }
}

impl SerialConnector {
    fn select_port(&self) -> Result<String, String> {
        resolve_usb_port(
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

    if let Some(selected) = macos_callout_alias(&matching) {
        return Ok(selected);
    }
    Err(format!(
        "more than one serial interface reports USB serial {usb_serial}"
    ))
}

#[cfg(target_os = "macos")]
fn macos_callout_alias(matching: &[SerialPortInfo]) -> Option<SerialPortInfo> {
    if matching.len() != 2 {
        return None;
    }
    let callout = matching
        .iter()
        .find(|port| port.port_name.starts_with("/dev/cu."))?;
    let dial_in = matching
        .iter()
        .find(|port| port.port_name.starts_with("/dev/tty."))?;
    (callout.port_name.strip_prefix("/dev/cu.")? == dial_in.port_name.strip_prefix("/dev/tty.")?)
        .then(|| callout.clone())
}

#[cfg(not(target_os = "macos"))]
const fn macos_callout_alias(_matching: &[SerialPortInfo]) -> Option<SerialPortInfo> {
    None
}

fn port_matches_usb_serial(port: &SerialPortInfo, usb_serial: &str) -> bool {
    match &port.port_type {
        SerialPortType::UsbPort(info) => {
            info.vid == ESPRESSIF_USB_VID
                && info.pid == ESP32_USB_SERIAL_JTAG_PID
                && info
                    .serial_number
                    .as_deref()
                    .and_then(normalize_usb_serial)
                    .is_some_and(|serial| serial == usb_serial)
        }
        SerialPortType::BluetoothPort | SerialPortType::PciPort | SerialPortType::Unknown => false,
    }
}

fn read_credential(path: &Path) -> Result<ActivatedCredential, String> {
    let path_metadata = symlink_metadata(path)
        .map_err(|error| format!("could not inspect configured credential: {error}"))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err("configured credential must be a regular non-symlink file".to_owned());
    }
    enforce_owner_only(&path_metadata)?;
    let mut file = File::open(path)
        .map_err(|error| format!("could not open configured credential: {error}"))?;
    verify_open_file_identity(&path_metadata, &file)?;
    ActivatedCredential::read_from(&mut file)
        .map_err(|error| format!("could not load configured credential: {error}"))
}

#[cfg(unix)]
fn enforce_owner_only(metadata: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(
            "configured credential must not grant group or other permissions (use chmod 600)"
                .to_owned(),
        );
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err("configured credential must be owned by the effective user".to_owned());
    }
    if metadata.nlink() != 1 {
        return Err("configured credential must have exactly one hard link".to_owned());
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
        .map_err(|error| format!("could not recheck configured credential: {error}"))?;
    if expected.dev() != opened.dev() || expected.ino() != opened.ino() {
        return Err("configured credential changed while it was being opened".to_owned());
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
    fn connection_gate_is_shared_across_owners() {
        let gate = SerialConnectionGate::new();
        let authenticated_owner = gate.clone();
        assert!(!authenticated_owner.is_paused());
        let exclusive = gate.acquire_exclusive();
        assert!(authenticated_owner.is_paused());
        assert!(authenticated_owner.acquire().is_err());
        drop(exclusive);
        assert!(!gate.is_paused());
        assert!(authenticated_owner.acquire().is_ok());
    }

    #[test]
    fn exclusive_owner_closes_admission_before_waiting_for_sessions_to_drain() {
        use std::sync::mpsc;

        let gate = SerialConnectionGate::new();
        let ordinary = gate.acquire().unwrap();
        let exclusive_gate = gate.clone();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let owner = thread::spawn(move || {
            let exclusive = exclusive_gate.acquire_exclusive();
            acquired_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(exclusive);
        });

        for _ in 0..1_000 {
            if gate.is_paused() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(gate.is_paused());
        assert!(gate.acquire().is_err());
        assert!(matches!(
            acquired_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        drop(ordinary);
        acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("exclusive owner should acquire after the live session drains");
        assert!(gate.is_paused());
        release_tx.send(()).unwrap();
        owner.join().unwrap();
        assert!(!gate.is_paused());
    }

    #[cfg(target_os = "macos")]
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

    #[test]
    fn candidate_discovery_filters_generic_ports_and_collapses_aliases() {
        let mut unrelated_usb = usb_port("/dev/cu.other", "AC:A7:04:E1:40:88");
        let SerialPortType::UsbPort(info) = &mut unrelated_usb.port_type else {
            unreachable!("fixture is USB")
        };
        info.vid = 0x1234;

        let discovered = discover_usb_serials_from(vec![
            usb_port("/dev/tty.usbmodem1101", "AC:A7:04:E1:3E:88"),
            usb_port("/dev/cu.usbmodem1101", "ac-a7-04-e1-3e-88"),
            usb_port("/dev/cu.usbmodem101", "AC:A7:04:E1:3F:88"),
            unrelated_usb,
            SerialPortInfo {
                port_name: "/dev/cu.Bluetooth-Incoming-Port".to_owned(),
                port_type: SerialPortType::BluetoothPort,
            },
        ]);

        assert_eq!(
            discovered,
            vec!["ACA704E13E88".to_owned(), "ACA704E13F88".to_owned()]
        );
    }
}
