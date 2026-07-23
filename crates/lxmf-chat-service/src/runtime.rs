use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use reticulum_device_api::ApiErrorCode;
use reticulum_device_client::ClientError;
use reticulum_lxmf_chat_app::{
    ChatEngine, DeviceSessionError, EngineError, InboxStep, LxmfSession, ReconcileStep,
};
use reticulum_lxmf_chat_core::{
    ChatStore, Contact, ContactUpsertOutcome, DestinationHash, DeviceBinding, InboundCommitOutcome,
    OutboxCommitOutcome, OutboxId, OutboxMaterial, OutboxStatus, SqliteChatStore, SqliteStoreError,
    SubmissionFailure, SubmissionState, TimelineDirection as CoreTimelineDirection, TimelineEntry,
};
use serde::Serialize;
use tokio::sync::{oneshot, watch};
use ts_rs::TS;

use crate::bindings::{JsonSafeInteger, serialize_json_safe_u64, serialize_json_safe_usize};
use crate::serial::{SerialConnectionLease, SerialConnector, SerialConnectorConfig};

const COMMAND_CAPACITY: usize = 32;
const COMMAND_WAIT: Duration = Duration::from_millis(50);

type BoxedSession = Box<dyn LxmfSession<Error = DeviceSessionError> + Send>;

pub(crate) struct ConnectedSession {
    pub(crate) session: BoxedSession,
    pub(crate) port_name: String,
    pub(crate) usb_serial: String,
    pub(crate) connection_lease: Option<SerialConnectionLease>,
}

pub(crate) trait Connector: Send + 'static {
    fn connect(&mut self) -> Result<ConnectedSession, String>;
}

/// Timing and path policy for one appliance actor.
#[derive(Clone, Debug)]
pub struct ApplianceConfig {
    database: PathBuf,
    serial: SerialConnectorConfig,
    reconnect_initial: Duration,
    reconnect_maximum: Duration,
    operation_gap: Duration,
    inbox_poll_interval: Duration,
    status_poll_interval: Duration,
    capacity_backoff: Duration,
}

impl ApplianceConfig {
    /// Construct the default developer-alpha policy.
    pub fn new(database: PathBuf, serial: SerialConnectorConfig) -> Self {
        Self {
            database,
            serial,
            reconnect_initial: Duration::from_secs(1),
            reconnect_maximum: Duration::from_secs(30),
            operation_gap: Duration::from_millis(100),
            inbox_poll_interval: Duration::from_secs(2),
            status_poll_interval: Duration::from_secs(1),
            capacity_backoff: Duration::from_secs(30),
        }
    }

    /// Chat SQLite path owned exclusively by the service actor.
    pub fn database(&self) -> &PathBuf {
        &self.database
    }

    /// Concrete USB discovery and credential policy.
    pub const fn serial(&self) -> &SerialConnectorConfig {
        &self.serial
    }
}

/// Public connection lifecycle exposed to the UI.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionState {
    /// Actor has opened the database but has not attempted USB discovery.
    Starting,
    /// No matching USB port is currently usable.
    Disconnected,
    /// A matching USB port is being opened and authenticated.
    Connecting,
    /// The registered board is authenticated and background work may run.
    Ready {
        /// Current ephemeral serial path.
        port: String,
        /// Configured stable USB descriptor serial.
        usb_serial: String,
    },
    /// A retryable failure is waiting for bounded exponential backoff.
    Backoff,
    /// A permanent local/device invariant needs operator action.
    Faulted,
    /// Actor has accepted shutdown.
    Stopped,
}

/// JSON-safe authenticated device identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub(crate) struct DeviceView {
    device_id: String,
    primary_destination: String,
    lxmf_delivery_destination: String,
}

impl From<DeviceBinding> for DeviceView {
    fn from(binding: DeviceBinding) -> Self {
        Self {
            device_id: hex::encode(binding.device_id()),
            primary_destination: hex::encode(binding.primary_destination().as_bytes()),
            lxmf_delivery_destination: hex::encode(binding.lxmf_delivery_destination().as_bytes()),
        }
    }
}

