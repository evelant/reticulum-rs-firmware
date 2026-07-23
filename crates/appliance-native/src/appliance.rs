//! Offline-capable native ownership of the durable chat runtime.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use reticulum_lxmf_chat_runtime::{
    ApplianceConfig, ApplianceHandle, ConnectFailure, ConnectionTransport, Connector,
    ContactRequest, SendRequest, SendResponse, ServiceError, start_appliance,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex as AsyncMutex;

use crate::ble::{
    BleConnector, BleHub, NativeBleError, NativeBlePlatformCommand, PLATFORM_COMMAND_WAIT,
};
use crate::wifi::WifiConnector;

/// Bearer selected for the native appliance session.
///
/// These variants are an intentional stable vocabulary. The generic
/// [`NativeAppliance::open`] constructor retains explicit unavailable stubs;
/// [`NativeAppliance::open_wifi`] and [`NativeAppliance::open_ble`] select the
/// implemented proof connectors without changing that vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum NativeTransport {
    /// USB serial/JTAG provided to a mobile application by a future adapter.
    UsbSerial,
    /// Direct USB OTG provided by a future iOS/Android adapter.
    UsbOtg,
    /// Bluetooth Low Energy provided by a platform-owned GATT bearer.
    BluetoothLowEnergy,
    /// Wi-Fi provided by the native raw local-network proof bearer.
    Wifi,
}

impl NativeTransport {
    const fn label(self) -> &'static str {
        match self {
            Self::UsbSerial => "USB serial/JTAG",
            Self::UsbOtg => "USB OTG",
            Self::BluetoothLowEnergy => "Bluetooth Low Energy",
            Self::Wifi => "Wi-Fi",
        }
    }

    const fn unavailable_reason(self) -> &'static str {
        match self {
            Self::UsbSerial => {
                "USB serial/JTAG requires a platform connector; only the separate host service implements it today"
            }
            Self::UsbOtg => "USB OTG transport is reserved for a future native platform connector",
            Self::BluetoothLowEnergy => {
                "Bluetooth Low Energy requires NativeAppliance.open_ble and a platform GATT link"
            }
            Self::Wifi => {
                "Wi-Fi requires NativeAppliance.open_wifi, a proof endpoint, and an activated credential"
            }
        }
    }

    fn unavailable_error(self) -> NativeApplianceError {
        NativeApplianceError::TransportUnavailable {
            transport: self,
            reason: self.unavailable_reason().to_owned(),
        }
    }

    const fn runtime_transport(self) -> ConnectionTransport {
        match self {
            Self::UsbSerial => ConnectionTransport::UsbSerial,
            Self::UsbOtg => ConnectionTransport::UsbOtg,
            Self::BluetoothLowEnergy => ConnectionTransport::BluetoothLowEnergy,
            Self::Wifi => ConnectionTransport::Wifi,
        }
    }
}

/// Failure returned through the native bridge.
#[derive(Debug, Eq, PartialEq, uniffi::Error)]
#[allow(missing_docs)]
pub enum NativeApplianceError {
    InvalidArgument {
        reason: String,
    },
    Busy,
    Stopped,
    TransportUnavailable {
        transport: NativeTransport,
        reason: String,
    },
    Storage {
        reason: String,
    },
    Serialization {
        reason: String,
    },
    Internal {
        reason: String,
    },
}

impl fmt::Display for NativeApplianceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument { reason } => write!(formatter, "invalid argument: {reason}"),
            Self::Busy => formatter.write_str("appliance command queue is busy"),
            Self::Stopped => formatter.write_str("native appliance has stopped"),
            Self::TransportUnavailable { transport, reason } => {
                write!(
                    formatter,
                    "{} transport unavailable: {reason}",
                    transport.label()
                )
            }
            Self::Storage { reason } => {
                write!(formatter, "chat storage operation failed: {reason}")
            }
            Self::Serialization { reason } => {
                write!(formatter, "chat JSON serialization failed: {reason}")
            }
            Self::Internal { reason } => write!(formatter, "native appliance failure: {reason}"),
        }
    }
}

