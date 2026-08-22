use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, HOST, ORIGIN, REFERRER_POLICY,
    SET_COOKIE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinHandle;
use ts_rs::TS;

use reticulum_appliance_runtime::{
    ApplianceCapabilitiesView, ApplianceHandle, ApplianceSnapshot, ClientRequestError,
    ConnectionState, ContactRequest, DeviceView, JsonSafeInteger, MessageActivityPageRequest,
    MutationResponse, NomadFetchPollRequest, NomadFetchStartRequest, RadioTracePageRequest,
    ReticulumProbePollRequest, ReticulumProbeStartRequest, RetrySendRequest, RetrySendResponse,
    SendRequest, SendResponse, ServiceError, parse_destination, serialize_json_safe_u64,
};

const BODY_LIMIT: usize = 16 * 1024;
const MAX_SSE_CLIENTS: usize = 8;
const SESSION_COOKIE: &str = "reticulum_lxmf_session";
const CLIENT_HEADER: &str = "x-reticulum-client";
const CLIENT_HEADER_VALUE: &str = "web-alpha";

const INDEX_HTML: &[u8] = include_bytes!("../assets/index.html");
const APP_JS: &[u8] = include_bytes!("../assets/app.js");
const STYLE_CSS: &[u8] = include_bytes!("../assets/style.css");

/// Loopback listener policy for the bundled web client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebConfig {
    port: u16,
}

impl WebConfig {
    /// Bind to `127.0.0.1`, using zero for an ephemeral port.
    pub const fn new(port: u16) -> Self {
        Self { port }
    }

    /// Requested loopback TCP port.
    pub const fn port(self) -> u16 {
        self.port
    }
}

impl Default for WebConfig {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Running loopback web server and one-time capability URL.
pub struct WebServer {
    address: SocketAddr,
    url: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), std::io::Error>>,
}

impl WebServer {
    /// Actual loopback listener address.
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Browser bootstrap URL. Its fragment is never sent in an HTTP request.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Gracefully stop the listener and wait for its task.
    pub async fn shutdown(mut self) -> Result<(), String> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task
            .await
            .map_err(|error| format!("web task join failed: {error}"))?
            .map_err(|error| format!("web server failed: {error}"))
    }
}

#[derive(Clone)]
struct WebState {
    appliance: ApplianceHandle,
    expected_host: String,
    expected_origin: String,
    capability: Arc<String>,
    sse_slots: Arc<Semaphore>,
}

/// Bind the loopback server, generate a process capability, and spawn serving.
pub async fn serve_web(appliance: ApplianceHandle, config: WebConfig) -> Result<WebServer, String> {
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        config.port,
    ))
    .await
    .map_err(|error| format!("could not bind loopback web server: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("could not read loopback listener address: {error}"))?;
    let expected_host = address.to_string();
    let expected_origin = format!("http://{expected_host}");
    let capability = Arc::new(generate_capability()?);
    let state = WebState {
        appliance,
        expected_host,
        expected_origin: expected_origin.clone(),
        capability: capability.clone(),
        sse_slots: Arc::new(Semaphore::new(MAX_SSE_CLIENTS)),
    };
    let router = router(state);
    let (shutdown, receive_shutdown) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = receive_shutdown.await;
            })
            .await
    });
    Ok(WebServer {
        address,
        url: format!("{expected_origin}/#cap={capability}"),
        shutdown: Some(shutdown),
        task,
    })
}

fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/api/v1/session", post(create_session))
        .route("/api/v1/snapshot", get(snapshot))
        .route("/api/v1/contacts", get(contacts))
        .route("/api/v1/conversations", get(conversation_peers))
        .route("/api/v1/nearby", get(nearby_peers))
        .route("/api/v1/radio-routes", get(radio_routes_status))
        .route("/api/v1/contacts/{destination}", put(upsert_contact))
        .route("/api/v1/conversations/{destination}", get(conversation))
        .route("/api/v1/messages", post(send_message))
        .route("/api/v1/messages/retry", post(retry_message))
        .route("/api/v1/activity/query", post(message_activity))
        .route("/api/v1/radio-trace/query", post(radio_trace))
        .route("/api/v1/nomad/fetches", post(start_nomad_fetch))
        .route("/api/v1/nomad/fetches/poll", post(poll_nomad_fetch))
        .route("/api/v1/reticulum/probes", post(start_reticulum_probe))
        .route("/api/v1/reticulum/probes/poll", post(poll_reticulum_probe))
        .route("/api/v1/sync", post(sync_now))
        .route("/api/v1/ensure-connected", post(ensure_connected))
        .route("/api/v1/reconnect", post(reconnect))
        .route("/api/v1/events", get(events))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'; connect-src 'self' https:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; worker-src 'self' blob:",
        ),
    );
    response
}

async fn index(State(state): State<WebState>, headers: HeaderMap) -> Response {
    static_asset(&state, &headers, "text/html; charset=utf-8", INDEX_HTML)
}

async fn app_js(State(state): State<WebState>, headers: HeaderMap) -> Response {
    static_asset(&state, &headers, "text/javascript; charset=utf-8", APP_JS)
}

async fn style_css(State(state): State<WebState>, headers: HeaderMap) -> Response {
    static_asset(&state, &headers, "text/css; charset=utf-8", STYLE_CSS)
}

fn static_asset(
    state: &WebState,
    headers: &HeaderMap,
    content_type: &'static str,
    bytes: &'static [u8],
) -> Response {
    if let Err(error) = require_host(state, headers) {
        return error.into_response();
    }
    ([(CONTENT_TYPE, content_type)], bytes).into_response()
}

#[derive(Deserialize, TS)]
pub(crate) struct SessionRequest {
    capability: String,
}

async fn create_session(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<SessionRequest>,
) -> Result<Response, HttpError> {
    require_host(&state, &headers)?;
    require_mutation_headers(&state, &headers)?;
    if !constant_time_equal(request.capability.as_bytes(), state.capability.as_bytes()) {
        return Err(HttpError::unauthorized("invalid browser capability"));
    }
    let cookie = format!(
        "{SESSION_COOKIE}={}; HttpOnly; SameSite=Strict; Path=/api/v1",
        state.capability
    );
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie)
            .map_err(|_| HttpError::internal("could not encode session cookie"))?,
    );
    Ok(response)
}

async fn snapshot(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, false)?;
    let snapshot = state.appliance.snapshot();
    Ok(Json(HttpApplianceSnapshot::from(snapshot.as_ref())).into_response())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[allow(missing_docs)]
pub(crate) enum HttpConnectionState {
    Starting,
    Disconnected,
    Connecting,
    Ready {
        transport: reticulum_appliance_runtime::ConnectionTransport,
        endpoint: String,
        device_label: String,
    },
    Backoff,
    Faulted,
    Stopped,
}

