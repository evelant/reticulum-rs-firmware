use super::*;

fn lengths(
    normalized: usize,
    carrier: usize,
    title: usize,
    content: usize,
    fields: usize,
) -> InboundMessageLengths {
    InboundMessageLengths::new(normalized, carrier, title, content, fields).unwrap()
}

fn metadata(
    id: u8,
    destination: u8,
    source: u8,
    timestamp_bits: u64,
    carrier: CarrierProvenance,
    lengths: InboundMessageLengths,
    stamp_admission: StampAdmissionProvenance,
) -> InboundMessageMetadata {
    InboundMessageMetadata::new(
        MessageId::new([id; MESSAGE_ID_LENGTH]),
        AuthenticatedMaterialFingerprint::new([id; AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH]),
        DestinationHash::new([destination; DESTINATION_HASH_LENGTH]),
        SourceHash::new([source; DESTINATION_HASH_LENGTH]),
        SignatureVerification::Validated,
        timestamp_bits,
        carrier,
        stamp_admission,
        lengths,
    )
    .unwrap()
}

#[test]
fn identifiers_retain_all_bytes_and_handle_zero_is_reserved() {
    assert_eq!(MessageId::new([1; 32]).as_bytes(), &[1; 32]);
    assert_eq!(
        AuthenticatedMaterialFingerprint::new([5; 32]).as_bytes(),
        &[5; 32]
    );
    assert_eq!(DestinationHash::new([2; 16]).as_bytes(), &[2; 16]);
    assert_eq!(SourceHash::new([3; 16]).as_bytes(), &[3; 16]);
    assert_eq!(ExactWireDigest::new([4; 32]).as_bytes(), &[4; 32]);
    assert_eq!(MessageHandle::new(0), Err(InvalidMessageHandle));
    assert_eq!(MessageHandle::new(9).unwrap().get(), 9);
}

#[test]
fn required_stamp_cost_matches_protocol_range() {
    assert_eq!(RequiredStampCost::new(0).unwrap_err().actual(), 0);
    assert_eq!(RequiredStampCost::new(1).unwrap().get(), 1);
    assert_eq!(RequiredStampCost::new(254).unwrap().get(), 254);
    assert_eq!(RequiredStampCost::new(255).unwrap_err().actual(), 255);
    assert_eq!(
        RequiredStampCost::new(u16::MAX).unwrap_err().actual(),
        u16::MAX
    );
}

#[test]
fn metadata_enforces_carrier_normalization_without_setting_a_product_ceiling() {
    let opportunistic = lengths(407, 391, 4, 270, 1);
    let admitted = metadata(
        1,
        2,
        3,
        4,
        CarrierProvenance::Opportunistic,
        opportunistic,
        StampAdmissionProvenance::NotRequired {
            stamp_present: false,
        },
    );
    assert_eq!(admitted.lengths().normalized_wire(), 407);
    assert_eq!(admitted.lengths().carrier_payload(), 391);
    assert_eq!(admitted.lengths().title(), 4);
    assert_eq!(admitted.lengths().content(), 270);
    assert_eq!(admitted.lengths().fields_encoded(), 1);

    let mismatch = InboundMessageMetadata::new(
        admitted.message_id(),
        admitted.authenticated_material(),
        admitted.destination(),
        admitted.source(),
        admitted.signature_verification(),
        admitted.timestamp_bits(),
        CarrierProvenance::Opportunistic,
        admitted.stamp_admission(),
        lengths(406, 391, 4, 270, 1),
    )
    .unwrap_err();
    assert_eq!(mismatch.carrier(), CarrierProvenance::Opportunistic);

    assert!(
        InboundMessageMetadata::new(
            admitted.message_id(),
            admitted.authenticated_material(),
            admitted.destination(),
            admitted.source(),
            admitted.signature_verification(),
            admitted.timestamp_bits(),
            CarrierProvenance::LinkDataContextNone,
            admitted.stamp_admission(),
            lengths(431, 431, 4, 294, 1),
        )
        .is_ok()
    );
}

