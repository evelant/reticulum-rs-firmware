use super::*;
use rand_core::{CryptoRng, RngCore};
use reticulum_lxmf_durable_ingress::{DurableCandidateError, EventCarrierMismatch};
use reticulum_lxmf_ingress::{DeferredIngress, RejectedIngress, UnrelatedEvent};
use reticulum_lxmf_store::{LxmfStoreBindingError, LxmfStoreDeviceId, LxmfStoreFault};
use reticulum_lxmf_wire::WireError;
use reticulum_node_core::{
    AnnounceEmissionTime, ApplicationEvent, ApplicationEventDiscardReason, ApplicationEventOwner,
    ApplicationEventSlot, MonotonicSeconds, NodeActions, NodeConfig, NodeIdentity, NodeInstanceId,
};
use std::{vec, vec::Vec};

fn retry_token(
    owner: &mut ApplicationEventOwner<'static>,
    destination: u8,
) -> ApplicationEventRetryToken<'static> {
    owner
        .try_offer_actions(NodeActions::without_retained_proofs(
            vec![ApplicationEvent::DataReceived {
                destination: [destination; 16],
                payload: Vec::from([destination]),
                ingress: None,
            }],
            vec![],
            0,
        ))
        .expect("test event fits");
    owner
        .lease_next()
        .expect("test event is ready")
        .quarantine_for_retry(LXMF_RETRY_QUARANTINE)
}

fn static_owner<const N: usize>() -> ApplicationEventOwner<'static> {
    let slots = std::boxed::Box::leak(std::boxed::Box::new(
        [const { ApplicationEventSlot::new() }; N],
    ));
    ApplicationEventOwner::new(slots)
}

#[derive(Default)]
struct CounterRng(u8);

