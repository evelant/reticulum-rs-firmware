use super::*;

pub(crate) struct RetainedActions {
    pub(crate) actions: NodeActions,
    pub(crate) admission: reticulum_tx_supervisor::OrdinaryRouterAdmission,
    pub(crate) protocol_dispatch: Option<OrdinaryProtocolDispatch>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubmissionProtocolDispatch {
    Path {
        offer: PathDiscoveryOffer,
        token: Option<OrdinaryDispatchToken>,
    },
    Link {
        offer: LinkEstablishmentOffer,
        link: LinkHandle,
    },
}

impl SubmissionProtocolDispatch {
    pub(crate) const fn path(offer: PathDiscoveryOffer) -> Self {
        Self::Path { offer, token: None }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NomadProtocolDispatch {
    Path {
        destination: NomadDestinationHash,
    },
    Link {
        destination: NomadDestinationHash,
        link: LinkHandle,
    },
    Request {
        handle: RequestHandle,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OrdinaryProtocolDispatch {
    Submission(SubmissionProtocolDispatch),
    Nomad(NomadProtocolDispatch),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NomadDispatchResolution {
    Committed,
    Reconciled,
    InvariantReconciled,
    CleanupTransferred,
}

impl NomadDispatchResolution {
    pub(crate) const fn requires_fail_closed_drain(self) -> bool {
        matches!(self, Self::InvariantReconciled | Self::CleanupTransferred)
    }
}

impl RetainedActions {
    pub(crate) fn ordinary(
        actions: NodeActions,
        admission: reticulum_tx_supervisor::OrdinaryRouterAdmission,
    ) -> Self {
        Self {
            actions,
            admission,
            protocol_dispatch: None,
        }
    }

    pub(crate) fn with_protocol_dispatch(mut self, protocol: OrdinaryProtocolDispatch) -> Self {
        debug_assert!(self.protocol_dispatch.is_none());
        self.protocol_dispatch = Some(protocol);
        self
    }

    pub(crate) fn protocol_dispatch(&self) -> Option<OrdinaryProtocolDispatch> {
        self.protocol_dispatch
    }

    pub(crate) fn take_protocol_dispatch(&mut self) -> Option<OrdinaryProtocolDispatch> {
        self.protocol_dispatch.take()
    }

    pub(crate) fn has_application_events(&self) -> bool {
        !self.actions.events.is_empty()
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        NodeActions,
        reticulum_tx_supervisor::OrdinaryRouterAdmission,
        Option<OrdinaryProtocolDispatch>,
    ) {
        (self.actions, self.admission, self.protocol_dispatch)
    }
}
pub(crate) type QuarantinedIngressBuffer = (NodeInterfaceIngressRecycleFault, SealedIngressPacket);

/// Boot-lifetime LXMF scheduler owners placed together in external PSRAM.
///
/// Application-event slots remain in their existing internal static storage;
/// every token here is only structural authority over one of those slots.
#[must_use = "the LXMF volatile owner graph must remain alive for the node task"]
pub(crate) struct ApplicationVolatileState {
    pub(crate) retries: LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    pub(crate) proof_holder: LxmfProofActionsHolder,
    pub(crate) authority_faults: LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    pub(crate) discovered_peers: DiscoveredPeers<
        { config::LXMF_DISCOVERED_PEERS },
        { config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES },
    >,
    pub(crate) pending_owner_fault_observed: bool,
    pub(crate) service_fault_observed: bool,
    pub(crate) nomad: ProductNomadRuntimeState,
    pub(crate) nomad_api: NomadFetchApiState,
    pub(crate) reticulum_probe: ProductReticulumProbeState,
    pub(crate) rmap: RmapDiscoveryRuntime,
}

impl ApplicationVolatileState {
    /// Construct the empty boot-time owner graph before any event is admitted.
    pub(crate) const fn new() -> Self {
        Self {
            retries: LxmfRetrySet::new(),
            proof_holder: LxmfProofActionsHolder::new(),
            authority_faults: LxmfAuthorityFault::new(),
            discovered_peers: match DiscoveredPeers::try_new() {
                Ok(peers) => peers,
                Err(_) => panic!("the product peer-discovery capacity must be nonzero"),
            },
            pending_owner_fault_observed: false,
            service_fault_observed: false,
            nomad: ProductNomadRuntimeState::new(),
            nomad_api: NomadFetchApiState::new(),
            reticulum_probe: ProductReticulumProbeState::new(),
            rmap: RmapDiscoveryRuntime::disabled(),
        }
    }

    /// Install one opt-in RMAP payload and compact incremental stamp search.
    pub(crate) fn configure_rmap(
        &mut self,
        packed: PackedDiscoveryInfo,
        search: DiscoveryStampSearch,
        status: &'static RmapDiscoveryStatusCell,
        publication_policy: RmapPublicationPolicy,
    ) {
        self.rmap
            .configure(packed, search, status, publication_policy);
    }
}

/// Registered application destinations borrowed by the permanent node task.
#[derive(Clone, Copy)]
pub(crate) struct NodeApplicationDestinations {
    pub(crate) lxmf: Option<DestinationHash>,
    pub(crate) nomad: DestinationHash,
    pub(crate) proof_probe: DestinationHash,
    pub(crate) rmap: Option<DestinationHash>,
}

impl NodeApplicationDestinations {
    /// Bundle the destination set without increasing the executor task's
    /// bounded argument tuple.
    pub(crate) const fn new(
        lxmf: Option<DestinationHash>,
        nomad: DestinationHash,
        proof_probe: DestinationHash,
        rmap: Option<DestinationHash>,
    ) -> Self {
        Self {
            lxmf,
            nomad,
            proof_probe,
            rmap,
        }
    }
}

pub(crate) enum ActionOfferHandling {
    Retry(RetainedActions),
    RetainAndDrain(RetainedActions),
}

#[derive(Clone, Copy)]
pub(crate) enum LxmfProofOfferHandling {
    Retry,
    RetainAndDrain,
}

pub(crate) enum ActionRetryStep {
    Accepted(Option<OrdinaryProtocolDispatch>),
    Busy,
    Terminal,
}

pub(crate) struct IngressDrainState<'a> {
    pub(crate) observed_recycle_fault: &'a mut Option<NodeInterfaceIngressRecycleFault>,
    pub(crate) terminal_correlation_fault: &'a mut Option<ReceiptCorrelationError>,
    pub(crate) correlation_recycle_pending: &'a mut bool,
    pub(crate) fail_closed_draining: &'a mut bool,
    pub(crate) quarantined_actions: &'a mut Option<RetainedActions>,
    pub(crate) quarantined_ingress_buffer: &'a mut Option<QuarantinedIngressBuffer>,
    pub(crate) local_quarantine_available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveRequestKind {
    Begin,
    ProofStart,
    Activate,
    AbortCurrent,
}

impl LiveRequestKind {
    pub(crate) const fn from_request(request: &PairingRequest) -> Self {
        match request {
            PairingRequest::Begin(_) => Self::Begin,
            PairingRequest::ProofStart(_) => Self::ProofStart,
            PairingRequest::Activate(_) => Self::Activate,
            PairingRequest::AbortCurrent(_) => Self::AbortCurrent,
        }
    }

    pub(crate) const fn expected_mutation(self) -> Option<PairingMutation> {
        match self {
            Self::Begin => Some(PairingMutation::AddPending),
            Self::ProofStart => None,
            Self::Activate => Some(PairingMutation::ActivatePending),
            Self::AbortCurrent => Some(PairingMutation::AbortPending),
        }
    }

    pub(crate) fn blocked_response(self, sequence: u64) -> PairingResponse {
        match self {
            Self::Begin => {
                PairingResponse::Begin(BeginResponse::failure(sequence, PairingFailure::Blocked))
            }
            Self::ProofStart => PairingResponse::ProofStart(ProofStartResponse::failure(
                sequence,
                PairingFailure::Blocked,
            )),
            Self::Activate => PairingResponse::Activate(ActivateResponse::failure(
                sequence,
                ActivateFailure::Blocked,
            )),
            Self::AbortCurrent => PairingResponse::AbortCurrent(AbortCurrentResponse::new(
                sequence,
                AbortResult::Blocked,
            )),
        }
    }

    pub(crate) const fn matches_response(self, response: &PairingResponse) -> bool {
        matches!(
            (self, response),
            (Self::Begin, PairingResponse::Begin(_))
                | (Self::ProofStart, PairingResponse::ProofStart(_))
                | (Self::Activate, PairingResponse::Activate(_))
                | (Self::AbortCurrent, PairingResponse::AbortCurrent(_))
        )
    }
}

pub(crate) struct PairingNodeState {
    pub(crate) pending_control_command: Option<PairingControlCommand>,
    pub(crate) pending_control_reply: Option<PairingControlReply>,
    #[cfg(feature = "appliance")]
    pub(crate) pending_ble_bond_command: Option<BleBondCommitCommand>,
    #[cfg(feature = "appliance")]
    pub(crate) pending_ble_bond_reply: Option<BleBondCommitReply>,
    pub(crate) pending_session_admission_command: Option<SessionAdmissionCommand>,
    pub(crate) pending_session_admission_reply: Option<SessionAdmissionReply>,
    pub(crate) pending_exclusive: Option<(ConnectionId, AcquirePairingExclusive)>,
    pub(crate) initialization_retry_not_before_ms: Option<u64>,
    pub(crate) pending_live_command: Option<LivePairingCommand>,
    pub(crate) pending_live_operation: Option<LivePairingOperation>,
    pub(crate) pending_live_reply: Option<LivePairingReply>,
    pub(crate) live_retry_not_before_ms: Option<u64>,
    pub(crate) live_lane_faulted: bool,
}

pub(crate) enum AuthenticatedApiNodeState {
    Ready,
    PendingRequest(LocalApiRequest<AuthenticatedGrant>),
    PendingReply(LocalApiReply),
    Quarantined {
        request: LocalApiRequest<AuthenticatedGrant>,
        fault: AuthenticatedApiDispatchFailureKind,
    },
}

const _: () = assert!(
    mem::size_of::<AuthenticatedApiNodeState>()
        <= config::MAXIMUM_AUTHENTICATED_API_NODE_STATE_BYTES
);

impl AuthenticatedApiNodeState {
    pub(crate) const fn new() -> Self {
        Self::Ready
    }
}

pub(crate) struct NodeHandoffs {
    pub(crate) control: NodePairingHandoff<CriticalSectionRawMutex>,
    pub(crate) live: NodeLivePairingHandoff<CriticalSectionRawMutex>,
    #[cfg(feature = "appliance")]
    pub(crate) ble_bond: NodeBleBondHandoff<CriticalSectionRawMutex>,
    pub(crate) session_admission: NodeSessionAdmissionHandoff<CriticalSectionRawMutex>,
    pub(crate) authenticated_api: NodeHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    pub(crate) frame: AuthorizedFrameNodeHandoff<CriticalSectionRawMutex>,
    #[cfg(feature = "display")]
    pub(crate) display_telemetry: Option<DisplayTelemetryPublisher<CriticalSectionRawMutex>>,
}

pub(crate) struct NodeHandoffParts {
    pub(crate) control: NodePairingHandoff<CriticalSectionRawMutex>,
    pub(crate) live: NodeLivePairingHandoff<CriticalSectionRawMutex>,
    #[cfg(feature = "appliance")]
    pub(crate) ble_bond: NodeBleBondHandoff<CriticalSectionRawMutex>,
    pub(crate) session_admission: NodeSessionAdmissionHandoff<CriticalSectionRawMutex>,
    pub(crate) authenticated_api: NodeHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    pub(crate) frame: AuthorizedFrameNodeHandoff<CriticalSectionRawMutex>,
    #[cfg(feature = "display")]
    pub(crate) display_telemetry: Option<DisplayTelemetryPublisher<CriticalSectionRawMutex>>,
}

impl NodeHandoffs {
    pub(crate) const fn new(
        control: NodePairingHandoff<CriticalSectionRawMutex>,
        live: NodeLivePairingHandoff<CriticalSectionRawMutex>,
        #[cfg(feature = "appliance")] ble_bond: NodeBleBondHandoff<CriticalSectionRawMutex>,
        session_admission: NodeSessionAdmissionHandoff<CriticalSectionRawMutex>,
        authenticated_api: NodeHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
        frame: AuthorizedFrameNodeHandoff<CriticalSectionRawMutex>,
        #[cfg(feature = "display")] display_telemetry: Option<
            DisplayTelemetryPublisher<CriticalSectionRawMutex>,
        >,
    ) -> Self {
        Self {
            control,
            live,
            #[cfg(feature = "appliance")]
            ble_bond,
            session_admission,
            authenticated_api,
            frame,
            #[cfg(feature = "display")]
            display_telemetry,
        }
    }

    pub(crate) fn into_parts(self) -> NodeHandoffParts {
        NodeHandoffParts {
            control: self.control,
            live: self.live,
            #[cfg(feature = "appliance")]
            ble_bond: self.ble_bond,
            session_admission: self.session_admission,
            authenticated_api: self.authenticated_api,
            frame: self.frame,
            #[cfg(feature = "display")]
            display_telemetry: self.display_telemetry,
        }
    }
}

impl PairingNodeState {
    pub(crate) const fn new() -> Self {
        Self {
            pending_control_command: None,
            pending_control_reply: None,
            #[cfg(feature = "appliance")]
            pending_ble_bond_command: None,
            #[cfg(feature = "appliance")]
            pending_ble_bond_reply: None,
            pending_session_admission_command: None,
            pending_session_admission_reply: None,
            pending_exclusive: None,
            initialization_retry_not_before_ms: None,
            pending_live_command: None,
            pending_live_operation: None,
            pending_live_reply: None,
            live_retry_not_before_ms: None,
            live_lane_faulted: false,
        }
    }
}

pub(crate) struct NodeDiagnosticsOwners {
    pub(crate) radio: &'static RadioDiagnosticsCell,
    pub(crate) routes: &'static mut RouteDiagnosticsSnapshot<{ config::PATHS }>,
    pub(crate) lora_profile: LoRaProfile,
}

impl NodeDiagnosticsOwners {
    pub(crate) const fn new(
        radio: &'static RadioDiagnosticsCell,
        routes: &'static mut RouteDiagnosticsSnapshot<{ config::PATHS }>,
        lora_profile: LoRaProfile,
    ) -> Self {
        Self {
            radio,
            routes,
            lora_profile,
        }
    }
}