impl From<&ConnectionState> for HttpConnectionState {
    fn from(state: &ConnectionState) -> Self {
        match state {
            ConnectionState::Starting => Self::Starting,
            ConnectionState::Disconnected | ConnectionState::Unavailable { .. } => {
                Self::Disconnected
            }
            ConnectionState::Connecting => Self::Connecting,
            ConnectionState::Ready {
                transport,
                endpoint,
                device_label,
            } => Self::Ready {
                transport: transport.clone(),
                endpoint: endpoint.clone(),
                device_label: device_label.clone(),
            },
            ConnectionState::Backoff => Self::Backoff,
            ConnectionState::Faulted => Self::Faulted,
            ConnectionState::Stopped => Self::Stopped,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub(crate) struct HttpApplianceSnapshot {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    revision: u64,
    connection: HttpConnectionState,
    device: Option<DeviceView>,
    capabilities: ApplianceCapabilitiesView,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    pending_outbox: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    contact_count: u64,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    imported_this_run: u64,
    last_error: Option<String>,
}

impl From<&ApplianceSnapshot> for HttpApplianceSnapshot {
    fn from(snapshot: &ApplianceSnapshot) -> Self {
        Self {
            revision: snapshot.revision(),
            connection: HttpConnectionState::from(snapshot.connection()),
            device: snapshot.device().cloned(),
            capabilities: snapshot.capabilities(),
            pending_outbox: u64::try_from(snapshot.pending_outbox())
                .expect("outbox count must fit u64"),
            contact_count: u64::try_from(snapshot.contact_count())
                .expect("contact count must fit u64"),
            imported_this_run: snapshot.imported_this_run(),
            last_error: snapshot.last_error().map(str::to_owned),
        }
    }
}

async fn contacts(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, false)?;
    let contacts = state
        .appliance
        .contacts()
        .await
        .map_err(HttpError::from_service)?;
    Ok(Json(contacts).into_response())
}

async fn conversation_peers(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, false)?;
    let peers = state
        .appliance
        .conversation_peers()
        .await
        .map_err(HttpError::from_service)?;
    Ok(Json(peers).into_response())
}

async fn nearby_peers(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, false)?;
    let peers = state
        .appliance
        .nearby_peers()
        .await
        .map_err(HttpError::from_service)?;
    Ok(Json(peers).into_response())
}

async fn radio_routes_status(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, false)?;
    let status = state
        .appliance
        .radio_routes_status()
        .await
        .map_err(HttpError::from_service)?;
    Ok(Json(status).into_response())
}

async fn upsert_contact(
    State(state): State<WebState>,
    Path(destination): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ContactRequest>,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, true)?;
    let contact = request
        .into_contact(&destination)
        .map_err(HttpError::from_client_request)?;
    let outcome = state
        .appliance
        .upsert_contact(contact)
        .await
        .map_err(HttpError::from_service)?;
    Ok(Json(MutationResponse::from(outcome)).into_response())
}

async fn conversation(
    State(state): State<WebState>,
    Path(destination): Path<String>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, false)?;
    let peer = parse_destination(&destination).map_err(HttpError::from_client_request)?;
    let timeline = state
        .appliance
        .timeline(peer)
        .await
        .map_err(HttpError::from_service)?;
    Ok(Json(timeline).into_response())
}

async fn send_message(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<SendRequest>,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, true)?;
    let material = request
        .into_material()
        .map_err(HttpError::from_client_request)?;
    let outcome = state
        .appliance
        .enqueue_send(material)
        .await
        .map_err(HttpError::from_service)?;
    Ok((StatusCode::ACCEPTED, Json(SendResponse::from(outcome))).into_response())
}

async fn retry_message(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<RetrySendRequest>,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, true)?;
    let (outbox_id, idempotency_key) = request
        .into_retry()
        .map_err(HttpError::from_client_request)?;
    let outcome = state
        .appliance
        .retry_send(outbox_id, idempotency_key)
        .await
        .map_err(HttpError::from_service)?;
    Ok((StatusCode::ACCEPTED, Json(RetrySendResponse::from(outcome))).into_response())
}

async fn message_activity(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<MessageActivityPageRequest>,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, false)?;
    request
        .validate()
        .map_err(|error| HttpError::bad_request(error.to_string()))?;
    let page = state
        .appliance
        .message_activity(request)
        .await
        .map_err(HttpError::from_service)?;
    Ok(Json(page).into_response())
}

async fn radio_trace(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<RadioTracePageRequest>,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, false)?;
    request
        .validate()
        .map_err(|error| HttpError::bad_request(error.to_string()))?;
    let page = state
        .appliance
        .radio_trace(request)
        .await
        .map_err(HttpError::from_service)?;
    Ok(Json(page).into_response())
}

