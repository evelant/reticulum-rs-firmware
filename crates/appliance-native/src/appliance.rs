//! Offline-capable native ownership of the durable chat runtime.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use reticulum_appliance_runtime::{
    ApplianceConfig, ApplianceHandle, ContactRequest, MessageActivityPageRequest,
    NetworkConfigMutationRequest, NomadFetchPollRequest, NomadFetchStartRequest,
    PhoneLocationObservationView, RadioTracePageRequest, ReticulumProbePollRequest,
    ReticulumProbeStartRequest, RetrySendRequest, RetrySendResponse, SendRequest, SendResponse,
    ServiceError, start_appliance,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex as AsyncMutex;
use zeroize::Zeroizing;

use crate::ble::{
    BleConnector, BleHub, NativeBleError, NativeBlePlatformCommand, PLATFORM_COMMAND_WAIT,
};
use crate::credential::{
    CredentialImportError, CredentialImportPolicy, NativeCredentialStatus, NativeCredentialSummary,
};
use crate::profile::NativeProfileStore;

/// Failure returned through the native bridge.
#[derive(Debug, Eq, PartialEq, uniffi::Error)]
#[allow(missing_docs)]
pub enum NativeApplianceError {
    InvalidArgument { reason: String },
    Busy,
    Stopped,
    Storage { reason: String },
    CredentialPublicationUncertain { reason: String },
    Serialization { reason: String },
    Internal { reason: String },
}

