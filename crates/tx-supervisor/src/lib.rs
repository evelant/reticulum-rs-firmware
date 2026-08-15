//! Permanent transport-neutral Reticulum node and interface supervision.
//!
//! [`NodeInterfaceSupervisor`] owns one [`reticulum_node_core::NodeCore`], the
//! authoritative outbound interface router, direct ticket-aware
//! [`DataRouterCoordinator`] and [`OrdinaryRouterCoordinator`] paths, one DATA
//! and ordinary permit service per concrete interface actor, and the shared
//! authorization policy. Its checked constructor consumes the unsplit
//! interface fabric and paired permit proofs before returning common-slot actor
//! capabilities.
//!
//! This crate deliberately has no firmware, radio, HAL, device-API, flash, or
//! executor dependency. The aggregate is the intended sole node owner;
//! portable RNS ingress accepts only registry-validated interface provenance,
//! while firmware remains responsible for transport framing and for draining
//! every returned protocol action.

#![no_std]
#![deny(unsafe_code)]
#![deny(missing_docs)]

mod data_permit;
mod data_router;
mod node_interface;
mod ordinary_permit;
mod ordinary_router;

pub use data_permit::{
    DataPermitServer, DataPermitServerFaultResidueKind, DataPermitServerPhase,
    DataPermitServerStep, DataPermitServerWait,
};

pub use data_router::{
    DataCompletionAcceptError, DataCompletionAcceptFailure, DataPermitAuthorizationError,
    DataPermitAuthorizationFailure, DataPreparedHop, DataRecoveryAckError, DataRouterBuildError,
    DataRouterBuildFailure, DataRouterCompletionProgress, DataRouterConfig, DataRouterCoordinator,
    DataRouterFault, DataRouterFaultResidueKind, DataRouterOwnerMismatch, DataRouterParkedCounts,
    DataRouterParkedKind, DataRouterPrepareRequest, DataRouterPrepareResult, DataRouterStep,
};

pub use node_interface::{
    NodeInterfaceActorPorts, NodeInterfaceAnnounceFlushResult, NodeInterfaceApplicationEventDrain,
    NodeInterfaceCompletionFamily, NodeInterfaceCompletionOrigin, NodeInterfaceCompletionResidue,
    NodeInterfaceDataPrepareResult, NodeInterfaceIngressActionFault,
    NodeInterfaceIngressRecycleFault, NodeInterfaceIngressStep, NodeInterfaceOrdinaryOfferError,
    NodeInterfaceOrdinaryOfferFailure, NodeInterfaceQueuedIngressProcessed,
    NodeInterfaceSupervisor, NodeInterfaceSupervisorBuildError,
    NodeInterfaceSupervisorBuildFailure, NodeInterfaceSupervisorBuildSuccess,
    NodeInterfaceSupervisorFault, NodeInterfaceSupervisorInit, NodeInterfaceSupervisorPass,
    NodeInterfaceSupervisorTransition, NodeInterfaceTerminalIngressActions,
    NodeInterfaceTickAccepted, NodeInterfaceTickActionFailure, NodeInterfaceTickResult,
    RouteDiagnosticsEntry, RouteDiagnosticsSnapshot, RouteExpiryState, RouteInterfaceResolution,
};

pub use ordinary_permit::{
    OrdinaryPermitServer, OrdinaryPermitServerFaultResidueKind, OrdinaryPermitServerPhase,
    OrdinaryPermitServerStep, OrdinaryPermitServerWait,
};

pub use ordinary_router::{
    OrdinaryCompletionAcceptError, OrdinaryCompletionAcceptFailure,
    OrdinaryPermitAuthorizationError, OrdinaryPermitAuthorizationFailure, OrdinaryRouterAdmission,
    OrdinaryRouterBuildError, OrdinaryRouterBuildFailure, OrdinaryRouterBusyReason,
    OrdinaryRouterCompletionObservation, OrdinaryRouterCompletionProgress, OrdinaryRouterConfig,
    OrdinaryRouterCoordinator, OrdinaryRouterFault, OrdinaryRouterFaultResidueKind,
    OrdinaryRouterOfferError, OrdinaryRouterOfferFailure, OrdinaryRouterRejectedActions,
    OrdinaryRouterStep,
};

pub use reticulum_node_core::LxmfMessageLocation;
