//! Long-running BLE appliance service and host web gateway.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod bindings;
mod ble;
mod credential;
mod onboarding;
mod profile;
mod web;

pub use bindings::{NoContent, render_api_bindings};
pub use ble::{BleConnector, BleConnectorConfig};
pub use onboarding::{
    OnboardingFault, OnboardingMethod, OnboardingSnapshot, OnboardingStage, OnboardingState,
    OnboardingView, RecoveryAction, RecoveryRequest,
};
pub use profile::{DeviceProfile, ProfileRoot, normalize_eui48, parse_eui48};
pub use reticulum_appliance_runtime::{
    ApplianceConfig, ApplianceHandle, ApplianceSnapshot, BytesEncoding, BytesView,
    ConnectedSession, ConnectionMetadata, ConnectionState, ConnectionTransport, Connector,
    ContactView, ConversationPeerView, DeviceView, DiagnosticInterfaceKindView,
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
    RadioTraceTxOutcomeView, RetainedRouteView, RnsDiagnosticsView, RouteDiagnosticResolutionView,
    ServiceError, TimelineDirection, TimelineStatus, TimelineView, start_appliance,
};
pub use web::{WebConfig, WebServer, serve_web};