async fn start_nomad_fetch(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<NomadFetchStartRequest>,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, true)?;
    request.validate().map_err(HttpError::from_client_request)?;
    let response = state
        .appliance
        .nomad_fetch_start(request)
        .await
        .map_err(HttpError::from_service)?;
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

async fn poll_nomad_fetch(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<NomadFetchPollRequest>,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, true)?;
    request.validate().map_err(HttpError::from_client_request)?;
    let response = state
        .appliance
        .nomad_fetch_poll(request)
        .await
        .map_err(HttpError::from_service)?;
    Ok(Json(response).into_response())
}

async fn start_reticulum_probe(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<ReticulumProbeStartRequest>,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, true)?;
    request.validate().map_err(HttpError::from_client_request)?;
    let response = state
        .appliance
        .reticulum_probe_start(request)
        .await
        .map_err(HttpError::from_service)?;
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

async fn poll_reticulum_probe(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<ReticulumProbePollRequest>,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, false)?;
    request.validate().map_err(HttpError::from_client_request)?;
    let response = state
        .appliance
        .reticulum_probe_poll(request)
        .await
        .map_err(HttpError::from_service)?;
    Ok(Json(response).into_response())
}

async fn sync_now(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, true)?;
    state
        .appliance
        .sync_now()
        .await
        .map_err(HttpError::from_service)?;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn reconnect(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, true)?;
    state
        .appliance
        .reconnect()
        .await
        .map_err(HttpError::from_service)?;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn ensure_connected(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    require_api(&state, &headers, true)?;
    state
        .appliance
        .ensure_connected()
        .await
        .map_err(HttpError::from_service)?;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn events(State(state): State<WebState>, headers: HeaderMap) -> Result<Response, HttpError> {
    require_api(&state, &headers, false)?;
    let permit = state
        .sse_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| HttpError::unavailable("too many event clients"))?;
    let receiver = state.appliance.subscribe_revisions();
    let stream = stream::unfold(
        (receiver, true, permit),
        |(mut receiver, initial, permit)| async move {
            if !initial && receiver.changed().await.is_err() {
                return None;
            }
            let revision = *receiver.borrow_and_update();
            let event = Event::default()
                .event("invalidate")
                .id(revision.to_string())
                .data(revision.to_string());
            Some((Ok::<Event, Infallible>(event), (receiver, false, permit)))
        },
    );
    Ok(Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response())
}

fn require_api(state: &WebState, headers: &HeaderMap, mutation: bool) -> Result<(), HttpError> {
    require_host(state, headers)?;
    if mutation {
        require_mutation_headers(state, headers)?;
    }
    let cookie = headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(session_cookie)
        .ok_or_else(|| HttpError::unauthorized("browser session is required"))?;
    if !constant_time_equal(cookie.as_bytes(), state.capability.as_bytes()) {
        return Err(HttpError::unauthorized("browser session is invalid"));
    }
    Ok(())
}

fn require_host(state: &WebState, headers: &HeaderMap) -> Result<(), HttpError> {
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| HttpError::bad_request("Host header is required"))?;
    if host != state.expected_host {
        return Err(HttpError::forbidden(
            "Host header is not the loopback listener",
        ));
    }
    Ok(())
}

fn require_mutation_headers(state: &WebState, headers: &HeaderMap) -> Result<(), HttpError> {
    let origin = headers
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| HttpError::forbidden("Origin header is required"))?;
    if origin != state.expected_origin {
        return Err(HttpError::forbidden(
            "Origin is not the loopback application",
        ));
    }
    if headers
        .get(CLIENT_HEADER)
        .and_then(|value| value.to_str().ok())
        != Some(CLIENT_HEADER_VALUE)
    {
        return Err(HttpError::forbidden("client request header is required"));
    }
    Ok(())
}

fn session_cookie(cookies: &str) -> Option<&str> {
    cookies.split(';').find_map(|item| {
        let (name, value) = item.trim().split_once('=')?;
        (name == SESSION_COOKIE).then_some(value)
    })
}

