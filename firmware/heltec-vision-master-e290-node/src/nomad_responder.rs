//! Static policy for the E290's minimal inbound NomadNet node.
//!
//! This module registers one `nomadnetwork.node` destination and classifies
//! already-projected application events. It owns no application-event lease,
//! response packet, ordinary-router admission, timer or executor state. The
//! permanent node task remains responsible for preserving those owners while
//! calling the response-preparation wrapper.

use reticulum_node_core::{
    ApplicationEvent, ApplicationLinkBinding, ApplicationLinkRole, DestinationHash,
    InboundProofPolicy, LocalDestinationAnnounceAppDataError, LocalDestinationLinkPolicyError,
    LocalDestinationProofPolicyError, LocalDestinationRegistrationError, NodeCore,
};

/// Reticulum application name of a NomadNet node destination.
pub const NOMAD_NODE_APPLICATION_NAME: &str = "nomadnetwork";
/// Reticulum aspects of a NomadNet node destination.
pub const NOMAD_NODE_ASPECTS: [&str; 1] = ["node"];
/// Default UTF-8 display name carried by NomadNet node announces.
pub const NOMAD_NODE_ANNOUNCE_APP_DATA: &str = "Metalbeard";
/// Initial page path served by the minimal responder.
pub const NOMAD_INDEX_PATH: &str = reticulum_nomad_protocol::DEFAULT_INDEX_PATH;
/// SHA-256(`/page/index.mu`)[..16], as used by Reticulum request dispatch.
pub const NOMAD_INDEX_PATH_HASH: [u8; 16] = [
    0xfb, 0x40, 0xab, 0xf3, 0x59, 0xb3, 0xf2, 0x5f, 0xa0, 0x08, 0x61, 0x07, 0xc5, 0xee, 0xe5, 0x16,
];
/// Canonical MessagePack `nil` used by an anonymous NomadNet page request.
pub const NOMAD_ANONYMOUS_REQUEST_VALUE: [u8; 1] = [0xc0];
/// Largest page body admitted by the first direct-response product slice.
pub const NOMAD_INDEX_PAGE_MAX_BYTES: usize = 400;
/// Static UTF-8 Micron page returned for `/page/index.mu`.
pub const NOMAD_INDEX_PAGE: &str = "\
>Metalbeard

This page is served directly by an embedded Rust Reticulum node.

The node is online and ready to exchange LXMF messages over the LoRa mesh.
";

const _: () = assert!(NOMAD_INDEX_PAGE_MAX_BYTES == reticulum_nomad_protocol::MAX_PAGE_BYTES);
const _: () = assert!(NOMAD_INDEX_PAGE.len() <= NOMAD_INDEX_PAGE_MAX_BYTES);
const _: () =
    assert!(NOMAD_NODE_ANNOUNCE_APP_DATA.len() <= reticulum_node_core::MAX_ANNOUNCE_APP_DATA);

/// Construction failure after the product elected to enable its Nomad responder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NomadResponderActivationError {
    /// RNS rejected or lacked capacity for the additional destination.
    Registration(LocalDestinationRegistrationError),
    /// RNS could not retain the default UTF-8 announce application data.
    AnnounceAppData(LocalDestinationAnnounceAppDataError),
    /// RNS could not enable inbound Links on the destination.
    LinkPolicy(LocalDestinationLinkPolicyError),
    /// RNS rejected the explicit default no-proof policy.
    ProofPolicy(LocalDestinationProofPolicyError),
}

/// Register the boot-lifetime inbound `nomadnetwork.node` destination.
///
/// This matches NomadNet's `RNS.Destination.IN`/`RNS.Destination.SINGLE`
/// construction. Links are enabled, the default announce application data is
/// the raw UTF-8 display name [`NOMAD_NODE_ANNOUNCE_APP_DATA`] (without an
/// application envelope), and automatic DATA proofs use
/// [`InboundProofPolicy::Never`], equivalent to Reticulum's `PROVE_NONE`.
/// Request responses travel through the separate ordinary-action wrapper.
///
/// This construction step runs before the node enters the permanent
/// supervisor. Any error is fail-stop because the narrow node API deliberately
/// exposes no destination-unregister operation.
pub fn activate_nomad_responder<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const PACKET_BUFFERS: usize,
>(
    node: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
) -> Result<DestinationHash, NomadResponderActivationError> {
    let destination = node
        .register_inbound_single_destination(NOMAD_NODE_APPLICATION_NAME, &NOMAD_NODE_ASPECTS)
        .map_err(NomadResponderActivationError::Registration)?;
    node.set_destination_announce_app_data(
        &destination,
        Some(NOMAD_NODE_ANNOUNCE_APP_DATA.as_bytes()),
    )
    .map_err(NomadResponderActivationError::AnnounceAppData)?;
    node.set_destination_accepts_links(&destination, true)
        .map_err(NomadResponderActivationError::LinkPolicy)?;
    node.set_destination_inbound_proof_policy(&destination, InboundProofPolicy::Never)
        .map_err(NomadResponderActivationError::ProofPolicy)?;
    Ok(destination)
}