impl fmt::Display for NativeApplianceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument { reason } => write!(formatter, "invalid argument: {reason}"),
            Self::Busy => formatter.write_str("appliance command queue is busy"),
            Self::Stopped => formatter.write_str("native appliance has stopped"),
            Self::Storage { reason } => {
                write!(formatter, "chat storage operation failed: {reason}")
            }
            Self::CredentialPublicationUncertain { reason } => {
                write!(
                    formatter,
                    "credential publication completed with uncertain durability: {reason}"
                )
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

impl From<CredentialImportError> for NativeApplianceError {
    fn from(error: CredentialImportError) -> Self {
        match error {
            CredentialImportError::Rejected { reason } => Self::Storage { reason },
            CredentialImportError::PublicationUncertain { reason } => {
                Self::CredentialPublicationUncertain { reason }
            }
        }
    }
}

/// Native owner for one device-keyed profile and its BLE appliance session.
///
/// App-facing semantic DTOs remain defined by
/// `reticulum-appliance-runtime`. Methods exchange canonical JSON so the Expo
/// adapter parses the same generated types used by the HTTP client.
#[derive(uniffi::Object)]
pub struct NativeAppliance {
    handle: Mutex<Option<ApplianceHandle>>,
    close_gate: AsyncMutex<()>,
    profile_store: Arc<NativeProfileStore>,
    ble_hub: Arc<BleHub>,
}

#[uniffi::export(async_runtime = "tokio")]
impl NativeAppliance {
    /// Open the active device-keyed profile with the platform-owned BLE GATT
    /// connector.
    ///
    /// Rust resolves both storage paths and retains the profile store for
    /// secret-free status, import, and future board activation operations.
    #[uniffi::constructor]
    pub fn open_ble_profile(
        profile_store: Arc<NativeProfileStore>,
    ) -> Result<Arc<Self>, NativeApplianceError> {
        let paths = profile_store.runtime_paths()?;
        let ble_hub = BleHub::new();
        let handle = start_appliance(
            ApplianceConfig::new(paths.database),
            BleConnector::new(paths.credential.clone(), Arc::clone(&ble_hub)),
        )
        .map_err(NativeApplianceError::from)?;
        Ok(Arc::new(Self {
            handle: Mutex::new(Some(handle)),
            close_gate: AsyncMutex::new(()),
            profile_store,
            ble_hub,
        }))
    }

    /// Inspect the configured app-private activated credential without
    /// returning any secret bytes.
    ///
    /// This reports storage and credential-format validity. Connector policy
    /// remains separate: for example, a canonical future-board credential is
    /// Active even when the current E290 BLE profile cannot derive its exact
    /// advertising name.
    pub fn credential_status(&self) -> Result<NativeCredentialStatus, NativeApplianceError> {
        self.profile_store.credential_status()
    }

    /// Validate one app-private staging file and publish its canonical 96-byte
    /// Active credential into the configured destination.
    ///
    /// This alpha import is create-only: it never replaces an existing path.
    /// The current BLE connector accepts only an E290 credential from which it
    /// can derive the exact advertising name before publication.
    /// The caller remains responsible for deleting its staging copy after this
    /// method returns.
    /// A later BLE pairing owner will populate the same storage boundary
    /// without moving pairing proofs or secret generation into TypeScript.
    pub fn import_activated_credential(
        &self,
        staging_path: String,
    ) -> Result<NativeCredentialSummary, NativeApplianceError> {
        let staging_path = PathBuf::from(staging_path);
        self.profile_store
            .import_activated_credential(&staging_path, CredentialImportPolicy::E290BleTarget)
            .map_err(NativeApplianceError::from)
    }

    /// Register one connected, subscribed GATT peripheral.
    ///
    /// A prior generation must first be reported through
    /// [`Self::ble_disconnected`]; this prevents silently orphaning its GATT
    /// ownership. `max_write_bytes` must be the platform-reported
    /// conservative single-write value bound, not the negotiated ATT MTU. The
    /// bridge validates that the platform supports the mandatory 20-byte ATT
    /// value, then caps every emitted fragment to the generated profile maximum.
    pub fn ble_link_connected(
        &self,
        peripheral_id: String,
        max_write_bytes: u32,
    ) -> Result<u64, NativeBleError> {
        self.ble_hub.link_connected(peripheral_id, max_write_bytes)
    }

    /// Append bytes from one confirmed TX characteristic indication.
    pub fn ble_ingest_indication(
        &self,
        generation: u64,
        bytes: Vec<u8>,
    ) -> Result<(), NativeBleError> {
        self.ble_hub.ingest_indication(generation, bytes)
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
        let hub = Arc::clone(&self.ble_hub);
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
        self.ble_hub.write_succeeded(generation, token)
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
        self.ble_hub.write_failed(generation, token, reason)
    }

    /// Report that the platform GATT link and subscriptions are gone.
    pub fn ble_disconnected(&self, generation: u64, reason: String) -> Result<(), NativeBleError> {
        self.ble_hub.disconnected(generation, reason)
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

    /// Return saved contacts and otherwise-unknown durable message peers.
    pub async fn conversation_peers_json(&self) -> Result<String, NativeApplianceError> {
        let peers = self.active_handle()?.conversation_peers().await?;
        to_json(&peers)
    }

    /// Return the bounded semantic projection of authenticated nearby
    /// `lxmf.delivery` announces as canonical JSON.
    ///
    /// Rust retains device-API paging, boot-incarnation handling, announce
    /// metadata decoding, and exact destination/identity formatting. The
    /// platform layer receives no announce bytes, cursors, or public keys.
    pub async fn nearby_peers_json(&self) -> Result<String, NativeApplianceError> {
        let peers = self.active_handle()?.nearby_peers().await?;
        to_json(&peers)
    }

    /// Return one coherent bounded local radio, interface, and retained-route
    /// diagnostics snapshot as canonical JSON.
    ///
    /// Retained routes are route-table state rather than a list of connected
    /// or necessarily reachable peers.
    pub async fn radio_routes_status_json(&self) -> Result<String, NativeApplianceError> {
        let status = self.active_handle()?.radio_routes_status().await?;
        to_json(&status)
    }

    /// Return the board-owned desired Wi-Fi and Reticulum TCP configuration as
    /// canonical JSON with all credentials redacted.
    pub async fn network_config_json(&self) -> Result<String, NativeApplianceError> {
        let config = self.active_handle()?.network_config().await?;
        to_json(&config)
    }

    /// Return current secret-free Wi-Fi station and Reticulum TCP interface
    /// state as canonical JSON.
    pub async fn network_status_json(&self) -> Result<String, NativeApplianceError> {
        let status = self.active_handle()?.network_status().await?;
        to_json(&status)
    }

    /// Queue ordinary primary, LXMF, and NomadNet service announces.
    ///
    /// Repeated requests are successful and return `already_pending` when the
    /// board has already retained an equivalent announce schedule.
    pub async fn manual_service_announce_json(&self) -> Result<String, NativeApplianceError> {
        let disposition = self.active_handle()?.manual_service_announce().await?;
        to_json(&disposition)
    }

    /// Validate and apply one compare-and-swap desired-network mutation.
    ///
    /// The input can contain a WPA2-Personal passphrase. Both the incoming JSON
    /// buffer and the shared runtime DTO zeroize their secret-bearing storage
    /// when dropped. Validation failures use the runtime's fixed safe
    /// vocabulary and never include caller-supplied fields.
    pub async fn mutate_network_config_json(
        &self,
        request_json: String,
    ) -> Result<String, NativeApplianceError> {
        let request_json = Zeroizing::new(request_json);
        let request: NetworkConfigMutationRequest =
            from_secret_json(request_json.as_str(), "network configuration mutation")?;
        let response = self.active_handle()?.mutate_network_config(request).await?;
        to_json(&response)
    }

    /// Validate and begin or replay one bounded NomadNet page fetch.
    pub async fn nomad_fetch_start_json(
        &self,
        request_json: String,
    ) -> Result<String, NativeApplianceError> {
        let request: NomadFetchStartRequest =
            from_json(&request_json, "Nomad fetch start request")?;
        request
            .validate()
            .map_err(|error| NativeApplianceError::InvalidArgument {
                reason: error.to_string(),
            })?;
        let response = self.active_handle()?.nomad_fetch_start(request).await?;
        to_json(&response)
    }

    /// Validate and poll one boot-scoped NomadNet page fetch.
    pub async fn nomad_fetch_poll_json(
        &self,
        request_json: String,
    ) -> Result<String, NativeApplianceError> {
        let request: NomadFetchPollRequest = from_json(&request_json, "Nomad fetch poll request")?;
        request
            .validate()
            .map_err(|error| NativeApplianceError::InvalidArgument {
                reason: error.to_string(),
            })?;
        let response = self.active_handle()?.nomad_fetch_poll(request).await?;
        to_json(&response)
    }

    /// Validate and begin or replay one bounded Reticulum path-and-proof probe.
    pub async fn reticulum_probe_start_json(
        &self,
        request_json: String,
    ) -> Result<String, NativeApplianceError> {
        let request: ReticulumProbeStartRequest =
            from_json(&request_json, "Reticulum probe start request")?;
        request
            .validate()
            .map_err(|error| NativeApplianceError::InvalidArgument {
                reason: error.to_string(),
            })?;
        let response = self.active_handle()?.reticulum_probe_start(request).await?;
        to_json(&response)
    }

    /// Validate and poll one boot-scoped Reticulum path-and-proof probe.
    pub async fn reticulum_probe_poll_json(
        &self,
        request_json: String,
    ) -> Result<String, NativeApplianceError> {
        let request: ReticulumProbePollRequest =
            from_json(&request_json, "Reticulum probe poll request")?;
        request
            .validate()
            .map_err(|error| NativeApplianceError::InvalidArgument {
                reason: error.to_string(),
            })?;
        let response = self.active_handle()?.reticulum_probe_poll(request).await?;
        to_json(&response)
    }

    /// Return one peer's durable timeline as canonical JSON.
    pub async fn timeline_json(&self, destination: String) -> Result<String, NativeApplianceError> {
        let peer =
            reticulum_appliance_runtime::parse_destination(&destination).map_err(|error| {
                NativeApplianceError::InvalidArgument {
                    reason: error.to_string(),
                }
            })?;
        let timeline = self.active_handle()?.timeline(peer).await?;
        to_json(&timeline)
    }

    /// Return one validated, bounded page of durable message activity as
    /// canonical JSON.
    pub async fn message_activity_json(
        &self,
        request_json: String,
    ) -> Result<String, NativeApplianceError> {
        let request: MessageActivityPageRequest =
            from_json(&request_json, "message activity request")?;
        request
            .validate()
            .map_err(|error| NativeApplianceError::InvalidArgument {
                reason: error.to_string(),
            })?;
        let page = self.active_handle()?.message_activity(request).await?;
        to_json(&page)
    }

    /// Return one validated, bounded page of durable packet-correlated RF
    /// trace as canonical JSON.
    pub async fn radio_trace_json(
        &self,
        request_json: String,
    ) -> Result<String, NativeApplianceError> {
        let request: RadioTracePageRequest = from_json(&request_json, "radio trace request")?;
        request
            .validate()
            .map_err(|error| NativeApplianceError::InvalidArgument {
                reason: error.to_string(),
            })?;
        let page = self.active_handle()?.radio_trace(request).await?;
        to_json(&page)
    }

    /// Return the phone-location state that will stamp the next local outbound
    /// attempt. This reads only the app-owned runtime and never contacts the
    /// appliance.
    pub async fn phone_location_observation_json(&self) -> Result<String, NativeApplianceError> {
        let observation = self.active_handle()?.phone_location_observation().await?;
        to_json(&observation)
    }

    /// Replace the app-owned phone-location state used by future outbound
    /// attempts. Existing durable attempt records remain immutable.
    pub async fn update_phone_location_observation_json(
        &self,
        observation_json: String,
    ) -> Result<String, NativeApplianceError> {
        let observation: PhoneLocationObservationView =
            from_json(&observation_json, "phone location observation")?;
        observation
            .validate()
            .map_err(|error| NativeApplianceError::InvalidArgument {
                reason: error.to_string(),
            })?;
        let observation = self
            .active_handle()?
            .update_phone_location(observation)
            .await?;
        to_json(&observation)
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
        let response = reticulum_appliance_runtime::MutationResponse::from(
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

    /// Rearm one retryable terminal message while preserving its timeline and
    /// semantic LXMF identity.
    pub async fn retry_message_json(
        &self,
        request_json: String,
    ) -> Result<String, NativeApplianceError> {
        let request: RetrySendRequest = from_json(&request_json, "retry send request")?;
        let (outbox_id, idempotency_key) =
            request
                .into_retry()
                .map_err(|error| NativeApplianceError::InvalidArgument {
                    reason: error.to_string(),
                })?;
        let response = RetrySendResponse::from(
            self.active_handle()?
                .retry_send(outbox_id, idempotency_key)
                .await?,
        );
        to_json(&response)
    }

    /// Schedule local inbox/outbox work without requiring a ready bearer.
    pub async fn sync_now(&self) -> Result<(), NativeApplianceError> {
        self.active_handle()?.sync_now().await?;
        Ok(())
    }

    /// Request a fresh device connection.
    ///
    /// The BLE owner schedules a fresh connection attempt.
    pub async fn reconnect(&self) -> Result<(), NativeApplianceError> {
        let handle = self.active_handle()?;
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
        handle.ensure_connected().await?;
        Ok(())
    }

    /// Idempotently stop the actor and close its SQLite ownership.
    pub async fn close(&self) -> Result<(), NativeApplianceError> {
        let _close_guard = self.close_gate.lock().await;
        self.ble_hub
            .request_owner_disconnect("native appliance was closed");
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
}

fn from_json<T: DeserializeOwned>(input: &str, label: &str) -> Result<T, NativeApplianceError> {
    serde_json::from_str(input).map_err(|error| NativeApplianceError::InvalidArgument {
        reason: format!("invalid {label} JSON: {error}"),
    })
}

fn from_secret_json<T: DeserializeOwned>(
    input: &str,
    label: &str,
) -> Result<T, NativeApplianceError> {
    serde_json::from_str(input).map_err(|_| NativeApplianceError::InvalidArgument {
        reason: format!("invalid {label} JSON"),
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

    use reticulum_appliance_runtime::{
        ConnectFailure, ConnectedSession, ConnectionMetadata, ConnectionTransport, Connector,
    };
    use reticulum_appliance_store::{
        AcceptanceIds, DestinationHash as CoreDestinationHash, DeviceBinding, InboundMessage,
        OutboxMaterial, SubmissionId, SubmissionState,
    };
    use reticulum_appliance_sync::{DeviceSessionError, InboxCursor, InboxSummary, LxmfSession};
    use reticulum_device_api::{
        NetworkConfigMutation, NetworkConfigMutationOutcome, NetworkConfigSnapshot,
        NetworkRuntimeStatus, ReticulumTcpPeerConfigSummary, ReticulumTcpPeerIpv4Address,
        ReticulumTcpPeerState, WifiNetworkConfigSummary, WifiNetworkProfileId, WifiStationState,
    };
    use serde_json::{Value, json};

    use super::*;

    static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn empty_radio_trace_page() -> reticulum_device_api::RadioTracePage {
        reticulum_device_api::RadioTracePage::new(
            1,
            reticulum_device_api::RadioTraceAppliedLoraProfile::new(
                [0x51; 16],
                915_000_000,
                125_000,
                8,
                22,
                10,
                5,
                true,
                true,
                false,
            ),
            1,
            1,
            false,
            [None, None],
            None,
        )
        .unwrap()
    }

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

        fn profile_root(&self) -> PathBuf {
            self.0.with_extension("profiles")
        }

        fn profile_store(&self) -> Arc<NativeProfileStore> {
            NativeProfileStore::open(self.profile_root().to_string_lossy().into_owned())
                .expect("test profile store opens")
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
            let _ = fs::remove_file(self.0.with_extension("sqlite3-shm"));
            let _ = fs::remove_file(self.0.with_extension("sqlite3-wal"));
            let _ = fs::remove_dir_all(self.profile_root());
        }
    }

    fn activated_credential_bytes(
        device_id: [u8; 16],
    ) -> [u8; reticulum_device_client::ACTIVATED_CREDENTIAL_STATE_BYTES] {
        let mut bytes = [0_u8; reticulum_device_client::ACTIVATED_CREDENTIAL_STATE_BYTES];
        bytes[..8].copy_from_slice(b"RDPKEY1\0");
        bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
        bytes[10] = 2;
        bytes[16..32].copy_from_slice(&device_id);
        bytes[32..48].fill(0x42);
        bytes[48..56].copy_from_slice(&7_u64.to_le_bytes());
        bytes[56..88].fill(0x24);
        bytes
    }

    struct NetworkTestSession;

    impl LxmfSession for NetworkTestSession {
        type Error = DeviceSessionError;

        fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
            Ok(DeviceBinding::new(
                [0x31; 16],
                CoreDestinationHash::new([0x32; 16]),
                CoreDestinationHash::new([0x33; 16]),
            ))
        }

        fn submit(&mut self, _material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
            unreachable!("the native network test has no outbox work")
        }

        fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
            unreachable!("the native network test has no accepted submissions")
        }

        fn next_inbox(
            &mut self,
            _after: Option<InboxCursor>,
        ) -> Result<Option<InboxSummary>, Self::Error> {
            Ok(None)
        }

        fn read_inbox(&mut self, _summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
            unreachable!("the native network test has no inbox messages")
        }

        fn inbox_status(&mut self) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
            Ok(reticulum_device_api::LxmfMailboxStatus::new(None, None).unwrap())
        }

        fn acknowledge_inbox_through(
            &mut self,
            through: InboxCursor,
        ) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
            let handle = reticulum_device_api::LxmfMessageHandle::new(through.get()).unwrap();
            Ok(reticulum_device_api::LxmfMailboxStatus::new(Some(handle), Some(handle)).unwrap())
        }

        fn next_nearby_peer(
            &mut self,
            _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
        ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
            unreachable!("the native network test does not request nearby peers")
        }

        fn nomad_fetch_start(
            &mut self,
            _request: reticulum_device_api::NomadFetchStartRequest<'_>,
        ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
            unreachable!("the native network test does not request NomadNet")
        }

        fn nomad_fetch_poll(
            &mut self,
            _id: reticulum_device_api::NomadFetchId,
        ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
            unreachable!("the native network test does not request NomadNet")
        }

        fn reticulum_probe_start(
            &mut self,
            _request: reticulum_device_api::ProbeStartRequest,
        ) -> Result<reticulum_device_api::ProbeStartAccepted, Self::Error> {
            unreachable!("the native network test does not request Reticulum probes")
        }

        fn reticulum_probe_poll(
            &mut self,
            _id: reticulum_device_api::ProbeId,
        ) -> Result<reticulum_device_api::ProbePollResponse, Self::Error> {
            unreachable!("the native network test does not request Reticulum probes")
        }

        fn network_config_get(&mut self) -> Result<NetworkConfigSnapshot, Self::Error> {
            let profile_id = WifiNetworkProfileId::new([0x44; 16]).unwrap();
            let profile =
                WifiNetworkConfigSummary::new(profile_id, true, 200, b"field\xff", true).unwrap();
            let peer = ReticulumTcpPeerConfigSummary::new(
                true,
                ReticulumTcpPeerIpv4Address::new([192, 0, 2, 9]).unwrap(),
                4242,
            )
            .unwrap();
            Ok(NetworkConfigSnapshot::with_defaults(
                9,
                [Some(profile), None, None, None],
                Some(peer),
            )
            .unwrap())
        }

        fn network_config_mutate(
            &mut self,
            request: reticulum_device_api::NetworkConfigMutationRequest<'_>,
        ) -> Result<NetworkConfigMutationOutcome, Self::Error> {
            let outcome = match request.mutation() {
                NetworkConfigMutation::UpsertWifi {
                    profile_id,
                    network,
                } => {
                    assert_eq!(request.expected_revision(), 9);
                    assert_eq!(request.idempotency_key().0, [0x66; 16]);
                    assert_eq!(profile_id.as_bytes(), &[0x55; 16]);
                    assert_eq!(network.ssid().as_bytes(), b"mesh");
                    assert!(network.credential().replacement().is_some());
                    NetworkConfigMutationOutcome::Applied {
                        revision: 10,
                        reboot_required: true,
                    }
                }
                NetworkConfigMutation::SetLoraTxPower(power) => {
                    assert_eq!(request.expected_revision(), 10);
                    assert_eq!(request.idempotency_key().0, [0x67; 16]);
                    assert_eq!(power.get(), 22);
                    NetworkConfigMutationOutcome::Applied {
                        revision: 11,
                        reboot_required: true,
                    }
                }
                _ => panic!("expected Wi-Fi or LoRa transmit-power mutation"),
            };
            Ok(outcome)
        }

        fn network_status(&mut self) -> Result<NetworkRuntimeStatus, Self::Error> {
            Ok(NetworkRuntimeStatus::new(
                9,
                8,
                WifiStationState::Connected,
                Some(WifiNetworkProfileId::new([0x44; 16]).unwrap()),
                Some(b"field\xff"),
                Some([198, 51, 100, 7]),
                Some(-81),
                ReticulumTcpPeerState::WaitingForNetwork,
            )
            .unwrap())
        }

        fn manual_service_announce(
            &mut self,
        ) -> Result<reticulum_device_api::ManualServiceAnnounceDisposition, Self::Error> {
            Ok(reticulum_device_api::ManualServiceAnnounceDisposition::Queued)
        }

        fn node_diagnostics(
            &mut self,
        ) -> Result<reticulum_device_api::NodeDiagnosticsSnapshot, Self::Error> {
            unreachable!("the native network test does not request node diagnostics")
        }

        fn route_diagnostics_page(
            &mut self,
            _request: reticulum_device_api::RouteDiagnosticsRequest,
        ) -> Result<reticulum_device_api::RouteDiagnosticsPage, Self::Error> {
            unreachable!("the native network test does not request route diagnostics")
        }

        fn radio_trace_page(
            &mut self,
            _request: reticulum_device_api::RadioTracePageRequest,
        ) -> Result<reticulum_device_api::RadioTracePage, Self::Error> {
            Ok(empty_radio_trace_page())
        }

        fn is_usable(&self) -> bool {
            true
        }
    }

    #[derive(Default)]
    struct NetworkTestConnector {
        connected: bool,
    }

    impl Connector for NetworkTestConnector {
        fn connect(
            &mut self,
        ) -> Result<reticulum_appliance_runtime::ConnectedSession, ConnectFailure> {
            if self.connected {
                return Err(ConnectFailure::retryable(
                    "native network test session was already claimed",
                ));
            }
            self.connected = true;
            Ok(ConnectedSession::new(
                NetworkTestSession,
                ConnectionMetadata::new(
                    ConnectionTransport::BluetoothLowEnergy,
                    "test-link",
                    "test-board",
                ),
            ))
        }
    }

    struct OfflineTestConnector;

    impl Connector for OfflineTestConnector {
        fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
            Err(ConnectFailure::unavailable(
                ConnectionTransport::BluetoothLowEnergy,
                "BLE is intentionally unavailable in this facade test",
            ))
        }
    }

    fn open_test_appliance<C: Connector>(
        database: &TestDatabase,
        connector: C,
    ) -> Arc<NativeAppliance> {
        let profile_store = database.profile_store();
        let ble_hub = BleHub::new();
        let handle = start_appliance(ApplianceConfig::new(database.0.clone()), connector).unwrap();
        Arc::new(NativeAppliance {
            handle: Mutex::new(Some(handle)),
            close_gate: AsyncMutex::new(()),
            profile_store,
            ble_hub,
        })
    }

    fn open_offline_test_appliance(database: &TestDatabase) -> Arc<NativeAppliance> {
        open_test_appliance(database, OfflineTestConnector)
    }

    fn open_network_test_appliance(database: &TestDatabase) -> Arc<NativeAppliance> {
        open_test_appliance(database, NetworkTestConnector::default())
    }

    async fn await_network_config(appliance: &NativeAppliance) -> String {
        for _ in 0..20 {
            match appliance.network_config_json().await {
                Ok(config) => return config,
                Err(NativeApplianceError::Storage { reason })
                    if reason.contains("no authenticated appliance session") =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => panic!("unexpected network configuration error: {error}"),
            }
        }
        panic!("test network session did not become ready");
    }

    fn valid_network_mutation_json(passphrase: &str) -> String {
        json!({
            "mutation": {
                "kind": "upsert_wifi",
                "profile_id": "55".repeat(16),
                "enabled": true,
                "priority": 240,
                "ssid": {
                    "encoding": "utf8",
                    "value": "mesh"
                },
                "credential": {
                    "kind": "replace",
                    "passphrase": passphrase
                }
            },
            "expected_revision": 9,
            "idempotency_key": "66".repeat(16)
        })
        .to_string()
    }

    #[test]
    fn only_post_publication_import_failures_cross_the_reconciliation_boundary() {
        assert_eq!(
            NativeApplianceError::from(CredentialImportError::Rejected {
                reason: "exact readback mismatch".to_owned(),
            }),
            NativeApplianceError::Storage {
                reason: "exact readback mismatch".to_owned(),
            }
        );
        assert_eq!(
            NativeApplianceError::from(CredentialImportError::PublicationUncertain {
                reason: "directory sync failed".to_owned(),
            }),
            NativeApplianceError::CredentialPublicationUncertain {
                reason: "directory sync failed".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn native_facade_owns_offline_contacts_timeline_and_outbox() {
        let database = TestDatabase::new("offline");
        let appliance = open_offline_test_appliance(&database);
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
        assert_eq!(
            serde_json::from_str::<Value>(&appliance.conversation_peers_json().await.unwrap())
                .unwrap(),
            json!([{
                "destination": "ab".repeat(16),
                "name": "Field node",
                "message_count": 0,
                "inbound_message_count": 0,
                "last_message": null,
            }])
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
        let peers: Value =
            serde_json::from_str(&appliance.conversation_peers_json().await.unwrap()).unwrap();
        assert_eq!(peers.as_array().unwrap().len(), 1);
        assert_eq!(peers[0]["message_count"], 1);
        assert_eq!(peers[0]["last_message"]["direction"], "outbound");
        assert_eq!(
            peers[0]["last_message"]["content"]["value"],
            "offline durable payload"
        );
        let activity: Value = serde_json::from_str(
            &appliance
                .message_activity_json(
                    json!({
                        "before_event_id": null,
                        "limit": 20,
                        "timeline_sequence": null,
                    })
                    .to_string(),
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(activity["events"].as_array().unwrap().len(), 1);
        assert_eq!(activity["events"][0]["event_id"], 1);
        assert_eq!(activity["events"][0]["timeline_sequence"], 1);
        assert_eq!(activity["events"][0]["peer"], "ab".repeat(16));
        assert_eq!(activity["events"][0]["direction"], "outbound");
        assert_eq!(activity["events"][0]["outbox_id"], 1);
        assert_eq!(activity["events"][0]["attempt_number"], 1);
        assert_eq!(
            activity["events"][0]["activity"],
            json!({ "kind": "outbound_queued" })
        );
        assert_eq!(activity["next_before_event_id"], Value::Null);
        assert_eq!(activity["history_incomplete"], false);
        assert!(matches!(
            appliance
                .message_activity_json(
                    json!({
                        "before_event_id": null,
                        "limit": 0,
                        "timeline_sequence": null,
                    })
                    .to_string(),
                )
                .await,
            Err(NativeApplianceError::InvalidArgument { .. })
        ));

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
            "BLE is intentionally unavailable in this facade test"
        );

        appliance.close().await.unwrap();
        appliance.close().await.unwrap();
        assert_eq!(
            appliance.contacts_json().await.unwrap_err(),
            NativeApplianceError::Stopped
        );
    }

    #[tokio::test]
    async fn native_network_facade_returns_redacted_views_and_applies_valid_mutations() {
        let database = TestDatabase::new("network");
        let appliance = open_network_test_appliance(&database);
        let secret = "bridge secret passphrase";

        let config: Value = serde_json::from_str(&await_network_config(&appliance).await).unwrap();
        assert_eq!(
            config,
            json!({
                "revision": 9,
                "wifi_profiles": [{
                    "profile_id": "44".repeat(16),
                    "enabled": true,
                    "priority": 200,
                    "ssid": {
                        "encoding": "hex",
                        "value": "6669656c64ff"
                    },
                    "credential_configured": true
                }],
                "tcp_peer": {
                    "enabled": true,
                    "ipv4_address": "192.0.2.9",
                    "port": 4242
                },
                "wifi_transport_enabled": true,
                "automatic_announces_enabled": true,
                "rmap_discovery_enabled": false,
                "rmap_share_location": false,
                "rmap_phone_location": null,
                "lora_tx_power_dbm": 14,
                "lora_profile": {
                    "frequency_hz": 915_000_000,
                    "bandwidth_hz": 125_000,
                    "spreading_factor": 7,
                    "coding_rate_denominator": 5,
                    "tx_power_dbm": 14
                }
            })
        );
        assert!(!config.to_string().contains(secret));

        let status: Value =
            serde_json::from_str(&appliance.network_status_json().await.unwrap()).unwrap();
        assert_eq!(
            status,
            json!({
                "configured_revision": 9,
                "applied_revision": 8,
                "wifi_state": "connected",
                "active_wifi_profile": "44".repeat(16),
                "connected_ssid": {
                    "encoding": "hex",
                    "value": "6669656c64ff"
                },
                "ipv4_address": "198.51.100.7",
                "rssi_dbm": -81,
                "tcp_peer_state": "waiting_for_network",
                "last_tcp_failure": null,
                "dns_diagnostics": null,
                "rmap_status": null
            })
        );
        assert!(!status.to_string().contains(secret));
        assert_eq!(
            appliance.manual_service_announce_json().await.unwrap(),
            r#""queued""#
        );

        let response = appliance
            .mutate_network_config_json(valid_network_mutation_json(secret))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&response).unwrap(),
            json!({
                "outcome": "applied",
                "revision": 10,
                "reboot_required": true
            })
        );
        assert!(!response.contains(secret));

        let response = appliance
            .mutate_network_config_json(
                json!({
                    "mutation": {
                        "kind": "set_lora_tx_power",
                        "lora_tx_power_dbm": 22
                    },
                    "expected_revision": 10,
                    "idempotency_key": "67".repeat(16)
                })
                .to_string(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&response).unwrap(),
            json!({
                "outcome": "applied",
                "revision": 11,
                "reboot_required": true
            })
        );

        appliance.close().await.unwrap();
    }

    #[tokio::test]
    async fn network_mutation_errors_never_echo_secret_json_fields() {
        let database = TestDatabase::new("network-errors");
        let appliance = open_network_test_appliance(&database);
        let _ = await_network_config(&appliance).await;
        let secret = "TOP-SECRET-PASSPHRASE";

        let malformed = format!(
            r#"{{"mutation":{{"kind":"upsert_wifi","credential":{{"kind":"replace","passphrase":"{secret}"}}}}"#
        );
        let error = appliance
            .mutate_network_config_json(malformed)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            NativeApplianceError::InvalidArgument {
                reason: "invalid network configuration mutation JSON".to_owned()
            }
        );
        assert!(!error.to_string().contains(secret));

        let invalid_passphrase = "short";
        let error = appliance
            .mutate_network_config_json(valid_network_mutation_json(invalid_passphrase))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            NativeApplianceError::Storage { ref reason }
                if reason == "WPA2-Personal passphrase must contain 8 to 63 printable ASCII bytes"
        ));
        assert!(!error.to_string().contains(invalid_passphrase));

        appliance.close().await.unwrap();
    }

    #[tokio::test]
    async fn network_facade_reports_stopped_after_the_actor_is_closed() {
        let database = TestDatabase::new("network-stopped");
        let appliance = open_network_test_appliance(&database);
        let _ = await_network_config(&appliance).await;
        appliance.close().await.unwrap();

        assert_eq!(
            appliance.network_config_json().await.unwrap_err(),
            NativeApplianceError::Stopped
        );
        assert_eq!(
            appliance.network_status_json().await.unwrap_err(),
            NativeApplianceError::Stopped
        );
        assert_eq!(
            appliance
                .mutate_network_config_json(valid_network_mutation_json("valid passphrase"))
                .await
                .unwrap_err(),
            NativeApplianceError::Stopped
        );
    }

    #[tokio::test]
    async fn malformed_shared_request_json_is_an_invalid_argument() {
        let database = TestDatabase::new("bad-request");
        let appliance = open_offline_test_appliance(&database);
        let error = appliance
            .send_message_json(r#"{"destination":7}"#.to_owned())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            NativeApplianceError::InvalidArgument { .. }
        ));
        let error = appliance
            .nomad_fetch_start_json(
                json!({
                    "destination": "11".repeat(16),
                    "path": "relative",
                    "timestamp_unix_ms": 1,
                    "idempotency_key": "22".repeat(16),
                })
                .to_string(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            NativeApplianceError::InvalidArgument { .. }
        ));
        appliance.close().await.unwrap();
    }

    #[tokio::test]
    async fn nearby_read_requires_the_actor_owned_authenticated_session() {
        let database = TestDatabase::new("nearby-offline");
        let appliance = open_offline_test_appliance(&database);
        assert!(matches!(
            appliance.nearby_peers_json().await,
            Err(NativeApplianceError::Storage { reason })
                if reason.contains("no authenticated appliance session")
        ));
        appliance.close().await.unwrap();
    }

    #[tokio::test]
    async fn radio_routes_read_requires_the_actor_owned_authenticated_session() {
        let database = TestDatabase::new("radio-routes-offline");
        let appliance = open_offline_test_appliance(&database);
        assert!(matches!(
            appliance.radio_routes_status_json().await,
            Err(NativeApplianceError::Storage { reason })
                if reason.contains("no authenticated appliance session")
        ));
        appliance.close().await.unwrap();
    }

    #[tokio::test]
    async fn nomad_fetch_requires_the_actor_owned_authenticated_session() {
        let database = TestDatabase::new("nomad-offline");
        let appliance = open_offline_test_appliance(&database);
        let start = json!({
            "destination": "11".repeat(16),
            "path": "/page/index.mu",
            "timestamp_unix_ms": 1,
            "idempotency_key": "22".repeat(16),
        })
        .to_string();
        assert!(matches!(
            appliance.nomad_fetch_start_json(start).await,
            Err(NativeApplianceError::Storage { reason })
                if reason.contains("no authenticated appliance session")
        ));
        let id = format!("{}0000000000000001", "33".repeat(8));
        assert!(matches!(
            appliance
                .nomad_fetch_poll_json(json!({ "id": id }).to_string())
                .await,
            Err(NativeApplianceError::Storage { reason })
                if reason.contains("no authenticated appliance session")
        ));
        appliance.close().await.unwrap();
    }

    #[tokio::test]
    async fn reticulum_probe_requires_the_actor_owned_authenticated_session() {
        let database = TestDatabase::new("probe-offline");
        let appliance = open_offline_test_appliance(&database);
        let start = json!({
            "destination": "11".repeat(16),
            "idempotency_key": "22".repeat(16),
        })
        .to_string();
        assert!(matches!(
            appliance.reticulum_probe_start_json(start).await,
            Err(NativeApplianceError::Storage { reason })
                if reason.contains("no authenticated appliance session")
        ));
        assert!(matches!(
            appliance
                .reticulum_probe_poll_json(
                    json!({ "id": "33".repeat(16) }).to_string()
                )
                .await,
            Err(NativeApplianceError::Storage { reason })
                if reason.contains("no authenticated appliance session")
        ));
        appliance.close().await.unwrap();
    }

    #[tokio::test]
    async fn configured_native_owner_imports_and_inspects_one_credential() {
        let database = TestDatabase::new("credential-import");
        let staging = database.0.with_extension("picked.rdpkey");
        let bytes = activated_credential_bytes(*b"e290-api-1\xac\xa7\x04\xe1\x3e\x88");
        fs::write(&staging, bytes).expect("selected credential is staged");

        let profile_store = database.profile_store();
        let appliance = NativeAppliance::open_ble_profile(profile_store).unwrap();
        assert_eq!(
            appliance.credential_status().unwrap(),
            NativeCredentialStatus::Missing
        );

        let summary = appliance
            .import_activated_credential(staging.to_string_lossy().into_owned())
            .expect("native facade imports one credential");
        assert_eq!(
            summary.expected_ble_local_name.as_deref(),
            Some("reticulum-e290-e13e88")
        );
        assert_eq!(
            appliance.credential_status().unwrap(),
            NativeCredentialStatus::Active {
                summary: summary.clone()
            }
        );
        assert!(
            staging.exists(),
            "the app retains staging cleanup ownership"
        );
        assert_eq!(
            appliance
                .import_activated_credential(staging.to_string_lossy().into_owned())
                .expect("an exact profile import is idempotent"),
            summary
        );

        appliance.close().await.unwrap();
        fs::remove_file(staging).expect("staged test credential is removed");
    }

    #[tokio::test]
    async fn ble_import_rejects_a_non_e290_target_without_claiming_the_destination() {
        let database = TestDatabase::new("ble-import-target");
        let staging = database.0.with_extension("picked.rdpkey");
        let bytes = activated_credential_bytes(*b"other-api-\x01\x02\x03\x04\x05\x06");
        fs::write(&staging, bytes).expect("generic credential is staged");

        let profile_store = database.profile_store();
        let appliance = NativeAppliance::open_ble_profile(Arc::clone(&profile_store)).unwrap();
        assert!(matches!(
            appliance.import_activated_credential(staging.to_string_lossy().into_owned()),
            Err(NativeApplianceError::Storage { reason })
                if reason.contains("current BLE import requires an E290 credential")
        ));
        assert_eq!(
            appliance.credential_status().unwrap(),
            NativeCredentialStatus::Missing
        );

        profile_store
            .import_activated_credential(&staging, CredentialImportPolicy::AnyDevice)
            .expect("generic policy can install the same credential");
        assert!(matches!(
            appliance.credential_status().unwrap(),
            NativeCredentialStatus::Active { summary }
                if summary.expected_ble_local_name.is_none()
        ));

        appliance.close().await.unwrap();
        fs::remove_file(staging).expect("staged test credential is removed");
    }
}