/// Immutable authoritative service state read without touching serial or SQLite.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ApplianceSnapshot {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    revision: u64,
    connection: ConnectionState,
    device: Option<DeviceView>,
    #[serde(serialize_with = "serialize_json_safe_usize")]
    #[ts(as = "JsonSafeInteger")]
    pending_outbox: usize,
    #[serde(serialize_with = "serialize_json_safe_usize")]
    #[ts(as = "JsonSafeInteger")]
    contact_count: usize,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    imported_this_run: u64,
    last_error: Option<String>,
}

impl ApplianceSnapshot {
    fn starting() -> Self {
        Self {
            revision: 1,
            connection: ConnectionState::Starting,
            device: None,
            pending_outbox: 0,
            contact_count: 0,
            imported_this_run: 0,
            last_error: None,
        }
    }

    /// Monotonically increasing in-process state revision.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Current USB/session lifecycle.
    pub const fn connection(&self) -> &ConnectionState {
        &self.connection
    }

    /// Number of locally durable outbox rows still needing device work.
    pub const fn pending_outbox(&self) -> usize {
        self.pending_outbox
    }

    /// Latest actionable background failure, when present.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub(crate) struct ContactView {
    pub(crate) destination: String,
    pub(crate) name: String,
}

impl From<Contact> for ContactView {
    fn from(contact: Contact) -> Self {
        Self {
            destination: hex::encode(contact.destination().as_bytes()),
            name: contact.display_name().to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BytesEncoding {
    Utf8,
    Hex,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub(crate) struct BytesView {
    pub(crate) encoding: BytesEncoding,
    pub(crate) value: String,
}

impl BytesView {
    fn new(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(text) => Self {
                encoding: BytesEncoding::Utf8,
                value: text.to_owned(),
            },
            Err(_) => Self {
                encoding: BytesEncoding::Hex,
                value: hex::encode(bytes),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TimelineDirection {
    Inbound,
    Outbound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TimelineStatus {
    Committed,
    Accepted,
    Queued,
    Preparing,
    AwaitingDelivery,
    Delivered,
    FailedNoPath,
    FailedDeliveryTimeout,
    FailedDownstreamRejection,
    FailedInternal,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub(crate) struct TimelineView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    pub(crate) sequence: u64,
    pub(crate) direction: TimelineDirection,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    pub(crate) timestamp_ms: u64,
    pub(crate) message_id: Option<String>,
    #[serde(serialize_with = "crate::bindings::serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    pub(crate) outbox_id: Option<u64>,
    pub(crate) status: Option<TimelineStatus>,
    pub(crate) title: BytesView,
    pub(crate) content: BytesView,
}

impl From<TimelineEntry> for TimelineView {
    fn from(entry: TimelineEntry) -> Self {
        Self {
            sequence: entry.sequence().get(),
            direction: match entry.direction() {
                CoreTimelineDirection::Inbound => TimelineDirection::Inbound,
                CoreTimelineDirection::Outbound => TimelineDirection::Outbound,
            },
            timestamp_ms: entry.timestamp().get(),
            message_id: entry
                .message_id()
                .map(|message_id| hex::encode(message_id.as_bytes())),
            outbox_id: entry.outbox_id().map(OutboxId::get),
            status: entry.outbox_status().map(status_name),
            title: BytesView::new(entry.title()),
            content: BytesView::new(entry.content()),
        }
    }
}

fn status_name(status: OutboxStatus) -> TimelineStatus {
    match status {
        OutboxStatus::Committed => TimelineStatus::Committed,
        OutboxStatus::Accepted => TimelineStatus::Accepted,
        OutboxStatus::Device(SubmissionState::Queued) => TimelineStatus::Queued,
        OutboxStatus::Device(SubmissionState::Preparing) => TimelineStatus::Preparing,
        OutboxStatus::Device(SubmissionState::AwaitingDelivery(_)) => {
            TimelineStatus::AwaitingDelivery
        }
        OutboxStatus::Device(SubmissionState::Delivered(_)) => TimelineStatus::Delivered,
        OutboxStatus::Device(SubmissionState::Failed(failure)) => match failure {
            SubmissionFailure::NoPath => TimelineStatus::FailedNoPath,
            SubmissionFailure::DeliveryTimeout => TimelineStatus::FailedDeliveryTimeout,
            SubmissionFailure::DownstreamRejection => TimelineStatus::FailedDownstreamRejection,
            SubmissionFailure::Internal => TimelineStatus::FailedInternal,
        },
        OutboxStatus::Device(SubmissionState::Cancelled) => TimelineStatus::Cancelled,
    }
}

enum Command {
    Contacts(oneshot::Sender<Result<Vec<ContactView>, String>>),
    Timeline {
        peer: DestinationHash,
        reply: oneshot::Sender<Result<Vec<TimelineView>, String>>,
    },
    UpsertContact {
        contact: Contact,
        reply: oneshot::Sender<Result<ContactUpsertOutcome, String>>,
    },
    EnqueueSend {
        material: OutboxMaterial,
        reply: oneshot::Sender<Result<OutboxCommitOutcome, String>>,
    },
    SyncNow(oneshot::Sender<Result<(), String>>),
    Reconnect(oneshot::Sender<Result<(), String>>),
    Shutdown,
}

/// Bounded service-handle failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceError {
    /// Actor command queue is currently full.
    Busy,
    /// Actor has stopped or dropped the response.
    Stopped,
    /// A durable local operation failed.
    Operation(String),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("appliance command queue is busy"),
            Self::Stopped => formatter.write_str("appliance service has stopped"),
            Self::Operation(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for ServiceError {}

/// Cloneable bounded command and immutable-snapshot handle.
#[derive(Clone)]
pub struct ApplianceHandle {
    commands: mpsc::SyncSender<Command>,
    snapshot: Arc<RwLock<Arc<ApplianceSnapshot>>>,
    revisions: watch::Receiver<u64>,
    terminated: watch::Receiver<bool>,
}

impl ApplianceHandle {
    /// Read the current immutable state without contacting the actor.
    pub fn snapshot(&self) -> Arc<ApplianceSnapshot> {
        self.snapshot
            .read()
            .expect("snapshot lock must not be poisoned")
            .clone()
    }

    pub(crate) fn subscribe_revisions(&self) -> watch::Receiver<u64> {
        self.revisions.clone()
    }

    pub(crate) async fn contacts(&self) -> Result<Vec<ContactView>, ServiceError> {
        self.request(Command::Contacts).await
    }

    pub(crate) async fn timeline(
        &self,
        peer: DestinationHash,
    ) -> Result<Vec<TimelineView>, ServiceError> {
        self.request(|reply| Command::Timeline { peer, reply })
            .await
    }

    pub(crate) async fn upsert_contact(
        &self,
        contact: Contact,
    ) -> Result<ContactUpsertOutcome, ServiceError> {
        self.request(|reply| Command::UpsertContact { contact, reply })
            .await
    }

    pub(crate) async fn enqueue_send(
        &self,
        material: OutboxMaterial,
    ) -> Result<OutboxCommitOutcome, ServiceError> {
        self.request(|reply| Command::EnqueueSend { material, reply })
            .await
    }

    pub(crate) async fn sync_now(&self) -> Result<(), ServiceError> {
        self.request(Command::SyncNow).await
    }

    pub(crate) async fn reconnect(&self) -> Result<(), ServiceError> {
        self.request(Command::Reconnect).await
    }

    /// Ask the actor to stop. This does not block on thread termination.
    pub fn shutdown(&self) -> Result<(), ServiceError> {
        self.commands
            .try_send(Command::Shutdown)
            .map_err(map_try_send)
    }

    /// Stop the actor after any already-queued commands and wait until its
    /// final immutable snapshot confirms that the database owner exited.
    pub async fn shutdown_and_wait(&self) -> Result<(), ServiceError> {
        let mut terminated = self.terminated.clone();
        if *terminated.borrow() {
            return Ok(());
        }
        loop {
            match self.shutdown() {
                Ok(()) => break,
                Err(ServiceError::Busy) => tokio::time::sleep(COMMAND_WAIT).await,
                Err(error) => return Err(error),
            }
        }
        while !*terminated.borrow_and_update() {
            terminated
                .changed()
                .await
                .map_err(|_| ServiceError::Stopped)?;
        }
        Ok(())
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Command,
    ) -> Result<T, ServiceError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .try_send(command(reply))
            .map_err(map_try_send)?;
        receive
            .await
            .map_err(|_| ServiceError::Stopped)?
            .map_err(ServiceError::Operation)
    }

    #[cfg(test)]
    pub(crate) fn for_web_test() -> Self {
        let snapshot = ApplianceSnapshot::starting();
        let (commands, _receiver) = mpsc::sync_channel(1);
        let revision = snapshot.revision();
        let (_revision_tx, revisions) = watch::channel(revision);
        let (_termination_tx, terminated) = watch::channel(false);
        Self {
            commands,
            snapshot: Arc::new(RwLock::new(Arc::new(snapshot))),
            revisions,
            terminated,
        }
    }
}

fn map_try_send(error: mpsc::TrySendError<Command>) -> ServiceError {
    match error {
        mpsc::TrySendError::Full(_) => ServiceError::Busy,
        mpsc::TrySendError::Disconnected(_) => ServiceError::Stopped,
    }
}

/// Open the database and start its sole USB/device owner thread.
pub fn start_appliance(config: ApplianceConfig) -> Result<ApplianceHandle, ServiceError> {
    let store = SqliteChatStore::open(&config.database)
        .map_err(|error| ServiceError::Operation(error.to_string()))?;
    start_with_connector(config.clone(), store, SerialConnector::new(config.serial))
}

fn start_with_connector<C: Connector>(
    config: ApplianceConfig,
    store: SqliteChatStore,
    connector: C,
) -> Result<ApplianceHandle, ServiceError> {
    let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let initial = Arc::new(ApplianceSnapshot::starting());
    let snapshot = Arc::new(RwLock::new(initial.clone()));
    let (revision_tx, revision_rx) = watch::channel(initial.revision());
    let (termination_tx, termination_rx) = watch::channel(false);
    let handle = ApplianceHandle {
        commands,
        snapshot: snapshot.clone(),
        revisions: revision_rx,
        terminated: termination_rx,
    };
    thread::Builder::new()
        .name("lxmf-appliance".to_owned())
        .spawn(move || {
            Actor::new(
                config,
                ChatEngine::new(store),
                connector,
                receiver,
                snapshot,
                revision_tx,
            )
            .run();
            termination_tx.send_replace(true);
        })
        .map_err(|error| {
            ServiceError::Operation(format!("could not start appliance actor: {error}"))
        })?;
    Ok(handle)
}

struct Actor<C> {
    config: ApplianceConfig,
    engine: ChatEngine<SqliteChatStore>,
    connector: C,
    commands: mpsc::Receiver<Command>,
    published: Arc<RwLock<Arc<ApplianceSnapshot>>>,
    revisions: watch::Sender<u64>,
    snapshot: ApplianceSnapshot,
    session: Option<BoxedSession>,
    session_lease: Option<SerialConnectionLease>,
    reconnect_delay: Duration,
    next_connect: Instant,
    next_reconcile: Instant,
    next_inbox: Instant,
    prefer_inbox: bool,
    background_error_work: Option<BackgroundWork>,
    permanent_fault: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BackgroundWork {
    Inbox,
    Reconcile,
}

impl<C: Connector> Actor<C> {
    fn new(
        config: ApplianceConfig,
        engine: ChatEngine<SqliteChatStore>,
        connector: C,
        commands: mpsc::Receiver<Command>,
        published: Arc<RwLock<Arc<ApplianceSnapshot>>>,
        revisions: watch::Sender<u64>,
    ) -> Self {
        let now = Instant::now();
        Self {
            reconnect_delay: config.reconnect_initial,
            config,
            engine,
            connector,
            commands,
            published,
            revisions,
            snapshot: ApplianceSnapshot::starting(),
            session: None,
            session_lease: None,
            next_connect: now,
            next_reconcile: now,
            next_inbox: now,
            prefer_inbox: false,
            background_error_work: None,
            permanent_fault: false,
        }
    }

    fn run(mut self) {
        self.refresh_counts();
        self.publish();
        loop {
            match self.commands.recv_timeout(COMMAND_WAIT) {
                Ok(Command::Shutdown) => break,
                Ok(command) => self.handle_command(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.background_turn();
        }
        self.clear_session();
        self.snapshot.connection = ConnectionState::Stopped;
        self.publish();
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::Contacts(reply) => {
                let result = self
                    .engine
                    .contacts()
                    .map(|contacts| contacts.into_iter().map(ContactView::from).collect())
                    .map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
            Command::Timeline { peer, reply } => {
                let result = self
                    .engine
                    .timeline(peer)
                    .map(|entries| entries.into_iter().map(TimelineView::from).collect())
                    .map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
            Command::UpsertContact { contact, reply } => {
                let result = self
                    .engine
                    .upsert_contact(contact)
                    .map_err(|error| error.to_string());
                if result.is_ok() {
                    self.refresh_counts();
                    self.publish();
                }
                let _ = reply.send(result);
            }
            Command::EnqueueSend { material, reply } => {
                let result = self
                    .engine
                    .enqueue_send(material)
                    .map_err(|error| error.to_string());
                if result.is_ok() {
                    self.next_reconcile = Instant::now();
                    self.refresh_counts();
                    self.publish();
                }
                let _ = reply.send(result);
            }
            Command::SyncNow(reply) => {
                self.next_inbox = Instant::now();
                self.next_reconcile = Instant::now();
                let _ = reply.send(Ok(()));
            }
            Command::Reconnect(reply) => {
                self.clear_session();
                self.engine.reset_session_scan();
                self.background_error_work = None;
                self.permanent_fault = false;
                self.next_connect = Instant::now();
                self.snapshot.connection = ConnectionState::Disconnected;
                self.snapshot.last_error = None;
                self.publish();
                let _ = reply.send(Ok(()));
            }
            Command::Shutdown => unreachable!("shutdown is handled by the actor loop"),
        }
    }

    fn background_turn(&mut self) {
        let now = Instant::now();
        if self.session.is_none() {
            if !self.permanent_fault && now >= self.next_connect {
                self.connect();
            }
            return;
        }

        let inbox_due = now >= self.next_inbox;
        let reconcile_due = now >= self.next_reconcile;
        if !inbox_due && !reconcile_due {
            return;
        }
        let run_inbox = inbox_due && (!reconcile_due || self.prefer_inbox);
        self.prefer_inbox = !run_inbox;
        if run_inbox {
            self.inbox_turn();
        } else {
            self.reconcile_turn();
        }
    }

    fn connect(&mut self) {
        self.snapshot.connection = ConnectionState::Connecting;
        self.snapshot.last_error = None;
        self.publish();
        let connected = match self.connector.connect() {
            Ok(connected) => connected,
            Err(error) => {
                self.retry_connection(error);
                return;
            }
        };
        let ConnectedSession {
            mut session,
            port_name,
            usb_serial,
            connection_lease,
        } = connected;
        let binding = match session.binding() {
            Ok(binding) => binding,
            Err(error) => {
                self.retry_connection(error.to_string());
                return;
            }
        };
        match self.engine.store_mut().bind_device(binding) {
            Ok(_) => {}
            Err(SqliteStoreError::DeviceBindingMismatch { .. }) => {
                self.permanent_fault(format!(
                    "database/device identity mismatch for USB serial {usb_serial}"
                ));
                return;
            }
            Err(error) => {
                self.permanent_fault(error.to_string());
                return;
            }
        }
        self.engine.reset_session_scan();
        self.session = Some(session);
        self.session_lease = connection_lease;
        self.reconnect_delay = self.config.reconnect_initial;
        self.next_inbox = Instant::now();
        self.next_reconcile = Instant::now();
        self.snapshot.connection = ConnectionState::Ready {
            port: port_name,
            usb_serial,
        };
        self.snapshot.device = Some(binding.into());
        self.background_error_work = None;
        self.snapshot.last_error = None;
        self.refresh_counts();
        self.publish();
    }

    fn reconcile_turn(&mut self) {
        let session = self.session.as_mut().expect("ready actor has a session");
        match self.engine.reconcile_step(session.as_mut()) {
            Ok(ReconcileStep::Idle) => {
                self.next_reconcile = Instant::now() + self.config.status_poll_interval;
                if self.clear_background_error(BackgroundWork::Reconcile) {
                    self.publish();
                }
            }
            Ok(ReconcileStep::Submitted { .. } | ReconcileStep::Refreshed { .. }) => {
                self.next_reconcile = Instant::now() + self.config.operation_gap;
                self.clear_background_error(BackgroundWork::Reconcile);
                self.refresh_counts();
                self.publish();
            }
            Err(error) => self.background_error(BackgroundWork::Reconcile, error),
        }
    }

    fn inbox_turn(&mut self) {
        let session = self.session.as_mut().expect("ready actor has a session");
        match self.engine.inbox_step(session.as_mut()) {
            Ok(InboxStep::EndOfScan) => {
                self.next_inbox = Instant::now() + self.config.inbox_poll_interval;
                if self.clear_background_error(BackgroundWork::Inbox) {
                    self.publish();
                }
            }
            Ok(InboxStep::AlreadyImported { .. }) => {
                self.next_inbox = Instant::now() + self.config.operation_gap;
                if self.clear_background_error(BackgroundWork::Inbox) {
                    self.publish();
                }
            }
            Ok(InboxStep::Imported { outcome, .. }) => {
                self.next_inbox = Instant::now() + self.config.operation_gap;
                if outcome == InboundCommitOutcome::Inserted {
                    self.snapshot.imported_this_run =
                        self.snapshot.imported_this_run.saturating_add(1);
                }
                self.clear_background_error(BackgroundWork::Inbox);
                self.publish();
            }
            Err(error) => self.background_error(BackgroundWork::Inbox, error),
        }
    }

    fn background_error(
        &mut self,
        work: BackgroundWork,
        error: EngineError<SqliteStoreError, DeviceSessionError>,
    ) {
        match error {
            EngineError::Store(error) => self.permanent_fault(error.to_string()),
            EngineError::InboxMessageIdMismatch { .. } => {
                self.permanent_fault("device inbox summary/message mismatch".to_owned());
            }
            EngineError::Session(error) => {
                let usable = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_usable());
                if !usable {
                    self.clear_session();
                    self.engine.reset_session_scan();
                    self.retry_connection(error.to_string());
                    return;
                }
                match &error {
                    DeviceSessionError::Client(ClientError::Api {
                        error: response, ..
                    }) if response.code == ApiErrorCode::CapacityExhausted => {
                        self.background_error_work = Some(work);
                        self.snapshot.last_error = Some(error.to_string());
                        let retry_at = Instant::now() + self.config.capacity_backoff;
                        match work {
                            BackgroundWork::Inbox => self.next_inbox = retry_at,
                            BackgroundWork::Reconcile => self.next_reconcile = retry_at,
                        }
                        self.publish();
                    }
                    DeviceSessionError::Client(ClientError::Api { .. }) => {
                        self.permanent_fault(error.to_string());
                    }
                    _ => self.permanent_fault(error.to_string()),
                }
            }
        }
    }

    fn clear_background_error(&mut self, work: BackgroundWork) -> bool {
        if self.background_error_work != Some(work) {
            return false;
        }
        self.background_error_work = None;
        self.snapshot.last_error = None;
        true
    }

    fn retry_connection(&mut self, error: String) {
        self.clear_session();
        self.engine.reset_session_scan();
        self.background_error_work = None;
        self.snapshot.connection = ConnectionState::Backoff;
        self.snapshot.last_error = Some(error);
        self.next_connect = Instant::now() + self.reconnect_delay;
        self.reconnect_delay = self
            .reconnect_delay
            .saturating_mul(2)
            .min(self.config.reconnect_maximum);
        self.publish();
    }

    fn permanent_fault(&mut self, error: String) {
        self.clear_session();
        self.background_error_work = None;
        self.permanent_fault = true;
        self.snapshot.connection = ConnectionState::Faulted;
        self.snapshot.last_error = Some(error);
        self.publish();
    }

    fn clear_session(&mut self) {
        self.session = None;
        self.session_lease = None;
    }

    fn refresh_counts(&mut self) {
        match self.engine.store().reconcile() {
            Ok(work) => self.snapshot.pending_outbox = work.len(),
            Err(error) => self.snapshot.last_error = Some(error.to_string()),
        }
        match self.engine.contacts() {
            Ok(contacts) => self.snapshot.contact_count = contacts.len(),
            Err(error) => self.snapshot.last_error = Some(error.to_string()),
        }
    }

    fn publish(&mut self) {
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
        let value = Arc::new(self.snapshot.clone());
        *self
            .published
            .write()
            .expect("snapshot lock must not be poisoned") = value;
        self.revisions.send_replace(self.snapshot.revision);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use reticulum_lxmf_chat_app::{InboxCursor, InboxSummary};
    use reticulum_lxmf_chat_core::{
        AcceptanceIds, IdempotencyKey, InboundMessage, MessageId, SubmissionId, UnixTimestampMillis,
    };

    use super::*;

    static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    type FakeInbox = Vec<(InboxSummary, InboundMessage)>;
    type ConnectOutcome = Result<(DeviceBinding, FakeInbox), String>;

    struct TestDatabase(PathBuf);

    impl TestDatabase {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "reticulum-appliance-{label}-{}-{nonce}-{sequence}.sqlite3",
                std::process::id()
            )))
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
            let _ = fs::remove_file(self.0.with_extension("sqlite3-shm"));
            let _ = fs::remove_file(self.0.with_extension("sqlite3-wal"));
        }
    }

    fn destination(tag: u8) -> DestinationHash {
        DestinationHash::new([tag; 16])
    }

    fn binding(tag: u8) -> DeviceBinding {
        DeviceBinding::new(
            [tag; 16],
            destination(tag.wrapping_add(1)),
            destination(tag.wrapping_add(2)),
        )
    }

    fn outbound(tag: u8) -> OutboxMaterial {
        OutboxMaterial::new(
            destination(tag),
            UnixTimestampMillis::new(1_000 + u64::from(tag)).unwrap(),
            IdempotencyKey::new([tag; 16]),
            b"title".to_vec(),
            vec![tag],
        )
    }

    fn inbox(handle: u64, tag: u8) -> (InboxSummary, InboundMessage) {
        let id = MessageId::new([tag; 32]);
        (
            InboxSummary::new(InboxCursor::new(handle).unwrap(), id),
            InboundMessage::new(
                id,
                destination(0xa0),
                destination(tag),
                UnixTimestampMillis::new(2_000 + u64::from(tag)).unwrap(),
                b"inbound".to_vec(),
                vec![tag],
            ),
        )
    }

    struct FakeSession {
        binding: DeviceBinding,
        inbox: Vec<(InboxSummary, InboundMessage)>,
        submitted: Arc<Mutex<Vec<OutboxMaterial>>>,
    }

    impl LxmfSession for FakeSession {
        type Error = DeviceSessionError;

        fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
            Ok(self.binding)
        }

        fn submit(&mut self, material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
            let mut submitted = self.submitted.lock().unwrap();
            submitted.push(material.clone());
            let id = SubmissionId::new(submitted.len() as u64).unwrap();
            Ok(AcceptanceIds::new(id, MessageId::new([id.get() as u8; 32])))
        }

        fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
            Ok(SubmissionState::Queued)
        }

        fn next_inbox(
            &mut self,
            after: Option<InboxCursor>,
        ) -> Result<Option<InboxSummary>, Self::Error> {
            let after = after.map_or(0, InboxCursor::get);
            Ok(self
                .inbox
                .iter()
                .map(|(summary, _)| *summary)
                .find(|summary| summary.cursor().get() > after))
        }

        fn read_inbox(&mut self, summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
            Ok(self
                .inbox
                .iter()
                .find(|(candidate, _)| *candidate == summary)
                .unwrap()
                .1
                .clone())
        }

        fn is_usable(&self) -> bool {
            true
        }
    }

    struct FakeConnector {
        outcomes: VecDeque<ConnectOutcome>,
        attempts: Arc<AtomicUsize>,
        submitted: Arc<Mutex<Vec<OutboxMaterial>>>,
    }

    impl Connector for FakeConnector {
        fn connect(&mut self) -> Result<ConnectedSession, String> {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            let (binding, inbox) = self
                .outcomes
                .pop_front()
                .unwrap_or_else(|| Err("no scripted connection remains".to_owned()))?;
            Ok(ConnectedSession {
                session: Box::new(FakeSession {
                    binding,
                    inbox,
                    submitted: self.submitted.clone(),
                }),
                port_name: "/dev/fake".to_owned(),
                usb_serial: "001122334455".to_owned(),
                connection_lease: None,
            })
        }
    }

    fn config(database: &TestDatabase) -> ApplianceConfig {
        let serial =
            SerialConnectorConfig::new("00:11:22:33:44:55", PathBuf::from("unused")).unwrap();
        let mut config = ApplianceConfig::new(database.0.clone(), serial);
        config.reconnect_initial = Duration::from_millis(5);
        config.reconnect_maximum = Duration::from_millis(20);
        config.operation_gap = Duration::from_millis(2);
        config.inbox_poll_interval = Duration::from_millis(5);
        config.status_poll_interval = Duration::from_millis(5);
        config
    }

    async fn wait_for(handle: &ApplianceHandle, predicate: impl Fn(&ApplianceSnapshot) -> bool) {
        for _ in 0..200 {
            let snapshot = handle.snapshot();
            if predicate(&snapshot) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!(
            "timed out waiting for appliance state: {:?}",
            handle.snapshot()
        );
    }

    #[tokio::test]
    async fn actor_reconnects_binds_imports_and_processes_offline_outbox() {
        let database = TestDatabase::new("actor-flow");
        let expected_binding = binding(0x21);
        let inbound = inbox(1, 0x31);
        let attempts = Arc::new(AtomicUsize::new(0));
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let connector = FakeConnector {
            outcomes: VecDeque::from([
                Err("board absent".to_owned()),
                Ok((expected_binding, vec![inbound.clone()])),
            ]),
            attempts: attempts.clone(),
            submitted: submitted.clone(),
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(config(&database), store, connector).unwrap();

        let committed = handle.enqueue_send(outbound(0x41)).await.unwrap();
        assert!(matches!(committed, OutboxCommitOutcome::Inserted(_)));
        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Ready { .. })
                && snapshot.imported_this_run == 1
        })
        .await;
        wait_for(&handle, |_| !submitted.lock().unwrap().is_empty()).await;
        assert!(attempts.load(Ordering::Relaxed) >= 2);
        assert_eq!(submitted.lock().unwrap().as_slice(), &[outbound(0x41)]);
        let timeline = handle.timeline(destination(0x31)).await.unwrap();
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].content.encoding, BytesEncoding::Utf8);
        assert_eq!(timeline[0].content.value, "1");

        handle.shutdown_and_wait().await.unwrap();
        let reopened = SqliteChatStore::open(&database.0).unwrap();
        assert_eq!(reopened.device_binding().unwrap(), Some(expected_binding));
    }

    #[tokio::test]
    async fn database_binding_mismatch_faults_without_retrying_another_board() {
        let database = TestDatabase::new("binding-mismatch");
        let mut store = SqliteChatStore::open(&database.0).unwrap();
        store.bind_device(binding(0x51)).unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let connector = FakeConnector {
            outcomes: VecDeque::from([Ok((binding(0x61), Vec::new()))]),
            attempts: attempts.clone(),
            submitted: Arc::new(Mutex::new(Vec::new())),
        };
        let handle = start_with_connector(config(&database), store, connector).unwrap();
        wait_for(&handle, |snapshot| {
            snapshot.connection() == &ConnectionState::Faulted
        })
        .await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert!(handle.snapshot().last_error().unwrap().contains("mismatch"));
        handle.shutdown_and_wait().await.unwrap();
    }
}
