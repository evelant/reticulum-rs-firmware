//! Unit tests that require private access to the embedded Reticulum adapter.

extern crate std;

use alloc::{format, string::String, vec};

use super::*;
use rete_core::{
    CONTEXT_KEEPALIVE, CONTEXT_LINKCLOSE, CONTEXT_LRPROOF, CONTEXT_LRRTT, PacketBuilder,
};
use serde::Deserialize;

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

#[derive(Default)]
struct RecordingReceiptSink {
    refuse: bool,
    attempted: Vec<ReceiptCandidate>,
    terminals: Vec<ReceiptTerminal>,
    active_reservations: usize,
}

struct RecordingReceiptReservation<'a> {
    candidate: ReceiptCandidate,
    terminals: &'a mut Vec<ReceiptTerminal>,
    active_reservations: &'a mut usize,
}

impl ReceiptTerminalSink for RecordingReceiptSink {
    type Reservation<'a>
        = RecordingReceiptReservation<'a>
    where
        Self: 'a;

    fn try_reserve(
        &mut self,
        candidate: ReceiptCandidate,
    ) -> Result<Self::Reservation<'_>, ReceiptReservationUnavailable> {
        self.attempted.push(candidate);
        if self.refuse {
            return Err(ReceiptReservationUnavailable);
        }
        self.active_reservations += 1;
        Ok(RecordingReceiptReservation {
            candidate,
            terminals: &mut self.terminals,
            active_reservations: &mut self.active_reservations,
        })
    }
}

impl ReceiptTerminalReservation for RecordingReceiptReservation<'_> {
    fn commit(self, terminal: ReceiptTerminal) {
        assert_eq!(terminal.candidate(), self.candidate);
        self.terminals.push(terminal);
    }
}

impl Drop for RecordingReceiptReservation<'_> {
    fn drop(&mut self) {
        *self.active_reservations -= 1;
    }
}

type TestNode = EmbeddedNode<4, 2, 8, 2>;
type TwoLinkNode = EmbeddedNode<4, 2, 8, 2>;

fn test_link_binding(
    link: [u8; rete_core::TRUNCATED_HASH_LEN],
    destination: [u8; rete_core::TRUNCATED_HASH_LEN],
    role: ApplicationLinkRole,
) -> ApplicationLinkBinding {
    ApplicationLinkBinding {
        link,
        destination,
        role,
    }
}

#[test]
fn inbound_data_projection_moves_the_exact_payload_owner() {
    let expected_destination = [0x42; rete_core::TRUNCATED_HASH_LEN];
    let mut payload = Vec::with_capacity(64);
    payload.extend_from_slice(b"exact moved payload");
    let expected_pointer = payload.as_ptr();
    let expected_capacity = payload.capacity();

    let projection = project_inbound_data(
        project_application_event(
            NativeNodeEvent::DataReceived {
                dest_hash: DestHash::from(expected_destination),
                payload,
            },
            None,
        )
        .unwrap(),
    );
    let InboundDataProjection::Data(data) = projection else {
        panic!("destination DATA must project onto the product-owned value")
    };
    assert_eq!(data.destination(), &expected_destination);
    assert_eq!(data.payload(), b"exact moved payload");

    let (destination, payload) = data.into_parts();
    assert_eq!(destination, expected_destination);
    assert_eq!(payload.as_ptr(), expected_pointer);
    assert_eq!(payload.capacity(), expected_capacity);
}

#[test]
fn inbound_data_projection_returns_non_data_event_unchanged() {
    let expected_link = LinkId::from([0x7b; rete_core::TRUNCATED_HASH_LEN]);
    let mut expected_data = Vec::with_capacity(48);
    expected_data.extend_from_slice(b"unchanged link data");
    let expected_pointer = expected_data.as_ptr();
    let expected_capacity = expected_data.capacity();
    let expected_destination = [0x6c; rete_core::TRUNCATED_HASH_LEN];
    let projection = project_inbound_data(
        project_application_event(
            NativeNodeEvent::LinkData {
                link_id: expected_link,
                data: expected_data,
                context: 0x5a,
            },
            Some(test_link_binding(
                *expected_link.as_bytes(),
                expected_destination,
                ApplicationLinkRole::Responder,
            )),
        )
        .unwrap(),
    );

    let InboundDataProjection::Other(ApplicationEvent::LinkData {
        binding,
        data,
        context,
        ..
    }) = projection
    else {
        panic!("non-DATA event must return through the unchanged projection")
    };
    assert_eq!(binding.link(), expected_link.as_bytes());
    assert_eq!(binding.destination(), &expected_destination);
    assert_eq!(binding.role(), ApplicationLinkRole::Responder);
    assert_eq!(context, 0x5a);
    assert_eq!(data, b"unchanged link data");
    assert_eq!(data.as_ptr(), expected_pointer);
    assert_eq!(data.capacity(), expected_capacity);
}

#[test]
fn link_bound_event_projection_fails_closed_without_retained_link_state() {
    let missing_link = LinkId::from([0x6d; rete_core::TRUNCATED_HASH_LEN]);
    let mut node = node(0x6e);

    assert!(matches!(
        node.project_retained_application_event(NativeNodeEvent::LinkData {
            link_id: missing_link,
            data: vec![0xa5; 8],
            context: LINK_DATA_CONTEXT_NONE,
        }),
        Err(ApplicationEventProjectionError::LinkStateNotRetained { link })
            if link == *missing_link.as_bytes()
    ));
    assert!(matches!(
        node.project_retained_application_event(NativeNodeEvent::RequestReceived {
            link_id: missing_link,
            request_id: rete_core::RequestId::from([
                0x71;
                rete_core::TRUNCATED_HASH_LEN
            ]),
            path_hash: rete_core::PathHash::from([
                0x72;
                rete_core::TRUNCATED_HASH_LEN
            ]),
            data: vec![0xa5; 8],
        }),
        Err(ApplicationEventProjectionError::LinkStateNotRetained { link })
            if link == *missing_link.as_bytes()
    ));
    assert!(matches!(
        node.project_retained_application_event(NativeNodeEvent::RequestValueReceived {
            link_id: missing_link,
            request_id: rete_core::RequestId::from([
                0x73;
                rete_core::TRUNCATED_HASH_LEN
            ]),
            path_hash: rete_core::PathHash::from([
                0x74;
                rete_core::TRUNCATED_HASH_LEN
            ]),
            requested_at: 1_700_000_000.25,
            value: vec![0xc0],
        }),
        Err(ApplicationEventProjectionError::LinkStateNotRetained { link })
            if link == *missing_link.as_bytes()
    ));
    assert!(matches!(
        project_application_event(
            NativeNodeEvent::RequestReceived {
                link_id: missing_link,
                request_id: rete_core::RequestId::from(
                    [0x75; rete_core::TRUNCATED_HASH_LEN]
                ),
                path_hash: rete_core::PathHash::from(
                    [0x76; rete_core::TRUNCATED_HASH_LEN]
                ),
                data: vec![0xa5; 8],
            },
            Some(test_link_binding(
                [0x77; rete_core::TRUNCATED_HASH_LEN],
                [0x78; rete_core::TRUNCATED_HASH_LEN],
                ApplicationLinkRole::Responder,
            )),
        ),
        Err(ApplicationEventProjectionError::LinkStateNotRetained { link })
            if link == *missing_link.as_bytes()
    ));

    let before = node.metrics().ingress.link_state_not_retained;
    let actions = node.finish_tick(IngestOutcome {
        events: vec![NativeNodeEvent::LinkData {
            link_id: missing_link,
            data: vec![0x5a; 8],
            context: LINK_DATA_CONTEXT_NONE,
        }],
        packets: Vec::new(),
        rejection: None,
    });
    assert!(actions.events.is_empty());
    assert!(actions.packets.is_empty());
    assert_eq!(node.metrics().ingress.link_state_not_retained, before + 1);
}

#[test]
#[should_panic(expected = "pinned Rete close_link emitted unexpected application event tick")]
fn local_close_projection_fails_closed_on_an_unexpected_native_event() {
    let _ = project_local_close_event(NativeNodeEvent::Tick {
        expired_paths: 0,
        closed_links: 0,
    });
}

#[test]
fn inbound_data_projection_preserves_maximum_encrypted_data() {
    let payload = vec![0xa5; MAX_DATA_PAYLOAD];
    let expected_pointer = payload.as_ptr();
    let ingress =
        IngressObservation::remote(InterfaceId(3), Some(IngressSignalObservation::new(-87, 6)));
    let projection = project_inbound_data(ApplicationEvent::DataReceived {
        destination: [0x33; rete_core::TRUNCATED_HASH_LEN],
        payload,
        ingress: Some(ingress),
    });
    let InboundDataProjection::Data(data) = projection else {
        panic!("maximum encrypted DATA must project")
    };
    assert_eq!(data.ingress(), Some(ingress));
    let (_, payload) = data.into_parts();
    assert_eq!(payload.len(), MAX_DATA_PAYLOAD);
    assert_eq!(payload.as_ptr(), expected_pointer);
    assert!(payload.iter().all(|byte| *byte == 0xa5));
}

#[test]
fn inbound_data_projection_does_not_apply_an_encrypted_payload_limit() {
    let payload = vec![0x5a; MAX_DATA_PAYLOAD + 1];
    let projection = project_inbound_data(ApplicationEvent::DataReceived {
        destination: [0x66; rete_core::TRUNCATED_HASH_LEN],
        payload,
        ingress: None,
    });
    let InboundDataProjection::Data(data) = projection else {
        panic!("projection must leave mailbox and destination-size policy to its caller")
    };
    assert_eq!(data.payload().len(), MAX_DATA_PAYLOAD + 1);
}

#[test]
fn inbound_data_debug_redacts_data_and_non_data_payloads() {
    let destination = [0x42; rete_core::TRUNCATED_HASH_LEN];
    let data = project_inbound_data(ApplicationEvent::DataReceived {
        destination,
        payload: vec![0xde, 0xad, 0xbe, 0xef],
        ingress: None,
    });
    let data_debug = format!("{data:?}");
    assert!(data_debug.contains("payload_len: 4"));
    assert!(!data_debug.contains("222, 173, 190, 239"));

    let other = project_inbound_data(ApplicationEvent::LinkData {
        binding: test_link_binding(
            [0x24; rete_core::TRUNCATED_HASH_LEN],
            [0x25; rete_core::TRUNCATED_HASH_LEN],
            ApplicationLinkRole::Initiator,
        ),
        data: vec![0xca, 0xfe, 0xba, 0xbe],
        context: 0x55,
        ingress: None,
    });
    let other_debug = format!("{other:?}");
    assert_eq!(other_debug, "Other(..)");
    assert!(!other_debug.contains("202, 254, 186, 190"));
}

#[test]
fn application_event_projection_covers_every_pinned_native_variant() {
    let link = LinkId::from([0x11; rete_core::TRUNCATED_HASH_LEN]);
    let destination = DestHash::from([0x22; rete_core::TRUNCATED_HASH_LEN]);
    let identity = IdentityHash::from([0x33; rete_core::TRUNCATED_HASH_LEN]);
    let request = rete_core::RequestId::from([0x44; rete_core::TRUNCATED_HASH_LEN]);
    let path = rete_core::PathHash::from([0x55; rete_core::TRUNCATED_HASH_LEN]);
    let resource_hash = [0x66; rete_core::TRUNCATED_HASH_LEN];

    let projected: Vec<_> = vec![
        NativeNodeEvent::AnnounceReceived {
            dest_hash: destination,
            identity_hash: identity,
            hops: 3,
            app_data: Some(vec![0xa1, 0xa2]),
        },
        NativeNodeEvent::DataReceived {
            dest_hash: destination,
            payload: vec![0xb1, 0xb2],
        },
        NativeNodeEvent::ProofReceived {
            packet_hash: [0xc1; 32],
        },
        NativeNodeEvent::ReceiptFailed {
            packet_hash: [0xc2; 32],
        },
        NativeNodeEvent::LinkEstablished { link_id: link },
        NativeNodeEvent::LinkRttUpdated {
            link_id: link,
            rtt: 1.25,
        },
        NativeNodeEvent::LinkData {
            link_id: link,
            data: vec![0xd1, 0xd2],
            context: 7,
        },
        NativeNodeEvent::ChannelMessages {
            link_id: link,
            messages: vec![(9, vec![0xe1, 0xe2])],
        },
        NativeNodeEvent::RequestReceived {
            link_id: link,
            request_id: request,
            path_hash: path,
            data: vec![0xf1, 0xf2],
        },
        NativeNodeEvent::RequestValueReceived {
            link_id: link,
            request_id: request,
            path_hash: path,
            requested_at: 1_700_000_000.25,
            value: vec![0xc0],
        },
        NativeNodeEvent::ResponseReceived {
            link_id: link,
            request_id: request,
            data: vec![0xf3, 0xf4],
        },
        NativeNodeEvent::LinkClosed { link_id: link },
        NativeNodeEvent::LinkIdentified {
            link_id: link,
            identity_hash: identity,
            public_key: [0x77; 64],
        },
        NativeNodeEvent::ResourceOffered {
            link_id: link,
            resource_hash,
            total_size: 123,
        },
        NativeNodeEvent::ResourceProgress {
            link_id: link,
            resource_hash,
            current: 2,
            total: 4,
        },
        NativeNodeEvent::ResourceComplete {
            link_id: link,
            resource_hash,
            data: vec![0x81, 0x82],
        },
        NativeNodeEvent::ResourceFailed {
            link_id: link,
            resource_hash,
        },
        NativeNodeEvent::ResourceRejected {
            link_id: link,
            resource_hash,
        },
        NativeNodeEvent::RequestFailed {
            link_id: link,
            request_id: request,
            reason: NativeRequestFailReason::ResourceFailed,
        },
        NativeNodeEvent::RequestProgress {
            link_id: link,
            request_id: request,
            current: 5,
            total: 8,
        },
        NativeNodeEvent::Tick {
            expired_paths: 6,
            closed_links: 7,
        },
    ]
    .into_iter()
    .map(|event| {
        let binding = match &event {
            NativeNodeEvent::LinkData { link_id, .. }
            | NativeNodeEvent::RequestReceived { link_id, .. }
            | NativeNodeEvent::RequestValueReceived { link_id, .. } => Some(test_link_binding(
                *link_id.as_bytes(),
                *destination.as_bytes(),
                ApplicationLinkRole::Responder,
            )),
            _ => None,
        };
        project_application_event(event, binding).unwrap()
    })
    .collect();

    let expected_kinds = [
        (ApplicationEventKind::AnnounceReceived, "announce_received"),
        (ApplicationEventKind::DataReceived, "data_received"),
        (ApplicationEventKind::ProofReceived, "proof_received"),
        (ApplicationEventKind::ReceiptFailed, "receipt_failed"),
        (ApplicationEventKind::LinkEstablished, "link_established"),
        (ApplicationEventKind::LinkRttUpdated, "link_rtt_updated"),
        (ApplicationEventKind::LinkData, "link_data"),
        (ApplicationEventKind::ChannelMessages, "channel_messages"),
        (ApplicationEventKind::RequestReceived, "request_received"),
        (
            ApplicationEventKind::RequestValueReceived,
            "request_value_received",
        ),
        (ApplicationEventKind::ResponseReceived, "response_received"),
        (ApplicationEventKind::LinkClosed, "link_closed"),
        (ApplicationEventKind::LinkIdentified, "link_identified"),
        (ApplicationEventKind::ResourceOffered, "resource_offered"),
        (ApplicationEventKind::ResourceProgress, "resource_progress"),
        (ApplicationEventKind::ResourceComplete, "resource_complete"),
        (ApplicationEventKind::ResourceFailed, "resource_failed"),
        (ApplicationEventKind::ResourceRejected, "resource_rejected"),
        (ApplicationEventKind::RequestFailed, "request_failed"),
        (ApplicationEventKind::RequestProgress, "request_progress"),
        (ApplicationEventKind::Tick, "tick"),
    ];
    assert_eq!(projected.len(), expected_kinds.len());
    for (event, (expected_kind, expected_label)) in projected.iter().zip(expected_kinds) {
        assert_eq!(event.kind(), expected_kind);
        assert_eq!(event.kind().as_str(), expected_label);
        assert_eq!(format!("{}", event.kind()), expected_label);
    }

    assert!(matches!(
        &projected[0],
        ApplicationEvent::AnnounceReceived {
            destination: observed_destination,
            identity: observed_identity,
            hops: 3,
            app_data: Some(data),
            ingress: None,
        } if *observed_destination == *destination.as_bytes()
            && *observed_identity == *identity.as_bytes()
            && data == &[0xa1, 0xa2]
    ));
    assert!(matches!(
        &projected[1],
        ApplicationEvent::DataReceived {
            destination: observed,
            payload,
            ingress: None,
        } if *observed == *destination.as_bytes() && payload == &[0xb1, 0xb2]
    ));
    assert!(matches!(
        &projected[2],
        ApplicationEvent::ProofReceived {
            packet_hash,
            ingress: None,
        } if packet_hash == &[0xc1; 32]
    ));
    assert!(matches!(
        &projected[3],
        ApplicationEvent::ReceiptFailed { packet_hash } if packet_hash == &[0xc2; 32]
    ));
    assert!(matches!(
        &projected[4],
        ApplicationEvent::LinkEstablished { link: observed } if *observed == *link.as_bytes()
    ));
    assert!(matches!(
        &projected[5],
        ApplicationEvent::LinkRttUpdated {
            link: observed,
            rtt_seconds: 1.25,
        } if *observed == *link.as_bytes()
    ));
    assert!(matches!(
        &projected[6],
        ApplicationEvent::LinkData {
            binding,
            data,
            context: 7,
            ingress: None,
        } if binding.link() == link.as_bytes()
            && binding.destination() == destination.as_bytes()
            && data == &[0xd1, 0xd2]
    ));
    assert!(matches!(
        &projected[7],
        ApplicationEvent::ChannelMessages {
            link: observed,
            messages,
        } if *observed == *link.as_bytes()
            && messages.len() == 1
            && messages[0].0 == 9
            && messages[0].1 == [0xe1, 0xe2]
    ));
    assert!(matches!(
        &projected[8],
        ApplicationEvent::RequestReceived {
            binding,
            request: observed_request,
            path: observed_path,
            data,
        } if binding.link() == link.as_bytes()
            && binding.destination() == destination.as_bytes()
            && binding.role() == ApplicationLinkRole::Responder
            && *observed_request == *request.as_bytes()
            && *observed_path == *path.as_bytes()
            && data == &[0xf1, 0xf2]
    ));
    assert!(matches!(
        &projected[9],
        ApplicationEvent::RequestValueReceived {
            binding,
            request: observed_request,
            path: observed_path,
            requested_at: 1_700_000_000.25,
            encoded_value,
        } if binding.link() == link.as_bytes()
            && binding.destination() == destination.as_bytes()
            && binding.role() == ApplicationLinkRole::Responder
            && *observed_request == *request.as_bytes()
            && *observed_path == *path.as_bytes()
            && encoded_value == &[0xc0]
    ));
    assert!(matches!(
        &projected[10],
        ApplicationEvent::ResponseReceived {
            link: observed_link,
            request: observed_request,
            data,
        } if *observed_link == *link.as_bytes()
            && *observed_request == *request.as_bytes()
            && data == &[0xf3, 0xf4]
    ));
    assert!(matches!(
        &projected[11],
        ApplicationEvent::LinkClosed { link: observed } if *observed == *link.as_bytes()
    ));
    assert!(matches!(
        &projected[12],
        ApplicationEvent::LinkIdentified {
            link: observed_link,
            identity: observed_identity,
            public_key,
        } if *observed_link == *link.as_bytes()
            && *observed_identity == *identity.as_bytes()
            && public_key == &[0x77; 64]
    ));
    assert!(matches!(
        &projected[13],
        ApplicationEvent::ResourceOffered {
            link: observed,
            resource_hash: observed_hash,
            total_size: 123,
        } if *observed == *link.as_bytes() && observed_hash == &resource_hash
    ));
    assert!(matches!(
        &projected[14],
        ApplicationEvent::ResourceProgress {
            link: observed,
            resource_hash: observed_hash,
            current: 2,
            total: 4,
        } if *observed == *link.as_bytes() && observed_hash == &resource_hash
    ));
    assert!(matches!(
        &projected[15],
        ApplicationEvent::ResourceComplete {
            link: observed,
            resource_hash: observed_hash,
            data,
        } if *observed == *link.as_bytes()
            && observed_hash == &resource_hash
            && data == &[0x81, 0x82]
    ));
    assert!(matches!(
        &projected[16],
        ApplicationEvent::ResourceFailed {
            link: observed,
            resource_hash: observed_hash,
        } if *observed == *link.as_bytes() && observed_hash == &resource_hash
    ));
    assert!(matches!(
        &projected[17],
        ApplicationEvent::ResourceRejected {
            link: observed,
            resource_hash: observed_hash,
        } if *observed == *link.as_bytes() && observed_hash == &resource_hash
    ));
    assert!(matches!(
        &projected[18],
        ApplicationEvent::RequestFailed {
            link: observed_link,
            request: observed_request,
            reason: ApplicationRequestFailReason::ResourceFailed,
        } if *observed_link == *link.as_bytes()
            && *observed_request == *request.as_bytes()
    ));
    assert!(matches!(
        &projected[19],
        ApplicationEvent::RequestProgress {
            link: observed_link,
            request: observed_request,
            current: 5,
            total: 8,
        } if *observed_link == *link.as_bytes()
            && *observed_request == *request.as_bytes()
    ));
    assert!(matches!(
        &projected[20],
        ApplicationEvent::Tick {
            expired_paths: 6,
            closed_links: 7,
        }
    ));
}

#[test]
fn ingress_observation_attaches_to_bounded_ingress_originated_events() {
    let mut actions = NodeActions::without_retained_proofs(
        vec![
            ApplicationEvent::AnnounceReceived {
                destination: [0x11; rete_core::TRUNCATED_HASH_LEN],
                identity: [0x22; rete_core::TRUNCATED_HASH_LEN],
                hops: 2,
                app_data: None,
                ingress: None,
            },
            ApplicationEvent::DataReceived {
                destination: [0x33; rete_core::TRUNCATED_HASH_LEN],
                payload: vec![0x44],
                ingress: None,
            },
            ApplicationEvent::ProofReceived {
                packet_hash: [0x55; 32],
                ingress: None,
            },
            ApplicationEvent::LinkData {
                binding: test_link_binding(
                    [0x66; rete_core::TRUNCATED_HASH_LEN],
                    [0x77; rete_core::TRUNCATED_HASH_LEN],
                    ApplicationLinkRole::Responder,
                ),
                data: vec![0x88],
                context: LINK_DATA_CONTEXT_NONE,
                ingress: None,
            },
            ApplicationEvent::Tick {
                expired_paths: 0,
                closed_links: 0,
            },
        ],
        Vec::new(),
        0,
    );

    actions.attach_ingress_observation(7, Some((-91, 4)));

    let expected =
        IngressObservation::remote(InterfaceId(7), Some(IngressSignalObservation::new(-91, 4)));
    for event in &actions.events.as_slice()[..4] {
        let observation = match event {
            ApplicationEvent::AnnounceReceived { ingress, .. }
            | ApplicationEvent::DataReceived { ingress, .. }
            | ApplicationEvent::ProofReceived { ingress, .. }
            | ApplicationEvent::LinkData { ingress, .. } => *ingress,
            _ => panic!("test event changed variant"),
        };
        assert_eq!(observation, Some(expected));
    }
    assert!(matches!(actions.events[4], ApplicationEvent::Tick { .. }));
}

#[test]
fn application_event_debug_redacts_owned_bodies_and_public_key() {
    let resource = ApplicationEvent::ResourceComplete {
        link: [0x11; rete_core::TRUNCATED_HASH_LEN],
        resource_hash: [0x22; rete_core::TRUNCATED_HASH_LEN],
        data: vec![222, 173, 190, 239],
    };
    let resource_debug = format!("{resource:?}");
    assert!(resource_debug.contains("data_len: 4"));
    assert!(!resource_debug.contains("222, 173, 190, 239"));

    let request_value = ApplicationEvent::RequestValueReceived {
        binding: test_link_binding(
            [0x31; rete_core::TRUNCATED_HASH_LEN],
            [0x34; rete_core::TRUNCATED_HASH_LEN],
            ApplicationLinkRole::Responder,
        ),
        request: [0x32; rete_core::TRUNCATED_HASH_LEN],
        path: [0x33; rete_core::TRUNCATED_HASH_LEN],
        requested_at: 1_700_000_000.25,
        encoded_value: vec![222, 173, 190, 239],
    };
    let request_value_debug = format!("{request_value:?}");
    assert!(request_value_debug.contains("requested_at: 1700000000.25"));
    assert!(request_value_debug.contains("encoded_value_len: 4"));
    assert!(!request_value_debug.contains("222, 173, 190, 239"));

    let identified = ApplicationEvent::LinkIdentified {
        link: [0x41; rete_core::TRUNCATED_HASH_LEN],
        identity: [0x44; rete_core::TRUNCATED_HASH_LEN],
        public_key: [0x55; 64],
    };
    let identified_debug = format!("{identified:?}");
    assert!(identified_debug.contains("[redacted; 64 bytes]"));
    assert!(!identified_debug.contains("85, 85, 85"));
}

fn identity(tag: u8) -> Identity {
    Identity::from_seed(&[tag; 32]).unwrap()
}

