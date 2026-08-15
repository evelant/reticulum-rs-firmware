//! Authenticated host connector for the E290 BLE GATT device-API bearer.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(target_os = "macos")]
use reticulum_appliance_runtime::ConnectionMetadata;
use reticulum_appliance_runtime::{
    ConnectFailure, ConnectedSession, ConnectionTransport, Connector,
};
#[cfg(target_os = "macos")]
use reticulum_appliance_sync::DeviceClientSession;
#[cfg(any(target_os = "macos", test))]
use reticulum_device_api_ble::{eui48_from_device_api_id, local_name};
#[cfg(any(target_os = "macos", test))]
use reticulum_device_client::ClientError;
#[cfg(target_os = "macos")]
use reticulum_device_client::{ClientConfig, ClientSessionProfile, DeviceClient};
#[cfg(any(target_os = "macos", test))]
use reticulum_host_ble::BleConfig;
#[cfg(target_os = "macos")]
use reticulum_host_ble::BleTransport;

#[cfg(target_os = "macos")]
use crate::credential::{HostRng, read_credential};

const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(8);
const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_IO_SLICE: Duration = Duration::from_millis(50);

/// Stable target identity, secret location, and bounded BLE timing policy.
#[derive(Clone)]
pub struct BleConnectorConfig {
    expected_eui48: [u8; 6],
    credential: PathBuf,
    peripheral_id: Option<String>,
    scan_timeout: Duration,
    operation_timeout: Duration,
    io_slice: Duration,
}

impl std::fmt::Debug for BleConnectorConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BleConnectorConfig")
            .field("expected_eui48", &hex::encode_upper(self.expected_eui48))
            .field("credential", &"<redacted>")
            .field("peripheral_id", &self.peripheral_id)
            .field("scan_timeout", &self.scan_timeout)
            .field("operation_timeout", &self.operation_timeout)
            .field("io_slice", &self.io_slice)
            .finish()
    }
}

impl BleConnectorConfig {
    /// Construct the default E290 BLE discovery and session policy.
    pub fn new(expected_eui48: [u8; 6], credential: PathBuf) -> Self {
        Self {
            expected_eui48,
            credential,
            peripheral_id: None,
            scan_timeout: DEFAULT_SCAN_TIMEOUT,
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
            io_slice: DEFAULT_IO_SLICE,
        }
    }

    /// Add an exact platform-owned peripheral selector.
    ///
    /// Authentication still checks the credential-bound device ID; the opaque
    /// peripheral identifier only narrows discovery.
    pub fn with_peripheral_id(mut self, peripheral_id: Option<String>) -> Self {
        self.peripheral_id = peripheral_id;
        self
    }

    /// Activated credential path reloaded for every handshake attempt.
    pub fn credential(&self) -> &Path {
        &self.credential
    }

    /// Optional exact platform-owned peripheral selector.
    pub fn peripheral_id(&self) -> Option<&str> {
        self.peripheral_id.as_deref()
    }

    /// EUI-48 selected by the operator and used for profile storage.
    pub const fn expected_eui48(&self) -> [u8; 6] {
        self.expected_eui48
    }

    #[cfg(any(target_os = "macos", test))]
    fn transport_config(&self, device_id: [u8; 16]) -> Result<(BleConfig, String), String> {
        if self
            .peripheral_id
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err("BLE peripheral ID must not be empty".to_owned());
        }
        let expected_local_name = expected_local_name(device_id, self.expected_eui48)?;
        Ok((
            BleConfig {
                peripheral_id: self.peripheral_id.clone(),
                name_suffix: Some(expected_local_name.clone()),
                scan_timeout: self.scan_timeout,
                operation_timeout: self.operation_timeout,
                io_slice: self.io_slice,
            },
            expected_local_name,
        ))
    }
}

/// Authenticated host BLE connector for one credential-bound E290.
pub struct BleConnector {
    config: BleConnectorConfig,
}

impl BleConnector {
    /// Construct a connector from stable BLE selection and credential policy.
    pub const fn new(config: BleConnectorConfig) -> Self {
        Self { config }
    }
}

impl Connector for BleConnector {
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
        connect_platform(&self.config)
    }
}

#[cfg(target_os = "macos")]
fn connect_platform(config: &BleConnectorConfig) -> Result<ConnectedSession, ConnectFailure> {
    let credential = read_credential(&config.credential).map_err(ConnectFailure::permanent)?;
    let expected_device_id = *credential.device_id().as_bytes();
    let (transport_config, expected_local_name) = config
        .transport_config(expected_device_id)
        .map_err(ConnectFailure::permanent)?;
    let (transport, selected) = BleTransport::connect(transport_config)
        .map_err(|error| ConnectFailure::retryable(format!("BLE connection failed: {error}")))?;

    if !selected
        .local_name
        .as_deref()
        .is_some_and(|observed| observed.eq_ignore_ascii_case(&expected_local_name))
    {
        return Err(ConnectFailure::retryable(
            "selected BLE peripheral did not retain the exact credential-derived E290 local name",
        ));
    }

    let handshake_timeout = config
        .operation_timeout
        .saturating_mul(3)
        .max(Duration::from_secs(15));
    let client = DeviceClient::connect_with_profile(
        transport,
        credential,
        &mut HostRng,
        ClientConfig::new(
            handshake_timeout,
            config.operation_timeout,
            512,
            16 * 1024 * 1024,
        ),
        ClientSessionProfile::BleGatt,
    )
    .map_err(classify_client_failure)?;
    let device_label = hex::encode_upper(config.expected_eui48);
    Ok(ConnectedSession::new(
        DeviceClientSession::new(client),
        ConnectionMetadata::new(
            ConnectionTransport::BluetoothLowEnergy,
            selected.id,
            device_label,
        ),
    ))
}

