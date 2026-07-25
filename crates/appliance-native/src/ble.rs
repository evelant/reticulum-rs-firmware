//! Native BLE GATT byte bridge and authenticated device connector.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use reticulum_device_api_ble::INITIAL_ATT_VALUE_BYTES;
use reticulum_device_client::{ClientConfig, ClientError, ClientSessionProfile, DeviceClient};
use reticulum_lxmf_chat_app::DeviceClientSession;
use reticulum_lxmf_chat_runtime::{
    ConnectFailure, ConnectedSession, ConnectionMetadata, ConnectionTransport, Connector,
};

use crate::credential::{HostRng, read_credential};

const DEFAULT_IO_SLICE: Duration = Duration::from_millis(100);
const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const PLATFORM_COMMAND_WAIT: Duration = Duration::from_secs(15);
const DEFAULT_INBOUND_CAPACITY: usize = 16 * 1024;
const MAX_GATT_WRITE_BYTES: usize = 512;
const MAX_PERIPHERAL_ID_BYTES: usize = 256;
const MAX_PLATFORM_REASON_BYTES: usize = 512;

/// One command for the platform BLE central implementation.
///
/// `Write` bytes must be sent with GATT write-with-response. The platform must
/// then report exactly one success or failure for the returned token.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum NativeBlePlatformCommand {
    /// Write one opaque RDA1 byte-stream fragment to the device RX
    /// characteristic.
    Write {
        /// Physical-link generation that owns this command.
        generation: u64,
        /// Nonzero acknowledgement token.
        token: u64,
        /// Opaque bytes, already bounded to the platform's write limit.
        bytes: Vec<u8>,
    },
    /// Tear down the platform GATT connection and its subscriptions.
    Disconnect {
        /// Physical-link generation that must be torn down.
        generation: u64,
        /// Human-readable reason suitable for diagnostics.
        reason: String,
    },
}

/// Failure at the platform-to-Rust BLE byte bridge.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Error)]
#[allow(missing_docs)]
pub enum NativeBleError {
    Unavailable,
    InvalidArgument {
        reason: String,
    },
    StaleGeneration {
        requested: u64,
        active_generation: Option<u64>,
    },
    LinkAlreadyActive {
        generation: u64,
    },
    LinkClosed {
        generation: u64,
        reason: String,
    },
    QueueOverflow {
        generation: u64,
        capacity: u32,
    },
    UnexpectedWriteToken {
        generation: u64,
        token: u64,
    },
    Internal {
        reason: String,
    },
}

impl fmt::Display for NativeBleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("this native appliance has no BLE connector"),
            Self::InvalidArgument { reason } => write!(formatter, "invalid argument: {reason}"),
            Self::StaleGeneration {
                requested,
                active_generation,
            } => match active_generation {
                Some(active) => write!(
                    formatter,
                    "BLE generation {requested} is stale; active generation is {active}"
                ),
                None => write!(
                    formatter,
                    "BLE generation {requested} is stale; there is no active generation"
                ),
            },
            Self::LinkAlreadyActive { generation } => write!(
                formatter,
                "BLE generation {generation} must be explicitly disconnected before replacement"
            ),
            Self::LinkClosed { generation, reason } => {
                write!(formatter, "BLE generation {generation} is closed: {reason}")
            }
            Self::QueueOverflow {
                generation,
                capacity,
            } => write!(
                formatter,
                "BLE generation {generation} exceeded its {capacity}-byte inbound queue"
            ),
            Self::UnexpectedWriteToken { generation, token } => write!(
                formatter,
                "BLE generation {generation} has no delivered write token {token}"
            ),
            Self::Internal { reason } => write!(formatter, "BLE bridge failure: {reason}"),
        }
    }
}

impl std::error::Error for NativeBleError {}

#[derive(Clone, Copy)]
struct BleHubConfig {
    io_slice: Duration,
    write_ack_timeout: Duration,
    inbound_capacity: usize,
}

impl Default for BleHubConfig {
    fn default() -> Self {
        Self {
            io_slice: DEFAULT_IO_SLICE,
            write_ack_timeout: DEFAULT_OPERATION_TIMEOUT,
            inbound_capacity: DEFAULT_INBOUND_CAPACITY,
        }
    }
}

pub(crate) struct BleHub {
    state: Mutex<HubState>,
    changed: Condvar,
    config: BleHubConfig,
}

struct HubState {
    next_generation: u64,
    next_token: u64,
    active: Option<LinkState>,
}

struct LinkState {
    generation: u64,
    peripheral_id: String,
    max_write_bytes: usize,
    claimed: bool,
    incoming: VecDeque<u8>,
    pending_write: Option<PendingWrite>,
    disconnect: Option<DisconnectRequest>,
    disconnected_reason: Option<String>,
}

struct PendingWrite {
    token: u64,
    bytes: Vec<u8>,
    delivered: bool,
    outcome: Option<Result<(), String>>,
}

