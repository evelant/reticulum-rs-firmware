//! Browser-assisted Web Bluetooth transport for hosts where the launching
//! process cannot use the platform BLE API directly.

use std::{
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{
        HeaderMap, StatusCode,
        header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, ORIGIN},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use reticulum_device_api_ble::{
    GATT_PROFILE_MAJOR, GATT_PROFILE_MINOR, INITIAL_ATT_VALUE_BYTES, RX_UUID, SERVICE_UUID, TX_UUID,
};
use serde::Deserialize;
#[cfg(test)]
use serde::Serialize;
use serde_json::json;
use tokio::{
    runtime::Builder,
    sync::{mpsc as async_mpsc, watch},
    time,
};

use super::{AtomicStats, MAX_WRITE_CALL_BYTES, SelectedPeripheral, SharedRx, TransportStats};

const COMMAND_CAPACITY: usize = 1;
const BRIDGE_PROTOCOL_VERSION: u8 = 1;
const FRAME_WRITE: u8 = 1;
const FRAME_INDICATION: u8 = 2;
const FRAME_PREFIX_BYTES: usize = 2;
const WRITE_ID_BYTES: usize = 4;
const MAX_CONTROL_BYTES: usize = 1_024;
const MAX_DEVICE_METADATA_BYTES: usize = 128;
const MAX_ERROR_BYTES: usize = 256;
const MAX_PROOF_BYTES: usize = 16 * 1024;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JAVASCRIPT: &str = include_str!("../web/dist/app.js");

/// Local loopback server and finite wait policy for Web Bluetooth assistance.
#[derive(Clone, Debug)]
pub struct BrowserBridgeConfig {
    /// IPv4 localhost socket used to serve the browser helper.
    pub bind_address: SocketAddr,
    /// Time allowed for a browser to open the page, receive a user click, and
    /// report a fully validated/subscribed GATT connection.
    pub connect_timeout: Duration,
    /// Time allowed for each browser-acknowledged GATT operation.
    pub operation_timeout: Duration,
    /// Finite synchronous read polling interval.
    pub io_slice: Duration,
}

impl Default for BrowserBridgeConfig {
    fn default() -> Self {
        Self {
            bind_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8_329),
            connect_timeout: Duration::from_secs(5 * 60),
            operation_timeout: Duration::from_secs(5),
            io_slice: Duration::from_millis(50),
        }
    }
}

enum WorkerCommand {
    Write {
        bytes: Vec<u8>,
        reply: mpsc::SyncSender<Result<usize, String>>,
    },
    PublishProof {
        json: String,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    Shutdown {
        reply: mpsc::SyncSender<()>,
    },
}

#[derive(Clone, Debug)]
enum ReadyOutcome {
    Waiting,
    Ready(SelectedPeripheral),
    Terminal(String),
}

struct ReadySignal {
    outcome: Mutex<ReadyOutcome>,
    changed: Condvar,
}

impl Default for ReadySignal {
    fn default() -> Self {
        Self {
            outcome: Mutex::new(ReadyOutcome::Waiting),
            changed: Condvar::new(),
        }
    }
}

impl ReadySignal {
    fn ready(&self, selected: SelectedPeripheral) -> Result<(), String> {
        let mut outcome = self
            .outcome
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !matches!(*outcome, ReadyOutcome::Waiting) {
            return Err("browser bridge reported readiness more than once".to_owned());
        }
        *outcome = ReadyOutcome::Ready(selected);
        self.changed.notify_all();
        Ok(())
    }

    fn terminate(&self, reason: impl Into<String>) {
        let mut outcome = self
            .outcome
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if matches!(*outcome, ReadyOutcome::Waiting) {
            *outcome = ReadyOutcome::Terminal(reason.into());
        }
        self.changed.notify_all();
    }