impl std::error::Error for NativeApplianceError {}

impl From<ServiceError> for NativeApplianceError {
    fn from(error: ServiceError) -> Self {
        match error {
            ServiceError::Busy => Self::Busy,
            ServiceError::Stopped => Self::Stopped,
            ServiceError::Operation(reason) => Self::Storage { reason },
        }
    }
}

#[derive(Clone, Copy)]
struct UnavailableConnector {
    transport: NativeTransport,
}

impl Connector for UnavailableConnector {
    fn connect(&mut self) -> Result<reticulum_lxmf_chat_runtime::ConnectedSession, ConnectFailure> {
        Err(ConnectFailure::unavailable(
            self.transport.runtime_transport(),
            self.transport.unavailable_reason(),
        ))
    }
}

/// Native owner for one durable LXMF chat database and future device bearer.
///
/// App-facing semantic DTOs remain defined by
/// `reticulum-lxmf-chat-runtime`. Methods exchange canonical JSON so the Expo
/// adapter parses the same generated types used by the HTTP client.
#[derive(uniffi::Object)]
pub struct NativeAppliance {
    handle: Mutex<Option<ApplianceHandle>>,
    close_gate: AsyncMutex<()>,
    transport: NativeTransport,
    connector_configured: bool,
    ble_hub: Option<Arc<BleHub>>,
}

#[uniffi::export(async_runtime = "tokio")]
impl NativeAppliance {
    /// Open one SQLite chat database and start its single-owner actor.
    ///
    /// The selected connector is an explicit unavailable stub. This does not
    /// prevent local contacts, conversations, or durable outbox writes. Use
    /// [`Self::open_wifi`] or [`Self::open_ble`] to configure an implemented
    /// authenticated connector.
    #[uniffi::constructor]
    pub fn open(
        database_path: String,
        transport: NativeTransport,
    ) -> Result<Arc<Self>, NativeApplianceError> {
        if database_path.is_empty() {
            return Err(NativeApplianceError::InvalidArgument {
                reason: "database path must not be empty".to_owned(),
            });
        }
        let handle = start_appliance(
            ApplianceConfig::new(PathBuf::from(database_path)),
            UnavailableConnector { transport },
        )
        .map_err(NativeApplianceError::from)?;
        Ok(Arc::new(Self {
            handle: Mutex::new(Some(handle)),
            close_gate: AsyncMutex::new(()),
            transport,
            connector_configured: false,
            ble_hub: None,
        }))
    }

    /// Open one SQLite chat database with the Wi-Fi raw-TCP proof connector.
    ///
    /// The endpoint must be a literal IP socket address. The current E290 proof
    /// profile listens at `192.168.4.1:29716`. The credential path must be an
    /// absolute path to the 96-byte activated credential previously seeded into
    /// the app sandbox.
    /// This suite authenticates and integrity-protects the device API but does
    /// not add application-layer confidentiality.
    #[uniffi::constructor]
    pub fn open_wifi(
        database_path: String,
        endpoint: String,
        credential_path: String,
    ) -> Result<Arc<Self>, NativeApplianceError> {
        if database_path.is_empty() {
            return Err(NativeApplianceError::InvalidArgument {
                reason: "database path must not be empty".to_owned(),
            });
        }
        let endpoint = endpoint.parse::<SocketAddr>().map_err(|error| {
            NativeApplianceError::InvalidArgument {
                reason: format!("Wi-Fi endpoint must be a literal IP socket address: {error}"),
            }
        })?;
        if endpoint.port() == 0 {
            return Err(NativeApplianceError::InvalidArgument {
                reason: "Wi-Fi endpoint port must not be zero".to_owned(),
            });
        }
        let credential_path = PathBuf::from(credential_path);
        if !credential_path.is_absolute() {
            return Err(NativeApplianceError::InvalidArgument {
                reason: "Wi-Fi credential path must be absolute".to_owned(),
            });
        }
        let handle = start_appliance(
            ApplianceConfig::new(PathBuf::from(database_path)),
            WifiConnector::new(endpoint, credential_path),
        )
        .map_err(NativeApplianceError::from)?;
        Ok(Arc::new(Self {
            handle: Mutex::new(Some(handle)),
            close_gate: AsyncMutex::new(()),
            transport: NativeTransport::Wifi,
            connector_configured: true,
            ble_hub: None,
        }))
    }