fn node(tag: u8) -> TestNode {
    TestNode::new(
        identity(tag),
        "reticulum",
        &["embedded"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap()
}

#[test]
fn shared_medium_interfaces_are_applied_to_transport() {
    let node = TestNode::new(
        identity(60),
        "reticulum",
        &["embedded"],
        EmbeddedNodeConfig::endpoint().with_shared_medium_interfaces(1 << 1),
    )
    .unwrap();
    assert!(node.core.transport.interface_is_shared_medium(1));
    assert!(!node.core.transport.interface_is_shared_medium(0));
    assert!(!node.core.transport.interface_is_shared_medium(2));
}

#[test]
#[allow(
    unsafe_code,
    reason = "the successful in-place test value is explicitly dropped after its borrow ends"
)]
fn in_place_construction_matches_value_construction() {
    let by_value = TestNode::new(
        identity(41),
        "reticulum",
        &["embedded"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut destination = MaybeUninit::<TestNode>::uninit();
    {
        let in_place = TestNode::new_in(
            &mut destination,
            identity(41),
            "reticulum",
            &["embedded"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();

        assert_eq!(in_place.destination_hash(), by_value.destination_hash());
        assert_eq!(in_place.identity_hash(), by_value.identity_hash());
        assert_eq!(in_place.role(), by_value.role());
        assert_eq!(in_place.metrics(), by_value.metrics());
    }

    // SAFETY: `new_in` returned success above and the resulting reference
    // no longer exists, so the slot contains exactly one initialized node.
    unsafe { destination.assume_init_drop() };
}

#[test]
#[allow(
    unsafe_code,
    reason = "the successful retry value is explicitly dropped after its borrow ends"
)]
fn failed_in_place_construction_leaves_destination_reusable() {
    let mut destination = MaybeUninit::<TestNode>::uninit();
    let oversized_name = "x".repeat(130);
    let failure = TestNode::new_in(
        &mut destination,
        identity(42),
        &oversized_name,
        &[],
        EmbeddedNodeConfig::endpoint(),
    );
    assert!(matches!(failure, Err(rete_core::Error::BufferTooSmall)));

    let expected = TestNode::new(
        identity(42),
        "reticulum",
        &["retry"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    {
        let retried = TestNode::new_in(
            &mut destination,
            identity(42),
            "reticulum",
            &["retry"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        assert_eq!(retried.destination_hash(), expected.destination_hash());
        assert_eq!(retried.metrics(), expected.metrics());
    }

    // SAFETY: the failed attempt left the slot uninitialized and the
    // retry returned success; its resulting reference no longer exists.
    unsafe { destination.assume_init_drop() };
}

#[test]
fn canonical_probe_registration_is_inbound_single_prove_all_without_links() {
    let mut responder = node(201);
    let destination = responder.register_probe_destination().unwrap();
    let identity_hash = responder.identity_hash();
    assert_eq!(
        destination,
        rete_core::destination_hash(RNSTRANSPORT_PROBE_EXPANDED_NAME, Some(&identity_hash))
    );

    let registered = responder
        .core
        .get_destination(&destination)
        .expect("registered probe destination remains visible");
    assert_eq!(registered.app_name, RNSTRANSPORT_PROBE_APPLICATION_NAME);
    assert_eq!(registered.aspects.as_slice(), &[RNSTRANSPORT_PROBE_ASPECT]);
    assert_eq!(registered.direction, Direction::In);
    assert_eq!(registered.dest_type, DestinationType::Single);
    assert!(!registered.accepts_links);
    assert_eq!(
        registered.proof_strategy,
        rete_stack::ProofStrategy::ProveAll
    );
}

#[test]
fn canonical_probe_destination_returns_a_proof_with_exact_ingress_signal() {
    let mut sender = node(202);
    let mut responder = node(203);
    let probe_destination = responder.register_probe_destination().unwrap();
    sender
        .register_peer(
            &identity(203),
            RNSTRANSPORT_PROBE_APPLICATION_NAME,
            &[RNSTRANSPORT_PROBE_ASPECT],
            100,
        )
        .unwrap();

    let mut rng = CounterRng::default();
    let mut output = [0_u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &probe_destination,
            b"standard proof probe",
            101,
            &mut rng,
            &mut output,
        )
        .unwrap();
    let received = responder.ingest(
        &output[..usize::from(prepared.packet_len())],
        101,
        InterfaceId(7),
        &mut rng,
    );
    assert!(received.actions.events.iter().any(|event| matches!(
        event,
        ApplicationEvent::DataReceived {
            destination,
            payload,
            ..
        } if *destination == *probe_destination.as_bytes()
            && payload == b"standard proof probe"
    )));
    assert_eq!(received.actions.packets.len(), 1);
    let proof = &received.actions.packets[0];
    assert_eq!(proof.target(), TxTarget::Only(InterfaceId(7)));

    let ingress =
        IngressObservation::remote(InterfaceId(9), Some(IngressSignalObservation::new(-88, 7)));
    let expected = ReceiptCandidate {
        kind: ReceiptKind::Data,
        receipt: prepared.receipt(),
        ingress: Some(ingress),
    };
    let mut sink = RecordingReceiptSink::default();
    sender
        .ingest_observed_with_receipt_sink_at_with_broadcast_scope(
            proof.bytes(),
            102,
            MonotonicInstant::from_secs(102),
            ingress,
            IngressBroadcastScope::SharedMedium,
            &mut rng,
            &mut sink,
        )
        .unwrap();
    assert_eq!(sink.terminals, [ReceiptTerminal::Delivered(expected)]);
}

#[test]
fn probe_destination_derives_from_any_retained_announce_identity() {
    let mut owner = node(204);
    let peer = identity(205);
    owner
        .register_peer(&peer, LXMF_APPLICATION_NAME, &[LXMF_DELIVERY_ASPECT], 100)
        .unwrap();
    let identity_hash = peer.hash();
    let announced = rete_core::destination_hash(LXMF_DELIVERY_EXPANDED_NAME, Some(&identity_hash));
    let expected =
        rete_core::destination_hash(RNSTRANSPORT_PROBE_EXPANDED_NAME, Some(&identity_hash));
    assert_eq!(
        owner.proof_probe_destination_for(&announced),
        Some(expected)
    );
    assert!(!owner.has_path(&expected));
    let mut rng = CounterRng::default();
    let mut output = [0xa5_u8; RNS_MTU];
    assert_eq!(
        owner.prepare_data_into(&expected, b"identity alias", 101, &mut rng, &mut output),
        Err(PrepareDataError::UnknownDestination)
    );
    assert!(output.iter().all(|byte| *byte == 0xa5));

    assert_eq!(
        owner.prepare_proof_probe_destination_for(&announced),
        Ok(expected)
    );
    assert_eq!(
        owner.prepare_proof_probe_destination_for(&announced),
        Ok(expected),
        "identity aliasing is idempotent"
    );
    assert_eq!(owner.recall_identity(&expected), Some(peer.public_key()));
    assert!(
        !owner.has_path(&expected),
        "identity aliasing must not fabricate a direct path"
    );
    let prepared = owner
        .prepare_data_into(&expected, b"identity alias", 102, &mut rng, &mut output)
        .expect("the promoted identity enables probe DATA encryption");
    assert_eq!(prepared.target(), TxTarget::All);
    assert!(owner.cancel_data_receipt(prepared.receipt()));

    assert_eq!(
        owner.proof_probe_destination_for(&DestHash::from([0xee; 16])),
        None
    );
    assert_eq!(
        owner.prepare_proof_probe_destination_for(&DestHash::from([0xee; 16])),
        Err(ProofProbeIdentityAliasError::SourceIdentityUnknown)
    );
}

fn fixture_hash(encoded: &str) -> DestHash {
    let bytes = hex::decode(encoded).expect("fixture hash is hexadecimal");
    DestHash::from_slice(&bytes)
}

#[derive(Deserialize)]
struct LxmfCorpus {
    fixture_identity: LxmfCorpusIdentity,
    messages: Vec<LxmfCorpusMessage>,
}

#[derive(Deserialize)]
struct LxmfCorpusIdentity {
    destination_public_key_hex: String,
}

#[derive(Deserialize)]
struct LxmfCorpusMessage {
    decoded: LxmfCorpusDecoded,
    destination_hash_hex: String,
    full_wire_hex: String,
    message_id_hex: String,
    name: String,
    selection_content_size: i64,
    source_hash_hex: String,
}

#[derive(Deserialize)]
struct LxmfCorpusDecoded {
    content_hex: String,
    timestamp_f64_bits_hex: String,
    title_hex: String,
}

fn python_lxmf_corpus() -> LxmfCorpus {
    serde_json::from_str(include_str!(
        "../../../../interop/vectors/lxmf-1.0.1-v1.json"
    ))
    .expect("checked-in Python LXMF corpus parses")
}

fn corpus_message<'a>(corpus: &'a LxmfCorpus, name: &str) -> &'a LxmfCorpusMessage {
    corpus
        .messages
        .iter()
        .find(|message| message.name == name)
        .expect("named Python LXMF vector exists")
}

fn corpus_timestamp_ms(message: &LxmfCorpusMessage) -> u64 {
    let bits = u64::from_str_radix(&message.decoded.timestamp_f64_bits_hex, 16)
        .expect("fixture timestamp bits are hexadecimal");
    let seconds = f64::from_bits(bits);
    let milliseconds = (seconds * 1_000.0) as u64;
    assert_eq!(milliseconds as f64 / 1_000.0, seconds);
    milliseconds
}

fn python_lxmf_fixture_node_without_delivery() -> TestNode {
    let mut private_key = [0_u8; 64];
    private_key[..32].fill(0x05);
    private_key[32..].fill(0x06);
    TestNode::new(
        Identity::from_private_key(&private_key).expect("fixture identity imports"),
        "reticulum",
        &["embedded"],
        EmbeddedNodeConfig::endpoint(),
    )
    .expect("fixture node constructs")
}

fn python_lxmf_fixture_node() -> TestNode {
    let mut node = python_lxmf_fixture_node_without_delivery();
    let source = node
        .register_destination(
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            DestinationType::Single,
            Direction::In,
        )
        .expect("fixture LXMF delivery destination registers");
    assert_eq!(source, fixture_hash("20f7e44b55b06cff39719106f2bd1fd2"));
    node
}

fn python_lxmf_fixture_recipient_identity() -> Identity {
    let mut private_key = [0_u8; 64];
    private_key[..32].fill(0x07);
    private_key[32..].fill(0x08);
    Identity::from_private_key(&private_key).expect("fixture recipient identity imports")
}

fn python_lxmf_fixture_recipient_node() -> (TestNode, DestHash) {
    let mut node = TestNode::new(
        python_lxmf_fixture_recipient_identity(),
        "reticulum",
        &["embedded"],
        EmbeddedNodeConfig::endpoint(),
    )
    .expect("fixture recipient node constructs");
    let destination = node
        .register_destination(
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            DestinationType::Single,
            Direction::In,
        )
        .expect("fixture recipient delivery destination registers");
    assert!(node.set_accepts_links(&destination, true));
    (node, destination)
}

fn python_lxmf_fixture_sender(fixture: &LxmfCorpusMessage) -> (TestNode, DestHash) {
    let mut sender = python_lxmf_fixture_node();
    let recipient = python_lxmf_fixture_recipient_identity();
    let destination = fixture_hash(&fixture.destination_hash_hex);
    sender
        .register_peer(
            &recipient,
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            1,
        )
        .unwrap();
    (sender, destination)
}

#[test]
fn basic_lxmf_composition_matches_python_1_0_1_exactly() {
    let node = python_lxmf_fixture_node();
    let destination = fixture_hash("021e68345db8a80c29d0c2f193baa5f4");
    let expected = hex::decode(
        "021e68345db8a80c29d0c2f193baa5f4\
         20f7e44b55b06cff39719106f2bd1fd2\
         cfeaf89e57248baad43791a115345482f6b54b6e90aa0d02b5d8eddad1dc6a6\
         a323ec74921c618ae95e69153e9645db6f223d5d387db37ae23f58ef1f0560700\
         94cb41d954fc40000000c4094772656574696e6773\
         c41648656c6c6f2066726f6d20507974686f6e204c584d4680",
    )
    .expect("Python LXMF fixture is hexadecimal");
    let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];

    let prepared = node
        .prepare_basic_lxmf_into(
            &destination,
            1_700_000_000_000,
            b"Greetings",
            b"Hello from Python LXMF",
            &mut carrier,
        )
        .expect("Python-compatible basic message prepares");

    assert_eq!(
        &carrier[..usize::from(prepared.carrier_len())],
        &expected[LXMF_DESTINATION_HASH_LENGTH..]
    );
    let expected_message_id: [u8; 32] =
        hex::decode("c00af1f9ba72e66d4b9a41fbe76a55d6bbb1c8dfb9271f0cf660ed101e174c96")
            .unwrap()
            .try_into()
            .unwrap();
    assert_eq!(prepared.message_id(), expected_message_id);
    assert_eq!(prepared.destination(), destination);
}

#[test]
fn basic_direct_lxmf_composition_matches_python_and_exact_link_boundary() {
    let corpus = python_lxmf_corpus();
    let at_limit = corpus_message(&corpus, "direct_limit_319");
    let over_limit = corpus_message(&corpus, "direct_over_320");
    assert_eq!(
        at_limit.selection_content_size,
        MAX_DIRECT_LXMF_CONTENT_SIZE as i64
    );
    assert_eq!(
        over_limit.selection_content_size,
        MAX_DIRECT_LXMF_CONTENT_SIZE as i64 + 1
    );

    let node = python_lxmf_fixture_node();
    let destination = fixture_hash(&at_limit.destination_hash_hex);
    let mut wire = [0xa2_u8; MAX_DIRECT_LXMF_WIRE];
    let prepared = node
        .prepare_basic_direct_lxmf_into(
            &destination,
            corpus_timestamp_ms(at_limit),
            &hex::decode(&at_limit.decoded.title_hex).unwrap(),
            &hex::decode(&at_limit.decoded.content_hex).unwrap(),
            &mut wire,
        )
        .expect("Python's exact direct packet boundary prepares");
    let expected_wire = hex::decode(&at_limit.full_wire_hex).unwrap();
    assert_eq!(expected_wire.len(), MAX_DIRECT_LXMF_WIRE);
    assert_eq!(usize::from(prepared.wire_len()), expected_wire.len());
    assert_eq!(wire.as_slice(), expected_wire.as_slice());
    assert_eq!(prepared.destination(), destination);
    let expected_message_id: [u8; 32] = hex::decode(&at_limit.message_id_hex)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(prepared.message_id(), expected_message_id);

    let mut untouched = [0x5c_u8; MAX_DIRECT_LXMF_WIRE];
    assert_eq!(
        node.prepare_basic_direct_lxmf_into(
            &fixture_hash(&over_limit.destination_hash_hex),
            corpus_timestamp_ms(over_limit),
            &hex::decode(&over_limit.decoded.title_hex).unwrap(),
            &hex::decode(&over_limit.decoded.content_hex).unwrap(),
            &mut untouched,
        ),
        Err(PrepareBasicLxmfError::PayloadTooLarge {
            actual: MAX_DIRECT_LXMF_CONTENT_SIZE + 1,
            maximum: MAX_DIRECT_LXMF_CONTENT_SIZE,
        })
    );
    assert!(untouched.iter().all(|byte| *byte == 0x5c));
}

#[test]
fn exact_python_direct_lxmf_round_trips_over_bound_link_with_typed_receipt() {
    let corpus = python_lxmf_corpus();
    let fixture = corpus_message(&corpus, "direct_limit_319");
    let expected_wire = hex::decode(&fixture.full_wire_hex).unwrap();
    let mut initiator = python_lxmf_fixture_node();
    let (mut responder, destination) = python_lxmf_fixture_recipient_node();
    assert_eq!(destination, fixture_hash(&fixture.destination_hash_hex));
    initiator
        .register_peer(
            &python_lxmf_fixture_recipient_identity(),
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            99,
        )
        .unwrap();
    responder
        .set_destination_inbound_proof_policy(&destination, InboundProofPolicy::Always)
        .unwrap();
    let mut rng = CounterRng::default();

    let (request, link_id) = initiator.initiate_link(destination, 100, &mut rng).unwrap();
    let response = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
    let established = initiator.ingest(
        response.actions.packets[0].bytes(),
        101,
        InterfaceId(3),
        &mut rng,
    );
    responder.ingest(
        established.actions.packets[0].bytes(),
        102,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(initiator.link_state(&link_id), Some(LinkState::Active));
    assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));

    let mut packet = [0_u8; RNS_MTU];
    let prepared = initiator
        .prepare_rehydrated_direct_lxmf_link_data_into(
            &expected_wire,
            &link_id,
            110,
            &mut rng,
            &mut packet,
        )
        .expect("exact direct wire gains one Link-DATA receipt");
    assert_eq!(prepared.target(), TxTarget::Only(InterfaceId(3)));
    let outbound = Packet::parse(&packet[..usize::from(prepared.packet_len())]).unwrap();
    assert_eq!(&outbound.compute_hash(), prepared.receipt().as_bytes());
    assert_eq!(initiator.core.transport.link_data_receipt_count(), 1);

    let received = responder.ingest(
        &packet[..usize::from(prepared.packet_len())],
        110,
        InterfaceId(7),
        &mut rng,
    );
    assert!(matches!(
        received.actions.events.as_slice(),
        [ApplicationEvent::LinkData {
            binding,
            data,
            context: LINK_DATA_CONTEXT_NONE,
            ..
        }] if binding.link() == link_id.as_bytes()
            && binding.destination() == destination.as_bytes()
            && binding.role() == ApplicationLinkRole::Responder
            && data.as_slice() == expected_wire.as_slice()
    ));
    assert_eq!(received.metadata.generated_proof_actions(), 1);
    let proof = received
        .actions
        .packets
        .iter()
        .find(|packet| {
            Packet::parse(packet.bytes())
                .is_ok_and(|packet| packet.packet_type == PacketType::Proof)
        })
        .expect("ordinary Link DATA produces an explicit proof");
    assert_eq!(proof.target(), TxTarget::Only(InterfaceId(7)));

    let expected = ReceiptCandidate {
        kind: ReceiptKind::LinkData,
        receipt: prepared.receipt(),
        ingress: Some(IngressObservation::remote(InterfaceId(3), None)),
    };
    let mut sink = RecordingReceiptSink::default();
    let delivered = initiator
        .ingest_with_receipt_sink(proof.bytes(), 111, InterfaceId(3), &mut rng, &mut sink)
        .unwrap();
    assert_eq!(delivered.metadata.delivered_receipt_terminals(), 1);
    assert_eq!(sink.attempted, [expected]);
    assert_eq!(sink.terminals, [ReceiptTerminal::Delivered(expected)]);
    assert_eq!(initiator.core.transport.link_data_receipt_count(), 0);
}

#[test]
fn direct_lxmf_substitution_is_rejected_before_entropy_or_receipt_state() {
    let corpus = python_lxmf_corpus();
    let fixture = corpus_message(&corpus, "direct_limit_319");
    let mut sender = python_lxmf_fixture_node();
    let (mut responder, destination) = python_lxmf_fixture_recipient_node();
    sender
        .register_peer(
            &python_lxmf_fixture_recipient_identity(),
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            99,
        )
        .unwrap();
    let mut rng = CounterRng::default();
    let (request, link_id) = sender.initiate_link(destination, 100, &mut rng).unwrap();
    let response = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
    let established = sender.ingest(
        response.actions.packets[0].bytes(),
        101,
        InterfaceId(3),
        &mut rng,
    );
    responder.ingest(
        established.actions.packets[0].bytes(),
        102,
        InterfaceId(7),
        &mut rng,
    );

    let mut wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
    let composed = sender
        .prepare_basic_direct_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &hex::decode(&fixture.decoded.title_hex).unwrap(),
            &hex::decode(&fixture.decoded.content_hex).unwrap(),
            &mut wire,
        )
        .unwrap();
    let mut output = [0xa5_u8; RNS_MTU];
    let entropy_before = rng.0;

    assert_eq!(
        sender.prepare_direct_lxmf_link_data_into(
            composed,
            &wire[..wire.len() - 1],
            &link_id,
            110,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDirectLxmfLinkDataError::WireLengthMismatch {
            actual: MAX_DIRECT_LXMF_WIRE - 1,
            expected: MAX_DIRECT_LXMF_WIRE,
        })
    );
    assert_eq!(rng.0, entropy_before);
    assert_eq!(sender.core.transport.link_data_receipt_count(), 0);
    assert!(output.iter().all(|byte| *byte == 0xa5));

    let mut substituted = wire;
    *substituted.last_mut().unwrap() ^= 0x01;
    assert_eq!(
        sender.prepare_direct_lxmf_link_data_into(
            composed,
            &substituted,
            &link_id,
            110,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDirectLxmfLinkDataError::WireDigestMismatch)
    );
    assert_eq!(rng.0, entropy_before);
    assert_eq!(sender.core.transport.link_data_receipt_count(), 0);
    assert!(output.iter().all(|byte| *byte == 0xa5));

    let inconsistent_destination = PreparedBasicDirectLxmf {
        destination: DestHash::from([0xd1; LXMF_DESTINATION_HASH_LENGTH]),
        ..composed
    };
    assert_eq!(
        sender.prepare_direct_lxmf_link_data_into(
            inconsistent_destination,
            &wire,
            &link_id,
            110,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDirectLxmfLinkDataError::WireDestinationMismatch)
    );
    assert_eq!(rng.0, entropy_before);
    assert_eq!(sender.core.transport.link_data_receipt_count(), 0);

    let other_destination = DestHash::from([0xd2; LXMF_DESTINATION_HASH_LENGTH]);
    let mut other_destination_wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
    let other_destination_composed = sender
        .prepare_basic_direct_lxmf_into(
            &other_destination,
            corpus_timestamp_ms(fixture),
            &hex::decode(&fixture.decoded.title_hex).unwrap(),
            &hex::decode(&fixture.decoded.content_hex).unwrap(),
            &mut other_destination_wire,
        )
        .unwrap();
    assert_eq!(
        sender.prepare_direct_lxmf_link_data_into(
            other_destination_composed,
            &other_destination_wire,
            &link_id,
            110,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDirectLxmfLinkDataError::LinkDestinationMismatch)
    );
    assert_eq!(rng.0, entropy_before);
    assert_eq!(sender.core.transport.link_data_receipt_count(), 0);

    let mut other_source = node(77);
    other_source
        .register_destination(
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            DestinationType::Single,
            Direction::In,
        )
        .unwrap();
    let mut other_source_wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
    let other_source_composed = other_source
        .prepare_basic_direct_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &hex::decode(&fixture.decoded.title_hex).unwrap(),
            &hex::decode(&fixture.decoded.content_hex).unwrap(),
            &mut other_source_wire,
        )
        .unwrap();
    assert_eq!(
        sender.prepare_direct_lxmf_link_data_into(
            other_source_composed,
            &other_source_wire,
            &link_id,
            110,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDirectLxmfLinkDataError::WireSourceMismatch)
    );
    assert_eq!(rng.0, entropy_before);
    assert_eq!(sender.core.transport.link_data_receipt_count(), 0);
    assert!(output.iter().all(|byte| *byte == 0xa5));
}

#[test]
fn direct_lxmf_link_preflight_enforces_state_mdu_capacity_cancel_and_timeout() {
    let corpus = python_lxmf_corpus();
    let fixture = corpus_message(&corpus, "direct_limit_319");
    let mut sender = python_lxmf_fixture_node();
    let (mut responder, destination) = python_lxmf_fixture_recipient_node();
    sender
        .register_peer(
            &python_lxmf_fixture_recipient_identity(),
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            99,
        )
        .unwrap();
    let mut rng = CounterRng::default();
    let (request, link_id) = sender.initiate_link(destination, 100, &mut rng).unwrap();
    let response = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
    let established = sender.ingest(
        response.actions.packets[0].bytes(),
        101,
        InterfaceId(3),
        &mut rng,
    );
    responder.ingest(
        established.actions.packets[0].bytes(),
        102,
        InterfaceId(7),
        &mut rng,
    );

    let mut wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
    let composed = sender
        .prepare_basic_direct_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &hex::decode(&fixture.decoded.title_hex).unwrap(),
            &hex::decode(&fixture.decoded.content_hex).unwrap(),
            &mut wire,
        )
        .unwrap();
    let mut output = [0x91_u8; RNS_MTU];

    let entropy_before_missing = rng.0;
    assert_eq!(
        sender.prepare_direct_lxmf_link_data_into(
            composed,
            &wire,
            &LinkId::from([0xee; LXMF_DESTINATION_HASH_LENGTH]),
            110,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDirectLxmfLinkDataError::LinkNotFound)
    );
    assert_eq!(rng.0, entropy_before_missing);
    assert_eq!(sender.core.transport.link_data_receipt_count(), 0);

    let mut pending_sender = python_lxmf_fixture_node();
    pending_sender
        .register_peer(
            &python_lxmf_fixture_recipient_identity(),
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            99,
        )
        .unwrap();
    let (_, pending_link) = pending_sender
        .initiate_link(destination, 100, &mut rng)
        .unwrap();
    let entropy_before_inactive = rng.0;
    assert_eq!(
        pending_sender.prepare_direct_lxmf_link_data_into(
            composed,
            &wire,
            &pending_link,
            110,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDirectLxmfLinkDataError::LinkNotActive)
    );
    assert_eq!(rng.0, entropy_before_inactive);
    assert_eq!(pending_sender.core.transport.link_data_receipt_count(), 0);

    let original_signalling = sender.core.transport.get_link(&link_id).unwrap().signalling;
    sender
        .core
        .transport
        .get_link_mut(&link_id)
        .unwrap()
        .signalling = rete_transport::signalling_bytes(300, 1);
    let reduced_mdu = rete_transport::compute_link_mdu(300);
    let entropy_before_mdu = rng.0;
    assert_eq!(
        sender.prepare_direct_lxmf_link_data_into(
            composed,
            &wire,
            &link_id,
            110,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDirectLxmfLinkDataError::LinkMduExceeded {
            actual: MAX_DIRECT_LXMF_WIRE,
            maximum: reduced_mdu,
        })
    );
    assert_eq!(rng.0, entropy_before_mdu);
    assert_eq!(sender.core.transport.link_data_receipt_count(), 0);
    assert_eq!(sender.metrics().admission.link_payload_too_large, 1);
    sender
        .core
        .transport
        .get_link_mut(&link_id)
        .unwrap()
        .signalling = original_signalling;

    let mut receipts = Vec::new();
    for now in 110..114 {
        let prepared = sender
            .prepare_direct_lxmf_link_data_into(
                composed,
                &wire,
                &link_id,
                now,
                &mut rng,
                &mut output,
            )
            .unwrap();
        assert_eq!(prepared.target(), TxTarget::Only(InterfaceId(3)));
        receipts.push(prepared.receipt());
    }
    assert_eq!(sender.core.transport.link_data_receipt_count(), 4);
    let entropy_before_full = rng.0;
    assert_eq!(
        sender.prepare_direct_lxmf_link_data_into(
            composed,
            &wire,
            &link_id,
            114,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDirectLxmfLinkDataError::ReceiptTableFull { limit: 4 })
    );
    assert_eq!(rng.0, entropy_before_full);
    assert_eq!(sender.core.transport.link_data_receipt_count(), 4);
    assert_eq!(sender.metrics().admission.link_data_receipt_table_full, 1);

    for receipt in receipts {
        assert!(sender.cancel_link_data_receipt(receipt));
        assert!(!sender.cancel_link_data_receipt(receipt));
    }
    assert_eq!(sender.core.transport.link_data_receipt_count(), 0);

    let expiring = sender
        .prepare_direct_lxmf_link_data_into(composed, &wire, &link_id, 200, &mut rng, &mut output)
        .unwrap();
    let expected = ReceiptCandidate {
        kind: ReceiptKind::LinkData,
        receipt: expiring.receipt(),
        ingress: None,
    };
    let mut sink = RecordingReceiptSink::default();
    let report = sender.tick_with_receipt_sink(231, &mut rng, &mut sink);
    assert_eq!(report.timed_out_receipts, 0);
    assert_eq!(report.timed_out_link_data_receipts, 1);
    assert!(!report.receipt_terminals_deferred);
    assert_eq!(sink.attempted, [expected]);
    assert_eq!(sink.terminals, [ReceiptTerminal::TimedOut(expected)]);
    assert_eq!(sender.core.transport.link_data_receipt_count(), 0);
}

#[test]
fn direct_lxmf_keeps_the_destination_prefix_opportunistic_data_omits() {
    let corpus = python_lxmf_corpus();
    let fixture = corpus_message(&corpus, "opportunistic_over_296");
    let node = python_lxmf_fixture_node();
    let destination = fixture_hash(&fixture.destination_hash_hex);
    let title = hex::decode(&fixture.decoded.title_hex).unwrap();
    let content = hex::decode(&fixture.decoded.content_hex).unwrap();
    let expected_wire = hex::decode(&fixture.full_wire_hex).unwrap();
    let mut direct = [0x61_u8; MAX_DIRECT_LXMF_WIRE];

    let prepared = node
        .prepare_basic_direct_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &title,
            &content,
            &mut direct,
        )
        .expect("a message above the opportunistic boundary remains direct-packet sized");
    let wire_len = usize::from(prepared.wire_len());
    assert_eq!(&direct[..wire_len], expected_wire.as_slice());
    assert_eq!(
        &direct[..LXMF_DESTINATION_HASH_LENGTH],
        destination.as_ref()
    );
    assert!(direct[wire_len..].iter().all(|byte| *byte == 0x61));

    let mut opportunistic = [0x62_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    assert_eq!(
        node.prepare_basic_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &title,
            &content,
            &mut opportunistic,
        ),
        Err(PrepareBasicLxmfError::PayloadTooLarge {
            actual: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE + 1,
            maximum: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE,
        })
    );
    assert!(opportunistic.iter().all(|byte| *byte == 0x62));
}

#[test]
fn basic_lxmf_location_is_sideband_compatible_and_changes_message_identity() {
    let node = python_lxmf_fixture_node();
    let destination = fixture_hash("021e68345db8a80c29d0c2f193baa5f4");
    let location = SidebandLocationTelemetry::new(
        44_123_456,
        -73_987_654,
        12_345,
        678,
        12_345,
        250,
        1_785_700_123,
    );
    let mut located_wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
    let located = node
        .prepare_basic_direct_lxmf_with_location_into(
            &destination,
            1_785_700_123_456,
            b"location",
            b"meet me here",
            location,
            &mut located_wire,
        )
        .unwrap();
    let decoded = LXMessage::unpack(
        &located_wire[..usize::from(located.wire_len())],
        Some(node.core.identity()),
    )
    .unwrap();
    assert_eq!(decoded.fields.len(), 1);
    let telemetry = decoded.fields.get(&FIELD_TELEMETRY).unwrap();
    let mut expected = [0_u8; MAX_ENCODED_SIDEBAND_LOCATION_TELEMETRY_BYTES];
    let expected_len = encode_sideband_location_telemetry(location, &mut expected).unwrap();
    assert_eq!(telemetry, &expected[..expected_len]);

    let mut plain_wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
    let plain = node
        .prepare_basic_direct_lxmf_into(
            &destination,
            1_785_700_123_456,
            b"location",
            b"meet me here",
            &mut plain_wire,
        )
        .unwrap();
    assert_ne!(located.message_id(), plain.message_id());

    let maximum_located_content = vec![0x42; 268];
    let mut boundary_wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
    node.prepare_basic_direct_lxmf_with_location_into(
        &destination,
        1_785_700_123_456,
        b"",
        &maximum_located_content,
        location,
        &mut boundary_wire,
    )
    .expect("268 content bytes exactly fit with current Sideband location fields");

    let oversized_located_content = vec![0x42; 269];
    assert_eq!(
        node.prepare_basic_direct_lxmf_with_location_into(
            &destination,
            1_785_700_123_456,
            b"",
            &oversized_located_content,
            location,
            &mut boundary_wire,
        ),
        Err(PrepareBasicLxmfError::PayloadTooLarge {
            actual: MAX_DIRECT_LXMF_CONTENT_SIZE + 1,
            maximum: MAX_DIRECT_LXMF_CONTENT_SIZE,
        })
    );
}

#[test]
fn basic_lxmf_rejections_leave_caller_output_unchanged() {
    let node = python_lxmf_fixture_node();
    let destination = fixture_hash("021e68345db8a80c29d0c2f193baa5f4");

    let mut invalid_time = [0x31_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    assert_eq!(
        node.prepare_basic_lxmf_into(&destination, 0, b"title", b"content", &mut invalid_time,),
        Err(PrepareBasicLxmfError::InvalidTimestamp)
    );
    assert!(invalid_time.iter().all(|byte| *byte == 0x31));

    let mut too_large = [0x52_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let content = vec![0x42; MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE + 1];
    assert_eq!(
        node.prepare_basic_lxmf_into(
            &destination,
            1_700_000_000_000,
            b"",
            &content,
            &mut too_large,
        ),
        Err(PrepareBasicLxmfError::PayloadTooLarge {
            actual: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE + 1,
            maximum: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE,
        })
    );
    assert!(too_large.iter().all(|byte| *byte == 0x52));

    let mut too_small = [0x73_u8; 8];
    assert!(matches!(
        node.prepare_basic_lxmf_into(
            &destination,
            1_700_000_000_000,
            b"title",
            b"content",
            &mut too_small,
        ),
        Err(PrepareBasicLxmfError::OutputTooSmall { available: 8, .. })
    ));
    assert!(too_small.iter().all(|byte| *byte == 0x73));

    let mut direct_too_small = [0x74_u8; 8];
    assert!(matches!(
        node.prepare_basic_direct_lxmf_into(
            &destination,
            1_700_000_000_000,
            b"title",
            b"content",
            &mut direct_too_small,
        ),
        Err(PrepareBasicLxmfError::OutputTooSmall { available: 8, .. })
    ));
    assert!(direct_too_small.iter().all(|byte| *byte == 0x74));
}

#[test]
fn python_corpus_preserves_negative_and_zero_content_size_messages() {
    let corpus = python_lxmf_corpus();
    let node = python_lxmf_fixture_node();

    for (name, expected_content_size, expected_payload_len) in
        [("empty_binary", -1, 15), ("one_byte_content", 0, 16)]
    {
        let fixture = corpus_message(&corpus, name);
        assert_eq!(fixture.selection_content_size, expected_content_size);
        let destination = fixture_hash(&fixture.destination_hash_hex);
        let expected_wire = hex::decode(&fixture.full_wire_hex).unwrap();
        assert_eq!(
            expected_wire.len() - LXMF_WIRE_PREFIX_LENGTH,
            expected_payload_len
        );

        let mut carrier = [0xa4_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let prepared = node
            .prepare_basic_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &hex::decode(&fixture.decoded.title_hex).unwrap(),
                &hex::decode(&fixture.decoded.content_hex).unwrap(),
                &mut carrier,
            )
            .expect("Python-compatible small payload prepares");
        let carrier_len = usize::from(prepared.carrier_len());
        assert_eq!(
            &carrier[..carrier_len],
            &expected_wire[LXMF_DESTINATION_HASH_LENGTH..]
        );
        assert!(carrier[carrier_len..].iter().all(|byte| *byte == 0xa4));
        let expected_message_id: [u8; 32] = hex::decode(&fixture.message_id_hex)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(prepared.message_id(), expected_message_id);
    }
}

#[test]
fn python_corpus_proves_the_exact_295_content_size_boundary() {
    let corpus = python_lxmf_corpus();
    let at_limit = corpus_message(&corpus, "opportunistic_limit_295");
    let over_limit = corpus_message(&corpus, "opportunistic_over_296");
    assert_eq!(
        at_limit.selection_content_size,
        MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE as i64
    );
    assert_eq!(
        over_limit.selection_content_size,
        MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE as i64 + 1
    );

    let node = python_lxmf_fixture_node();
    let destination = fixture_hash(&at_limit.destination_hash_hex);
    let title = hex::decode(&at_limit.decoded.title_hex).unwrap();
    let content = hex::decode(&at_limit.decoded.content_hex).unwrap();
    let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let prepared = node
        .prepare_basic_lxmf_into(
            &destination,
            corpus_timestamp_ms(at_limit),
            &title,
            &content,
            &mut carrier,
        )
        .expect("Python's exact opportunistic boundary prepares");

    let expected_wire = hex::decode(&at_limit.full_wire_hex).unwrap();
    assert_eq!(usize::from(prepared.carrier_len()), carrier.len());
    assert_eq!(
        expected_wire.len(),
        carrier.len() + destination.as_ref().len()
    );
    assert_eq!(&carrier[..], &expected_wire[LXMF_DESTINATION_HASH_LENGTH..]);
    assert_eq!(
        &carrier[..LXMF_DESTINATION_HASH_LENGTH],
        fixture_hash(&at_limit.source_hash_hex).as_ref()
    );
    let expected_message_id: [u8; 32] = hex::decode(&at_limit.message_id_hex)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(prepared.message_id(), expected_message_id);

    let over_title = hex::decode(&over_limit.decoded.title_hex).unwrap();
    let over_content = hex::decode(&over_limit.decoded.content_hex).unwrap();
    let mut untouched = [0xa7_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    assert_eq!(
        node.prepare_basic_lxmf_into(
            &fixture_hash(&over_limit.destination_hash_hex),
            corpus_timestamp_ms(over_limit),
            &over_title,
            &over_content,
            &mut untouched,
        ),
        Err(PrepareBasicLxmfError::PayloadTooLarge {
            actual: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE + 1,
            maximum: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE,
        })
    );
    assert!(untouched.iter().all(|byte| *byte == 0xa7));
}

#[test]
fn basic_lxmf_requires_the_registered_local_delivery_source() {
    let node = python_lxmf_fixture_node_without_delivery();
    let destination = fixture_hash("021e68345db8a80c29d0c2f193baa5f4");
    let mut output = [0x4d_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];

    assert_eq!(
        node.prepare_basic_lxmf_into(
            &destination,
            1_700_000_000_000,
            b"title",
            b"content",
            &mut output,
        ),
        Err(PrepareBasicLxmfError::DeliveryDestinationUnavailable)
    );
    assert!(output.iter().all(|byte| *byte == 0x4d));
}

#[test]
fn basic_lxmf_timestamp_subset_is_bounded_and_millisecond_distinct() {
    let node = python_lxmf_fixture_node();
    let destination = fixture_hash("021e68345db8a80c29d0c2f193baa5f4");
    let mut lower_output = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let mut upper_output = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];

    let lower = node
        .prepare_basic_lxmf_into(
            &destination,
            MAX_LXMF_TIMESTAMP_UNIX_MS - 1,
            b"",
            b"timestamp",
            &mut lower_output,
        )
        .expect("penultimate supported millisecond prepares");
    let upper = node
        .prepare_basic_lxmf_into(
            &destination,
            MAX_LXMF_TIMESTAMP_UNIX_MS,
            b"",
            b"timestamp",
            &mut upper_output,
        )
        .expect("maximum supported millisecond prepares");
    assert_ne!(lower.message_id(), upper.message_id());

    let mut untouched = [0xc1_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    assert_eq!(
        node.prepare_basic_lxmf_into(
            &destination,
            MAX_LXMF_TIMESTAMP_UNIX_MS + 1,
            b"",
            b"timestamp",
            &mut untouched,
        ),
        Err(PrepareBasicLxmfError::TimestampTooLarge {
            actual: MAX_LXMF_TIMESTAMP_UNIX_MS + 1,
            maximum: MAX_LXMF_TIMESTAMP_UNIX_MS,
        })
    );
    assert!(untouched.iter().all(|byte| *byte == 0xc1));
}

#[test]
fn maximum_opportunistic_carrier_uses_the_narrow_header1_data_path() {
    let corpus = python_lxmf_corpus();
    let fixture = corpus_message(&corpus, "opportunistic_limit_295");
    let mut sender = python_lxmf_fixture_node();
    let mut destination_private_key = [0_u8; 64];
    destination_private_key[..32].fill(0x07);
    destination_private_key[32..].fill(0x08);
    let recipient = Identity::from_private_key(&destination_private_key).unwrap();
    assert_eq!(
        hex::encode(recipient.public_key()),
        corpus.fixture_identity.destination_public_key_hex
    );
    let destination = fixture_hash(&fixture.destination_hash_hex);
    sender
        .register_peer(
            &recipient,
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            1,
        )
        .unwrap();

    let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let prepared_lxmf = sender
        .prepare_basic_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &hex::decode(&fixture.decoded.title_hex).unwrap(),
            &hex::decode(&fixture.decoded.content_hex).unwrap(),
            &mut carrier,
        )
        .unwrap();
    assert_eq!(
        usize::from(prepared_lxmf.carrier_len()),
        MAX_OPPORTUNISTIC_LXMF_CARRIER
    );

    let mut generic_rng = CounterRng::default();
    let mut generic_output = [0x65_u8; RNS_MTU];
    assert_eq!(
        sender.prepare_data_into(
            &destination,
            &carrier,
            2,
            &mut generic_rng,
            &mut generic_output,
        ),
        Err(PrepareDataError::PayloadTooLarge {
            actual: MAX_OPPORTUNISTIC_LXMF_CARRIER,
            maximum: MAX_DATA_PAYLOAD,
        })
    );
    assert_eq!(generic_rng.0, 0);
    assert!(generic_output.iter().all(|byte| *byte == 0x65));

    let mut rng = CounterRng::default();
    let mut packet_bytes = [0_u8; RNS_MTU];
    let prepared_data = sender
        .prepare_opportunistic_lxmf_data_into(
            prepared_lxmf,
            &carrier,
            2,
            &mut rng,
            &mut packet_bytes,
        )
        .expect("391-byte opportunistic carrier fits direct DATA");
    assert_eq!(usize::from(prepared_data.packet_len()), RNS_MTU - 1);
    let packet = Packet::parse(&packet_bytes[..usize::from(prepared_data.packet_len())]).unwrap();
    assert_eq!(packet.header_type, HeaderType::Header1);
    assert_eq!(packet.packet_type, PacketType::Data);
    assert_eq!(packet.destination_hash, destination.as_ref());
    // Rete's token decryptor writes the padded AES body before returning
    // the shorter unpadded length.
    let mut decrypted = [0_u8; RNS_MTU];
    let decrypted_len = recipient.decrypt(packet.payload, &mut decrypted).unwrap();
    assert_eq!(decrypted_len, carrier.len());
    assert_eq!(&decrypted[..decrypted_len], &carrier);
}

#[test]
fn rehydrated_maximum_opportunistic_wire_uses_the_dedicated_data_path() {
    let corpus = python_lxmf_corpus();
    let fixture = corpus_message(&corpus, "opportunistic_limit_295");
    let mut sender = python_lxmf_fixture_node();
    let mut destination_private_key = [0_u8; 64];
    destination_private_key[..32].fill(0x07);
    destination_private_key[32..].fill(0x08);
    let recipient = Identity::from_private_key(&destination_private_key).unwrap();
    let destination = fixture_hash(&fixture.destination_hash_hex);
    sender
        .register_peer(
            &recipient,
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            1,
        )
        .unwrap();

    let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let prepared = sender
        .prepare_basic_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &hex::decode(&fixture.decoded.title_hex).unwrap(),
            &hex::decode(&fixture.decoded.content_hex).unwrap(),
            &mut carrier,
        )
        .unwrap();
    let mut wire =
        Vec::with_capacity(LXMF_DESTINATION_HASH_LENGTH + MAX_OPPORTUNISTIC_LXMF_CARRIER);
    wire.extend_from_slice(destination.as_ref());
    wire.extend_from_slice(&carrier[..usize::from(prepared.carrier_len())]);

    let mut rng = CounterRng::default();
    let mut packet_bytes = [0_u8; RNS_MTU];
    let prepared_data = sender
        .prepare_rehydrated_opportunistic_lxmf_data_into(&wire, 2, &mut rng, &mut packet_bytes)
        .expect("durable exact wire must retain the Header-1 LXMF exception");
    let packet = Packet::parse(&packet_bytes[..usize::from(prepared_data.packet_len())]).unwrap();
    assert_eq!(packet.header_type, HeaderType::Header1);
    assert_eq!(packet.destination_hash, destination.as_ref());
    let mut decrypted = [0_u8; RNS_MTU];
    let decrypted_len = recipient.decrypt(packet.payload, &mut decrypted).unwrap();
    assert_eq!(
        &decrypted[..decrypted_len],
        &carrier[..usize::from(prepared.carrier_len())]
    );
}

#[test]
fn rehydrated_opportunistic_wire_rejects_signature_mutation_before_state_change() {
    let corpus = python_lxmf_corpus();
    let fixture = corpus_message(&corpus, "opportunistic_limit_295");
    let (mut sender, destination) = python_lxmf_fixture_sender(fixture);
    let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let prepared = sender
        .prepare_basic_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &hex::decode(&fixture.decoded.title_hex).unwrap(),
            &hex::decode(&fixture.decoded.content_hex).unwrap(),
            &mut carrier,
        )
        .unwrap();
    let mut wire =
        Vec::with_capacity(LXMF_DESTINATION_HASH_LENGTH + MAX_OPPORTUNISTIC_LXMF_CARRIER);
    wire.extend_from_slice(destination.as_ref());
    wire.extend_from_slice(&carrier[..usize::from(prepared.carrier_len())]);
    wire[LXMF_DESTINATION_HASH_LENGTH * 2] ^= 0x01;

    let mut rng = CounterRng::default();
    let mut output = [0x83_u8; RNS_MTU];
    assert_eq!(
        sender.prepare_rehydrated_opportunistic_lxmf_data_into(&wire, 2, &mut rng, &mut output,),
        Err(PrepareOpportunisticLxmfDataError::InvalidCompleteWire)
    );
    assert_eq!(rng.0, 0);
    assert!(output.iter().all(|byte| *byte == 0x83));
    assert_eq!(sender.core.transport.receipt_count(), 0);
}