    fn wait(&self, timeout: Duration) -> Result<SelectedPeripheral, String> {
        let outcome = self
            .outcome
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (outcome, wait) = self
            .changed
            .wait_timeout_while(outcome, timeout, |outcome| {
                matches!(outcome, ReadyOutcome::Waiting)
            })
            .unwrap_or_else(|poison| poison.into_inner());
        match &*outcome {
            ReadyOutcome::Ready(selected) => Ok(selected.clone()),
            ReadyOutcome::Terminal(reason) => Err(reason.clone()),
            ReadyOutcome::Waiting if wait.timed_out() => Err(format!(
                "no Web Bluetooth client became ready within {timeout:?}"
            )),
            ReadyOutcome::Waiting => Err("browser readiness wait ended unexpectedly".to_owned()),
        }
    }
}

#[derive(Clone)]
struct AppState {
    token: Arc<str>,
    allowed_origin: Arc<str>,
    claimed: Arc<AtomicBool>,
    commands: Arc<Mutex<Option<async_mpsc::Receiver<WorkerCommand>>>>,
    ready: Arc<ReadySignal>,
    receive: Arc<SharedRx>,
    stats: Arc<AtomicStats>,
    connect_timeout: Duration,
    operation_timeout: Duration,
    shutdown: watch::Receiver<bool>,
}

/// A finite, bounded synchronous byte stream whose browser helper moves only
/// opaque GATT fragments.
pub struct BrowserBleTransport {
    commands: async_mpsc::Sender<WorkerCommand>,
    receive: Arc<SharedRx>,
    stats: Arc<AtomicStats>,
    io_slice: Duration,
    operation_timeout: Duration,
    shutdown: watch::Sender<bool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl BrowserBleTransport {
    /// Serve the browser helper on loopback, announce its unguessable URL, and
    /// wait for one user-selected, validated, indication-subscribed peripheral.
    pub fn connect(
        config: BrowserBridgeConfig,
        announce_url: impl FnOnce(&str),
    ) -> Result<(Self, SelectedPeripheral), String> {
        validate_config(&config)?;
        let listener = std::net::TcpListener::bind(config.bind_address).map_err(|error| {
            format!(
                "could not bind browser bridge to {}: {error}",
                config.bind_address
            )
        })?;
        listener.set_nonblocking(true).map_err(|error| {
            format!("could not make browser bridge listener nonblocking: {error}")
        })?;
        let local_address = listener
            .local_addr()
            .map_err(|error| format!("could not inspect browser bridge address: {error}"))?;
        let token = random_token()?;
        let origin = format!("http://{local_address}");
        let url = format!("{origin}/session/{token}/");

        let receive = Arc::new(SharedRx::default());
        let stats = Arc::new(AtomicStats::default());
        let ready = Arc::new(ReadySignal::default());
        let claimed = Arc::new(AtomicBool::new(false));
        let (commands, command_rx) = async_mpsc::channel(COMMAND_CAPACITY);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let state = AppState {
            token: Arc::from(token),
            allowed_origin: Arc::from(origin),
            claimed,
            commands: Arc::new(Mutex::new(Some(command_rx))),
            ready: Arc::clone(&ready),
            receive: Arc::clone(&receive),
            stats: Arc::clone(&stats),
            connect_timeout: config.connect_timeout,
            operation_timeout: config.operation_timeout,
            shutdown: shutdown_rx.clone(),
        };
        let worker_shutdown = shutdown_rx;
        let worker_ready = Arc::clone(&ready);
        let worker_receive = Arc::clone(&receive);
        let worker = thread::Builder::new()
            .name("e290-web-bluetooth".to_owned())
            .spawn(move || {
                let runtime = match Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let reason = format!("could not create browser bridge runtime: {error}");
                        worker_ready.terminate(reason.clone());
                        worker_receive.terminate(reason);
                        return;
                    }
                };
                runtime.block_on(run_server(listener, state, worker_shutdown));
            })
            .map_err(|error| format!("could not spawn browser bridge worker: {error}"))?;

        announce_url(&url);
        let selected = match ready.wait(config.connect_timeout) {
            Ok(selected) => selected,
            Err(error) => {
                let _ = shutdown_tx.send(true);
                let _ = worker.join();
                return Err(error);
            }
        };

        Ok((
            Self {
                commands,
                receive,
                stats,
                io_slice: config.io_slice,
                operation_timeout: config.operation_timeout,
                shutdown: shutdown_tx,
                worker: Some(worker),
            },
            selected,
        ))
    }

    /// Snapshot browser-acknowledged GATT traffic counters.
    pub fn stats(&self) -> TransportStats {
        self.stats.snapshot()
    }