    /// Open one SQLite chat database with the platform-owned BLE GATT
    /// connector.
    ///
    /// The credential path must be an absolute path to the activated
    /// credential in the app sandbox. For an initial link, the caller
    /// establishes the shared GATT service, enables TX indications, calls
    /// [`Self::ble_link_connected`], then [`Self::ensure_connected`]. To
    /// replace a link, it first closes and reports the old generation, calls
    /// [`Self::reconnect`] while no replacement is claimable, registers the
    /// replacement, then calls [`Self::ensure_connected`]. TypeScript, Swift,
    /// or Kotlin only move opaque bytes and report write-with-response
    /// completion; Rust retains all authenticated session and LXMF parsing.
    #[uniffi::constructor]
    pub fn open_ble(
        database_path: String,
        credential_path: String,
    ) -> Result<Arc<Self>, NativeApplianceError> {
        if database_path.is_empty() {
            return Err(NativeApplianceError::InvalidArgument {
                reason: "database path must not be empty".to_owned(),
            });
        }
        let credential_path = PathBuf::from(credential_path);
        if !credential_path.is_absolute() {
            return Err(NativeApplianceError::InvalidArgument {
                reason: "BLE credential path must be absolute".to_owned(),
            });
        }
        let ble_hub = BleHub::new();
        let handle = start_appliance(
            ApplianceConfig::new(PathBuf::from(database_path)),
            BleConnector::new(credential_path, Arc::clone(&ble_hub)),
        )
        .map_err(NativeApplianceError::from)?;
        Ok(Arc::new(Self {
            handle: Mutex::new(Some(handle)),
            close_gate: AsyncMutex::new(()),
            transport: NativeTransport::BluetoothLowEnergy,
            connector_configured: true,
            ble_hub: Some(ble_hub),
        }))
    }

    /// Register one connected, subscribed GATT peripheral.
    ///
    /// A prior generation must first be reported through
    /// [`Self::ble_disconnected`]; this prevents silently orphaning its GATT
    /// ownership. `max_write_bytes` must be the platform-reported
    /// write-with-response value bound, not the negotiated ATT MTU. The bridge
    /// validates that the platform supports the GATT 1.0 20-byte value, then
    /// caps every emitted fragment to that fixed profile bound.
    pub fn ble_link_connected(
        &self,
        peripheral_id: String,
        max_write_bytes: u32,
    ) -> Result<u64, NativeBleError> {
        self.ble_hub()?
            .link_connected(peripheral_id, max_write_bytes)
    }

    /// Append bytes from one confirmed TX characteristic indication.
    pub fn ble_ingest_indication(
        &self,
        generation: u64,
        bytes: Vec<u8>,
    ) -> Result<(), NativeBleError> {
        self.ble_hub()?.ingest_indication(generation, bytes)
    }

    /// Await the next write-with-response or disconnect command.
    ///
    /// `None` is a normal long-poll timeout. A returned write is delivered only
    /// once; the caller must report its result through
    /// [`Self::ble_write_succeeded`] or [`Self::ble_write_failed`].
    pub async fn ble_next_platform_command(
        &self,
        generation: u64,
    ) -> Result<Option<NativeBlePlatformCommand>, NativeBleError> {
        let hub = self.ble_hub()?;
        tokio::task::spawn_blocking(move || {
            hub.next_platform_command(generation, PLATFORM_COMMAND_WAIT)
        })
        .await
        .map_err(|error| NativeBleError::Internal {
            reason: format!("BLE command waiter failed: {error}"),
        })?
    }

    /// Confirm that one GATT write-with-response completed successfully.
    pub fn ble_write_succeeded(&self, generation: u64, token: u64) -> Result<(), NativeBleError> {
        self.ble_hub()?.write_succeeded(generation, token)
    }