#[cfg(not(target_os = "macos"))]
fn connect_platform(_config: &BleConnectorConfig) -> Result<ConnectedSession, ConnectFailure> {
    Err(ConnectFailure::unavailable(
        ConnectionTransport::BluetoothLowEnergy,
        "the direct BLE appliance connector is currently available only on macOS",
    ))
}

#[cfg(any(target_os = "macos", test))]
fn expected_local_name(device_id: [u8; 16], expected_eui48: [u8; 6]) -> Result<String, String> {
    let eui48 = eui48_from_device_api_id(device_id)
        .ok_or_else(|| "configured credential does not identify an E290 BLE device".to_owned())?;
    if eui48 != expected_eui48 {
        return Err(
            "configured credential does not match the selected E290 device EUI-48".to_owned(),
        );
    }
    let name = local_name(eui48);
    String::from_utf8(name.to_vec())
        .map_err(|_| "shared E290 BLE local-name contract is not ASCII".to_owned())
}

#[cfg(any(target_os = "macos", test))]
fn classify_client_failure(error: ClientError) -> ConnectFailure {
    if matches!(&error, ClientError::Handshake(_)) {
        ConnectFailure::permanent(format!("BLE authentication failed: {error}"))
    } else {
        ConnectFailure::retryable(format!("BLE device session failed: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_device_api_ble::device_api_id;
    use reticulum_device_api_session::ClientHandshakeError;

    const EUI48: [u8; 6] = [0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88];

    #[test]
    fn credential_device_id_selects_one_exact_e290_local_name() {
        let config = BleConnectorConfig::new(EUI48, PathBuf::from("/private/credential.rdpkey"));
        let (transport, expected) = config
            .transport_config(device_api_id(EUI48))
            .expect("E290 device ID should derive a BLE selector");

        assert_eq!(config.expected_eui48(), EUI48);
        assert_eq!(expected, "reticulum-e290-e13e88");
        assert_eq!(transport.name_suffix.as_deref(), Some(expected.as_str()));
        assert_eq!(transport.peripheral_id, None);
        assert_eq!(transport.scan_timeout, DEFAULT_SCAN_TIMEOUT);
        assert_eq!(transport.operation_timeout, DEFAULT_OPERATION_TIMEOUT);
        assert_eq!(transport.io_slice, DEFAULT_IO_SLICE);
    }

    #[test]
    fn foreign_device_namespace_is_rejected_before_discovery() {
        assert_eq!(
            expected_local_name(*b"another-device!!", EUI48),
            Err("configured credential does not identify an E290 BLE device".to_owned())
        );
    }

    #[test]
    fn credential_must_match_the_selected_profile_eui48() {
        let config = BleConnectorConfig::new(EUI48, PathBuf::from("/private/credential.rdpkey"));
        assert_eq!(
            config
                .transport_config(device_api_id([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88]))
                .expect_err("misplaced credential must fail before BLE discovery"),
            "configured credential does not match the selected E290 device EUI-48"
        );
    }

    #[test]
    fn optional_peripheral_selector_is_exact_and_nonempty() {
        let config = BleConnectorConfig::new(EUI48, PathBuf::from("/private/credential.rdpkey"))
            .with_peripheral_id(Some("A1B2C3".to_owned()));
        assert_eq!(config.peripheral_id(), Some("A1B2C3"));
        let invalid = BleConnectorConfig::new(EUI48, PathBuf::from("/private/credential.rdpkey"))
            .with_peripheral_id(Some("  ".to_owned()));
        assert_eq!(
            invalid
                .transport_config(device_api_id(EUI48))
                .expect_err("blank selector must fail before discovery"),
            "BLE peripheral ID must not be empty"
        );
    }

    #[test]
    fn debug_output_redacts_the_credential_path() {
        let config = BleConnectorConfig::new(EUI48, PathBuf::from("/private/super-secret.rdpkey"));
        let debug = format!("{config:?}");

        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("super-secret"));
    }

    #[test]
    fn non_handshake_client_failures_remain_retryable() {
        let failure = classify_client_failure(ClientError::Timeout {
            stage: "server hello",
        });
        assert!(matches!(
            failure,
            ConnectFailure::Retryable(reason)
                if reason == "BLE device session failed: timed out during server hello"
        ));
    }

    #[test]
    fn authentication_failure_requires_operator_action_instead_of_background_retry() {
        let failure = classify_client_failure(ClientError::Handshake(
            ClientHandshakeError::AuthenticationFailed,
        ));
        assert!(matches!(
            failure,
            ConnectFailure::Permanent(reason)
                if reason
                    == "BLE authentication failed: handshake failed: AuthenticationFailed"
        ));
    }
}