impl RngCore for CounterRng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0; 4];
        self.fill_bytes(&mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0; 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        for byte in destination {
            self.0 = self.0.wrapping_add(1);
            *byte = self.0;
        }
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl CryptoRng for CounterRng {}

fn packet_only_announce(tag: u8) -> NodeActions {
    let mut node = NodeCore::<4, 1, 4, 2, 0>::new(
        NodeIdentity::from_private_key(&[tag; 64]).expect("test identity"),
        "proof-holder",
        &["source"],
        NodeInstanceId::new([tag; 16]),
        NodeConfig::endpoint(),
    )
    .expect("test node");
    let mut rng = CounterRng::default();
    node.queue_announce(
        None,
        AnnounceEmissionTime::new(1).expect("announce time"),
        &mut rng,
    )
    .expect("announce fits");
    let actions = node.flush_announces(MonotonicSeconds::new(1), &mut rng);
    assert_eq!(actions.packets.len(), 1);
    assert!(actions.events.is_empty());
    actions
}

#[test]
fn selected_wire_and_stamp_profile_is_explicit() {
    assert_eq!(
        wire_limits(),
        WireLimits::new(4_096, 2_048, 256, 2_048, 65_536, 16)
    );
    assert!(matches!(stamp_policy(), StampPolicy::NotRequired));
}

#[test]
fn mount_gated_activation_registers_only_the_durable_service() {
    let mut disabled = NodeCore::<4, 1, 4, 2, 0>::new(
        NodeIdentity::from_private_key(&[0x51; 64]).expect("test identity"),
        config::RNS_APPLICATION_NAME,
        &config::RNS_PRIMARY_ASPECTS,
        NodeInstanceId::new([1; 16]),
        NodeConfig::transport(),
    )
    .expect("test node");
    let primary = disabled.destination_hash();
    disabled.set_inbound_proof_policy(InboundProofPolicy::Always);
    assert_eq!(
        activate_lxmf_delivery(&mut disabled, false, &[0x93, 0xc0, 0xc0, 0x90]),
        Ok(LxmfDeliveryActivation::Disabled)
    );
    assert_eq!(disabled.destination_hash(), primary);
    let expected = disabled
        .register_inbound_single_destination(
            config::RNS_LXMF_APPLICATION_NAME,
            &config::RNS_LXMF_DELIVERY_ASPECTS,
        )
        .expect("disabled activation consumed no destination slot");

    let mut active = NodeCore::<4, 1, 4, 2, 0>::new(
        NodeIdentity::from_private_key(&[0x51; 64]).expect("test identity"),
        config::RNS_APPLICATION_NAME,
        &config::RNS_PRIMARY_ASPECTS,
        NodeInstanceId::new([2; 16]),
        NodeConfig::transport(),
    )
    .expect("test node");
    active.set_inbound_proof_policy(InboundProofPolicy::Always);
    assert_eq!(
        activate_lxmf_delivery(&mut active, true, &[0x93, 0xc0, 0xc0, 0x90]),
        Ok(LxmfDeliveryActivation::Active(expected))
    );
    assert_eq!(active.destination_hash(), primary);
}

#[test]
fn malformed_is_terminal_while_source_missing_is_retryable() {
    let malformed =
        DurableIngressRetentionReason::<()>::Rejected(RejectedIngress::Wire(WireError::TooShort {
            minimum: 113,
            actual: 1,
        }));
    assert_eq!(
        retention_action(&malformed, None),
        LxmfRetentionAction::Terminal(LxmfTerminalReject::InvalidMessage)
    );

    let missing =
        DurableIngressRetentionReason::<()>::Deferred(DeferredIngress::SourceIdentityUnavailable {
            source: [0x5a; 16],
        });
    assert_eq!(
        retention_action(&missing, None),
        LxmfRetentionAction::Retry(LxmfRetryClass::AdmissionDeferred)
    );
    assert_eq!(
        admission_deferred_source(&missing),
        Some(DestinationHash::new([0x5a; 16]))
    );
    assert_eq!(admission_deferred_source(&malformed), None);
}

#[test]
fn admission_retry_schedule_is_exponential_staggered_and_bounded() {
    let mut owner = static_owner::<1>();
    let token = retry_token(&mut owner, 0x51);
    let sequence = token.sequence();
    for (attempt, nominal) in [5_000_u64, 10_000, 20_000, 40_000, 80_000, 160_000]
        .into_iter()
        .enumerate()
    {
        let attempt = u8::try_from(attempt + 1).expect("six attempts fit u8");
        let delay = admission_retry_delay_ms(attempt, sequence);
        assert!(delay >= nominal);
        assert!(delay <= nominal + nominal / 5);
    }
    let first_capped = admission_retry_delay_ms(7, sequence);
    assert!(first_capped >= config::LXMF_ADMISSION_RETRY_MAX_MS * 4 / 5);
    assert!(first_capped < config::LXMF_ADMISSION_RETRY_MAX_MS);
    let capped = admission_retry_delay_ms(u8::MAX, sequence);
    assert!(capped >= config::LXMF_ADMISSION_RETRY_MAX_MS * 4 / 5);
    assert!(capped < config::LXMF_ADMISSION_RETRY_MAX_MS);
    assert_eq!(
        admission_retry_delay_ms(0, sequence),
        admission_retry_delay_ms(1, sequence),
    );
    owner
        .try_reacquire_quarantined(token)
        .expect("schedule inspection preserves exact token")
        .discard(ApplicationEventDiscardReason::Superseded);
}

#[test]
fn exact_source_announce_wakes_only_matching_admission_retries() {
    let mut owner = static_owner::<3>();
    let source_a = DestinationHash::new([0xa1; 16]);
    let source_b = DestinationHash::new([0xb2; 16]);
    let mut retries = LxmfRetrySet::<3>::new();
    retries
        .try_insert(
            LxmfRetryEntry::new(
                retry_token(&mut owner, 1),
                100,
                LxmfRetryClass::AdmissionDeferred,
                None,
            )
            .with_admission_state(7, Some(source_a)),
        )
        .unwrap_or_else(|_| panic!("first deferred owner fits"));
    retries
        .try_insert(
            LxmfRetryEntry::new(
                retry_token(&mut owner, 2),
                100,
                LxmfRetryClass::AdmissionDeferred,
                None,
            )
            .with_admission_state(3, Some(source_b)),
        )
        .unwrap_or_else(|_| panic!("second deferred owner fits"));
    retries
        .try_insert(
            LxmfRetryEntry::new(
                retry_token(&mut owner, 3),
                100,
                LxmfRetryClass::CrossStoreBusy,
                None,
            )
            .with_admission_state(99, Some(source_a)),
        )
        .unwrap_or_else(|_| panic!("non-admission owner fits"));

    assert_eq!(retries.wake_admission_for_source(source_a, 10), 1);
    assert!(retries.take_due(9, None).is_none());
    let woken = retries
        .take_due(10, None)
        .expect("the exact identity dependency wakes immediately");
    assert_eq!(woken.admission_attempt(), 7);
    assert_eq!(woken.admission_source(), Some(source_a));
    assert_eq!(woken.retry_not_before_ms(), 10);
    owner
        .try_reacquire_quarantined(woken.into_token())
        .expect("woken owner remains exact")
        .discard(ApplicationEventDiscardReason::Superseded);

    assert_eq!(retries.wake_admission_for_source(source_a, 10), 0);
    assert!(retries.take_due(99, None).is_none());
    assert_eq!(retries.wake_admission_for_source(source_b, 20), 1);
    let second = retries
        .take_due(20, None)
        .expect("independent exact source wakes separately");
    owner
        .try_reacquire_quarantined(second.into_token())
        .expect("second woken owner remains exact")
        .discard(ApplicationEventDiscardReason::Superseded);
    let remaining = retries
        .take_due(100, None)
        .expect("non-admission retry keeps its original deadline");
    assert_eq!(remaining.class(), LxmfRetryClass::CrossStoreBusy);
    assert_eq!(remaining.admission_attempt(), 0);
    assert_eq!(remaining.admission_source(), None);
    owner
        .try_reacquire_quarantined(remaining.into_token())
        .expect("remaining owner remains exact")
        .discard(ApplicationEventDiscardReason::Superseded);
}

#[test]
fn busy_and_ambiguous_store_results_preserve_exact_retry_policy() {
    let backend = DurableIngressRetentionReason::Store(LxmfCommitError::Backend {
        stage: reticulum_lxmf_store::LxmfProgramStage::Commit,
        error: (),
    });
    assert_eq!(
        retention_action(&backend, Some(MessageId::new([6; 32]))),
        LxmfRetentionAction::Retry(LxmfRetryClass::StoreReconcile)
    );
    let message_id = MessageId::new([7; 32]);
    let busy =
        DurableIngressRetentionReason::<()>::Store(LxmfCommitError::AmbiguousMutationPending {
            message_id,
        });
    assert_eq!(
        retention_action(&busy, Some(message_id)),
        LxmfRetentionAction::Retry(LxmfRetryClass::StoreBusy)
    );
}

#[test]
fn pre_io_deferral_preserves_only_existing_reconciliation_authority() {
    let pending = MessageId::new([0x7a; 32]);
    for fallback in [
        LxmfRetryClass::AdmissionDeferred,
        LxmfRetryClass::CrossStoreBusy,
        LxmfRetryClass::DelayedProofBusy,
    ] {
        assert_eq!(
            retry_after_pre_io_deferral(
                Some(LxmfRetryClass::StoreReconcile),
                Some(pending),
                fallback,
            ),
            (LxmfRetryClass::StoreReconcile, Some(pending)),
        );
        assert_eq!(
            retry_after_pre_io_deferral(Some(LxmfRetryClass::StoreBusy), Some(pending), fallback,),
            (fallback, None),
            "StoreBusy must never impersonate exact reconciliation authority",
        );
    }

    let mut owner = static_owner::<1>();
    let (class, message_id) = retry_after_pre_io_deferral(
        Some(LxmfRetryClass::StoreReconcile),
        Some(pending),
        LxmfRetryClass::AdmissionDeferred,
    );
    let mut retries = LxmfRetrySet::<1>::new();
    retries
        .try_insert(LxmfRetryEntry::new(
            retry_token(&mut owner, 0x7a),
            10,
            class,
            message_id,
        ))
        .unwrap_or_else(|_| panic!("exact pending owner fits"));
    assert!(retries.contains_pending_owner(pending));
    assert!(retries.take_pressure_relief().is_none());
    assert!(retries.take_due(9, Some(pending)).is_none());
    assert_eq!(
        retries
            .take_due(10, Some(pending))
            .expect("identity-restored retry remains selectable")
            .message_id(),
        Some(pending),
    );
}

#[test]
fn index_store_and_handle_capacity_have_explicit_terminal_policy() {
    for (reason, expected) in [
        (
            DurableIngressRetentionReason::<()>::Store(LxmfCommitError::Full {
                required_extents: 2,
                remaining_extents: 1,
            }),
            LxmfTerminalReject::StoreFull,
        ),
        (
            DurableIngressRetentionReason::<()>::Store(LxmfCommitError::IndexFull {
                capacity: 512,
            }),
            LxmfTerminalReject::IndexFull,
        ),
        (
            DurableIngressRetentionReason::<()>::Store(LxmfCommitError::HandleExhausted),
            LxmfTerminalReject::HandleExhausted,
        ),
    ] {
        assert_eq!(
            retention_action(&reason, None),
            LxmfRetentionAction::Terminal(expected)
        );
    }
}

#[test]
fn contradictions_and_store_faults_fail_closed() {
    let candidate = DurableIngressRetentionReason::<()>::Candidate(
        DurableCandidateError::EventCarrierMismatch(EventCarrierMismatch::Destination),
    );
    assert_eq!(
        retention_action(&candidate, None),
        LxmfRetentionAction::Terminal(LxmfTerminalReject::CandidateContradiction)
    );
    let binding = DurableIngressRetentionReason::<()>::Store(LxmfCommitError::Binding(
        LxmfStoreBindingError::DeviceMismatch {
            expected: LxmfStoreDeviceId::new([1; 16]),
            actual: LxmfStoreDeviceId::new([2; 16]),
        },
    ));
    assert_eq!(
        retention_action(&binding, None),
        LxmfRetentionAction::Terminal(LxmfTerminalReject::StoreFault)
    );
    let fault = DurableIngressRetentionReason::<()>::Store(LxmfCommitError::Fault(
        LxmfStoreFault::UnknownProgrammedExtent { extent: 3 },
    ));
    assert_eq!(
        retention_action(&fault, None),
        LxmfRetentionAction::Terminal(LxmfTerminalReject::StoreFault)
    );
    assert_eq!(
        retention_action(&fault, Some(MessageId::new([0xf0; 32]))),
        LxmfRetentionAction::HoldPendingFault,
        "a post-pending fault must retain exact reconciliation authority"
    );
    let collision = DurableIngressRetentionReason::<()>::Store(LxmfCommitError::HashCollision {
        message_id: MessageId::new([0xc0; 32]),
    });
    assert_eq!(
        retention_action(&collision, None),
        LxmfRetentionAction::Terminal(LxmfTerminalReject::HashCollision),
        "a collision rejects only this candidate and does not disable the service"
    );
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "holder regression deliberately round-trips the exact no-alloc proof owner"
)]
fn one_packet_proof_holder_preserves_supervisor_return_and_rejects_a_second() {
    let mut holder = LxmfProofActionsHolder::new();
    let first = packet_only_announce(0x61);
    let first_pointer = first.packets.as_ptr();
    holder
        .try_stage(first, config::ordinary_admission(10))
        .unwrap_or_else(|_| panic!("one packet fits"));
    let busy = holder
        .begin_offer()
        .expect("offer takes sole owner")
        .try_submit(|actions, admission| {
            assert_eq!(actions.packets.as_ptr(), first_pointer);
            Err(LxmfProofSinkFailure::returned((), actions, admission))
        });
    assert_eq!(busy, Err(()));
    assert!(holder.is_occupied());

    let second = packet_only_announce(0x62);
    let second_pointer = second.packets.as_ptr();
    let failure = holder
        .try_stage(second, config::ordinary_admission(20))
        .expect_err("occupied holder rejects a second release");
    assert_eq!(failure.reason(), LxmfProofHolderError::Occupied);
    let (second, _) = failure.into_parts();
    assert_eq!(second.packets.as_ptr(), second_pointer);

    drop(
        holder
            .begin_offer()
            .expect("cancelled offer temporarily owns first proof"),
    );
    assert!(holder.is_occupied(), "cancelled offer restores first proof");

    holder
        .begin_offer()
        .expect("first returned proof remains exact")
        .try_submit::<(), _>(|actions, _admission| {
            assert_eq!(actions.packets.as_ptr(), first_pointer);
            assert_eq!(actions.packets.len(), 1);
            Ok(())
        })
        .expect("ordinary sink accepts first proof");
    holder
        .try_stage(second, config::ordinary_admission(20))
        .unwrap_or_else(|_| panic!("second fits only after first leaves"));
}