    /// Send the Rust-produced machine-readable qualification proof to the
    /// helper page for display. The page acknowledges display but cannot alter
    /// the proof or the authenticated session that produced it.
    pub fn publish_proof(&self, evidence: &serde_json::Value) -> Result<(), String> {
        let json = serde_json::to_string(evidence)
            .map_err(|error| format!("could not serialize browser proof: {error}"))?;
        if json.len() > MAX_PROOF_BYTES {
            return Err(format!(
                "browser proof is {} bytes; maximum is {MAX_PROOF_BYTES}",
                json.len()
            ));
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commands
            .try_send(WorkerCommand::PublishProof {
                json,
                reply: reply_tx,
            })
            .map_err(|error| match error {
                async_mpsc::error::TrySendError::Full(_) => {
                    "browser proof command queue is temporarily full".to_owned()
                }
                async_mpsc::error::TrySendError::Closed(_) => {
                    "browser bridge is not running".to_owned()
                }
            })?;
        match reply_rx.recv_timeout(self.operation_timeout) {
            Ok(result) => result,
            Err(_) => Err("browser did not acknowledge the displayed proof".to_owned()),
        }
    }

    /// Close the one browser bridge session and join its loopback server.
    pub fn disconnect(mut self) -> Result<TransportStats, String> {
        let result = self.stop();
        let stats = self.stats();
        result.map(|()| stats)
    }

    fn stop(&mut self) -> Result<(), String> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let enqueued = match self
            .commands
            .try_send(WorkerCommand::Shutdown { reply: reply_tx })
        {
            Ok(()) => true,
            Err(async_mpsc::error::TrySendError::Full(_)) => {
                self.receive
                    .terminate("browser bridge shutdown queue is full");
                false
            }
            Err(async_mpsc::error::TrySendError::Closed(_)) => false,
        };
        let acknowledged = enqueued && reply_rx.recv_timeout(self.operation_timeout).is_ok();
        let _ = self.shutdown.send(true);
        worker
            .join()
            .map_err(|_| "browser bridge worker panicked during shutdown".to_owned())?;
        if acknowledged || !enqueued {
            Ok(())
        } else {
            Err("browser bridge did not acknowledge shutdown".to_owned())
        }
    }
}

impl Read for BrowserBleTransport {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.receive.read(output, self.io_slice)
    }
}

impl Write for BrowserBleTransport {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        if input.len() > MAX_WRITE_CALL_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "one browser BLE stream write is {} bytes; maximum is {MAX_WRITE_CALL_BYTES}",
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
                    "browser bridge command queue is temporarily full",
                ),
                async_mpsc::error::TrySendError::Closed(_) => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "browser bridge is not running")
                }
            })?;
        let fragments = input.len().div_ceil(INITIAL_ATT_VALUE_BYTES);
        let host_deadline = self
            .operation_timeout
            .saturating_mul(u32::try_from(fragments).unwrap_or(u32::MAX))
            .saturating_add(Duration::from_secs(1));
        match reply_rx.recv_timeout(host_deadline) {
            Ok(Ok(written)) => Ok(written),
            Ok(Err(reason)) => Err(io::Error::new(io::ErrorKind::BrokenPipe, reason)),
            Err(_) => {
                self.receive
                    .terminate("browser GATT write result missed its terminal host deadline");
                let _ = self.shutdown.send(true);
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "browser GATT write acknowledgement became ambiguous; connection is terminal",
                ))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for BrowserBleTransport {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

async fn run_server(
    listener: std::net::TcpListener,
    state: AppState,
    mut shutdown: watch::Receiver<bool>,
) {
    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(error) => {
            let reason = format!("could not adopt browser bridge listener: {error}");
            state.ready.terminate(reason.clone());
            state.receive.terminate(reason);
            return;
        }
    };
    let app = Router::new()
        .route("/session/{token}/", get(index))
        .route("/session/{token}/app.js", get(app_javascript))
        .route("/session/{token}/profile.json", get(profile))
        .route("/session/{token}/bridge", get(websocket))
        .with_state(state.clone());
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            while !*shutdown.borrow() {
                if shutdown.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
    if let Err(error) = result {
        let reason = format!("browser bridge server failed: {error}");
        state.ready.terminate(reason.clone());
        state.receive.terminate(reason);
    }
}

async fn index(Path(token): Path<String>, State(state): State<AppState>) -> Response {
    if token != state.token.as_ref() {
        return StatusCode::NOT_FOUND.into_response();
    }
    static_response("text/html; charset=utf-8", INDEX_HTML)
}

async fn app_javascript(Path(token): Path<String>, State(state): State<AppState>) -> Response {
    if token != state.token.as_ref() {
        return StatusCode::NOT_FOUND.into_response();
    }
    static_response("text/javascript; charset=utf-8", APP_JAVASCRIPT)
}

async fn profile(Path(token): Path<String>, State(state): State<AppState>) -> Response {
    if token != state.token.as_ref() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut response = Json(json!({
        "bridgeProtocol": BRIDGE_PROTOCOL_VERSION,
        "gattProfileMajor": GATT_PROFILE_MAJOR,
        "gattProfileMinor": GATT_PROFILE_MINOR,
        "serviceUuid": SERVICE_UUID,
        "rxUuid": RX_UUID,
        "txUuid": TX_UUID,
        "maximumFragmentBytes": INITIAL_ATT_VALUE_BYTES,
        "operationTimeoutMs": u64::try_from(state.operation_timeout.as_millis()).unwrap_or(u64::MAX),
        "writeType": "with_response",
        "txDelivery": "indication",
    }))
    .into_response();
    secure_headers(response.headers_mut());
    response
}

async fn websocket(
    Path(token): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if token != state.token.as_ref() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if headers.get(ORIGIN).and_then(|value| value.to_str().ok())
        != Some(state.allowed_origin.as_ref())
    {
        return (StatusCode::FORBIDDEN, "browser bridge origin rejected").into_response();
    }
    if state
        .claimed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return (StatusCode::CONFLICT, "browser bridge already has a client").into_response();
    }
    let commands = state
        .commands
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
    let Some(commands) = commands else {
        return (StatusCode::CONFLICT, "browser bridge client is unavailable").into_response();
    };
    upgrade
        .max_frame_size(MAX_CONTROL_BYTES)
        .max_message_size(MAX_CONTROL_BYTES)
        .on_upgrade(move |socket| run_socket(socket, commands, state))
}