#[test]
fn opportunistic_lxmf_rejects_same_length_carrier_substitution_before_mutation() {
    let corpus = python_lxmf_corpus();
    let fixture = corpus_message(&corpus, "opportunistic_limit_295");
    let (mut sender, destination) = python_lxmf_fixture_sender(fixture);
    let title = hex::decode(&fixture.decoded.title_hex).unwrap();
    let content = hex::decode(&fixture.decoded.content_hex).unwrap();
    let mut original = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let prepared = sender
        .prepare_basic_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &title,
            &content,
            &mut original,
        )
        .unwrap();
    let mut substituted = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let substituted_prepared = sender
        .prepare_basic_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture) + 1_000,
            &title,
            &content,
            &mut substituted,
        )
        .unwrap();
    assert_eq!(prepared.carrier_len(), substituted_prepared.carrier_len());
    assert_eq!(
        &original[..LXMF_DESTINATION_HASH_LENGTH],
        &substituted[..LXMF_DESTINATION_HASH_LENGTH]
    );
    assert_ne!(original, substituted);

    let mut rng = CounterRng::default();
    let mut output = [0x6d_u8; RNS_MTU];
    assert_eq!(
        sender.prepare_opportunistic_lxmf_data_into(
            prepared,
            &substituted,
            2,
            &mut rng,
            &mut output,
        ),
        Err(PrepareOpportunisticLxmfDataError::CarrierDigestMismatch)
    );
    assert_eq!(rng.0, 0);
    assert!(output.iter().all(|byte| *byte == 0x6d));
    assert_eq!(sender.core.transport.receipt_count(), 0);
}

#[test]
fn opportunistic_lxmf_rejects_post_prefix_mutation_before_state_mutation() {
    let corpus = python_lxmf_corpus();
    let fixture = corpus_message(&corpus, "opportunistic_limit_295");
    let (mut sender, destination) = python_lxmf_fixture_sender(fixture);
    let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let prepared = sender
        .prepare_basic_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &hex::decode(&fixture.decoded.title_hex).unwrap(),
            &hex::decode(&fixture.decoded.content_hex).unwrap(),
            &mut carrier,
        )
        .unwrap();
    carrier[LXMF_DESTINATION_HASH_LENGTH] ^= 0x01;

    let mut rng = CounterRng::default();
    let mut output = [0xb6_u8; RNS_MTU];
    assert_eq!(
        sender.prepare_opportunistic_lxmf_data_into(prepared, &carrier, 3, &mut rng, &mut output,),
        Err(PrepareOpportunisticLxmfDataError::CarrierDigestMismatch)
    );
    assert_eq!(rng.0, 0);
    assert!(output.iter().all(|byte| *byte == 0xb6));
    assert_eq!(sender.core.transport.receipt_count(), 0);
}

#[test]
fn maximum_opportunistic_carrier_fails_closed_on_a_header2_route() {
    let corpus = python_lxmf_corpus();
    let fixture = corpus_message(&corpus, "opportunistic_limit_295");
    let mut sender = python_lxmf_fixture_node();
    let mut destination_private_key = [0_u8; 64];
    destination_private_key[..32].fill(0x07);
    destination_private_key[32..].fill(0x08);
    let recipient = Identity::from_private_key(&destination_private_key).unwrap();
    let destination = fixture_hash(&fixture.destination_hash_hex);
    sender
        .register_peer(
            &recipient,
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            1,
        )
        .unwrap();
    sender.core.transport.insert_path(
        destination,
        rete_transport::Path::via_repeater(identity(0xfa).hash(), 2, 2),
    );

    let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let prepared_lxmf = sender
        .prepare_basic_lxmf_into(
            &destination,
            corpus_timestamp_ms(fixture),
            &hex::decode(&fixture.decoded.title_hex).unwrap(),
            &hex::decode(&fixture.decoded.content_hex).unwrap(),
            &mut carrier,
        )
        .unwrap();
    let mut rng = CounterRng::default();
    let mut output = [0x9b_u8; RNS_MTU];
    assert_eq!(
        sender.prepare_opportunistic_lxmf_data_into(
            prepared_lxmf,
            &carrier,
            3,
            &mut rng,
            &mut output,
        ),
        Err(PrepareOpportunisticLxmfDataError::Header2PayloadTooLarge {
            actual: MAX_OPPORTUNISTIC_LXMF_CARRIER,
            maximum: MAX_DATA_PAYLOAD,
        })
    );
    assert_eq!(rng.0, 0);
    assert!(output.iter().all(|byte| *byte == 0x9b));
    assert_eq!(sender.core.transport.receipt_count(), 0);
}

#[test]
fn learned_destination_identity_is_copied_without_exposing_native_storage() {
    let peer = identity(201);
    let expected_public_key = peer.public_key();
    let peer_destination = TestNode::new(
        identity(201),
        "lxmf",
        &["delivery"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap()
    .destination_hash();
    let mut owner = node(200);

    owner
        .register_peer(&peer, "lxmf", &["delivery"], 1)
        .unwrap();

    assert_eq!(
        owner.recall_identity(&peer_destination),
        Some(expected_public_key)
    );
    assert_eq!(owner.recall_identity(&DestHash::from([0xee; 16])), None);
}

#[test]
fn learned_identity_is_classified_only_by_its_exact_lxmf_delivery_hash() {
    let delivery_peer = identity(202);
    let other_peer = identity(203);
    let delivery_public_key = delivery_peer.public_key();
    let other_public_key = other_peer.public_key();
    let mut delivery_announcer = TestNode::new(
        delivery_peer,
        LXMF_APPLICATION_NAME,
        &[LXMF_DELIVERY_ASPECT],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    let mut other_announcer = TestNode::new(
        other_peer,
        "nomadnetwork",
        &["node"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    let delivery_destination = delivery_announcer.destination_hash();
    let other_destination = other_announcer.destination_hash();
    let mut owner = node(200);
    let mut rng = CounterRng::default();

    delivery_announcer
        .queue_announce(None, 1, &mut rng)
        .unwrap();
    let delivery_announce = delivery_announcer
        .flush_announces(1, &mut rng)
        .pop()
        .unwrap();
    let learned = owner.ingest(delivery_announce.bytes(), 1, InterfaceId(4), &mut rng);
    assert!(matches!(
        learned.actions.events.as_slice(),
        [ApplicationEvent::AnnounceReceived { destination, .. }]
            if destination == delivery_destination.as_bytes()
    ));

    other_announcer.queue_announce(None, 2, &mut rng).unwrap();
    let other_announce = other_announcer.flush_announces(2, &mut rng).pop().unwrap();
    let learned = owner.ingest(other_announce.bytes(), 2, InterfaceId(4), &mut rng);
    assert!(matches!(
        learned.actions.events.as_slice(),
        [ApplicationEvent::AnnounceReceived { destination, .. }]
            if destination == other_destination.as_bytes()
    ));

    assert_eq!(
        owner.recall_lxmf_delivery_identity(&delivery_destination),
        Some(delivery_public_key)
    );
    assert_eq!(
        owner.recall_identity(&other_destination),
        Some(other_public_key)
    );
    assert_eq!(
        owner.recall_lxmf_delivery_identity(&other_destination),
        None
    );
    assert_eq!(
        owner.recall_lxmf_delivery_identity(&DestHash::from([0xee; 16])),
        None
    );
}

fn link_request(
    initiator: &mut TestNode,
    responder: &TestNode,
    rng: &mut CounterRng,
) -> (TxPacket, LinkId) {
    initiator
        .register_peer(&identity(2), "reticulum", &["embedded"], 100)
        .unwrap();
    initiator
        .initiate_link(responder.destination_hash(), 100, rng)
        .unwrap()
}

fn establish_bound_link(
    initiator: &mut TestNode,
    responder: &mut TestNode,
    rng: &mut CounterRng,
) -> LinkId {
    let (request, link_id) = link_request(initiator, responder, rng);
    let proof = responder.ingest(request.bytes(), 100, InterfaceId(7), rng);
    assert!(proof.actions.events.is_empty());
    assert_eq!(proof.actions.packets.len(), 1);
    assert_eq!(
        proof.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(7))
    );
    assert_eq!(
        Packet::parse(proof.actions.packets[0].bytes())
            .unwrap()
            .context,
        CONTEXT_LRPROOF
    );

    let established = initiator.ingest(proof.actions.packets[0].bytes(), 101, InterfaceId(3), rng);
    assert_eq!(initiator.link_state(&link_id), Some(LinkState::Active));
    assert_eq!(initiator.link_rtt(&link_id), Some(1.0));
    assert_eq!(established.actions.packets.len(), 1);
    assert_eq!(
        established.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(3))
    );
    assert_eq!(
        Packet::parse(established.actions.packets[0].bytes())
            .unwrap()
            .context,
        CONTEXT_LRRTT
    );

    let active = responder.ingest(
        established.actions.packets[0].bytes(),
        102,
        InterfaceId(7),
        rng,
    );
    assert_eq!(active.disposition, IngressDisposition::Processed);
    assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));
    assert_eq!(responder.link_rtt(&link_id), Some(2.0));
    link_id
}

#[test]
fn direct_response_is_exactly_routed_and_delivers_a_400_byte_body() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);
    let prepared = initiator
        .prepare_anonymous_request(&link_id, "/page/index.mu", 1_700_000_000.125, &mut rng)
        .unwrap();
    let request_handle = prepared.handle();
    assert_eq!(
        initiator.confirm_request_dispatch(request_handle, 103, true),
        Ok(RequestDispatchConfirmation::Confirmed)
    );
    let inbound = responder.ingest(prepared.packet().bytes(), 103, InterfaceId(7), &mut rng);
    let [
        ApplicationEvent::RequestValueReceived {
            binding, request, ..
        },
    ] = inbound.actions.events.as_slice()
    else {
        panic!("anonymous request must retain exact responder provenance")
    };
    assert_eq!(binding.link(), link_id.as_bytes());
    assert_eq!(
        binding.destination(),
        responder.destination_hash().as_bytes()
    );
    assert_eq!(binding.role(), ApplicationLinkRole::Responder);
    assert_eq!(request, request_handle.request());

    let response_body = vec![0x5a; 400];
    let entropy_before = rng.0;
    let last_outbound_before = responder
        .link_snapshot_for_conformance(&link_id)
        .unwrap()
        .last_outbound;
    let response = responder
        .prepare_response(binding, request, &response_body, 110, &mut rng)
        .unwrap();
    assert!(rng.0 > entropy_before);
    assert_eq!(response.target(), TxTarget::Only(InterfaceId(7)));
    assert_eq!(response.protocol_token(), None);

    let packet = Packet::parse(response.bytes()).unwrap();
    assert_eq!(packet.context, rete_core::CONTEXT_RESPONSE);
    assert_eq!(packet.dest_type, DestType::Link);
    assert_eq!(packet.destination_hash, link_id.as_bytes());
    let mut plaintext = [0_u8; RNS_MTU];
    let plaintext_len = initiator
        .core
        .transport
        .get_link(&link_id)
        .unwrap()
        .decrypt(packet.payload, &mut plaintext)
        .unwrap();
    assert_eq!(plaintext_len, 422);
    let (decoded_request, decoded_body) =
        rete_transport::parse_response(&plaintext[..plaintext_len]).unwrap();
    assert_eq!(decoded_request.as_bytes(), request_handle.request());
    assert_eq!(decoded_body, response_body);

    let last_outbound_after = responder
        .link_snapshot_for_conformance(&link_id)
        .unwrap()
        .last_outbound;
    assert!(last_outbound_after > last_outbound_before);
    assert_eq!(last_outbound_after, MonotonicInstant::from_secs(110));

    let received = initiator.ingest(response.bytes(), 110, InterfaceId(3), &mut rng);
    assert!(matches!(
        received.actions.events.as_slice(),
        [ApplicationEvent::ResponseReceived {
            link,
            request,
            data,
        }] if link == request_handle.link()
            && request == request_handle.request()
            && data == &response_body
    ));
    assert_eq!(initiator.request_dispatch_count(), 0);
}

#[test]
fn direct_response_preflight_accounts_for_the_bin8_bin16_boundary() {
    assert_eq!(direct_response_body_maximum(20), None);
    assert_eq!(direct_response_body_maximum(21), Some(0));
    assert_eq!(direct_response_body_maximum(276), Some(255));
    assert_eq!(direct_response_body_maximum(277), Some(255));
    assert_eq!(direct_response_body_maximum(278), Some(256));
    assert_eq!(
        direct_response_body_maximum(rete_transport::LINK_MDU),
        Some(409)
    );

    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);
    let prepared = initiator
        .prepare_anonymous_request(&link_id, "/page/index.mu", 100.0, &mut rng)
        .unwrap();
    let inbound = responder.ingest(prepared.packet().bytes(), 103, InterfaceId(7), &mut rng);
    let [
        ApplicationEvent::RequestValueReceived {
            binding, request, ..
        },
    ] = inbound.actions.events.as_slice()
    else {
        panic!("anonymous request must retain exact responder provenance")
    };

    for (body_len, packed_len) in [(255, 276), (256, 278)] {
        let response = responder
            .prepare_response(binding, request, &vec![0xa5; body_len], 110, &mut rng)
            .unwrap();
        let packet = Packet::parse(response.bytes()).unwrap();
        let mut plaintext = [0_u8; RNS_MTU];
        let plaintext_len = initiator
            .core
            .transport
            .get_link(&link_id)
            .unwrap()
            .decrypt(packet.payload, &mut plaintext)
            .unwrap();
        assert_eq!(plaintext_len, packed_len);
        let (_, decoded_body) =
            rete_transport::parse_response(&plaintext[..plaintext_len]).unwrap();
        assert_eq!(decoded_body.len(), body_len);
    }
}

#[test]
fn direct_response_rejects_invalid_retained_state_before_entropy_or_timing_mutation() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);
    let prepared = initiator
        .prepare_anonymous_request(&link_id, "/page/index.mu", 100.0, &mut rng)
        .unwrap();
    let inbound = responder.ingest(prepared.packet().bytes(), 103, InterfaceId(7), &mut rng);
    let [
        ApplicationEvent::RequestValueReceived {
            binding, request, ..
        },
    ] = inbound.actions.events.as_slice()
    else {
        panic!("anonymous request must retain exact responder provenance")
    };

    let assert_unchanged =
        |node: &TestNode, entropy: u8, last_outbound: MonotonicInstant, rng: &CounterRng| {
            assert_eq!(rng.0, entropy);
            assert_eq!(
                node.link_snapshot_for_conformance(&link_id)
                    .unwrap()
                    .last_outbound,
                last_outbound
            );
        };

    let wrong_destination =
        test_link_binding(*binding.link(), [0x5c; TRUNCATED_HASH_LEN], binding.role());
    let entropy_before = rng.0;
    let last_outbound_before = responder
        .link_snapshot_for_conformance(&link_id)
        .unwrap()
        .last_outbound;
    assert_eq!(
        responder.prepare_response(
            &wrong_destination,
            request,
            b"must not encrypt",
            110,
            &mut rng,
        ),
        Err(PrepareResponseError::LinkBindingMismatch)
    );
    assert_unchanged(&responder, entropy_before, last_outbound_before, &rng);

    let wrong_role = test_link_binding(
        *binding.link(),
        *binding.destination(),
        ApplicationLinkRole::Initiator,
    );
    let entropy_before = rng.0;
    let last_outbound_before = responder
        .link_snapshot_for_conformance(&link_id)
        .unwrap()
        .last_outbound;
    assert_eq!(
        responder.prepare_response(&wrong_role, request, b"must not encrypt", 111, &mut rng,),
        Err(PrepareResponseError::LinkBindingMismatch)
    );
    assert_unchanged(&responder, entropy_before, last_outbound_before, &rng);

    responder
        .core
        .transport
        .get_link_mut(&link_id)
        .unwrap()
        .state = LinkState::Stale;
    let entropy_before = rng.0;
    let last_outbound_before = responder
        .link_snapshot_for_conformance(&link_id)
        .unwrap()
        .last_outbound;
    assert_eq!(
        responder.prepare_response(binding, request, b"must not encrypt", 112, &mut rng),
        Err(PrepareResponseError::LinkNotActive)
    );
    assert_unchanged(&responder, entropy_before, last_outbound_before, &rng);

    let link = responder.core.transport.get_link_mut(&link_id).unwrap();
    link.state = LinkState::Active;
    link.signalling = rete_transport::signalling_bytes(300, 1);
    let negotiated_mdu = rete_transport::compute_link_mdu(300);
    let maximum = direct_response_body_maximum(negotiated_mdu).unwrap();
    let oversized = vec![0x33; maximum + 1];
    let entropy_before = rng.0;
    let last_outbound_before = responder
        .link_snapshot_for_conformance(&link_id)
        .unwrap()
        .last_outbound;
    assert_eq!(
        responder.prepare_response(binding, request, &oversized, 113, &mut rng),
        Err(PrepareResponseError::ResponseTooLarge {
            actual: maximum + 1,
            maximum,
        })
    );
    assert_unchanged(&responder, entropy_before, last_outbound_before, &rng);

    let default_oversized = vec![0x44; 410];
    responder
        .core
        .transport
        .get_link_mut(&link_id)
        .unwrap()
        .signalling = [0; rete_transport::LINK_MTU_SIZE];
    let entropy_before = rng.0;
    let last_outbound_before = responder
        .link_snapshot_for_conformance(&link_id)
        .unwrap()
        .last_outbound;
    assert_eq!(
        responder.prepare_response(binding, request, &default_oversized, 114, &mut rng),
        Err(PrepareResponseError::ResponseTooLarge {
            actual: 410,
            maximum: 409,
        })
    );
    assert_unchanged(&responder, entropy_before, last_outbound_before, &rng);

    let _ = responder.close_link(&link_id, &mut rng);
    let entropy_before = rng.0;
    assert_eq!(
        responder.prepare_response(binding, request, b"must not encrypt", 115, &mut rng),
        Err(PrepareResponseError::LinkNotFound)
    );
    assert_eq!(rng.0, entropy_before);
}

#[test]
fn anonymous_request_is_canonical_exact_routed_and_times_out_only_after_dispatch() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);
    let requested_at = 1_700_000_000.125_f64;

    let prepared = initiator
        .prepare_anonymous_request(&link_id, "/page/index.mu", requested_at, &mut rng)
        .unwrap();
    let handle = prepared.handle();
    assert_eq!(handle.link(), link_id.as_bytes());
    assert_eq!(prepared.packet().target(), TxTarget::Only(InterfaceId(3)));
    assert_eq!(prepared.packet().protocol_token(), None);
    assert_eq!(initiator.request_dispatch_count(), 1);

    let packet = Packet::parse(prepared.packet().bytes()).unwrap();
    assert_eq!(packet.context, rete_core::CONTEXT_REQUEST);
    assert_eq!(packet.dest_type, DestType::Link);
    assert_eq!(packet.destination_hash, link_id.as_bytes());
    assert_eq!(
        handle.request(),
        &packet.compute_hash()[..TRUNCATED_HASH_LEN]
    );
    let mut plaintext = [0_u8; RNS_MTU];
    let plaintext_len = responder
        .core
        .transport
        .get_link(&link_id)
        .unwrap()
        .decrypt(packet.payload, &mut plaintext)
        .unwrap();
    let (wire_time, path_hash, value) =
        rete_transport::parse_request_value(&plaintext[..plaintext_len]).unwrap();
    assert_eq!(wire_time.to_bits(), requested_at.to_bits());
    assert_eq!(path_hash, rete_transport::path_hash("/page/index.mu"));
    assert_eq!(value, [0xc0]);

    let before_dispatch = initiator.tick_at(10_000, MonotonicInstant::from_secs(103), &mut rng);
    assert!(
        !before_dispatch
            .events
            .as_slice()
            .iter()
            .any(|event| matches!(
                event,
                ApplicationEvent::RequestFailed { request, .. }
                    if request == handle.request()
            ))
    );
    assert_eq!(initiator.request_dispatch_count(), 1);

    assert_eq!(
        initiator.confirm_request_dispatch(handle, 20_000, true),
        Ok(RequestDispatchConfirmation::Confirmed)
    );
    let timed_out = initiator.tick_at(u64::MAX - 1, MonotonicInstant::from_secs(103), &mut rng);
    assert!(timed_out.events.as_slice().iter().any(|event| matches!(
        event,
        ApplicationEvent::RequestFailed {
            link,
            request,
            reason: ApplicationRequestFailReason::Timeout,
        } if link == handle.link() && request == handle.request()
    )));
    assert_eq!(initiator.request_dispatch_count(), 0);
}

#[test]
fn request_dispatch_confirmation_cancel_and_response_cleanup_are_exact() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

    let response_request = initiator
        .prepare_anonymous_request(&link_id, "/page/index.mu", 100.0, &mut rng)
        .unwrap();
    let response_handle = response_request.handle();
    assert_eq!(
        initiator.confirm_request_dispatch(response_handle, 110, false),
        Ok(RequestDispatchConfirmation::NotFirstDispatch)
    );
    assert_eq!(
        initiator.cancel_confirmed_request(response_handle),
        Err(RequestDispatchError::NotConfirmed)
    );
    assert_eq!(
        initiator.confirm_request_dispatch(response_handle, 110, true),
        Ok(RequestDispatchConfirmation::Confirmed)
    );
    assert_eq!(
        initiator.confirm_request_dispatch(response_handle, 111, false),
        Ok(RequestDispatchConfirmation::NotFirstDispatch)
    );
    assert_eq!(
        initiator.confirm_request_dispatch(response_handle, 111, true),
        Err(RequestDispatchError::NotPrepared)
    );
    assert_eq!(
        initiator.cancel_prepared_request(response_handle),
        Err(RequestDispatchError::NotPrepared)
    );

    let request_id = rete_core::RequestId::from(*response_handle.request());
    let response = responder
        .core
        .send_response(&link_id, &request_id, b"hello micron", &mut rng)
        .unwrap();
    let response = resolve_origin_packet(response);
    let received = initiator.ingest(response.bytes(), 112, InterfaceId(3), &mut rng);
    assert!(
        received
            .actions
            .events
            .as_slice()
            .iter()
            .any(|event| matches!(
                event,
                ApplicationEvent::ResponseReceived {
                    link,
                    request,
                    data,
                } if link == response_handle.link()
                    && request == response_handle.request()
                    && data == b"hello micron"
            ))
    );
    assert_eq!(initiator.request_dispatch_count(), 0);
    assert_eq!(
        initiator.cancel_request(response_handle),
        Err(RequestDispatchError::NotTracked)
    );
    assert_eq!(
        initiator.confirm_request_dispatch(response_handle, 113, false),
        Err(RequestDispatchError::NotTracked)
    );

    let prepared = initiator
        .prepare_anonymous_request(&link_id, "/page/prepared.mu", 120.0, &mut rng)
        .unwrap();
    assert_eq!(
        initiator.cancel_request(prepared.handle()),
        Ok(CanceledRequestDispatch::Prepared)
    );
    assert_eq!(initiator.request_dispatch_count(), 0);

    let confirmed = initiator
        .prepare_anonymous_request(&link_id, "/page/confirmed.mu", 121.0, &mut rng)
        .unwrap();
    assert_eq!(
        initiator.confirm_request_dispatch(confirmed.handle(), 122, true),
        Ok(RequestDispatchConfirmation::Confirmed)
    );
    assert_eq!(
        initiator.cancel_request(confirmed.handle()),
        Ok(CanceledRequestDispatch::Confirmed)
    );
    assert_eq!(initiator.request_dispatch_count(), 0);
}

#[test]
fn request_dispatch_reconciliation_is_exact_idempotent_and_phase_agnostic() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

    let first = initiator
        .prepare_anonymous_request(&link_id, "/page/first.mu", 100.0, &mut rng)
        .unwrap()
        .handle();
    let sibling = initiator
        .prepare_anonymous_request(&link_id, "/page/sibling.mu", 101.0, &mut rng)
        .unwrap()
        .handle();
    assert_eq!(
        initiator.reconcile_request_dispatch(first),
        RequestDispatchReconciliation::ReclaimedPrepared
    );
    assert_eq!(
        initiator.reconcile_request_dispatch(first),
        RequestDispatchReconciliation::Absent
    );
    assert_eq!(initiator.request_dispatch_count(), 1);
    assert_eq!(
        initiator.cancel_request(sibling),
        Ok(CanceledRequestDispatch::Prepared)
    );

    let confirmed = initiator
        .prepare_anonymous_request(&link_id, "/page/confirmed.mu", 102.0, &mut rng)
        .unwrap()
        .handle();
    assert_eq!(
        initiator.confirm_request_dispatch(confirmed, 103, true),
        Ok(RequestDispatchConfirmation::Confirmed)
    );
    assert_eq!(
        initiator.reconcile_request_dispatch(confirmed),
        RequestDispatchReconciliation::ReclaimedConfirmed
    );

    let adapter_only = initiator
        .prepare_anonymous_request(&link_id, "/page/adapter-only.mu", 104.0, &mut rng)
        .unwrap()
        .handle();
    let request = rete_core::RequestId::from(*adapter_only.request());
    assert_eq!(
        initiator.core.reclaim_request_dispatch(&request, &link_id),
        Some(rete_stack::RequestStatus::Prepared)
    );
    assert_eq!(
        initiator.reconcile_request_dispatch(adapter_only),
        RequestDispatchReconciliation::ReclaimedInconsistent
    );

    let native_only = initiator
        .prepare_anonymous_request(&link_id, "/page/native-only.mu", 105.0, &mut rng)
        .unwrap()
        .handle();
    let index = initiator
        .request_dispatches
        .iter()
        .position(|entry| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.handle == native_only)
        })
        .unwrap();
    let entry = initiator.request_dispatches[index].take().unwrap();
    let Some(NativeRequestDispatchAuthority::Prepared(confirmation)) = entry.authority else {
        panic!("fresh request must retain prepared authority");
    };
    let confirmed = initiator
        .core
        .confirm_prepared_request(confirmation, 106)
        .unwrap();
    let _ = confirmed.dispatched();
    assert_eq!(
        initiator.reconcile_request_dispatch(native_only),
        RequestDispatchReconciliation::ReclaimedInconsistent
    );
    assert_eq!(
        initiator.reconcile_request_dispatch(native_only),
        RequestDispatchReconciliation::Absent
    );
    assert_eq!(initiator.request_dispatch_count(), 0);
    assert_eq!(
        initiator
            .core
            .get_request_status(&rete_core::RequestId::from(*native_only.request())),
        None
    );
}

#[test]
fn request_preflight_bounds_storage_and_local_link_close_rolls_back() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);
    let maximum = initiator.core.transport.get_link(&link_id).unwrap().mdu();

    let entropy_before_invalid = rng.0;
    assert_eq!(
        initiator.prepare_direct_request_value(
            &link_id,
            "/page/form.mu",
            Some(&[0xc0, 0xc0]),
            100.0,
            &mut rng,
        ),
        Err(PrepareDirectRequestError::InvalidRequestValue)
    );
    assert_eq!(rng.0, entropy_before_invalid);
    assert_eq!(initiator.request_dispatch_count(), 0);

    let oversized = vec![0xc0; maximum - SINGLE_PACKET_REQUEST_OVERHEAD + 1];
    let entropy_before_oversized = rng.0;
    assert_eq!(
        initiator.prepare_direct_request_value(
            &link_id,
            "/page/form.mu",
            Some(&oversized),
            100.0,
            &mut rng,
        ),
        Err(PrepareDirectRequestError::RequestTooLarge {
            actual: maximum + 1,
            maximum,
        })
    );
    assert_eq!(rng.0, entropy_before_oversized);
    assert_eq!(initiator.request_dispatch_count(), 0);

    let mut handles = Vec::new();
    for _ in 0..4 {
        handles.push(
            initiator
                .prepare_anonymous_request(&link_id, "/page/index.mu", 101.0, &mut rng)
                .unwrap()
                .handle(),
        );
    }
    let entropy_before_full = rng.0;
    assert_eq!(
        initiator.prepare_anonymous_request(&link_id, "/page/index.mu", 101.0, &mut rng,),
        Err(PrepareDirectRequestError::DispatchTableFull { limit: 4 })
    );
    assert_eq!(rng.0, entropy_before_full);
    assert_eq!(initiator.request_dispatch_count(), 4);

    let closing_handle = handles[0];
    let closed = initiator.close_link(&link_id, &mut rng);
    assert!(closed.events.as_slice().iter().any(|event| matches!(
        event,
        ApplicationEvent::LinkClosed { link } if link == closing_handle.link()
    )));
    assert_eq!(initiator.request_dispatch_count(), 0);
    for handle in handles {
        assert_eq!(
            initiator.cancel_request(handle),
            Err(RequestDispatchError::NotTracked)
        );
    }
}