#[test]
fn circular_proof_pressure_evicts_exactly_one_non_pending_retry() {
    let mut owner = static_owner::<{ config::APPLICATION_EVENT_SLOTS }>();
    let mut retries = LxmfRetrySet::<{ config::APPLICATION_EVENT_SLOTS }>::new();
    let pending = MessageId::new([0x91; 32]);
    retries
        .try_insert(LxmfRetryEntry::new(
            retry_token(&mut owner, 0),
            0,
            LxmfRetryClass::StoreReconcile,
            Some(pending),
        ))
        .unwrap_or_else(|_| panic!("pending owner fits"));
    for marker in 1..15 {
        retries
            .try_insert(LxmfRetryEntry::new(
                retry_token(&mut owner, marker),
                0,
                LxmfRetryClass::AdmissionDeferred,
                None,
            ))
            .unwrap_or_else(|_| panic!("deferred owner fits"));
    }
    assert_eq!(owner.capacities().vacant, 1);
    assert_eq!(retries.len(), 15);
    for capacity_backpressured in [false, true] {
        for fail_closed in [false, true] {
            for holder_occupied in [false, true] {
                for ready_proofs in [0, 1] {
                    assert_eq!(
                        should_relieve_lxmf_retry(
                            capacity_backpressured,
                            fail_closed,
                            holder_occupied,
                            ready_proofs,
                        ),
                        capacity_backpressured
                            && !fail_closed
                            && (holder_occupied || ready_proofs != 0),
                    );
                }
            }
        }
    }

    let evicted = retries
        .take_pressure_relief()
        .expect("one non-pending retry relieves circular pressure");
    assert_eq!(evicted.class(), LxmfRetryClass::AdmissionDeferred);
    owner
        .try_reacquire_quarantined(evicted.into_token())
        .expect("exact evictee reacquires")
        .discard(ApplicationEventDiscardReason::DownstreamCapacity);
    assert_eq!(owner.capacities().vacant, 2);
    assert_eq!(retries.len(), 14, "one pass evicts exactly one owner");
    assert!(retries.contains_reconciliation_owner(pending));

    owner
        .try_offer_actions(NodeActions::without_retained_proofs(
            vec![
                ApplicationEvent::Tick {
                    expired_paths: 1,
                    closed_links: 0,
                },
                ApplicationEvent::Tick {
                    expired_paths: 0,
                    closed_links: 1,
                },
            ],
            vec![],
            0,
        ))
        .expect("two-event retained ordinary batch now fits atomically");
    assert_eq!(owner.capacities().ready, 2);
}