fn constant_time_equal(candidate: &[u8], expected: &[u8]) -> bool {
    candidate.len() == expected.len() && bool::from(candidate.ct_eq(expected))
}

fn generate_capability() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("could not generate browser capability: {error}"))?;
    Ok(hex::encode(bytes))
}

#[derive(Serialize, TS)]
pub(crate) struct ErrorBody {
    error: String,
}

#[derive(Debug)]
struct HttpError {
    status: StatusCode,
    message: String,
}

impl HttpError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    fn from_service(error: ServiceError) -> Self {
        match error {
            ServiceError::Busy | ServiceError::Stopped => Self::unavailable(error.to_string()),
            ServiceError::Operation(_) => Self::internal(error.to_string()),
        }
    }

    fn from_client_request(error: ClientRequestError) -> Self {
        Self::bad_request(error.to_string())
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request as HttpRequest;
    use reticulum_appliance_runtime::{
        ApplianceConfig, ConnectFailure, ConnectedSession, ConnectionTransport, Connector,
        start_appliance,
    };
    use tower::ServiceExt;

    fn test_state_with_appliance(appliance: ApplianceHandle) -> WebState {
        WebState {
            appliance,
            expected_host: "127.0.0.1:43123".to_owned(),
            expected_origin: "http://127.0.0.1:43123".to_owned(),
            capability: Arc::new("ab".repeat(32)),
            sse_slots: Arc::new(Semaphore::new(MAX_SSE_CLIENTS)),
        }
    }

    fn test_state() -> WebState {
        test_state_with_appliance(ApplianceHandle::for_web_test())
    }

    struct UnavailableTestConnector;

    impl Connector for UnavailableTestConnector {
        fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
            Err(ConnectFailure::unavailable(
                ConnectionTransport::Reticulum,
                "device sessions are unavailable in web adapter tests",
            ))
        }
    }

    fn local_actor_test_state() -> WebState {
        let appliance = start_appliance(
            ApplianceConfig::new(":memory:".into()),
            UnavailableTestConnector,
        )
        .expect("local web test actor starts");
        test_state_with_appliance(appliance)
    }

    #[test]
    fn cookie_parser_requires_the_exact_cookie_name() {
        assert_eq!(
            session_cookie("one=1; reticulum_lxmf_session=abc; two=2"),
            Some("abc")
        );
        assert_eq!(session_cookie("reticulum_lxmf_session_extra=abc"), None);
    }

    #[test]
    fn constant_time_comparison_rejects_wrong_length_and_value() {
        assert!(constant_time_equal(b"abcd", b"abcd"));
        assert!(!constant_time_equal(b"abc", b"abcd"));
        assert!(!constant_time_equal(b"abce", b"abcd"));
    }

    #[test]
    fn exact_destination_hex_is_enforced() {
        assert!(parse_destination(&"a0".repeat(16)).is_ok());
        assert!(parse_destination(&"a0".repeat(15)).is_err());
        assert!(parse_destination(&"zz".repeat(16)).is_err());
    }

    #[test]
    fn http_connection_projection_reports_reticulum() {
        let ready = ConnectionState::Ready {
            transport: reticulum_appliance_runtime::ConnectionTransport::Reticulum,
            endpoint: "peripheral-a".to_owned(),
            device_label: "001122334455".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(HttpConnectionState::from(&ready)).unwrap(),
            serde_json::json!({
                "state": "ready",
                "transport": "reticulum",
                "endpoint": "peripheral-a",
                "device_label": "001122334455",
            })
        );

        let unavailable = ConnectionState::Unavailable {
            transport: reticulum_appliance_runtime::ConnectionTransport::Reticulum,
        };
        assert_eq!(
            serde_json::to_value(HttpConnectionState::from(&unavailable)).unwrap(),
            serde_json::json!({ "state": "disconnected" })
        );
    }

    #[tokio::test]
    async fn static_assets_reject_host_header_confusion_and_set_security_headers() {
        let app = router(test_state());
        let rejected = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/")
                    .header(HOST, "attacker.invalid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        assert!(rejected.headers().contains_key(CONTENT_SECURITY_POLICY));

        let accepted = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/")
                    .header(HOST, "127.0.0.1:43123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(
            accepted.headers().get(CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        let policy = accepted
            .headers()
            .get(CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(policy.contains("style-src 'self' 'unsafe-inline'"));
        assert!(policy.contains("img-src 'self' data: blob: https:"));
        assert!(policy.contains("worker-src 'self' blob:"));
    }

    #[tokio::test]
    async fn nearby_route_requires_the_loopback_session_and_uses_the_actor() {
        let app = router(test_state());
        let unauthenticated = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/nearby")
                    .header(HOST, "127.0.0.1:43123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let unavailable = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/nearby")
                    .header(HOST, "127.0.0.1:43123")
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn conversation_peer_route_surfaces_local_history_without_a_device_session() {
        let app = router(local_actor_test_state());
        let unauthenticated = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/conversations")
                    .header(HOST, "127.0.0.1:43123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/conversations")
                    .header(HOST, "127.0.0.1:43123")
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), BODY_LIMIT).await.unwrap();
        assert_eq!(body.as_ref(), b"[]");
    }

    #[tokio::test]
    async fn activity_query_is_authenticated_validated_and_actor_owned() {
        let app = router(local_actor_test_state());
        let request = |limit| {
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/activity/query")
                .header(HOST, "127.0.0.1:43123")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "before_event_id": null,
                        "limit": limit,
                        "timeline_sequence": null,
                    })
                    .to_string(),
                ))
                .unwrap()
        };

        let unauthenticated = app.clone().oneshot(request(20)).await.unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let mut invalid = request(0);
        invalid.headers_mut().insert(
            COOKIE,
            HeaderValue::from_str(&format!("{SESSION_COOKIE}={}", "ab".repeat(32))).unwrap(),
        );
        let invalid = app.clone().oneshot(invalid).await.unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let mut valid = request(20);
        valid.headers_mut().insert(
            COOKIE,
            HeaderValue::from_str(&format!("{SESSION_COOKIE}={}", "ab".repeat(32))).unwrap(),
        );
        let response = app.oneshot(valid).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), BODY_LIMIT).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["events"], serde_json::json!([]));
        assert_eq!(json["next_before_event_id"], serde_json::Value::Null);
        assert_eq!(json["history_incomplete"], false);
    }

    #[tokio::test]
    async fn radio_routes_status_requires_the_loopback_session_and_actor_device_session() {
        let app = router(test_state());
        let unauthenticated = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/radio-routes")
                    .header(HOST, "127.0.0.1:43123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let unavailable = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/radio-routes")
                    .header(HOST, "127.0.0.1:43123")
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn nomad_fetch_routes_are_session_guarded_actor_operations() {
        let app = router(test_state());
        let start_body = || {
            Body::from(
                serde_json::json!({
                    "destination": "11".repeat(16),
                    "path": "/page/index.mu",
                    "timestamp_unix_ms": 1,
                    "idempotency_key": "22".repeat(16),
                })
                .to_string(),
            )
        };
        let unauthenticated = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/nomad/fetches")
                    .header(HOST, "127.0.0.1:43123")
                    .header(ORIGIN, "http://127.0.0.1:43123")
                    .header(CLIENT_HEADER, CLIENT_HEADER_VALUE)
                    .header(CONTENT_TYPE, "application/json")
                    .body(start_body())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let missing_mutation_proof = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/nomad/fetches")
                    .header(HOST, "127.0.0.1:43123")
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .header(CONTENT_TYPE, "application/json")
                    .body(start_body())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_mutation_proof.status(), StatusCode::FORBIDDEN);

        let invalid_start = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/nomad/fetches")
                    .header(HOST, "127.0.0.1:43123")
                    .header(ORIGIN, "http://127.0.0.1:43123")
                    .header(CLIENT_HEADER, CLIENT_HEADER_VALUE)
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "destination": "11".repeat(16),
                            "path": "relative",
                            "timestamp_unix_ms": 1,
                            "idempotency_key": "22".repeat(16),
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_start.status(), StatusCode::BAD_REQUEST);

        let unavailable_start = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/nomad/fetches")
                    .header(HOST, "127.0.0.1:43123")
                    .header(ORIGIN, "http://127.0.0.1:43123")
                    .header(CLIENT_HEADER, CLIENT_HEADER_VALUE)
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .header(CONTENT_TYPE, "application/json")
                    .body(start_body())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unavailable_start.status(), StatusCode::SERVICE_UNAVAILABLE);

        let unavailable_poll = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/nomad/fetches/poll")
                    .header(HOST, "127.0.0.1:43123")
                    .header(ORIGIN, "http://127.0.0.1:43123")
                    .header(CLIENT_HEADER, CLIENT_HEADER_VALUE)
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "id": format!("{}0000000000000001", "33".repeat(8)) })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unavailable_poll.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn reticulum_probe_start_is_mutating_while_poll_is_authenticated_read_only() {
        let app = router(test_state());
        let start_body = || {
            Body::from(
                serde_json::json!({
                    "destination": "11".repeat(16),
                    "idempotency_key": "22".repeat(16),
                })
                .to_string(),
            )
        };

        let missing_mutation_proof = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/reticulum/probes")
                    .header(HOST, "127.0.0.1:43123")
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .header(CONTENT_TYPE, "application/json")
                    .body(start_body())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_mutation_proof.status(), StatusCode::FORBIDDEN);

        let unavailable_start = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/reticulum/probes")
                    .header(HOST, "127.0.0.1:43123")
                    .header(ORIGIN, "http://127.0.0.1:43123")
                    .header(CLIENT_HEADER, CLIENT_HEADER_VALUE)
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .header(CONTENT_TYPE, "application/json")
                    .body(start_body())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unavailable_start.status(), StatusCode::SERVICE_UNAVAILABLE);

        let unavailable_poll = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/reticulum/probes/poll")
                    .header(HOST, "127.0.0.1:43123")
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "id": "33".repeat(16) }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unavailable_poll.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn capability_bootstrap_sets_http_only_cookie_and_unlocks_snapshot() {
        let app = router(test_state());
        let bootstrap = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/session")
                    .header(HOST, "127.0.0.1:43123")
                    .header(ORIGIN, "http://127.0.0.1:43123")
                    .header(CLIENT_HEADER, CLIENT_HEADER_VALUE)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        "{{\"capability\":\"{}\"}}",
                        "ab".repeat(32)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bootstrap.status(), StatusCode::NO_CONTENT);
        let set_cookie = bootstrap
            .headers()
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));

        let unauthorized = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/snapshot")
                    .header(HOST, "127.0.0.1:43123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/snapshot")
                    .header(HOST, "127.0.0.1:43123")
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);
        let body = to_bytes(authorized.into_body(), BODY_LIMIT).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["connection"]["state"], "starting");
    }

    #[tokio::test]
    async fn mutations_require_exact_origin_and_custom_header() {
        let app = router(test_state());
        let request = || {
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/sync")
                .header(HOST, "127.0.0.1:43123")
                .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap()
        };
        assert_eq!(
            app.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );

        let accepted_security = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/sync")
                    .header(HOST, "127.0.0.1:43123")
                    .header(ORIGIN, "http://127.0.0.1:43123")
                    .header(CLIENT_HEADER, CLIENT_HEADER_VALUE)
                    .header(COOKIE, format!("{SESSION_COOKIE}={}", "ab".repeat(32)))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted_security.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