async fn run_socket(
    mut socket: WebSocket,
    mut commands: async_mpsc::Receiver<WorkerCommand>,
    state: AppState,
) {
    let mut shutdown = state.shutdown.clone();
    let selected = tokio::select! {
        result = time::timeout(state.connect_timeout, wait_for_ready(&mut socket)) => {
            match result {
                Ok(Ok(selected)) => selected,
                Ok(Err(error)) => {
                    terminate_socket(&mut socket, &state, error).await;
                    return;
                }
                Err(_) => {
                    terminate_socket(
                        &mut socket,
                        &state,
                        "browser GATT setup exceeded its connection deadline".to_owned(),
                    ).await;
                    return;
                }
            }
        }
        _ = shutdown.changed() => {
            let _ = socket.send(Message::Close(None)).await;
            state.ready.terminate("browser bridge shut down before GATT setup");
            state.receive.terminate("browser bridge shut down before GATT setup");
            return;
        }
    };
    if let Err(error) = state.ready.ready(selected) {
        terminate_socket(&mut socket, &state, error).await;
        return;
    }

    let next_write_id = AtomicU32::new(1);
    let terminal = loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(WorkerCommand::Write { bytes, reply }) => {
                        let result = write_fragments(
                            &mut socket,
                            &bytes,
                            &next_write_id,
                            &state,
                        ).await;
                        let failed = result.as_ref().err().cloned();
                        let _ = reply.send(result);
                        if let Some(error) = failed {
                            break error;
                        }
                    }
                    Some(WorkerCommand::PublishProof { json, reply }) => {
                        let result = publish_proof(&mut socket, &json, &state).await;
                        let failed = result.as_ref().err().cloned();
                        let _ = reply.send(result);
                        if let Some(error) = failed {
                            break error;
                        }
                    }
                    Some(WorkerCommand::Shutdown { reply }) => {
                        let _ = socket.send(Message::Close(None)).await;
                        let _ = reply.send(());
                        break "browser bridge shut down".to_owned();
                    }
                    None => break "browser bridge command channel ended".to_owned(),
                }
            }
            message = socket.recv() => {
                match handle_idle_message(message, &state) {
                    Ok(()) => {}
                    Err(error) => break error,
                }
            }
            _ = shutdown.changed() => {
                let _ = socket.send(Message::Close(None)).await;
                break "browser bridge server shut down".to_owned();
            }
        }
    };
    state.receive.terminate(terminal);
}