#[test]
fn local_close_fails_before_link_removal_when_request_cannot_cancel() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);
    let prepared = initiator
        .prepare_anonymous_request(&link_id, "/page/index.mu", 100.0, &mut rng)
        .unwrap();
    let handle = prepared.handle();
    assert_eq!(
        initiator.confirm_request_dispatch(handle, 110, true),
        Ok(RequestDispatchConfirmation::Confirmed)
    );

    // Bypass the owning adapter to simulate native state becoming terminal
    // without its exact projected event reclaiming the retained control.
    let request_id = rete_core::RequestId::from(*handle.request());
    let response = responder
        .core
        .send_response(&link_id, &request_id, b"terminal", &mut rng)
        .unwrap();
    let terminal = initiator
        .core
        .handle_ingest(&response.data, 111, 3, &mut rng);
    assert!(matches!(
        terminal.events.as_slice(),
        [NativeNodeEvent::ResponseReceived {
            link_id: event_link,
            request_id: event_request,
            data,
        }] if *event_link == link_id
            && event_request.as_bytes() == handle.request()
            && data == b"terminal"
    ));
    assert_eq!(initiator.core.get_request_status(&request_id), None);
    assert_eq!(initiator.request_dispatch_count(), 1);

    let close_panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = initiator.close_link(&link_id, &mut rng);
    }));
    assert!(close_panicked.is_err());
    assert_eq!(initiator.link_state(&link_id), Some(LinkState::Active));
    assert_eq!(initiator.request_dispatch_count(), 1);

    initiator.reclaim_native_request_terminals(&terminal.events);
    assert_eq!(initiator.request_dispatch_count(), 0);
    let closed = initiator.close_link(&link_id, &mut rng);
    assert!(matches!(
        closed.events.as_slice(),
        [ApplicationEvent::LinkClosed { link }] if link == link_id.as_bytes()
    ));
    assert_eq!(initiator.link_state(&link_id), None);
}

#[test]
fn authenticated_malformed_lrrtt_closes_once_on_bound_interface() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
    let proof = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
    assert_eq!(responder.link_state(&link_id), Some(LinkState::Handshake));
    assert!(proof.actions.events.is_empty());
    assert_eq!(proof.actions.packets.len(), 1);

    // Validating LRPROOF gives the initiator the negotiated session key.
    // Build nil as an encrypted LRRTT plaintext through that key, while
    // deliberately discarding the automatically generated valid LRRTT.
    let established = initiator.ingest(
        proof.actions.packets[0].bytes(),
        101,
        InterfaceId(3),
        &mut rng,
    );
    assert!(matches!(
        established.actions.events.as_slice(),
        [ApplicationEvent::LinkEstablished { link }] if *link == *link_id.as_bytes()
    ));
    let malformed = initiator
        .core
        .transport
        .build_lrrtt_packet(&link_id, &[0xc0], &mut rng)
        .unwrap();

    let counters_before = responder.metrics().transport;
    let closed = responder.ingest(&malformed, 102, InterfaceId(7), &mut rng);
    assert_eq!(closed.disposition, IngressDisposition::NativeInvalid);
    assert!(matches!(
        closed.actions.events.as_slice(),
        [ApplicationEvent::LinkClosed { link }] if *link == *link_id.as_bytes()
    ));
    assert_eq!(closed.actions.packets.len(), 1);
    assert_eq!(
        closed.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(7))
    );
    let linkclose = Packet::parse(closed.actions.packets[0].bytes()).unwrap();
    assert_eq!(linkclose.packet_type, PacketType::Data);
    assert_eq!(linkclose.dest_type, DestType::Link);
    assert_eq!(linkclose.destination_hash, link_id.as_ref());
    assert_eq!(linkclose.context, CONTEXT_LINKCLOSE);
    assert_eq!(responder.link_state(&link_id), None);
    assert_eq!(responder.link_rtt(&link_id), None);
    let counters_after = responder.metrics().transport;
    assert_eq!(
        counters_after.packets_dropped_invalid,
        counters_before.packets_dropped_invalid + 1
    );
    assert_eq!(
        counters_after.links_failed,
        counters_before.links_failed + 1
    );
    assert_eq!(
        counters_after.links_closed,
        counters_before.links_closed + 1
    );
    assert_eq!(
        counters_after.links_established,
        counters_before.links_established
    );

    // The close packet is authenticated with the retained Link key and is
    // accepted by its peer, while replaying malformed LRRTT cannot emit a
    // second close or lifecycle event after responder state was purged.
    let peer_closed = initiator.ingest(
        closed.actions.packets[0].bytes(),
        103,
        InterfaceId(3),
        &mut rng,
    );
    assert!(matches!(
        peer_closed.actions.events.as_slice(),
        [ApplicationEvent::LinkClosed { link }] if *link == *link_id.as_bytes()
    ));
    assert_eq!(initiator.link_state(&link_id), None);

    let replayed = responder.ingest(&malformed, 104, InterfaceId(7), &mut rng);
    assert!(replayed.actions.events.is_empty());
    assert!(replayed.actions.packets.is_empty());
    assert_eq!(responder.link_state(&link_id), None);
    let counters_replayed = responder.metrics().transport;
    assert_eq!(
        counters_replayed.packets_dropped_invalid,
        counters_after.packets_dropped_invalid
    );
    assert_eq!(counters_replayed.links_failed, counters_after.links_failed);
    assert_eq!(counters_replayed.links_closed, counters_after.links_closed);
    assert_eq!(
        counters_replayed.links_established,
        counters_after.links_established
    );
}

#[test]
fn wrapper_rejects_wrong_hop_lrproof_before_dedup_and_accepts_correct_copy() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
    let proof = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
    assert_eq!(proof.actions.packets.len(), 1);
    let proof_bytes = proof.actions.packets[0].bytes();
    assert_eq!(Packet::parse(proof_bytes).unwrap().hops, 0);

    // A registered direct path expects one hop after local ingress. The
    // modified wire byte produces two, but does not alter the proof hash.
    let mut wrong_hops = proof_bytes.to_vec();
    wrong_hops[1] = 1;
    let dedup_before = initiator.metrics().transport.packets_dropped_dedup;
    let rejected = initiator.ingest(&wrong_hops, 101, InterfaceId(3), &mut rng);
    assert_eq!(rejected.disposition, IngressDisposition::NativeInvalid);
    assert!(rejected.actions.events.is_empty());
    assert!(rejected.actions.packets.is_empty());
    assert_eq!(rejected.actions.unroutable_packets, 0);
    assert_eq!(initiator.link_state(&link_id), Some(LinkState::Handshake));
    assert_eq!(
        initiator.metrics().transport.packets_dropped_dedup,
        dedup_before
    );

    let established = initiator.ingest(proof_bytes, 102, InterfaceId(3), &mut rng);
    assert_eq!(established.disposition, IngressDisposition::Processed);
    assert_eq!(initiator.link_state(&link_id), Some(LinkState::Active));
    assert_eq!(established.actions.packets.len(), 1);
    assert_eq!(
        established.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(3))
    );
    assert_eq!(
        Packet::parse(established.actions.packets[0].bytes())
            .unwrap()
            .context,
        CONTEXT_LRRTT
    );
}

#[test]
fn wrapper_keepalive_roundtrip_is_exact_internal_repeatable_and_bound() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

    // LRPROOF takes one whole monotonic second in this fixture. The
    // compatibility tick supplies whole seconds, so round the precise
    // RTT-derived interval upward to the first representable due instant.
    let initiator_keepalive_micros = initiator
        .link_snapshot_for_conformance(&link_id)
        .unwrap()
        .keepalive_interval
        .as_micros();
    let initiator_keepalive = initiator_keepalive_micros.saturating_add(999_999) / 1_000_000;
    let request_at = 101 + initiator_keepalive;
    let tick = initiator.tick(request_at, &mut rng);
    let requests: Vec<_> = tick
        .packets
        .iter()
        .filter(|packet| {
            Packet::parse(packet.bytes()).is_ok_and(|parsed| parsed.context == CONTEXT_KEEPALIVE)
        })
        .collect();
    assert_eq!(requests.len(), 1);
    let request = requests[0];
    assert_eq!(request.target(), TxTarget::Only(InterfaceId(3)));
    assert_eq!(request.bytes().len(), 20);
    let parsed_request = Packet::parse(request.bytes()).unwrap();
    assert_eq!(parsed_request.packet_type, PacketType::Data);
    assert_eq!(parsed_request.dest_type, DestType::Link);
    assert_eq!(parsed_request.destination_hash, link_id.as_ref());
    assert_eq!(parsed_request.payload, &[0xFF]);
    assert!(matches!(
        tick.events.as_slice(),
        [ApplicationEvent::Tick {
            closed_links: 0,
            ..
        }]
    ));

    let responder_dedup = responder.metrics().transport.packets_dropped_dedup;
    let response = responder.ingest(request.bytes(), request_at, InterfaceId(7), &mut rng);
    assert_eq!(response.disposition, IngressDisposition::Processed);
    assert!(response.actions.events.is_empty());
    assert_eq!(response.actions.packets.len(), 1);
    assert_eq!(
        response.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(7))
    );
    assert_eq!(response.actions.packets[0].bytes().len(), 20);
    let parsed_response = Packet::parse(response.actions.packets[0].bytes()).unwrap();
    assert_eq!(parsed_response.packet_type, PacketType::Data);
    assert_eq!(parsed_response.dest_type, DestType::Link);
    assert_eq!(parsed_response.destination_hash, link_id.as_ref());
    assert_eq!(parsed_response.payload, &[0xFE]);
    assert_eq!(
        responder.metrics().transport.packets_dropped_dedup,
        responder_dedup
    );

    let initiator_dedup = initiator.metrics().transport.packets_dropped_dedup;
    let consumed = initiator.ingest(
        response.actions.packets[0].bytes(),
        request_at + 1,
        InterfaceId(3),
        &mut rng,
    );
    assert_eq!(
        consumed.disposition,
        IngressDisposition::NoObservableOutcome
    );
    assert!(consumed.actions.events.is_empty());
    assert!(consumed.actions.packets.is_empty());
    assert_eq!(
        initiator.metrics().transport.packets_dropped_dedup,
        initiator_dedup
    );

    // Replaying the identical deterministic FF and FE remains valid Link
    // lifecycle traffic instead of falling into packet deduplication.
    let repeated_response =
        responder.ingest(request.bytes(), request_at + 2, InterfaceId(7), &mut rng);
    assert_eq!(repeated_response.disposition, IngressDisposition::Processed);
    assert!(repeated_response.actions.events.is_empty());
    assert_eq!(repeated_response.actions.packets.len(), 1);
    assert_eq!(
        repeated_response.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(7))
    );
    assert_eq!(
        repeated_response.actions.packets[0].bytes(),
        response.actions.packets[0].bytes()
    );
    assert_eq!(
        responder.metrics().transport.packets_dropped_dedup,
        responder_dedup
    );

    let repeated_consumed = initiator.ingest(
        repeated_response.actions.packets[0].bytes(),
        request_at + 3,
        InterfaceId(3),
        &mut rng,
    );
    assert_eq!(
        repeated_consumed.disposition,
        IngressDisposition::NoObservableOutcome
    );
    assert!(repeated_consumed.actions.events.is_empty());
    assert!(repeated_consumed.actions.packets.is_empty());
    assert_eq!(
        initiator.metrics().transport.packets_dropped_dedup,
        initiator_dedup
    );

    // The responder's RTT is two seconds in this fixture. Even after a
    // complete interval of inbound silence, it must never originate FF.
    let responder_keepalive_micros = responder
        .link_snapshot_for_conformance(&link_id)
        .unwrap()
        .keepalive_interval
        .as_micros();
    let responder_keepalive = responder_keepalive_micros.saturating_add(999_999) / 1_000_000;
    let responder_tick = responder.tick(request_at + 2 + responder_keepalive, &mut rng);
    assert!(responder_tick.packets.iter().all(|packet| {
        Packet::parse(packet.bytes()).is_ok_and(|parsed| parsed.context != CONTEXT_KEEPALIVE)
    }));
    assert!(matches!(
        responder_tick.events.as_slice(),
        [ApplicationEvent::Tick {
            closed_links: 0,
            ..
        }]
    ));
    assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));
}

#[test]
fn wrapper_resolves_source_relative_routing_and_completes_link_lifecycle() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();

    let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
    assert_eq!(request.target, TxTarget::All);

    let response = responder.ingest(&request.bytes, 100, InterfaceId(7), &mut rng);
    assert_eq!(response.disposition, IngressDisposition::Processed);
    assert_eq!(
        response.metadata.wire_packet_type(),
        Some(PacketType::LinkRequest)
    );
    assert_eq!(response.metadata.emitted_packets(), 1);
    assert_eq!(response.metadata.generated_proof_actions(), 0);
    assert_eq!(response.metadata.generated_proof_tag(), None);
    assert!(response.actions.events.is_empty());
    assert_eq!(
        responder.metrics().ingress.premature_link_events_suppressed,
        0
    );
    assert_eq!(response.actions.packets.len(), 1);
    assert_eq!(
        response.actions.packets[0].target,
        TxTarget::Only(InterfaceId(7))
    );
    let proof = Packet::parse(&response.actions.packets[0].bytes).unwrap();
    assert_eq!(proof.context, CONTEXT_LRPROOF);

    let established = initiator.ingest(
        &response.actions.packets[0].bytes,
        101,
        InterfaceId(3),
        &mut rng,
    );
    assert_eq!(initiator.link_state(&link_id), Some(LinkState::Active));
    assert_eq!(established.actions.packets.len(), 1);
    assert_eq!(
        established.actions.packets[0].target,
        TxTarget::Only(InterfaceId(3))
    );
    let lrrtt = Packet::parse(&established.actions.packets[0].bytes).unwrap();
    assert_eq!(lrrtt.context, CONTEXT_LRRTT);

    let active = responder.ingest(
        &established.actions.packets[0].bytes,
        102,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(active.disposition, IngressDisposition::Processed);
    assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));

    let oversized = [0xA5; rete_transport::LINK_MDU + 1];
    assert_eq!(
        initiator.send_link_data(&link_id, &oversized, 109, &mut rng),
        Err(EmbeddedSendError::LinkPayloadTooLarge {
            actual: rete_transport::LINK_MDU + 1,
            maximum: rete_transport::LINK_MDU,
        })
    );
    let data = initiator
        .send_link_data(&link_id, b"bounded link payload", 110, &mut rng)
        .unwrap();
    assert_eq!(data.target(), TxTarget::Only(InterfaceId(3)));
    let dedup_before = responder.metrics().transport.packets_dropped_dedup;
    let wrong_interface = responder.ingest(&data.bytes, 110, InterfaceId(8), &mut rng);
    assert_eq!(
        wrong_interface.disposition,
        IngressDisposition::NativeInvalid
    );
    assert!(wrong_interface.actions.events.is_empty());
    assert_eq!(
        responder.metrics().transport.packets_dropped_dedup,
        dedup_before
    );
    let received = responder.ingest(&data.bytes, 110, InterfaceId(7), &mut rng);
    assert!(matches!(
        received.actions.events.first(),
        Some(ApplicationEvent::LinkData { binding, data, .. })
            if binding.link() == link_id.as_bytes()
                && binding.destination() == responder.destination_hash().as_bytes()
                && binding.role() == ApplicationLinkRole::Responder
                && data == b"bounded link payload"
    ));
    assert_eq!(
        responder.metrics().transport.packets_dropped_dedup,
        dedup_before
    );
    assert_eq!(initiator.metrics().admission.link_payload_too_large, 1);
}

#[test]
fn link_data_binding_uses_the_registered_secondary_destination() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let delivery = responder
        .register_destination(
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            DestinationType::Single,
            Direction::In,
        )
        .unwrap();
    assert!(responder.set_accepts_links(&delivery, true));
    initiator
        .register_peer(
            &identity(2),
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            0,
        )
        .unwrap();
    let mut rng = CounterRng::default();

    let (request, link_id) = initiator.initiate_link(delivery, 100, &mut rng).unwrap();
    let proof = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
    assert_eq!(proof.actions.packets.len(), 1);
    let established = initiator.ingest(
        proof.actions.packets[0].bytes(),
        101,
        InterfaceId(3),
        &mut rng,
    );
    assert_eq!(established.actions.packets.len(), 1);
    let active = responder.ingest(
        established.actions.packets[0].bytes(),
        102,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(active.disposition, IngressDisposition::Processed);
    assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));

    let data = initiator
        .send_link_data(&link_id, b"secondary destination payload", 110, &mut rng)
        .unwrap();
    let received = responder.ingest(data.bytes(), 110, InterfaceId(7), &mut rng);
    assert!(matches!(
        received.actions.events.as_slice(),
        [ApplicationEvent::LinkData {
            binding,
            data,
            context: LINK_DATA_CONTEXT_NONE,
            ..
        }] if binding.link() == link_id.as_bytes()
            && binding.destination() == delivery.as_bytes()
            && binding.role() == ApplicationLinkRole::Responder
            && data == b"secondary destination payload"
    ));
}

#[test]
fn request_bindings_use_the_registered_secondary_destination() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let delivery = responder
        .register_destination(
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            DestinationType::Single,
            Direction::In,
        )
        .unwrap();
    assert!(responder.set_accepts_links(&delivery, true));
    initiator
        .register_peer(
            &identity(2),
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            0,
        )
        .unwrap();
    let mut rng = CounterRng::default();

    let (request, link_id) = initiator.initiate_link(delivery, 100, &mut rng).unwrap();
    let proof = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
    let established = initiator.ingest(
        proof.actions.packets[0].bytes(),
        101,
        InterfaceId(3),
        &mut rng,
    );
    responder.ingest(
        established.actions.packets[0].bytes(),
        102,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));

    let anonymous = initiator
        .prepare_anonymous_request(&link_id, "/page/index.mu", 1_700_000_000.25, &mut rng)
        .unwrap();
    let anonymous_handle = anonymous.handle();
    let received = responder.ingest(anonymous.packet().bytes(), 110, InterfaceId(7), &mut rng);
    assert!(matches!(
        received.actions.events.as_slice(),
        [ApplicationEvent::RequestValueReceived {
            binding,
            request,
            path,
            requested_at: 1_700_000_000.25,
            encoded_value,
        }] if binding.link() == link_id.as_bytes()
            && binding.destination() == delivery.as_bytes()
            && binding.role() == ApplicationLinkRole::Responder
            && request == anonymous_handle.request()
            && path == rete_transport::path_hash("/page/index.mu").as_bytes()
            && encoded_value == &[0xc0]
    ));

    let encoded_string = [0xa2, b'o', b'k'];
    let string = initiator
        .prepare_direct_request_value(
            &link_id,
            "/test/echo",
            Some(&encoded_string),
            1_700_000_001.5,
            &mut rng,
        )
        .unwrap();
    let string_handle = string.handle();
    let received = responder.ingest(string.packet().bytes(), 111, InterfaceId(7), &mut rng);
    assert!(matches!(
        received.actions.events.as_slice(),
        [ApplicationEvent::RequestReceived {
            binding,
            request,
            path,
            data,
        }] if binding.link() == link_id.as_bytes()
            && binding.destination() == delivery.as_bytes()
            && binding.role() == ApplicationLinkRole::Responder
            && request == string_handle.request()
            && path == rete_transport::path_hash("/test/echo").as_bytes()
            && data == b"ok"
    ));
}

#[test]
fn responder_link_data_proof_is_exact_withheld_and_released_by_ownership() {
    let mut initiator = node(1);
    let mut responder = node(2);
    responder.set_inbound_proof_policy(InboundProofPolicy::Retain);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

    let data = initiator
        .send_link_data(&link_id, b"durable direct LXMF carrier", 110, &mut rng)
        .unwrap();
    let packet_hash = Packet::parse(data.bytes()).unwrap().compute_hash();
    let preflight = responder
        .preflight_ingest(data.bytes(), InterfaceId(7), IngressOrigin::RemoteInterface)
        .unwrap();
    assert!(matches!(
        preflight.proof_expectation,
        Some(InboundProofExpectation::ResponderLinkData(
            ResponderLinkProofExpectation {
                link_id: expected_link_id,
                packet_hash: expected_packet_hash,
                delivery: ResponderLinkProofDelivery::Retain,
            }
        )) if expected_link_id == link_id && expected_packet_hash == packet_hash
    ));

    let wrong_interface = responder.ingest(data.bytes(), 110, InterfaceId(8), &mut rng);
    assert_eq!(
        wrong_interface.disposition,
        IngressDisposition::NativeInvalid
    );
    assert!(wrong_interface.actions.events.is_empty());
    assert!(wrong_interface.actions.packets.is_empty());
    assert_eq!(wrong_interface.actions.retained_proof_count(), 0);

    let received = responder.ingest(data.bytes(), 110, InterfaceId(7), &mut rng);
    assert_eq!(received.disposition, IngressDisposition::Processed);
    assert_eq!(received.metadata.emitted_packets(), 0);
    assert_eq!(received.metadata.generated_proof_actions(), 0);
    assert!(received.actions.packets.is_empty());
    assert_eq!(received.actions.retained_proof_count(), 1);
    assert!(matches!(
        received.actions.events.as_slice(),
        [ApplicationEvent::LinkData {
            binding,
            data,
            context: LINK_DATA_CONTEXT_NONE,
            ..
        }] if binding.link() == link_id.as_bytes()
            && binding.destination() == responder.destination_hash().as_bytes()
            && binding.role() == ApplicationLinkRole::Responder
            && data == b"durable direct LXMF carrier"
    ));

    let retained = received.actions.events.retained_proof().unwrap();
    assert_eq!(retained.event_index(), 0);
    let proof_pointer = retained.proof.packet.bytes.as_ptr();
    let proof = &retained.proof.packet;
    assert_eq!(proof.target(), TxTarget::Only(InterfaceId(7)));
    assert_eq!(proof.protocol_token(), None);
    assert_eq!(proof.bytes().len(), 115);
    let parsed = Packet::parse(proof.bytes()).unwrap();
    assert_eq!(parsed.flags, 0x0f);
    assert_eq!(parsed.hops, 0);
    assert_eq!(parsed.header_type, HeaderType::Header1);
    assert_eq!(parsed.packet_type, PacketType::Proof);
    assert_eq!(parsed.dest_type, DestType::Link);
    assert_eq!(parsed.destination_hash, link_id.as_ref());
    assert_eq!(parsed.context, CONTEXT_NONE);
    assert_eq!(parsed.payload.len(), 96);
    assert_eq!(&parsed.payload[..32], &packet_hash);
    let peer_signing_key = initiator
        .core
        .transport
        .get_link(&link_id)
        .unwrap()
        .peer_ed25519_pub;
    assert!(
        Identity::verify_raw_ed25519(&peer_signing_key, &packet_hash, &parsed.payload[32..96],)
            .is_ok()
    );

    let mut event_slots = [crate::ApplicationEventSlot::new()];
    let mut event_owner = crate::ApplicationEventOwner::new(&mut event_slots);
    event_owner
        .try_offer_actions(received.actions)
        .expect("the Link event and proof move into one owner");
    let lease = event_owner.lease_next().unwrap();
    assert!(lease.has_retained_proof());
    assert_eq!(lease.event().kind(), ApplicationEventKind::LinkData);

    let mut proof_slots = [crate::DelayedProofSlot::new()];
    let mut proof_owner = crate::DelayedProofOwner::new(&mut proof_slots);
    let acknowledged = lease
        .try_reserve_delayed(&mut proof_owner)
        .unwrap()
        .acknowledge_into_ready();
    assert_eq!(acknowledged.event().kind(), ApplicationEventKind::LinkData);
    drop(acknowledged.into_event());

    let released = proof_owner.lease_next().unwrap().release_actions();
    assert!(released.events.is_empty());
    assert_eq!(released.packets.len(), 1);
    assert_eq!(released.packets[0].bytes().as_ptr(), proof_pointer);
    assert_eq!(released.packets[0].target(), TxTarget::Only(InterfaceId(7)));
    assert_eq!(released.packets[0].bytes().len(), 115);
}

#[test]
fn always_link_proof_is_immediate_exact_and_has_no_retained_owner() {
    let mut initiator = node(1);
    let mut responder = node(2);
    responder.set_inbound_proof_policy(InboundProofPolicy::Always);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

    let data = initiator
        .send_link_data(&link_id, b"immediate direct LXMF carrier", 110, &mut rng)
        .unwrap();
    let packet_hash = Packet::parse(data.bytes()).unwrap().compute_hash();
    let preflight = responder
        .preflight_ingest(data.bytes(), InterfaceId(7), IngressOrigin::RemoteInterface)
        .unwrap();
    assert!(matches!(
        preflight.proof_expectation,
        Some(InboundProofExpectation::ResponderLinkData(
            ResponderLinkProofExpectation {
                link_id: expected_link_id,
                packet_hash: expected_packet_hash,
                delivery: ResponderLinkProofDelivery::Immediate,
            }
        )) if expected_link_id == link_id && expected_packet_hash == packet_hash
    ));

    let received = responder.ingest(data.bytes(), 110, InterfaceId(7), &mut rng);
    assert_eq!(received.disposition, IngressDisposition::Processed);
    assert_eq!(received.metadata.emitted_packets(), 1);
    assert_eq!(received.metadata.generated_proof_actions(), 1);
    assert_eq!(received.actions.retained_proof_count(), 0);
    assert!(matches!(
        received.actions.events.as_slice(),
        [ApplicationEvent::LinkData {
            binding,
            data,
            context: LINK_DATA_CONTEXT_NONE,
            ..
        }] if binding.link() == link_id.as_bytes()
            && binding.destination() == responder.destination_hash().as_bytes()
            && binding.role() == ApplicationLinkRole::Responder
            && data == b"immediate direct LXMF carrier"
    ));
    assert_eq!(received.actions.packets.len(), 1);
    let proof = &received.actions.packets[0];
    assert_eq!(proof.target(), TxTarget::Only(InterfaceId(7)));
    assert_eq!(proof.protocol_token(), None);
    let parsed = Packet::parse(proof.bytes()).unwrap();
    assert_eq!(parsed.flags, 0x0f);
    assert_eq!(parsed.hops, 0);
    assert_eq!(parsed.header_type, HeaderType::Header1);
    assert_eq!(parsed.packet_type, PacketType::Proof);
    assert_eq!(parsed.dest_type, DestType::Link);
    assert_eq!(parsed.destination_hash, link_id.as_ref());
    assert_eq!(parsed.context, CONTEXT_NONE);
    assert_eq!(parsed.payload.len(), 96);
    assert_eq!(&parsed.payload[..32], &packet_hash);
    let peer_signing_key = initiator
        .core
        .transport
        .get_link(&link_id)
        .unwrap()
        .peer_ed25519_pub;
    assert!(
        Identity::verify_raw_ed25519(&peer_signing_key, &packet_hash, &parsed.payload[32..96],)
            .is_ok()
    );

    let replay = responder.ingest(data.bytes(), 111, InterfaceId(7), &mut rng);
    assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
    assert!(replay.actions.events.is_empty());
    assert!(replay.actions.packets.is_empty());
    assert_eq!(replay.actions.retained_proof_count(), 0);

    let invalid = initiator
        .send_link_data(&link_id, b"wrong interface", 112, &mut rng)
        .unwrap();
    let invalid = responder.ingest(invalid.bytes(), 112, InterfaceId(8), &mut rng);
    assert_eq!(invalid.disposition, IngressDisposition::NativeInvalid);
    assert!(invalid.actions.events.is_empty());
    assert!(invalid.actions.packets.is_empty());
    assert_eq!(invalid.actions.retained_proof_count(), 0);
}

#[test]
fn retained_link_proof_requires_role_context_binding_and_proof_policy() {
    let mut initiator = node(1);
    let mut responder = node(2);
    responder.set_inbound_proof_policy(InboundProofPolicy::Retain);
    let mut rng = CounterRng::default();
    let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

    let other_context = initiator
        .core
        .transport
        .build_link_data_packet(&link_id, b"other context", 0x5a, &mut rng)
        .unwrap();
    let other_context = responder.ingest(&other_context, 110, InterfaceId(7), &mut rng);
    assert_eq!(other_context.actions.retained_proof_count(), 0);
    assert!(other_context.actions.packets.is_empty());
    assert!(matches!(
        other_context.actions.events.as_slice(),
        [ApplicationEvent::LinkData {
            binding,
            context: 0x5a,
            ..
        }] if binding.role() == ApplicationLinkRole::Responder
    ));

    let reverse = responder
        .send_link_data(&link_id, b"initiator-side receive", 111, &mut rng)
        .unwrap();
    let reverse = initiator.ingest(reverse.bytes(), 111, InterfaceId(3), &mut rng);
    assert_eq!(reverse.actions.retained_proof_count(), 0);
    assert!(reverse.actions.packets.is_empty());
    assert!(matches!(
        reverse.actions.events.as_slice(),
        [ApplicationEvent::LinkData { binding, .. }]
            if binding.role() == ApplicationLinkRole::Initiator
    ));

    responder.set_inbound_proof_policy(InboundProofPolicy::Never);
    let non_retaining = initiator
        .send_link_data(&link_id, b"non-retaining destination", 112, &mut rng)
        .unwrap();
    let non_retaining = responder.ingest(non_retaining.bytes(), 112, InterfaceId(7), &mut rng);
    assert_eq!(non_retaining.actions.retained_proof_count(), 0);
    assert!(non_retaining.actions.packets.is_empty());
    assert!(matches!(
        non_retaining.actions.events.as_slice(),
        [ApplicationEvent::LinkData { binding, .. }]
            if binding.role() == ApplicationLinkRole::Responder
    ));

    responder.set_inbound_proof_policy(InboundProofPolicy::Retain);
    assert!(responder.set_accepts_links(&responder.destination_hash(), false));
    let links_disabled = initiator
        .send_link_data(&link_id, b"links now disabled", 113, &mut rng)
        .unwrap();
    let links_disabled = responder.ingest(links_disabled.bytes(), 113, InterfaceId(7), &mut rng);
    assert_eq!(links_disabled.disposition, IngressDisposition::Processed);
    assert_eq!(links_disabled.actions.retained_proof_count(), 1);
    assert!(links_disabled.actions.packets.is_empty());
    assert!(matches!(
        links_disabled.actions.events.as_slice(),
        [ApplicationEvent::LinkData {
            binding,
            data,
            context: LINK_DATA_CONTEXT_NONE,
            ..
        }] if binding.link() == link_id.as_bytes()
            && binding.role() == ApplicationLinkRole::Responder
            && data == b"links now disabled"
    ));

    assert!(responder.set_accepts_links(&responder.destination_hash(), true));
    responder
        .core
        .transport
        .get_link_mut(&link_id)
        .unwrap()
        .state = LinkState::Stale;
    let stale = initiator
        .send_link_data(&link_id, b"revive stale responder", 114, &mut rng)
        .unwrap();
    let stale = responder.ingest(stale.bytes(), 114, InterfaceId(7), &mut rng);
    assert_eq!(stale.actions.retained_proof_count(), 1);
    assert!(stale.actions.packets.is_empty());
    assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));
}

