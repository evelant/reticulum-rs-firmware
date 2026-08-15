//! Bounded synchronous adapter from the E290 BLE GATT profile to `DeviceClient`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{
        Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
#[cfg(target_os = "macos")]
use std::{
    sync::{Arc, mpsc},
    thread,
};

#[cfg(target_os = "macos")]
use btleplug::{
    api::{
        Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter,
        WriteType,
    },
    platform::{Adapter, Manager, Peripheral},
};
#[cfg(target_os = "macos")]
use futures_util::StreamExt;
#[cfg(target_os = "macos")]
use reticulum_device_api_ble::{LOCAL_NAME_PREFIX, RX_UUID_U128, SERVICE_UUID_U128, TX_UUID_U128};
use reticulum_device_api_ble::{
    MAXIMUM_ATT_VALUE_BYTES, MINIMUM_ATT_VALUE_BYTES as SAFE_ATT_VALUE_BYTES,
};
#[cfg(target_os = "macos")]
use tokio::{
    runtime::Builder,
    sync::{mpsc as async_mpsc, oneshot},
    time,
};
#[cfg(target_os = "macos")]
use uuid::Uuid;

mod browser;

pub use browser::{BrowserBleTransport, BrowserBridgeConfig};

#[cfg(target_os = "macos")]
const COMMAND_CAPACITY: usize = 1;
const MAX_RX_BYTES: usize = 64 * 1024;
const MAX_WRITE_CALL_BYTES: usize = 1_024;

/// BLE discovery, I/O, and selection policy for one physical qualification.
#[derive(Clone, Debug)]
pub struct BleConfig {
    /// Optional opaque platform peripheral ID.
    pub peripheral_id: Option<String>,
    /// Optional suffix of the stable `reticulum-e290-*` local name.
    pub name_suffix: Option<String>,
    /// Time allowed for one service-filtered scan.
    pub scan_timeout: Duration,
    /// Time allowed for each CoreBluetooth operation.
    pub operation_timeout: Duration,
    /// Finite synchronous read polling interval.
    pub io_slice: Duration,
}

impl Default for BleConfig {
    fn default() -> Self {
        Self {
            peripheral_id: None,
            name_suffix: None,
            scan_timeout: Duration::from_secs(8),
            operation_timeout: Duration::from_secs(5),
            io_slice: Duration::from_millis(50),
        }
    }
}

/// Advertising metadata for the selected physical peripheral.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedPeripheral {
    /// Opaque platform peripheral identifier.
    pub id: String,
    /// Last observed local name.
    pub local_name: Option<String>,
    /// Last observed RSSI in dBm.
    pub rssi: Option<i16>,
}

/// Monotonic byte and fragment counters from one BLE connection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransportStats {
    /// Write-with-response fragments acknowledged by the platform.
    pub tx_fragments: u64,
    /// Ordered bytes acknowledged by the platform.
    pub tx_bytes: u64,
    /// TX-characteristic indications accepted into the receive queue.
    pub rx_indications: u64,
    /// Ordered indication bytes accepted into the receive queue.
    pub rx_bytes: u64,
}

#[derive(Default)]
struct AtomicStats {
    tx_fragments: AtomicU64,
    tx_bytes: AtomicU64,
    rx_indications: AtomicU64,
    rx_bytes: AtomicU64,
}

impl AtomicStats {
    fn snapshot(&self) -> TransportStats {
        TransportStats {
            tx_fragments: self.tx_fragments.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            rx_indications: self.rx_indications.load(Ordering::Relaxed),
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
        }
    }
}

#[derive(Default)]
struct RxState {
    bytes: VecDeque<u8>,
    terminal: Option<String>,
}

#[derive(Default)]
struct SharedRx {
    state: Mutex<RxState>,
    ready: Condvar,
}