    /// Report that one GATT write-with-response failed.
    ///
    /// Failure closes the generation and schedules an explicit platform
    /// disconnect command.
    pub fn ble_write_failed(
        &self,
        generation: u64,
        token: u64,
        reason: String,
    ) -> Result<(), NativeBleError> {
        self.ble_hub()?.write_failed(generation, token, reason)
    }

    /// Report that the platform GATT link and subscriptions are gone.
    pub fn ble_disconnected(&self, generation: u64, reason: String) -> Result<(), NativeBleError> {
        self.ble_hub()?.disconnected(generation, reason)
    }

    /// Selected bearer, including future transports that are not available yet.
    pub fn transport(&self) -> NativeTransport {
        self.transport
    }

    /// Return the authoritative appliance snapshot as canonical JSON.
    pub fn snapshot_json(&self) -> Result<String, NativeApplianceError> {
        let handle = self.active_handle()?;
        to_json(handle.snapshot().as_ref())
    }

    /// Return durable contacts as canonical JSON.
    pub async fn contacts_json(&self) -> Result<String, NativeApplianceError> {
        let contacts = self.active_handle()?.contacts().await?;
        to_json(&contacts)
    }

    /// Return one peer's durable timeline as canonical JSON.
    pub async fn timeline_json(&self, destination: String) -> Result<String, NativeApplianceError> {
        let peer =
            reticulum_lxmf_chat_runtime::parse_destination(&destination).map_err(|error| {
                NativeApplianceError::InvalidArgument {
                    reason: error.to_string(),
                }
            })?;
        let timeline = self.active_handle()?.timeline(peer).await?;
        to_json(&timeline)
    }

    /// Validate and durably upsert a contact using the shared request DTO.
    pub async fn upsert_contact_json(
        &self,
        destination: String,
        request_json: String,
    ) -> Result<String, NativeApplianceError> {
        let request: ContactRequest = from_json(&request_json, "contact request")?;
        let contact = request.into_contact(&destination).map_err(|error| {
            NativeApplianceError::InvalidArgument {
                reason: error.to_string(),
            }
        })?;
        let response = reticulum_lxmf_chat_runtime::MutationResponse::from(
            self.active_handle()?.upsert_contact(contact).await?,
        );
        to_json(&response)
    }

    /// Validate and durably enqueue a message using the shared request DTO.
    pub async fn send_message_json(
        &self,
        request_json: String,
    ) -> Result<String, NativeApplianceError> {
        let request: SendRequest = from_json(&request_json, "send request")?;
        let material =
            request
                .into_material()
                .map_err(|error| NativeApplianceError::InvalidArgument {
                    reason: error.to_string(),
                })?;
        let response = SendResponse::from(self.active_handle()?.enqueue_send(material).await?);
        to_json(&response)
    }

    /// Schedule local inbox/outbox work without requiring a ready bearer.
    pub async fn sync_now(&self) -> Result<(), NativeApplianceError> {
        self.active_handle()?.sync_now().await?;
        Ok(())
    }

    /// Request a fresh device connection.
    ///
    /// Configured Wi-Fi and BLE owners schedule a fresh connection attempt.
    /// Reserved connector stubs return typed `TransportUnavailable` instead of
    /// silently falling back to another bearer.
    pub async fn reconnect(&self) -> Result<(), NativeApplianceError> {
        let handle = self.active_handle()?;
        if !self.connector_configured {
            return Err(self.transport.unavailable_error());
        }
        handle.reconnect().await?;
        Ok(())
    }

    /// Ask a configured connector to run now if no authenticated session is
    /// active, without disrupting an already-ready bearer.
    ///
    /// Platform-owned transports call this after registering a physical link.
    /// It is deliberately distinct from [`Self::reconnect`], which drops the
    /// current session and transport lease.
    pub async fn ensure_connected(&self) -> Result<(), NativeApplianceError> {
        let handle = self.active_handle()?;
        if !self.connector_configured {
            return Err(self.transport.unavailable_error());
        }
        handle.ensure_connected().await?;
        Ok(())
    }