#[cfg(target_pointer_width = "64")]
#[test]
fn portable_lengths_reject_values_larger_than_u32() {
    let actual = u32::MAX as usize + 1;
    let error = InboundMessageLengths::new(actual, 0, 0, 0, 0).unwrap_err();
    assert_eq!(error.kind(), MessageLengthKind::NormalizedWire);
    assert_eq!(error.actual(), actual);
}

#[test]
fn opportunistic_candidate_borrows_two_exact_segments_without_copying() {
    let destination = [0x11; 16];
    let carrier_payload = [0x22; 391];
    let metadata = metadata(
        1,
        0x11,
        3,
        4,
        CarrierProvenance::Opportunistic,
        lengths(407, 391, 2, 270, 1),
        StampAdmissionProvenance::TrustedPriorTicket,
    );
    let candidate = InboundMessageCandidate::new(
        metadata,
        NormalizedWire::Opportunistic {
            implied_destination: &destination,
            carrier_payload: &carrier_payload,
        },
    )
    .unwrap();
    let segments = candidate.segments();
    assert_eq!(segments.segment_count(), 2);
    assert_eq!(segments.total_len(), 407);
    assert_eq!(segments.first().as_ptr(), destination.as_ptr());
    assert_eq!(segments.second().as_ptr(), carrier_payload.as_ptr());
    assert_eq!(candidate.wire().carrier_payload(), &carrier_payload);
}

#[test]
fn complete_candidate_borrows_one_segment_and_checks_destination() {
    let mut complete = [0x44; 431];
    complete[..16].fill(0x22);
    let metadata = metadata(
        1,
        0x22,
        3,
        4,
        CarrierProvenance::ResourceComplete,
        lengths(431, 431, 2, 294, 1),
        StampAdmissionProvenance::ProofOfWork {
            target_cost: RequiredStampCost::new(8).unwrap(),
            observed_value: 12,
        },
    );
    let candidate =
        InboundMessageCandidate::new(metadata, NormalizedWire::Contiguous(&complete)).unwrap();
    assert_eq!(candidate.segments().segment_count(), 1);
    assert_eq!(candidate.segments().first().as_ptr(), complete.as_ptr());
    assert!(candidate.segments().second().is_empty());

    complete[0] ^= 1;
    assert_eq!(
        InboundMessageCandidate::new(metadata, NormalizedWire::Contiguous(&complete)),
        Err(CandidateError::DestinationMismatch)
    );
}

#[test]
fn candidate_rejects_shape_and_normalized_length_mismatches() {
    let destination = [2; 16];
    let payload = [7; 391];
    let opportunistic = metadata(
        1,
        2,
        3,
        4,
        CarrierProvenance::Opportunistic,
        lengths(407, 391, 0, 0, 1),
        StampAdmissionProvenance::NotRequired {
            stamp_present: false,
        },
    );
    assert!(matches!(
        InboundMessageCandidate::new(opportunistic, NormalizedWire::Contiguous(&payload)),
        Err(CandidateError::CarrierShapeMismatch { .. })
    ));

    let short = &payload[..390];
    assert_eq!(
        InboundMessageCandidate::new(
            opportunistic,
            NormalizedWire::Opportunistic {
                implied_destination: &destination,
                carrier_payload: short,
            },
        ),
        Err(CandidateError::NormalizedLengthMismatch {
            expected: 407,
            actual: 406,
        })
    );

    let complete_metadata = metadata(
        1,
        2,
        3,
        4,
        CarrierProvenance::Complete,
        lengths(407, 407, 0, 0, 1),
        StampAdmissionProvenance::NotRequired {
            stamp_present: false,
        },
    );
    let mut complete = [8; 407];
    complete[..16].fill(2);
    assert!(
        InboundMessageCandidate::new(complete_metadata, NormalizedWire::Contiguous(&complete))
            .is_ok()
    );
}