async fn wait_for_ready(socket: &mut WebSocket) -> Result<SelectedPeripheral, String> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                let control = parse_control(&text)?;
                match control {
                    BrowserControl::Ready {
                        device_id,
                        device_name,
                        rx_write_with_response,
                        tx_indicate,
                        maximum_write_bytes,
                    } => {
                        validate_ready(
                            &device_id,
                            device_name.as_deref(),
                            rx_write_with_response,
                            tx_indicate,
                            maximum_write_bytes,
                        )?;
                        return Ok(SelectedPeripheral {
                            id: device_id,
                            local_name: device_name,
                            rssi: None,
                        });
                    }
                    BrowserControl::Closed { error } => {
                        return Err(bounded_remote_error("browser closed BLE setup", &error));
                    }
                    _ => return Err("browser sent write state before GATT readiness".to_owned()),
                }
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Binary(_))) => {
                return Err("browser sent indication bytes before GATT readiness".to_owned());
            }
            Some(Ok(Message::Close(_))) | None => {
                return Err("browser bridge closed before GATT readiness".to_owned());
            }
            Some(Err(error)) => {
                return Err(format!(
                    "browser WebSocket failed during GATT setup: {error}"
                ));
            }
        }
    }
}

async fn write_fragments(
    socket: &mut WebSocket,
    bytes: &[u8],
    next_write_id: &AtomicU32,
    state: &AppState,
) -> Result<usize, String> {
    for fragment in bytes.chunks(INITIAL_ATT_VALUE_BYTES) {
        let id = next_write_id.fetch_add(1, Ordering::Relaxed);
        let id = if id == 0 {
            next_write_id.store(2, Ordering::Relaxed);
            1
        } else {
            id
        };
        let frame = encode_write_frame(id, fragment)?;
        deadline(
            "sending browser GATT write",
            state.operation_timeout,
            socket.send(Message::Binary(frame.into())),
        )
        .await?;
        wait_for_write_ack(socket, id, state).await?;
        state.stats.tx_fragments.fetch_add(1, Ordering::Relaxed);
        state
            .stats
            .tx_bytes
            .fetch_add(fragment.len() as u64, Ordering::Relaxed);
    }
    Ok(bytes.len())
}