/// Borrowed inputs required to prepare one static index-page response.
///
/// The Link binding and request ID remain borrowed from the exact projected
/// application event. Safe application code cannot manufacture the binding,
/// and this value exposes no Link handle, receipt, protocol token or
/// cancellation authority.
#[derive(Clone, Copy, Debug)]
#[must_use = "an accepted Nomad request must be responded to or explicitly discarded"]
pub struct NomadIndexResponse<'event> {
    binding: &'event ApplicationLinkBinding,
    request: &'event [u8; 16],
    page: &'static [u8],
}

impl<'event> NomadIndexResponse<'event> {
    /// Borrow the authoritative event-carried Link binding.
    pub const fn binding(&self) -> &'event ApplicationLinkBinding {
        self.binding
    }

    /// Borrow the exact event-carried request identifier.
    pub const fn request(&self) -> &'event [u8; 16] {
        self.request
    }

    /// Borrow the static UTF-8 Micron response body.
    pub const fn page(&self) -> &'static [u8] {
        self.page
    }

    /// Split into the three borrowed response-preparation inputs.
    pub const fn into_parts(
        self,
    ) -> (
        &'event ApplicationLinkBinding,
        &'event [u8; 16],
        &'static [u8],
    ) {
        (self.binding, self.request, self.page)
    }
}

/// Static Nomad responder classification for one application event.
#[derive(Clone, Copy, Debug)]
#[must_use = "every inbound request needs an explicit responder disposition"]
pub enum NomadResponderDisposition<'event> {
    /// The exact anonymous index request may enter response preparation.
    Respond(NomadIndexResponse<'event>),
    /// The event is not either supported inbound request representation.
    Unrelated,
    /// The request arrived on a locally initiated Link instead of a responder Link.
    WrongRole,
    /// The responder Link terminates at another registered local destination.
    WrongDestination,
    /// The request path is not exactly `/page/index.mu`.
    WrongPath,
    /// The request value is not exactly canonical MessagePack `nil`.
    WrongValue,
    /// Rete decoded a binary or string request into the legacy request variant.
    LegacyRequestReceived,
}

/// Classify one projected application event for the static Nomad index service.
///
/// Only [`ApplicationEvent::RequestValueReceived`] can be accepted. The
/// event-carried opaque binding must still name a responder Link terminating at
/// `configured_destination`, and both path hash and complete encoded value must
/// match the canonical anonymous index request. Rejections borrow or retain no
/// response authority.
pub fn classify_nomad_responder_event<'event>(
    configured_destination: &DestinationHash,
    event: &'event ApplicationEvent,
) -> NomadResponderDisposition<'event> {
    match event {
        ApplicationEvent::RequestValueReceived {
            binding,
            request,
            path,
            encoded_value,
            ..
        } => classify_request_value(
            configured_destination,
            binding,
            request,
            path,
            encoded_value,
        ),
        ApplicationEvent::RequestReceived { binding, path, .. } => {
            classify_legacy_request(configured_destination, binding, path)
        }
        _ => NomadResponderDisposition::Unrelated,
    }
}

fn classify_request_binding<'event>(
    configured_destination: &DestinationHash,
    binding: &'event ApplicationLinkBinding,
) -> Result<(), NomadResponderDisposition<'event>> {
    if binding.role() != ApplicationLinkRole::Responder {
        return Err(NomadResponderDisposition::WrongRole);
    }
    if binding.destination() != configured_destination.as_bytes() {
        return Err(NomadResponderDisposition::WrongDestination);
    }
    Ok(())
}

fn classify_request_value<'event>(
    configured_destination: &DestinationHash,
    binding: &'event ApplicationLinkBinding,
    request: &'event [u8; 16],
    path: &[u8; 16],
    encoded_value: &[u8],
) -> NomadResponderDisposition<'event> {
    if let Err(disposition) = classify_request_binding(configured_destination, binding) {
        return disposition;
    }
    if path != &NOMAD_INDEX_PATH_HASH {
        return NomadResponderDisposition::WrongPath;
    }
    if encoded_value != NOMAD_ANONYMOUS_REQUEST_VALUE {
        return NomadResponderDisposition::WrongValue;
    }
    NomadResponderDisposition::Respond(NomadIndexResponse {
        binding,
        request,
        page: NOMAD_INDEX_PAGE.as_bytes(),
    })
}