    /// Idempotently stop the actor and close its SQLite ownership.
    pub async fn close(&self) -> Result<(), NativeApplianceError> {
        let _close_guard = self.close_gate.lock().await;
        if let Some(hub) = &self.ble_hub {
            hub.request_owner_disconnect("native appliance was closed");
        }
        let handle = self.lock_handle()?.take();
        match handle {
            Some(handle) => handle
                .shutdown_and_wait()
                .await
                .map_err(NativeApplianceError::from),
            None => Ok(()),
        }
    }
}

impl NativeAppliance {
    fn lock_handle(&self) -> Result<MutexGuard<'_, Option<ApplianceHandle>>, NativeApplianceError> {
        self.handle
            .lock()
            .map_err(|_| NativeApplianceError::Internal {
                reason: "appliance lifecycle lock is poisoned".to_owned(),
            })
    }

    fn active_handle(&self) -> Result<ApplianceHandle, NativeApplianceError> {
        self.lock_handle()?
            .clone()
            .ok_or(NativeApplianceError::Stopped)
    }

    fn ble_hub(&self) -> Result<Arc<BleHub>, NativeBleError> {
        self.ble_hub
            .as_ref()
            .map(Arc::clone)
            .ok_or(NativeBleError::Unavailable)
    }
}

fn from_json<T: DeserializeOwned>(input: &str, label: &str) -> Result<T, NativeApplianceError> {
    serde_json::from_str(input).map_err(|error| NativeApplianceError::InvalidArgument {
        reason: format!("invalid {label} JSON: {error}"),
    })
}

