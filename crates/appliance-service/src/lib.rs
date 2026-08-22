//! Long-running PRNS appliance service and host web gateway.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod bindings;
mod web;

pub use bindings::{NoContent, render_api_bindings};
pub use reticulum_appliance_runtime::{
    ApplianceConfig, ApplianceHandle, ApplianceSnapshot, BytesEncoding, BytesView,
    ConnectedSession, ConnectionMetadata, ConnectionState, ConnectionTransport, Connector,
    ContactView, ConversationPeerView, DeviceView, DiagnosticInterfaceModeView,
    DiagnosticInterfaceStateView, DiagnosticInterfaceView, DiagnosticLoraDataTxEvidenceView,
    DiagnosticLoraLastRxView, DiagnosticLoraLastTxView, DiagnosticLoraTxFamilyView,
    DiagnosticLoraTxOutcomeView, LoraDiagnosticsView, MAX_JSON_SAFE_INTEGER,
    MessageActivityEventView, MessageActivityKindView, MessageActivityPageRequest,
    MessageActivityPageView, MessageActivityRequestError, NearbyPeerView, PacketEvidenceView,
    PhoneLocationAuthorizationView, PhoneLocationObservationError, PhoneLocationObservationView,
    PhoneLocationSourceView, PhoneLocationUnavailableReasonView, RadioRoutesStatusView,
    RadioTraceAttemptOutcomeView, RadioTraceEventKindView, RadioTraceEventView,
    RadioTraceMessageCorrelationView, RadioTracePageRequest, RadioTracePageView,
    RadioTraceProfileView, RadioTraceRequestError, RadioTraceRouteResolutionView,
    RadioTraceTxOutcomeView, RetainedRouteView, RouteNextHopView, ServiceError, TimelineDirection,
    TimelineStatus, TimelineView, start_appliance,
};
pub use web::{WebConfig, WebServer, serve_web};