struct DisconnectRequest {
    reason: String,
    delivered: bool,
}

struct LinkClaim {
    generation: u64,
    peripheral_id: String,
}

impl BleHub {
    pub(crate) fn new() -> Arc<Self> {
        Self::with_config(BleHubConfig::default())
    }

    fn with_config(config: BleHubConfig) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(HubState {
                next_generation: 1,
                next_token: 1,
                active: None,
            }),
            changed: Condvar::new(),
            config,
        })
    }

    pub(crate) fn link_connected(
        &self,
        peripheral_id: String,
        max_write_bytes: u32,
    ) -> Result<u64, NativeBleError> {
        if peripheral_id.is_empty() || peripheral_id.len() > MAX_PERIPHERAL_ID_BYTES {
            return Err(NativeBleError::InvalidArgument {
                reason: format!(
                    "BLE peripheral id must contain 1..={MAX_PERIPHERAL_ID_BYTES} bytes"
                ),
            });
        }
        let platform_max_write_bytes =
            usize::try_from(max_write_bytes).map_err(|_| NativeBleError::InvalidArgument {
                reason: "BLE maximum write size does not fit this platform".to_owned(),
            })?;
        if !(INITIAL_ATT_VALUE_BYTES..=MAX_GATT_WRITE_BYTES).contains(&platform_max_write_bytes) {
            return Err(NativeBleError::InvalidArgument {
                reason: format!(
                    "BLE maximum write size must be in {INITIAL_ATT_VALUE_BYTES}..={MAX_GATT_WRITE_BYTES}"
                ),
            });
        }
        // GATT 1.0 deliberately fixes both characteristic values at the
        // unnegotiated ATT value bound. A larger platform-reported
        // write-with-response maximum is useful capability information, but it
        // must not enlarge fragments sent to this firmware profile.
        let max_write_bytes = platform_max_write_bytes.min(INITIAL_ATT_VALUE_BYTES);

        let mut state = self.lock()?;
        if let Some(link) = state.active.as_ref()
            && link.disconnected_reason.is_none()
        {
            return Err(NativeBleError::LinkAlreadyActive {
                generation: link.generation,
            });
        }
        let generation = state.next_generation;
        state.next_generation =
            next_nonzero(state.next_generation).ok_or_else(|| NativeBleError::Internal {
                reason: "BLE generation counter exhausted".to_owned(),
            })?;
        state.active = Some(LinkState {
            generation,
            peripheral_id,
            max_write_bytes,
            claimed: false,
            incoming: VecDeque::with_capacity(self.config.inbound_capacity),
            pending_write: None,
            disconnect: None,
            disconnected_reason: None,
        });
        self.changed.notify_all();
        Ok(generation)
    }

    pub(crate) fn ingest_indication(
        &self,
        generation: u64,
        bytes: Vec<u8>,
    ) -> Result<(), NativeBleError> {
        let mut state = self.lock()?;
        let link = active_link_mut(&mut state, generation)?;
        ensure_link_open(link)?;
        if bytes.is_empty() {
            return Ok(());
        }
        let remaining = self
            .config
            .inbound_capacity
            .saturating_sub(link.incoming.len());
        if bytes.len() > remaining {
            let reason = format!(
                "platform delivered more than {} queued indication bytes",
                self.config.inbound_capacity
            );
            mark_closing(link, reason);
            self.changed.notify_all();
            return Err(NativeBleError::QueueOverflow {
                generation,
                capacity: u32::try_from(self.config.inbound_capacity)
                    .expect("BLE inbound capacity fits u32"),
            });
        }
        link.incoming.extend(bytes);
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn next_platform_command(
        &self,
        generation: u64,
        timeout: Duration,
    ) -> Result<Option<NativeBlePlatformCommand>, NativeBleError> {
        let deadline =
            Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| NativeBleError::InvalidArgument {
                    reason: "BLE command wait is too large".to_owned(),
                })?;
        let mut state = self.lock()?;
        loop {
            let link = active_link_mut(&mut state, generation)?;
            if let Some(disconnect) = link.disconnect.as_mut() {
                if !disconnect.delivered {
                    disconnect.delivered = true;
                    return Ok(Some(NativeBlePlatformCommand::Disconnect {
                        generation,
                        reason: disconnect.reason.clone(),
                    }));
                }
                return Err(link_closed_error(link));
            }
            if let Some(reason) = link.disconnected_reason.as_ref() {
                return Err(NativeBleError::LinkClosed {
                    generation,
                    reason: reason.clone(),
                });
            }
            if let Some(pending) = link.pending_write.as_mut()
                && !pending.delivered
            {
                pending.delivered = true;
                return Ok(Some(NativeBlePlatformCommand::Write {
                    generation,
                    token: pending.token,
                    bytes: pending.bytes.clone(),
                }));
            }

            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next, wait) = self.changed.wait_timeout(state, remaining).map_err(|_| {
                NativeBleError::Internal {
                    reason: "BLE bridge lock is poisoned".to_owned(),
                }
            })?;
            state = next;
            if wait.timed_out() {
                return Ok(None);
            }
        }
    }

    pub(crate) fn write_succeeded(
        &self,
        generation: u64,
        token: u64,
    ) -> Result<(), NativeBleError> {
        let mut state = self.lock()?;
        let link = active_link_mut(&mut state, generation)?;
        ensure_link_open(link)?;
        let pending = delivered_pending_write(link, token)?;
        pending.outcome = Some(Ok(()));
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn write_failed(
        &self,
        generation: u64,
        token: u64,
        reason: String,
    ) -> Result<(), NativeBleError> {
        validate_platform_reason(&reason)?;
        let mut state = self.lock()?;
        let link = active_link_mut(&mut state, generation)?;
        ensure_link_open(link)?;
        let pending = delivered_pending_write(link, token)?;
        pending.outcome = Some(Err(reason.clone()));
        mark_closing(link, format!("platform GATT write failed: {reason}"));
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn disconnected(
        &self,
        generation: u64,
        reason: String,
    ) -> Result<(), NativeBleError> {
        validate_platform_reason(&reason)?;
        let mut state = self.lock()?;
        let link = active_link_mut(&mut state, generation)?;
        link.disconnected_reason = Some(reason.clone());
        link.incoming.clear();
        if let Some(pending) = link.pending_write.as_mut()
            && pending.outcome.is_none()
        {
            pending.outcome = Some(Err(reason));
        }
        link.claimed = false;
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn request_owner_disconnect(&self, reason: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(link) = state.active.as_mut()
            && link.disconnected_reason.is_none()
        {
            mark_closing(link, reason.to_owned());
            self.changed.notify_all();
        }
    }

    pub(crate) fn claim_link(
        self: &Arc<Self>,
    ) -> Result<(BleStream, BleLinkLease, String), ConnectFailure> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ConnectFailure::retryable("BLE bridge lock is poisoned"))?;
        let Some(link) = state.active.as_mut() else {
            return Err(ConnectFailure::retryable(
                "no subscribed BLE GATT link is ready",
            ));
        };
        if let Some(reason) = link
            .disconnected_reason
            .as_ref()
            .or_else(|| link.disconnect.as_ref().map(|request| &request.reason))
        {
            return Err(ConnectFailure::retryable(format!(
                "BLE GATT link requires replacement: {reason}"
            )));
        }
        if link.claimed {
            return Err(ConnectFailure::retryable(
                "BLE GATT link is already owned by a device session",
            ));
        }
        link.claimed = true;
        let claim = LinkClaim {
            generation: link.generation,
            peripheral_id: link.peripheral_id.clone(),
        };
        Ok((
            BleStream {
                hub: Arc::clone(self),
                generation: claim.generation,
            },
            BleLinkLease {
                hub: Arc::clone(self),
                generation: claim.generation,
            },
            claim.peripheral_id,
        ))
    }

    fn release_lease(&self, generation: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(link) = state.active.as_mut() else {
            return;
        };
        if link.generation != generation || !link.claimed {
            return;
        }
        link.claimed = false;
        if link.disconnected_reason.is_none() {
            mark_closing(link, "Rust BLE session lease was released".to_owned());
        }
        self.changed.notify_all();
    }

    fn lock(&self) -> Result<MutexGuard<'_, HubState>, NativeBleError> {
        self.state.lock().map_err(|_| NativeBleError::Internal {
            reason: "BLE bridge lock is poisoned".to_owned(),
        })
    }
}