#[test]
fn pressure_relief_never_selects_reconciliation_or_fault_hold() {
    let mut owner = static_owner::<2>();
    let pending = MessageId::new([0xa5; 32]);
    let mut retries = LxmfRetrySet::<2>::new();
    for (marker, class) in [
        (1, LxmfRetryClass::StoreReconcile),
        (2, LxmfRetryClass::StoreFaultHold),
    ] {
        retries
            .try_insert(LxmfRetryEntry::new(
                retry_token(&mut owner, marker),
                0,
                class,
                Some(pending),
            ))
            .unwrap_or_else(|_| panic!("protected owner fits"));
    }
    assert!(retries.take_pressure_relief().is_none());
    assert!(retries.take_nonheld().is_none());
    assert!(retries.contains_reconciliation_owner(pending));
    assert!(retries.has_fault_hold(pending));
}

#[test]
fn foreign_reacquire_fault_is_owned_outside_retry_scheduler() {
    let mut original = static_owner::<1>();
    let token = retry_token(&mut original, 0x31);
    let mut foreign = static_owner::<1>();
    let failure = foreign
        .try_reacquire_quarantined(token)
        .expect_err("foreign owner rejects exact capability");
    let first_message = MessageId::new([0x31; 32]);
    let mut fault = LxmfAuthorityFault::<2>::new();
    fault
        .try_capture(LxmfRetryEntry::new(
            failure.into_token(),
            u64::MAX,
            LxmfRetryClass::AdmissionDeferred,
            Some(first_message),
        ))
        .unwrap_or_else(|_| panic!("first authority fault is retained"));
    assert!(fault.is_some());
    assert!(fault.contains_message(first_message));

    let mut second_owner = static_owner::<1>();
    let second_message = MessageId::new([0x32; 32]);
    fault
        .try_capture(LxmfRetryEntry::new(
            retry_token(&mut second_owner, 0x32),
            0,
            LxmfRetryClass::CrossStoreBusy,
            Some(second_message),
        ))
        .unwrap_or_else(|_| panic!("second distinct authority is also retained"));
    assert!(fault.contains_message(second_message));

    let mut overflow_owner = static_owner::<1>();
    let overflow = LxmfRetryEntry::new(
        retry_token(&mut overflow_owner, 0x33),
        0,
        LxmfRetryClass::CrossStoreBusy,
        None,
    );
    let overflow = fault
        .try_capture(overflow)
        .expect_err("bounded fault owner returns overflow unchanged")
        .into_entry();
    overflow_owner
        .try_reacquire_quarantined(overflow.into_token())
        .expect("rejected overflow authority remains exact")
        .discard(ApplicationEventDiscardReason::Superseded);
}