fn to_json<T: Serialize>(value: &T) -> Result<String, NativeApplianceError> {
    serde_json::to_string(value).map_err(|error| NativeApplianceError::Serialization {
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::{Value, json};

    use super::*;

    static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDatabase(PathBuf);

    impl TestDatabase {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "reticulum-native-{label}-{}-{nonce}-{sequence}.sqlite3",
                std::process::id()
            )))
        }

        fn path_string(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
            let _ = fs::remove_file(self.0.with_extension("sqlite3-shm"));
            let _ = fs::remove_file(self.0.with_extension("sqlite3-wal"));
        }
    }

    #[tokio::test]
    async fn native_facade_owns_offline_contacts_timeline_and_outbox() {
        let database = TestDatabase::new("offline");
        let appliance =
            NativeAppliance::open(database.path_string(), NativeTransport::BluetoothLowEnergy)
                .unwrap();
        let destination = "ab".repeat(16);

        assert_eq!(appliance.contacts_json().await.unwrap(), "[]");
        assert_eq!(
            appliance
                .upsert_contact_json(
                    destination.clone(),
                    json!({ "name": "Field node" }).to_string(),
                )
                .await
                .unwrap(),
            r#"{"outcome":"inserted"}"#
        );
        assert_eq!(
            serde_json::from_str::<Value>(&appliance.contacts_json().await.unwrap()).unwrap(),
            json!([{ "destination": destination, "name": "Field node" }])
        );

        let send = json!({
            "destination": "ab".repeat(16),
            "timestamp_ms": 1_000,
            "idempotency_key": "cd".repeat(16),
            "title": "hello",
            "content": "offline durable payload",
        })
        .to_string();
        assert_eq!(
            appliance.send_message_json(send.clone()).await.unwrap(),
            r#"{"outbox_id":1,"outcome":"inserted"}"#
        );
        assert_eq!(
            appliance.send_message_json(send).await.unwrap(),
            r#"{"outbox_id":1,"outcome":"existing"}"#
        );

        let timeline: Value =
            serde_json::from_str(&appliance.timeline_json("ab".repeat(16)).await.unwrap()).unwrap();
        assert_eq!(timeline.as_array().unwrap().len(), 1);
        assert_eq!(timeline[0]["direction"], "outbound");
        assert_eq!(timeline[0]["status"], "committed");
        assert_eq!(timeline[0]["content"]["value"], "offline durable payload");

        let snapshot: Value = serde_json::from_str(&appliance.snapshot_json().unwrap()).unwrap();
        assert_eq!(snapshot["contact_count"], 1);
        assert_eq!(snapshot["pending_outbox"], 1);
        assert_eq!(
            snapshot["connection"],
            json!({
                "state": "unavailable",
                "transport": "bluetooth_low_energy",
            })
        );
        assert_eq!(
            snapshot["last_error"],
            "Bluetooth Low Energy requires NativeAppliance.open_ble and a platform GATT link"
        );

        appliance.close().await.unwrap();
        appliance.close().await.unwrap();
        assert_eq!(
            appliance.contacts_json().await.unwrap_err(),
            NativeApplianceError::Stopped
        );
    }

    #[tokio::test]
    async fn every_reserved_native_transport_fails_reconnect_explicitly() {
        for transport in [
            NativeTransport::UsbSerial,
            NativeTransport::UsbOtg,
            NativeTransport::BluetoothLowEnergy,
            NativeTransport::Wifi,
        ] {
            let database = TestDatabase::new("transport");
            let appliance = NativeAppliance::open(database.path_string(), transport).unwrap();
            assert!(matches!(
                appliance.reconnect().await,
                Err(NativeApplianceError::TransportUnavailable {
                    transport: actual,
                    ..
                }) if actual == transport
            ));
            appliance.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn malformed_shared_request_json_is_an_invalid_argument() {
        let database = TestDatabase::new("bad-request");
        let appliance =
            NativeAppliance::open(database.path_string(), NativeTransport::Wifi).unwrap();
        let error = appliance
            .send_message_json(r#"{"destination":7}"#.to_owned())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            NativeApplianceError::InvalidArgument { .. }
        ));
        appliance.close().await.unwrap();
    }

    #[test]
    fn wifi_constructor_rejects_ambiguous_or_incomplete_endpoints() {
        let database = TestDatabase::new("bad-wifi");
        assert!(matches!(
            NativeAppliance::open_wifi(
                database.path_string(),
                "reticulum.local:4242".to_owned(),
                "/tmp/credential.rdpkey".to_owned(),
            ),
            Err(NativeApplianceError::InvalidArgument { .. })
        ));
        assert!(matches!(
            NativeAppliance::open_wifi(
                database.path_string(),
                "127.0.0.1:0".to_owned(),
                "/tmp/credential.rdpkey".to_owned(),
            ),
            Err(NativeApplianceError::InvalidArgument { .. })
        ));
        assert!(matches!(
            NativeAppliance::open_wifi(
                database.path_string(),
                "127.0.0.1:4242".to_owned(),
                "relative/credential.rdpkey".to_owned(),
            ),
            Err(NativeApplianceError::InvalidArgument { .. })
        ));
    }

    #[test]
    fn ble_constructor_requires_an_absolute_app_private_credential_path() {
        let database = TestDatabase::new("bad-ble");
        assert!(matches!(
            NativeAppliance::open_ble(
                database.path_string(),
                "relative/credential.rdpkey".to_owned(),
            ),
            Err(NativeApplianceError::InvalidArgument { .. })
        ));
    }

    #[tokio::test]
    async fn reserved_transport_owner_rejects_ble_platform_callbacks() {
        let database = TestDatabase::new("ble-unavailable");
        let appliance =
            NativeAppliance::open(database.path_string(), NativeTransport::BluetoothLowEnergy)
                .unwrap();

        assert_eq!(
            appliance
                .ble_link_connected("peripheral".to_owned(), 20)
                .unwrap_err(),
            NativeBleError::Unavailable
        );
        appliance.close().await.unwrap();
    }

    #[tokio::test]
    async fn configured_wifi_owner_accepts_explicit_reconnect_requests() {
        let database = TestDatabase::new("wifi-reconnect");
        let credential = std::env::temp_dir().join("reticulum-native-missing-credential.rdpkey");
        let appliance = NativeAppliance::open_wifi(
            database.path_string(),
            "127.0.0.1:9".to_owned(),
            credential.to_string_lossy().into_owned(),
        )
        .unwrap();

        appliance.reconnect().await.unwrap();
        appliance.close().await.unwrap();
    }
}