async fn wait_for_write_ack(
    socket: &mut WebSocket,
    expected_id: u32,
    state: &AppState,
) -> Result<(), String> {
    let result = time::timeout(state.operation_timeout, async {
        loop {
            match socket.recv().await {
                Some(Ok(Message::Text(text))) => match parse_control(&text)? {
                    BrowserControl::WriteAck { id } if id == expected_id => return Ok(()),
                    BrowserControl::WriteAck { id } => {
                        return Err(format!(
                            "browser acknowledged write {id}, expected {expected_id}"
                        ));
                    }
                    BrowserControl::WriteError { id, error } if id == expected_id => {
                        return Err(bounded_remote_error("browser GATT write failed", &error));
                    }
                    BrowserControl::WriteError { id, .. } => {
                        return Err(format!(
                            "browser rejected write {id}, expected {expected_id}"
                        ));
                    }
                    BrowserControl::Closed { error } => {
                        return Err(bounded_remote_error("browser closed BLE link", &error));
                    }
                    BrowserControl::ProofAck => {
                        return Err(
                            "browser acknowledged a proof while a GATT write was pending"
                                .to_owned(),
                        );
                    }
                    BrowserControl::Ready { .. } => {
                        return Err("browser reported duplicate GATT readiness".to_owned());
                    }
                },
                Some(Ok(Message::Binary(frame))) => accept_indication(&frame, state)?,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => {
                    return Err(
                        "browser bridge closed while awaiting GATT write response".to_owned()
                    );
                }
                Some(Err(error)) => {
                    return Err(format!(
                        "browser WebSocket failed while awaiting GATT write response: {error}"
                    ));
                }
            }
        }
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => Err(format!(
            "browser did not confirm GATT write {expected_id} within {:?}",
            state.operation_timeout
        )),
    }
}

fn handle_idle_message(
    message: Option<Result<Message, axum::Error>>,
    state: &AppState,
) -> Result<(), String> {
    match message {
        Some(Ok(Message::Binary(frame))) => accept_indication(&frame, state),
        Some(Ok(Message::Text(text))) => match parse_control(&text)? {
            BrowserControl::Closed { error } => {
                Err(bounded_remote_error("browser closed BLE link", &error))
            }
            BrowserControl::Ready { .. } => {
                Err("browser reported duplicate GATT readiness".to_owned())
            }
            BrowserControl::ProofAck => {
                Err("browser acknowledged a proof while none was pending".to_owned())
            }
            BrowserControl::WriteAck { id } | BrowserControl::WriteError { id, .. } => Err(
                format!("browser reported write {id} while no write was pending"),
            ),
        },
        Some(Ok(Message::Ping(_) | Message::Pong(_))) => Ok(()),
        Some(Ok(Message::Close(_))) | None => Err("browser bridge closed".to_owned()),
        Some(Err(error)) => Err(format!("browser WebSocket failed: {error}")),
    }
}

async fn publish_proof(
    socket: &mut WebSocket,
    evidence_json: &str,
    state: &AppState,
) -> Result<(), String> {
    let envelope = format!(r#"{{"type":"proof","evidence":{evidence_json}}}"#);
    deadline(
        "sending browser qualification proof",
        state.operation_timeout,
        socket.send(Message::Text(envelope.into())),
    )
    .await?;
    let result = time::timeout(state.operation_timeout, async {
        loop {
            match socket.recv().await {
                Some(Ok(Message::Text(text))) => match parse_control(&text)? {
                    BrowserControl::ProofAck => return Ok(()),
                    BrowserControl::Closed { error } => {
                        return Err(bounded_remote_error("browser closed BLE link", &error));
                    }
                    BrowserControl::Ready { .. } => {
                        return Err("browser reported duplicate GATT readiness".to_owned());
                    }
                    BrowserControl::WriteAck { id } | BrowserControl::WriteError { id, .. } => {
                        return Err(format!(
                            "browser reported write {id} while proof acknowledgement was pending"
                        ));
                    }
                },
                Some(Ok(Message::Binary(frame))) => accept_indication(&frame, state)?,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => {
                    return Err("browser bridge closed before displaying proof".to_owned());
                }
                Some(Err(error)) => {
                    return Err(format!(
                        "browser WebSocket failed while displaying proof: {error}"
                    ));
                }
            }
        }
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => Err("browser did not acknowledge the displayed proof in time".to_owned()),
    }
}

fn accept_indication(frame: &[u8], state: &AppState) -> Result<(), String> {
    let value = decode_indication_frame(frame)?;
    state.receive.push(value)?;
    state.stats.rx_indications.fetch_add(1, Ordering::Relaxed);
    state
        .stats
        .rx_bytes
        .fetch_add(value.len() as u64, Ordering::Relaxed);
    Ok(())
}

async fn terminate_socket(socket: &mut WebSocket, state: &AppState, reason: String) {
    state.ready.terminate(reason.clone());
    state.receive.terminate(reason);
    let _ = socket.send(Message::Close(None)).await;
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BrowserControl {
    Ready {
        device_id: String,
        device_name: Option<String>,
        rx_write_with_response: bool,
        tx_indicate: bool,
        maximum_write_bytes: usize,
    },
    WriteAck {
        id: u32,
    },
    WriteError {
        id: u32,
        error: String,
    },
    Closed {
        error: String,
    },
    ProofAck,
}

#[cfg(test)]
#[derive(Serialize)]
struct ProtocolEvidence<'a> {
    bridge_protocol: u8,
    direction: &'a str,
    id: u32,
    bytes: usize,
}

fn parse_control(text: &str) -> Result<BrowserControl, String> {
    if text.len() > MAX_CONTROL_BYTES {
        return Err(format!(
            "browser control message exceeds {MAX_CONTROL_BYTES} bytes"
        ));
    }
    serde_json::from_str(text).map_err(|error| format!("invalid browser control message: {error}"))
}

fn validate_ready(
    device_id: &str,
    device_name: Option<&str>,
    rx_write_with_response: bool,
    tx_indicate: bool,
    maximum_write_bytes: usize,
) -> Result<(), String> {
    if device_id.is_empty() || device_id.len() > MAX_DEVICE_METADATA_BYTES {
        return Err("browser device ID is empty or too long".to_owned());
    }
    if device_name.is_some_and(|name| name.len() > MAX_DEVICE_METADATA_BYTES) {
        return Err("browser device name is too long".to_owned());
    }
    if !rx_write_with_response {
        return Err("browser did not validate RX write-with-response".to_owned());
    }
    if !tx_indicate {
        return Err("browser did not validate TX indication support".to_owned());
    }
    if maximum_write_bytes != INITIAL_ATT_VALUE_BYTES {
        return Err(format!(
            "browser write bound is {maximum_write_bytes}, expected {INITIAL_ATT_VALUE_BYTES}"
        ));
    }
    Ok(())
}

fn encode_write_frame(id: u32, value: &[u8]) -> Result<Vec<u8>, String> {
    if value.is_empty() || value.len() > INITIAL_ATT_VALUE_BYTES {
        return Err(format!(
            "browser GATT write length {} is outside 1..={INITIAL_ATT_VALUE_BYTES}",
            value.len()
        ));
    }
    let mut frame = Vec::with_capacity(FRAME_PREFIX_BYTES + WRITE_ID_BYTES + value.len());
    frame.push(BRIDGE_PROTOCOL_VERSION);
    frame.push(FRAME_WRITE);
    frame.extend_from_slice(&id.to_be_bytes());
    frame.extend_from_slice(value);
    Ok(frame)
}

fn decode_indication_frame(frame: &[u8]) -> Result<&[u8], String> {
    if frame.len() < FRAME_PREFIX_BYTES
        || frame[0] != BRIDGE_PROTOCOL_VERSION
        || frame[1] != FRAME_INDICATION
    {
        return Err("browser sent an invalid indication bridge frame".to_owned());
    }
    let value = &frame[FRAME_PREFIX_BYTES..];
    if value.is_empty() || value.len() > INITIAL_ATT_VALUE_BYTES {
        return Err(format!(
            "browser indication length {} is outside 1..={INITIAL_ATT_VALUE_BYTES}",
            value.len()
        ));
    }
    Ok(value)
}

fn bounded_remote_error(stage: &str, error: &str) -> String {
    let error = if error.len() <= MAX_ERROR_BYTES {
        error
    } else {
        let mut end = MAX_ERROR_BYTES;
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        &error[..end]
    };
    format!("{stage}: {error}")
}

fn static_response(content_type: &'static str, body: &'static str) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        content_type.parse().expect("valid content type"),
    );
    secure_headers(response.headers_mut());
    response
}

fn secure_headers(headers: &mut HeaderMap) {
    headers.insert(
        CACHE_CONTROL,
        "no-store".parse().expect("valid cache policy"),
    );
    headers.insert(
        CONTENT_SECURITY_POLICY,
        "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self' ws://127.0.0.1:*; img-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"
            .parse()
            .expect("valid content security policy"),
    );
    headers.insert(
        "x-content-type-options",
        "nosniff".parse().expect("valid header"),
    );
    headers.insert(
        "referrer-policy",
        "no-referrer".parse().expect("valid header"),
    );
}

fn validate_config(config: &BrowserBridgeConfig) -> Result<(), String> {
    if config.bind_address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
        return Err("browser bridge must bind IPv4 localhost (127.0.0.1)".to_owned());
    }
    if config.connect_timeout.is_zero() {
        return Err("browser connection timeout must be positive".to_owned());
    }
    if config.operation_timeout.is_zero() {
        return Err("browser operation timeout must be positive".to_owned());
    }
    if config.operation_timeout > Duration::from_secs(60) {
        return Err("browser operation timeout must not exceed 60 seconds".to_owned());
    }
    if config.io_slice.is_zero() {
        return Err("browser I/O slice must be positive".to_owned());
    }
    Ok(())
}