impl SharedRx {
    fn push(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_ATT_VALUE_BYTES {
            return Err(format!(
                "TX indication length {} is outside 1..={MAXIMUM_ATT_VALUE_BYTES}",
                bytes.len()
            ));
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.terminal.is_some() {
            return Err("BLE receive path is already terminal".to_owned());
        }
        if state.bytes.len().saturating_add(bytes.len()) > MAX_RX_BYTES {
            return Err(format!(
                "BLE receive queue exceeded its {MAX_RX_BYTES}-byte bound"
            ));
        }
        state.bytes.extend(bytes.iter().copied());
        self.ready.notify_all();
        Ok(())
    }

    fn terminate(&self, reason: impl Into<String>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.terminal.is_none() {
            state.terminal = Some(reason.into());
        }
        self.ready.notify_all();
    }

    fn read(&self, output: &mut [u8], timeout: Duration) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (mut state, _) = self
            .ready
            .wait_timeout_while(state, timeout, |state| {
                state.bytes.is_empty() && state.terminal.is_none()
            })
            .unwrap_or_else(|poison| poison.into_inner());
        if state.bytes.is_empty() {
            return match &state.terminal {
                Some(reason) => Err(io::Error::new(io::ErrorKind::BrokenPipe, reason.clone())),
                None => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no BLE indication arrived during the finite I/O slice",
                )),
            };
        }
        let count = output.len().min(state.bytes.len());
        for byte in &mut output[..count] {
            *byte = state
                .bytes
                .pop_front()
                .expect("bounded count cannot exceed queued bytes");
        }
        Ok(count)
    }
}

#[cfg(target_os = "macos")]
enum WorkerCommand {
    Write {
        bytes: Vec<u8>,
        reply: mpsc::SyncSender<Result<usize, String>>,
    },
}

/// A finite, bounded synchronous byte stream backed by one BLE GATT connection.
#[cfg(target_os = "macos")]
pub struct BleTransport {
    commands: async_mpsc::Sender<WorkerCommand>,
    shutdown: Option<oneshot::Sender<mpsc::SyncSender<()>>>,
    receive: Arc<SharedRx>,
    stats: Arc<AtomicStats>,
    io_slice: Duration,
    operation_timeout: Duration,
    worker: Option<thread::JoinHandle<()>>,
}

#[cfg(target_os = "macos")]
impl BleTransport {
    /// Scan, uniquely select, connect, discover, and subscribe to an E290.
    pub fn connect(config: BleConfig) -> Result<(Self, SelectedPeripheral), String> {
        validate_config(&config)?;
        let receive = Arc::new(SharedRx::default());
        let stats = Arc::new(AtomicStats::default());
        let (commands, command_rx) = async_mpsc::channel(COMMAND_CAPACITY);
        let (shutdown, shutdown_rx) = oneshot::channel::<mpsc::SyncSender<()>>();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_receive = Arc::clone(&receive);
        let worker_stats = Arc::clone(&stats);
        let worker_config = config.clone();
        let worker = thread::Builder::new()
            .name("e290-ble-api".to_owned())
            .spawn(move || {
                let runtime = match Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let reason = format!("could not create BLE async runtime: {error}");
                        let _ = ready_tx.send(Err(reason.clone()));
                        worker_receive.terminate(reason);
                        return;
                    }
                };
                runtime.block_on(run_worker(
                    worker_config,
                    command_rx,
                    shutdown_rx,
                    ready_tx,
                    worker_receive,
                    worker_stats,
                ));
            })
            .map_err(|error| format!("could not spawn BLE worker: {error}"))?;

        let init_timeout = config
            .scan_timeout
            .saturating_add(config.operation_timeout.saturating_mul(12))
            .saturating_add(Duration::from_secs(1));
        let selected = match ready_rx.recv_timeout(init_timeout) {
            Ok(Ok(selected)) => selected,
            Ok(Err(error)) => {
                let _ = worker.join();
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                receive.terminate("BLE initialization exceeded its host deadline");
                return Err("BLE initialization exceeded its host deadline".to_owned());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                return Err("BLE worker ended before reporting initialization".to_owned());
            }
        };

        Ok((
            Self {
                commands,
                shutdown: Some(shutdown),
                receive,
                stats,
                io_slice: config.io_slice,
                operation_timeout: config.operation_timeout,
                worker: Some(worker),
            },
            selected,
        ))
    }

    /// Snapshot acknowledged BLE traffic counters.
    pub fn stats(&self) -> TransportStats {
        self.stats.snapshot()
    }

    /// Unsubscribe, disconnect, and join the platform worker.
    pub fn disconnect(mut self) -> Result<TransportStats, String> {
        let result = self.shutdown();
        let stats = self.stats();
        result.map(|()| stats)
    }

    fn shutdown(&mut self) -> Result<(), String> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let signaled = self
            .shutdown
            .take()
            .is_some_and(|shutdown| shutdown.send(reply_tx).is_ok());
        let acknowledged = signaled && reply_rx.recv_timeout(self.operation_timeout).is_ok();
        worker
            .join()
            .map_err(|_| "BLE worker panicked during shutdown".to_owned())?;
        if acknowledged {
            Ok(())
        } else {
            Err("BLE worker did not acknowledge shutdown".to_owned())
        }
    }
}