fn next_nonzero(current: u64) -> Option<u64> {
    current.checked_add(1).filter(|next| *next != 0)
}

fn active_link_mut(
    state: &mut HubState,
    generation: u64,
) -> Result<&mut LinkState, NativeBleError> {
    let active_generation = state.active.as_ref().map(|link| link.generation);
    match state.active.as_mut() {
        Some(link) if link.generation == generation => Ok(link),
        _ => Err(NativeBleError::StaleGeneration {
            requested: generation,
            active_generation,
        }),
    }
}

fn ensure_link_open(link: &LinkState) -> Result<(), NativeBleError> {
    if let Some(reason) = link.disconnected_reason.as_ref() {
        return Err(NativeBleError::LinkClosed {
            generation: link.generation,
            reason: reason.clone(),
        });
    }
    if link.disconnect.is_some() {
        return Err(link_closed_error(link));
    }
    Ok(())
}

fn link_closed_error(link: &LinkState) -> NativeBleError {
    let reason = link
        .disconnected_reason
        .as_ref()
        .or_else(|| link.disconnect.as_ref().map(|request| &request.reason))
        .cloned()
        .unwrap_or_else(|| "link is no longer available".to_owned());
    NativeBleError::LinkClosed {
        generation: link.generation,
        reason,
    }
}

