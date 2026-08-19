//! Resolved protocol actions and inbound DATA projection.

use super::*;

/// Application events and resolved transmission actions from one core call.
#[derive(Debug, Default)]
#[must_use = "every protocol event, packet, and unroutable-action count must be drained or retained"]
pub struct NodeActions {
    /// Exhaustive project-owned events with exact moved payload owners.
    pub events: ApplicationEvents,
    /// Packets with their interface target resolved while ingress context is
    /// still available.
    pub packets: Vec<TxPacket>,
    /// Native source-dependent actions dropped because no source interface was
    /// available. This should remain zero for timer-driven work.
    pub unroutable_packets: usize,
}

/// Scalar counts returned when a complete action envelope is intentionally
/// destroyed without routing its contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscardedNodeActionCounts {
    events: usize,
    packets: usize,
    unroutable_packets: usize,
}

impl DiscardedNodeActionCounts {
    /// Application events destroyed with the envelope.
    pub const fn events(self) -> usize {
        self.events
    }

    /// Outbound packets destroyed with the envelope, including any privately
    /// retained proof owner.
    pub const fn packets(self) -> usize {
        self.packets
    }

    /// Source-dependent actions already classified as unroutable upstream.
    pub const fn unroutable_packets(self) -> usize {
        self.unroutable_packets
    }
}

impl NodeActions {
    /// Construct an action envelope that cannot contain retained proof
    /// authority.
    pub fn without_retained_proofs(
        events: Vec<ApplicationEvent>,
        packets: Vec<TxPacket>,
        unroutable_packets: usize,
    ) -> Self {
        Self {
            events: ApplicationEvents::without_retained_proofs(events),
            packets,
            unroutable_packets,
        }
    }

    /// Attach the authoritative interface and optional physical-link signal
    /// values to application events produced by this one synchronous ingress.
    ///
    /// The interface owner calls this before the action envelope can be
    /// queued or separated from its ingress packet. Events that are not part of
    /// this bounded ingress-observation surface remain unchanged.
    pub fn attach_ingress_observation(&mut self, interface: u8, signal: Option<(i16, i16)>) {
        let signal =
            signal.map(|(rssi_dbm, snr_db)| IngressSignalObservation::new(rssi_dbm, snr_db));
        self.attach_exact_ingress_observation(IngressObservation::remote(
            InterfaceId(interface),
            signal,
        ));
    }

    pub(crate) fn attach_exact_ingress_observation(&mut self, observation: IngressObservation) {
        for event in &mut self.events.events {
            match event {
                ApplicationEvent::AnnounceReceived { ingress, .. }
                | ApplicationEvent::DataReceived { ingress, .. }
                | ApplicationEvent::ProofReceived { ingress, .. }
                | ApplicationEvent::LinkData { ingress, .. } => {
                    *ingress = Some(observation);
                }
                _ => {}
            }
        }
    }

    /// Whether this envelope owns no event, retained proof, packet, or
    /// unroutable-action observation.
    pub fn is_empty(&self) -> bool {
        let Self {
            events,
            packets,
            unroutable_packets,
        } = self;
        events.owns_no_actions() && packets.is_empty() && *unroutable_packets == 0
    }

    /// Number of retained proofs privately bound to events in this envelope.
    pub fn retained_proof_count(&self) -> usize {
        self.events.retained_proof_count()
    }

    /// Intentionally destroy every action and return only exhaustive scalar
    /// counts.
    ///
    /// Exact destructuring here deliberately makes a future owning field fail
    /// compilation until this destruction boundary is reviewed.
    pub fn discard(self) -> DiscardedNodeActionCounts {
        let Self {
            events,
            packets,
            unroutable_packets,
        } = self;
        let counts = DiscardedNodeActionCounts {
            events: events.len(),
            packets: packets.len().saturating_add(events.retained_proof_count()),
            unroutable_packets,
        };
        drop(events);
        drop(packets);
        counts
    }
}

/// Product-owned plaintext from one native destination-DATA event.
///
/// Projection moves the native payload allocation into this value unchanged.
/// It deliberately applies no mailbox capacity or payload-length policy: a
/// caller can therefore observe DATA for future destination types whose
/// plaintext limit differs from the current encrypted SINGLE-destination
/// limit.
#[must_use = "inbound DATA must be retained, delivered, or explicitly discarded"]
pub struct InboundData {
    destination: [u8; rete_core::TRUNCATED_HASH_LEN],
    payload: Vec<u8>,
    ingress: Option<IngressObservation>,
}

impl core::fmt::Debug for InboundData {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InboundData")
            .field("destination", &self.destination)
            .field("payload_len", &self.payload.len())
            .field("ingress", &self.ingress)
            .finish_non_exhaustive()
    }
}

impl InboundData {
    /// Complete destination hash addressed by the received DATA packet.
    pub const fn destination(&self) -> &[u8; rete_core::TRUNCATED_HASH_LEN] {
        &self.destination
    }

    /// Decrypted application payload.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Interface provenance and optional physical-link signal values.
    pub const fn ingress(&self) -> Option<IngressObservation> {
        self.ingress
    }

    /// Consume this value into its destination and exact moved payload owner.
    pub fn into_parts(self) -> ([u8; rete_core::TRUNCATED_HASH_LEN], Vec<u8>) {
        (self.destination, self.payload)
    }

    /// Consume this value into destination, exact payload owner, and ingress observation.
    pub fn into_observed_parts(
        self,
    ) -> (
        [u8; rete_core::TRUNCATED_HASH_LEN],
        Vec<u8>,
        Option<IngressObservation>,
    ) {
        (self.destination, self.payload, self.ingress)
    }
}

/// Result of projecting one application event onto the inbound-DATA surface.
#[must_use = "the projected DATA or unchanged non-DATA event must be handled"]
pub enum InboundDataProjection {
    /// Decrypted destination DATA with its original payload allocation.
    Data(InboundData),
    /// Any other application event, returned unchanged to its caller.
    Other(ApplicationEvent),
}

impl core::fmt::Debug for InboundDataProjection {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Data(data) => formatter.debug_tuple("Data").field(data).finish(),
            Self::Other(_) => formatter.write_str("Other(..)"),
        }
    }
}

/// Consume one application event and project destination DATA without cloning.
pub fn project_inbound_data(event: ApplicationEvent) -> InboundDataProjection {
    match event {
        ApplicationEvent::DataReceived {
            destination,
            payload,
            ingress,
        } => InboundDataProjection::Data(InboundData {
            destination,
            payload,
            ingress,
        }),
        other => InboundDataProjection::Other(other),
    }
}