#[test]
fn sixteen_tokens_progress_independently_and_pending_store_owner_wins() {
    let mut owner = static_owner::<{ config::APPLICATION_EVENT_SLOTS }>();
    let mut retries = LxmfRetrySet::<{ config::APPLICATION_EVENT_SLOTS }>::new();
    let pending = MessageId::new([0x44; 32]);
    for marker in 0..config::APPLICATION_EVENT_SLOTS {
        let token = retry_token(&mut owner, marker as u8);
        let message_id = (marker == 9).then_some(pending);
        let class = if message_id.is_some() {
            LxmfRetryClass::StoreReconcile
        } else {
            LxmfRetryClass::AdmissionDeferred
        };
        retries
            .try_insert(LxmfRetryEntry::new(token, 10, class, message_id))
            .unwrap_or_else(|_| panic!("each physical slot has one retry owner"));
    }
    assert_eq!(retries.len(), config::APPLICATION_EVENT_SLOTS);
    assert!(retries.take_due(9, Some(pending)).is_none());
    let exact = retries
        .take_due(10, Some(pending))
        .expect("pending store owner is selected");
    assert_eq!(exact.message_id(), Some(pending));
    let exact_slot = exact.slot();
    let lease = owner
        .try_reacquire_quarantined(exact.into_token())
        .expect("opaque token reacquires exact event");
    lease.discard(reticulum_node_core::ApplicationEventDiscardReason::Superseded);
    assert_eq!(retries.len(), config::APPLICATION_EVENT_SLOTS - 1);

    let unrelated = retries
        .take_due(10, None)
        .expect("another slot can progress independently");
    assert_ne!(unrelated.slot(), exact_slot);
    owner
        .try_reacquire_quarantined(unrelated.into_token())
        .expect("independent token remains valid")
        .discard(reticulum_node_core::ApplicationEventDiscardReason::Superseded);
}