fn delivered_pending_write(
    link: &mut LinkState,
    token: u64,
) -> Result<&mut PendingWrite, NativeBleError> {
    match link.pending_write.as_mut() {
        Some(pending)
            if pending.token == token && pending.delivered && pending.outcome.is_none() =>
        {
            Ok(pending)
        }
        _ => Err(NativeBleError::UnexpectedWriteToken {
            generation: link.generation,
            token,
        }),
    }
}

fn validate_platform_reason(reason: &str) -> Result<(), NativeBleError> {
    if reason.is_empty() || reason.len() > MAX_PLATFORM_REASON_BYTES {
        return Err(NativeBleError::InvalidArgument {
            reason: format!(
                "BLE platform reason must contain 1..={MAX_PLATFORM_REASON_BYTES} bytes"
            ),
        });
    }
    Ok(())
}

fn mark_closing(link: &mut LinkState, reason: String) {
    if link.disconnect.is_none() {
        link.disconnect = Some(DisconnectRequest {
            reason: reason.clone(),
            delivered: false,
        });
    }
    link.incoming.clear();
    if let Some(pending) = link.pending_write.as_mut()
        && pending.outcome.is_none()
    {
        pending.outcome = Some(Err(reason));
    }
}

pub(crate) struct BleStream {
    hub: Arc<BleHub>,
    generation: u64,
}

impl Read for BleStream {
    fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        if destination.is_empty() {
            return Ok(0);
        }
        let mut state = self.hub.state.lock().map_err(lock_io_error)?;
        loop {
            let link = stream_link_mut(&mut state, self.generation, io::ErrorKind::UnexpectedEof)?;
            if link.disconnected_reason.is_some() || link.disconnect.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    stream_closed_reason(link),
                ));
            }
            if !link.incoming.is_empty() {
                let count = destination.len().min(link.incoming.len());
                for output in &mut destination[..count] {
                    *output = link
                        .incoming
                        .pop_front()
                        .expect("BLE inbound length was checked");
                }
                return Ok(count);
            }
            let (next, wait) = self
                .hub
                .changed
                .wait_timeout(state, self.hub.config.io_slice)
                .map_err(|_| lock_io_error(()))?;
            state = next;
            if wait.timed_out() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "BLE indication read made no progress",
                ));
            }
        }
    }
}

impl Write for BleStream {
    fn write(&mut self, source: &[u8]) -> io::Result<usize> {
        if source.is_empty() {
            return Ok(0);
        }
        let deadline = Instant::now()
            .checked_add(self.hub.config.write_ack_timeout)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "BLE timeout is too large")
            })?;
        let mut state = self.hub.state.lock().map_err(lock_io_error)?;
        let max_write_bytes = {
            let link = stream_link_mut(&mut state, self.generation, io::ErrorKind::BrokenPipe)?;
            ensure_stream_open(link, io::ErrorKind::BrokenPipe)?;
            if !link.claimed {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "BLE link has no active Rust session lease",
                ));
            }
            if link.pending_write.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "BLE link already has one write awaiting acknowledgement",
                ));
            }
            link.max_write_bytes
        };
        let token = state.next_token;
        state.next_token = next_nonzero(state.next_token)
            .ok_or_else(|| io::Error::other("BLE write token counter exhausted"))?;
        let bytes = source[..source.len().min(max_write_bytes)].to_vec();
        let link = stream_link_mut(&mut state, self.generation, io::ErrorKind::BrokenPipe)?;
        link.pending_write = Some(PendingWrite {
            token,
            bytes,
            delivered: false,
            outcome: None,
        });
        self.hub.changed.notify_all();

        loop {
            let link = stream_link_mut(&mut state, self.generation, io::ErrorKind::BrokenPipe)?;
            if let Some(outcome) = link
                .pending_write
                .as_ref()
                .and_then(|pending| pending.outcome.clone())
            {
                let count = link
                    .pending_write
                    .as_ref()
                    .expect("BLE pending outcome belongs to a pending write")
                    .bytes
                    .len();
                link.pending_write = None;
                self.hub.changed.notify_all();
                return outcome.map(|()| count).map_err(|reason| {
                    io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        format!("BLE write failed: {reason}"),
                    )
                });
            }
            ensure_stream_open(link, io::ErrorKind::BrokenPipe)?;

            let now = Instant::now();
            if now >= deadline {
                mark_closing(
                    link,
                    format!("BLE write token {token} acknowledgement timed out"),
                );
                link.pending_write = None;
                self.hub.changed.notify_all();
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "BLE write acknowledgement timed out; link must be replaced",
                ));
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next, wait) = self
                .hub
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| lock_io_error(()))?;
            state = next;
            if wait.timed_out() {
                let link = stream_link_mut(&mut state, self.generation, io::ErrorKind::BrokenPipe)?;
                mark_closing(
                    link,
                    format!("BLE write token {token} acknowledgement timed out"),
                );
                link.pending_write = None;
                self.hub.changed.notify_all();
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "BLE write acknowledgement timed out; link must be replaced",
                ));
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn stream_link_mut(
    state: &mut HubState,
    generation: u64,
    kind: io::ErrorKind,
) -> io::Result<&mut LinkState> {
    match state.active.as_mut() {
        Some(link) if link.generation == generation => Ok(link),
        _ => Err(io::Error::new(
            kind,
            format!("BLE generation {generation} is no longer active"),
        )),
    }
}

