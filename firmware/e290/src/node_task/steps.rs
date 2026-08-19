use super::*;

#[allow(
    clippy::result_large_err,
    reason = "proof admission failure must return the exact no-alloc action owner to its holder"
)]
pub(crate) fn drive_one_lxmf_proof(
    supervisor: &mut ProductSupervisor,
    delayed_proofs: &mut DelayedProofOwner<'static>,
    holder: &mut LxmfProofActionsHolder,
    trace_evidence: &mut Option<InboundProofEvidence>,
    radio_diagnostics: &RadioDiagnosticsCell,
    lora_interface: PacketInterfaceId,
    retry_actions_a: &mut Option<RetainedActions>,
    retry_actions_b: &mut Option<RetainedActions>,
    protocol_dispatch_pending: bool,
    fail_closed_draining: &mut bool,
) -> bool {
    let staged = if !holder.is_occupied() {
        let Some(proof) = delayed_proofs.lease_next() else {
            return false;
        };
        let event_id = proof.event_id();
        let proof_id = proof.id();
        let evidence = proof.evidence();
        let actions = proof.release_actions();
        holder
            .try_stage(actions, config::ordinary_admission(now_millis()))
            .unwrap_or_else(|_| unreachable!("released delayed proof is one packet only"));
        debug_assert!(trace_evidence.is_none());
        *trace_evidence = Some(evidence);
        if config::inbound_proof_uses_lora_trace(evidence.interface().0, lora_interface) {
            let _ = radio_diagnostics.record_inbound_proof_stage(
                now_micros(),
                RadioTraceInboundProofStage::ProofStaged,
                None,
                evidence,
            );
        }
        info!(
            "e290-node stage=lxmf-proof status=STAGED event_slot={} event_generation={} proof_slot={} proof_generation={} correlation={:02x?} holder=packet-only ordinary_supervisor=pending",
            event_id.slot().get(),
            event_id.generation().get(),
            proof_id.slot().get(),
            proof_id.generation().get(),
            evidence.covered_packet_hash(),
        );
        true
    } else {
        false
    };

    let offer = holder
        .begin_offer()
        .expect("a staged or previously retained LXMF proof exists");
    match offer.try_submit(|actions, admission| {
        let displacement_allowed = config::lxmf_proof_displacement_allowed(
            retry_actions_a.is_none() || retry_actions_b.is_none(),
            protocol_dispatch_pending,
        );
        let offered = if displacement_allowed {
            supervisor.try_offer_priority_actions(actions, admission)
        } else {
            supervisor
                .try_offer_actions(actions, admission)
                .map(|()| None)
        };
        offered
            .map(|displaced| {
                if let Some(displaced) = displaced {
                    let (actions, admission) = displaced.into_parts();
                    let retained = RetainedActions::ordinary(actions, admission);
                    if retry_actions_a.is_none() {
                        *retry_actions_a = Some(retained);
                    } else {
                        debug_assert!(retry_actions_b.is_none());
                        *retry_actions_b = Some(retained);
                    }
                    info!(
                        "e290-node stage=lxmf-proof status=PRIORITY-ADMITTED displaced=pending-ordinary action=retain-exact-displaced-owner-for-retry"
                    );
                }
            })
            .map_err(
                |failure| match handle_action_offer_failure(failure, "lxmf-proof") {
                    ActionOfferHandling::Retry(retained) => {
                        let (actions, admission, submission_protocol) = retained.into_parts();
                        debug_assert!(submission_protocol.is_none());
                        LxmfProofSinkFailure::returned(
                            LxmfProofOfferHandling::Retry,
                            actions,
                            admission,
                        )
                    }
                    ActionOfferHandling::RetainAndDrain(retained) => {
                        let (actions, admission, submission_protocol) = retained.into_parts();
                        debug_assert!(submission_protocol.is_none());
                        LxmfProofSinkFailure::returned(
                            LxmfProofOfferHandling::RetainAndDrain,
                            actions,
                            admission,
                        )
                    }
                },
            )
    }) {
        Ok(()) => {
            let evidence = trace_evidence
                .take()
                .expect("an accepted staged proof retains trace evidence");
            if config::inbound_proof_uses_lora_trace(evidence.interface().0, lora_interface) {
                let armed = radio_diagnostics.record_inbound_proof_ordinary_queued(
                    now_micros(),
                    None,
                    evidence,
                );
                if !armed {
                    // Diagnostics are observational only. The admitted proof
                    // keeps moving even when an earlier LoRa trace correlation
                    // is still armed or its terminal report was lost.
                    error!(
                        "e290-node stage=lxmf-proof status=TRACE-CORRELATION-SKIPPED reason=prior-proof-tx-still-armed correlation={:02x?} proof_admission=unaffected",
                        evidence.covered_packet_hash(),
                    );
                }
            }
            info!(
                "e290-node stage=lxmf-proof status=HANDED-OFF correlation={:02x?} owner=ordinary-supervisor direct_radio_send=false",
                evidence.covered_packet_hash(),
            );
            true
        }
        Err(LxmfProofOfferHandling::Retry) => staged,
        Err(LxmfProofOfferHandling::RetainAndDrain) => {
            *fail_closed_draining = true;
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_one_application_event(
    owner: &mut ApplicationEventOwner<'static>,
    delayed_proofs: &mut DelayedProofOwner<'static>,
    retries: &mut LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    storage: &mut ProductStorageCoordinator,
    supervisor: &mut ProductSupervisor,
    radio_diagnostics: &RadioDiagnosticsCell,
    lora_interface: PacketInterfaceId,
    nomad: &mut ProductNomadRuntimeState,
    discovered_peers: &mut DiscoveredPeers<
        { config::LXMF_DISCOVERED_PEERS },
        { config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES },
    >,
    lxmf_destination: Option<DestinationHash>,
    nomad_destination: DestinationHash,
    proof_probe_destination: DestinationHash,
    retry_actions_a: &mut Option<RetainedActions>,
    retry_actions_b: &mut Option<RetainedActions>,
    fail_closed_draining: &mut bool,
    authority_fault: &mut LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    pending_owner_fault_observed: bool,
    service_fault_observed: &mut bool,
    proof_backpressured: bool,
    rng: &mut Trng,
    now_ms: u64,
) -> bool {
    let pending_message = storage.lxmf_pending_message_id();
    let held_store_fault = pending_message.is_some_and(|message_id| {
        retries.has_fault_hold(message_id) || authority_fault.has_fault_hold(message_id)
    });

    if held_store_fault && let Some(entry) = retries.take_nonheld() {
        let class = entry.class();
        let message_id = entry.message_id();
        let admission_attempt = entry.admission_attempt();
        let admission_source = entry.admission_source();
        match owner.try_reacquire_quarantined(entry.into_token()) {
            Ok(lease) => {
                let event_id = lease.id();
                lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
                warn!(
                    "e290-node stage=lxmf-retry status=DISCARDED slot={} generation={} prior_class={class:?} reason=pending-store-fault-held proof=not-sent",
                    event_id.slot().get(),
                    event_id.generation().get(),
                );
            }
            Err(failure) => {
                let reason = failure.reason();
                retain_lxmf_authority_fault(
                    authority_fault,
                    LxmfRetryEntry::new(failure.into_token(), u64::MAX, class, message_id)
                        .with_admission_state(admission_attempt, admission_source),
                );
                error!(
                    "e290-node stage=lxmf-retry status=FAIL reason={reason:?} action=retain-unchanged-token-and-stop-lxmf"
                );
            }
        }
        return true;
    }

    let lxmf_fail_stopped =
        authority_fault.is_some() || pending_owner_fault_observed || *service_fault_observed;
    if (proof_backpressured || lxmf_fail_stopped)
        && let Some(entry) = retries.take_pressure_relief()
    {
        let class = entry.class();
        let message_id = entry.message_id();
        let admission_attempt = entry.admission_attempt();
        let admission_source = entry.admission_source();
        match owner.try_reacquire_quarantined(entry.into_token()) {
            Ok(lease) => {
                let event_id = lease.id();
                let discard_reason = if proof_backpressured {
                    ApplicationEventDiscardReason::DownstreamCapacity
                } else {
                    ApplicationEventDiscardReason::ConsumerUnavailable
                };
                lease.discard(discard_reason);
                warn!(
                    "e290-node stage=lxmf-retry status=DISCARDED slot={} generation={} prior_class={class:?} reason={} proof=not-sent pending-store-owner=preserved",
                    event_id.slot().get(),
                    event_id.generation().get(),
                    if proof_backpressured {
                        "ordinary-proof-pressure-relief"
                    } else {
                        "lxmf-service-fail-stop-cleanup"
                    },
                );
            }
            Err(failure) => {
                let reason = failure.reason();
                retain_lxmf_authority_fault(
                    authority_fault,
                    LxmfRetryEntry::new(failure.into_token(), u64::MAX, class, message_id)
                        .with_admission_state(admission_attempt, admission_source),
                );
                error!(
                    "e290-node stage=lxmf-retry status=FAIL reason={reason:?} action=retain-unchanged-token-and-stop-lxmf"
                );
            }
        }
        return true;
    }

    let retry = if authority_fault.is_none() && !pending_owner_fault_observed {
        retries.take_due(now_ms, pending_message)
    } else {
        None
    };
    let (lease, retry_class, retry_message_id, retry_attempt, retry_source) = if let Some(entry) =
        retry
    {
        let class = entry.class();
        let message_id = entry.message_id();
        let admission_attempt = entry.admission_attempt();
        let admission_source = entry.admission_source();
        match owner.try_reacquire_quarantined(entry.into_token()) {
            Ok(lease) => (
                lease,
                Some(class),
                message_id,
                admission_attempt,
                admission_source,
            ),
            Err(failure) => {
                let reason = failure.reason();
                retain_lxmf_authority_fault(
                    authority_fault,
                    LxmfRetryEntry::new(failure.into_token(), u64::MAX, class, message_id)
                        .with_admission_state(admission_attempt, admission_source),
                );
                error!(
                    "e290-node stage=lxmf-retry status=FAIL reason={reason:?} action=retain-unchanged-token-and-stop-lxmf"
                );
                return true;
            }
        }
    } else {
        let Some(lease) = owner.lease_next() else {
            return false;
        };
        (lease, None, None, 0, None)
    };

    let is_lxmf = match lease.event() {
        ApplicationEvent::DataReceived { destination, .. } => {
            lxmf_destination.is_some_and(|lxmf| destination == lxmf.as_bytes())
        }
        ApplicationEvent::LinkData {
            binding, context, ..
        } => {
            *context == APPLICATION_LINK_CONTEXT_NONE
                && binding.role() == ApplicationLinkRole::Responder
                && lxmf_destination.is_some_and(|lxmf| binding.destination() == lxmf.as_bytes())
        }
        _ => false,
    };

    if is_lxmf {
        if held_store_fault
            || authority_fault.is_some()
            || pending_owner_fault_observed
            || *service_fault_observed
        {
            let event_id = lease.id();
            lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
            warn!(
                "e290-node stage=lxmf-ingress status=DISCARDED slot={} generation={} reason=service-fail-stopped proof=not-sent routing=continue",
                event_id.slot().get(),
                event_id.generation().get(),
            );
            return true;
        }
        if pending_message.is_some() && retry_class != Some(LxmfRetryClass::StoreReconcile) {
            if proof_backpressured {
                let event_id = lease.id();
                lease.discard(ApplicationEventDiscardReason::DownstreamCapacity);
                warn!(
                    "e290-node stage=lxmf-ingress status=DISCARDED slot={} generation={} reason=ordinary-proof-pressure-relief pending-store-owner=preserved proof=not-sent",
                    event_id.slot().get(),
                    event_id.generation().get(),
                );
                return true;
            }
            quarantine_lxmf_retry(
                lease,
                retries,
                authority_fault,
                now_ms,
                LxmfRetryClass::StoreBusy,
                None,
                0,
                None,
            );
            return true;
        }
        return drive_lxmf_event(
            lease,
            delayed_proofs,
            retries,
            storage,
            supervisor,
            radio_diagnostics,
            lora_interface,
            lxmf_destination.expect("LXMF event matched an active destination"),
            authority_fault,
            service_fault_observed,
            retry_class,
            retry_message_id,
            retry_attempt,
            retry_source,
            now_ms,
        );
    }

    drive_non_lxmf_application_event(
        lease,
        supervisor,
        nomad,
        discovered_peers,
        retries,
        nomad_destination,
        proof_probe_destination,
        retry_actions_a,
        retry_actions_b,
        fail_closed_draining,
        rng,
        now_ms,
    )
}

pub(crate) fn drive_non_lxmf_application_event(
    lease: ApplicationEventLease<'_, 'static>,
    supervisor: &mut ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    discovered_peers: &mut DiscoveredPeers<
        { config::LXMF_DISCOVERED_PEERS },
        { config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES },
    >,
    retries: &mut LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    nomad_destination: DestinationHash,
    proof_probe_destination: DestinationHash,
    retry_actions_a: &mut Option<RetainedActions>,
    retry_actions_b: &mut Option<RetainedActions>,
    fail_closed_draining: &mut bool,
    rng: &mut Trng,
    now_ms: u64,
) -> bool {
    let event_id = lease.id();
    let sequence = lease.sequence();
    let kind = lease.event().kind();

    match nomad.observe_application_event(lease.event()) {
        NomadEventObservation::Applied => {
            let event = lease.acknowledge();
            drop(event);
            info!(
                "e290-node stage=nomad-application-event status=ACKNOWLEDGED kind={kind} slot={} generation={} sequence={}",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            return true;
        }
        NomadEventObservation::Fault(fault) => {
            let event = lease.acknowledge();
            drop(event);
            error!(
                "e290-node stage=nomad-application-event status=FAULT-CLEANED kind={kind} slot={} generation={} sequence={} reason={fault:?}",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            return true;
        }
        NomadEventObservation::Unrelated => {}
    }

    match classify_nomad_responder_event(&nomad_destination, lease.event()) {
        NomadResponderDisposition::Respond(response) => {
            if *fail_closed_draining {
                warn!(
                    "e290-node stage=nomad-responder status=DISCARDED kind={kind} slot={} generation={} sequence={} reason=aggregate-fail-closed",
                    event_id.slot().get(),
                    event_id.generation().get(),
                    sequence.get(),
                );
                lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
                return true;
            }
            let response_retry_slot = if retry_actions_a.is_none() {
                &mut *retry_actions_a
            } else if retry_actions_b.is_none() {
                &mut *retry_actions_b
            } else {
                *fail_closed_draining = true;
                error!(
                    "e290-node stage=nomad-responder status=QUARANTINED kind={kind} slot={} generation={} sequence={} reason=response-owner-slot-not-reserved-before-preparation action=fail-closed-drain",
                    event_id.slot().get(),
                    event_id.generation().get(),
                    sequence.get(),
                );
                lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
                return true;
            };
            let (binding, request, page) = response.into_parts();
            let preparation = supervisor.prepare_response_actions(
                binding,
                request,
                page,
                MonotonicSeconds::new(now_ms / 1_000),
                rng,
            );
            return drive_prepared_nomad_response(
                lease,
                preparation,
                supervisor,
                response_retry_slot,
                fail_closed_draining,
                now_ms,
            );
        }
        NomadResponderDisposition::WrongPath
        | NomadResponderDisposition::WrongValue
        | NomadResponderDisposition::LegacyRequestReceived => {
            warn!(
                "e290-node stage=nomad-responder status=DISCARDED kind={kind} slot={} generation={} sequence={} reason=invalid-or-unsupported-request",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.discard(ApplicationEventDiscardReason::InvalidPayload);
            return true;
        }
        NomadResponderDisposition::Unrelated
        | NomadResponderDisposition::WrongRole
        | NomadResponderDisposition::WrongDestination => {}
    }

    match lease.event() {
        ApplicationEvent::DataReceived { destination, .. }
            if destination == proof_probe_destination.as_bytes() =>
        {
            // `rnstransport.probe` is registered with PROVE_ALL. Rete emits
            // that proof as an immediate packet action on the ingress
            // interface, so this semantic event intentionally carries no
            // retained delayed proof. The packet action remains owned by the
            // ordinary transport-neutral action pipeline.
            match lease.acknowledge() {
                Ok(event) => {
                    drop(event);
                    info!(
                        "e290-node stage=reticulum-proof-probe-responder status=ACKNOWLEDGED kind={kind} slot={} generation={} sequence={} proof_policy=prove-all proof_delivery=immediate-action",
                        event_id.slot().get(),
                        event_id.generation().get(),
                        sequence.get(),
                    );
                }
                Err(failure) => {
                    let lease = failure.into_lease();
                    error!(
                        "e290-node stage=reticulum-proof-probe-responder status=FAIL kind={kind} slot={} generation={} sequence={} reason=unexpected-retained-proof-for-prove-all action=fail-closed-drain",
                        event_id.slot().get(),
                        event_id.generation().get(),
                        sequence.get(),
                    );
                    *fail_closed_draining = true;
                    lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
                }
            }
        }
        ApplicationEvent::DataReceived { payload, .. } => {
            let payload_len = payload.len();
            lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
            debug!(
                "e290-node stage=application-event-consumer status=DISCARDED kind={kind} slot={} generation={} sequence={} reason=no-registered-application-consumer payload_len={payload_len}",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
        }
        ApplicationEvent::AnnounceReceived {
            destination,
            identity,
            hops,
            app_data,
            ingress,
        } => {
            let destination = *destination;
            let identity = *identity;
            let hops = *hops;
            let app_data_len = app_data.as_ref().map_or(0, |data| data.len());
            let destination_hash = DestinationHash::new(destination);
            if supervisor.recall_identity(&destination_hash).is_some() {
                let woken = retries.wake_admission_for_source(destination_hash, now_ms);
                if woken != 0 {
                    info!(
                        "e290-node stage=lxmf-retry status=WOKEN source={destination:02x?} count={woken} trigger=authenticated-announce"
                    );
                }
            }
            let Some(ingress) = *ingress else {
                warn!(
                    "e290-node stage=peer-discovery status=DROPPED destination={destination:02x?} reason=missing-ingress-observation routing=unchanged"
                );
                lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
                return true;
            };
            let observed_interface = ingress.interface().0;
            let signal = ingress.signal().map_or(
                SignalObservations::UNKNOWN,
                |signal| {
                    SignalObservations::new(
                        Some(signal.rssi_dbm()),
                        Some(signal.snr_db()),
                    )
                },
            );
            let Some(public_key) = supervisor.recall_lxmf_delivery_identity(&destination_hash)
            else {
                lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
                return true;
            };
            let observation = AuthenticatedAnnounceObservation::new(
                PeerDestinationHash::new(destination),
                PeerIdentityHash::new(identity),
                IdentityPublicKey::new(public_key),
                app_data.as_deref().unwrap_or(&[]),
                ObservationMetadata::new(
                    hops,
                    ObservedInterfaceId::new(observed_interface),
                    signal,
                    PeerObservedMillis::new(now_ms),
                ),
            );
            match discovered_peers.observe(observation) {
                Ok(outcome) => {
                    let disposition = outcome.disposition();
                    let generation = outcome.generation().get();
                    let event = lease.acknowledge();
                    drop(event);
                    info!(
                        "e290-node stage=peer-discovery status=OBSERVED destination={destination:02x?} identity={identity:02x?} hops={hops} interface={observed_interface} app_data_len={app_data_len} generation={generation} disposition={disposition:?}",
                    );
                }
                Err(reason) => {
                    warn!(
                        "e290-node stage=peer-discovery status=DROPPED destination={destination:02x?} reason={reason:?} routing=unchanged"
                    );
                    lease.discard(ApplicationEventDiscardReason::DownstreamCapacity);
                }
            }
        }
        ApplicationEvent::Tick { .. } => {
            lease.discard(ApplicationEventDiscardReason::MaintenanceCoalesced);
        }
        ApplicationEvent::ResourceOffered { .. }
        | ApplicationEvent::ResourceProgress { .. }
        | ApplicationEvent::ResourceComplete { .. }
        | ApplicationEvent::ResourceFailed { .. }
        | ApplicationEvent::ResourceRejected { .. }
        | ApplicationEvent::RequestProgress { .. } => {
            error!(
                "e290-node stage=application-event-consumer status=QUARANTINED kind={kind} slot={} generation={} sequence={} reason=resource-ingress-disabled-invariant",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
        }
        // The Nomad responder claimed every exact supported request above.
        // Remaining request values have a role or destination that no current
        // application service owns; they are never outbound completions.
        ApplicationEvent::RequestValueReceived { .. } => {
            warn!(
                "e290-node stage=application-event-consumer status=DISCARDED kind={kind} slot={} generation={} sequence={} reason=consumer-unavailable consumer=unsupported-or-unowned-request-binding",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
        }
        ApplicationEvent::ProofReceived { .. }
        | ApplicationEvent::ReceiptFailed { .. }
        | ApplicationEvent::LinkEstablished { .. }
        | ApplicationEvent::LinkRttUpdated { .. }
        | ApplicationEvent::LinkData { .. }
        | ApplicationEvent::ChannelMessages { .. }
        // The exact Nomad request representations were handled above.
        // Remaining Link requests target an unsupported or unowned binding.
        | ApplicationEvent::RequestReceived { .. }
        | ApplicationEvent::ResponseReceived { .. }
        | ApplicationEvent::LinkClosed { .. }
        | ApplicationEvent::LinkIdentified { .. }
        | ApplicationEvent::RequestFailed { .. } => {
            warn!(
                "e290-node stage=application-event-consumer status=DISCARDED kind={kind} slot={} generation={} sequence={} reason=consumer-unavailable consumer=unsupported-or-unowned-application-event",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_prepared_nomad_response(
    lease: ApplicationEventLease<'_, 'static>,
    preparation: Result<
        Result<NodeActions, PrepareResponseError>,
        reticulum_tx_supervisor::NodeInterfaceSupervisorFault,
    >,
    supervisor: &mut ProductSupervisor,
    response_retry_slot: &mut Option<RetainedActions>,
    fail_closed_draining: &mut bool,
    now_ms: u64,
) -> bool {
    let event_id = lease.id();
    let sequence = lease.sequence();
    let kind = lease.event().kind();
    let actions = match preparation {
        Ok(Ok(actions)) => actions,
        Ok(Err(PrepareResponseError::LinkNotFound | PrepareResponseError::LinkNotActive)) => {
            warn!(
                "e290-node stage=nomad-responder status=DISCARDED kind={kind} slot={} generation={} sequence={} reason=link-unavailable",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
            return true;
        }
        Ok(Err(
            PrepareResponseError::ResponseAllocationFailed
            | PrepareResponseError::ActionAllocationFailed,
        )) => {
            warn!(
                "e290-node stage=nomad-responder status=DISCARDED kind={kind} slot={} generation={} sequence={} reason=response-capacity",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.discard(ApplicationEventDiscardReason::DownstreamCapacity);
            return true;
        }
        Ok(Err(PrepareResponseError::ResponseTooLarge { actual, maximum })) => {
            error!(
                "e290-node stage=nomad-responder status=DISCARDED kind={kind} slot={} generation={} sequence={} reason=response-too-large actual={actual} maximum={maximum}",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.discard(ApplicationEventDiscardReason::PolicyRejected);
            return true;
        }
        Ok(Err(
            reason @ (PrepareResponseError::LinkBindingMismatch
            | PrepareResponseError::LinkInterfaceUnknown
            | PrepareResponseError::Crypto
            | PrepareResponseError::PacketBuild
            | PrepareResponseError::Invariant),
        )) => {
            *fail_closed_draining = true;
            error!(
                "e290-node stage=nomad-responder status=QUARANTINED kind={kind} slot={} generation={} sequence={} reason={reason:?} action=fail-closed-drain",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            return true;
        }
        Err(fault) => {
            *fail_closed_draining = true;
            error!(
                "e290-node stage=nomad-responder status=QUARANTINED kind={kind} slot={} generation={} sequence={} reason=supervisor-fault fault={fault:?} action=fail-closed-drain",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            return true;
        }
    };

    let action_owner =
        match supervisor.try_offer_actions(actions, config::ordinary_admission(now_ms)) {
            Ok(()) => "ordinary-supervisor",
            Err(failure) => {
                let (retained, terminal) =
                    match handle_action_offer_failure(failure, "nomad-responder") {
                        ActionOfferHandling::Retry(retained) => (retained, false),
                        ActionOfferHandling::RetainAndDrain(retained) => (retained, true),
                    };
                debug_assert!(response_retry_slot.is_none());
                *response_retry_slot = Some(retained);
                if terminal {
                    *fail_closed_draining = true;
                }
                "reserved-retry-slot"
            }
        };

    match lease.acknowledge() {
        Ok(event) => {
            drop(event);
            info!(
                "e290-node stage=nomad-responder status=ACKNOWLEDGED kind={kind} slot={} generation={} sequence={} response_owner={action_owner}",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
        }
        Err(failure) => {
            *fail_closed_draining = true;
            failure
                .into_lease()
                .quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            error!(
                "e290-node stage=nomad-responder status=QUARANTINED kind={kind} slot={} generation={} sequence={} response_owner={action_owner} reason=unexpected-retained-proof action=keep-single-response-owner-and-fail-closed-drain",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_lxmf_event(
    lease: ApplicationEventLease<'_, 'static>,
    delayed_proofs: &mut DelayedProofOwner<'static>,
    retries: &mut LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    storage: &mut ProductStorageCoordinator,
    supervisor: &ProductSupervisor,
    radio_diagnostics: &RadioDiagnosticsCell,
    lora_interface: PacketInterfaceId,
    lxmf_destination: DestinationHash,
    authority_fault: &mut LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    service_fault_observed: &mut bool,
    attempted_retry_class: Option<LxmfRetryClass>,
    attempted_message_id: Option<reticulum_lxmf_model::MessageId>,
    attempted_admission_attempt: u8,
    attempted_admission_source: Option<DestinationHash>,
    now_ms: u64,
) -> bool {
    let event_id = lease.id();
    let sequence = lease.sequence();
    let resolver = |source: &[u8; 16]| supervisor.recall_identity(&DestinationHash::new(*source));
    let outcome = storage.commit_lxmf_event(
        lease,
        delayed_proofs,
        LocalDeliveryDestination::new(*lxmf_destination.as_bytes()),
        wire_limits(),
        &resolver,
        stamp_policy(),
    );
    match outcome {
        ProductLxmfAdmission::DeferredForCredentialMutation(lease) => {
            let (class, message_id) = retry_after_pre_io_deferral(
                attempted_retry_class,
                attempted_message_id,
                LxmfRetryClass::CrossStoreBusy,
            );
            quarantine_lxmf_retry(
                lease,
                retries,
                authority_fault,
                now_ms,
                class,
                message_id,
                0,
                None,
            );
        }
        ProductLxmfAdmission::DeferredForJournalMutation(lease) => {
            let (class, message_id) = retry_after_pre_io_deferral(
                attempted_retry_class,
                attempted_message_id,
                LxmfRetryClass::CrossStoreBusy,
            );
            quarantine_lxmf_retry(
                lease,
                retries,
                authority_fault,
                now_ms,
                class,
                message_id,
                0,
                None,
            );
        }
        ProductLxmfAdmission::RuntimeUnavailable(lease) => {
            lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
            warn!(
                "e290-node stage=lxmf-ingress status=DISCARDED slot={} generation={} sequence={} reason=store-unavailable proof=not-sent routing=continue",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
        }
        ProductLxmfAdmission::Ingress(DurableIngressOutcome::Durable(success)) => {
            if let Some(evidence) = success.proof_evidence().filter(|evidence| {
                config::inbound_proof_uses_lora_trace(evidence.interface().0, lora_interface)
            }) {
                let message_id = Some(*success.receipt().message_id().as_bytes());
                let committed_at_us = now_micros();
                let _ = radio_diagnostics.record_inbound_proof_stage(
                    committed_at_us,
                    RadioTraceInboundProofStage::DurableCommit,
                    message_id,
                    evidence,
                );
                let _ = radio_diagnostics.record_inbound_proof_stage(
                    committed_at_us,
                    RadioTraceInboundProofStage::ProofRetained,
                    message_id,
                    evidence,
                );
            }
            log_durable_lxmf(success);
        }
        ProductLxmfAdmission::Ingress(DurableIngressOutcome::Retained(retained)) => {
            let pending = storage.lxmf_pending_message_id();
            let action = retention_action(retained.reason(), pending);
            let deferred_source = admission_deferred_source(retained.reason());
            let (lease, reason) = retained.into_parts();
            match action {
                LxmfRetentionAction::Retry(class) => {
                    let (class, message_id) = if matches!(
                        class,
                        LxmfRetryClass::AdmissionDeferred | LxmfRetryClass::DelayedProofBusy
                    ) {
                        retry_after_pre_io_deferral(
                            attempted_retry_class,
                            attempted_message_id,
                            class,
                        )
                    } else if class == LxmfRetryClass::StoreReconcile {
                        (class, pending)
                    } else {
                        (class, None)
                    };
                    let (prior_admission_attempt, admission_source) =
                        if class == LxmfRetryClass::AdmissionDeferred {
                            let prior = if attempted_retry_class
                                == Some(LxmfRetryClass::AdmissionDeferred)
                                && attempted_admission_source == deferred_source
                            {
                                attempted_admission_attempt
                            } else {
                                0
                            };
                            (prior, deferred_source)
                        } else {
                            (0, None)
                        };
                    let retry_not_before_ms = quarantine_lxmf_retry(
                        lease,
                        retries,
                        authority_fault,
                        now_ms,
                        class,
                        message_id,
                        prior_admission_attempt,
                        admission_source,
                    );
                    warn!(
                        "e290-node stage=lxmf-ingress status=RETRY reason={reason:?} class={class:?} retry_not_before_ms={} proof=retained",
                        retry_not_before_ms,
                    );
                }
                LxmfRetentionAction::HoldPendingFault => {
                    let Some(message_id) = pending else {
                        lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
                        error!(
                            "e290-node stage=lxmf-ingress status=FAIL reason=pending-fault-without-message-id detail={reason:?} action=quarantine-event"
                        );
                        return true;
                    };
                    quarantine_lxmf_retry(
                        lease,
                        retries,
                        authority_fault,
                        u64::MAX,
                        LxmfRetryClass::StoreFaultHold,
                        Some(message_id),
                        0,
                        None,
                    );
                    error!(
                        "e290-node stage=lxmf-ingress status=FAIL-STOP reason={reason:?} message_id={:02x?} action=hold-exact-token-no-retry-until-reset other-flash-mutation=blocked routing=continue proof=not-sent",
                        message_id.as_bytes(),
                    );
                }
                LxmfRetentionAction::Terminal(terminal) => {
                    handle_terminal_lxmf_reject(
                        lease,
                        storage,
                        terminal,
                        &reason,
                        service_fault_observed,
                    );
                }
            }
        }
    }
    true
}

pub(crate) fn log_durable_lxmf(success: DurableIngressSuccess) {
    let receipt = success.receipt();
    info!(
        "e290-node stage=lxmf-ingress status=DURABLE kind={:?} slot={} generation={} message_id={:02x?} handle={} proof_ready={} proof_handoff=ordinary-supervisor",
        success.kind(),
        success.event_id().slot().get(),
        success.event_id().generation().get(),
        receipt.message_id().as_bytes(),
        receipt.handle().get(),
        success.queued_proof_id().is_some(),
    );
}

pub(crate) fn quarantine_lxmf_retry(
    lease: ApplicationEventLease<'_, 'static>,
    retries: &mut LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    authority_fault: &mut LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    now_ms: u64,
    class: LxmfRetryClass,
    message_id: Option<reticulum_lxmf_model::MessageId>,
    prior_admission_attempt: u8,
    admission_source: Option<DestinationHash>,
) -> u64 {
    let event_id = lease.id();
    let sequence = lease.sequence();
    let token = lease.quarantine_for_retry(LXMF_RETRY_QUARANTINE);
    let (retry_not_before_ms, admission_attempt, admission_source) = match class {
        LxmfRetryClass::StoreFaultHold => (u64::MAX, 0, None),
        LxmfRetryClass::AdmissionDeferred => {
            let attempt = prior_admission_attempt.saturating_add(1);
            let delay_ms = admission_retry_delay_ms(attempt, sequence);
            (now_ms.saturating_add(delay_ms), attempt, admission_source)
        }
        _ => (
            now_ms.saturating_add(config::STORAGE_RETRY_BACKOFF_MS),
            0,
            None,
        ),
    };
    retain_lxmf_retry_entry(
        retries,
        authority_fault,
        LxmfRetryEntry::new(token, retry_not_before_ms, class, message_id)
            .with_admission_state(admission_attempt, admission_source),
    );
    info!(
        "e290-node stage=lxmf-retry status=RETAINED slot={} generation={} class={class:?} admission_attempt={admission_attempt} admission_source={:?} retry_not_before_ms={retry_not_before_ms}",
        event_id.slot().get(),
        event_id.generation().get(),
        admission_source.map(|source| *source.as_bytes()),
    );
    retry_not_before_ms
}

pub(crate) fn retain_lxmf_retry_entry(
    retries: &mut LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    authority_fault: &mut LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    entry: LxmfRetryEntry<'static>,
) {
    if let Err(failure) = retries.try_insert(entry) {
        let retained = failure.into_entry();
        error!(
            "e290-node stage=lxmf-retry status=FAIL slot={} class={:?} reason=retry-set-slot-occupied action=retain-extra-authority-and-stop-lxmf",
            retained.slot(),
            retained.class(),
        );
        retain_lxmf_authority_fault(authority_fault, retained);
    }
}

pub(crate) fn retain_lxmf_authority_fault(
    authority_fault: &mut LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    entry: LxmfRetryEntry<'static>,
) {
    if let Err(failure) = authority_fault.try_capture(entry) {
        let retained_overflow_owner = failure.into_entry();
        error!(
            "e290-node stage=lxmf-authority status=FAIL reason=bounded-fault-owner-exhausted action=quarantined-capability-unrecoverable-continue-non-lxmf"
        );
        // At most 16 physical application-event tokens can exist, so a 16-slot
        // fault owner cannot overflow in a valid owner graph. If that invariant
        // is violated, the event itself remains quarantined in its caller-owned
        // slot even though this recovery capability becomes unreachable. Do
        // not globally spin the shared non-LXMF consumer.
        let _ = retained_overflow_owner;
    }
}

pub(crate) fn handle_terminal_lxmf_reject<E: core::fmt::Debug>(
    lease: ApplicationEventLease<'_, 'static>,
    storage: &mut ProductStorageCoordinator,
    terminal: LxmfTerminalReject,
    detail: &E,
    service_fault_observed: &mut bool,
) {
    let event_id = lease.id();
    match terminal {
        LxmfTerminalReject::InvalidMessage => {
            lease.discard(ApplicationEventDiscardReason::InvalidPayload);
            warn!(
                "e290-node stage=lxmf-ingress status=REJECTED slot={} generation={} reason={detail:?} proof=not-sent",
                event_id.slot().get(),
                event_id.generation().get(),
            );
        }
        LxmfTerminalReject::StoreFull
        | LxmfTerminalReject::IndexFull
        | LxmfTerminalReject::HandleExhausted => {
            lease.discard(ApplicationEventDiscardReason::DownstreamCapacity);
            warn!(
                "e290-node stage=lxmf-ingress status=CAPACITY-REJECTED slot={} generation={} policy={terminal:?} reason={detail:?} service=remain-enabled-for-replay proof=not-sent",
                event_id.slot().get(),
                event_id.generation().get(),
            );
        }
        LxmfTerminalReject::StoreFault => {
            let disabled = storage.disable_lxmf_service_if_clean();
            *service_fault_observed = true;
            lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            error!(
                "e290-node stage=lxmf-ingress status=QUARANTINED slot={} generation={} policy={terminal:?} reason={detail:?} service_disabled={disabled} proof=not-sent",
                event_id.slot().get(),
                event_id.generation().get(),
            );
        }
        LxmfTerminalReject::HashCollision => {
            lease.discard(ApplicationEventDiscardReason::InvalidPayload);
            warn!(
                "e290-node stage=lxmf-ingress status=COLLISION-REJECTED slot={} generation={} policy={terminal:?} reason={detail:?} service=remain-enabled-for-other-messages proof=not-sent",
                event_id.slot().get(),
                event_id.generation().get(),
            );
        }
        LxmfTerminalReject::Unrelated
        | LxmfTerminalReject::CandidateContradiction
        | LxmfTerminalReject::ProofInvariant => {
            let disabled = storage.disable_lxmf_service_if_clean();
            *service_fault_observed = true;
            lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            error!(
                "e290-node stage=lxmf-ingress status=QUARANTINED slot={} generation={} policy={terminal:?} reason={detail:?} service_disabled={disabled} proof=not-sent",
                event_id.slot().get(),
                event_id.generation().get(),
            );
        }
    }
}

pub(crate) fn step_authenticated_api(
    storage: &mut ProductStorageCoordinator,
    supervisor: &ProductSupervisor,
    radio_diagnostics: &RadioDiagnosticsCell,
    route_diagnostics: &mut RouteDiagnosticsSnapshot<{ config::PATHS }>,
    discovered_peers: &DiscoveredPeers<
        { config::LXMF_DISCOVERED_PEERS },
        { config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES },
    >,
    peer_discovery_incarnation: LxmfPeerDiscoveryIncarnation,
    now_ms: u64,
    manual_announce_schedule: &mut ManualAnnounceSchedule,
    announce_now_seconds: u64,
    nomad: &mut ProductNomadRuntimeState,
    nomad_api: &mut NomadFetchApiState,
    reticulum_probe: &mut ProductReticulumProbeState,
    nomad_service_enabled: bool,
    identity: IdentitySummary,
    handoff: &mut NodeHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    state: &mut AuthenticatedApiNodeState,
) -> bool {
    // Each invocation performs at most one ownership transition. This keeps a
    // pressured reply or terminal request resident without letting local API
    // traffic consume the node task's other fair lanes.
    if matches!(state, AuthenticatedApiNodeState::PendingReply(_)) {
        let AuthenticatedApiNodeState::PendingReply(reply) =
            mem::replace(state, AuthenticatedApiNodeState::Ready)
        else {
            unreachable!()
        };
        return match handoff.replies().try_send(reply) {
            Ok(()) => true,
            Err(pressure) => {
                *state = AuthenticatedApiNodeState::PendingReply(pressure.into_inner());
                false
            }
        };
    }

    if matches!(state, AuthenticatedApiNodeState::PendingRequest(_)) {
        let AuthenticatedApiNodeState::PendingRequest(request) =
            mem::replace(state, AuthenticatedApiNodeState::Ready)
        else {
            unreachable!()
        };
        let mut nomad_port = ProductNomadFetchPort::new(
            nomad,
            nomad_api,
            *peer_discovery_incarnation.as_bytes(),
            nomad_service_enabled,
        );
        let mut probe_port = ProductReticulumProbePort::new(
            reticulum_probe,
            *peer_discovery_incarnation.as_bytes(),
            now_ms,
            nomad_service_enabled,
        );
        let dispatch = storage.dispatch_authenticated_request(
            supervisor,
            radio_diagnostics,
            route_diagnostics,
            discovered_peers,
            peer_discovery_incarnation,
            now_ms,
            manual_announce_schedule,
            announce_now_seconds,
            &mut nomad_port,
            &mut probe_port,
            request,
            identity,
        );
        match dispatch {
            Ok(reply) => *state = AuthenticatedApiNodeState::PendingReply(reply),
            Err(failure) => {
                let kind = failure.kind();
                *state = AuthenticatedApiNodeState::Quarantined {
                    request: failure.into_request(),
                    fault: kind,
                };
                error!(
                    "e290-node stage=authenticated-api status=FAIL-STOP reason={kind:?} action=retain-request-and-close-api-lane"
                );
            }
        }
        return true;
    }

    if let AuthenticatedApiNodeState::Quarantined { request, fault } = state {
        let _ = (request, fault);
        return false;
    }
    if let Some(request) = handoff.requests().try_receive() {
        *state = AuthenticatedApiNodeState::PendingRequest(request);
        return true;
    }
    false
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_one_reticulum_probe(
    supervisor: &mut ProductSupervisor,
    state: &mut ProductReticulumProbeState,
    retry_actions_a: &mut Option<RetainedActions>,
    retry_actions_b: &mut Option<RetainedActions>,
    fail_closed_draining: &mut bool,
    ordinary_owners_quiescent: bool,
    rng: &mut Trng,
    now_ms: u64,
) -> bool {
    match state.next_drive(now_ms) {
        ReticulumProbeDrive::Idle | ReticulumProbeDrive::AwaitingAttempt => false,
        ReticulumProbeDrive::ResolveIdentity {
            destination,
            request_path,
        } => match supervisor.prepare_proof_probe_destination_for(&destination) {
            Ok(probe_destination) => {
                match state.identity_resolved(destination, probe_destination, now_ms) {
                    Ok(()) => info!(
                        "e290-node stage=reticulum-proof-probe status=IDENTITY-READY requested={:02x?} probe_destination={:02x?} path=fresh-lookup-required",
                        destination.as_bytes(),
                        probe_destination.as_bytes(),
                    ),
                    Err(reason) => {
                        error!(
                            "e290-node stage=reticulum-proof-probe status=FAIL reason={reason:?} source=identity-transition action=fail-closed-drain"
                        );
                        *fail_closed_draining = true;
                    }
                }
                true
            }
            Err(ProofProbeIdentityAliasError::SourceIdentityUnknown)
                if request_path && ordinary_owners_quiescent =>
            {
                offer_reticulum_probe_path_request(
                    supervisor,
                    state,
                    retry_actions_a,
                    retry_actions_b,
                    fail_closed_draining,
                    destination,
                    "identity",
                    rng,
                    now_ms,
                )
            }
            Err(ProofProbeIdentityAliasError::SourceIdentityUnknown) => false,
            Err(reason) => {
                error!(
                    "e290-node stage=reticulum-proof-probe status=FAILED reason=identity-alias:{reason:?} action=report-internal"
                );
                if state
                    .fail_before_attempt(reticulum_device_api::ProbeFailure::Internal)
                    .is_err()
                {
                    *fail_closed_draining = true;
                }
                true
            }
        },
        ReticulumProbeDrive::ResolvePath {
            destination,
            request_path,
        } => {
            if supervisor.has_usable_path(&destination) {
                match state.path_resolved(destination, now_ms) {
                    Ok(()) => info!(
                        "e290-node stage=reticulum-proof-probe status=PATH-READY probe_destination={:02x?} route_hops={:?}",
                        destination.as_bytes(),
                        supervisor.retained_path_hops(&destination),
                    ),
                    Err(reason) => {
                        error!(
                            "e290-node stage=reticulum-proof-probe status=FAIL reason={reason:?} source=path-transition action=fail-closed-drain"
                        );
                        *fail_closed_draining = true;
                    }
                }
                true
            } else if request_path && ordinary_owners_quiescent {
                offer_reticulum_probe_path_request(
                    supervisor,
                    state,
                    retry_actions_a,
                    retry_actions_b,
                    fail_closed_draining,
                    destination,
                    "probe-destination",
                    rng,
                    now_ms,
                )
            } else {
                false
            }
        }
        ReticulumProbeDrive::Prepare { destination } => {
            let route_hops = match (
                supervisor.has_usable_path(&destination),
                supervisor.retained_path_hops(&destination),
            ) {
                (true, Some(route_hops)) => route_hops,
                _ => {
                    warn!(
                        "e290-node stage=reticulum-proof-probe status=FAILED reason=path-lost-before-prepare probe_destination={:02x?} public=NoPath",
                        destination.as_bytes(),
                    );
                    if state
                        .fail_before_attempt(reticulum_device_api::ProbeFailure::NoPath)
                        .is_err()
                    {
                        *fail_closed_draining = true;
                    }
                    return true;
                }
            };
            let mut payload = [0_u8; PROBE_PAYLOAD_BYTES];
            rng.fill_bytes(&mut payload);
            let deadline = TxLeaseDeadline::new(MonotonicMillis::new(
                now_ms.saturating_add(PROBE_DATA_OWNER_LEASE_MS),
            ));
            let result = supervisor.try_prepare_data(
                DataRouterPrepareRequest {
                    destination,
                    plaintext: &payload,
                    rns_now: MonotonicSeconds::new(now_ms / 1_000),
                    deadline,
                },
                MonotonicMillis::new(now_ms),
                rng,
            );
            payload.fill(0);
            match result {
                NodeInterfaceDataPrepareResult::Coordinator(DataRouterPrepareResult::Prepared(
                    hop,
                )) => {
                    match state.prepared(destination, hop.prepared(), route_hops, now_ms) {
                        Ok(()) => info!(
                            "e290-node stage=reticulum-proof-probe status=PREPARED probe_destination={:02x?} slot={} route_hops={} owner_deadline_ms={} durable_submission_journal=bypassed",
                            destination.as_bytes(),
                            hop.slot_id().get(),
                            route_hops,
                            deadline.instant().get(),
                        ),
                        Err(reason) => {
                            error!(
                                "e290-node stage=reticulum-proof-probe status=FAIL reason={reason:?} source=prepared-owner-binding action=fail-closed-drain"
                            );
                            *fail_closed_draining = true;
                        }
                    }
                    true
                }
                NodeInterfaceDataPrepareResult::Coordinator(
                    DataRouterPrepareResult::NoAvailable,
                ) => false,
                NodeInterfaceDataPrepareResult::Coordinator(
                    DataRouterPrepareResult::Rejected { reason, .. },
                ) => {
                    let failure = match reason {
                        SubmitError::NoEligibleInterface { .. } => {
                            reticulum_device_api::ProbeFailure::Dispatch
                        }
                        SubmitError::UnknownDestination => {
                            reticulum_device_api::ProbeFailure::IdentityUnavailable
                        }
                        _ => reticulum_device_api::ProbeFailure::Internal,
                    };
                    warn!(
                        "e290-node stage=reticulum-proof-probe status=FAILED reason=prepare:{reason:?} public={failure:?}"
                    );
                    if state.fail_before_attempt(failure).is_err() {
                        *fail_closed_draining = true;
                    }
                    true
                }
                NodeInterfaceDataPrepareResult::Coordinator(
                    DataRouterPrepareResult::RejectedQuarantined {
                        reason,
                        observation,
                    },
                ) => {
                    error!(
                        "e290-node stage=reticulum-proof-probe status=FAIL reason=prepare-quarantined:{reason:?} recovery={observation:?} action=fail-closed-drain"
                    );
                    *fail_closed_draining = true;
                    true
                }
                NodeInterfaceDataPrepareResult::Coordinator(
                    DataRouterPrepareResult::OwnerMismatch | DataRouterPrepareResult::Disabled(_),
                )
                | NodeInterfaceDataPrepareResult::Fault(_) => {
                    error!(
                        "e290-node stage=reticulum-proof-probe status=FAIL reason=data-coordinator-unavailable action=fail-closed-drain"
                    );
                    *fail_closed_draining = true;
                    true
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn offer_reticulum_probe_path_request(
    supervisor: &mut ProductSupervisor,
    state: &mut ProductReticulumProbeState,
    retry_actions_a: &mut Option<RetainedActions>,
    retry_actions_b: &mut Option<RetainedActions>,
    fail_closed_draining: &mut bool,
    destination: DestinationHash,
    lookup: &'static str,
    rng: &mut Trng,
    now_ms: u64,
) -> bool {
    let actions = match supervisor.request_path(&destination, rng) {
        Ok(actions) => actions,
        Err(reason) => {
            error!(
                "e290-node stage=reticulum-proof-probe status=FAILED reason=path-request-build:{reason:?} lookup={lookup} action=report-internal"
            );
            if state
                .fail_before_attempt(reticulum_device_api::ProbeFailure::Internal)
                .is_err()
            {
                *fail_closed_draining = true;
            }
            return true;
        }
    };
    if state.path_request_attempted(destination, now_ms).is_err() {
        error!(
            "e290-node stage=reticulum-proof-probe status=FAIL reason=path-request-state-correlation lookup={lookup} action=retain-actions-and-fail-closed-drain"
        );
        *fail_closed_draining = true;
    }
    match supervisor.try_offer_actions(actions, config::ordinary_admission(now_ms)) {
        Ok(()) => {
            info!(
                "e290-node stage=reticulum-proof-probe status=PATH-REQUEST-QUEUED lookup={lookup} destination={:02x?} retry_after_ms={}",
                destination.as_bytes(),
                reticulum_e290_firmware::reticulum_probe::PROBE_PATH_REQUEST_INTERVAL_MS,
            );
        }
        Err(failure) => match handle_action_offer_failure(failure, "reticulum-proof-probe-path") {
            ActionOfferHandling::Retry(retained) => {
                if retry_actions_a.is_none() {
                    *retry_actions_a = Some(retained);
                } else if retry_actions_b.is_none() {
                    *retry_actions_b = Some(retained);
                } else {
                    error!(
                        "e290-node stage=reticulum-proof-probe status=FAIL reason=retry-owner-capacity-contradiction action=fail-closed-drain"
                    );
                    *fail_closed_draining = true;
                }
            }
            ActionOfferHandling::RetainAndDrain(retained) => {
                if retry_actions_a.is_none() {
                    *retry_actions_a = Some(retained);
                } else if retry_actions_b.is_none() {
                    *retry_actions_b = Some(retained);
                }
                *fail_closed_draining = true;
            }
        },
    }
    true
}

pub(crate) fn drive_reticulum_probe_recovery(
    supervisor: &mut ProductSupervisor,
    state: &mut ProductReticulumProbeState,
    fail_closed_draining: &mut bool,
) -> bool {
    let mut exact = None;
    for observation in supervisor.recovered_data_observations() {
        match state.classifies_recovery(observation) {
            Ok(true) => {
                exact = Some(observation);
                break;
            }
            Ok(false) => {}
            Err(reason) => {
                error!(
                    "e290-node stage=reticulum-proof-probe-recovery status=FAIL reason={reason:?} action=fail-closed-drain"
                );
                *fail_closed_draining = true;
                return true;
            }
        }
    }
    let Some(observation) = exact else {
        return false;
    };
    if let Err(reason) = state.observe_recovery(observation) {
        error!(
            "e290-node stage=reticulum-proof-probe-recovery status=FAIL reason={reason:?} source=tracker action=fail-closed-drain"
        );
        *fail_closed_draining = true;
        return true;
    }
    match supervisor.acknowledge_recovered_data(observation) {
        Ok(()) => warn!(
            "e290-node stage=reticulum-proof-probe-recovery status=ACKNOWLEDGED reason={:?} receipt=await-terminal durable_submission_journal=bypassed",
            observation.record().reason(),
        ),
        Err(reason) => {
            error!(
                "e290-node stage=reticulum-proof-probe-recovery status=FAIL reason=ack:{reason:?} action=fail-closed-drain"
            );
            *fail_closed_draining = true;
        }
    }
    true
}

pub(crate) fn drive_reticulum_probe_terminal(
    supervisor: &mut ProductSupervisor,
    state: &mut ProductReticulumProbeState,
    fail_closed_draining: &mut bool,
    now_ms: u64,
) -> bool {
    let mut exact = None;
    for terminal in supervisor.terminal_attempts() {
        match state.classifies_terminal(terminal) {
            Ok(true) => {
                exact = Some(terminal);
                break;
            }
            Ok(false) => {}
            Err(reason) => {
                error!(
                    "e290-node stage=reticulum-proof-probe-terminal status=FAIL reason={reason:?} action=fail-closed-drain"
                );
                *fail_closed_draining = true;
                return true;
            }
        }
    }
    let Some(terminal) = exact else {
        return false;
    };
    if let Err(reason) = state.observe_terminal(terminal, now_ms) {
        error!(
            "e290-node stage=reticulum-proof-probe-terminal status=FAIL reason={reason:?} source=tracker action=fail-closed-drain"
        );
        *fail_closed_draining = true;
        return true;
    }
    let acknowledged = match supervisor.acknowledge_terminal(terminal.handle()) {
        Ok(acknowledged) if acknowledged == terminal => acknowledged,
        Ok(acknowledged) => {
            error!(
                "e290-node stage=reticulum-proof-probe-terminal status=FAIL reason=ack-mismatch expected={terminal:?} observed={acknowledged:?} action=fail-closed-drain"
            );
            *fail_closed_draining = true;
            return true;
        }
        Err(reason) => {
            error!(
                "e290-node stage=reticulum-proof-probe-terminal status=FAIL reason=ack:{reason:?} action=fail-closed-drain"
            );
            *fail_closed_draining = true;
            return true;
        }
    };
    match state.finalize_terminal(acknowledged) {
        Ok(response) => info!(
            "e290-node stage=reticulum-proof-probe-terminal status=COMPLETE result={response:?} signal_semantics=proof-receiver-local-final-return-hop durable_submission_journal=bypassed"
        ),
        Err(reason) => {
            error!(
                "e290-node stage=reticulum-proof-probe-terminal status=FAIL reason={reason:?} source=finalize-after-ack action=fail-closed-drain"
            );
            *fail_closed_draining = true;
        }
    }
    true
}

pub(crate) fn step_pairing_frontier(
    storage: &mut ProductStorageCoordinator,
    control_handoff: &mut NodePairingHandoff<CriticalSectionRawMutex>,
    live_handoff: &mut NodeLivePairingHandoff<CriticalSectionRawMutex>,
    #[cfg(feature = "appliance")] ble_bond_handoff: &mut NodeBleBondHandoff<
        CriticalSectionRawMutex,
    >,
    session_admission_handoff: &mut NodeSessionAdmissionHandoff<CriticalSectionRawMutex>,
    state: &mut PairingNodeState,
    rng: &mut Trng,
    now_millis: u64,
) -> bool {
    let mut progressed = flush_pairing_reply(control_handoff, &mut state.pending_control_reply);
    progressed |= flush_live_pairing_reply(live_handoff, &mut state.pending_live_reply);
    #[cfg(feature = "appliance")]
    {
        progressed |= flush_ble_bond_reply(ble_bond_handoff, &mut state.pending_ble_bond_reply);
    }
    progressed |= flush_session_admission_reply(
        session_admission_handoff,
        &mut state.pending_session_admission_reply,
    );

    // A freshly authenticated BLE link is not reboot-safe until the sole flash
    // owner has committed its exact bond. Keep all live-pairing and ordinary
    // session admission behind that owner, including while its terminal reply
    // is backpressured. This also prevents application credentials from being
    // activated on a link whose transport authentication would disappear at
    // the next reset.
    #[cfg(feature = "appliance")]
    {
        if state.pending_ble_bond_command.is_none() && state.pending_ble_bond_reply.is_none() {
            state.pending_ble_bond_command = ble_bond_handoff.try_receive_command();
        }
        if let Some(command) = state.pending_ble_bond_command.take() {
            let (connection, bond) = command.into_parts();
            let outcome = match storage.commit_ble_bond(bond) {
                ProductBleBondCommitOutcome::Committed { .. } => BleBondCommitOutcome::Durable,
                ProductBleBondCommitOutcome::ReconciledRebootRequired { .. }
                | ProductBleBondCommitOutcome::StoreUnavailable { .. } => {
                    BleBondCommitOutcome::Failed
                }
            };
            debug_assert!(state.pending_ble_bond_reply.is_none());
            state.pending_ble_bond_reply = Some(BleBondCommitReply::new(connection, outcome));
            progressed = true;
            progressed |= flush_ble_bond_reply(ble_bond_handoff, &mut state.pending_ble_bond_reply);
            return progressed;
        }
        if state.pending_ble_bond_reply.is_some() {
            return progressed;
        }
    }

    let initialization_status = storage.initialization_status();
    if matches!(
        initialization_status,
        CredentialInitializationStatus::InFlight { .. }
    ) && state
        .initialization_retry_not_before_ms
        .is_none_or(|deadline| now_millis >= deadline)
    {
        let _ = drive_initialization_and_schedule(
            storage,
            &mut state.initialization_retry_not_before_ms,
            now_millis,
        );
        progressed = true;
    } else if !matches!(
        initialization_status,
        CredentialInitializationStatus::InFlight { .. }
    ) {
        state.initialization_retry_not_before_ms = None;
    }

    if state.pending_control_command.is_none() && state.pending_control_reply.is_none() {
        state.pending_control_command = control_handoff.try_receive_command();
    }
    if state.pending_live_command.is_none()
        && state.pending_live_operation.is_none()
        && state.pending_live_reply.is_none()
    {
        state.pending_live_command = live_handoff.try_receive_command();
    }
    if state.pending_session_admission_command.is_none()
        && state.pending_session_admission_reply.is_none()
    {
        state.pending_session_admission_command = session_admission_handoff.try_receive_command();
    }

    // All command channels have the same sole bearer producer. Compare captured
    // event time anyway so a queued earlier wire request or scalar event cannot
    // be overtaken. Equal timestamps choose wire work, most notably before a
    // bus-reset disconnect observed in the same millisecond.
    for _ in 0..3 {
        let next_lane = select_pairing_command_lane(
            state
                .pending_session_admission_command
                .as_ref()
                .map(|admission| admission.at().get()),
            state
                .pending_live_command
                .as_ref()
                .map(|live| live.at().get()),
            state
                .pending_control_command
                .as_ref()
                .map(|control| control.at().get()),
        );
        if next_lane == Some(PairingCommandLane::SessionAdmission) {
            let command = state
                .pending_session_admission_command
                .take()
                .expect("the selected session-admission command is retained");
            let connection = command.connection();
            let outcome = match storage.select_ordinary_session(
                command.at(),
                connection,
                command.credential_id(),
            ) {
                Ok(selected) => SessionAdmissionOutcome::Selected(selected),
                Err(_) => SessionAdmissionOutcome::Refused,
            };
            state.pending_session_admission_reply =
                Some(SessionAdmissionReply::new(connection, outcome));
            progressed = true;
        } else if next_lane == Some(PairingCommandLane::Live)
            && state
                .live_retry_not_before_ms
                .is_none_or(|deadline| now_millis >= deadline)
        {
            progressed |= admit_live_pairing_command(storage, state, rng, now_millis);
            if state.pending_live_command.is_some() {
                // A conflicting cross-store mutation is causally after this
                // request. Keep later policy events and timeout polls behind
                // the retained command until storage ownership settles.
                break;
            }
        } else if next_lane == Some(PairingCommandLane::Control) {
            let command = state
                .pending_control_command
                .take()
                .expect("the selected control command is retained");
            handle_pairing_control_command(storage, state, command, now_millis);
            progressed = true;
        } else {
            break;
        }
    }

    progressed |= drive_live_pairing_and_schedule(storage, state, now_millis);
    progressed |= flush_pairing_reply(control_handoff, &mut state.pending_control_reply);
    progressed |= flush_live_pairing_reply(live_handoff, &mut state.pending_live_reply);
    progressed |= flush_session_admission_reply(
        session_admission_handoff,
        &mut state.pending_session_admission_reply,
    );

    if state.pending_live_command.is_none()
        && state.pending_control_command.is_none()
        && state.pending_session_admission_command.is_none()
    {
        progressed |= poll_pairing_timeout(storage, &mut state.pending_exclusive, now_millis);
    }
    progressed
}

pub(crate) fn handle_pairing_control_command(
    storage: &mut ProductStorageCoordinator,
    state: &mut PairingNodeState,
    command: PairingControlCommand,
    now_millis: u64,
) {
    let connection = command.connection();
    let reply_kind = match command {
        PairingControlCommand::Connected { at, connection } => {
            state.pending_exclusive = None;
            let _ = storage.pairing_connected(at, connection);
            PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Connected)
        }
        PairingControlCommand::Disconnected { at, connection } => {
            if state
                .pending_exclusive
                .as_ref()
                .is_some_and(|(owner, _)| *owner == connection)
            {
                state.pending_exclusive = None;
            }
            let _ = storage.pairing_disconnected(at, connection);
            PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Disconnected)
        }
        PairingControlCommand::ObserveButton {
            at,
            connection,
            level,
        } => {
            let outcome = match storage.pairing_observe_button(at, level) {
                Some(ButtonEffect::AcquirePairingExclusive(capability)) => {
                    state.pending_exclusive = Some((connection, capability));
                    ButtonObservationReply::AcquireExclusive
                }
                Some(ButtonEffect::Closed(_) | ButtonEffect::Fault(_)) => {
                    if state
                        .pending_exclusive
                        .as_ref()
                        .is_some_and(|(owner, _)| *owner == connection)
                    {
                        state.pending_exclusive = None;
                    }
                    ButtonObservationReply::Observed
                }
                Some(ButtonEffect::None) | None => ButtonObservationReply::Observed,
            };
            PairingControlReplyKind::Button(outcome)
        }
        PairingControlCommand::ExclusiveAcquired { at, connection } => {
            let outcome = if state
                .pending_exclusive
                .as_ref()
                .is_some_and(|(owner, _)| *owner == connection)
            {
                let (_, capability) = state
                    .pending_exclusive
                    .take()
                    .expect("the matching exclusive capability was retained");
                match storage.pairing_exclusive_acquired(at, capability) {
                    Some(ExclusiveAcquireOutcome::Opened(_)) => ExclusiveAcquisitionReply::Opened,
                    Some(ExclusiveAcquireOutcome::Closed(_)) => ExclusiveAcquisitionReply::Closed,
                    Some(ExclusiveAcquireOutcome::Stale | ExclusiveAcquireOutcome::Fault(_))
                    | None => ExclusiveAcquisitionReply::Refused,
                }
            } else {
                ExclusiveAcquisitionReply::Refused
            };
            PairingControlReplyKind::Exclusive(outcome)
        }
        PairingControlCommand::Control {
            at,
            connection,
            request,
        } => {
            let response = match request {
                ControlRequest::Status { sequence } => ControlResponse::status(
                    sequence,
                    public_initialization_status(storage.initialization_status()),
                ),
                ControlRequest::Initialize { sequence } => {
                    let result = match storage.request_initialization(at, connection) {
                        ProductInitializationRequest::IdentityUnavailable
                        | ProductInitializationRequest::DeferredForJournalMutation => {
                            InitializeResult::Retrying
                        }
                        ProductInitializationRequest::DeferredForLxmfMutation => {
                            info!(
                                "e290-node stage=credential-initialization status=DEFERRED reason=lxmf-mutation-in-flight"
                            );
                            InitializeResult::Retrying
                        }
                        ProductInitializationRequest::Runtime(Err(refusal)) => {
                            public_initialization_refusal(refusal)
                        }
                        ProductInitializationRequest::Runtime(Ok(_accepted)) => {
                            drive_initialization_and_schedule(
                                storage,
                                &mut state.initialization_retry_not_before_ms,
                                now_millis,
                            )
                        }
                    };
                    ControlResponse::initialize(sequence, result)
                }
            };
            PairingControlReplyKind::Control(response)
        }
    };
    debug_assert!(state.pending_control_reply.is_none());
    state.pending_control_reply = Some(PairingControlReply::new(connection, reply_kind));
}

pub(crate) fn admit_live_pairing_command(
    storage: &mut ProductStorageCoordinator,
    state: &mut PairingNodeState,
    rng: &mut Trng,
    now_millis: u64,
) -> bool {
    let command = state
        .pending_live_command
        .take()
        .expect("live admission requires one retained command");
    let at = command.at();
    let bearer = command.bearer();
    let connection = command.connection();
    let request = command.into_request();
    let sequence = request.sequence();
    let kind = LiveRequestKind::from_request(&request);

    if state.live_lane_faulted {
        state.pending_live_reply = Some(LivePairingReply::new(
            connection,
            kind.blocked_response(sequence),
        ));
        return true;
    }
    state.live_retry_not_before_ms = None;

    match storage.request_live_pairing(at, bearer, connection, request, rng) {
        ProductLivePairingAdmission::DeferredForJournalMutation(request) => {
            state.pending_live_command =
                Some(LivePairingCommand::new(at, bearer, connection, request));
        }
        ProductLivePairingAdmission::DeferredForLxmfMutation(request) => {
            let retry_not_before_ms = now_millis.saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
            state.pending_live_command =
                Some(LivePairingCommand::new(at, bearer, connection, request));
            state.live_retry_not_before_ms = Some(retry_not_before_ms);
            info!(
                "e290-node stage=credential-live-pairing status=DEFERRED reason=lxmf-mutation-in-flight retry_not_before_ms={retry_not_before_ms}"
            );
        }
        ProductLivePairingAdmission::Immediate(response) => {
            if response.sequence() == sequence && kind.matches_response(&response) {
                state.pending_live_reply = Some(LivePairingReply::new(connection, response));
            } else {
                drop(response);
                state.live_lane_faulted = true;
                state.pending_live_reply = Some(LivePairingReply::new(
                    connection,
                    kind.blocked_response(sequence),
                ));
            }
        }
        ProductLivePairingAdmission::MutationAccepted(mutation) => {
            if kind.expected_mutation() == Some(mutation) {
                state.pending_live_operation = Some(LivePairingOperation::new(
                    bearer, connection, sequence, mutation,
                ));
                state.live_retry_not_before_ms = None;
            } else {
                state.live_lane_faulted = true;
                state.pending_live_reply = Some(LivePairingReply::new(
                    connection,
                    kind.blocked_response(sequence),
                ));
            }
        }
    }
    true
}

pub(crate) fn drive_live_pairing_and_schedule(
    storage: &mut ProductStorageCoordinator,
    state: &mut PairingNodeState,
    now_millis: u64,
) -> bool {
    if state.pending_live_reply.is_some()
        || state
            .live_retry_not_before_ms
            .is_some_and(|deadline| now_millis < deadline)
    {
        return false;
    }
    let status = storage.live_pairing_status();
    let drive_required = state.pending_live_operation.is_some()
        || matches!(
            status,
            CredentialPairingStatus::AwaitingCleanStore(_)
                | CredentialPairingStatus::MutationPrepared(_)
                | CredentialPairingStatus::ReconcileRequired(_)
                | CredentialPairingStatus::CleanupRequired
        );
    if !drive_required {
        if status == CredentialPairingStatus::Blocked {
            state.live_lane_faulted = true;
        }
        state.live_retry_not_before_ms = None;
        return false;
    }

    let outcome = storage.drive_live_pairing();
    let Some(operation) = state.pending_live_operation.take() else {
        // Cleanup without a request is expected. Any mutation-bearing state
        // without correlation is an internal fault, but must keep driving to a
        // terminal durable state so secret/store ownership is not abandoned.
        match outcome {
            reticulum_e290_firmware::credential_pairing::CredentialPairingDriveOutcome::CleanupCompleted => {
                state.live_retry_not_before_ms = None;
            }
            reticulum_e290_firmware::credential_pairing::CredentialPairingDriveOutcome::Retry {
                mutation,
                ..
            } => {
                if mutation.is_some() {
                    state.live_lane_faulted = true;
                }
                state.live_retry_not_before_ms =
                    Some(now_millis.saturating_add(config::STORAGE_RETRY_BACKOFF_MS));
            }
            other => {
                drop(other);
                state.live_lane_faulted = true;
                state.live_retry_not_before_ms = None;
            }
        }
        return true;
    };

    match operation.apply(outcome) {
        LivePairingOperationStep::Progress(operation) => {
            state.pending_live_operation = Some(operation);
            state.live_retry_not_before_ms = None;
        }
        LivePairingOperationStep::Retry { operation, reason } => {
            state.pending_live_operation = Some(operation);
            state.live_retry_not_before_ms =
                Some(now_millis.saturating_add(config::STORAGE_RETRY_BACKOFF_MS));
            warn!("e290-node stage=live-pairing status=RETRY reason={reason:?}");
        }
        LivePairingOperationStep::Reply(reply) => {
            state.pending_live_reply = Some(reply);
            state.live_retry_not_before_ms = None;
        }
        LivePairingOperationStep::Fault(reply) => {
            state.live_lane_faulted = true;
            state.pending_live_reply = Some(reply);
            state.live_retry_not_before_ms = None;
            error!(
                "e290-node stage=live-pairing status=FAIL-CLOSED reason=drive-correlation-mismatch"
            );
        }
    }
    true
}

pub(crate) fn poll_pairing_timeout(
    storage: &mut ProductStorageCoordinator,
    pending_exclusive: &mut Option<(ConnectionId, AcquirePairingExclusive)>,
    now_millis: u64,
) -> bool {
    // Run after any queued command carrying an earlier observation timestamp,
    // so asynchronous handoff delay cannot manufacture a clock regression.
    // This node-owned poll still runs every loop even when a host stops reading
    // and back-pressures the USB TX FIFO indefinitely.
    if matches!(
        storage.pairing_poll_timeout(PairingMillis::new(now_millis)),
        Some(PolicyEvent::Closed(_) | PolicyEvent::Fault(_))
    ) {
        *pending_exclusive = None;
        true
    } else {
        false
    }
}

pub(crate) fn flush_pairing_reply(
    handoff: &mut NodePairingHandoff<CriticalSectionRawMutex>,
    pending_reply: &mut Option<PairingControlReply>,
) -> bool {
    let Some(reply) = pending_reply.take() else {
        return false;
    };
    match handoff.try_send_reply(reply) {
        Ok(()) => true,
        Err(pressure) => {
            *pending_reply = Some(pressure.into_inner());
            false
        }
    }
}

pub(crate) fn flush_live_pairing_reply(
    handoff: &mut NodeLivePairingHandoff<CriticalSectionRawMutex>,
    pending_reply: &mut Option<LivePairingReply>,
) -> bool {
    let Some(reply) = pending_reply.take() else {
        return false;
    };
    match handoff.try_send_reply(reply) {
        Ok(()) => true,
        Err(pressure) => {
            *pending_reply = Some(pressure.into_inner());
            false
        }
    }
}

#[cfg(feature = "appliance")]
pub(crate) fn flush_ble_bond_reply(
    handoff: &mut NodeBleBondHandoff<CriticalSectionRawMutex>,
    pending_reply: &mut Option<BleBondCommitReply>,
) -> bool {
    let Some(reply) = pending_reply.take() else {
        return false;
    };
    match handoff.try_send_reply(reply) {
        Ok(()) => true,
        Err(pressure) => {
            *pending_reply = Some(pressure.into_inner());
            false
        }
    }
}

pub(crate) fn flush_session_admission_reply(
    handoff: &mut NodeSessionAdmissionHandoff<CriticalSectionRawMutex>,
    pending_reply: &mut Option<SessionAdmissionReply>,
) -> bool {
    let Some(reply) = pending_reply.take() else {
        return false;
    };
    match handoff.try_send_reply(reply) {
        Ok(()) => true,
        Err(pressure) => {
            *pending_reply = Some(pressure.into_inner());
            false
        }
    }
}

pub(crate) fn drive_initialization_and_schedule(
    storage: &mut ProductStorageCoordinator,
    retry_not_before_ms: &mut Option<u64>,
    now_millis: u64,
) -> InitializeResult {
    let result = match storage.drive_initialization() {
        ProductInitializationDrive::IdentityUnavailable => InitializeResult::Retrying,
        ProductInitializationDrive::Runtime(outcome) => public_initialization_drive(outcome),
    };
    if result == InitializeResult::Retrying
        && matches!(
            storage.initialization_status(),
            CredentialInitializationStatus::InFlight { .. }
        )
    {
        *retry_not_before_ms = Some(now_millis.saturating_add(config::STORAGE_RETRY_BACKOFF_MS));
    } else {
        *retry_not_before_ms = None;
    }
    result
}

pub(crate) fn authorized_frame_durability(
    retained: &Option<AuthorizedFrameObservation>,
    acknowledgement: &Option<AuthorizedFrameObservation>,
) -> AuthorizedFrameDurability {
    if retained.is_some() {
        AuthorizedFrameDurability::Unresolved
    } else if acknowledgement.is_some() {
        AuthorizedFrameDurability::DurableAcknowledgementPending
    } else {
        AuthorizedFrameDurability::None
    }
}

const NOMAD_LINK_PREPARATION_PROTOCOL_CODE: u16 = 1;
const NOMAD_LINK_PREPARATION_STATE_CODE: u16 = 2;
const NOMAD_LINK_PREPARATION_ROLLBACK_CODE: u16 = 3;
const NOMAD_REQUEST_LINK_UNAVAILABLE_CODE: u16 = 1;
const NOMAD_REQUEST_PREPARATION_SIZE_CODE: u16 = 2;
const NOMAD_REQUEST_PREPARATION_CRYPTO_CODE: u16 = 3;
const NOMAD_REQUEST_PREPARATION_PACKET_CODE: u16 = 4;
const NOMAD_REQUEST_PREPARATION_INVARIANT_CODE: u16 = 5;
const NOMAD_REQUEST_DISPATCH_TERMINAL_CODE: u16 = 1;
const NOMAD_REQUEST_DISPATCH_NATIVE_CODE: u16 = 3;
const NOMAD_LINK_DISPATCH_CORRELATION_CODE: u16 = 4;
const NOMAD_LINK_TIMEOUT_CODE: u16 = 1;

#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_one_nomad_command(
    supervisor: &mut ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    retry_actions_a: &mut Option<RetainedActions>,
    retry_actions_b: &mut Option<RetainedActions>,
    pending_protocol_dispatch: &mut Option<OrdinaryProtocolDispatch>,
    fail_closed_draining: &mut bool,
    native_terminal_events_pending: bool,
    ordinary_owners_quiescent: bool,
    rng: &mut Trng,
    now_ms: u64,
) -> bool {
    let exact_undispatched_path_owner = [
        retry_actions_a
            .as_ref()
            .and_then(RetainedActions::protocol_dispatch),
        retry_actions_b
            .as_ref()
            .and_then(RetainedActions::protocol_dispatch),
        *pending_protocol_dispatch,
    ]
    .into_iter()
    .flatten()
    .any(|protocol| {
        matches!(
            protocol,
            OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Path { .. })
        )
    });
    let usable_path = nomad.active_destination().is_some_and(|destination| {
        supervisor.has_usable_path(&DestinationHash::new(*destination.as_bytes()))
    });
    let command = match nomad.next_step(
        NomadMillis::new(now_ms),
        usable_path,
        exact_undispatched_path_owner,
    ) {
        Ok(NomadDriveStep::Progressed) => return true,
        Ok(NomadDriveStep::Command(command)) => command,
        Ok(NomadDriveStep::Idle) => return false,
        Err(fault) => {
            error!("e290-node stage=nomad-path-observation status=FAULT reason={fault:?}");
            return true;
        }
    };
    let fresh_command = matches!(
        command,
        CoordinatorCommand::RequestPath { .. }
            | CoordinatorCommand::EstablishLink { .. }
            | CoordinatorCommand::PrepareAnonymousRequest { .. }
    );
    if fresh_command && !ordinary_owners_quiescent {
        return false;
    }
    if native_terminal_events_pending
        && matches!(
            command,
            CoordinatorCommand::AbortTimedOutLink { .. }
                | CoordinatorCommand::CancelTimedOutRequest { .. }
        )
    {
        return false;
    }

    match command {
        CoordinatorCommand::RequestPath { destination } => {
            let native_destination = DestinationHash::new(*destination.as_bytes());
            if supervisor.has_usable_path(&native_destination) {
                if let Err(fault) = nomad.coordinator_mut().path_already_available(destination) {
                    error!(
                        "e290-node stage=nomad-path status=FAULT reason=already-available-transition destination={:02x?} fault={fault:?}",
                        destination.as_bytes(),
                    );
                }
                return true;
            }
            match supervisor.request_path(&native_destination, rng) {
                Ok(actions) => offer_nomad_actions(
                    supervisor,
                    nomad,
                    actions,
                    OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Path { destination }),
                    retry_actions_a,
                    retry_actions_b,
                    pending_protocol_dispatch,
                    fail_closed_draining,
                    now_ms,
                ),
                Err(reason) => {
                    warn!(
                        "e290-node stage=nomad-path status=DEFERRED reason=prepare:{reason:?} destination={:02x?}",
                        destination.as_bytes(),
                    );
                    true
                }
            }
        }
        CoordinatorCommand::EstablishLink { destination } => {
            let native_destination = DestinationHash::new(*destination.as_bytes());
            match supervisor.initiate_link(
                &native_destination,
                MonotonicSeconds::new(now_ms / 1_000),
                rng,
            ) {
                Ok((actions, link)) => offer_nomad_actions(
                    supervisor,
                    nomad,
                    actions,
                    OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Link {
                        destination,
                        link,
                    }),
                    retry_actions_a,
                    retry_actions_b,
                    pending_protocol_dispatch,
                    fail_closed_draining,
                    now_ms,
                ),
                Err(
                    InitiateLinkError::LinkTableFull { .. }
                    | InitiateLinkError::LinkIdCollision
                    | InitiateLinkError::ActionAllocationFailed,
                ) => false,
                Err(reason) => {
                    let code = match reason {
                        InitiateLinkError::Protocol => NOMAD_LINK_PREPARATION_PROTOCOL_CODE,
                        InitiateLinkError::LinkStateNotRetained => {
                            NOMAD_LINK_PREPARATION_STATE_CODE
                        }
                        InitiateLinkError::RollbackFailed => NOMAD_LINK_PREPARATION_ROLLBACK_CODE,
                        InitiateLinkError::LinkTableFull { .. }
                        | InitiateLinkError::LinkIdCollision
                        | InitiateLinkError::ActionAllocationFailed => unreachable!(),
                    };
                    if let Err(fault) = nomad.coordinator_mut().link_preparation_failed(
                        destination,
                        LinkFailure::new(LinkFailureStage::Preparation, code),
                    ) {
                        error!(
                            "e290-node stage=nomad-link status=FAULT reason=preparation-transition destination={:02x?} fault={fault:?}",
                            destination.as_bytes(),
                        );
                    }
                    true
                }
            }
        }
        CoordinatorCommand::PrepareAnonymousRequest {
            destination: _,
            link,
            path,
            requested_at,
        } => {
            let native_link = LinkHandle::new(*link.as_bytes());
            match supervisor.prepare_anonymous_request(
                native_link,
                path.as_str(),
                requested_at.as_seconds_f64(),
                rng,
            ) {
                Ok(Ok(prepared)) => {
                    let handle = prepared.handle();
                    let (actions, _) = prepared.into_parts();
                    match nomad.coordinator_mut().request_prepared(
                        link,
                        RequestId::new(*handle.request()),
                        handle,
                    ) {
                        Ok(_) => offer_nomad_actions(
                            supervisor,
                            nomad,
                            actions,
                            OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Request {
                                handle,
                            }),
                            retry_actions_a,
                            retry_actions_b,
                            pending_protocol_dispatch,
                            fail_closed_draining,
                            now_ms,
                        ),
                        Err(fault) => {
                            let reconciliation = supervisor.reconcile_request_dispatch(handle);
                            let acknowledgement = nomad
                                .coordinator_mut()
                                .acknowledge_invariant_request_cancellation(handle);
                            let expected_reconciliation =
                                reconciliation == RequestDispatchReconciliation::ReclaimedPrepared;
                            if !expected_reconciliation {
                                *fail_closed_draining = true;
                            }
                            error!(
                                "e290-node stage=nomad-request status=FAULT reason=request-prepared-transition link={:02x?} request={:02x?} fault={fault:?} native_reconciliation={reconciliation:?} expected_prepared={expected_reconciliation} cleanup_ack={acknowledgement:?}",
                                handle.link(),
                                handle.request(),
                            );
                            true
                        }
                    }
                }
                Ok(Err(
                    PrepareRequestError::LinkNotFound
                    | PrepareRequestError::LinkNotActive
                    | PrepareRequestError::LinkInterfaceUnknown,
                )) => {
                    if let Err(fault) = nomad
                        .coordinator_mut()
                        .request_link_unavailable(link, NOMAD_REQUEST_LINK_UNAVAILABLE_CODE)
                    {
                        error!(
                            "e290-node stage=nomad-request status=FAULT reason=link-unavailable-transition link={:02x?} fault={fault:?}",
                            link.as_bytes(),
                        );
                    }
                    true
                }
                Ok(Err(
                    PrepareRequestError::DispatchTableFull { .. }
                    | PrepareRequestError::RequestAlreadyTracked
                    | PrepareRequestError::RequestAllocationFailed
                    | PrepareRequestError::ActionAllocationFailed,
                )) => false,
                Ok(Err(reason)) => {
                    let code = match reason {
                        PrepareRequestError::RequestTooLarge { .. } => {
                            NOMAD_REQUEST_PREPARATION_SIZE_CODE
                        }
                        PrepareRequestError::Crypto => NOMAD_REQUEST_PREPARATION_CRYPTO_CODE,
                        PrepareRequestError::PacketBuild => NOMAD_REQUEST_PREPARATION_PACKET_CODE,
                        PrepareRequestError::Invariant => NOMAD_REQUEST_PREPARATION_INVARIANT_CODE,
                        PrepareRequestError::LinkNotFound
                        | PrepareRequestError::LinkNotActive
                        | PrepareRequestError::LinkInterfaceUnknown
                        | PrepareRequestError::DispatchTableFull { .. }
                        | PrepareRequestError::RequestAlreadyTracked
                        | PrepareRequestError::RequestAllocationFailed
                        | PrepareRequestError::ActionAllocationFailed => unreachable!(),
                    };
                    if let Err(fault) = nomad.coordinator_mut().request_preparation_failed(
                        link,
                        RequestFailure::new(RequestFailureStage::Preparation, code),
                    ) {
                        error!(
                            "e290-node stage=nomad-request status=FAULT reason=preparation-transition link={:02x?} fault={fault:?}",
                            link.as_bytes(),
                        );
                    }
                    true
                }
                Err(fault) => {
                    error!(
                        "e290-node stage=nomad-request status=DEFERRED reason=supervisor-fault fault={fault:?}"
                    );
                    false
                }
            }
        }
        CoordinatorCommand::ExpirePath { candidate } => {
            if let Err(fault) = nomad.coordinator_mut().confirm_path_timeout(candidate) {
                error!("e290-node stage=nomad-path status=FAULT reason=timeout fault={fault:?}");
            }
            true
        }
        CoordinatorCommand::AbortTimedOutLink { candidate } => {
            let link = LinkHandle::new(*candidate.link().as_bytes());
            if supervisor.abort_unestablished_link(link) {
                if let Err(fault) = nomad
                    .coordinator_mut()
                    .confirm_link_timeout_after_abort(candidate, NOMAD_LINK_TIMEOUT_CODE)
                {
                    error!(
                        "e290-node stage=nomad-link status=FAULT reason=timeout-after-abort fault={fault:?}"
                    );
                }
                true
            } else {
                let native_state = supervisor.link_state(link);
                let fault = nomad.coordinator_mut().link_native_reconciliation_failed(
                    candidate.destination(),
                    candidate.link(),
                    CoordinatorOperation::LinkFailed,
                );
                *fail_closed_draining = true;
                error!(
                    "e290-node stage=nomad-link status=FAULT reason=timeout-native-phase link={:02x?} native_state={native_state:?} fault={fault:?} action=no-timeout-outcome-and-fail-closed-drain",
                    link.as_bytes(),
                );
                true
            }
        }
        CoordinatorCommand::AbortLinkForInvariant { candidate } => {
            let link = LinkHandle::new(*candidate.link().as_bytes());
            let aborted = supervisor.abort_unestablished_link(link);
            if aborted || supervisor.link_state(link).is_none() {
                if let Err(fault) = nomad
                    .coordinator_mut()
                    .acknowledge_invariant_link_abort(candidate)
                {
                    error!(
                        "e290-node stage=nomad-link status=FAULT reason=invariant-abort-ack fault={fault:?}"
                    );
                }
                true
            } else {
                let native_state = supervisor.link_state(link);
                let acknowledgement = nomad
                    .coordinator_mut()
                    .acknowledge_invariant_link_abort(candidate);
                *fail_closed_draining = true;
                error!(
                    "e290-node stage=nomad-link status=FAULT reason=invariant-abort-native-phase link={:02x?} native_state={native_state:?} cleanup_ack={acknowledgement:?} action=retain-sticky-fault-and-require-reset",
                    link.as_bytes(),
                );
                true
            }
        }
        CoordinatorCommand::CancelTimedOutRequest { token, candidate } => {
            let reconciliation = supervisor.reconcile_request_dispatch(token);
            if reconciliation == RequestDispatchReconciliation::ReclaimedConfirmed {
                if let Err(fault) = nomad
                    .coordinator_mut()
                    .confirm_request_timeout_after_native_cancel(token, candidate)
                {
                    *fail_closed_draining = true;
                    error!(
                        "e290-node stage=nomad-request status=FAULT reason=timeout-after-reclaim native_reconciliation={reconciliation:?} fault={fault:?} action=fail-closed-drain"
                    );
                }
            } else {
                let fault = nomad
                    .coordinator_mut()
                    .request_native_reconciliation_failed(
                        token,
                        NativeRequestPhase::Confirmed,
                        CoordinatorOperation::ConfirmRequestTimeout,
                    );
                *fail_closed_draining = true;
                error!(
                    "e290-node stage=nomad-request status=FAULT reason=timeout-native-phase native_reconciliation={reconciliation:?} fault={fault:?} action=no-timeout-outcome-and-fail-closed-drain"
                );
            }
            true
        }
        CoordinatorCommand::CancelRequestForInvariant {
            token,
            link: _,
            request: _,
            phase: _,
        } => {
            let reconciliation = supervisor.reconcile_request_dispatch(token);
            if let Err(fault) = nomad
                .coordinator_mut()
                .acknowledge_invariant_request_cancellation(token)
            {
                error!(
                    "e290-node stage=nomad-request status=FAULT reason=invariant-reclaim-ack native_reconciliation={reconciliation:?} fault={fault:?}"
                );
            }
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn offer_nomad_actions(
    supervisor: &mut ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    actions: NodeActions,
    protocol: OrdinaryProtocolDispatch,
    retry_actions_a: &mut Option<RetainedActions>,
    retry_actions_b: &mut Option<RetainedActions>,
    pending_protocol_dispatch: &mut Option<OrdinaryProtocolDispatch>,
    fail_closed_draining: &mut bool,
    now_ms: u64,
) -> bool {
    match supervisor.try_offer_actions(actions, config::ordinary_admission(now_ms)) {
        Ok(()) => {
            debug_assert!(pending_protocol_dispatch.is_none());
            *pending_protocol_dispatch = Some(protocol);
        }
        Err(failure) => {
            let handling = handle_action_offer_failure(failure, "nomad-command");
            let (retained, terminal) = match handling {
                ActionOfferHandling::Retry(retained) => {
                    (retained.with_protocol_dispatch(protocol), false)
                }
                ActionOfferHandling::RetainAndDrain(retained) => {
                    let _ = handle_terminal_protocol_dispatch(supervisor, nomad, protocol, true);
                    (retained, true)
                }
            };
            if retry_actions_a.is_none() {
                *retry_actions_a = Some(retained);
            } else {
                debug_assert!(retry_actions_b.is_none());
                *retry_actions_b = Some(retained);
            }
            if terminal {
                *fail_closed_draining = true;
                error!(
                    "e290-node stage=nomad-command status=FAIL-STOP reason=ordinary-offer-terminal"
                );
            }
        }
    }
    true
}

pub(crate) fn confirm_nomad_dispatch(
    supervisor: &mut ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    protocol: NomadProtocolDispatch,
    first_dispatch: bool,
    protocol_token_present: bool,
    protocol_confirmed: bool,
    native_terminal_events_pending: bool,
    dispatched_at: MonotonicMillis,
) -> NomadDispatchResolution {
    match protocol {
        NomadProtocolDispatch::Path { destination } => {
            if !first_dispatch || protocol_token_present {
                let dispatch = nomad
                    .coordinator_mut()
                    .path_request_dispatched(destination, NomadMillis::new(dispatched_at.get()));
                let terminal = dispatch.and_then(|()| {
                    nomad
                        .coordinator_mut()
                        .path_unavailable(destination)
                        .map(|_| ())
                });
                error!(
                    "e290-node stage=nomad-path status=FAULT reason=dispatch-correlation first_dispatch={first_dispatch} protocol_token_present={protocol_token_present} terminal_reconciliation={terminal:?}"
                );
                return NomadDispatchResolution::InvariantReconciled;
            }
            match nomad
                .coordinator_mut()
                .path_request_dispatched(destination, NomadMillis::new(dispatched_at.get()))
            {
                Ok(()) => NomadDispatchResolution::Committed,
                Err(fault) => {
                    error!("e290-node stage=nomad-path status=FAULT reason={fault:?}");
                    NomadDispatchResolution::InvariantReconciled
                }
            }
        }
        NomadProtocolDispatch::Link { destination, link } => {
            if !first_dispatch || !protocol_token_present || !protocol_confirmed {
                let aborted = supervisor.abort_unestablished_link(link);
                let terminal = nomad.coordinator_mut().link_preparation_failed(
                    destination,
                    LinkFailure::new(
                        LinkFailureStage::Dispatch,
                        NOMAD_LINK_DISPATCH_CORRELATION_CODE,
                    ),
                );
                error!(
                    "e290-node stage=nomad-link status=FAULT reason=dispatch-correlation first_dispatch={first_dispatch} protocol_token_present={protocol_token_present} protocol_confirmed={protocol_confirmed} pending_link_aborted={aborted} terminal_reconciliation={terminal:?}"
                );
                return NomadDispatchResolution::InvariantReconciled;
            }
            let result = nomad.coordinator_mut().link_request_dispatched(
                destination,
                LinkId::new(*link.as_bytes()),
                NomadMillis::new(dispatched_at.get()),
            );
            match result {
                Ok(()) => NomadDispatchResolution::Committed,
                Err(fault) => {
                    let cleanup = nomad.next_command(NomadMillis::new(dispatched_at.get()));
                    let cleanup_transferred = if let Some(
                        CoordinatorCommand::AbortLinkForInvariant { candidate },
                    ) = cleanup
                    {
                        let aborted = supervisor.abort_unestablished_link(link);
                        if aborted {
                            let acknowledgement = nomad
                                .coordinator_mut()
                                .acknowledge_invariant_link_abort(candidate);
                            error!(
                                "e290-node stage=nomad-link status=FAULT reason=dispatch-transition fault={fault:?} pending_link_aborted=true cleanup_ack={acknowledgement:?}"
                            );
                            false
                        } else {
                            error!(
                                "e290-node stage=nomad-link status=FAULT reason=dispatch-transition fault={fault:?} pending_link_aborted=false action=retain-coordinator-cleanup"
                            );
                            true
                        }
                    } else {
                        error!(
                            "e290-node stage=nomad-link status=FAULT reason=dispatch-transition fault={fault:?} cleanup_command={cleanup:?}"
                        );
                        false
                    };
                    if cleanup_transferred {
                        NomadDispatchResolution::CleanupTransferred
                    } else {
                        NomadDispatchResolution::InvariantReconciled
                    }
                }
            }
        }
        NomadProtocolDispatch::Request { handle } => {
            if !first_dispatch || protocol_token_present {
                let reconciliation = supervisor.reconcile_request_dispatch(handle);
                let fault = nomad
                    .coordinator_mut()
                    .request_native_reconciliation_failed(
                        handle,
                        NativeRequestPhase::Prepared,
                        CoordinatorOperation::ConfirmRequestDispatch,
                    );
                error!(
                    "e290-node stage=nomad-request status=FAULT reason=dispatch-correlation first_dispatch={first_dispatch} protocol_token_present={protocol_token_present} native_reconciliation={reconciliation:?} fault={fault:?}"
                );
                return NomadDispatchResolution::InvariantReconciled;
            }
            match supervisor.confirm_request_dispatch(
                handle,
                MonotonicSeconds::new(dispatched_at.get() / 1_000),
                first_dispatch,
            ) {
                Ok(RequestDispatchConfirmation::Confirmed) => {
                    match nomad
                        .coordinator_mut()
                        .request_dispatch_confirmed(handle, NomadMillis::new(dispatched_at.get()))
                    {
                        Ok(()) => NomadDispatchResolution::Committed,
                        Err(fault) => {
                            let reconciliation = supervisor.reconcile_request_dispatch(handle);
                            let acknowledgement = nomad
                                .coordinator_mut()
                                .acknowledge_invariant_request_cancellation(handle);
                            error!(
                                "e290-node stage=nomad-request status=FAULT reason=coordinator-dispatch-confirmation fault={fault:?} native_reconciliation={reconciliation:?} cleanup_ack={acknowledgement:?}"
                            );
                            NomadDispatchResolution::InvariantReconciled
                        }
                    }
                }
                Ok(RequestDispatchConfirmation::NotFirstDispatch) => {
                    let reconciliation = supervisor.reconcile_request_dispatch(handle);
                    let fault = nomad
                        .coordinator_mut()
                        .request_native_reconciliation_failed(
                            handle,
                            NativeRequestPhase::Prepared,
                            CoordinatorOperation::ConfirmRequestDispatch,
                        );
                    error!(
                        "e290-node stage=nomad-request status=FAULT reason=native-not-first-after-router-first native_reconciliation={reconciliation:?} fault={fault:?}"
                    );
                    NomadDispatchResolution::InvariantReconciled
                }
                Err(RequestDispatchError::NotTracked) if native_terminal_events_pending => {
                    match nomad
                        .coordinator_mut()
                        .request_dispatch_confirmed(handle, NomadMillis::new(dispatched_at.get()))
                    {
                        Ok(()) => {
                            warn!(
                                "e290-node stage=nomad-request status=TERMINAL-EVENT-PENDING reason=native-request-already-reclaimed action=preserve-product-correlation-until-event-drain"
                            );
                            NomadDispatchResolution::Committed
                        }
                        Err(fault) => {
                            error!(
                                "e290-node stage=nomad-request status=FAULT reason=terminal-event-pending-transition fault={fault:?}"
                            );
                            NomadDispatchResolution::InvariantReconciled
                        }
                    }
                }
                Err(reason) => {
                    let reconciliation = supervisor.reconcile_request_dispatch(handle);
                    let lifecycle_race = matches!(
                        reason,
                        RequestDispatchError::LinkNotFound | RequestDispatchError::LinkNotActive
                    );
                    if lifecycle_race && reconciliation == RequestDispatchReconciliation::Absent {
                        let terminal = nomad
                            .coordinator_mut()
                            .request_dispatch_failed_after_native_reclaim(
                                handle,
                                RequestFailure::new(
                                    RequestFailureStage::Dispatch,
                                    NOMAD_REQUEST_DISPATCH_NATIVE_CODE,
                                ),
                            );
                        error!(
                            "e290-node stage=nomad-request status=RECONCILED reason=native-dispatch-confirmation:{reason:?} native_reconciliation={reconciliation:?} terminal_reconciliation={terminal:?}"
                        );
                        if terminal.is_ok() {
                            NomadDispatchResolution::Reconciled
                        } else {
                            NomadDispatchResolution::InvariantReconciled
                        }
                    } else {
                        let fault = nomad
                            .coordinator_mut()
                            .request_native_reconciliation_failed(
                                handle,
                                NativeRequestPhase::Prepared,
                                CoordinatorOperation::ConfirmRequestDispatch,
                            );
                        error!(
                            "e290-node stage=nomad-request status=FAULT reason=native-dispatch-confirmation:{reason:?} native_reconciliation={reconciliation:?} fault={fault:?}"
                        );
                        NomadDispatchResolution::InvariantReconciled
                    }
                }
            }
        }
    }
}

pub(crate) fn handle_terminal_protocol_dispatch(
    supervisor: &mut ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    protocol: OrdinaryProtocolDispatch,
    terminal: bool,
) -> bool {
    match protocol {
        OrdinaryProtocolDispatch::Submission(SubmissionProtocolDispatch::Path { .. }) => false,
        OrdinaryProtocolDispatch::Submission(SubmissionProtocolDispatch::Link { offer, link }) => {
            let aborted = supervisor.abort_unestablished_link(link);
            warn!(
                "e290-node stage=link-establishment status=ABORT submission={} generation={} link={:02x?} pending_link_aborted={aborted}",
                offer.id().get(),
                offer.generation(),
                link.as_bytes(),
            );
            false
        }
        OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Path { destination }) => {
            warn!(
                "e290-node stage=nomad-path status=RELEASED reason=pre-dispatch-return destination={:02x?}",
                destination.as_bytes(),
            );
            false
        }
        OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Link { destination, link }) => {
            let aborted = supervisor.abort_unestablished_link(link);
            let native_state = supervisor.link_state(link);
            let reconciliation_fault = (!aborted && native_state.is_some()).then(|| {
                nomad.coordinator_mut().link_native_reconciliation_failed(
                    destination,
                    LinkId::new(*link.as_bytes()),
                    CoordinatorOperation::ConfirmLinkRequest,
                )
            });
            warn!(
                "e290-node stage=nomad-link status=ABORT reason=pre-dispatch-return destination={:02x?} link={:02x?} pending_link_aborted={aborted} native_state={native_state:?} reconciliation_fault={reconciliation_fault:?}",
                destination.as_bytes(),
                link.as_bytes(),
            );
            reconciliation_fault.is_some()
        }
        OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Request { handle }) => {
            let reconciliation = supervisor.reconcile_request_dispatch(handle);
            let expected_pre_dispatch =
                reconciliation == RequestDispatchReconciliation::ReclaimedPrepared;
            let result = if !expected_pre_dispatch {
                Err(nomad
                    .coordinator_mut()
                    .request_native_reconciliation_failed(
                        handle,
                        NativeRequestPhase::Prepared,
                        CoordinatorOperation::CancelRequestDispatch,
                    ))
            } else if terminal {
                nomad
                    .coordinator_mut()
                    .request_dispatch_failed_after_native_reclaim(
                        handle,
                        RequestFailure::new(
                            RequestFailureStage::Dispatch,
                            NOMAD_REQUEST_DISPATCH_TERMINAL_CODE,
                        ),
                    )
            } else {
                nomad
                    .coordinator_mut()
                    .request_dispatch_canceled_after_native_cancel(handle)
            };
            if let Err(fault) = result {
                error!(
                    "e290-node stage=nomad-request status=FAULT reason=pre-dispatch-reconciliation terminal={terminal} native_reconciliation={reconciliation:?} expected_pre_dispatch={expected_pre_dispatch} fault={fault:?}"
                );
            }
            !expected_pre_dispatch || result.is_err()
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn disable_submission_for_path_fault(
    storage: &mut ProductStorageCoordinator,
    durability_service: &mut DurabilityServiceState,
    retained_frame: &Option<AuthorizedFrameObservation>,
    pending_frame_acknowledgement: &Option<AuthorizedFrameObservation>,
    supervisor: &mut ProductSupervisor,
    online: InterfaceDescriptor,
    fail_closed_draining: &mut bool,
    trigger: &'static str,
) {
    storage.disable_submission_service();
    let previous = *durability_service;
    *durability_service = (*durability_service).permanent_failure(authorized_frame_durability(
        retained_frame,
        pending_frame_acknowledgement,
    ));
    if !previous.is_active_owner_fail_stopped() && durability_service.is_active_owner_fail_stopped()
    {
        enter_active_owner_durability_fail_stop(supervisor, online, fail_closed_draining, trigger);
    }
}

pub(crate) fn enter_active_owner_durability_fail_stop(
    supervisor: &mut ProductSupervisor,
    online: InterfaceDescriptor,
    fail_closed_draining: &mut bool,
    trigger: &'static str,
) {
    match supervisor.disable_interface(online.lease()) {
        Ok(offline) => error!(
            "e290-node stage=durability-policy status=ACTIVE-OWNER-FAIL-STOP trigger={trigger} interface={} generation={} online={} action=retain-frame-completion-ticket-and-deny-fresh-work",
            offline.lease().interface().get(),
            offline.lease().generation().get(),
            offline.is_online(),
        ),
        Err(reason) => error!(
            "e290-node stage=durability-policy status=FAIL trigger={trigger} reason=interface-offline:{reason:?} action=retain-frame-completion-ticket-and-fail-closed-drain"
        ),
    }
    *fail_closed_draining = true;
}

pub(crate) fn enter_protocol_dispatch_fail_stop(
    supervisor: &mut ProductSupervisor,
    fail_closed_draining: &mut bool,
    routed_interface: reticulum_node_core::PacketInterfaceId,
    first_dispatch: bool,
    interval_ordered: bool,
) {
    let Some(routed_descriptor) = supervisor.interface_registry().descriptor(routed_interface)
    else {
        error!(
            "e290-node stage=protocol-dispatch-confirmation status=FAIL reason=routed-interface-unregistered routed_interface={} first_dispatch={} interval_ordered={} edge=router-egress-acceptance-not-rf-txdone action=fail-closed-drain",
            routed_interface.get(),
            first_dispatch,
            interval_ordered,
        );
        *fail_closed_draining = true;
        return;
    };
    match supervisor.disable_interface(routed_descriptor.lease()) {
        Ok(offline) => error!(
            "e290-node stage=protocol-dispatch-confirmation status=INTERFACE-FAIL-STOP routed_interface={} interface={} generation={} online={} first_dispatch={} interval_ordered={} edge=router-egress-acceptance-not-rf-txdone action=deny-fresh-interface-work-and-drain-retained-owners",
            routed_interface.get(),
            offline.lease().interface().get(),
            offline.lease().generation().get(),
            offline.is_online(),
            first_dispatch,
            interval_ordered,
        ),
        Err(reason) => error!(
            "e290-node stage=protocol-dispatch-confirmation status=FAIL trigger=interface-offline:{reason:?} routed_interface={} first_dispatch={} interval_ordered={} edge=router-egress-acceptance-not-rf-txdone action=fail-closed-drain",
            routed_interface.get(),
            first_dispatch,
            interval_ordered,
        ),
    }
    *fail_closed_draining = true;
}

pub(crate) fn step_ingress(
    supervisor: &mut ProductSupervisor,
    rng: &mut Trng,
    mut state: IngressDrainState<'_>,
) -> bool {
    let link_now = MonotonicInstant::from_micros(now_micros());
    match supervisor.step_ingress_at(
        MonotonicSeconds::new(link_now.as_secs()),
        link_now,
        config::ordinary_admission(link_now.as_micros() / 1_000),
        rng,
    ) {
        NodeInterfaceIngressStep::Idle => false,
        NodeInterfaceIngressStep::RecycleBackpressured(fault) => {
            observe_retryable_recycle_fault(&mut *state.observed_recycle_fault, fault);
            false
        }
        NodeInterfaceIngressStep::TerminalRecyclePending(fault) => {
            *state.fail_closed_draining = true;
            quarantine_terminal_ingress_buffer(
                supervisor,
                fault,
                &mut *state.quarantined_ingress_buffer,
            )
        }
        NodeInterfaceIngressStep::ActionsBackpressured(_) => false,
        NodeInterfaceIngressStep::TerminalActionsPending(fault) => {
            *state.fail_closed_draining = true;
            if state.local_quarantine_available {
                quarantine_terminal_ingress_actions(
                    supervisor,
                    fault,
                    &mut *state.quarantined_actions,
                )
            } else {
                error!(
                    "e290-node stage=ingress-actions status=TERMINAL-RETAINED reason={fault:?} owner=supervisor-residue action=fail-closed-drain"
                );
                false
            }
        }
        NodeInterfaceIngressStep::RouteRejected {
            reason,
            recycle_fault,
            ..
        } => {
            if let Some(fault) = recycle_fault {
                handle_ingress_recycle_fault(supervisor, &mut state, fault);
            }
            warn!("e290-node stage=ingress status=ROUTE-REJECTED reason={reason:?}");
            true
        }
        NodeInterfaceIngressStep::CorrelationRejected {
            reason,
            recycle_fault,
            ..
        } => {
            *state.fail_closed_draining = true;
            observe_terminal_correlation_fault(
                &mut *state.terminal_correlation_fault,
                reason,
                "ingress",
            );
            let retryable_recycle_pending = match recycle_fault {
                Some(fault) if fault.is_retryable() => {
                    observe_retryable_recycle_fault(&mut *state.observed_recycle_fault, fault);
                    true
                }
                Some(fault) => {
                    let _ = quarantine_terminal_ingress_buffer(
                        supervisor,
                        fault,
                        &mut *state.quarantined_ingress_buffer,
                    );
                    false
                }
                None => false,
            };
            match config::terminal_ingress_disposition(retryable_recycle_pending) {
                config::TerminalIngressDisposition::DeferUntilBufferRecycled => {
                    *state.correlation_recycle_pending = true;
                    error!(
                        "e290-node stage=ingress status=CORRELATION-REJECTED-DEFERRED reason={reason:?} action=recycle-exact-buffer-then-fail-closed-drain"
                    );
                }
                config::TerminalIngressDisposition::HandleTerminal => {
                    error!(
                        "e290-node stage=ingress status=CORRELATION-REJECTED reason={reason:?} action=fail-closed-drain"
                    );
                }
            }
            true
        }
        NodeInterfaceIngressStep::Processed(processed) => {
            let recycle_fault = processed.recycle_fault();
            let retryable_recycle_pending = match recycle_fault {
                Some(fault) if fault.is_retryable() => {
                    observe_retryable_recycle_fault(&mut *state.observed_recycle_fault, fault);
                    true
                }
                Some(fault) => {
                    *state.fail_closed_draining = true;
                    let _ = quarantine_terminal_ingress_buffer(
                        supervisor,
                        fault,
                        &mut *state.quarantined_ingress_buffer,
                    );
                    false
                }
                None => false,
            };
            let terminal_action_fault = processed.terminal_action_fault();
            let report = processed.into_report();
            let _ = report;
            if let Some(fault) = terminal_action_fault {
                *state.fail_closed_draining = true;
                match config::terminal_ingress_disposition(retryable_recycle_pending) {
                    config::TerminalIngressDisposition::DeferUntilBufferRecycled => {
                        warn!(
                            "e290-node stage=ingress-actions status=TERMINAL-DEFERRED reason={fault:?} owner=supervisor-residue action=recycle-exact-buffer-before-quarantine"
                        );
                    }
                    config::TerminalIngressDisposition::HandleTerminal => {
                        if state.local_quarantine_available {
                            let _ = quarantine_terminal_ingress_actions(
                                supervisor,
                                fault,
                                &mut *state.quarantined_actions,
                            );
                        } else {
                            error!(
                                "e290-node stage=ingress-actions status=TERMINAL-RETAINED reason={fault:?} owner=supervisor-residue action=fail-closed-drain"
                            );
                        }
                    }
                }
            }
            true
        }
        NodeInterfaceIngressStep::RecycleRetried(buffer) => {
            if let Some(fault) = state.observed_recycle_fault.take() {
                info!(
                    "e290-node stage=ingress-recycle status=RECOVERED buffer={buffer:?} previous_reason={:?}",
                    fault.reason(),
                );
            }
            if core::mem::take(&mut *state.correlation_recycle_pending)
                && let Some(reason) = state.terminal_correlation_fault.as_ref()
            {
                error!(
                    "e290-node stage=ingress status=CORRELATION-REJECTED-RECYCLED reason={reason:?} buffer={buffer:?} action=continue-fail-closed-drain"
                );
            }
            true
        }
        NodeInterfaceIngressStep::RetainedActionsAdmitted => true,
    }
}

pub(crate) fn handle_ingress_recycle_fault(
    supervisor: &mut ProductSupervisor,
    state: &mut IngressDrainState<'_>,
    fault: NodeInterfaceIngressRecycleFault,
) {
    if fault.is_retryable() {
        observe_retryable_recycle_fault(&mut *state.observed_recycle_fault, fault);
    } else {
        *state.fail_closed_draining = true;
        let _ = quarantine_terminal_ingress_buffer(
            supervisor,
            fault,
            &mut *state.quarantined_ingress_buffer,
        );
    }
}

pub(crate) fn observe_retryable_recycle_fault(
    observed: &mut Option<NodeInterfaceIngressRecycleFault>,
    fault: NodeInterfaceIngressRecycleFault,
) {
    debug_assert!(fault.is_retryable());
    if *observed != Some(fault) {
        warn!(
            "e290-node stage=ingress-recycle status=BACKPRESSURED buffer={:?} reason={:?} action=retain-and-retry",
            fault.buffer(),
            fault.reason(),
        );
        *observed = Some(fault);
    }
}

pub(crate) fn quarantine_terminal_ingress_buffer(
    supervisor: &mut ProductSupervisor,
    observed: NodeInterfaceIngressRecycleFault,
    quarantined: &mut Option<QuarantinedIngressBuffer>,
) -> bool {
    if quarantined.is_some() {
        error!(
            "e290-node stage=ingress-recycle status=TERMINAL-RETAINED observed={observed:?} owner=supervisor-residue reason=local-quarantine-occupied action=fail-closed-drain"
        );
        return false;
    }
    match supervisor.take_terminal_ingress_buffer() {
        Some((retained, packet)) => {
            error!(
                "e290-node stage=ingress-recycle status=TERMINAL-QUARANTINED observed={observed:?} retained={retained:?} buffer={:?} bytes={} action=fail-closed-drain",
                packet.id(),
                packet.len(),
            );
            *quarantined = Some((retained, packet));
            true
        }
        None => {
            error!(
                "e290-node stage=ingress-recycle status=FAIL observed={observed:?} reason=terminal-buffer-missing action=fail-closed-drain"
            );
            false
        }
    }
}

pub(crate) fn quarantine_terminal_ingress_actions(
    supervisor: &mut ProductSupervisor,
    observed: NodeInterfaceIngressActionFault,
    quarantined_actions: &mut Option<RetainedActions>,
) -> bool {
    match supervisor.take_terminal_ingress_actions() {
        Some(terminal) => {
            let retained = terminal.fault();
            let (actions, admission) = terminal.into_parts();
            error!(
                "e290-node stage=ingress-actions status=TERMINAL-QUARANTINED observed={observed:?} retained={retained:?} packets={} events={} unroutable={} action=fail-closed-drain",
                actions.packets.len(),
                actions.events.len(),
                actions.unroutable_packets,
            );
            debug_assert!(quarantined_actions.is_none());
            *quarantined_actions = Some(RetainedActions::ordinary(actions, admission));
            true
        }
        None => {
            error!(
                "e290-node stage=ingress-actions status=FAIL observed={observed:?} reason=terminal-owner-missing action=fail-closed-drain"
            );
            false
        }
    }
}

pub(crate) fn handle_action_offer_failure(
    failure: NodeInterfaceOrdinaryOfferFailure,
    stage: &'static str,
) -> ActionOfferHandling {
    let reason = failure.reason();
    let (actions, admission) = failure.into_parts();
    match config::ordinary_offer_disposition(reason) {
        config::OrdinaryOfferDisposition::RetryBusy => {
            ActionOfferHandling::Retry(RetainedActions::ordinary(actions, admission))
        }
        config::OrdinaryOfferDisposition::QuarantineAndDrain => {
            error!(
                "e290-node stage={stage} status=TERMINAL-OFFER reason={reason:?} packets={} events={} unroutable={} action=quarantine-owner-and-fail-closed-drain",
                actions.packets.len(),
                actions.events.len(),
                actions.unroutable_packets,
            );
            ActionOfferHandling::RetainAndDrain(RetainedActions::ordinary(actions, admission))
        }
    }
}

pub(crate) fn retry_retained_actions(
    supervisor: &mut ProductSupervisor,
    retained: &mut Option<RetainedActions>,
    stage: &'static str,
) -> ActionRetryStep {
    if retained
        .as_ref()
        .and_then(RetainedActions::protocol_dispatch)
        .is_some()
        && !ordinary_router_is_idle(supervisor)
    {
        return ActionRetryStep::Busy;
    }
    let Some(owner) = retained.take() else {
        error!(
            "e290-node stage={stage} status=FAIL reason=selected-retry-owner-missing action=fail-closed-drain"
        );
        return ActionRetryStep::Terminal;
    };
    let (actions, admission, protocol_dispatch) = owner.into_parts();
    match supervisor.try_offer_actions(actions, admission) {
        Ok(()) => ActionRetryStep::Accepted(protocol_dispatch),
        Err(failure) => match handle_action_offer_failure(failure, stage) {
            ActionOfferHandling::Retry(owner) => {
                *retained = Some(match protocol_dispatch {
                    Some(protocol) => owner.with_protocol_dispatch(protocol),
                    None => owner,
                });
                ActionRetryStep::Busy
            }
            ActionOfferHandling::RetainAndDrain(owner) => {
                *retained = Some(match protocol_dispatch {
                    Some(protocol) => owner.with_protocol_dispatch(protocol),
                    None => owner,
                });
                ActionRetryStep::Terminal
            }
        },
    }
}

pub(crate) fn ordinary_router_is_idle(supervisor: &ProductSupervisor) -> bool {
    let capacities = supervisor.ordinary_capacities();
    capacities.active == 0 && supervisor.ordinary_parked_count() == capacities.registered
}

pub(crate) fn observe_terminal_correlation_fault(
    retained: &mut Option<ReceiptCorrelationError>,
    fault: ReceiptCorrelationError,
    source: &'static str,
) {
    if retained.is_none() {
        error!(
            "e290-node stage={source} status=TERMINAL-CORRELATION reason={fault:?} action=fail-closed-drain"
        );
        *retained = Some(fault);
    }
}

pub(crate) fn terminal_transition_observation(transition: NodeInterfaceSupervisorTransition) -> u8 {
    match transition {
        NodeInterfaceSupervisorTransition::Fault(_) => 1 << 0,
        NodeInterfaceSupervisorTransition::Data(_) => 1 << 1,
        NodeInterfaceSupervisorTransition::Ordinary(_) => 1 << 2,
        NodeInterfaceSupervisorTransition::DataPermit { .. } => 1 << 3,
        NodeInterfaceSupervisorTransition::OrdinaryPermit { .. } => 1 << 4,
        NodeInterfaceSupervisorTransition::Lifecycle(_) => 1 << 5,
        NodeInterfaceSupervisorTransition::CompletionAccepted { .. }
        | NodeInterfaceSupervisorTransition::Idle => 1 << 7,
    }
}

pub(crate) fn now_millis() -> u64 {
    Instant::now().as_millis()
}

pub(crate) fn now_micros() -> u64 {
    Instant::now().as_micros()
}

pub(crate) fn now_seconds() -> u64 {
    Instant::now().as_secs()
}