#[test]
fn store_busy_token_cannot_impersonate_reconciliation_owner() {
    let mut owner = static_owner::<2>();
    let pending = MessageId::new([0xa1; 32]);
    let exact = retry_token(&mut owner, 1);
    let blocked = retry_token(&mut owner, 2);
    let mut retries = LxmfRetrySet::<2>::new();
    retries
        .try_insert(LxmfRetryEntry::new(
            exact,
            0,
            LxmfRetryClass::StoreReconcile,
            Some(pending),
        ))
        .unwrap_or_else(|_| panic!("exact owner fits"));
    retries
        .try_insert(LxmfRetryEntry::new(
            blocked,
            0,
            LxmfRetryClass::StoreBusy,
            None,
        ))
        .unwrap_or_else(|_| panic!("blocked owner fits"));

    let selected = retries
        .take_due(0, Some(pending))
        .expect("only exact reconciliation owner is eligible");
    assert_eq!(selected.class(), LxmfRetryClass::StoreReconcile);
    assert_eq!(selected.message_id(), Some(pending));
    assert!(retries.take_due(0, Some(pending)).is_none());
    owner
        .try_reacquire_quarantined(selected.into_token())
        .expect("selected exact owner remains structurally valid")
        .discard(reticulum_node_core::ApplicationEventDiscardReason::Superseded);
}

#[test]
fn stale_and_foreign_tokens_fail_closed_without_scalar_substitution() {
    let mut first = static_owner::<1>();
    let token = retry_token(&mut first, 1);
    let mut foreign = static_owner::<1>();
    let failure = foreign
        .try_reacquire_quarantined(token)
        .expect_err("equal scalar slots from another owner are foreign");
    assert_eq!(
        failure.reason(),
        reticulum_node_core::ApplicationEventRetryError::ForeignOwner
    );
    let token = failure.into_token();
    let lease = first
        .try_reacquire_quarantined(token)
        .expect("unchanged token still authorizes its original owner");
    lease.discard(reticulum_node_core::ApplicationEventDiscardReason::Superseded);
}

#[test]
fn unrelated_classification_is_never_mistaken_for_durable_lxmf() {
    let reason = DurableIngressRetentionReason::<()>::Unrelated(UnrelatedEvent::OtherDestination {
        expected: reticulum_lxmf_ingress::LocalDeliveryDestination::new([1; 16]),
        actual: [2; 16],
    });
    assert_eq!(
        retention_action(&reason, None),
        LxmfRetentionAction::Terminal(LxmfTerminalReject::Unrelated)
    );
}