#[test]
fn replay_identity_ignores_carrier_stamp_and_exact_wire_variation() {
    let opportunistic = metadata(
        1,
        2,
        3,
        0x4009_21fb_5444_2d18,
        CarrierProvenance::Opportunistic,
        lengths(407, 391, 4, 270, 1),
        StampAdmissionProvenance::TrustedPriorTicket,
    );
    let resource = metadata(
        1,
        2,
        3,
        0x4009_21fb_5444_2d18,
        CarrierProvenance::ResourceComplete,
        lengths(431, 431, 4, 270, 1),
        StampAdmissionProvenance::ProofOfWork {
            target_cost: RequiredStampCost::new(8).unwrap(),
            observed_value: 9,
        },
    );
    assert_eq!(
        opportunistic.authenticated_fingerprint(),
        resource.authenticated_fingerprint()
    );

    let first = ReplayFingerprint::new(
        opportunistic.authenticated_fingerprint(),
        ExactWireDigest::new([1; 32]),
    );
    let exact = ReplayFingerprint::new(
        resource.authenticated_fingerprint(),
        ExactWireDigest::new([1; 32]),
    );
    let alternate = ReplayFingerprint::new(
        resource.authenticated_fingerprint(),
        ExactWireDigest::new([2; 32]),
    );
    assert_eq!(exact.classify_against(first), ReplayRelation::ExactReplay);
    assert_eq!(
        alternate.classify_against(first),
        ReplayRelation::EquivalentReplay
    );
}

#[test]
fn replay_comparison_distinguishes_new_ids_from_forced_same_id_collisions() {
    let base = metadata(
        1,
        2,
        3,
        4,
        CarrierProvenance::Complete,
        lengths(431, 431, 4, 294, 1),
        StampAdmissionProvenance::NotRequired {
            stamp_present: false,
        },
    );
    let different_id = metadata(
        9,
        2,
        3,
        4,
        CarrierProvenance::Complete,
        lengths(431, 431, 4, 294, 1),
        base.stamp_admission(),
    );
    let conflicting_material = InboundMessageMetadata::new(
        base.message_id(),
        AuthenticatedMaterialFingerprint::new([0xee; 32]),
        base.destination(),
        base.source(),
        base.signature_verification(),
        base.timestamp_bits(),
        base.carrier(),
        base.stamp_admission(),
        base.lengths(),
    )
    .unwrap();
    let committed = ReplayFingerprint::new(
        base.authenticated_fingerprint(),
        ExactWireDigest::new([1; 32]),
    );
    assert_eq!(
        ReplayFingerprint::new(
            different_id.authenticated_fingerprint(),
            ExactWireDigest::new([2; 32]),
        )
        .classify_against(committed),
        ReplayRelation::DistinctMessage
    );
    assert_eq!(
        ReplayFingerprint::new(
            conflicting_material.authenticated_fingerprint(),
            ExactWireDigest::new([2; 32]),
        )
        .classify_against(committed),
        ReplayRelation::AuthenticatedMetadataConflict
    );
}

#[test]
fn durable_receipt_exposes_only_stable_logical_evidence() {
    let metadata = metadata(
        1,
        2,
        3,
        4,
        CarrierProvenance::Complete,
        lengths(431, 431, 4, 294, 1),
        StampAdmissionProvenance::NotRequired {
            stamp_present: false,
        },
    );
    let fingerprint = ReplayFingerprint::new(
        metadata.authenticated_fingerprint(),
        ExactWireDigest::new([9; 32]),
    );
    let receipt = DurableMessageReceipt::new(MessageHandle::new(17).unwrap(), fingerprint);
    assert_eq!(receipt.handle().get(), 17);
    assert_eq!(receipt.message_id(), metadata.message_id());
    assert_eq!(receipt.fingerprint(), fingerprint);
}