fn ensure_stream_open(link: &LinkState, kind: io::ErrorKind) -> io::Result<()> {
    if link.disconnected_reason.is_some() || link.disconnect.is_some() {
        Err(io::Error::new(kind, stream_closed_reason(link)))
    } else {
        Ok(())
    }
}

fn stream_closed_reason(link: &LinkState) -> String {
    link.disconnected_reason
        .as_ref()
        .or_else(|| link.disconnect.as_ref().map(|request| &request.reason))
        .cloned()
        .unwrap_or_else(|| "BLE link closed".to_owned())
}

fn lock_io_error<T>(_: T) -> io::Error {
    io::Error::other("BLE bridge lock is poisoned")
}

pub(crate) struct BleLinkLease {
    hub: Arc<BleHub>,
    generation: u64,
}

impl Drop for BleLinkLease {
    fn drop(&mut self) {
        self.hub.release_lease(self.generation);
    }
}

/// Connector that authenticates the device API across one platform-owned GATT
/// link.
pub(crate) struct BleConnector {
    credential_path: PathBuf,
    hub: Arc<BleHub>,
    operation_timeout: Duration,
}

impl BleConnector {
    pub(crate) const fn new(credential_path: PathBuf, hub: Arc<BleHub>) -> Self {
        Self {
            credential_path,
            hub,
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
        }
    }
}

impl Connector for BleConnector {
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
        // Claim the registered physical link before reading local
        // configuration. If the credential is missing or malformed, dropping
        // this lease schedules the platform disconnect instead of leaving an
        // unowned subscribed GATT link open.
        let (stream, lease, peripheral_id) = self.hub.claim_link()?;
        let credential =
            read_credential(&self.credential_path).map_err(ConnectFailure::permanent)?;
        let client = DeviceClient::connect_with_profile(
            stream,
            credential,
            &mut HostRng,
            ClientConfig::new(
                self.operation_timeout,
                self.operation_timeout,
                512,
                16 * 1024 * 1024,
            ),
            ClientSessionProfile::BleGattQualification,
        )
        .map_err(classify_client_failure)?;
        let device_label = hex::encode(client.device_id().as_bytes());
        Ok(ConnectedSession::new(
            DeviceClientSession::new(client),
            ConnectionMetadata::new(
                ConnectionTransport::BluetoothLowEnergy,
                peripheral_id,
                device_label,
            ),
        )
        .with_connection_lease(lease))
    }
}