#[cfg(target_os = "macos")]
impl Read for BleTransport {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.receive.read(output, self.io_slice)
    }
}

#[cfg(target_os = "macos")]
impl Write for BleTransport {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        if input.len() > MAX_WRITE_CALL_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "one BLE stream write is {} bytes; maximum is {MAX_WRITE_CALL_BYTES}",
                    input.len()
                ),
            ));
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commands
            .try_send(WorkerCommand::Write {
                bytes: input.to_vec(),
                reply: reply_tx,
            })
            .map_err(|error| match error {
                async_mpsc::error::TrySendError::Full(_) => io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "BLE command queue is temporarily full",
                ),
                async_mpsc::error::TrySendError::Closed(_) => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "BLE worker is not running")
                }
            })?;

        // Once enqueued, never return a retryable timeout: doing so could make
        // DeviceClient replay bytes whose ATT acknowledgement was ambiguous.
        let host_deadline = self
            .operation_timeout
            .saturating_mul(fragment_count(input.len()) as u32)
            .saturating_add(Duration::from_secs(1));
        await_write_reply(reply_rx, host_deadline, &self.receive)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl Drop for BleTransport {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Portable diagnostic stub for the native adapter on hosts where this tool
/// intentionally does not link a platform BLE backend.
#[cfg(not(target_os = "macos"))]
pub struct BleTransport;

#[cfg(not(target_os = "macos"))]
impl BleTransport {
    /// Report that the direct CoreBluetooth adapter is macOS-only.
    pub fn connect(_config: BleConfig) -> Result<(Self, SelectedPeripheral), String> {
        Err(
            "the direct BLE adapter is available only on macOS; run this qualifier with --browser and use a Web Bluetooth-capable browser"
                .to_owned(),
        )
    }

    /// Return empty counters; a non-macOS native transport cannot connect.
    pub fn stats(&self) -> TransportStats {
        TransportStats::default()
    }

    /// Return empty counters; a non-macOS native transport cannot connect.
    pub fn disconnect(self) -> Result<TransportStats, String> {
        Ok(TransportStats::default())
    }
}

#[cfg(not(target_os = "macos"))]
impl Read for BleTransport {
    fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the native BLE adapter is macOS-only; use --browser",
        ))
    }
}