#[test]
fn inbound_link_capacity_rejects_before_emitting_a_false_proof() {
    let mut responder = TwoLinkNode::new(
        identity(9),
        "reticulum",
        &["embedded"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    let responder_identity = identity(9);
    let destination = responder.destination_hash();
    let mut rng = CounterRng::default();
    let mut first_link_id = None;
    for (tag, now) in [(10, 1), (11, 2)] {
        let mut initiator = TwoLinkNode::new(
            identity(tag),
            "reticulum",
            &["initiator"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        initiator
            .register_peer(&responder_identity, "reticulum", &["embedded"], 0)
            .unwrap();
        let (request, link_id) = initiator.initiate_link(destination, now, &mut rng).unwrap();
        first_link_id.get_or_insert(link_id);
        let result = responder.ingest(&request.bytes, now, InterfaceId(now as u8), &mut rng);
        assert_eq!(result.disposition, IngressDisposition::Processed);
    }
    assert!(
        !responder.can_initiate_link(),
        "inbound responder Links occupy the same owned-Link table as local initiators"
    );

    let mut overflow = TwoLinkNode::new(
        identity(12),
        "reticulum",
        &["overflow"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    overflow
        .register_peer(&responder_identity, "reticulum", &["embedded"], 0)
        .unwrap();
    let (overflow_request, overflow_link_id) =
        overflow.initiate_link(destination, 3, &mut rng).unwrap();
    let rng_before_rejection = rng.0;
    let received_before_rejection = responder.metrics().transport.packets_received;
    let overflow_result = responder.ingest(&overflow_request.bytes, 3, InterfaceId(3), &mut rng);
    assert_eq!(
        overflow_result.disposition,
        IngressDisposition::Rejected(IngressDropReason::OwnedLinkTableFull { limit: 2 })
    );
    assert!(overflow_result.actions.events.is_empty());
    assert!(overflow_result.actions.packets.is_empty());
    let rejected_metrics = responder.metrics();
    assert_eq!(rng.0, rng_before_rejection);
    assert_eq!(
        rejected_metrics.transport.packets_received,
        received_before_rejection
    );
    assert_eq!(rejected_metrics.ingress.owned_link_full, 1);
    assert_eq!(rejected_metrics.capacity.links.used, 2);

    let immediate_replay = responder.ingest(&overflow_request.bytes, 4, InterfaceId(3), &mut rng);
    assert_eq!(
        immediate_replay.disposition,
        IngressDisposition::NativeDuplicate
    );
    assert!(immediate_replay.actions.events.is_empty());
    assert!(immediate_replay.actions.packets.is_empty());
    assert_eq!(responder.metrics().ingress.owned_link_full, 1);
    assert_eq!(responder.metrics().capacity.links.used, 2);
    assert_eq!(rng.0, rng_before_rejection);

    let close = responder.close_link(&first_link_id.unwrap(), &mut rng);
    assert_eq!(close.packets.len(), 1);
    assert_eq!(close.events.len(), 1);
    assert_eq!(close.unroutable_packets, 0);
    assert_eq!(responder.metrics().capacity.links.used, 1);
    assert!(responder.can_initiate_link());

    let replay = responder.ingest(&overflow_request.bytes, 5, InterfaceId(3), &mut rng);
    assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
    assert!(replay.actions.events.is_empty());
    assert!(replay.actions.packets.is_empty());
    assert_eq!(responder.metrics().capacity.links.used, 1);

    let (fresh_request, fresh_link_id) = overflow.initiate_link(destination, 6, &mut rng).unwrap();
    assert_ne!(fresh_link_id, overflow_link_id);
    let fresh = responder.ingest(&fresh_request.bytes, 6, InterfaceId(3), &mut rng);
    assert_eq!(fresh.disposition, IngressDisposition::Processed);
    assert_eq!(fresh.actions.packets.len(), 1);
    assert_eq!(responder.metrics().capacity.links.used, 2);
}

#[test]
fn inbound_handshake_timeout_recovers_embedded_capacity_without_close_packets() {
    let mut responder = TwoLinkNode::new(
        identity(30),
        "reticulum",
        &["embedded-timeout"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    let responder_identity = identity(30);
    let destination = responder.destination_hash();
    let mut rng = CounterRng::default();

    for (tag, now) in [(31, 1), (32, 2)] {
        let mut initiator = TwoLinkNode::new(
            identity(tag),
            "reticulum",
            &["timeout-initiator"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        initiator
            .register_peer(&responder_identity, "reticulum", &["embedded-timeout"], 0)
            .unwrap();
        let (request, _) = initiator.initiate_link(destination, now, &mut rng).unwrap();
        let admitted = responder.ingest(&request.bytes, now, InterfaceId(now as u8), &mut rng);
        assert_eq!(admitted.disposition, IngressDisposition::Processed);
        assert_eq!(admitted.actions.packets.len(), 1);
    }

    assert_eq!(responder.metrics().capacity.links.used, 2);
    assert!(!responder.can_initiate_link());
    let failed_before = responder.metrics().transport.links_failed;
    let closed_before = responder.metrics().transport.links_closed;

    let before_deadline = responder.tick(366, &mut rng);
    assert!(before_deadline.packets.is_empty());
    assert!(matches!(
        before_deadline.events.as_slice(),
        [ApplicationEvent::Tick {
            closed_links: 0,
            ..
        }]
    ));
    assert_eq!(responder.metrics().capacity.links.used, 2);

    // Direct ingress stores one post-ingress hop, so the first responder
    // expires at 1 + 360 + 6 seconds while the second remains retained.
    let first_deadline = responder.tick(367, &mut rng);
    assert!(first_deadline.packets.is_empty());
    assert!(matches!(
        first_deadline.events.as_slice(),
        [ApplicationEvent::Tick {
            closed_links: 1,
            ..
        }]
    ));
    assert_eq!(responder.metrics().capacity.links.used, 1);
    assert!(responder.can_initiate_link());

    let second_deadline = responder.tick(368, &mut rng);
    assert!(second_deadline.packets.is_empty());
    assert!(matches!(
        second_deadline.events.as_slice(),
        [ApplicationEvent::Tick {
            closed_links: 1,
            ..
        }]
    ));
    let final_metrics = responder.metrics();
    assert_eq!(final_metrics.capacity.links.used, 0);
    assert_eq!(final_metrics.transport.links_failed, failed_before);
    assert_eq!(
        final_metrics.transport.links_closed,
        closed_before.saturating_add(2)
    );
}

#[test]
fn transport_rejects_arbitrary_header1_relay_link_without_interface_roles() {
    let mut relay = TestNode::new(
        identity(20),
        "reticulum",
        &["relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut initiator = node(21);
    let responder = node(22);
    let mut rng = CounterRng::default();

    let (request, _) = initiator
        .initiate_link(responder.destination_hash(), 1, &mut rng)
        .unwrap();
    let result = relay.ingest(&request.bytes, 1, InterfaceId(4), &mut rng);
    assert_eq!(
        result.disposition,
        IngressDisposition::Rejected(IngressDropReason::Header1RemoteLinkRequestDisabled)
    );
    assert_eq!(relay.metrics().transport.packets_received, 0);
}

#[test]
fn shared_lora_bystander_does_not_repeat_remote_direct_header1_data() {
    let mut sender = node(140);
    let mut receiver = node(141);
    let mut bystander = TestNode::new(
        identity(142),
        "reticulum",
        &["bystander"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let receiver_identity = identity(141);
    let destination = receiver.destination_hash();
    let lora = InterfaceId(1);
    let mut rng = CounterRng::default();

    for participant in [&mut sender, &mut bystander] {
        participant
            .register_peer(&receiver_identity, "reticulum", &["embedded"], 1)
            .unwrap();
        let mut direct = rete_transport::Path::direct(1);
        direct.received_on = Some(lora.0);
        assert!(participant.core.transport.insert_path(destination, direct));
    }

    let mut raw = [0_u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &destination,
            b"direct shared-medium payload",
            2,
            &mut rng,
            &mut raw,
        )
        .unwrap();
    let wire = &raw[..usize::from(prepared.packet_len())];
    let packet = Packet::parse(wire).unwrap();
    assert_eq!(packet.header_type, HeaderType::Header1);
    assert_eq!(prepared.target(), TxTarget::Only(lora));

    let before = bystander.metrics();
    let overheard = bystander.ingest(wire, 2, lora, &mut rng);
    assert_eq!(
        overheard.disposition,
        IngressDisposition::Rejected(IngressDropReason::Header1RemoteDataForwardingDisabled)
    );
    assert!(overheard.actions.events.is_empty());
    assert!(overheard.actions.packets.is_empty());
    let after = bystander.metrics();
    assert_eq!(
        after.ingress.header1_remote_data_forwarding_disabled,
        before
            .ingress
            .header1_remote_data_forwarding_disabled
            .saturating_add(1)
    );
    assert_eq!(
        after.transport.packets_received,
        before.transport.packets_received
    );
    assert_eq!(
        after.transport.packets_forwarded,
        before.transport.packets_forwarded
    );
    assert_eq!(
        after.capacity.reverse_entries,
        before.capacity.reverse_entries
    );

    let received = receiver.ingest(wire, 2, lora, &mut rng);
    assert_eq!(received.disposition, IngressDisposition::Processed);
    assert!(received.actions.events.iter().any(|event| matches!(
        event,
        ApplicationEvent::DataReceived { payload, .. }
            if payload == b"direct shared-medium payload"
    )));
}

#[test]
fn shared_lora_selected_header2_relay_forwards_while_bystander_stays_silent() {
    let mut sender = node(150);
    let mut relay = TestNode::new(
        identity(151),
        "reticulum",
        &["relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut bystander = TestNode::new(
        identity(152),
        "reticulum",
        &["bystander"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut receiver = node(153);
    let receiver_identity = identity(153);
    let destination = receiver.destination_hash();
    let relay_identity = relay.identity_hash();
    let lora = InterfaceId(1);
    let mut rng = CounterRng::default();

    sender
        .register_peer(&receiver_identity, "reticulum", &["embedded"], 1)
        .unwrap();
    let mut via_relay = rete_transport::Path::via_repeater(relay_identity, 2, 1);
    via_relay.received_on = Some(lora.0);
    assert!(sender.core.transport.insert_path(destination, via_relay));

    for participant in [&mut relay, &mut bystander] {
        participant
            .register_peer(&receiver_identity, "reticulum", &["embedded"], 1)
            .unwrap();
        let mut direct = rete_transport::Path::direct(1);
        direct.received_on = Some(lora.0);
        assert!(participant.core.transport.insert_path(destination, direct));
    }

    let mut raw = [0_u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &destination,
            b"selected relay payload",
            2,
            &mut rng,
            &mut raw,
        )
        .unwrap();
    let wire = &raw[..usize::from(prepared.packet_len())];
    let packet = Packet::parse(wire).unwrap();
    assert_eq!(packet.header_type, HeaderType::Header2);
    assert_eq!(packet.transport_id, Some(relay_identity.as_ref()));
    assert_eq!(prepared.target(), TxTarget::Only(lora));

    let ignored = bystander.ingest(wire, 2, lora, &mut rng);
    assert!(matches!(
        ignored.disposition,
        IngressDisposition::Rejected(IngressDropReason::Header2NotAddressedToUs {
            transport_id,
        }) if transport_id == relay_identity
    ));
    assert!(ignored.actions.packets.is_empty());

    let relayed = relay.ingest(wire, 2, lora, &mut rng);
    assert_eq!(relayed.disposition, IngressDisposition::Processed);
    assert_eq!(relayed.actions.packets.len(), 1);
    assert_eq!(relayed.actions.packets[0].target(), TxTarget::Only(lora));
    let forwarded_wire = relayed.actions.packets[0].bytes();
    let forwarded = Packet::parse(forwarded_wire).unwrap();
    assert_eq!(forwarded.header_type, HeaderType::Header1);
    assert_eq!(forwarded.hops, 1);

    let bystander_before = bystander.metrics();
    let repeated = bystander.ingest(forwarded_wire, 3, lora, &mut rng);
    assert_eq!(
        repeated.disposition,
        IngressDisposition::Rejected(IngressDropReason::Header1RemoteDataForwardingDisabled)
    );
    assert!(repeated.actions.packets.is_empty());
    let bystander_after = bystander.metrics();
    assert_eq!(
        bystander_after
            .ingress
            .header1_remote_data_forwarding_disabled,
        bystander_before
            .ingress
            .header1_remote_data_forwarding_disabled
            .saturating_add(1)
    );
    assert_eq!(
        bystander_after.transport.packets_forwarded,
        bystander_before.transport.packets_forwarded
    );

    let received = receiver.ingest(forwarded_wire, 3, lora, &mut rng);
    assert_eq!(received.disposition, IngressDisposition::Processed);
    assert!(received.actions.events.iter().any(|event| matches!(
        event,
        ApplicationEvent::DataReceived { payload, .. } if payload == b"selected relay payload"
    )));
}

#[test]
fn header2_relay_link_capacity_is_typed_and_transactional() {
    let mut relay = TestNode::new(
        identity(30),
        "reticulum",
        &["relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let relay_identity = relay.identity_hash();
    let mut rng = CounterRng::default();
    let mut overflow_request = None;

    for index in 0..3_u8 {
        let responder_tag = 31 + index;
        let responder_identity = identity(responder_tag);
        let responder = node(responder_tag);
        let destination = responder.destination_hash();
        relay
            .register_peer(
                &responder_identity,
                "reticulum",
                &["embedded"],
                u64::from(index),
            )
            .unwrap();
        let mut relay_path = rete_transport::Path::direct(u64::from(index));
        relay_path.received_on = Some(9);
        assert!(relay.core.transport.insert_path(destination, relay_path));

        let mut initiator = node(40 + index);
        initiator
            .register_peer(
                &responder_identity,
                "reticulum",
                &["embedded"],
                u64::from(index),
            )
            .unwrap();
        let mut initiator_path =
            rete_transport::Path::via_repeater(relay_identity, 2, u64::from(index));
        initiator_path.received_on = Some(4);
        assert!(
            initiator
                .core
                .transport
                .insert_path(destination, initiator_path)
        );
        let (request, _) = initiator
            .initiate_link(destination, u64::from(index + 1), &mut rng)
            .unwrap();
        assert_eq!(
            Packet::parse(request.bytes()).unwrap().transport_id,
            Some(relay_identity.as_ref())
        );

        let report = relay.ingest(
            request.bytes(),
            u64::from(index + 1),
            InterfaceId(4),
            &mut rng,
        );
        if index < 2 {
            assert_eq!(report.disposition, IngressDisposition::Processed);
            assert_eq!(report.actions.packets.len(), 1);
            assert_eq!(
                report.actions.packets[0].target(),
                TxTarget::Only(InterfaceId(9))
            );
        } else {
            assert_eq!(
                report.disposition,
                IngressDisposition::Rejected(IngressDropReason::RelayLinkTableFull { limit: 2 })
            );
            assert!(report.actions.packets.is_empty());
            overflow_request = Some(request);
        }
    }

    let metrics = relay.metrics();
    assert_eq!(metrics.capacity.relay_links.used, 2);
    assert_eq!(metrics.capacity.links.used, 0);
    assert_eq!(metrics.ingress.relay_link_full, 1);
    assert_eq!(metrics.transport.packets_received, 3);

    let replay = relay.ingest(
        overflow_request.as_ref().unwrap().bytes(),
        10,
        InterfaceId(4),
        &mut rng,
    );
    assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
    assert_eq!(relay.metrics().capacity.relay_links.used, 2);
}

#[test]
fn header2_policy_filters_other_transports_and_admits_owned_local_termination() {
    let mut endpoint = node(23);
    let mut rng = CounterRng::default();
    let other_transport = [0xEE; rete_core::TRUNCATED_HASH_LEN];
    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Single)
        .destination_hash(endpoint.destination_hash().as_ref())
        .context(0)
        .payload(b"not for this transport")
        .via(Some(&other_transport))
        .build()
        .unwrap();
    let rejected = endpoint.ingest(&raw[..len], 1, InterfaceId(1), &mut rng);
    assert!(matches!(
        rejected.disposition,
        IngressDisposition::Rejected(IngressDropReason::Header2NotAddressedToUs { .. })
    ));
    assert_eq!(endpoint.metrics().transport.packets_received, 0);

    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Link)
        .destination_hash(&[0x7e; rete_core::TRUNCATED_HASH_LEN])
        .context(CONTEXT_RESOURCE_ADV)
        .payload(b"foreign resource")
        .via(Some(&other_transport))
        .build()
        .unwrap();
    let foreign_resource = endpoint.ingest(&raw[..len], 2, InterfaceId(1), &mut rng);
    assert!(matches!(
        foreign_resource.disposition,
        IngressDisposition::Rejected(IngressDropReason::Header2NotAddressedToUs { .. })
    ));
    assert_eq!(endpoint.metrics().ingress.resource_disabled, 0);
    assert_eq!(endpoint.metrics().ingress.header2_filtered, 2);

    let plain_destination = endpoint
        .register_destination(
            "reticulum",
            &["plain"],
            DestinationType::Plain,
            Direction::In,
        )
        .unwrap();
    let endpoint_transport = endpoint.identity_hash();
    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Single)
        .destination_hash(plain_destination.as_ref())
        .context(0)
        .payload(b"wrong destination type")
        .via(Some(endpoint_transport.as_bytes()))
        .build()
        .unwrap();
    let mismatched = endpoint.ingest(&raw[..len], 3, InterfaceId(1), &mut rng);
    assert_eq!(
        mismatched.disposition,
        IngressDisposition::NoObservableOutcome
    );
    assert!(mismatched.actions.events.is_empty());
    assert!(mismatched.actions.packets.is_empty());

    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Plain)
        .destination_hash(plain_destination.as_ref())
        .context(0)
        .payload(b"local plain payload")
        .via(Some(endpoint_transport.as_bytes()))
        .build()
        .unwrap();
    let admitted = endpoint.ingest(&raw[..len], 4, InterfaceId(1), &mut rng);
    assert!(matches!(
        admitted.actions.events.as_slice(),
        [ApplicationEvent::DataReceived {
            destination,
            payload,
            ..
        }]
            if *destination == *plain_destination.as_bytes()
                && payload == b"local plain payload"
    ));

    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Announce)
        .dest_type(DestType::Single)
        .destination_hash(&[0xA4; rete_core::TRUNCATED_HASH_LEN])
        .context(0)
        .payload(b"invalid but dispatchable announce")
        .via(Some(endpoint_transport.as_bytes()))
        .build()
        .unwrap();
    let announce = endpoint.ingest(&raw[..len], 5, InterfaceId(1), &mut rng);
    assert!(!matches!(
        announce.disposition,
        IngressDisposition::Rejected(_)
    ));

    let mut transport = TestNode::new(
        identity(24),
        "reticulum",
        &["embedded"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let plain_destination = transport
        .register_destination(
            "reticulum",
            &["transport-plain"],
            DestinationType::Plain,
            Direction::In,
        )
        .unwrap();
    let transport_identity = transport.identity_hash();
    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Plain)
        .destination_hash(plain_destination.as_ref())
        .context(0)
        .payload(b"must terminate locally")
        .via(Some(transport_identity.as_bytes()))
        .build()
        .unwrap();
    let admitted = transport.ingest(&raw[..len], 2, InterfaceId(1), &mut rng);
    assert_eq!(admitted.disposition, IngressDisposition::Processed);
    assert!(matches!(
        admitted.actions.events.as_slice(),
        [ApplicationEvent::DataReceived {
            destination,
            payload,
            ..
        }]
            if *destination == *plain_destination.as_bytes()
                && payload == b"must terminate locally"
    ));
    assert_eq!(transport.metrics().ingress.header2_filtered, 0);
    assert_eq!(transport.metrics().transport.packets_received, 1);
}

#[test]
fn owned_header2_link_request_uses_canonical_local_admission() {
    let mut responder = TestNode::new(
        identity(26),
        "reticulum",
        &["embedded"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    let destination = responder.destination_hash();
    let responder_identity = identity(26);
    let mut initiator = node(27);
    initiator
        .register_peer(&responder_identity, "reticulum", &["embedded"], 1)
        .unwrap();
    let mut path = rete_transport::Path::via_repeater(responder.identity_hash(), 1, 1);
    path.received_on = Some(3);
    assert!(initiator.core.transport.insert_path(destination, path));
    let mut rng = CounterRng::default();
    let (request, link_id) = initiator.initiate_link(destination, 2, &mut rng).unwrap();
    assert_eq!(request.target(), TxTarget::Only(InterfaceId(3)));
    let parsed = Packet::parse(request.bytes()).unwrap();
    assert_eq!(parsed.header_type, HeaderType::Header2);
    assert_eq!(
        parsed.transport_id,
        Some(responder.identity_hash().as_ref())
    );

    let report = responder.ingest(request.bytes(), 3, InterfaceId(8), &mut rng);
    assert_eq!(report.disposition, IngressDisposition::Processed);
    assert!(report.actions.events.is_empty());
    assert_eq!(report.actions.packets.len(), 1);
    assert_eq!(
        report.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(8))
    );
    assert_eq!(responder.link_state(&link_id), Some(LinkState::Handshake));
    assert_eq!(responder.metrics().capacity.links.used, 1);
    assert_eq!(responder.metrics().capacity.relay_links.used, 0);
}

#[test]
fn endpoint_never_forwards_an_unmatched_proof() {
    let mut endpoint = node(25);
    let mut rng = CounterRng::default();
    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Proof)
        .dest_type(DestType::Single)
        .destination_hash(&[0xA5; rete_core::TRUNCATED_HASH_LEN])
        .context(0)
        .payload(b"unknown proof")
        .build()
        .unwrap();
    let report = endpoint.ingest(&raw[..len], 1, InterfaceId(1), &mut rng);
    assert_eq!(report.disposition, IngressDisposition::NoObservableOutcome);
    assert!(report.actions.packets.is_empty());
    assert_eq!(endpoint.metrics().ingress.endpoint_forward_suppressed, 1);
}

#[test]
fn endpoint_ingress_policy_retains_owned_exact_and_suppresses_propagation() {
    assert!(endpoint_retains_ingress_packet(PacketRouting::All));
    assert!(endpoint_retains_ingress_packet(
        PacketRouting::SourceInterface
    ));
    assert!(endpoint_retains_ingress_packet(
        PacketRouting::ExactInterface(9)
    ));
    assert!(endpoint_retains_ingress_packet(
        PacketRouting::BoundInterface(9)
    ));
    assert!(!endpoint_retains_ingress_packet(
        PacketRouting::AllExceptSource
    ));
}

#[test]
fn origin_packet_resolution_preserves_absolute_routing() {
    let exact = resolve_origin_packet(OutboundPacket::new(
        vec![0xa5],
        PacketRouting::ExactInterface(7),
    ));
    assert_eq!(exact.bytes(), &[0xa5]);
    assert_eq!(exact.target(), TxTarget::Only(InterfaceId(7)));

    let bound = resolve_origin_packet(OutboundPacket::new(
        vec![0xb5],
        PacketRouting::BoundInterface(8),
    ));
    assert_eq!(bound.bytes(), &[0xb5]);
    assert_eq!(bound.target(), TxTarget::Only(InterfaceId(8)));

    let broadcast = resolve_origin_packet(OutboundPacket::broadcast(vec![0xb6]));
    assert_eq!(broadcast.bytes(), &[0xb6]);
    assert_eq!(broadcast.target(), TxTarget::All);
}

#[test]
#[should_panic(expected = "pinned Rete origin API emitted source-relative routing")]
fn origin_packet_resolution_rejects_source_context() {
    let _ = resolve_origin_packet(OutboundPacket::new(
        vec![0xc7],
        PacketRouting::SourceInterface,
    ));
}

#[test]
fn tick_resolves_absolute_routes_and_counts_source_dependent_routes() {
    let actions = resolve_tick_actions(
        IngestOutcome {
            events: Vec::new(),
            packets: vec![
                OutboundPacket::broadcast(vec![0xa1]),
                OutboundPacket::new(vec![0xb2], PacketRouting::ExactInterface(9)),
                OutboundPacket::new(vec![0xb3], PacketRouting::BoundInterface(8)),
                OutboundPacket::new(vec![0xc3], PacketRouting::SourceInterface),
                OutboundPacket::new(vec![0xd4], PacketRouting::AllExceptSource),
            ],
            rejection: None,
        },
        Vec::new(),
    );

    assert_eq!(actions.packets.len(), 3);
    assert_eq!(actions.packets[0].bytes(), &[0xa1]);
    assert_eq!(actions.packets[0].target(), TxTarget::All);
    assert_eq!(actions.packets[1].bytes(), &[0xb2]);
    assert_eq!(actions.packets[1].target(), TxTarget::Only(InterfaceId(9)));
    assert_eq!(actions.packets[2].bytes(), &[0xb3]);
    assert_eq!(actions.packets[2].target(), TxTarget::Only(InterfaceId(8)));
    assert_eq!(actions.unroutable_packets, 2);
}

#[test]
fn forwarded_transport_proof_is_not_classified_as_locally_generated() {
    let mut transport = TestNode::new(
        identity(27),
        "reticulum",
        &["transport"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut rng = CounterRng::default();
    let mut destination_node = node(28);
    let destination = destination_node.destination_hash();
    destination_node.queue_announce(None, 0, &mut rng).unwrap();
    let announce = destination_node
        .flush_announces(0, &mut rng)
        .into_iter()
        .next()
        .expect("the destination announce must be ready immediately");
    let learned = transport.ingest(announce.bytes(), 0, InterfaceId(7), &mut rng);
    assert_eq!(learned.disposition, IngressDisposition::Processed);
    assert_eq!(
        transport.recall_identity(&destination),
        Some(identity(28).public_key())
    );
    assert_eq!(
        transport
            .route(&destination)
            .and_then(|route| route.received_on),
        Some(InterfaceId(7))
    );
    let mut data = [0u8; rete_core::MTU];
    let data_len = PacketBuilder::new(&mut data)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Single)
        .destination_hash(destination.as_ref())
        .context(0)
        .payload(b"establish reverse entry")
        .build()
        .unwrap();
    let covered_hash = Packet::parse(&data[..data_len]).unwrap().compute_hash();
    let forwarded_data =
        transport.ingest_local_origin(&data[..data_len], 1, InterfaceId(1), &mut rng);
    assert_eq!(forwarded_data.disposition, IngressDisposition::Processed);
    assert_eq!(forwarded_data.actions.packets.len(), 1);
    assert_eq!(
        forwarded_data.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(7))
    );

    let mut raw = [0u8; rete_core::MTU];
    let mut proof_payload = [0u8; 96];
    proof_payload[..32].copy_from_slice(&covered_hash);
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Proof)
        .dest_type(DestType::Single)
        .destination_hash(&covered_hash[..rete_core::TRUNCATED_HASH_LEN])
        .context(0)
        .payload(&proof_payload)
        .build()
        .unwrap();

    let report = transport.ingest(&raw[..len], 2, InterfaceId(7), &mut rng);

    assert_eq!(report.disposition, IngressDisposition::Processed);
    assert_eq!(report.actions.packets.len(), 1);
    assert_eq!(report.metadata.emitted_packets(), 1);
    assert_eq!(report.metadata.generated_proof_actions(), 0);
    assert_eq!(report.metadata.generated_proof_tag(), None);
    assert_eq!(
        report.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(1))
    );
}

#[test]
fn transport_rejects_forwarding_before_reverse_state_is_lost() {
    let mut relay = TestNode::new(
        identity(26),
        "reticulum",
        &["relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut rng = CounterRng::default();

    for tag in 70..74 {
        let peer = identity(tag);
        let destination = node(tag).destination_hash();
        relay
            .register_peer(&peer, "reticulum", &["embedded"], u64::from(tag))
            .unwrap();
        let mut path = rete_transport::Path::direct(u64::from(tag));
        path.received_on = Some(7);
        assert!(relay.core.transport.insert_path(destination, path));
        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(destination.as_ref())
            .context(0)
            .payload(&[tag])
            .build()
            .unwrap();
        let report =
            relay.ingest_local_origin(&raw[..len], u64::from(tag), InterfaceId(tag), &mut rng);
        assert_eq!(report.disposition, IngressDisposition::Processed);
        assert_eq!(report.actions.packets.len(), 1);
        assert_eq!(
            report.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(7))
        );
    }
    assert_eq!(relay.metrics().capacity.reverse_entries.used, 4);

    let overflow_tag = 74;
    let peer = identity(overflow_tag);
    let destination = node(overflow_tag).destination_hash();
    relay
        .register_peer(&peer, "reticulum", &["embedded"], u64::from(overflow_tag))
        .unwrap();
    let mut path = rete_transport::Path::direct(u64::from(overflow_tag));
    path.received_on = Some(7);
    assert!(relay.core.transport.insert_path(destination, path));
    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Single)
        .destination_hash(destination.as_ref())
        .context(0)
        .payload(&[overflow_tag])
        .build()
        .unwrap();
    let report = relay.ingest_local_origin(
        &raw[..len],
        u64::from(overflow_tag),
        InterfaceId(overflow_tag),
        &mut rng,
    );
    assert!(matches!(
        report.disposition,
        IngressDisposition::Rejected(IngressDropReason::ReverseTableFull { limit: 4, .. })
    ));
    assert!(report.actions.packets.is_empty());
    assert_eq!(relay.metrics().transport.packets_received, 4);
    assert_eq!(relay.metrics().ingress.reverse_table_full, 1);
}

#[test]
fn header1_reverse_shim_defers_unbound_peer_path_to_native_invalid() {
    let mut relay = TestNode::new(
        identity(75),
        "reticulum",
        &["relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let peer = identity(76);
    let destination = node(76).destination_hash();
    relay
        .register_peer(&peer, "reticulum", &["embedded"], 1)
        .unwrap();
    assert_eq!(
        relay
            .core
            .transport
            .get_path(&destination)
            .and_then(|path| path.received_on),
        None
    );

    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Single)
        .destination_hash(destination.as_ref())
        .context(0)
        .payload(b"no learned egress")
        .build()
        .unwrap();
    let mut rng = CounterRng::default();

    let report = relay.ingest_local_origin(&raw[..len], 2, InterfaceId(6), &mut rng);
    assert_eq!(report.disposition, IngressDisposition::NativeInvalid);
    assert!(report.actions.events.is_empty());
    assert!(report.actions.packets.is_empty());
    assert_eq!(relay.metrics().capacity.reverse_entries.used, 0);
    assert_eq!(relay.metrics().ingress.native_invalid, 1);
}

#[test]
fn header2_reverse_capacity_rejection_is_typed_and_deduplicated() {
    let mut relay = TestNode::new(
        identity(80),
        "reticulum",
        &["relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let relay_identity = relay.identity_hash();
    let mut rng = CounterRng::default();
    let mut overflow = Vec::new();

    for tag in 81..86_u8 {
        let peer = identity(tag);
        let destination = node(tag).destination_hash();
        relay
            .register_peer(&peer, "reticulum", &["embedded"], u64::from(tag))
            .unwrap();
        let mut path = rete_transport::Path::direct(u64::from(tag));
        path.received_on = Some(7);
        assert!(relay.core.transport.insert_path(destination, path));

        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(destination.as_ref())
            .context(0)
            .payload(&[tag])
            .via(Some(relay_identity.as_bytes()))
            .build()
            .unwrap();
        let report = relay.ingest(&raw[..len], u64::from(tag), InterfaceId(6), &mut rng);
        if tag < 85 {
            assert_eq!(report.disposition, IngressDisposition::Processed);
            assert_eq!(report.actions.packets.len(), 1);
            assert_eq!(
                report.actions.packets[0].target(),
                TxTarget::Only(InterfaceId(7))
            );
        } else {
            assert!(matches!(
                report.disposition,
                IngressDisposition::Rejected(IngressDropReason::ReverseTableFull { limit: 4, .. })
            ));
            assert!(report.actions.packets.is_empty());
            overflow.extend_from_slice(&raw[..len]);
        }
    }

    let metrics = relay.metrics();
    assert_eq!(metrics.capacity.reverse_entries.used, 4);
    assert_eq!(metrics.ingress.reverse_table_full, 1);
    assert_eq!(metrics.transport.packets_received, 5);

    let replay = relay.ingest(&overflow, 90, InterfaceId(6), &mut rng);
    assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
    assert_eq!(relay.metrics().capacity.reverse_entries.used, 4);
}

#[test]
fn native_reverse_route_conflict_maps_to_stable_product_diagnostics() {
    let mut node = node(86);
    let truncated_hash = [0x5a; rete_core::TRUNCATED_HASH_LEN];
    let report = node.finish_ingest(
        IngestOutcome {
            events: Vec::new(),
            packets: Vec::new(),
            rejection: Some(IngestRejection::ReverseRouteConflict { truncated_hash }),
        },
        IngressPreflight {
            before_duplicate: 0,
            before_invalid: 0,
            metadata: IngressMetadata::default(),
            proof_expectation: None,
            local_path_request: None,
            announce_path_before: None,
            derived_broadcast: None,
        },
        InterfaceId(1),
        IngressBroadcastPolicy::default(),
        TerminalCommitCounts::default(),
    );
    assert_eq!(
        report.disposition,
        IngressDisposition::Rejected(IngressDropReason::ReverseRouteConflict { truncated_hash })
    );
    assert!(report.actions.events.is_empty());
    assert!(report.actions.packets.is_empty());
    assert_eq!(node.metrics().ingress.reverse_route_conflict, 1);
}

#[test]
fn header1_reverse_shim_rejects_route_conflict_without_redirecting_state() {
    let mut relay = TestNode::new(
        identity(87),
        "reticulum",
        &["relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let peer = identity(88);
    let destination = node(88).destination_hash();
    relay
        .register_peer(&peer, "reticulum", &["embedded"], 1)
        .unwrap();
    let mut path = rete_transport::Path::direct(1);
    path.received_on = Some(7);
    assert!(relay.core.transport.insert_path(destination, path));
    let mut rng = CounterRng::default();

    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Single)
        .destination_hash(destination.as_ref())
        .context(0)
        .payload(b"stable reverse route")
        .build()
        .unwrap();
    let packet_hash = Packet::parse(&raw[..len]).unwrap().compute_hash();
    let key: [u8; rete_core::TRUNCATED_HASH_LEN] = packet_hash[..rete_core::TRUNCATED_HASH_LEN]
        .try_into()
        .unwrap();
    let first = relay.ingest_local_origin(&raw[..len], 2, InterfaceId(6), &mut rng);
    assert_eq!(first.disposition, IngressDisposition::Processed);
    assert_eq!(first.actions.packets.len(), 1);
    assert_eq!(
        relay.core.transport.get_reverse(&key).map(|entry| (
            entry.received_on,
            entry.forwarded_to,
            entry.timestamp
        )),
        Some((6, 7, 2))
    );

    // Evict the original full hash from the rolling dedup window while
    // leaving the longer-lived reverse entry intact.
    for tag in 0..8_u8 {
        let mut filler = [0u8; rete_core::MTU];
        let filler_len = PacketBuilder::new(&mut filler)
            .packet_type(PacketType::Proof)
            .dest_type(DestType::Single)
            .destination_hash(&[tag; rete_core::TRUNCATED_HASH_LEN])
            .context(0)
            .payload(&[tag])
            .build()
            .unwrap();
        let _ = relay.ingest(
            &filler[..filler_len],
            u64::from(tag + 3),
            InterfaceId(9),
            &mut rng,
        );
    }

    let conflict = relay.ingest_local_origin(&raw[..len], 20, InterfaceId(5), &mut rng);
    assert_eq!(
        conflict.disposition,
        IngressDisposition::Rejected(IngressDropReason::ReverseRouteConflict {
            truncated_hash: key,
        })
    );
    assert!(conflict.actions.packets.is_empty());
    assert_eq!(relay.metrics().ingress.reverse_route_conflict, 1);
    assert_eq!(relay.metrics().transport.packets_forwarded, 1);
    assert_eq!(
        relay.core.transport.get_reverse(&key).map(|entry| (
            entry.received_on,
            entry.forwarded_to,
            entry.timestamp
        )),
        Some((6, 7, 2))
    );

    let replay = relay.ingest_local_origin(&raw[..len], 21, InterfaceId(5), &mut rng);
    assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
    assert_eq!(relay.metrics().transport.packets_forwarded, 1);
    assert_eq!(relay.metrics().capacity.reverse_entries.used, 1);
}

#[test]
fn base_mtu_and_resource_gates_run_before_native_ingest() {
    let mut node = node(30);
    let mut rng = CounterRng::default();
    let oversized = [0u8; rete_core::MTU + 1];
    let oversized_result = node.ingest(&oversized, 1, InterfaceId(1), &mut rng);
    assert!(matches!(
        oversized_result.disposition,
        IngressDisposition::Rejected(IngressDropReason::PacketTooLong { .. })
    ));

    let mut raw = [0u8; rete_core::MTU];
    let len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Link)
        .destination_hash(&[0x44; rete_core::TRUNCATED_HASH_LEN])
        .context(CONTEXT_RESOURCE_ADV)
        .payload(&[0x55; 64])
        .build()
        .unwrap();
    let resource_result = node.ingest(&raw[..len], 2, InterfaceId(1), &mut rng);
    assert_eq!(
        resource_result.disposition,
        IngressDisposition::Rejected(IngressDropReason::ResourceIngressDisabled {
            context: CONTEXT_RESOURCE_ADV
        })
    );
    assert_eq!(node.metrics().transport.packets_received, 0);
}

#[test]
fn channel_receipt_preflight_prevents_native_window_mutation() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
    let response = responder.ingest(&request.bytes, 100, InterfaceId(1), &mut rng);
    let established = initiator.ingest(
        &response.actions.packets[0].bytes,
        101,
        InterfaceId(2),
        &mut rng,
    );
    responder.ingest(
        &established.actions.packets[0].bytes,
        102,
        InterfaceId(1),
        &mut rng,
    );

    initiator
        .send_channel_message(&link_id, 1, b"one", 110, &mut rng)
        .unwrap();
    initiator
        .send_channel_message(&link_id, 2, b"two", 111, &mut rng)
        .unwrap();
    assert_eq!(initiator.metrics().capacity.channel_receipts.used, 2);
    assert_eq!(
        initiator.send_channel_message(&link_id, 3, b"three", 112, &mut rng),
        Err(EmbeddedSendError::ChannelReceiptTableFull { limit: 2 })
    );
    assert_eq!(initiator.metrics().capacity.channel_receipts.used, 2);
}

#[test]
fn wrapper_tick_preflights_channel_retry_route_before_entropy_or_state() {
    let mut initiator = node(1);
    let responder = node(2);
    let mut rng = CounterRng::default();
    let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
    let request = Packet::parse(request.bytes()).unwrap();
    let responder_link =
        rete_transport::Link::from_request(link_id, request.payload, &mut rng, 100).unwrap();
    let responder_identity = identity(2);
    let proof = responder_link.build_proof(&responder_identity).unwrap();
    let link = initiator.core.transport.get_link_mut(&link_id).unwrap();
    link.validate_proof(&proof, &responder_identity).unwrap();
    link.activate(100);
    assert_eq!(link.bound_interface(), None);

    initiator
        .core
        .transport
        .send_channel_message(&link_id, 0x4242, b"route before retry", 200, &mut rng)
        .unwrap();
    let link = initiator.core.transport.get_link(&link_id).unwrap();
    let last_outbound = link.last_outbound;
    let window = link.channel().unwrap().window();
    let rng_before_tick = rng.0;

    let actions = initiator.tick(216, &mut rng);
    assert!(actions.packets.is_empty());
    assert_eq!(actions.unroutable_packets, 0);
    assert_eq!(rng.0, rng_before_tick);
    let link = initiator.core.transport.get_link(&link_id).unwrap();
    assert_eq!(link.last_outbound, last_outbound);
    assert_eq!(link.channel().unwrap().window(), window);
    assert_eq!(link.channel().unwrap().pending_count(), 1);
    assert_eq!(initiator.metrics().capacity.channel_receipts.used, 1);
    assert!(matches!(
        initiator
            .core
            .transport
            .pending_channel_maintenance(216)
            .as_slice(),
        [rete_transport::ChannelMaintenanceAction::Retransmit(_)]
    ));
}

#[test]
fn channel_payload_preflight_runs_before_native_queue_mutation() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
    let response = responder.ingest(request.bytes(), 100, InterfaceId(1), &mut rng);
    let established = initiator.ingest(
        response.actions.packets[0].bytes(),
        101,
        InterfaceId(2),
        &mut rng,
    );
    responder.ingest(
        established.actions.packets[0].bytes(),
        102,
        InterfaceId(1),
        &mut rng,
    );

    let oversized = [0xA5; MAX_CHANNEL_PAYLOAD + 1];
    assert_eq!(
        initiator.send_channel_message(&link_id, 1, &oversized, 110, &mut rng),
        Err(EmbeddedSendError::ChannelPayloadTooLarge {
            actual: MAX_CHANNEL_PAYLOAD + 1,
            maximum: MAX_CHANNEL_PAYLOAD,
        })
    );
    initiator
        .send_channel_message(&link_id, 2, b"one", 111, &mut rng)
        .unwrap();
    initiator
        .send_channel_message(&link_id, 3, b"two", 112, &mut rng)
        .unwrap();
    assert_eq!(initiator.metrics().capacity.channel_receipts.used, 2);
    assert_eq!(initiator.metrics().admission.channel_payload_too_large, 1);
}

#[test]
fn outbound_link_admission_is_mandatory_on_the_owning_type() {
    let mut node = node(40);
    let mut rng = CounterRng::default();
    assert!(node.can_initiate_link());
    for tag in [1, 2] {
        node.initiate_link(
            DestHash::from([tag; rete_core::TRUNCATED_HASH_LEN]),
            u64::from(tag),
            &mut rng,
        )
        .unwrap();
    }
    assert!(!node.can_initiate_link());
    assert_eq!(node.metrics().capacity.links.used, 2);
    let rng_before_rejection = rng.0;
    assert_eq!(
        node.initiate_link(
            DestHash::from([3; rete_core::TRUNCATED_HASH_LEN]),
            3,
            &mut rng,
        ),
        Err(LinkAdmissionError::LinkTableFull { limit: 2 })
    );
    assert_eq!(rng.0, rng_before_rejection);
    assert_eq!(node.metrics().admission.outbound_link_full, 1);
}

#[test]
fn outbound_link_id_collision_preserves_state_and_is_counted() {
    let mut node = node(41);
    let destination = DestHash::from([0xA5; rete_core::TRUNCATED_HASH_LEN]);
    let mut first_rng = CounterRng::default();
    let mut repeated_rng = CounterRng::default();

    let (_, link_id) = node.initiate_link(destination, 1, &mut first_rng).unwrap();
    let retained_state = node.link_state(&link_id);

    assert_eq!(
        node.initiate_link(destination, 2, &mut repeated_rng),
        Err(LinkAdmissionError::LinkIdCollision)
    );
    assert_eq!(node.metrics().capacity.links.used, 1);
    assert_eq!(node.link_state(&link_id), retained_state);
    assert_eq!(node.metrics().admission.outbound_link_collision, 1);
    assert_eq!(node.metrics().admission.outbound_link_full, 0);
    assert_eq!(node.metrics().admission.outbound_link_not_retained, 0);
}

#[test]
fn destination_and_receipt_quotas_preflight_growing_native_collections() {
    type ReceiptNode = EmbeddedNode<2, 2, 8, 2>;
    let mut sender = ReceiptNode::new(
        identity(41),
        "reticulum",
        &["sender"],
        EmbeddedNodeConfig {
            role: NodeRole::Endpoint,
            max_additional_destinations: 1,
            shared_medium_interfaces: 0,
        },
    )
    .unwrap();
    sender
        .register_destination(
            "reticulum",
            &["plain"],
            DestinationType::Plain,
            Direction::In,
        )
        .unwrap();
    assert_eq!(
        sender.register_destination(
            "reticulum",
            &["overflow"],
            DestinationType::Plain,
            Direction::In,
        ),
        Err(DestinationRegistrationError::LimitReached { limit: 1 })
    );

    let receiver = ReceiptNode::new(
        identity(42),
        "reticulum",
        &["receiver"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    sender
        .register_peer(&identity(42), "reticulum", &["receiver"], 0)
        .unwrap();
    let mut rng = CounterRng::default();
    let mut output = [0_u8; RNS_MTU];
    for now in [1, 2] {
        sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"receipt bounded",
                now,
                &mut rng,
                &mut output,
            )
            .unwrap();
    }
    assert_eq!(sender.metrics().capacity.receipts.used, 2);
    assert_eq!(
        sender.prepare_data_into(
            &receiver.destination_hash(),
            b"must not escape without a receipt",
            3,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDataError::ReceiptTableFull { limit: 2 })
    );
    assert_eq!(sender.metrics().admission.destination_limit, 1);
    assert_eq!(sender.metrics().admission.receipt_table_full, 1);

    let oversized = [0xA5; rete_core::ENCRYPTED_MDU + 1];
    assert_eq!(
        sender.prepare_data_into(
            &receiver.destination_hash(),
            &oversized,
            4,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDataError::PayloadTooLarge {
            actual: rete_core::ENCRYPTED_MDU + 1,
            maximum: rete_core::ENCRYPTED_MDU,
        })
    );
    assert_eq!(sender.metrics().admission.data_payload_too_large, 1);
}

#[test]
fn caller_owned_data_preparation_returns_scalar_metadata_and_can_cancel() {
    type ReceiptNode = EmbeddedNode<2, 2, 8, 2>;
    let mut sender = ReceiptNode::new(
        identity(80),
        "reticulum",
        &["sender"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    let receiver = ReceiptNode::new(
        identity(81),
        "reticulum",
        &["receiver"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    sender
        .register_peer(&identity(81), "reticulum", &["receiver"], 0)
        .unwrap();

    let mut rng = CounterRng::default();
    let mut output = [0u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"caller-owned",
            1,
            &mut rng,
            &mut output,
        )
        .unwrap();

    let packet = Packet::parse(&output[..usize::from(prepared.packet_len())]).unwrap();
    assert_eq!(&packet.compute_hash(), prepared.receipt().as_bytes());
    assert_eq!(prepared.target(), TxTarget::All);
    assert_eq!(sender.metrics().capacity.receipts.used, 1);

    assert!(sender.cancel_data_receipt(prepared.receipt()));
    assert!(!sender.cancel_data_receipt(prepared.receipt()));
    assert_eq!(sender.metrics().capacity.receipts.used, 0);
}

#[test]
fn receipt_sink_preserves_full_data_candidate_and_proof_retry_state() {
    let mut sender = node(84);
    let receiver = node(85);
    sender
        .register_peer(&identity(85), "reticulum", &["embedded"], 100)
        .unwrap();
    let mut rng = CounterRng::default();
    let mut output = [0u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"terminal bridge",
            100,
            &mut rng,
            &mut output,
        )
        .unwrap();
    let proof = rete_transport::Transport::<
        rete_transport::HeaplessStorage<4, 2, 8, 2>,
    >::build_proof_packet(&identity(85), prepared.receipt().as_bytes())
    .unwrap();
    let expected = ReceiptCandidate {
        kind: ReceiptKind::Data,
        receipt: prepared.receipt(),
        ingress: Some(IngressObservation::remote(InterfaceId(1), None)),
    };

    let mut invalid_proof = proof.clone();
    *invalid_proof.last_mut().unwrap() ^= 0xff;
    let mut sink = RecordingReceiptSink::default();
    let invalid = sender
        .ingest_with_receipt_sink(&invalid_proof, 101, InterfaceId(1), &mut rng, &mut sink)
        .unwrap();
    assert_eq!(invalid.disposition, IngressDisposition::NoObservableOutcome);
    assert_eq!(sink.attempted, [expected]);
    assert!(sink.terminals.is_empty());
    assert_eq!(sink.active_reservations, 0);
    assert_eq!(sender.metrics().capacity.receipts.used, 1);

    sink.refuse = true;
    let transport_before_refusal = sender.metrics().transport;
    assert!(matches!(
        sender.ingest_with_receipt_sink(&proof, 102, InterfaceId(1), &mut rng, &mut sink,),
        Err(ReceiptReservationUnavailable)
    ));
    assert_eq!(sender.metrics().transport, transport_before_refusal);
    assert_eq!(sender.metrics().capacity.receipts.used, 1);
    assert_eq!(
        sender.metrics().receipt_terminals.reservation_backpressure,
        1
    );

    sink.refuse = false;
    let delivered = sender
        .ingest_with_receipt_sink(&proof, 103, InterfaceId(1), &mut rng, &mut sink)
        .unwrap();
    assert_eq!(delivered.disposition, IngressDisposition::Processed);
    assert!(delivered.actions.events.is_empty());
    assert_eq!(sink.attempted, [expected, expected, expected]);
    assert_eq!(sink.terminals, [ReceiptTerminal::Delivered(expected)]);
    assert_eq!(sink.active_reservations, 0);
    assert_eq!(sender.metrics().capacity.receipts.used, 0);
}

#[test]
fn inbound_proof_policy_completes_announced_encrypted_data_receipt() {
    let mut sender = node(90);
    let mut receiver = node(91);
    receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
    let mut rng = CounterRng::default();

    receiver.queue_announce(None, 100, &mut rng).unwrap();
    let announce = receiver
        .flush_announces(100, &mut rng)
        .into_iter()
        .next()
        .expect("the queued announce must be ready immediately");
    let learned = sender.ingest(announce.bytes(), 100, InterfaceId(4), &mut rng);
    assert_eq!(learned.disposition, IngressDisposition::Processed);
    assert!(sender.route(&receiver.destination_hash()).is_some());

    let mut data = [0u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"announced and proven",
            101,
            &mut rng,
            &mut data,
        )
        .unwrap();
    assert_eq!(prepared.target(), TxTarget::Only(InterfaceId(4)));
    let received = receiver.ingest(
        &data[..usize::from(prepared.packet_len())],
        101,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(received.disposition, IngressDisposition::Processed);
    assert_eq!(received.metadata.wire_packet_type(), Some(PacketType::Data));
    assert_eq!(received.metadata.emitted_packets(), 1);
    assert_eq!(received.metadata.generated_proof_actions(), 1);
    assert_eq!(received.metadata.delivered_receipt_terminals(), 0);
    assert_eq!(received.metadata.timed_out_receipt_terminals(), 0);
    let proof_tag = received
        .metadata
        .generated_proof_tag()
        .expect("one direct proof action has a correlation tag");
    assert!(received.metadata.generated_proof_tags_consistent());
    assert!(!received.metadata.counts_saturated());
    assert!(received.actions.events.iter().any(|event| matches!(
        event,
        ApplicationEvent::DataReceived { payload, .. } if payload == b"announced and proven"
    )));
    assert_eq!(received.actions.packets.len(), 1);
    let proof = &received.actions.packets[0];
    assert_eq!(proof.target(), TxTarget::Only(InterfaceId(7)));
    assert_eq!(
        Packet::parse(proof.bytes()).unwrap().packet_type,
        PacketType::Proof
    );

    let expected = ReceiptCandidate {
        kind: ReceiptKind::Data,
        receipt: prepared.receipt(),
        ingress: Some(IngressObservation::remote(InterfaceId(4), None)),
    };
    let mut sink = RecordingReceiptSink::default();
    let delivered = sender
        .ingest_with_receipt_sink(proof.bytes(), 102, InterfaceId(4), &mut rng, &mut sink)
        .unwrap();
    assert_eq!(delivered.disposition, IngressDisposition::Processed);
    assert_eq!(
        delivered.metadata.wire_packet_type(),
        Some(PacketType::Proof)
    );
    assert_eq!(delivered.metadata.emitted_packets(), 0);
    assert_eq!(delivered.metadata.generated_proof_actions(), 0);
    assert_eq!(delivered.metadata.delivered_receipt_terminals(), 1);
    assert_eq!(delivered.metadata.timed_out_receipt_terminals(), 0);
    assert_eq!(delivered.metadata.delivered_receipt_tag(), Some(proof_tag));
    assert!(delivered.metadata.delivered_receipt_tags_consistent());
    assert!(!delivered.metadata.counts_saturated());
    assert_eq!(sink.terminals, [ReceiptTerminal::Delivered(expected)]);
    assert_eq!(sender.metrics().capacity.receipts.used, 0);
}

#[test]
fn retained_inbound_proof_is_exact_immediate_proof_on_the_source_interface() {
    let mut sender = node(94);
    let mut immediate = node(95);
    let mut retained = node(95);
    immediate.set_inbound_proof_policy(InboundProofPolicy::Always);
    retained.set_inbound_proof_policy(InboundProofPolicy::Retain);
    sender
        .register_peer(&identity(95), "reticulum", &["embedded"], 100)
        .unwrap();

    let mut rng = CounterRng::default();
    let mut data = [0u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &immediate.destination_hash(),
            b"retain until durable",
            101,
            &mut rng,
            &mut data,
        )
        .unwrap();
    let raw = &data[..usize::from(prepared.packet_len())];

    let mut immediate_report =
        immediate.ingest(raw, 101, InterfaceId(7), &mut CounterRng::default());
    assert_eq!(immediate_report.metadata.emitted_packets(), 1);
    assert_eq!(immediate_report.metadata.generated_proof_actions(), 1);
    let immediate_proof = immediate_report.actions.packets.pop().unwrap();
    assert_eq!(immediate_proof.target(), TxTarget::Only(InterfaceId(7)));

    let mut retained_report =
        retained.ingest(raw, 101, InterfaceId(23), &mut CounterRng::default());
    assert_eq!(retained_report.disposition, IngressDisposition::Processed);
    assert_eq!(retained_report.metadata.emitted_packets(), 0);
    assert_eq!(retained_report.metadata.generated_proof_actions(), 0);
    assert!(retained_report.actions.packets.is_empty());
    assert_eq!(retained_report.actions.retained_proof_count(), 1);
    let event = retained_report.actions.events.events.pop().unwrap();
    let event_debug = format!("{event:?}");
    assert!(!event_debug.contains("InterfaceId(23)"));

    let InboundDataProjection::Data(data) = project_inbound_data(event) else {
        panic!("retained destination DATA must remain projectable")
    };
    let (_, payload) = data.into_parts();
    assert_eq!(payload, b"retain until durable");
    let retained_proof = retained_report
        .actions
        .events
        .retained_proof
        .take()
        .unwrap();
    assert_eq!(retained_proof.event_index(), 0);
    let retained_proof_debug = format!("{retained_proof:?}");
    assert_eq!(
        retained_proof_debug,
        "RetainedApplicationProof { event_index: 0, proof_present: true }"
    );
    assert!(!retained_proof_debug.contains("InterfaceId"));
    assert!(!retained_proof_debug.contains('['));
    let (event_index, owner) = retained_proof.into_parts();
    assert_eq!(event_index, 0);

    let released = owner.into_packet();
    assert_eq!(released.target(), TxTarget::Only(InterfaceId(23)));
    assert_eq!(released.protocol_token(), None);
    assert_eq!(released.bytes(), immediate_proof.bytes());
    assert_eq!(
        retained
            .core
            .get_destination(&retained.destination_hash())
            .unwrap()
            .proof_strategy,
        rete_stack::ProofStrategy::ProveApp
    );
}

#[test]
fn retained_undecryptable_data_is_no_outcome_then_duplicate() {
    let mut sender = node(102);
    let mut receiver = node(103);
    receiver.set_inbound_proof_policy(InboundProofPolicy::Retain);
    sender
        .register_peer(&identity(103), "reticulum", &["embedded"], 100)
        .unwrap();

    let mut rng = CounterRng::default();
    let mut data = [0u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"corrupted retained ciphertext",
            101,
            &mut rng,
            &mut data,
        )
        .unwrap();
    let mut corrupted = data[..usize::from(prepared.packet_len())].to_vec();
    // Destination DATA uses an authenticated encrypted token; its final
    // bytes are the HMAC. Preserve the parseable packet shape while making
    // authentication fail.
    *corrupted.last_mut().unwrap() ^= 0xff;

    let before = receiver.metrics();
    let first = receiver.ingest(&corrupted, 101, InterfaceId(7), &mut rng);
    assert_eq!(first.disposition, IngressDisposition::NoObservableOutcome);
    assert!(first.actions.is_empty());
    let after = receiver.metrics();
    assert_eq!(
        after.transport.packets_received,
        before.transport.packets_received + 1
    );
    assert_eq!(after.ingress.seen, before.ingress.seen + 1);
    assert_eq!(after.ingress.admitted, before.ingress.admitted + 1);
    assert_eq!(
        after.ingress.native_no_outcome,
        before.ingress.native_no_outcome + 1
    );
    assert_eq!(
        after.transport.packets_dropped_invalid,
        before.transport.packets_dropped_invalid
    );
    assert_eq!(
        after.transport.crypto_failures,
        before.transport.crypto_failures
    );
    assert_eq!(after.ingress.rejected, before.ingress.rejected);
    assert_eq!(after.ingress.native_invalid, before.ingress.native_invalid);
    assert_eq!(
        after.ingress.retained_proof_invariant,
        before.ingress.retained_proof_invariant
    );

    let replay = receiver.ingest(&corrupted, 102, InterfaceId(7), &mut rng);
    assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
    assert!(replay.actions.is_empty());
}

#[test]
fn retained_policy_is_isolated_to_the_selected_additional_destination() {
    let receiver_identity = identity(96);
    let mut receiver = TestNode::new(
        identity(96),
        "reticulum",
        &["primary"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    let primary = receiver.destination_hash();
    let additional = receiver
        .register_destination(
            "lxmf",
            &["delivery"],
            DestinationType::Single,
            Direction::In,
        )
        .unwrap();
    receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
    receiver
        .set_destination_inbound_proof_policy(&additional, InboundProofPolicy::Retain)
        .unwrap();
    assert_eq!(
        receiver.set_destination_inbound_proof_policy(
            &DestHash::from([0xee; rete_core::TRUNCATED_HASH_LEN]),
            InboundProofPolicy::Retain,
        ),
        Err(InboundProofPolicyError::DestinationNotRegistered)
    );

    let mut sender = node(97);
    sender
        .register_peer(&receiver_identity, "reticulum", &["primary"], 100)
        .unwrap();
    sender
        .register_peer(&receiver_identity, "lxmf", &["delivery"], 100)
        .unwrap();
    let mut rng = CounterRng::default();
    let mut raw = [0u8; RNS_MTU];

    let prepared = sender
        .prepare_data_into(&primary, b"primary immediate", 101, &mut rng, &mut raw)
        .unwrap();
    let primary_report = receiver.ingest(
        &raw[..usize::from(prepared.packet_len())],
        101,
        InterfaceId(8),
        &mut rng,
    );
    assert_eq!(primary_report.actions.packets.len(), 1);
    assert_eq!(primary_report.actions.retained_proof_count(), 0);
    assert!(matches!(
        primary_report.actions.events.as_slice(),
        [ApplicationEvent::DataReceived { destination, .. }]
            if *destination == *primary.as_bytes()
    ));

    let prepared = sender
        .prepare_data_into(&additional, b"additional retained", 102, &mut rng, &mut raw)
        .unwrap();
    let mut additional_report = receiver.ingest(
        &raw[..usize::from(prepared.packet_len())],
        102,
        InterfaceId(9),
        &mut rng,
    );
    assert!(additional_report.actions.packets.is_empty());
    let [ApplicationEvent::DataReceived { destination, .. }] =
        additional_report.actions.events.as_slice()
    else {
        panic!("the selected additional destination must retain its proof")
    };
    assert_eq!(*destination, *additional.as_bytes());
    let retained_proof = additional_report
        .actions
        .events
        .retained_proof
        .take()
        .unwrap();
    assert_eq!(retained_proof.event_index(), 0);
    assert_eq!(
        retained_proof.into_parts().1.into_packet().target(),
        TxTarget::Only(InterfaceId(9))
    );
}

#[test]
fn retained_policy_accepts_only_registered_inbound_single_destinations() {
    let mut receiver = node(99);
    let plain = receiver
        .register_destination(
            "reticulum",
            &["plain"],
            DestinationType::Plain,
            Direction::In,
        )
        .unwrap();
    let outbound = receiver
        .register_destination(
            "reticulum",
            &["outbound"],
            DestinationType::Single,
            Direction::Out,
        )
        .unwrap();

    for destination in [plain, outbound] {
        assert_eq!(
            receiver
                .set_destination_inbound_proof_policy(&destination, InboundProofPolicy::Retain,),
            Err(InboundProofPolicyError::RetainRequiresInboundSingle)
        );
    }
    assert_eq!(
        receiver.set_destination_inbound_proof_policy(
            &DestHash::from([0xee; rete_core::TRUNCATED_HASH_LEN]),
            InboundProofPolicy::Retain,
        ),
        Err(InboundProofPolicyError::DestinationNotRegistered)
    );
    assert_eq!(
        receiver
            .core
            .get_destination(&plain)
            .unwrap()
            .proof_strategy,
        rete_stack::ProofStrategy::ProveNone
    );
    assert_eq!(
        receiver
            .core
            .get_destination(&outbound)
            .unwrap()
            .proof_strategy,
        rete_stack::ProofStrategy::ProveNone
    );
}

#[test]
fn retained_preflight_is_only_direct_header1_single_data_and_marker_is_bounded() {
    let mut receiver = node(100);
    receiver.set_inbound_proof_policy(InboundProofPolicy::Retain);
    let destination = receiver.destination_hash();
    assert_eq!(
        receiver
            .core
            .get_destination(&destination)
            .unwrap()
            .proof_strategy,
        rete_stack::ProofStrategy::ProveApp
    );

    let mut raw = [0_u8; RNS_MTU];
    let direct_len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Single)
        .destination_hash(destination.as_ref())
        .context(CONTEXT_NONE)
        .payload(b"direct shape")
        .build()
        .unwrap();
    let direct = receiver
        .preflight_ingest(
            &raw[..direct_len],
            InterfaceId(1),
            IngressOrigin::RemoteInterface,
        )
        .unwrap();
    assert!(direct.proof_expectation.is_some());

    let mut raw = [0_u8; RNS_MTU];
    let header2_len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Single)
        .destination_hash(destination.as_ref())
        .context(CONTEXT_NONE)
        .payload(b"relayed shape")
        .via(Some(receiver.identity_hash().as_bytes()))
        .build()
        .unwrap();
    let header2 = receiver
        .preflight_ingest(
            &raw[..header2_len],
            InterfaceId(1),
            IngressOrigin::RemoteInterface,
        )
        .unwrap();
    assert!(header2.proof_expectation.is_none());

    let mut raw = [0_u8; RNS_MTU];
    let plain_len = PacketBuilder::new(&mut raw)
        .packet_type(PacketType::Data)
        .dest_type(DestType::Plain)
        .destination_hash(destination.as_ref())
        .context(CONTEXT_NONE)
        .payload(b"wrong destination type")
        .build()
        .unwrap();
    let plain = receiver
        .preflight_ingest(
            &raw[..plain_len],
            InterfaceId(1),
            IngressOrigin::RemoteInterface,
        )
        .unwrap();
    assert!(plain.proof_expectation.is_none());
}

#[test]
fn retained_proof_interception_fails_closed_on_missing_malformed_or_multiple_actions() {
    type NativeStorage = rete_transport::HeaplessStorage<4, 2, 8, 2>;
    let packet_hash = [0xa5; 32];
    let expected = RetainedDestinationProofExpectation {
        destination: DestHash::from([0x42; rete_core::TRUNCATED_HASH_LEN]),
        packet_hash,
    };
    let proof_identity = identity(98);
    let wrong_hash = [0x5a; 32];
    let exact_bytes = rete_transport::Transport::<NativeStorage>::build_proof_packet(
        &proof_identity,
        &packet_hash,
    )
    .unwrap();
    let event = || NativeNodeEvent::DataReceived {
        dest_hash: expected.destination,
        payload: b"accepted plaintext".to_vec(),
    };
    let exact = || OutboundPacket::new(exact_bytes.clone(), PacketRouting::SourceInterface);

    let mut suppressed_duplicate_or_invalid = IngestOutcome {
        events: Vec::new(),
        packets: vec![
            exact(),
            OutboundPacket::new(
                rete_transport::Transport::<NativeStorage>::build_proof_packet(
                    &proof_identity,
                    &wrong_hash,
                )
                .unwrap(),
                PacketRouting::SourceInterface,
            ),
            OutboundPacket::new(vec![0xff], PacketRouting::SourceInterface),
            OutboundPacket::new(exact_bytes.clone(), PacketRouting::All),
            OutboundPacket::new(exact_bytes.clone(), PacketRouting::AllExceptSource),
            OutboundPacket::new(exact_bytes.clone(), PacketRouting::BoundInterface(7)),
            OutboundPacket::new(exact_bytes.clone(), PacketRouting::ExactInterface(7)),
        ],
        rejection: None,
    };
    suppress_inbound_proof_actions(
        &mut suppressed_duplicate_or_invalid,
        Some(&InboundProofExpectation::DestinationData(expected)),
        &proof_identity,
    );
    assert!(suppressed_duplicate_or_invalid.packets.is_empty());

    let mut no_data_event = IngestOutcome {
        events: Vec::new(),
        packets: vec![exact()],
        rejection: None,
    };
    assert_eq!(
        intercept_inbound_proof_actions::<4, 2, 8, 2>(
            &mut no_data_event,
            Some(InboundProofExpectation::DestinationData(expected)),
            InterfaceId(0),
            &proof_identity,
        )
        .unwrap_err(),
        RetainedProofInvariant::DataEventCount { actual: 0 }
    );

    let mut no_native_outcome = IngestOutcome {
        events: Vec::new(),
        packets: Vec::new(),
        rejection: None,
    };
    assert!(
        intercept_inbound_proof_actions::<4, 2, 8, 2>(
            &mut no_native_outcome,
            Some(InboundProofExpectation::DestinationData(expected)),
            InterfaceId(0),
            &proof_identity,
        )
        .unwrap()
        .is_none()
    );

    let mut multiple_data_events = IngestOutcome {
        events: vec![event(), event()],
        packets: vec![exact()],
        rejection: None,
    };
    assert_eq!(
        intercept_inbound_proof_actions::<4, 2, 8, 2>(
            &mut multiple_data_events,
            Some(InboundProofExpectation::DestinationData(expected)),
            InterfaceId(0),
            &proof_identity,
        )
        .unwrap_err(),
        RetainedProofInvariant::DataEventCount { actual: 2 }
    );

    let mut wrong_data_event = IngestOutcome {
        events: vec![NativeNodeEvent::DataReceived {
            dest_hash: DestHash::from([0x24; rete_core::TRUNCATED_HASH_LEN]),
            payload: b"wrong destination".to_vec(),
        }],
        packets: vec![exact()],
        rejection: None,
    };
    assert_eq!(
        intercept_inbound_proof_actions::<4, 2, 8, 2>(
            &mut wrong_data_event,
            Some(InboundProofExpectation::DestinationData(expected)),
            InterfaceId(0),
            &proof_identity,
        )
        .unwrap_err(),
        RetainedProofInvariant::DataEventDestination
    );

    let mut missing = IngestOutcome {
        events: vec![event()],
        packets: Vec::new(),
        rejection: None,
    };
    assert_eq!(
        intercept_inbound_proof_actions::<4, 2, 8, 2>(
            &mut missing,
            Some(InboundProofExpectation::DestinationData(expected)),
            InterfaceId(1),
            &proof_identity,
        )
        .unwrap_err(),
        RetainedProofInvariant::ProofActionCount { actual: 0 }
    );
    assert!(missing.packets.is_empty());

    let mut malformed = IngestOutcome {
        events: vec![event()],
        packets: vec![OutboundPacket::new(
            vec![0xff],
            PacketRouting::SourceInterface,
        )],
        rejection: None,
    };
    assert_eq!(
        intercept_inbound_proof_actions::<4, 2, 8, 2>(
            &mut malformed,
            Some(InboundProofExpectation::DestinationData(expected)),
            InterfaceId(2),
            &proof_identity,
        )
        .unwrap_err(),
        RetainedProofInvariant::MalformedProof
    );
    assert!(malformed.packets.is_empty());

    let mut multiple = IngestOutcome {
        events: vec![event()],
        packets: vec![exact(), exact()],
        rejection: None,
    };
    assert_eq!(
        intercept_inbound_proof_actions::<4, 2, 8, 2>(
            &mut multiple,
            Some(InboundProofExpectation::DestinationData(expected)),
            InterfaceId(3),
            &proof_identity,
        )
        .unwrap_err(),
        RetainedProofInvariant::ProofActionCount { actual: 2 }
    );
    assert!(multiple.packets.is_empty());

    let mut wrong = IngestOutcome {
        events: vec![event()],
        packets: vec![OutboundPacket::new(
            rete_transport::Transport::<NativeStorage>::build_proof_packet(
                &proof_identity,
                &wrong_hash,
            )
            .unwrap(),
            PacketRouting::SourceInterface,
        )],
        rejection: None,
    };
    assert_eq!(
        intercept_inbound_proof_actions::<4, 2, 8, 2>(
            &mut wrong,
            Some(InboundProofExpectation::DestinationData(expected)),
            InterfaceId(4),
            &proof_identity,
        )
        .unwrap_err(),
        RetainedProofInvariant::MismatchedProof
    );
    assert!(wrong.packets.is_empty());

    for routing in [
        PacketRouting::All,
        PacketRouting::AllExceptSource,
        PacketRouting::BoundInterface(4),
        PacketRouting::ExactInterface(4),
    ] {
        let mut wrong_routing = IngestOutcome {
            events: vec![event()],
            packets: vec![OutboundPacket::new(exact_bytes.clone(), routing)],
            rejection: None,
        };
        assert_eq!(
            intercept_inbound_proof_actions::<4, 2, 8, 2>(
                &mut wrong_routing,
                Some(InboundProofExpectation::DestinationData(expected)),
                InterfaceId(4),
                &proof_identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::MismatchedProof
        );
        assert!(wrong_routing.packets.is_empty());
    }

    let mut mixed = IngestOutcome {
        events: vec![event()],
        packets: vec![
            exact(),
            OutboundPacket::new(
                rete_transport::Transport::<NativeStorage>::build_proof_packet(
                    &proof_identity,
                    &wrong_hash,
                )
                .unwrap(),
                PacketRouting::SourceInterface,
            ),
        ],
        rejection: None,
    };
    assert_eq!(
        intercept_inbound_proof_actions::<4, 2, 8, 2>(
            &mut mixed,
            Some(InboundProofExpectation::DestinationData(expected)),
            InterfaceId(5),
            &proof_identity,
        )
        .unwrap_err(),
        RetainedProofInvariant::ProofActionCount { actual: 2 }
    );
    assert!(mixed.packets.is_empty());

    let mut indexed = IngestOutcome {
        events: vec![
            NativeNodeEvent::Tick {
                expired_paths: 0,
                closed_links: 0,
            },
            event(),
        ],
        packets: vec![exact()],
        rejection: None,
    };
    let retained_proof = intercept_inbound_proof_actions::<4, 2, 8, 2>(
        &mut indexed,
        Some(InboundProofExpectation::DestinationData(expected)),
        InterfaceId(6),
        &proof_identity,
    )
    .unwrap()
    .unwrap();
    assert_eq!(retained_proof.event_index(), 1);
    assert!(indexed.packets.is_empty());
}

#[test]
fn retained_proof_classifier_requires_exact_authenticated_wire_shape() {
    type NativeStorage = rete_transport::HeaplessStorage<4, 2, 8, 2>;
    let packet_hash = [0xa5; 32];
    let expected = RetainedDestinationProofExpectation {
        destination: DestHash::from([0x42; rete_core::TRUNCATED_HASH_LEN]),
        packet_hash,
    };
    let proof_identity = identity(98);
    let exact_bytes = rete_transport::Transport::<NativeStorage>::build_proof_packet(
        &proof_identity,
        &packet_hash,
    )
    .unwrap();
    let classify_with_routing = |bytes: Vec<u8>, routing| {
        classify_retained_proof_candidate(
            &OutboundPacket::new(bytes, routing),
            expected,
            &proof_identity,
        )
    };
    let classify = |bytes: Vec<u8>| classify_with_routing(bytes, PacketRouting::SourceInterface);

    assert_eq!(classify(exact_bytes.clone()), RetainedProofCandidate::Exact);

    for routing in [
        PacketRouting::All,
        PacketRouting::AllExceptSource,
        PacketRouting::BoundInterface(7),
        PacketRouting::ExactInterface(7),
    ] {
        assert_eq!(
            classify_with_routing(exact_bytes.clone(), routing),
            RetainedProofCandidate::Mismatched
        );
        assert_eq!(
            classify_with_routing(vec![0xff], routing),
            RetainedProofCandidate::Malformed
        );
    }

    let mut non_proof = exact_bytes.clone();
    non_proof[0] &= !0x03;
    assert_eq!(
        classify_with_routing(non_proof, PacketRouting::All),
        RetainedProofCandidate::Unrelated
    );

    let mut wrong_hops = exact_bytes.clone();
    wrong_hops[1] = 1;
    assert_eq!(classify(wrong_hops), RetainedProofCandidate::Mismatched);

    for bit in [0x20, 0x10, 0x80] {
        let mut wrong_flags = exact_bytes.clone();
        wrong_flags[0] |= bit;
        assert_eq!(classify(wrong_flags), RetainedProofCandidate::Mismatched);
    }

    let mut wrong_context = exact_bytes.clone();
    wrong_context[2 + rete_core::TRUNCATED_HASH_LEN] = 1;
    assert_eq!(classify(wrong_context), RetainedProofCandidate::Mismatched);

    let mut wrong_length = exact_bytes.clone();
    wrong_length.pop();
    assert_eq!(classify(wrong_length), RetainedProofCandidate::Mismatched);

    let mut wrong_destination = exact_bytes.clone();
    wrong_destination[2] ^= 0xff;
    assert_eq!(
        classify(wrong_destination),
        RetainedProofCandidate::Mismatched
    );

    let payload_offset = 2 + rete_core::TRUNCATED_HASH_LEN + 1;
    let mut wrong_covered_hash = exact_bytes.clone();
    wrong_covered_hash[payload_offset] ^= 0xff;
    assert_eq!(
        classify(wrong_covered_hash),
        RetainedProofCandidate::Mismatched
    );

    let mut wrong_signature = exact_bytes.clone();
    *wrong_signature.last_mut().unwrap() ^= 0xff;
    assert_eq!(
        classify(wrong_signature),
        RetainedProofCandidate::Mismatched
    );

    let wrong_signer =
        rete_transport::Transport::<NativeStorage>::build_proof_packet(&identity(99), &packet_hash)
            .unwrap();
    assert_eq!(classify(wrong_signer), RetainedProofCandidate::Mismatched);

    let mut token_node = node(97);
    let token_peer = identity(99);
    let token_destination = node(99).destination_hash();
    token_node
        .register_peer(&token_peer, "reticulum", &["embedded"], 100)
        .unwrap();
    let (mut tokenized, _) = try_initiate_heapless_link_at(
        &mut token_node.core,
        token_destination,
        100,
        MonotonicInstant::from_secs(100),
        &mut CounterRng::default(),
    )
    .unwrap();
    assert!(tokenized.protocol_token().is_some());
    tokenized.data = exact_bytes;
    tokenized.routing = PacketRouting::SourceInterface;
    assert_eq!(
        classify_retained_proof_candidate(&tokenized, expected, &proof_identity),
        RetainedProofCandidate::Mismatched
    );
}

#[test]
fn responder_link_binding_consumes_every_native_proof_routing_fail_closed() {
    type NativeStorage = rete_transport::HeaplessStorage<4, 2, 8, 2>;
    let identity = identity(77);
    let link_id = LinkId::from([0x42; rete_core::TRUNCATED_HASH_LEN]);
    let packet_hash = [0xa5; 32];
    let interface = InterfaceId(7);
    let exact_bytes = rete_transport::Transport::<NativeStorage>::build_link_proof_packet(
        &identity,
        &packet_hash,
        &link_id,
    )
    .unwrap();
    let expectation = || ResponderLinkProofExpectation {
        link_id,
        packet_hash,
        delivery: ResponderLinkProofDelivery::Retain,
    };
    let event = || NativeNodeEvent::LinkData {
        link_id,
        data: b"bound event".to_vec(),
        context: CONTEXT_NONE,
    };

    let mut source_relative = IngestOutcome {
        events: vec![event()],
        packets: vec![OutboundPacket::new(
            exact_bytes.clone(),
            PacketRouting::SourceInterface,
        )],
        rejection: None,
    };
    let retained = bind_responder_link_proof::<4, 2, 8, 2>(
        &mut source_relative,
        expectation(),
        interface,
        &identity,
    )
    .unwrap()
    .unwrap();
    assert_eq!(retained.event_index(), 0);
    assert!(source_relative.packets.is_empty());

    for routing in [
        PacketRouting::BoundInterface(interface.0),
        PacketRouting::ExactInterface(interface.0),
        PacketRouting::All,
    ] {
        let mut wrong_routing = IngestOutcome {
            events: vec![event()],
            packets: vec![OutboundPacket::new(exact_bytes.clone(), routing)],
            rejection: None,
        };
        assert_eq!(
            bind_responder_link_proof::<4, 2, 8, 2>(
                &mut wrong_routing,
                expectation(),
                interface,
                &identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::MismatchedProof
        );
        assert!(wrong_routing.packets.is_empty());
    }

    let mut malformed = IngestOutcome {
        events: vec![event()],
        packets: vec![OutboundPacket::new(
            vec![0xff],
            PacketRouting::BoundInterface(interface.0),
        )],
        rejection: None,
    };
    assert_eq!(
        bind_responder_link_proof::<4, 2, 8, 2>(
            &mut malformed,
            expectation(),
            interface,
            &identity,
        )
        .unwrap_err(),
        RetainedProofInvariant::MismatchedProof
    );
    assert!(malformed.packets.is_empty());
}

#[test]
fn retained_proof_invariant_rejection_publishes_no_event_or_packet_owner() {
    let mut receiver = node(101);
    let destination = receiver.destination_hash();
    let report = receiver.finish_ingest(
        IngestOutcome {
            events: vec![NativeNodeEvent::DataReceived {
                dest_hash: destination,
                payload: b"must not publish".to_vec(),
            }],
            packets: Vec::new(),
            rejection: None,
        },
        IngressPreflight {
            before_duplicate: receiver.core.transport.stats().packets_dropped_dedup,
            before_invalid: receiver.core.transport.stats().packets_dropped_invalid,
            metadata: IngressMetadata::default(),
            proof_expectation: Some(InboundProofExpectation::DestinationData(
                RetainedDestinationProofExpectation {
                    destination,
                    packet_hash: [0x31; 32],
                },
            )),
            local_path_request: None,
            announce_path_before: None,
            derived_broadcast: None,
        },
        InterfaceId(7),
        IngressBroadcastPolicy::default(),
        TerminalCommitCounts::default(),
    );
    assert_eq!(
        report.disposition,
        IngressDisposition::Rejected(IngressDropReason::RetainedProofInvariant(
            RetainedProofInvariant::ProofActionCount { actual: 0 }
        ))
    );
    assert!(report.actions.events.is_empty());
    assert!(report.actions.packets.is_empty());
    assert_eq!(report.actions.retained_proof_count(), 0);
    assert_eq!(receiver.metrics().ingress.retained_proof_invariant, 1);
}

#[test]
fn inbound_proof_policy_defaults_to_never_and_can_be_disabled_again() {
    let mut sender = node(92);
    let mut receiver = node(93);
    let mut rng = CounterRng::default();

    receiver.queue_announce(None, 100, &mut rng).unwrap();
    let announce = receiver
        .flush_announces(100, &mut rng)
        .into_iter()
        .next()
        .expect("the queued announce must be ready immediately");
    let learned = sender.ingest(announce.bytes(), 100, InterfaceId(4), &mut rng);
    assert_eq!(learned.disposition, IngressDisposition::Processed);

    let mut data = [0u8; RNS_MTU];
    let first = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"default never",
            101,
            &mut rng,
            &mut data,
        )
        .unwrap();
    let received = receiver.ingest(
        &data[..usize::from(first.packet_len())],
        101,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(received.disposition, IngressDisposition::Processed);
    assert!(received.actions.packets.is_empty());

    receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
    let second = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"always",
            102,
            &mut rng,
            &mut data,
        )
        .unwrap();
    let received = receiver.ingest(
        &data[..usize::from(second.packet_len())],
        102,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(received.actions.packets.len(), 1);

    receiver.set_inbound_proof_policy(InboundProofPolicy::Never);
    let third = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"disabled again",
            103,
            &mut rng,
            &mut data,
        )
        .unwrap();
    let received = receiver.ingest(
        &data[..usize::from(third.packet_len())],
        103,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(received.disposition, IngressDisposition::Processed);
    assert!(received.actions.packets.is_empty());
}

#[test]
fn receipt_timeout_defers_until_product_sink_can_reserve() {
    let mut sender = node(86);
    let receiver = node(87);
    sender
        .register_peer(&identity(87), "reticulum", &["embedded"], 100)
        .unwrap();
    let mut rng = CounterRng::default();
    let mut output = [0u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"eventual timeout",
            100,
            &mut rng,
            &mut output,
        )
        .unwrap();
    let expected = ReceiptCandidate {
        kind: ReceiptKind::Data,
        receipt: prepared.receipt(),
        ingress: None,
    };
    let mut sink = RecordingReceiptSink {
        refuse: true,
        ..RecordingReceiptSink::default()
    };

    let deferred = sender.tick_with_receipt_sink(131, &mut rng, &mut sink);
    assert_eq!(deferred.timed_out_receipts, 0);
    assert_eq!(deferred.timed_out_link_data_receipts, 0);
    assert_eq!(deferred.timed_out_receipt_tag, None);
    assert!(deferred.timed_out_receipt_tags_consistent);
    assert!(deferred.receipt_terminals_deferred);
    assert_eq!(sink.attempted, [expected]);
    assert!(sink.terminals.is_empty());
    assert_eq!(sender.metrics().capacity.receipts.used, 1);
    assert_eq!(
        sender.metrics().receipt_terminals.reservation_backpressure,
        1
    );

    sink.refuse = false;
    let completed = sender.tick_with_receipt_sink(132, &mut rng, &mut sink);
    assert_eq!(completed.timed_out_receipts, 1);
    assert_eq!(completed.timed_out_link_data_receipts, 0);
    assert_eq!(
        completed.timed_out_receipt_tag,
        Some(correlation_tag(prepared.receipt().as_bytes()))
    );
    assert!(completed.timed_out_receipt_tags_consistent);
    assert!(!completed.receipt_terminals_deferred);
    assert!(
        completed
            .actions
            .events
            .iter()
            .all(|event| !matches!(event, ApplicationEvent::ReceiptFailed { .. }))
    );
    assert_eq!(sink.attempted, [expected, expected]);
    assert_eq!(sink.terminals, [ReceiptTerminal::TimedOut(expected)]);
    assert_eq!(sink.active_reservations, 0);
    assert_eq!(sender.metrics().capacity.receipts.used, 0);
}

#[test]
fn rearmed_data_receipt_gets_a_full_native_timeout_budget() {
    let mut sender = node(106);
    let receiver = node(107);
    sender
        .register_peer(&identity(107), "reticulum", &["embedded"], 100)
        .unwrap();
    let mut rng = CounterRng::default();
    let mut output = [0u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"queue time must not spend the proof window",
            100,
            &mut rng,
            &mut output,
        )
        .unwrap();

    assert!(sender.park_data_receipt(prepared, 100));
    assert_eq!(sender.metrics().capacity.receipts.used, 1);

    let mut sink = RecordingReceiptSink::default();
    let old_deadline = sender.tick_with_receipt_sink(140, &mut rng, &mut sink);
    assert_eq!(old_deadline.timed_out_receipts, 0);
    assert!(sink.terminals.is_empty());
    assert_eq!(sender.metrics().capacity.receipts.used, 1);

    assert!(sender.rearm_data_receipt(prepared, 145));

    let exact_deadline = sender.tick_with_receipt_sink(175, &mut rng, &mut sink);
    assert_eq!(exact_deadline.timed_out_receipts, 0);
    assert!(sink.terminals.is_empty());

    let expired = sender.tick_with_receipt_sink(176, &mut rng, &mut sink);
    assert_eq!(expired.timed_out_receipts, 1);
    assert!(matches!(
        sink.terminals.as_slice(),
        [ReceiptTerminal::TimedOut(candidate)]
            if candidate.receipt() == prepared.receipt()
    ));
    assert_eq!(sender.metrics().capacity.receipts.used, 0);
    assert!(!sender.rearm_data_receipt(prepared, 177));
}

#[test]
fn receipt_timeout_tags_report_a_mixed_maintenance_batch() {
    let mut sender = node(88);
    let receiver = node(89);
    sender
        .register_peer(&identity(89), "reticulum", &["embedded"], 100)
        .unwrap();
    let mut rng = CounterRng::default();
    let mut output = [0u8; RNS_MTU];
    let first = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"first timeout",
            100,
            &mut rng,
            &mut output,
        )
        .unwrap();
    let second = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"second timeout",
            100,
            &mut rng,
            &mut output,
        )
        .unwrap();
    let first_tag = correlation_tag(first.receipt().as_bytes());
    let second_tag = correlation_tag(second.receipt().as_bytes());
    assert_ne!(first_tag, second_tag);

    let mut sink = RecordingReceiptSink::default();
    let completed = sender.tick_with_receipt_sink(131, &mut rng, &mut sink);

    assert_eq!(completed.timed_out_receipts, 2);
    assert_eq!(completed.timed_out_link_data_receipts, 0);
    assert!(matches!(
        completed.timed_out_receipt_tag,
        Some(tag) if tag == first_tag || tag == second_tag
    ));
    assert!(!completed.timed_out_receipt_tags_consistent);
    assert!(!completed.receipt_terminals_deferred);
    assert_eq!(sink.terminals.len(), 2);
    assert_eq!(sender.metrics().capacity.receipts.used, 0);
}

#[test]
fn receipt_sink_maps_channel_proof_candidate_without_native_types() {
    let mut initiator = node(1);
    let mut responder = node(2);
    let mut rng = CounterRng::default();
    let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
    let response = responder.ingest(request.bytes(), 100, InterfaceId(1), &mut rng);
    let established = initiator.ingest(
        response.actions.packets[0].bytes(),
        101,
        InterfaceId(2),
        &mut rng,
    );
    responder.ingest(
        established.actions.packets[0].bytes(),
        102,
        InterfaceId(1),
        &mut rng,
    );

    let message = initiator
        .send_channel_message(&link_id, 7, b"bounded channel proof", 110, &mut rng)
        .unwrap();
    assert_eq!(
        established.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(2))
    );
    assert_eq!(message.target(), TxTarget::Only(InterfaceId(2)));
    let initial_receipt = ReceiptId(Packet::parse(message.bytes()).unwrap().compute_hash());
    let initial_proof_actions = responder.ingest(message.bytes(), 110, InterfaceId(1), &mut rng);
    let initial_generated_tag = initial_proof_actions
        .metadata
        .generated_proof_tag()
        .expect("channel PROOF carries the explicit covered packet hash");
    assert_eq!(initial_proof_actions.metadata.generated_proof_actions(), 1);
    let initial_proof = initial_proof_actions
        .actions
        .packets
        .iter()
        .find(|packet| {
            Packet::parse(packet.bytes())
                .is_ok_and(|packet| packet.packet_type == PacketType::Proof)
        })
        .unwrap();
    assert_eq!(initial_proof.target(), TxTarget::Only(InterfaceId(1)));
    assert_eq!(initiator.metrics().capacity.channel_receipts.used, 1);

    // Deliberately lose the initial proof. A retry uses fresh ciphertext
    // and atomically replaces the sole retained receipt/proof target.
    let retry_actions = initiator.tick(126, &mut rng);
    assert_eq!(retry_actions.packets.len(), 1);
    assert_eq!(retry_actions.unroutable_packets, 0);
    let retry = &retry_actions.packets[0];
    assert_eq!(retry.target(), TxTarget::Only(InterfaceId(2)));
    let retry_receipt = ReceiptId(Packet::parse(retry.bytes()).unwrap().compute_hash());
    assert_ne!(retry_receipt, initial_receipt);
    assert_eq!(initiator.metrics().capacity.channel_receipts.used, 1);

    let retry_proof_actions = responder.ingest(retry.bytes(), 126, InterfaceId(1), &mut rng);
    assert!(retry_proof_actions.actions.events.is_empty());
    assert_eq!(retry_proof_actions.metadata.generated_proof_actions(), 1);
    let retry_generated_tag = retry_proof_actions
        .metadata
        .generated_proof_tag()
        .expect("replacement channel PROOF carries the retry packet hash");
    assert_ne!(retry_generated_tag, initial_generated_tag);
    let retry_proof = retry_proof_actions
        .actions
        .packets
        .iter()
        .find(|packet| {
            Packet::parse(packet.bytes())
                .is_ok_and(|packet| packet.packet_type == PacketType::Proof)
        })
        .unwrap();
    assert_eq!(retry_proof.target(), TxTarget::Only(InterfaceId(1)));

    let mut sink = RecordingReceiptSink::default();

    let obsolete = initiator
        .ingest_with_receipt_sink(
            initial_proof.bytes(),
            127,
            InterfaceId(2),
            &mut rng,
            &mut sink,
        )
        .unwrap();
    let expected = ReceiptCandidate {
        kind: ReceiptKind::Channel,
        receipt: retry_receipt,
        ingress: Some(IngressObservation::remote(InterfaceId(2), None)),
    };
    assert_eq!(
        obsolete.disposition,
        IngressDisposition::NoObservableOutcome
    );
    assert_eq!(obsolete.metadata.delivered_receipt_terminals(), 0);
    assert!(sink.attempted.is_empty());
    assert!(sink.terminals.is_empty());
    assert_eq!(initiator.metrics().capacity.channel_receipts.used, 1);

    let report = initiator
        .ingest_with_receipt_sink(
            retry_proof.bytes(),
            128,
            InterfaceId(2),
            &mut rng,
            &mut sink,
        )
        .unwrap();
    assert_eq!(report.disposition, IngressDisposition::Processed);
    assert_eq!(report.metadata.delivered_receipt_terminals(), 1);
    assert_eq!(report.metadata.timed_out_receipt_terminals(), 0);
    assert_eq!(
        report.metadata.delivered_receipt_tag(),
        Some(retry_generated_tag)
    );
    assert_eq!(sink.attempted, [expected]);
    assert_eq!(sink.terminals, [ReceiptTerminal::Delivered(expected)]);
    assert_eq!(initiator.metrics().capacity.channel_receipts.used, 0);
}

#[test]
fn caller_owned_data_preflight_rejects_before_entropy_or_receipt_mutation() {
    let mut sender = node(82);
    let receiver = node(83);
    sender
        .register_peer(&identity(83), "reticulum", &["embedded"], 0)
        .unwrap();
    let mut rng = CounterRng::default();
    let rng_before = rng.0;
    let mut output = [0xA5; RNS_MTU];
    let oversized = [0x5A; MAX_DATA_PAYLOAD + 1];

    assert_eq!(
        sender.prepare_data_into(
            &receiver.destination_hash(),
            &oversized,
            1,
            &mut rng,
            &mut output,
        ),
        Err(PrepareDataError::PayloadTooLarge {
            actual: MAX_DATA_PAYLOAD + 1,
            maximum: MAX_DATA_PAYLOAD,
        })
    );
    assert_eq!(rng.0, rng_before);
    assert!(output.iter().all(|byte| *byte == 0xA5));
    assert_eq!(sender.metrics().capacity.receipts.used, 0);
}

#[test]
fn link_request_policy_rejects_native_loose_forms_before_state_mutation() {
    let mut initiator = node(50);
    let mut responder = node(51);
    let responder_identity = identity(51);
    initiator
        .register_peer(&responder_identity, "reticulum", &["embedded"], 0)
        .unwrap();
    let mut rng = CounterRng::default();
    let (valid, _) = initiator
        .initiate_link(responder.destination_hash(), 1, &mut rng)
        .unwrap();

    assert!(responder.set_accepts_links(&responder.destination_hash(), false));
    let disabled = responder.ingest(&valid.bytes, 1, InterfaceId(1), &mut rng);
    assert_eq!(
        disabled.disposition,
        IngressDisposition::Rejected(IngressDropReason::DestinationDoesNotAcceptLinks)
    );
    assert_eq!(responder.metrics().capacity.links.used, 0);
    assert!(responder.set_accepts_links(&responder.destination_hash(), true));

    let mut loose = [0u8; rete_core::MTU];
    let loose_len = PacketBuilder::new(&mut loose)
        .packet_type(PacketType::LinkRequest)
        .dest_type(DestType::Single)
        .destination_hash(responder.destination_hash().as_ref())
        .context(0)
        .payload(&[0xA5; 65])
        .build()
        .unwrap();
    let rejected = responder.ingest(&loose[..loose_len], 2, InterfaceId(1), &mut rng);
    assert_eq!(
        rejected.disposition,
        IngressDisposition::Rejected(IngressDropReason::LinkRequestPayloadLength(65))
    );
    assert_eq!(responder.metrics().capacity.links.used, 0);
}

#[test]
fn announce_queue_is_owned_and_bounded() {
    let mut node = node(60);
    let mut rng = CounterRng::default();
    let primary = node.destination_hash();
    let expected_public_key = identity(60).public_key();
    let delivery = node
        .register_destination(
            "lxmf",
            &["delivery"],
            DestinationType::Single,
            Direction::In,
        )
        .unwrap();
    node.queue_announce(None, 1, &mut rng).unwrap();
    node.queue_announce_for(&delivery, Some(b"bounded app data"), 2, &mut rng)
        .unwrap();
    assert_eq!(node.metrics().capacity.announces.used, 2);
    assert_eq!(
        node.queue_announce(None, 1, &mut rng),
        Err(AnnounceAdmissionError::QueueFull { limit: 2 })
    );
    assert_eq!(node.metrics().admission.announce_queue_full, 1);

    let packets = node.flush_announces(1, &mut rng);
    assert_eq!(packets.len(), 2);
    assert!(packets.iter().all(|packet| packet.target == TxTarget::All));
    let primary_announce = packets
        .iter()
        .find_map(|packet| {
            let announce = crate::parse_announce_packet(packet.bytes()).ok()?;
            (announce.packet.destination_hash == primary.as_bytes()).then_some(announce)
        })
        .expect("primary destination announce must be present and valid");
    assert_eq!(primary_announce.fields.pub_key, expected_public_key);
    assert_eq!(primary_announce.fields.app_data, None);
    let delivery_announce = packets
        .iter()
        .find_map(|packet| {
            let announce = crate::parse_announce_packet(packet.bytes()).ok()?;
            (announce.packet.destination_hash == delivery.as_bytes()).then_some(announce)
        })
        .expect("LXMF delivery announce must be present and valid");
    assert_eq!(delivery_announce.fields.pub_key, expected_public_key);
    assert_eq!(
        delivery_announce.fields.app_data,
        Some(b"bounded app data".as_slice())
    );
}

#[test]
fn local_announce_target_applies_only_to_destination_and_native_retransmit() {
    let mut node = node(61);
    let mut rng = CounterRng::default();
    let delivery = node
        .register_destination(
            "rnstransport",
            &["discovery", "interface"],
            DestinationType::Single,
            Direction::In,
        )
        .unwrap();
    assert_eq!(
        node.set_local_announce_interface_target(&DestHash::from([0xAA; 16]), InterfaceId(2)),
        Err(LocalAnnounceTargetError::DestinationNotRegistered)
    );
    node.set_local_announce_interface_target(&delivery, InterfaceId(2))
        .unwrap();

    node.queue_announce(None, 1, &mut rng).unwrap();
    node.queue_announce_for(&delivery, Some(b"rmap"), 1, &mut rng)
        .unwrap();
    let initial = node.flush_announces(1, &mut rng);
    let primary = node.destination_hash();
    assert_eq!(initial.len(), 2);
    assert!(initial.iter().any(|packet| {
        packet.target() == TxTarget::All
            && Packet::parse(packet.bytes())
                .is_ok_and(|parsed| parsed.destination_hash == primary.as_ref())
    }));
    assert!(initial.iter().any(|packet| {
        packet.target() == TxTarget::Only(InterfaceId(2))
            && Packet::parse(packet.bytes())
                .is_ok_and(|parsed| parsed.destination_hash == delivery.as_ref())
    }));

    let retransmits = node.flush_announces(20, &mut rng);
    assert!(retransmits.iter().any(|packet| {
        packet.target() == TxTarget::Only(InterfaceId(2))
            && Packet::parse(packet.bytes())
                .is_ok_and(|parsed| parsed.destination_hash == delivery.as_ref())
    }));
}

#[test]
fn point_to_point_announce_retargets_only_its_forward_and_drops_only_its_retry() {
    let mut relay = TestNode::new(
        identity(63),
        "reticulum",
        &["relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let relay_destination = relay.destination_hash();
    let mut peer = node(64);
    let peer_destination = peer.destination_hash();
    let mut rng = CounterRng::default();

    relay.queue_announce(None, 100, &mut rng).unwrap();
    peer.queue_announce(None, 100, &mut rng).unwrap();
    let inbound = peer.flush_announces(100, &mut rng).remove(0);
    let inbound_wire_hash: [u8; 32] = Sha256::digest(inbound.bytes()).into();

    let report = relay.ingest_at_with_broadcast_scope(
        inbound.bytes(),
        100,
        MonotonicInstant::from_secs(100),
        InterfaceId(7),
        IngressBroadcastScope::PointToPoint,
        &mut rng,
    );
    assert_eq!(report.disposition, IngressDisposition::Processed);
    assert_eq!(report.actions.packets.len(), 2);

    let local = report
        .actions
        .packets
        .iter()
        .find(|packet| {
            Packet::parse(packet.bytes()).is_ok_and(|parsed| {
                parsed.packet_type == PacketType::Announce
                    && parsed.destination_hash == relay_destination.as_ref()
            })
        })
        .expect("the unrelated due local announce remains in the same envelope");
    assert_eq!(local.target(), TxTarget::All);

    let forwarded = report
        .actions
        .packets
        .iter()
        .find(|packet| {
            Packet::parse(packet.bytes()).is_ok_and(|parsed| {
                parsed.packet_type == PacketType::Announce
                    && parsed.destination_hash == peer_destination.as_ref()
            })
        })
        .expect("the received announce is forwarded immediately");
    assert_eq!(forwarded.target(), TxTarget::AllExcept(InterfaceId(7)));
    let forwarded_wire_hash: [u8; 32] = Sha256::digest(forwarded.bytes()).into();
    assert_ne!(
        inbound_wire_hash, forwarded_wire_hash,
        "transport forwarding rebuilds the received announce wire image"
    );

    assert_eq!(
        relay.metrics().capacity.announces.used,
        1,
        "only the unrelated local announce retry remains queued"
    );
    let delayed = relay.tick(106, &mut rng);
    assert_eq!(delayed.packets.len(), 1);
    assert_eq!(delayed.packets[0].target(), TxTarget::All);
    let delayed_packet = Packet::parse(delayed.packets[0].bytes()).unwrap();
    assert_eq!(delayed_packet.destination_hash, relay_destination.as_ref());
}

#[test]
fn explicit_announce_egress_suppresses_only_the_matching_announce_forward() {
    let mut relay = TestNode::new(
        identity(69),
        "reticulum",
        &["boundary-relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let relay_destination = relay.destination_hash();
    let mut peer = node(70);
    let peer_destination = peer.destination_hash();
    let mut rng = CounterRng::default();

    relay.queue_announce(None, 100, &mut rng).unwrap();
    peer.queue_announce(None, 100, &mut rng).unwrap();
    let inbound = peer.flush_announces(100, &mut rng).remove(0);
    let report = relay.ingest_at_with_broadcast_policy(
        inbound.bytes(),
        100,
        MonotonicInstant::from_secs(100),
        InterfaceId(2),
        IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
            .with_announce_egress(IngressEgressSet::empty()),
        &mut rng,
    );

    assert_eq!(report.disposition, IngressDisposition::Processed);
    assert_eq!(report.actions.events.len(), 1);
    assert!(matches!(
        &report.actions.events[0],
        ApplicationEvent::AnnounceReceived { destination, .. }
            if destination == peer_destination.as_bytes()
    ));
    assert!(relay.has_path(&peer_destination));
    assert_eq!(report.actions.packets.len(), 1);
    let local = &report.actions.packets[0];
    assert_eq!(local.target(), TxTarget::All);
    assert_eq!(
        Packet::parse(local.bytes()).unwrap().destination_hash,
        relay_destination.as_ref()
    );
    assert_eq!(
        relay.metrics().capacity.announces.used,
        1,
        "the suppressed nonlocal retry is removed while the unrelated local announce remains"
    );
}

#[test]
fn explicit_announce_egress_retains_exact_selected_interface_ids() {
    let mut relay = TestNode::new(
        identity(71),
        "reticulum",
        &["selected-egress-relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut peer = node(72);
    let mut rng = CounterRng::default();
    peer.queue_announce(None, 100, &mut rng).unwrap();
    let inbound = peer.flush_announces(100, &mut rng).remove(0);
    let selected = IngressEgressSet::from_bits((1 << 3) | (1 << 7));

    let report = relay.ingest_at_with_broadcast_policy(
        inbound.bytes(),
        100,
        MonotonicInstant::from_secs(100),
        InterfaceId(2),
        IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
            .with_announce_egress(selected),
        &mut rng,
    );

    assert_eq!(report.actions.packets.len(), 1);
    assert_eq!(
        report.actions.packets[0].target(),
        TxTarget::Selected(selected)
    );
    assert_eq!(relay.metrics().capacity.announces.used, 0);
}

#[test]
fn shared_medium_announce_keeps_same_interface_forward_and_native_retry() {
    let mut relay = TestNode::new(
        identity(65),
        "reticulum",
        &["relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut peer = node(66);
    let mut rng = CounterRng::default();
    peer.queue_announce(None, 100, &mut rng).unwrap();
    let inbound = peer.flush_announces(100, &mut rng).remove(0);

    let report = relay.ingest_at_with_broadcast_scope(
        inbound.bytes(),
        100,
        MonotonicInstant::from_secs(100),
        InterfaceId(3),
        IngressBroadcastScope::SharedMedium,
        &mut rng,
    );
    assert_eq!(report.actions.packets.len(), 1);
    assert_eq!(report.actions.packets[0].target(), TxTarget::All);
    assert_eq!(relay.metrics().capacity.announces.used, 1);

    let delayed = relay.tick(106, &mut rng);
    assert_eq!(delayed.packets.len(), 1);
    assert_eq!(delayed.packets[0].target(), TxTarget::All);
    assert_eq!(relay.metrics().capacity.announces.used, 0);
}

#[test]
fn recursive_unknown_path_request_excludes_source_and_rebuilds_relay_payload() {
    let mut relay = TestNode::new(
        identity(67),
        "reticulum",
        &["relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let requester = TestNode::new(
        identity(68),
        "reticulum",
        &["requester"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut rng = CounterRng::default();
    let destination = DestHash::from([0xa5; TRUNCATED_HASH_LEN]);
    let request = requester.request_path(&destination, &mut rng).unwrap();
    let source = Packet::parse(request.bytes()).unwrap();
    let source_tag = &source.payload[TRUNCATED_HASH_LEN * 2..];
    let selected = IngressEgressSet::from_bits((1 << 2) | (1 << 9));

    let report = relay.ingest_at_with_broadcast_policy(
        request.bytes(),
        100,
        MonotonicInstant::from_secs(100),
        InterfaceId(9),
        IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
            .with_recursive_path_search_egress(selected),
        &mut rng,
    );
    assert_eq!(report.actions.packets.len(), 1);
    assert_eq!(
        report.actions.packets[0].target(),
        TxTarget::Selected(IngressEgressSet::from_bits(1 << 2))
    );
    let forwarded = Packet::parse(report.actions.packets[0].bytes()).unwrap();
    assert_eq!(
        &forwarded.payload[..TRUNCATED_HASH_LEN],
        destination.as_ref()
    );
    assert_eq!(
        &forwarded.payload[TRUNCATED_HASH_LEN..TRUNCATED_HASH_LEN * 2],
        relay.identity_hash().as_ref()
    );
    assert_eq!(&forwarded.payload[TRUNCATED_HASH_LEN * 2..], source_tag);
    assert_eq!(relay.metrics().pending_discovery_paths.used, 1);
}

#[test]
fn boundary_empty_egress_retains_and_coalesces_global_pending_discovery() {
    let mut relay = TestNode::new(
        identity(73),
        "reticulum",
        &["boundary-relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let requester = TestNode::new(
        identity(74),
        "reticulum",
        &["requester"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut rng = CounterRng::default();
    let destination = DestHash::from([0xb5; TRUNCATED_HASH_LEN]);
    let boundary_request = requester.request_path(&destination, &mut rng).unwrap();

    let suppressed = relay.ingest_at_with_broadcast_policy(
        boundary_request.bytes(),
        100,
        MonotonicInstant::from_secs(100),
        InterfaceId(2),
        IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
            .with_recursive_path_search_egress(IngressEgressSet::empty()),
        &mut rng,
    );

    assert_eq!(suppressed.disposition, IngressDisposition::Processed);
    assert!(suppressed.actions.packets.is_empty());
    let after_suppressed = relay.metrics();
    assert_eq!(after_suppressed.ingress.admitted, 1);
    assert_eq!(
        after_suppressed
            .ingress
            .discovery_path_requests_without_egress,
        1
    );
    assert_eq!(after_suppressed.transport.packets_received, 1);
    assert_eq!(after_suppressed.pending_discovery_paths.used, 1);

    let internal_request = requester.request_path(&destination, &mut rng).unwrap();
    assert_ne!(
        Packet::parse(boundary_request.bytes())
            .unwrap()
            .compute_hash(),
        Packet::parse(internal_request.bytes())
            .unwrap()
            .compute_hash(),
        "the cross-interface request must carry an independent discovery tag"
    );
    let selected = IngressEgressSet::from_bits((1 << 1) | (1 << 2));
    let coalesced = relay.ingest_at_with_broadcast_policy(
        internal_request.bytes(),
        101,
        MonotonicInstant::from_secs(101),
        InterfaceId(1),
        IngressBroadcastPolicy::new(IngressBroadcastScope::SharedMedium)
            .with_recursive_path_search_egress(selected),
        &mut rng,
    );
    assert!(coalesced.actions.packets.is_empty());
    assert_eq!(relay.metrics().ingress.discovery_path_requests_coalesced, 1);
    assert_eq!(relay.metrics().pending_discovery_paths.used, 1);

    let _ = relay.tick(115, &mut rng);
    assert_eq!(relay.metrics().pending_discovery_paths.used, 0);
    assert_eq!(relay.metrics().ingress.discovery_path_requests_expired, 1);

    let after_timeout = requester.request_path(&destination, &mut rng).unwrap();
    let forwarded = relay.ingest_at_with_broadcast_policy(
        after_timeout.bytes(),
        116,
        MonotonicInstant::from_secs(116),
        InterfaceId(1),
        IngressBroadcastPolicy::new(IngressBroadcastScope::SharedMedium)
            .with_recursive_path_search_egress(selected),
        &mut rng,
    );
    assert_eq!(forwarded.actions.packets.len(), 1);
    assert_eq!(
        forwarded.actions.packets[0].target(),
        TxTarget::Selected(IngressEgressSet::from_bits(1 << 2))
    );
}

#[test]
fn internal_recursive_search_never_reflects_onto_shared_ingress() {
    let mut relay = TestNode::new(
        identity(75),
        "reticulum",
        &["internal-relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let requester = TestNode::new(
        identity(76),
        "reticulum",
        &["requester"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut rng = CounterRng::default();
    let request = requester
        .request_path(&DestHash::from([0xc5; TRUNCATED_HASH_LEN]), &mut rng)
        .unwrap();
    let selected = IngressEgressSet::from_bits((1 << 1) | (1 << 2));

    let report = relay.ingest_at_with_broadcast_policy(
        request.bytes(),
        100,
        MonotonicInstant::from_secs(100),
        InterfaceId(1),
        IngressBroadcastPolicy::new(IngressBroadcastScope::SharedMedium)
            .with_recursive_path_search_egress(selected),
        &mut rng,
    );

    assert_eq!(report.actions.packets.len(), 1);
    assert_eq!(
        report.actions.packets[0].target(),
        TxTarget::Selected(IngressEgressSet::from_bits(1 << 2))
    );
}

#[test]
fn boundary_known_path_response_waits_for_grace_and_returns_only_to_source() {
    let mut relay = TestNode::new(
        identity(77),
        "reticulum",
        &["boundary-relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut peer = node(78);
    let peer_destination = peer.destination_hash();
    let relay_identity = relay.identity_hash();
    let requester = TestNode::new(
        identity(79),
        "reticulum",
        &["requester"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut rng = CounterRng::default();

    peer.queue_announce(None, 100, &mut rng).unwrap();
    let announce = peer.flush_announces(100, &mut rng).remove(0);
    let source_announce = Packet::parse(announce.bytes()).unwrap();
    let source_hops = source_announce.hops;
    let source_context_flag = source_announce.context_flag;
    let source_payload = source_announce.payload.to_vec();
    let learned = relay.ingest_at_with_broadcast_policy(
        announce.bytes(),
        100,
        MonotonicInstant::from_secs(100),
        InterfaceId(1),
        IngressBroadcastPolicy::new(IngressBroadcastScope::SharedMedium)
            .with_announce_egress(IngressEgressSet::empty()),
        &mut rng,
    );
    assert!(learned.actions.packets.is_empty());
    assert!(relay.has_path(&peer_destination));

    let request = requester.request_path(&peer_destination, &mut rng).unwrap();
    let response = relay.ingest_at_with_broadcast_policy(
        request.bytes(),
        101,
        MonotonicInstant::from_secs(101),
        InterfaceId(2),
        IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
            .with_announce_egress(IngressEgressSet::empty()),
        &mut rng,
    );
    assert!(response.actions.packets.is_empty());
    assert!(relay.tick(101, &mut rng).packets.is_empty());

    let due = relay.tick(102, &mut rng);
    assert_eq!(due.packets.len(), 1);
    assert_eq!(due.packets[0].target(), TxTarget::Only(InterfaceId(2)));
    let response_announce = Packet::parse(due.packets[0].bytes()).unwrap();
    assert_eq!(response_announce.header_type, HeaderType::Header2);
    assert_eq!(response_announce.packet_type, PacketType::Announce);
    assert_eq!(response_announce.context, CONTEXT_PATH_RESPONSE);
    assert_eq!(
        response_announce.transport_id,
        Some(relay_identity.as_ref())
    );
    assert_eq!(
        response_announce.destination_hash,
        peer_destination.as_ref()
    );
    assert_eq!(response_announce.hops, source_hops.saturating_add(1));
    assert_eq!(response_announce.context_flag, source_context_flag);
    assert_eq!(response_announce.payload, source_payload.as_slice());

    let mut second_relay = TestNode::new(
        identity(80),
        "reticulum",
        &["second-relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let classified = second_relay
        .preflight_ingest(
            due.packets[0].bytes(),
            InterfaceId(3),
            IngressOrigin::RemoteInterface,
        )
        .expect("valid H2 path response passes preflight");
    assert!(matches!(
        classified.derived_broadcast,
        Some(IngressDerivedBroadcast::Announce { destination }) if destination == peer_destination
    ));
    let learned_response = second_relay.ingest_at_with_broadcast_policy(
        due.packets[0].bytes(),
        102,
        MonotonicInstant::from_secs(102),
        InterfaceId(3),
        IngressBroadcastPolicy::new(IngressBroadcastScope::SharedMedium),
        &mut rng,
    );
    assert_eq!(learned_response.disposition, IngressDisposition::Processed);
    assert!(second_relay.has_path(&peer_destination));
    assert_eq!(
        second_relay.route(&peer_destination).unwrap().received_on,
        Some(InterfaceId(3))
    );
    assert!(learned_response.actions.packets.is_empty());
    assert_eq!(second_relay.metrics().capacity.announces.used, 0);
    assert!(second_relay.tick(108, &mut rng).packets.is_empty());

    let second_request = requester.request_path(&peer_destination, &mut rng).unwrap();
    let second_response = relay.ingest_at_with_broadcast_policy(
        second_request.bytes(),
        103,
        MonotonicInstant::from_secs(103),
        InterfaceId(3),
        IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
            .with_announce_egress(IngressEgressSet::empty()),
        &mut rng,
    );
    assert!(second_response.actions.packets.is_empty());
    assert!(relay.tick(103, &mut rng).packets.is_empty());
    let second_due = relay.tick(104, &mut rng);
    assert_eq!(second_due.packets.len(), 1);
    assert_eq!(
        second_due.packets[0].target(),
        TxTarget::Only(InterfaceId(3))
    );
    assert_eq!(
        Packet::parse(second_due.packets[0].bytes())
            .unwrap()
            .context,
        CONTEXT_PATH_RESPONSE
    );
    assert!(relay.tick(110, &mut rng).packets.is_empty());
}

#[test]
fn recursive_path_response_returns_to_exact_requester_and_never_rebroadcasts() {
    let mut relay = TestNode::new(
        identity(81),
        "reticulum",
        &["recursive-relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let relay_identity = relay.identity_hash();
    let requester = TestNode::new(
        identity(82),
        "reticulum",
        &["recursive-requester"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut destination = node(83);
    let destination_hash = destination.destination_hash();
    let mut rng = CounterRng::default();

    let request = requester.request_path(&destination_hash, &mut rng).unwrap();
    let request_packet = Packet::parse(request.bytes()).unwrap();
    let original_tag = request_packet.payload[TRUNCATED_HASH_LEN * 2..].to_vec();
    let recursive = relay.ingest_at_with_broadcast_policy(
        request.bytes(),
        100,
        MonotonicInstant::from_secs(100),
        InterfaceId(1),
        IngressBroadcastPolicy::new(IngressBroadcastScope::SharedMedium)
            .with_recursive_path_search_egress(IngressEgressSet::from_bits(1 << 2)),
        &mut rng,
    );
    assert_eq!(recursive.actions.packets.len(), 1);
    assert_eq!(
        recursive.actions.packets[0].target(),
        TxTarget::Selected(IngressEgressSet::from_bits(1 << 2))
    );
    let forwarded = Packet::parse(recursive.actions.packets[0].bytes()).unwrap();
    assert_eq!(
        &forwarded.payload[TRUNCATED_HASH_LEN..TRUNCATED_HASH_LEN * 2],
        relay_identity.as_ref()
    );
    assert_eq!(&forwarded.payload[TRUNCATED_HASH_LEN * 2..], original_tag);
    assert_eq!(relay.metrics().pending_discovery_paths.used, 1);

    destination.queue_announce(None, 100, &mut rng).unwrap();
    let source = destination.flush_announces(100, &mut rng).remove(0);
    let source = Packet::parse(source.bytes()).unwrap();
    let upstream_transport = IdentityHash::from([0xd2; TRUNCATED_HASH_LEN]);
    let mut upstream_raw = [0_u8; RNS_MTU];
    let upstream_len = PacketBuilder::new(&mut upstream_raw)
        .header_type(HeaderType::Header2)
        .transport_type(rete_core::TRANSPORT_TYPE_TRANSPORT)
        .packet_type(PacketType::Announce)
        .dest_type(DestType::Single)
        .context_flag(source.context_flag)
        .hops(source.hops)
        .transport_id(upstream_transport.as_ref())
        .destination_hash(source.destination_hash)
        .context(CONTEXT_PATH_RESPONSE)
        .payload(source.payload)
        .build()
        .unwrap();

    let returned = relay.ingest_at_with_broadcast_policy(
        &upstream_raw[..upstream_len],
        101,
        MonotonicInstant::from_secs(101),
        InterfaceId(2),
        IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
            .with_announce_egress(IngressEgressSet::empty()),
        &mut rng,
    );
    assert_eq!(returned.actions.packets.len(), 1);
    assert_eq!(
        returned.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(1))
    );
    let response = Packet::parse(returned.actions.packets[0].bytes()).unwrap();
    assert_eq!(response.header_type, HeaderType::Header2);
    assert_eq!(response.context, CONTEXT_PATH_RESPONSE);
    assert_eq!(response.transport_id, Some(relay_identity.as_ref()));
    assert_eq!(response.destination_hash, destination_hash.as_ref());
    assert_eq!(response.context_flag, source.context_flag);
    assert_eq!(response.payload, source.payload);
    assert_eq!(relay.metrics().pending_discovery_paths.used, 1);
    assert_eq!(relay.metrics().ingress.discovery_path_responses, 1);

    let duplicate = relay.ingest_at_with_broadcast_policy(
        &upstream_raw[..upstream_len],
        102,
        MonotonicInstant::from_secs(102),
        InterfaceId(2),
        IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
            .with_announce_egress(IngressEgressSet::empty()),
        &mut rng,
    );
    assert_eq!(duplicate.disposition, IngressDisposition::NativeDuplicate);
    assert!(duplicate.actions.packets.is_empty());
    assert_eq!(relay.metrics().ingress.discovery_path_responses, 1);

    let mut unrelated = TestNode::new(
        identity(84),
        "reticulum",
        &["unrelated-relay"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let learned = unrelated.ingest(
        returned.actions.packets[0].bytes(),
        101,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(learned.disposition, IngressDisposition::Processed);
    assert!(unrelated.has_path(&destination_hash));
    assert!(learned.actions.packets.is_empty());
    assert!(unrelated.tick(108, &mut rng).packets.is_empty());

    assert_eq!(relay.metrics().pending_discovery_paths.used, 1);
    assert!(relay.tick(114, &mut rng).packets.is_empty());
    assert_eq!(relay.metrics().pending_discovery_paths.used, 1);
    assert!(relay.tick(115, &mut rng).packets.is_empty());
    assert_eq!(relay.metrics().pending_discovery_paths.used, 0);
}

#[test]
fn pending_discovery_capacity_fails_closed_before_recursive_forwarding() {
    let mut relay = TestNode::new(
        identity(85),
        "reticulum",
        &["bounded-discovery"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let requester = TestNode::new(
        identity(86),
        "reticulum",
        &["bounded-requester"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut rng = CounterRng::default();
    let policy = IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
        .with_recursive_path_search_egress(IngressEgressSet::empty());

    for tag in 0_u8..4 {
        let destination = DestHash::from([tag.wrapping_add(1); TRUNCATED_HASH_LEN]);
        let request = requester.request_path(&destination, &mut rng).unwrap();
        let admitted = relay.ingest_at_with_broadcast_policy(
            request.bytes(),
            200,
            MonotonicInstant::from_secs(200),
            InterfaceId(2),
            policy,
            &mut rng,
        );
        assert_eq!(admitted.disposition, IngressDisposition::Processed);
        assert!(admitted.actions.packets.is_empty());
    }
    assert_eq!(relay.metrics().pending_discovery_paths.used, 4);

    let destination = DestHash::from([0xf0; TRUNCATED_HASH_LEN]);
    let request = requester.request_path(&destination, &mut rng).unwrap();
    let received_before = relay.metrics().transport.packets_received;
    let rejected = relay.ingest_at_with_broadcast_policy(
        request.bytes(),
        201,
        MonotonicInstant::from_secs(201),
        InterfaceId(2),
        policy,
        &mut rng,
    );
    assert_eq!(
        rejected.disposition,
        IngressDisposition::Rejected(IngressDropReason::DiscoveryPathTableFull { limit: 4 })
    );
    assert!(rejected.actions.packets.is_empty());
    let metrics = relay.metrics();
    assert_eq!(metrics.pending_discovery_paths.used, 4);
    assert_eq!(metrics.ingress.discovery_path_table_full, 1);
    assert_eq!(metrics.transport.packets_received, received_before);

    assert!(relay.tick(215, &mut rng).packets.is_empty());
    assert_eq!(relay.metrics().pending_discovery_paths.used, 0);
}

#[test]
fn every_native_path_request_shape_remains_under_boundary_policy() {
    let mut relay = TestNode::new(
        identity(87),
        "reticulum",
        &["path-shape-policy"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let relay_identity = relay.identity_hash();
    let requester_identity = identity(88).hash();
    let mut rng = CounterRng::default();
    let policy = IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
        .with_recursive_path_search_egress(IngressEgressSet::empty());

    let build = |destination: DestHash,
                 header: HeaderType,
                 transport_id: Option<IdentityHash>,
                 destination_type: DestType,
                 context: u8,
                 hops: u8| {
        let mut payload = [0_u8; TRUNCATED_HASH_LEN * 3];
        payload[..TRUNCATED_HASH_LEN].copy_from_slice(destination.as_ref());
        payload[TRUNCATED_HASH_LEN..TRUNCATED_HASH_LEN * 2]
            .copy_from_slice(requester_identity.as_ref());
        payload[TRUNCATED_HASH_LEN * 2..].fill(destination.as_ref()[0]);
        let mut raw = [0_u8; RNS_MTU];
        let mut builder = PacketBuilder::new(&mut raw)
            .header_type(header)
            .packet_type(PacketType::Data)
            .dest_type(destination_type)
            .hops(hops)
            .destination_hash(rete_transport::PATH_REQUEST_DEST.as_ref())
            .context(context)
            .payload(&payload);
        if let Some(transport_id) = transport_id {
            builder = builder
                .transport_type(rete_core::TRANSPORT_TYPE_TRANSPORT)
                .transport_id(transport_id.as_ref());
        }
        let length = builder.build().unwrap();
        raw[..length].to_vec()
    };

    let h1_destination = DestHash::from([0xa1; TRUNCATED_HASH_LEN]);
    let h1 = build(
        h1_destination,
        HeaderType::Header1,
        None,
        DestType::Plain,
        0x73,
        0,
    );
    let h1_report = relay.ingest_at_with_broadcast_policy(
        &h1,
        300,
        MonotonicInstant::from_secs(300),
        InterfaceId(2),
        policy,
        &mut rng,
    );
    assert_eq!(h1_report.disposition, IngressDisposition::Processed);
    assert!(h1_report.actions.packets.is_empty());

    let h2_destination = DestHash::from([0xa2; TRUNCATED_HASH_LEN]);
    let h2 = build(
        h2_destination,
        HeaderType::Header2,
        Some(relay_identity),
        DestType::Plain,
        0x74,
        0,
    );
    let h2_report = relay.ingest_at_with_broadcast_policy(
        &h2,
        301,
        MonotonicInstant::from_secs(301),
        InterfaceId(2),
        policy,
        &mut rng,
    );
    assert_eq!(h2_report.disposition, IngressDisposition::Processed);
    assert!(h2_report.actions.packets.is_empty());
    assert_eq!(relay.metrics().pending_discovery_paths.used, 2);

    let wrong_transport = build(
        DestHash::from([0xa3; TRUNCATED_HASH_LEN]),
        HeaderType::Header2,
        Some(IdentityHash::from([0xff; TRUNCATED_HASH_LEN])),
        DestType::Plain,
        CONTEXT_NONE,
        0,
    );
    let wrong = relay.ingest_at_with_broadcast_policy(
        &wrong_transport,
        302,
        MonotonicInstant::from_secs(302),
        InterfaceId(2),
        policy,
        &mut rng,
    );
    assert!(matches!(
        wrong.disposition,
        IngressDisposition::Rejected(IngressDropReason::Header2NotAddressedToUs { .. })
    ));
    assert!(wrong.actions.packets.is_empty());
    assert_eq!(relay.metrics().pending_discovery_paths.used, 2);

    let overheight_destination = DestHash::from([0xa4; TRUNCATED_HASH_LEN]);
    let overheight = build(
        overheight_destination,
        HeaderType::Header1,
        None,
        DestType::Plain,
        CONTEXT_NONE,
        1,
    );
    let invalid = relay.ingest_at_with_broadcast_policy(
        &overheight,
        303,
        MonotonicInstant::from_secs(303),
        InterfaceId(2),
        policy,
        &mut rng,
    );
    assert_eq!(invalid.disposition, IngressDisposition::NativeInvalid);
    assert!(invalid.actions.packets.is_empty());
    assert_eq!(relay.metrics().pending_discovery_paths.used, 2);

    let valid_zero_hop = build(
        overheight_destination,
        HeaderType::Header1,
        None,
        DestType::Plain,
        CONTEXT_NONE,
        0,
    );
    let admitted = relay.ingest_at_with_broadcast_policy(
        &valid_zero_hop,
        304,
        MonotonicInstant::from_secs(304),
        InterfaceId(2),
        policy,
        &mut rng,
    );
    assert_eq!(admitted.disposition, IngressDisposition::Processed);
    assert!(admitted.actions.packets.is_empty());
    assert_eq!(relay.metrics().pending_discovery_paths.used, 3);

    for destination_type in [DestType::Single, DestType::Group, DestType::Link] {
        let invalid = build(
            DestHash::from([0xa5; TRUNCATED_HASH_LEN]),
            HeaderType::Header1,
            None,
            destination_type,
            CONTEXT_NONE,
            0,
        );
        let report = relay.ingest_at_with_broadcast_policy(
            &invalid,
            305,
            MonotonicInstant::from_secs(305),
            InterfaceId(2),
            policy,
            &mut rng,
        );
        assert!(report.actions.packets.is_empty());
    }
    assert_eq!(relay.metrics().pending_discovery_paths.used, 3);
}

#[test]
fn nonsingle_announce_is_invalid_before_route_replay_or_egress_state() {
    let mut source = node(89);
    let destination = source.destination_hash();
    let mut rng = CounterRng::default();
    source.queue_announce(None, 400, &mut rng).unwrap();
    let valid = source.flush_announces(400, &mut rng).remove(0);
    let valid_packet = Packet::parse(valid.bytes()).unwrap();

    for (index, destination_type) in [DestType::Plain, DestType::Group, DestType::Link]
        .into_iter()
        .enumerate()
    {
        let mut relay = TestNode::new(
            identity(90_u8.wrapping_add(index as u8)),
            "reticulum",
            &["announce-shape-policy"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let mut raw = [0_u8; RNS_MTU];
        let length = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Announce)
            .dest_type(destination_type)
            .context_flag(valid_packet.context_flag)
            .hops(valid_packet.hops)
            .destination_hash(valid_packet.destination_hash)
            .context(valid_packet.context)
            .payload(valid_packet.payload)
            .build()
            .unwrap();
        let invalid = relay.ingest_at_with_broadcast_policy(
            &raw[..length],
            400,
            MonotonicInstant::from_secs(400),
            InterfaceId(2),
            IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
                .with_announce_egress(IngressEgressSet::empty()),
            &mut rng,
        );
        assert_eq!(invalid.disposition, IngressDisposition::NativeInvalid);
        assert!(invalid.actions.packets.is_empty());
        assert!(!relay.has_path(&destination));
        assert_eq!(relay.metrics().capacity.announces.used, 0);

        let accepted = relay.ingest_at_with_broadcast_policy(
            valid.bytes(),
            401,
            MonotonicInstant::from_secs(401),
            InterfaceId(2),
            IngressBroadcastPolicy::new(IngressBroadcastScope::PointToPoint)
                .with_announce_egress(IngressEgressSet::empty()),
            &mut rng,
        );
        assert_eq!(accepted.disposition, IngressDisposition::Processed);
        assert!(relay.has_path(&destination));
    }
}

#[test]
fn received_secondary_announce_enables_targeted_data_preparation() {
    let mut receiver = node(61);
    let delivery = receiver
        .register_destination(
            "lxmf",
            &["delivery"],
            DestinationType::Single,
            Direction::In,
        )
        .unwrap();
    let mut sender = node(62);
    let mut rng = CounterRng::default();

    receiver
        .queue_announce_for(&delivery, Some(b"lxmf app data"), 1, &mut rng)
        .unwrap();
    let announce = receiver
        .flush_announces(1, &mut rng)
        .into_iter()
        .next()
        .unwrap();
    let learned = sender.ingest(announce.bytes(), 1, InterfaceId(7), &mut rng);
    assert_eq!(learned.disposition, IngressDisposition::Processed);
    assert_eq!(
        sender.route(&delivery).unwrap().received_on,
        Some(InterfaceId(7))
    );

    let mut output = [0; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &delivery,
            b"after secondary announce",
            2,
            &mut rng,
            &mut output,
        )
        .unwrap();
    assert_eq!(prepared.target(), TxTarget::Only(InterfaceId(7)));
}

#[test]
fn route_visitor_copies_complete_retained_path_fields() {
    let mut sender = node(63);
    let mut receiver = node(64);
    let destination = receiver.destination_hash();
    let mut rng = CounterRng::default();

    receiver.queue_announce(None, 100, &mut rng).unwrap();
    let announce = receiver
        .flush_announces(100, &mut rng)
        .into_iter()
        .next()
        .expect("the queued announce must be ready immediately");
    let learned = sender.ingest(announce.bytes(), 200, InterfaceId(7), &mut rng);
    assert_eq!(learned.disposition, IngressDisposition::Processed);

    let mut output = [0; RNS_MTU];
    sender
        .prepare_data_into(
            &destination,
            b"touch retained path",
            240,
            &mut rng,
            &mut output,
        )
        .expect("retained direct path must prepare DATA");

    let point = sender
        .route(&destination)
        .expect("route must remain retained");
    assert_eq!(point.destination, destination);
    assert_eq!(point.via, None);
    assert_eq!(point.hops, 1);
    assert_eq!(point.received_on, Some(InterfaceId(7)));
    assert_eq!(point.learned_at_seconds, 200);
    assert_eq!(point.last_accessed_at_seconds, 240);
    assert_eq!(
        point.expires_after_seconds,
        rete_transport::transport::PATH_EXPIRES
    );

    let mut copied = Vec::new();
    let visited = sender.visit_routes(|route| copied.push(route));
    assert_eq!(visited, 1);
    assert_eq!(visited, sender.route_count());
    assert_eq!(copied, [point]);
}

#[test]
fn tagged_transport_path_request_has_python_compatible_wire_shape() {
    let requester_identity = identity(72);
    let requester_hash = requester_identity.hash();
    let requester = TestNode::new(
        requester_identity,
        "reticulum",
        &["transport"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let requested = DestHash::from([0xa5; TRUNCATED_HASH_LEN]);
    let mut rng = CounterRng::default();
    let request = requester.request_path(&requested, &mut rng).unwrap();
    assert_eq!(request.target(), TxTarget::All);
    let packet = Packet::parse(request.bytes()).unwrap();
    assert_eq!(packet.header_type, HeaderType::Header1);
    assert_eq!(packet.packet_type, PacketType::Data);
    assert_eq!(packet.dest_type, DestType::Plain);
    assert_eq!(packet.context, CONTEXT_NONE);
    assert_eq!(
        packet.destination_hash,
        rete_transport::PATH_REQUEST_DEST.as_ref()
    );
    assert_eq!(packet.payload.len(), TRUNCATED_HASH_LEN * 3);
    assert_eq!(&packet.payload[..TRUNCATED_HASH_LEN], requested.as_ref());
    assert_eq!(
        &packet.payload[TRUNCATED_HASH_LEN..TRUNCATED_HASH_LEN * 2],
        requester_hash.as_ref()
    );
    assert_ne!(
        &packet.payload[TRUNCATED_HASH_LEN * 2..],
        &[0_u8; TRUNCATED_HASH_LEN]
    );
}

#[test]
fn tagged_endpoint_path_request_has_python_compatible_wire_shape() {
    let requester = node(71);
    let requested = DestHash::from([0xa4; TRUNCATED_HASH_LEN]);
    let mut rng = CounterRng::default();
    let request = requester.request_path(&requested, &mut rng).unwrap();
    let packet = Packet::parse(request.bytes()).unwrap();
    assert_eq!(packet.header_type, HeaderType::Header1);
    assert_eq!(packet.packet_type, PacketType::Data);
    assert_eq!(packet.dest_type, DestType::Plain);
    assert_eq!(packet.context, CONTEXT_NONE);
    assert_eq!(
        packet.destination_hash,
        rete_transport::PATH_REQUEST_DEST.as_ref()
    );
    assert_eq!(packet.payload.len(), TRUNCATED_HASH_LEN * 2);
    assert_eq!(&packet.payload[..TRUNCATED_HASH_LEN], requested.as_ref());
    assert_ne!(
        &packet.payload[TRUNCATED_HASH_LEN..],
        &[0_u8; TRUNCATED_HASH_LEN]
    );
}

#[test]
fn local_secondary_path_request_returns_exact_path_response_once() {
    let mut responder = TestNode::new(
        identity(73),
        "reticulum",
        &["transport"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let secondary = responder
        .register_destination(
            "lxmf",
            &["delivery"],
            DestinationType::Single,
            Direction::In,
        )
        .unwrap();
    responder
        .set_destination_announce_app_data(&secondary, Some(&[0x93, 0xc0, 0xc0, 0x90]))
        .unwrap();

    let mut requester = TestNode::new(
        identity(74),
        "reticulum",
        &["transport"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let mut rng = CounterRng::default();
    let request = requester.request_path(&secondary, &mut rng).unwrap();
    let first = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
    assert_eq!(first.disposition, IngressDisposition::Processed);
    assert_eq!(first.actions.packets.len(), 1);
    assert_eq!(
        first.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(7))
    );
    let response = crate::parse_announce_packet(first.actions.packets[0].bytes()).unwrap();
    assert_eq!(response.packet.destination_hash, secondary.as_ref());
    assert_eq!(response.packet.context, CONTEXT_PATH_RESPONSE);
    assert_eq!(
        response.fields.app_data,
        Some([0x93, 0xc0, 0xc0, 0x90].as_slice())
    );
    assert_eq!(responder.metrics().ingress.local_path_responses, 1);
    assert_eq!(responder.metrics().capacity.announces.used, 0);

    let learned = requester.ingest(
        first.actions.packets[0].bytes(),
        100,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(learned.disposition, IngressDisposition::Processed);
    assert_eq!(
        requester.route(&secondary).unwrap().received_on,
        Some(InterfaceId(7))
    );
    assert!(learned.actions.packets.is_empty());
    assert!(requester.tick(100, &mut rng).packets.is_empty());
    assert_eq!(requester.metrics().capacity.announces.used, 0);

    let duplicate = responder.ingest(request.bytes(), 101, InterfaceId(7), &mut rng);
    assert_eq!(duplicate.disposition, IngressDisposition::NativeDuplicate);
    assert!(duplicate.actions.packets.is_empty());
    assert_eq!(responder.metrics().ingress.local_path_responses, 1);

    let cross_interface_request = requester.request_path(&secondary, &mut rng).unwrap();
    let cross_interface = responder.ingest(
        cross_interface_request.bytes(),
        101,
        InterfaceId(8),
        &mut rng,
    );
    assert_eq!(cross_interface.disposition, IngressDisposition::Processed);
    assert_eq!(cross_interface.actions.packets.len(), 1);
    assert_eq!(
        cross_interface.actions.packets[0].target(),
        TxTarget::Only(InterfaceId(8)),
        "a fresh destination-plus-tag request receives an exact source response"
    );
    assert_eq!(responder.metrics().ingress.local_path_responses, 2);

    let fresh_request = requester.request_path(&secondary, &mut rng).unwrap();
    let fresh = responder.ingest(fresh_request.bytes(), 121, InterfaceId(7), &mut rng);
    assert_eq!(fresh.disposition, IngressDisposition::Processed);
    assert_eq!(fresh.actions.packets.len(), 1);
    assert_eq!(responder.metrics().ingress.local_path_responses, 3);
}

#[test]
fn full_mode_default_does_not_recursively_search_unknown_path() {
    let mut transport = TestNode::new(
        identity(75),
        "reticulum",
        &["transport"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let requester = TestNode::new(
        identity(76),
        "reticulum",
        &["requester"],
        EmbeddedNodeConfig::transport(),
    )
    .unwrap();
    let missing = DestHash::from([0xf5; TRUNCATED_HASH_LEN]);
    let mut rng = CounterRng::default();
    let request = requester.request_path(&missing, &mut rng).unwrap();
    let report = transport.ingest(request.bytes(), 200, InterfaceId(9), &mut rng);
    assert_eq!(report.disposition, IngressDisposition::NoObservableOutcome);
    assert!(report.actions.packets.is_empty());
    assert_eq!(transport.metrics().pending_discovery_paths.used, 0);
    assert_eq!(transport.metrics().ingress.local_path_responses, 0);
}
