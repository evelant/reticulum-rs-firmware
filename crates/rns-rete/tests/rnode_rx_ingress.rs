use core::num::NonZeroU64;
use std::cell::Cell;

use rand_core::{CryptoRng, RngCore};
use reticulum_radio_interface::{
    FrameSignal, RNODE_HW_MTU, RNODE_LORA_DATA_PER_FRAME, RNS_MTU, RxDiagnostics,
    TimedReceiveError, TimedReceiveOutcome, TimedRnodeRx,
};
use reticulum_rns_rete::{
    DestHash, EmbeddedNodeConfig, Identity, IngressDisposition, InitialEmbeddedNode, InterfaceId,
    NodeEvent, Packet,
};

const VECTOR_JSON: &str = include_str!("../../../interop/vectors/rns-1.3.8.json");
const FRAGMENT_TIMEOUT_TICKS: NonZeroU64 = NonZeroU64::new(100).unwrap();

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

fn announce_fixture() -> (Vec<u8>, DestHash) {
    let fixture: serde_json::Value = serde_json::from_str(VECTOR_JSON).unwrap();
    let announce = fixture.get("announce").unwrap();
    let raw = hex::decode(announce.get("raw_hex").unwrap().as_str().unwrap()).unwrap();
    let destination: [u8; 16] = hex::decode(
        announce
            .get("destination_hash_hex")
            .unwrap()
            .as_str()
            .unwrap(),
    )
    .unwrap()
    .try_into()
    .unwrap();
    (raw, DestHash::from(destination))
}

fn feed_and_handoff<T>(
    receiver: &mut TimedRnodeRx,
    frame: &[u8],
    now_ticks: u64,
    signal: FrameSignal,
    output: &mut [u8; RNODE_HW_MTU],
    handoff: impl FnOnce(&[u8], FrameSignal) -> T,
) -> Result<(TimedReceiveOutcome, Option<T>), TimedReceiveError> {
    let outcome = receiver.feed(frame, now_ticks, signal, output)?;
    let handed_off = match outcome {
        TimedReceiveOutcome::Packet {
            packet_len, signal, ..
        } => Some(handoff(&output[..packet_len], signal)),
        TimedReceiveOutcome::AwaitingContinuation { .. } => None,
    };
    Ok((outcome, handed_off))
}