#[cfg(not(target_os = "macos"))]
impl Write for BleTransport {
    fn write(&mut self, _input: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the native BLE adapter is macOS-only; use --browser",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn validate_config(config: &BleConfig) -> Result<(), String> {
    if config.scan_timeout.is_zero() {
        return Err("scan timeout must be positive".to_owned());
    }
    if config.operation_timeout.is_zero() {
        return Err("operation timeout must be positive".to_owned());
    }
    if config.io_slice.is_zero() {
        return Err("I/O slice must be positive".to_owned());
    }
    if config.peripheral_id.as_deref().is_some_and(str::is_empty) {
        return Err("peripheral ID selector must not be empty".to_owned());
    }
    if config.name_suffix.as_deref().is_some_and(str::is_empty) {
        return Err("name suffix selector must not be empty".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn run_worker(
    config: BleConfig,
    mut commands: async_mpsc::Receiver<WorkerCommand>,
    mut shutdown: oneshot::Receiver<mpsc::SyncSender<()>>,
    ready: mpsc::SyncSender<Result<SelectedPeripheral, String>>,
    receive: Arc<SharedRx>,
    stats: Arc<AtomicStats>,
) {
    let initialized = initialize(&config).await;
    let (adapter, peripheral, rx, tx, selected, mut notifications) = match initialized {
        Ok(initialized) => initialized,
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            receive.terminate(error);
            return;
        }
    };
    if ready.send(Ok(selected)).is_err() {
        cleanup(&peripheral, &tx, config.operation_timeout).await;
        receive.terminate("BLE initializer was abandoned");
        return;
    }

    let mut connection_poll = time::interval(Duration::from_millis(500));
    connection_poll.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    let mut terminal_reason = "BLE worker shut down".to_owned();
    loop {
        tokio::select! {
            biased;
            shutdown = &mut shutdown => {
                if let Ok(reply) = shutdown {
                    let _ = reply.send(());
                }
                break;
            }
            command = commands.recv() => {
                match command {
                    Some(WorkerCommand::Write { bytes, reply }) => {
                        let write = write_fragments(
                            &peripheral,
                            &rx,
                            &bytes,
                            config.operation_timeout,
                            &stats,
                        );
                        tokio::pin!(write);
                        tokio::select! {
                            biased;
                            shutdown = &mut shutdown => {
                                terminal_reason =
                                    "BLE shutdown interrupted a write; acknowledgement is ambiguous"
                                        .to_owned();
                                let _ = reply.send(Err(terminal_reason.clone()));
                                if let Ok(shutdown_reply) = shutdown {
                                    let _ = shutdown_reply.send(());
                                }
                                break;
                            }
                            result = &mut write => {
                                match result {
                                    Ok(written) => {
                                        let _ = reply.send(Ok(written));
                                    }
                                    Err(error) => {
                                        let _ = reply.send(Err(error.clone()));
                                        terminal_reason = error;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    None => break,
                }
            }
            notification = notifications.next() => {
                match notification {
                    Some(notification) if notification.uuid == tx.uuid => {
                        if let Err(error) = receive.push(&notification.value) {
                            terminal_reason = error;
                            break;
                        }
                        stats.rx_indications.fetch_add(1, Ordering::Relaxed);
                        stats.rx_bytes.fetch_add(notification.value.len() as u64, Ordering::Relaxed);
                    }
                    Some(_) => {}
                    None => {
                        terminal_reason = "BLE notification stream ended".to_owned();
                        break;
                    }
                }
            }
            _ = connection_poll.tick() => {
                match deadline(
                    "checking BLE connection",
                    config.operation_timeout,
                    peripheral.is_connected(),
                ).await {
                    Ok(true) => {}
                    Ok(false) => {
                        terminal_reason = "BLE peripheral disconnected".to_owned();
                        break;
                    }
                    Err(error) => {
                        terminal_reason = error;
                        break;
                    }
                }
            }
        }
    }
    cleanup(&peripheral, &tx, config.operation_timeout).await;
    let _ = time::timeout(config.operation_timeout, adapter.stop_scan()).await;
    receive.terminate(terminal_reason);
}

#[cfg(target_os = "macos")]
type Initialized = (
    Adapter,
    Peripheral,
    Characteristic,
    Characteristic,
    SelectedPeripheral,
    std::pin::Pin<Box<dyn futures_util::Stream<Item = btleplug::api::ValueNotification> + Send>>,
);

#[cfg(target_os = "macos")]
async fn initialize(config: &BleConfig) -> Result<Initialized, String> {
    let manager = deadline(
        "creating BLE manager",
        config.operation_timeout,
        Manager::new(),
    )
    .await?;
    let mut adapters = deadline(
        "enumerating BLE adapters",
        config.operation_timeout,
        manager.adapters(),
    )
    .await?;
    let adapter = adapters
        .drain(..)
        .next()
        .ok_or_else(|| "no BLE adapter is available".to_owned())?;
    let service_uuid = Uuid::from_u128(SERVICE_UUID_U128);
    deadline(
        "starting BLE scan",
        config.operation_timeout,
        adapter.start_scan(ScanFilter {
            services: vec![service_uuid],
        }),
    )
    .await?;
    time::sleep(config.scan_timeout).await;
    deadline(
        "stopping BLE scan",
        config.operation_timeout,
        adapter.stop_scan(),
    )
    .await?;

    let peripherals = deadline(
        "enumerating BLE peripherals",
        config.operation_timeout,
        adapter.peripherals(),
    )
    .await?;
    let (peripheral, selected) = deadline(
        "selecting BLE peripheral",
        config.operation_timeout.saturating_mul(2),
        select_peripheral(peripherals, config),
    )
    .await?;
    deadline(
        "connecting BLE peripheral",
        config.operation_timeout,
        peripheral.connect(),
    )
    .await?;
    if let Err(error) = deadline(
        "discovering BLE services",
        config.operation_timeout,
        peripheral.discover_services(),
    )
    .await
    {
        let _ = peripheral.disconnect().await;
        return Err(error);
    }

    let initialized = initialize_gatt(&peripheral, config).await;
    let (rx, tx, notifications) = match initialized {
        Ok(initialized) => initialized,
        Err(error) => {
            let _ = time::timeout(config.operation_timeout, peripheral.disconnect()).await;
            return Err(error);
        }
    };
    Ok((adapter, peripheral, rx, tx, selected, notifications))
}

#[cfg(target_os = "macos")]
type NotificationStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = btleplug::api::ValueNotification> + Send>>;

#[cfg(target_os = "macos")]
async fn initialize_gatt(
    peripheral: &Peripheral,
    config: &BleConfig,
) -> Result<(Characteristic, Characteristic, NotificationStream), String> {
    let service_uuid = Uuid::from_u128(SERVICE_UUID_U128);
    let rx_uuid = Uuid::from_u128(RX_UUID_U128);
    let tx_uuid = Uuid::from_u128(TX_UUID_U128);
    let characteristics = peripheral.characteristics();
    let rx = characteristics
        .iter()
        .find(|characteristic| {
            characteristic.service_uuid == service_uuid && characteristic.uuid == rx_uuid
        })
        .cloned()
        .ok_or_else(|| format!("service {service_uuid} is missing RX characteristic {rx_uuid}"))?;
    let tx = characteristics
        .iter()
        .find(|characteristic| {
            characteristic.service_uuid == service_uuid && characteristic.uuid == tx_uuid
        })
        .cloned()
        .ok_or_else(|| format!("service {service_uuid} is missing TX characteristic {tx_uuid}"))?;
    if !rx.properties.contains(CharPropFlags::WRITE) {
        return Err("BLE RX characteristic does not support write-with-response".to_owned());
    }
    if !tx.properties.contains(CharPropFlags::INDICATE) {
        return Err("BLE TX characteristic does not support indications".to_owned());
    }
    let notifications = deadline(
        "opening BLE notification stream",
        config.operation_timeout,
        peripheral.notifications(),
    )
    .await?;
    deadline(
        "subscribing to BLE TX indications",
        config.operation_timeout,
        peripheral.subscribe(&tx),
    )
    .await?;
    Ok((rx, tx, notifications))
}

#[cfg(target_os = "macos")]
async fn select_peripheral(
    peripherals: Vec<Peripheral>,
    config: &BleConfig,
) -> Result<(Peripheral, SelectedPeripheral), String> {
    let mut candidates = Vec::new();
    for peripheral in peripherals {
        let id = peripheral.id().to_string();
        let properties = match deadline(
            "reading BLE advertising properties",
            config.operation_timeout,
            peripheral.properties(),
        )
        .await
        {
            Ok(properties) => properties,
            Err(_) => continue,
        };
        let local_name = properties
            .as_ref()
            .and_then(|value| value.local_name.clone());
        let rssi = properties.as_ref().and_then(|value| value.rssi);
        let selected = candidate_matches(
            &id,
            local_name.as_deref(),
            config.peripheral_id.as_deref(),
            config.name_suffix.as_deref(),
        );
        if selected {
            candidates.push((
                peripheral,
                SelectedPeripheral {
                    id,
                    local_name,
                    rssi,
                },
            ));
        }
    }
    match candidates.len() {
        1 => Ok(candidates
            .pop()
            .expect("one selected BLE candidate must be present")),
        0 => Err("no E290 BLE API peripheral matched the selectors".to_owned()),
        _ => {
            let summary = candidates
                .iter()
                .map(|(_, selected)| {
                    format!(
                        "{} ({})",
                        selected.id,
                        selected.local_name.as_deref().unwrap_or("unnamed")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "more than one E290 BLE API peripheral matched; select one explicitly: {summary}"
            ))
        }
    }
}

#[cfg(target_os = "macos")]
fn candidate_matches(
    id: &str,
    local_name: Option<&str>,
    peripheral_id: Option<&str>,
    name_suffix: Option<&str>,
) -> bool {
    if peripheral_id.is_some_and(|expected| id != expected) {
        return false;
    }
    let prefix = str::from_utf8(LOCAL_NAME_PREFIX).expect("profile name prefix is ASCII");
    let named_e290 = local_name.is_some_and(|name| name.starts_with(prefix));
    if peripheral_id.is_none() && !named_e290 {
        return false;
    }
    name_suffix.is_none_or(|suffix| {
        local_name.is_some_and(|name| {
            name.to_ascii_lowercase()
                .ends_with(&suffix.to_ascii_lowercase())
        })
    })
}

#[cfg(target_os = "macos")]
async fn write_fragments(
    peripheral: &Peripheral,
    rx: &Characteristic,
    bytes: &[u8],
    operation_timeout: Duration,
    stats: &AtomicStats,
) -> Result<usize, String> {
    for fragment in bytes.chunks(SAFE_ATT_VALUE_BYTES) {
        deadline(
            "writing BLE RX fragment with response",
            operation_timeout,
            peripheral.write(rx, fragment, WriteType::WithResponse),
        )
        .await?;
        stats.tx_fragments.fetch_add(1, Ordering::Relaxed);
        stats
            .tx_bytes
            .fetch_add(fragment.len() as u64, Ordering::Relaxed);
    }
    Ok(bytes.len())
}

#[cfg(target_os = "macos")]
async fn cleanup(peripheral: &Peripheral, tx: &Characteristic, operation_timeout: Duration) {
    let _ = time::timeout(operation_timeout, peripheral.unsubscribe(tx)).await;
    let _ = time::timeout(operation_timeout, peripheral.disconnect()).await;
}

#[cfg(target_os = "macos")]
async fn deadline<T, E>(
    stage: &'static str,
    timeout: Duration,
    future: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, String>
where
    E: std::fmt::Display,
{
    match time::timeout(timeout, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(format!("{stage} failed: {error}")),
        Err(_) => Err(format!("{stage} exceeded {timeout:?}")),
    }
}

#[cfg(target_os = "macos")]
const fn fragment_count(length: usize) -> usize {
    length.div_ceil(SAFE_ATT_VALUE_BYTES)
}

#[cfg(target_os = "macos")]
fn ambiguous_write_error(receive: &SharedRx) -> io::Error {
    receive.terminate("BLE write result missed its terminal host deadline");
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "BLE write acknowledgement became ambiguous; connection is terminal",
    )
}

#[cfg(target_os = "macos")]
fn await_write_reply(
    reply: mpsc::Receiver<Result<usize, String>>,
    host_deadline: Duration,
    receive: &SharedRx,
) -> io::Result<usize> {
    match reply.recv_timeout(host_deadline) {
        Ok(Ok(written)) => Ok(written),
        Ok(Err(reason)) => Err(io::Error::new(io::ErrorKind::BrokenPipe, reason)),
        Err(_) => Err(ambiguous_write_error(receive)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn candidate_policy_requires_e290_name_unless_id_is_explicit() {
        assert!(candidate_matches(
            "opaque-a",
            Some("reticulum-e290-e13e88"),
            None,
            None
        ));
        assert!(!candidate_matches(
            "opaque-a",
            Some("other-device"),
            None,
            None
        ));
        assert!(candidate_matches(
            "opaque-a",
            Some("other-device"),
            Some("opaque-a"),
            None
        ));
        assert!(!candidate_matches(
            "opaque-a",
            Some("reticulum-e290-e13e88"),
            Some("opaque-b"),
            None
        ));
        assert!(candidate_matches(
            "opaque-a",
            Some("reticulum-e290-e13e88"),
            None,
            Some("E13E88")
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fragment_policy_covers_every_boundary_without_empty_fragments() {
        assert_eq!(fragment_count(1), 1);
        assert_eq!(fragment_count(SAFE_ATT_VALUE_BYTES), 1);
        assert_eq!(fragment_count(SAFE_ATT_VALUE_BYTES + 1), 2);
        let input = vec![0x5a; SAFE_ATT_VALUE_BYTES * 2 + 7];
        let chunks = input.chunks(SAFE_ATT_VALUE_BYTES).collect::<Vec<_>>();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat(), input);
        assert!(chunks.iter().all(|chunk| !chunk.is_empty()));
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.len() <= SAFE_ATT_VALUE_BYTES)
        );
    }

    #[test]
    fn receive_queue_preserves_partial_read_order() {
        let receive = SharedRx::default();
        receive.push(&[1, 2, 3]).unwrap();
        receive.push(&[4, 5]).unwrap();
        let mut first = [0_u8; 2];
        let mut second = [0_u8; 4];
        assert_eq!(
            receive.read(&mut first, Duration::from_millis(1)).unwrap(),
            2
        );
        assert_eq!(
            receive.read(&mut second, Duration::from_millis(1)).unwrap(),
            3
        );
        assert_eq!(first, [1, 2]);
        assert_eq!(&second[..3], &[3, 4, 5]);
    }

    #[test]
    fn receive_queue_rejects_invalid_fragments_and_is_terminal_on_disconnect() {
        let receive = SharedRx::default();
        assert!(receive.push(&[]).is_err());
        assert!(receive.push(&[0_u8; MAXIMUM_ATT_VALUE_BYTES + 1]).is_err());
        receive.terminate("gone");
        let error = receive
            .read(&mut [0_u8; 1], Duration::from_millis(1))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn receive_queue_enforces_byte_bound_without_reordering_existing_data() {
        let receive = SharedRx::default();
        for _ in 0..(MAX_RX_BYTES / MAXIMUM_ATT_VALUE_BYTES) {
            receive.push(&[0x7a; MAXIMUM_ATT_VALUE_BYTES]).unwrap();
        }
        receive
            .push(&[0x7a; MAX_RX_BYTES % MAXIMUM_ATT_VALUE_BYTES])
            .unwrap();
        assert!(receive.push(&[0x7b]).is_err());
        let state = receive
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert_eq!(state.bytes.len(), MAX_RX_BYTES);
        assert!(state.bytes.iter().all(|byte| *byte == 0x7a));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ambiguous_write_ack_is_non_retryable_and_terminates_reads() {
        let receive = SharedRx::default();
        let error = ambiguous_write_error(&receive);
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        let read_error = receive
            .read(&mut [0_u8; 1], Duration::from_millis(1))
            .unwrap_err();
        assert_eq!(read_error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shutdown_bypasses_a_full_write_queue_without_making_retry_safe() {
        let receive = Arc::new(SharedRx::default());
        let assertion_receive = Arc::clone(&receive);
        let stats = Arc::new(AtomicStats::default());
        let (commands, command_rx) = async_mpsc::channel(COMMAND_CAPACITY);
        let (write_reply_tx, write_reply_rx) = mpsc::sync_channel(1);
        commands
            .try_send(WorkerCommand::Write {
                bytes: vec![0x5a],
                reply: write_reply_tx,
            })
            .expect("one write must fill the command queue");
        assert_eq!(commands.capacity(), 0);

        let (shutdown, shutdown_rx) = oneshot::channel::<mpsc::SyncSender<()>>();
        let worker = thread::spawn(move || {
            let runtime = Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime must build");
            runtime.block_on(async move {
                let reply = shutdown_rx.await.expect("shutdown signal must arrive");
                let _ = reply.send(());
                drop(command_rx);
            });
        });
        let transport = BleTransport {
            commands,
            shutdown: Some(shutdown),
            receive,
            stats,
            io_slice: Duration::from_millis(1),
            operation_timeout: Duration::from_millis(100),
            worker: Some(worker),
        };
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = done_tx.send(transport.disconnect());
        });

        let stats = done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown must not wait on the full write queue")
            .expect("out-of-band shutdown must be acknowledged");
        assert_eq!(stats, TransportStats::default());

        let write_error = await_write_reply(
            write_reply_rx,
            Duration::from_millis(10),
            &assertion_receive,
        )
        .expect_err("a write abandoned by shutdown must remain ambiguous");
        assert_eq!(write_error.kind(), io::ErrorKind::BrokenPipe);
        let read_error = assertion_receive
            .read(&mut [0_u8; 1], Duration::from_millis(1))
            .expect_err("ambiguous queued write must terminate the stream");
        assert_eq!(read_error.kind(), io::ErrorKind::BrokenPipe);
    }
}