fn classify_legacy_request<'event>(
    configured_destination: &DestinationHash,
    binding: &'event ApplicationLinkBinding,
    path: &[u8; 16],
) -> NomadResponderDisposition<'event> {
    if let Err(disposition) = classify_request_binding(configured_destination, binding) {
        return disposition;
    }
    if path != &NOMAD_INDEX_PATH_HASH {
        return NomadResponderDisposition::WrongPath;
    }
    NomadResponderDisposition::LegacyRequestReceived
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::{CryptoRng, RngCore};
    use reticulum_node_core::{
        IngressReport, LinkHandle, LinkState, MonotonicSeconds, NodeConfig, NodeIdentity,
        NodeInstanceId, PacketInterfaceId, RequestDispatchConfirmation, RequestHandle,
    };
    use sha2::{Digest, Sha256};

    type TestNode = NodeCore<8, 4, 8, 8, 0>;

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

    fn time(seconds: u64) -> MonotonicSeconds {
        MonotonicSeconds::new(seconds)
    }

    fn identity(tag: u8) -> NodeIdentity {
        NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid")
    }

    fn node(tag: u8, aspect: &str) -> TestNode {
        TestNode::new(
            identity(tag),
            "reticulum",
            &[aspect],
            NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
            NodeConfig::endpoint(),
        )
        .expect("test node must construct")
    }

    fn establish_link(
        initiator: &mut TestNode,
        responder: &mut TestNode,
        destination: DestinationHash,
        initiator_interface: PacketInterfaceId,
        responder_interface: PacketInterfaceId,
        rng: &mut CounterRng,
    ) -> LinkHandle {
        let (request, link) = initiator
            .initiate_link(&destination, time(2), rng)
            .expect("test Link must initiate");
        let proof = responder
            .ingest(
                request.packets[0].bytes(),
                time(2),
                responder_interface,
                rng,
            )
            .expect("responder must accept LINKREQUEST");
        let established = initiator
            .ingest(
                proof.actions.packets[0].bytes(),
                time(3),
                initiator_interface,
                rng,
            )
            .expect("initiator must accept LRPROOF");
        responder
            .ingest(
                established.actions.packets[0].bytes(),
                time(4),
                responder_interface,
                rng,
            )
            .expect("responder must accept LRRTT");
        assert_eq!(initiator.link_state(link), Some(LinkState::Active));
        assert_eq!(
            responder.link_state(LinkHandle::new(*link.as_bytes())),
            Some(LinkState::Active)
        );
        link
    }

    fn activated_pair(
        initiator_tag: u8,
    ) -> (TestNode, TestNode, DestinationHash, LinkHandle, CounterRng) {
        let responder_tag = initiator_tag.wrapping_add(1);
        let mut responder = node(responder_tag, "nomad-responder-primary");
        let destination =
            activate_nomad_responder(&mut responder).expect("Nomad responder must activate");
        let mut initiator = node(initiator_tag, "nomad-request-initiator");
        initiator
            .register_peer(
                &identity(responder_tag),
                NOMAD_NODE_APPLICATION_NAME,
                &NOMAD_NODE_ASPECTS,
                time(1),
            )
            .expect("Nomad responder identity must cache");
        let mut rng = CounterRng::default();
        let link = establish_link(
            &mut initiator,
            &mut responder,
            destination,
            PacketInterfaceId::new(3),
            PacketInterfaceId::new(7),
            &mut rng,
        );
        (initiator, responder, destination, link, rng)
    }

    fn deliver_anonymous_request(
        sender: &mut TestNode,
        receiver: &mut TestNode,
        link: LinkHandle,
        path: &str,
        receiver_interface: PacketInterfaceId,
        rng: &mut CounterRng,
    ) -> (IngressReport, RequestHandle) {
        let prepared = sender
            .prepare_anonymous_request(link, path, 1_700_000_000.125, rng)
            .expect("anonymous request must prepare");
        let handle = prepared.handle();
        assert_eq!(
            sender.confirm_request_dispatch(handle, time(5), true),
            Ok(RequestDispatchConfirmation::Confirmed)
        );
        let report = receiver
            .ingest(
                prepared.actions().packets[0].bytes(),
                time(5),
                receiver_interface,
                rng,
            )
            .expect("anonymous request must reach its retained Link");
        (report, handle)
    }

    #[test]
    fn static_page_and_index_hash_match_the_reviewed_protocol_bounds() {
        assert_eq!(NOMAD_NODE_APPLICATION_NAME, "nomadnetwork");
        assert_eq!(NOMAD_NODE_ASPECTS, ["node"]);
        assert_eq!(NOMAD_NODE_ANNOUNCE_APP_DATA.as_bytes(), b"Metalbeard");
        assert_eq!(NOMAD_INDEX_PATH, "/page/index.mu");
        assert_eq!(NOMAD_INDEX_PAGE_MAX_BYTES, 400);
        assert!(NOMAD_INDEX_PAGE.len() <= NOMAD_INDEX_PAGE_MAX_BYTES);
        assert!(core::str::from_utf8(NOMAD_INDEX_PAGE.as_bytes()).is_ok());
        assert!(core::str::from_utf8(NOMAD_NODE_ANNOUNCE_APP_DATA.as_bytes()).is_ok());
        assert!(NOMAD_INDEX_PAGE.starts_with(">Metalbeard\n\n"));
        assert!(NOMAD_INDEX_PAGE.ends_with('\n'));
        let digest = Sha256::digest(NOMAD_INDEX_PATH.as_bytes());
        assert_eq!(&digest[..16], NOMAD_INDEX_PATH_HASH);
        assert_eq!(NOMAD_ANONYMOUS_REQUEST_VALUE, [0xc0]);
        assert_eq!(InboundProofPolicy::default(), InboundProofPolicy::Never);
    }

    #[test]
    fn activation_uses_the_nomad_destination_and_default_utf8_announce_data() {
        let mut responder = TestNode::new(
            identity(21),
            "reticulum",
            &["nomad-activation-responder"],
            NodeInstanceId::new([0x95; 16]),
            NodeConfig::transport(),
        )
        .expect("transport test node must construct");
        let destination =
            activate_nomad_responder(&mut responder).expect("Nomad responder must activate");
        assert_ne!(destination, responder.destination_hash());
        let canonical_nomad_destination = TestNode::new(
            identity(21),
            NOMAD_NODE_APPLICATION_NAME,
            &NOMAD_NODE_ASPECTS,
            NodeInstanceId::new([0x96; 16]),
            NodeConfig::endpoint(),
        )
        .expect("canonical Nomad destination must construct")
        .destination_hash();
        assert_eq!(destination, canonical_nomad_destination);

        let mut rng = CounterRng::default();
        let mut observer = node(22, "nomad-activation-observer");
        let request = observer
            .request_path(&destination, &mut rng)
            .expect("Nomad destination path request must build");
        let response = responder
            .ingest(
                request.packets[0].bytes(),
                time(1),
                PacketInterfaceId::new(5),
                &mut rng,
            )
            .expect("Nomad destination must answer its path request");
        assert_eq!(response.actions.packets.len(), 1);
        let observed = observer
            .ingest(
                response.actions.packets[0].bytes(),
                time(1),
                PacketInterfaceId::new(5),
                &mut rng,
            )
            .expect("valid Nomad path-response announce must ingest");
        assert!(matches!(
            observed.actions.events.as_slice(),
            [ApplicationEvent::AnnounceReceived {
                destination: announced_destination,
                app_data: Some(app_data),
                ..
            }] if announced_destination == destination.as_bytes()
                && app_data.as_slice() == NOMAD_NODE_ANNOUNCE_APP_DATA.as_bytes()
        ));

        let link = establish_link(
            &mut observer,
            &mut responder,
            destination,
            PacketInterfaceId::new(5),
            PacketInterfaceId::new(5),
            &mut rng,
        );
        assert_eq!(observer.link_state(link), Some(LinkState::Active));
    }

    #[test]
    fn exact_projected_anonymous_index_request_borrows_response_inputs() {
        let (mut initiator, mut responder, destination, link, mut rng) = activated_pair(31);
        let (inbound, handle) = deliver_anonymous_request(
            &mut initiator,
            &mut responder,
            link,
            NOMAD_INDEX_PATH,
            PacketInterfaceId::new(7),
            &mut rng,
        );
        let event = inbound
            .actions
            .events
            .first()
            .expect("request event must be present");
        let NomadResponderDisposition::Respond(response) =
            classify_nomad_responder_event(&destination, event)
        else {
            panic!("exact anonymous index request must be accepted")
        };
        assert_eq!(response.binding().role(), ApplicationLinkRole::Responder);
        assert_eq!(response.binding().destination(), destination.as_bytes());
        assert_eq!(response.request(), handle.request());
        assert_eq!(response.page(), NOMAD_INDEX_PAGE.as_bytes());
        let (binding, request, page) = response.into_parts();
        assert_eq!(binding.destination(), destination.as_bytes());
        assert_eq!(request, handle.request());
        assert_eq!(page, NOMAD_INDEX_PAGE.as_bytes());

        let ApplicationEvent::RequestValueReceived {
            binding,
            request,
            path,
            requested_at,
            encoded_value,
        } = event
        else {
            panic!("accepted event must remain the value-preserving request variant")
        };
        assert_eq!(*requested_at, 1_700_000_000.125);
        assert_eq!(encoded_value.as_slice(), NOMAD_ANONYMOUS_REQUEST_VALUE);
        let rejected_values: [&[u8]; 4] = [&[], &[0xc2], &[0xc0, 0xc0], &[0xc4, 0x00]];
        for non_nil in rejected_values {
            assert!(matches!(
                classify_request_value(&destination, binding, request, path, non_nil),
                NomadResponderDisposition::WrongValue
            ));
        }
        assert!(matches!(
            classify_legacy_request(&destination, binding, path),
            NomadResponderDisposition::LegacyRequestReceived
        ));

        let response_actions = responder
            .prepare_response_actions(
                binding,
                request,
                NOMAD_INDEX_PAGE.as_bytes(),
                time(6),
                &mut rng,
            )
            .expect("bounded static Micron response must prepare");
        assert_eq!(response_actions.packets.len(), 1);
        assert!(response_actions.events.is_empty());
        assert_eq!(response_actions.packets[0].protocol_token(), None);
        let decoded = initiator
            .ingest(
                response_actions.packets[0].bytes(),
                time(6),
                PacketInterfaceId::new(3),
                &mut rng,
            )
            .expect("initiator must decode the direct Nomad response");
        assert!(matches!(
            decoded.actions.events.as_slice(),
            [ApplicationEvent::ResponseReceived {
                link: response_link,
                request: response_request,
                data,
            }] if response_link == link.as_bytes()
                && response_request == handle.request()
                && data.as_slice() == NOMAD_INDEX_PAGE.as_bytes()
                && core::str::from_utf8(data).is_ok()
                && data.len() <= NOMAD_INDEX_PAGE_MAX_BYTES
        ));
    }

    #[test]
    fn real_projected_requests_reject_wrong_path_role_and_destination() {
        let (mut initiator, mut responder, destination, link, mut rng) = activated_pair(41);

        let (wrong_path, _) = deliver_anonymous_request(
            &mut initiator,
            &mut responder,
            link,
            "/page/other.mu",
            PacketInterfaceId::new(7),
            &mut rng,
        );
        assert!(matches!(
            classify_nomad_responder_event(
                &destination,
                wrong_path.actions.events.first().unwrap(),
            ),
            NomadResponderDisposition::WrongPath
        ));

        let responder_link = LinkHandle::new(*link.as_bytes());
        let (wrong_role, _) = deliver_anonymous_request(
            &mut responder,
            &mut initiator,
            responder_link,
            NOMAD_INDEX_PATH,
            PacketInterfaceId::new(3),
            &mut rng,
        );
        assert!(matches!(
            classify_nomad_responder_event(
                &destination,
                wrong_role.actions.events.first().unwrap(),
            ),
            NomadResponderDisposition::WrongRole
        ));

        let other_destination = responder
            .register_inbound_single_destination("other", &["service"])
            .expect("second inbound destination must register");
        responder
            .set_destination_accepts_links(&other_destination, true)
            .unwrap();
        responder
            .set_destination_inbound_proof_policy(&other_destination, InboundProofPolicy::Never)
            .unwrap();
        initiator
            .register_peer(&identity(42), "other", &["service"], time(6))
            .expect("second destination identity must cache");
        let other_link = establish_link(
            &mut initiator,
            &mut responder,
            other_destination,
            PacketInterfaceId::new(3),
            PacketInterfaceId::new(7),
            &mut rng,
        );
        let (wrong_destination, _) = deliver_anonymous_request(
            &mut initiator,
            &mut responder,
            other_link,
            NOMAD_INDEX_PATH,
            PacketInterfaceId::new(7),
            &mut rng,
        );
        assert!(matches!(
            classify_nomad_responder_event(
                &destination,
                wrong_destination.actions.events.first().unwrap(),
            ),
            NomadResponderDisposition::WrongDestination
        ));
    }
}