#[test]
fn python_announce_crosses_rnode_rx_and_embedded_ingress() {
    let (raw, expected_destination) = announce_fixture();
    let parsed = Packet::parse(&raw).unwrap();
    assert_eq!(
        DestHash::from_slice(parsed.destination_hash),
        expected_destination
    );
    assert_eq!(parsed.hops, 0);

    let mut frame = Vec::with_capacity(raw.len() + 1);
    frame.push(0xA0); // Sequence 10, no split flag.
    frame.extend_from_slice(&raw);

    let signal = FrameSignal::new(-91, 7);
    let mut receiver = TimedRnodeRx::new(FRAGMENT_TIMEOUT_TICKS);
    let mut output = [0; RNODE_HW_MTU];
    let mut node = InitialEmbeddedNode::new(
        Identity::from_seed(b"rnode rx integration node").unwrap(),
        "reticulum-rs-firmware",
        &["rx-integration"],
        EmbeddedNodeConfig::endpoint(),
    )
    .unwrap();
    let mut rng = CounterRng::default();

    let (outcome, report) = feed_and_handoff(
        &mut receiver,
        &frame,
        10,
        signal,
        &mut output,
        |packet, handed_off_signal| {
            assert_eq!(packet, raw);
            assert_eq!(handed_off_signal, signal);
            node.ingest(packet, 1_700_000_001, InterfaceId(7), &mut rng)
        },
    )
    .unwrap();

    assert_eq!(
        outcome,
        TimedReceiveOutcome::Packet {
            packet_len: raw.len(),
            signal,
            discarded_pending: false,
        }
    );
    assert_eq!(
        receiver.diagnostics(),
        RxDiagnostics {
            frames_seen: 1,
            framing_errors: 0,
            pending_started: 0,
            pending_replaced: 0,
            pending_expired: 0,
            pending_discarded: 0,
            packets_completed: 1,
            packets_accepted: 1,
            packets_too_long: 0,
            last_frame_len: u16::try_from(frame.len()).unwrap(),
            last_packet_len: u16::try_from(raw.len()).unwrap(),
            last_frame_signal: Some(signal),
            last_packet_signal: Some(signal),
        }
    );

    let report = report.expect("an accepted packet must reach EmbeddedNode");
    assert_eq!(report.disposition, IngressDisposition::Processed);
    assert!(report.actions.packets.is_empty());
    assert_eq!(report.actions.unroutable_packets, 0);
    assert_eq!(report.actions.events.len(), 1);
    match &report.actions.events[0] {
        NodeEvent::AnnounceReceived {
            dest_hash,
            hops,
            app_data,
            ..
        } => {
            assert_eq!(*dest_hash, expected_destination);
            // Rete reports the packet after the receiving hop has been applied;
            // the immutable Python fixture remains at its transmitted hop count.
            assert_eq!(*hops, parsed.hops.saturating_add(1));
            assert!(app_data.is_none());
        }
        event => panic!("expected AnnounceReceived, got {event:?}"),
    }

    let metrics = node.metrics();
    assert_eq!(metrics.ingress.seen, 1);
    assert_eq!(metrics.ingress.admitted, 1);
    assert_eq!(metrics.ingress.rejected, 0);
    assert_eq!(metrics.ingress.native_duplicate, 0);
    assert_eq!(metrics.ingress.native_invalid, 0);
    assert_eq!(metrics.transport.packets_received, 1);
    assert_eq!(metrics.transport.announces_received, 1);
    assert_eq!(metrics.transport.paths_learned, 1);
    assert_eq!(metrics.capacity.paths.used, 1);
}

#[test]
fn completed_501_byte_packet_stops_before_embedded_ingress() {
    let packet = [0x5A; RNS_MTU + 1];
    let mut first = [0; RNODE_LORA_DATA_PER_FRAME + 1];
    first[0] = 0x31; // Sequence 3, split flag.
    first[1..].copy_from_slice(&packet[..RNODE_LORA_DATA_PER_FRAME]);

    let mut second = Vec::with_capacity(packet.len() - RNODE_LORA_DATA_PER_FRAME + 1);
    second.push(0x31);
    second.extend_from_slice(&packet[RNODE_LORA_DATA_PER_FRAME..]);

    let signal = FrameSignal::new(-80, 4);
    let mut receiver = TimedRnodeRx::new(FRAGMENT_TIMEOUT_TICKS);
    let mut output = [0; RNODE_HW_MTU];
    let embedded_ingress_calls = Cell::new(0_u64);

    let (first_outcome, first_handoff) =
        feed_and_handoff(&mut receiver, &first, 20, signal, &mut output, |_, _| {
            embedded_ingress_calls.set(embedded_ingress_calls.get() + 1)
        })
        .unwrap();
    assert!(matches!(
        first_outcome,
        TimedReceiveOutcome::AwaitingContinuation { .. }
    ));
    assert_eq!(first_handoff, None);

    assert_eq!(
        feed_and_handoff(&mut receiver, &second, 21, signal, &mut output, |_, _| {
            embedded_ingress_calls.set(embedded_ingress_calls.get() + 1)
        },),
        Err(TimedReceiveError::RnsPacketTooLong {
            actual: RNS_MTU + 1,
            maximum: RNS_MTU,
        })
    );
    assert_eq!(embedded_ingress_calls.get(), 0);

    let diagnostics = receiver.diagnostics();
    assert_eq!(diagnostics.frames_seen, 2);
    assert_eq!(diagnostics.packets_completed, 1);
    assert_eq!(diagnostics.packets_accepted, 0);
    assert_eq!(diagnostics.packets_too_long, 1);
    assert_eq!(
        diagnostics.last_packet_len,
        u16::try_from(RNS_MTU + 1).unwrap()
    );
}