fn classify_client_failure(error: ClientError) -> ConnectFailure {
    if matches!(&error, ClientError::Handshake(_)) {
        ConnectFailure::permanent(format!("BLE authentication failed: {error}"))
    } else {
        ConnectFailure::retryable(format!("BLE device session failed: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use rand_core::{CryptoRng, RngCore};
    use reticulum_device_api_framing::{DecodeEvent, Record, StreamDecoder};
    use reticulum_device_api_session::{
        ActiveCredential, BearerBinding, ClientHello, CredentialGeneration, CredentialId, DeviceId,
        ServerHelloFlight, ServerParameters, SessionEpochAllocator, SessionSuite,
    };

    use super::*;

    const DEVICE_BYTES: [u8; 16] = [0x41; 16];
    const CREDENTIAL_BYTES: [u8; 16] = [0x42; 16];
    const CREDENTIAL_GENERATION: u64 = 9;
    const PSK: [u8; 32] = [0x43; 32];
    static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct FixedRng(u8);

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            u32::from_le_bytes([self.0; 4])
        }

        fn next_u64(&mut self) -> u64 {
            u64::from_le_bytes([self.0; 8])
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            destination.fill(self.0);
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for FixedRng {}

    struct TestCredential(PathBuf);

    impl TestCredential {
        fn unique_path(label: &str) -> PathBuf {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time follows the Unix epoch")
                .as_nanos();
            let sequence = FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            std::env::temp_dir().join(format!(
                "reticulum-native-ble-{label}-{}-{nonce}-{sequence}.rdpkey",
                std::process::id()
            ))
        }

        fn new() -> Self {
            let path = Self::unique_path("valid");
            let mut bytes = [0_u8; 96];
            bytes[..8].copy_from_slice(b"RDPKEY1\0");
            bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
            bytes[10] = 2;
            bytes[16..32].copy_from_slice(&DEVICE_BYTES);
            bytes[32..48].copy_from_slice(&CREDENTIAL_BYTES);
            bytes[48..56].copy_from_slice(&CREDENTIAL_GENERATION.to_le_bytes());
            bytes[56..88].copy_from_slice(&PSK);
            write_private_test_credential(&path, &bytes);
            Self(path)
        }

        fn absent() -> Self {
            Self(Self::unique_path("absent"))
        }

        fn malformed() -> Self {
            let path = Self::unique_path("malformed");
            write_private_test_credential(&path, b"not-an-activated-credential");
            Self(path)
        }
    }

    impl Drop for TestCredential {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn write_private_test_credential(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("test credential writes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .expect("test credential permissions are owner-only");
        }
    }

    fn claimed_hub(
        config: BleHubConfig,
        max_write_bytes: u32,
    ) -> (Arc<BleHub>, u64, BleStream, BleLinkLease) {
        let hub = BleHub::with_config(config);
        let generation = hub
            .link_connected("test-peripheral".to_owned(), max_write_bytes)
            .expect("test BLE link registers");
        let (stream, lease, peripheral_id) = hub.claim_link().expect("test BLE link is claimable");
        assert_eq!(peripheral_id, "test-peripheral");
        (hub, generation, stream, lease)
    }

    fn write_command(hub: &BleHub, generation: u64) -> (u64, Vec<u8>) {
        match hub
            .next_platform_command(generation, Duration::from_secs(1))
            .expect("platform command wait succeeds")
            .expect("write command arrives")
        {
            NativeBlePlatformCommand::Write { token, bytes, .. } => (token, bytes),
            NativeBlePlatformCommand::Disconnect { reason, .. } => {
                panic!("expected write command, got disconnect: {reason}")
            }
        }
    }

    #[test]
    fn write_does_not_advance_until_platform_acknowledges_and_is_fragment_bounded() {
        let (hub, generation, mut stream, _lease) = claimed_hub(BleHubConfig::default(), 20);
        let (result_sender, result_receiver) = mpsc::channel();
        let writer = thread::spawn(move || {
            result_sender
                .send(stream.write(&[0x5a; 25]))
                .expect("test result receiver remains");
        });

        let (token, bytes) = write_command(&hub, generation);
        assert_eq!(bytes, vec![0x5a; 20]);
        assert!(
            result_receiver
                .recv_timeout(Duration::from_millis(20))
                .is_err(),
            "Rust write must remain pending before platform acknowledgement"
        );

        hub.write_succeeded(generation, token)
            .expect("matching platform acknowledgement succeeds");
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("writer wakes after acknowledgement")
                .expect("acknowledged write succeeds"),
            20
        );
        writer.join().expect("writer exits");
    }

    #[test]
    fn platform_write_capability_never_enlarges_the_fixed_gatt_profile_value() {
        let (hub, generation, mut stream, _lease) = claimed_hub(BleHubConfig::default(), 512);
        let writer = thread::spawn(move || stream.write(&[0x5a; 512]));

        let (token, bytes) = write_command(&hub, generation);
        assert_eq!(bytes, vec![0x5a; INITIAL_ATT_VALUE_BYTES]);
        hub.write_succeeded(generation, token)
            .expect("profile-bounded write acknowledgement succeeds");
        assert_eq!(
            writer
                .join()
                .expect("writer exits")
                .expect("profile-bounded write succeeds"),
            INITIAL_ATT_VALUE_BYTES
        );
    }

    #[test]
    fn acknowledgement_timeout_never_redelivers_ambiguous_bytes() {
        let config = BleHubConfig {
            io_slice: Duration::from_secs(1),
            write_ack_timeout: Duration::from_millis(40),
            inbound_capacity: 64,
        };
        let (hub, generation, mut stream, _lease) = claimed_hub(config, 20);
        let writer = thread::spawn(move || stream.write(&[0xa5; 20]));
        let (token, bytes) = write_command(&hub, generation);
        assert_eq!(bytes, vec![0xa5; 20]);
        assert_eq!(
            hub.next_platform_command(generation, Duration::from_millis(10))
                .expect("second platform poll succeeds"),
            None,
            "a delivered write command is never duplicated"
        );

        let error = writer
            .join()
            .expect("writer exits")
            .expect_err("unacknowledged write fails");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert!(matches!(
            hub.next_platform_command(generation, Duration::from_secs(1))
                .expect("disconnect command is available"),
            Some(NativeBlePlatformCommand::Disconnect { .. })
        ));
        assert!(matches!(
            hub.write_succeeded(generation, token),
            Err(NativeBleError::LinkClosed { .. })
        ));
    }

    #[test]
    fn platform_disconnect_wakes_a_reader_with_eof() {
        let config = BleHubConfig {
            io_slice: Duration::from_secs(5),
            ..BleHubConfig::default()
        };
        let (hub, generation, mut stream, _lease) = claimed_hub(config, 20);
        let reader = thread::spawn(move || {
            let mut byte = [0_u8; 1];
            stream.read(&mut byte)
        });

        hub.disconnected(generation, "platform link closed".to_owned())
            .expect("platform disconnect is accepted");
        let error = reader
            .join()
            .expect("reader exits")
            .expect_err("disconnect is EOF");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn platform_disconnect_wakes_an_unacknowledged_write() {
        let (hub, generation, mut stream, _lease) = claimed_hub(BleHubConfig::default(), 20);
        let writer = thread::spawn(move || stream.write(&[0x77; 20]));
        let (_token, _bytes) = write_command(&hub, generation);

        hub.disconnected(generation, "radio left range".to_owned())
            .expect("platform disconnect is accepted");
        let error = writer
            .join()
            .expect("writer exits")
            .expect_err("pending write fails");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn stale_generations_are_rejected_after_explicit_replacement() {
        let hub = BleHub::new();
        let first = hub
            .link_connected("first".to_owned(), 20)
            .expect("first generation registers");
        assert!(matches!(
            hub.link_connected("replacement-too-early".to_owned(), 20),
            Err(NativeBleError::LinkAlreadyActive { generation }) if generation == first
        ));
        hub.disconnected(first, "first link was closed".to_owned())
            .expect("first generation closes");
        let second = hub
            .link_connected("second".to_owned(), 20)
            .expect("replacement generation registers");
        assert_ne!(first, second);

        for error in [
            hub.ingest_indication(first, vec![1]).unwrap_err(),
            hub.next_platform_command(first, Duration::ZERO)
                .unwrap_err(),
            hub.write_succeeded(first, 1).unwrap_err(),
            hub.disconnected(first, "late callback".to_owned())
                .unwrap_err(),
        ] {
            assert_eq!(
                error,
                NativeBleError::StaleGeneration {
                    requested: first,
                    active_generation: Some(second),
                }
            );
        }
    }

    #[test]
    fn dropping_a_stale_session_lease_cannot_close_its_registered_replacement() {
        let hub = BleHub::new();
        let first = hub
            .link_connected("first".to_owned(), 20)
            .expect("first generation registers");
        let (_first_stream, first_lease, _) =
            hub.claim_link().expect("first generation is claimed");
        hub.disconnected(first, "first physical link closed".to_owned())
            .expect("first generation closes");

        let second = hub
            .link_connected("second".to_owned(), 20)
            .expect("replacement generation registers");
        drop(first_lease);

        let (_second_stream, _second_lease, peripheral_id) = hub
            .claim_link()
            .expect("stale lease drop cannot close the replacement");
        assert_eq!(peripheral_id, "second");
        assert_ne!(first, second);
    }

    #[test]
    fn inbound_queue_overflow_closes_the_generation_and_requests_teardown() {
        let config = BleHubConfig {
            io_slice: Duration::from_secs(1),
            write_ack_timeout: Duration::from_secs(1),
            inbound_capacity: 4,
        };
        let (hub, generation, mut stream, _lease) = claimed_hub(config, 20);
        assert_eq!(
            hub.ingest_indication(generation, vec![1, 2, 3, 4, 5]),
            Err(NativeBleError::QueueOverflow {
                generation,
                capacity: 4,
            })
        );
        assert!(matches!(
            hub.next_platform_command(generation, Duration::from_secs(1))
                .expect("disconnect command is available"),
            Some(NativeBlePlatformCommand::Disconnect { .. })
        ));
        let mut byte = [0_u8; 1];
        assert_eq!(
            stream
                .read(&mut byte)
                .expect_err("overflowed stream is closed")
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn dropping_the_session_lease_yields_one_platform_disconnect_command() {
        let (hub, generation, mut stream, lease) = claimed_hub(BleHubConfig::default(), 20);
        drop(lease);

        assert!(matches!(
            hub.next_platform_command(generation, Duration::from_secs(1))
                .expect("lease teardown command is available"),
            Some(NativeBlePlatformCommand::Disconnect {
                generation: actual,
                ..
            }) if actual == generation
        ));
        assert!(matches!(
            hub.next_platform_command(generation, Duration::ZERO),
            Err(NativeBleError::LinkClosed { .. })
        ));
        let mut byte = [0_u8; 1];
        assert_eq!(
            stream
                .read(&mut byte)
                .expect_err("released stream is closed")
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn platform_limits_below_the_shared_initial_att_value_are_rejected() {
        let hub = BleHub::new();
        assert!(matches!(
            hub.link_connected("too-small".to_owned(), 19),
            Err(NativeBleError::InvalidArgument { .. })
        ));
        assert!(matches!(
            hub.link_connected("too-large".to_owned(), 513),
            Err(NativeBleError::InvalidArgument { .. })
        ));
    }

    #[test]
    fn connector_authenticates_with_the_ble_bound_suite_over_platform_callbacks() {
        let credential = TestCredential::new();
        let hub = BleHub::new();
        let generation = hub
            .link_connected("fake-ios-peripheral".to_owned(), 20)
            .expect("fake platform link registers");
        let server_hub = Arc::clone(&hub);
        let server = thread::spawn(move || serve_ble_handshake(&server_hub, generation));

        let mut connector = BleConnector::new(credential.0.clone(), Arc::clone(&hub));
        let connected = connector
            .connect()
            .expect("native BLE connector authenticates");
        drop(connected);

        server.join().expect("fake BLE platform exits");
    }

    #[test]
    fn missing_or_malformed_credential_releases_the_claimed_platform_link() {
        for credential in [TestCredential::absent(), TestCredential::malformed()] {
            let hub = BleHub::new();
            let generation = hub
                .link_connected("credential-preflight".to_owned(), 20)
                .expect("fake platform link registers");
            let mut connector = BleConnector::new(credential.0.clone(), Arc::clone(&hub));

            assert!(matches!(
                connector.connect(),
                Err(ConnectFailure::Permanent(_))
            ));
            assert!(matches!(
                hub.next_platform_command(generation, Duration::from_secs(1))
                    .expect("credential failure schedules teardown"),
                Some(NativeBlePlatformCommand::Disconnect {
                    generation: actual,
                    ..
                }) if actual == generation
            ));
            assert!(matches!(
                hub.link_connected("replacement-too-early".to_owned(), 20),
                Err(NativeBleError::LinkAlreadyActive {
                    generation: actual
                }) if actual == generation
            ));
            hub.disconnected(generation, "credential failure drained".to_owned())
                .expect("platform confirms credential-failure teardown");
            assert!(
                hub.link_connected("replacement-after-drain".to_owned(), 20)
                    .is_ok()
            );
        }
    }

    fn serve_ble_handshake(hub: &BleHub, generation: u64) {
        let hello = ClientHello::from_record(read_client_record(hub, generation))
            .expect("client sends a canonical hello");
        assert_eq!(hello.suite(), SessionSuite::BleGattQualification);
        assert_eq!(hello.bearer(), BearerBinding::BleGatt);

        let mut epochs = SessionEpochAllocator::new();
        let mut rng = FixedRng(0x88);
        let mut hello_flight = ServerHelloFlight::begin(
            hello,
            ActiveCredential::new(
                CredentialId::new(CREDENTIAL_BYTES),
                CredentialGeneration::new(CREDENTIAL_GENERATION),
                PSK,
            ),
            ServerParameters::new_for_suite(
                DeviceId::new(DEVICE_BYTES),
                BearerBinding::BleGatt,
                SessionSuite::BleGattQualification,
            ),
            &mut epochs,
            &mut rng,
        )
        .expect("server handshake begins");
        ingest_partial(hub, generation, hello_flight.remaining());
        let hello_length = hello_flight.remaining().len();
        hello_flight
            .advance(hello_length)
            .expect("server hello advances");
        let mut proof_flight = hello_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("complete server hello finishes"));
        ingest_partial(hub, generation, proof_flight.remaining());
        let proof_length = proof_flight.remaining().len();
        proof_flight
            .advance(proof_length)
            .expect("server proof advances");
        let pending = proof_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("complete server proof finishes"));
        let session = pending
            .authenticate(read_client_record(hub, generation))
            .expect("client proof authenticates");
        drop(session);

        assert!(matches!(
            hub.next_platform_command(generation, Duration::from_secs(1))
                .expect("lease teardown command arrives"),
            Some(NativeBlePlatformCommand::Disconnect { .. })
        ));
    }

    fn read_client_record(hub: &BleHub, generation: u64) -> Record {
        let mut decoder = StreamDecoder::new();
        loop {
            let (token, bytes) = write_command(hub, generation);
            let mut complete = None;
            for byte in bytes {
                match decoder.push(byte) {
                    DecodeEvent::Pending => {}
                    DecodeEvent::Record(record) => {
                        assert!(complete.replace(record).is_none());
                    }
                    DecodeEvent::MalformedCobs
                    | DecodeEvent::MalformedRecord(_)
                    | DecodeEvent::Overflow => panic!("client sent malformed framing"),
                }
            }
            hub.write_succeeded(generation, token)
                .expect("fake platform acknowledges client bytes");
            if let Some(record) = complete {
                return record;
            }
        }
    }

    fn ingest_partial(hub: &BleHub, generation: u64, bytes: &[u8]) {
        for chunk in bytes.chunks(3) {
            hub.ingest_indication(generation, chunk.to_vec())
                .expect("fake indication is accepted");
        }
    }
}