fn random_token() -> Result<String, String> {
    let mut token = [0_u8; 32];
    getrandom::fill(&mut token)
        .map_err(|error| format!("could not generate browser bridge token: {error}"))?;
    Ok(hex::encode(token))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_is_loopback_only_and_finite() {
        assert!(validate_config(&BrowserBridgeConfig::default()).is_ok());
        let public = BrowserBridgeConfig {
            bind_address: "0.0.0.0:8329".parse().unwrap(),
            ..BrowserBridgeConfig::default()
        };
        assert!(validate_config(&public).is_err());
        let ipv6 = BrowserBridgeConfig {
            bind_address: "[::1]:8329".parse().unwrap(),
            ..BrowserBridgeConfig::default()
        };
        assert!(validate_config(&ipv6).is_err());
        let infinite = BrowserBridgeConfig {
            operation_timeout: Duration::ZERO,
            ..BrowserBridgeConfig::default()
        };
        assert!(validate_config(&infinite).is_err());
    }

    #[test]
    fn ready_control_requires_exact_gatt_capabilities_and_bounds() {
        assert!(
            validate_ready(
                "opaque",
                Some("reticulum-e290-e13e88"),
                true,
                true,
                INITIAL_ATT_VALUE_BYTES,
            )
            .is_ok()
        );
        assert!(validate_ready("opaque", None, false, true, INITIAL_ATT_VALUE_BYTES,).is_err());
        assert!(validate_ready("opaque", None, true, true, INITIAL_ATT_VALUE_BYTES + 1,).is_err());
    }

    #[test]
    fn bridge_binary_frames_are_directional_and_bounded() {
        let write = encode_write_frame(0x0102_0304, &[0xaa, 0xbb]).unwrap();
        assert_eq!(
            write,
            [BRIDGE_PROTOCOL_VERSION, FRAME_WRITE, 1, 2, 3, 4, 0xaa, 0xbb]
        );
        assert!(encode_write_frame(1, &[]).is_err());
        assert!(encode_write_frame(1, &[0; INITIAL_ATT_VALUE_BYTES + 1]).is_err());

        let indication = [BRIDGE_PROTOCOL_VERSION, FRAME_INDICATION, 7, 8, 9];
        assert_eq!(decode_indication_frame(&indication).unwrap(), &[7, 8, 9]);
        assert!(decode_indication_frame(&write).is_err());
        let mut oversize = vec![BRIDGE_PROTOCOL_VERSION, FRAME_INDICATION];
        oversize.extend_from_slice(&[0; INITIAL_ATT_VALUE_BYTES + 1]);
        assert!(decode_indication_frame(&oversize).is_err());
    }

    #[test]
    fn control_parser_is_tagged_and_rejects_oversize_input() {
        let ready = parse_control(
            r#"{"type":"ready","device_id":"opaque","device_name":null,"rx_write_with_response":true,"tx_indicate":true,"maximum_write_bytes":20}"#,
        )
        .unwrap();
        assert!(matches!(ready, BrowserControl::Ready { .. }));
        assert!(parse_control(&"x".repeat(MAX_CONTROL_BYTES + 1)).is_err());
    }

    #[test]
    fn remote_errors_are_utf8_safe_and_bounded_by_encoded_bytes() {
        let ascii = bounded_remote_error("stage", &"a".repeat(MAX_ERROR_BYTES + 1));
        let ascii = ascii.strip_prefix("stage: ").unwrap();
        assert_eq!(ascii.len(), MAX_ERROR_BYTES);

        let exact = format!("{}é", "a".repeat(MAX_ERROR_BYTES - 2));
        let bounded = bounded_remote_error("stage", &exact);
        assert_eq!(bounded.strip_prefix("stage: ").unwrap(), exact);

        let split_code_point = format!("{}é", "a".repeat(MAX_ERROR_BYTES - 1));
        let bounded = bounded_remote_error("stage", &split_code_point);
        let remote = bounded.strip_prefix("stage: ").unwrap();
        assert_eq!(remote, "a".repeat(MAX_ERROR_BYTES - 1));
        assert!(remote.len() <= MAX_ERROR_BYTES);
    }

    #[test]
    fn readiness_signal_is_single_assignment_and_terminal() {
        let ready = ReadySignal::default();
        let selected = SelectedPeripheral {
            id: "opaque".to_owned(),
            local_name: Some("reticulum-e290-e13e88".to_owned()),
            rssi: None,
        };
        ready.ready(selected.clone()).unwrap();
        assert_eq!(ready.wait(Duration::from_millis(1)).unwrap(), selected);
        assert!(ready.ready(selected).is_err());

        let terminal = ReadySignal::default();
        terminal.terminate("gone");
        assert_eq!(terminal.wait(Duration::from_millis(1)).unwrap_err(), "gone");
    }

    #[test]
    fn protocol_metadata_is_machine_readable_without_stream_contents() {
        let evidence = ProtocolEvidence {
            bridge_protocol: BRIDGE_PROTOCOL_VERSION,
            direction: "rust_to_browser",
            id: 7,
            bytes: 20,
        };
        let value = serde_json::to_value(evidence).unwrap();
        assert_eq!(value["bridge_protocol"], BRIDGE_PROTOCOL_VERSION);
        assert_eq!(value["bytes"], INITIAL_ATT_VALUE_BYTES);
        assert!(value.get("payload").is_none());
    }
}
