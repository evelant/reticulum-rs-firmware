//! Owning receive-only pipeline from a physical RNode frame into Rete.
//!
//! This boundary deliberately has no operation that can return a Rete packet
//! action. Every action is counted and destroyed before control returns to the
//! caller, so target firmware can ingest hostile traffic without acquiring a
//! transmission-capable protocol surface.

use core::num::NonZeroU64;

use rand_core::{CryptoRng, RngCore};
use reticulum_radio_interface::{
    ExpiredFragment, FrameSignal, RNODE_HW_MTU, RawReceivedFrame, RxDiagnostics, TimedReceiveError,
    TimedReceiveOutcome, TimedRnodeRx,
};
use sha2::{Digest, Sha256};

use reticulum_rns_rete::{
    EmbeddedNode, EmbeddedNodeConfig, EmbeddedNodeMetrics, Identity, IdentityHash,
    IngressDisposition, IngressReport, InterfaceId, NodeActions,
};

/// Rete maintenance cadence used by the initial embedded owner.
///
/// Rete's transport, receipts, links and resources all rely on periodic
/// maintenance even when no packet arrives. Target code converts this value to
/// its monotonic tick domain; Rete itself receives monotonic seconds.
pub const RETE_MAINTENANCE_INTERVAL_SECONDS: u64 = 5;

/// Construction failure for the fail-closed receive-only owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReceiveOnlyIngressError {
    /// Rete rejected identity or destination construction.
    Rete,
    /// The primary destination could not be found to disable Link admission.
    PrimaryDestinationUnavailable,
}

impl core::fmt::Display for ReceiveOnlyIngressError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Rete => formatter.write_str("receive-only Rete construction failed"),
            Self::PrimaryDestinationUnavailable => {
                formatter.write_str("receive-only primary destination is unavailable")
            }
        }
    }
}

/// Why a queued physical frame was destroyed before RNode parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawFrameDropReason {
    /// The radio timestamp is ahead of the ingress clock sample.
    FutureTimestamp {
        received_at_ticks: u64,
        now_ticks: u64,
    },
    /// Frames must preserve the sole radio owner's monotonic FIFO order.
    OutOfOrderTimestamp {
        received_at_ticks: u64,
        previous_received_at_ticks: u64,
    },
    /// Queue residence reached the maximum accepted handoff age.
    Stale {
        age_ticks: u64,
        maximum_age_ticks: u64,
    },
    /// A retained first half expired before this frame could be processed.
    ///
    /// The complete collision frame is dropped instead of being reinterpreted
    /// as a new first half with the same sequence.
    PendingDeadlineElapsed { deadline_ticks: u64 },
}

/// Actions produced by Rete and destroyed inside the receive-only boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SuppressedActions {
    /// Application events observed and discarded after counting.
    pub events: u64,
    /// Complete outbound Reticulum packets destroyed without an interface.
    pub packets: u64,
    /// Source-relative packets Rete could not resolve and already discarded.
    pub unroutable_packets: u64,
}

/// Observable result of offering one completed physical frame to ingress.
#[derive(Debug, Eq, PartialEq)]
pub enum ReceiveOnlyIngressOutcome {
    /// A stale, future-dated or deadline-colliding queue item was destroyed.
    DroppedRawFrame(RawFrameDropReason),
    /// A first split half is retained until its absolute deadline.
    AwaitingContinuation {
        sequence: u8,
        data_len: usize,
        replaced_pending: bool,
        deadline_ticks: u64,
    },
    /// One packet crossed the 500-byte boundary and was synchronously ingested.
    Packet {
        packet_len: usize,
        /// SHA-256 of the exact reassembled bytes passed to Rete.
        raw_packet_sha256: [u8; 32],
        signal: FrameSignal,
        discarded_pending: bool,
        disposition: IngressDisposition,
        suppressed: SuppressedActions,
    },
    /// Physical framing or the independent 500-byte RNS guard rejected input.
    Rejected(TimedReceiveError),
}

/// Allocation-free receive, latency, suppression and native-node snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiveOnlyIngressMetrics {
    pub receive: RxDiagnostics,
    pub node: EmbeddedNodeMetrics,
    pub frames_handed_off: u64,
    pub raw_frames_dropped: u64,
    pub future_receive_timestamps: u64,
    pub out_of_order_receive_timestamps: u64,
    pub stale_raw_frames: u64,
    pub pending_deadline_collisions: u64,
    pub expired_fragment_watermark_ticks: Option<u64>,
    pub last_receive_timestamp_ticks: Option<u64>,
    pub last_handoff_latency_ticks: u64,
    pub maximum_handoff_latency_ticks: u64,
    pub rete_ingress_calls: u64,
    pub rete_tick_calls: u64,
    pub last_rete_ingress_seconds: Option<u64>,
    /// SHA-256 of the most recent exact byte slice passed to Rete.
    pub last_raw_packet_sha256: Option<[u8; 32]>,
    pub last_rete_tick_seconds: Option<u64>,
    pub suppressed: SuppressedActions,
}

/// One clock observation taken after an async channel-or-timer wake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiveOnlyClockSample {
    /// Embassy's monotonic tick domain, used only for scheduling/reassembly.
    pub ticks: u64,
    /// Monotonic whole seconds, supplied only to Rete state machines.
    pub transport_seconds: u64,
}

/// Cause delivered by the target's channel-or-absolute-timer select.
#[derive(Debug, Eq, PartialEq)]
pub enum ReceiveOnlyWake<'a> {
    /// One complete physical frame moved from the sole radio owner.
    Frame(&'a RawReceivedFrame),
    /// The coordinator's previously reported absolute deadline elapsed.
    Timer,
}

/// Scalar-only result of one coordinated wake.
#[derive(Debug, Eq, PartialEq)]
pub struct ReceiveOnlyStep {
    /// Fragment removed at or after its deadline, if any.
    pub expired_fragment: Option<ExpiredFragment>,
    /// Result of a frame wake; timer wakes leave this `None`.
    pub frame: Option<ReceiveOnlyIngressOutcome>,
    /// Suppressed output from one due Rete maintenance call.
    pub maintenance: Option<SuppressedActions>,
}

/// Pure monotonic schedule for Rete maintenance and fragment timer wakes.
///
/// The target checks [`Self::maintenance_due`] after every selected channel or
/// timer result. A continuously ready channel therefore cannot starve the core
/// maintenance tick. Fragment deadlines are read from the ingress owner each
/// time, so replacing a fragment also replaces the timer deadline without
/// retaining stale schedule state here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReceiveOnlySchedule {
    maintenance_interval_ticks: NonZeroU64,
    next_maintenance_ticks: u64,
}

impl ReceiveOnlySchedule {
    /// Schedule the first maintenance call one interval after `now_ticks`.
    const fn new(now_ticks: u64, maintenance_interval_ticks: NonZeroU64) -> Self {
        Self {
            maintenance_interval_ticks,
            next_maintenance_ticks: now_ticks.saturating_add(maintenance_interval_ticks.get()),
        }
    }

    /// Earliest absolute timer wake required by fragment or core maintenance.
    const fn next_wake_ticks(self, fragment_deadline: Option<u64>) -> u64 {
        match fragment_deadline {
            Some(deadline) if deadline < self.next_maintenance_ticks => deadline,
            Some(_) | None => self.next_maintenance_ticks,
        }
    }

    /// Whether target code must call receive-only Rete maintenance now.
    const fn maintenance_due(self, now_ticks: u64) -> bool {
        now_ticks >= self.next_maintenance_ticks
    }

    /// Move the maintenance deadline forward after one completed call.
    ///
    /// The deadline advances from its previous phase in O(1), while the caller
    /// still performs exactly one maintenance call after a long pause.
    fn maintenance_completed(&mut self, now_ticks: u64) {
        let interval = self.maintenance_interval_ticks.get();
        let elapsed = now_ticks.saturating_sub(self.next_maintenance_ticks);
        let intervals = (elapsed / interval).saturating_add(1);
        self.next_maintenance_ticks = self
            .next_maintenance_ticks
            .saturating_add(interval.saturating_mul(intervals));
    }

    /// Current absolute maintenance deadline for tests.
    #[cfg(test)]
    const fn next_maintenance_ticks(self) -> u64 {
        self.next_maintenance_ticks
    }
}

/// RNode reassembly and Rete state that cannot release outbound actions.
pub(crate) struct ReceiveOnlyIngress<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
> {
    receiver: TimedRnodeRx,
    packet: [u8; RNODE_HW_MTU],
    node: EmbeddedNode<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>,
    interface: InterfaceId,
    maximum_raw_frame_age_ticks: NonZeroU64,
    schedule: ReceiveOnlySchedule,
    frames_handed_off: u64,
    raw_frames_dropped: u64,
    future_receive_timestamps: u64,
    out_of_order_receive_timestamps: u64,
    stale_raw_frames: u64,
    pending_deadline_collisions: u64,
    expired_fragment_watermark_ticks: Option<u64>,
    last_receive_timestamp_ticks: Option<u64>,
    last_handoff_latency_ticks: u64,
    maximum_handoff_latency_ticks: u64,
    rete_ingress_calls: u64,
    rete_tick_calls: u64,
    last_rete_ingress_seconds: Option<u64>,
    last_raw_packet_sha256: Option<[u8; 32]>,
    last_rete_tick_seconds: Option<u64>,
    suppressed: SuppressedActions,
}

impl<const PATHS: usize, const ANNOUNCES: usize, const DEDUPLICATION: usize, const LINKS: usize>
    ReceiveOnlyIngress<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>
{
    /// Construct a receive-only endpoint around an explicitly supplied identity.
    ///
    /// Target code remains responsible for loading a durable identity or, in a
    /// clearly marked lab profile, generating an ephemeral identity from a
    /// qualified cryptographic entropy source.
    pub fn new(
        identity: Identity,
        app_name: &str,
        aspects: &[&str],
        fragment_timeout_ticks: NonZeroU64,
        initial_now_ticks: u64,
        maintenance_interval_ticks: NonZeroU64,
        interface: InterfaceId,
    ) -> Result<Self, ReceiveOnlyIngressError> {
        let mut node =
            EmbeddedNode::new(identity, app_name, aspects, EmbeddedNodeConfig::endpoint())
                .map_err(|_| ReceiveOnlyIngressError::Rete)?;
        let primary = node.destination_hash();
        if !node.set_accepts_links(&primary, false) {
            return Err(ReceiveOnlyIngressError::PrimaryDestinationUnavailable);
        }

        Ok(Self {
            receiver: TimedRnodeRx::new(fragment_timeout_ticks),
            packet: [0; RNODE_HW_MTU],
            node,
            interface,
            maximum_raw_frame_age_ticks: fragment_timeout_ticks,
            schedule: ReceiveOnlySchedule::new(initial_now_ticks, maintenance_interval_ticks),
            frames_handed_off: 0,
            raw_frames_dropped: 0,
            future_receive_timestamps: 0,
            out_of_order_receive_timestamps: 0,
            stale_raw_frames: 0,
            pending_deadline_collisions: 0,
            expired_fragment_watermark_ticks: None,
            last_receive_timestamp_ticks: None,
            last_handoff_latency_ticks: 0,
            maximum_handoff_latency_ticks: 0,
            rete_ingress_calls: 0,
            rete_tick_calls: 0,
            last_rete_ingress_seconds: None,
            last_raw_packet_sha256: None,
            last_rete_tick_seconds: None,
            suppressed: SuppressedActions::default(),
        })
    }

    /// Primary local destination, for diagnostics and controlled test setup.
    pub fn destination_hash(&self) -> reticulum_rns_rete::DestHash {
        self.node.destination_hash()
    }

    /// Local identity hash without exposing private identity material.
    pub fn identity_hash(&self) -> IdentityHash {
        self.node.identity_hash()
    }

    /// Absolute monotonic deadline of a retained first split half.
    pub const fn fragment_deadline_ticks(&self) -> Option<u64> {
        self.receiver.next_deadline()
    }

    /// Earliest absolute wake needed for fragment or Rete maintenance work.
    pub const fn next_wake_ticks(&self) -> u64 {
        self.schedule.next_wake_ticks(self.receiver.next_deadline())
    }

    /// Service one target wake and suppress all resulting protocol output.
    ///
    /// Expiry and core maintenance run from the same clock observation before
    /// a frame reaches RNode/Rete. If a first half expires on a frame wake, the
    /// entire collision frame is also dropped; it cannot become a new first
    /// half that later splices with a different packet.
    pub fn on_wake<R: RngCore + CryptoRng>(
        &mut self,
        wake: ReceiveOnlyWake<'_>,
        now: ReceiveOnlyClockSample,
        rng: &mut R,
    ) -> ReceiveOnlyStep {
        let expired_fragment = self.receiver.expire(now.ticks);
        if let Some(expired) = expired_fragment {
            self.expired_fragment_watermark_ticks = Some(
                self.expired_fragment_watermark_ticks
                    .map_or(expired.deadline_ticks, |watermark| {
                        watermark.max(expired.deadline_ticks)
                    }),
            );
        }
        let maintenance = if self.schedule.maintenance_due(now.ticks) {
            let suppressed = self.tick(now.transport_seconds, rng);
            self.schedule.maintenance_completed(now.ticks);
            Some(suppressed)
        } else {
            None
        };
        let frame = match wake {
            ReceiveOnlyWake::Frame(frame) => {
                Some(self.ingest_frame(frame, now.ticks, now.transport_seconds, rng))
            }
            ReceiveOnlyWake::Timer => None,
        };
        ReceiveOnlyStep {
            expired_fragment,
            frame,
            maintenance,
        }
    }

    /// Process one physical frame after coordinated deadline handling.
    ///
    /// `now_ticks` uses the same monotonic tick epoch as the radio timestamp.
    /// `transport_now_seconds` is the transport state machine's monotonic
    /// seconds value. Keeping them separate prevents accidental unit mixing;
    /// neither value is a wall-clock request timestamp.
    fn ingest_frame<R: RngCore + CryptoRng>(
        &mut self,
        frame: &RawReceivedFrame,
        now_ticks: u64,
        transport_now_seconds: u64,
        rng: &mut R,
    ) -> ReceiveOnlyIngressOutcome {
        self.frames_handed_off = self.frames_handed_off.saturating_add(1);
        if frame.received_at_ticks() > now_ticks {
            self.future_receive_timestamps = self.future_receive_timestamps.saturating_add(1);
            self.raw_frames_dropped = self.raw_frames_dropped.saturating_add(1);
            self.last_handoff_latency_ticks = 0;
            return ReceiveOnlyIngressOutcome::DroppedRawFrame(
                RawFrameDropReason::FutureTimestamp {
                    received_at_ticks: frame.received_at_ticks(),
                    now_ticks,
                },
            );
        }
        let latency = now_ticks - frame.received_at_ticks();
        self.last_handoff_latency_ticks = latency;
        self.maximum_handoff_latency_ticks = self.maximum_handoff_latency_ticks.max(latency);
        if let Some(previous_received_at_ticks) = self.last_receive_timestamp_ticks
            && frame.received_at_ticks() < previous_received_at_ticks
        {
            self.out_of_order_receive_timestamps =
                self.out_of_order_receive_timestamps.saturating_add(1);
            self.raw_frames_dropped = self.raw_frames_dropped.saturating_add(1);
            return ReceiveOnlyIngressOutcome::DroppedRawFrame(
                RawFrameDropReason::OutOfOrderTimestamp {
                    received_at_ticks: frame.received_at_ticks(),
                    previous_received_at_ticks,
                },
            );
        }
        self.last_receive_timestamp_ticks = Some(frame.received_at_ticks());
        if latency >= self.maximum_raw_frame_age_ticks.get() {
            self.stale_raw_frames = self.stale_raw_frames.saturating_add(1);
            self.raw_frames_dropped = self.raw_frames_dropped.saturating_add(1);
            return ReceiveOnlyIngressOutcome::DroppedRawFrame(RawFrameDropReason::Stale {
                age_ticks: latency,
                maximum_age_ticks: self.maximum_raw_frame_age_ticks.get(),
            });
        }

        if let Some(deadline_ticks) = self.expired_fragment_watermark_ticks
            && frame.received_at_ticks() <= deadline_ticks
        {
            self.pending_deadline_collisions = self.pending_deadline_collisions.saturating_add(1);
            self.raw_frames_dropped = self.raw_frames_dropped.saturating_add(1);
            return ReceiveOnlyIngressOutcome::DroppedRawFrame(
                RawFrameDropReason::PendingDeadlineElapsed { deadline_ticks },
            );
        }

        let receive = self.receiver.feed(
            frame.payload(),
            frame.received_at_ticks(),
            frame.signal(),
            &mut self.packet,
        );
        match receive {
            Err(error) => ReceiveOnlyIngressOutcome::Rejected(error),
            Ok(TimedReceiveOutcome::AwaitingContinuation {
                sequence,
                data_len,
                replaced_pending,
                deadline_ticks,
            }) => ReceiveOnlyIngressOutcome::AwaitingContinuation {
                sequence,
                data_len,
                replaced_pending,
                deadline_ticks,
            },
            Ok(TimedReceiveOutcome::Packet {
                packet_len,
                signal,
                discarded_pending,
            }) => {
                self.rete_ingress_calls = self.rete_ingress_calls.saturating_add(1);
                self.last_rete_ingress_seconds = Some(transport_now_seconds);
                let raw_packet_sha256: [u8; 32] = Sha256::digest(&self.packet[..packet_len]).into();
                self.last_raw_packet_sha256 = Some(raw_packet_sha256);
                let IngressReport {
                    disposition,
                    metadata: _,
                    actions,
                } = self.node.ingest(
                    &self.packet[..packet_len],
                    transport_now_seconds,
                    self.interface,
                    rng,
                );
                let suppressed = suppress_actions(actions);
                accumulate_suppressed(&mut self.suppressed, suppressed);
                ReceiveOnlyIngressOutcome::Packet {
                    packet_len,
                    raw_packet_sha256,
                    signal,
                    discarded_pending,
                    disposition,
                    suppressed,
                }
            }
        }
    }

    /// Run Rete maintenance while destroying every generated action.
    ///
    /// Target code must call this on the cadence described by
    /// [`RETE_MAINTENANCE_INTERVAL_SECONDS`] even in complete radio silence.
    fn tick<R: RngCore + CryptoRng>(
        &mut self,
        transport_now_seconds: u64,
        rng: &mut R,
    ) -> SuppressedActions {
        self.rete_tick_calls = self.rete_tick_calls.saturating_add(1);
        self.last_rete_tick_seconds = Some(transport_now_seconds);
        let suppressed = suppress_actions(self.node.tick(transport_now_seconds, rng));
        accumulate_suppressed(&mut self.suppressed, suppressed);
        suppressed
    }

    /// Return a fixed-size snapshot without exposing mutable Rete state.
    pub fn metrics(&self) -> ReceiveOnlyIngressMetrics {
        ReceiveOnlyIngressMetrics {
            receive: self.receiver.diagnostics(),
            node: self.node.metrics(),
            frames_handed_off: self.frames_handed_off,
            raw_frames_dropped: self.raw_frames_dropped,
            future_receive_timestamps: self.future_receive_timestamps,
            out_of_order_receive_timestamps: self.out_of_order_receive_timestamps,
            stale_raw_frames: self.stale_raw_frames,
            pending_deadline_collisions: self.pending_deadline_collisions,
            expired_fragment_watermark_ticks: self.expired_fragment_watermark_ticks,
            last_receive_timestamp_ticks: self.last_receive_timestamp_ticks,
            last_handoff_latency_ticks: self.last_handoff_latency_ticks,
            maximum_handoff_latency_ticks: self.maximum_handoff_latency_ticks,
            rete_ingress_calls: self.rete_ingress_calls,
            rete_tick_calls: self.rete_tick_calls,
            last_rete_ingress_seconds: self.last_rete_ingress_seconds,
            last_raw_packet_sha256: self.last_raw_packet_sha256,
            last_rete_tick_seconds: self.last_rete_tick_seconds,
            suppressed: self.suppressed,
        }
    }
}

/// Initial generic Phase-0/conformance profile for larger-capacity probes.
///
/// Constrained firmware targets should select explicit measured capacities.
#[cfg(test)]
type InitialReceiveOnlyIngress = ReceiveOnlyIngress<
    { reticulum_rns_rete::probe_capacity::PATHS },
    { reticulum_rns_rete::probe_capacity::ANNOUNCES },
    { reticulum_rns_rete::probe_capacity::DEDUPLICATION_ENTRIES },
    { reticulum_rns_rete::probe_capacity::LINKS },
>;

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn suppress_actions(actions: NodeActions) -> SuppressedActions {
    // Exact destructuring is intentional: a new outbound action field in the
    // Rete owner must fail compilation until this safety boundary is reviewed.
    let NodeActions {
        events,
        proof_sidecars,
        packets,
        unroutable_packets,
    } = actions;
    let suppressed = SuppressedActions {
        events: usize_to_u64(events.len()),
        packets: usize_to_u64(packets.len()),
        unroutable_packets: usize_to_u64(unroutable_packets),
    };

    // Destroy every allocation-backed action before returning scalar counts.
    drop(events);
    drop(proof_sidecars);
    drop(packets);
    suppressed
}

fn accumulate_suppressed(total: &mut SuppressedActions, update: SuppressedActions) {
    total.events = total.events.saturating_add(update.events);
    total.packets = total.packets.saturating_add(update.packets);
    total.unroutable_packets = total
        .unroutable_packets
        .saturating_add(update.unroutable_packets);
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rand_core::{CryptoRng, RngCore};
    use reticulum_radio_interface::{RNODE_LORA_DATA_PER_FRAME, RNS_MTU, SX1262_FRAME_MTU};

    use super::*;
    use reticulum_rns_rete::{IngressDropReason, InitialEmbeddedNode, TxPacket};

    const TIMEOUT: NonZeroU64 = NonZeroU64::new(100).unwrap();
    const MAINTENANCE: NonZeroU64 = NonZeroU64::new(1_000).unwrap();
    const INTERFACE: InterfaceId = InterfaceId(7);

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

    fn raw_frame(bytes: &[u8], received_at_ticks: u64) -> RawReceivedFrame {
        assert!(bytes.len() <= SX1262_FRAME_MTU);
        let mut frame = [0; SX1262_FRAME_MTU];
        frame[..bytes.len()].copy_from_slice(bytes);
        RawReceivedFrame::new(
            frame,
            u8::try_from(bytes.len()).unwrap(),
            FrameSignal::new(-91, 7),
            received_at_ticks,
        )
    }

    fn physical_packet(payload: &[u8], received_at_ticks: u64) -> RawReceivedFrame {
        assert!(payload.len() < SX1262_FRAME_MTU);
        let mut bytes = [0; SX1262_FRAME_MTU];
        bytes[0] = 0xA0;
        bytes[1..=payload.len()].copy_from_slice(payload);
        RawReceivedFrame::new(
            bytes,
            u8::try_from(payload.len() + 1).unwrap(),
            FrameSignal::new(-91, 7),
            received_at_ticks,
        )
    }

    fn ingress(
        seed: &[u8],
        aspect: &str,
        initial_now_ticks: u64,
        maintenance_interval_ticks: NonZeroU64,
    ) -> InitialReceiveOnlyIngress {
        InitialReceiveOnlyIngress::new(
            Identity::from_seed(seed).unwrap(),
            "reticulum-rs-firmware",
            &[aspect],
            TIMEOUT,
            initial_now_ticks,
            maintenance_interval_ticks,
            INTERFACE,
        )
        .unwrap()
    }

    fn wake_frame(
        ingress: &mut InitialReceiveOnlyIngress,
        frame: RawReceivedFrame,
        now_ticks: u64,
        transport_seconds: u64,
    ) -> ReceiveOnlyStep {
        ingress.on_wake(
            ReceiveOnlyWake::Frame(&frame),
            ReceiveOnlyClockSample {
                ticks: now_ticks,
                transport_seconds,
            },
            &mut CounterRng::default(),
        )
    }

    fn wake_timer(
        ingress: &mut InitialReceiveOnlyIngress,
        now_ticks: u64,
        transport_seconds: u64,
    ) -> ReceiveOnlyStep {
        ingress.on_wake(
            ReceiveOnlyWake::Timer,
            ReceiveOnlyClockSample {
                ticks: now_ticks,
                transport_seconds,
            },
            &mut CounterRng::default(),
        )
    }

    fn link_request_for_receive_only() -> (InitialReceiveOnlyIngress, TxPacket) {
        let responder_identity = Identity::from_seed(b"rx-only link responder identity").unwrap();
        let mut initiator = InitialEmbeddedNode::new(
            Identity::from_seed(b"rx-only link initiator identity").unwrap(),
            "reticulum-rs-firmware",
            &["rx-initiator"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        initiator
            .register_peer(
                &responder_identity,
                "reticulum-rs-firmware",
                &["rx-responder"],
                1,
            )
            .unwrap();

        let ingress = InitialReceiveOnlyIngress::new(
            responder_identity,
            "reticulum-rs-firmware",
            &["rx-responder"],
            TIMEOUT,
            0,
            MAINTENANCE,
            INTERFACE,
        )
        .unwrap();
        let request = initiator
            .initiate_link(ingress.destination_hash(), 2, &mut CounterRng::default())
            .unwrap()
            .0;
        (ingress, request)
    }

    #[test]
    fn proof_producing_actions_are_counted_and_destroyed_exhaustively() {
        let responder_identity = Identity::from_seed(b"proof responder identity").unwrap();
        let mut responder = InitialEmbeddedNode::new(
            responder_identity,
            "reticulum-rs-firmware",
            &["proof-responder"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        let mut initiator = InitialEmbeddedNode::new(
            Identity::from_seed(b"proof initiator identity").unwrap(),
            "reticulum-rs-firmware",
            &["proof-initiator"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        initiator
            .register_peer(
                &Identity::from_seed(b"proof responder identity").unwrap(),
                "reticulum-rs-firmware",
                &["proof-responder"],
                1,
            )
            .unwrap();
        let request = initiator
            .initiate_link(responder.destination_hash(), 2, &mut CounterRng::default())
            .unwrap()
            .0;
        let mut rng = CounterRng::default();
        let report = responder.ingest(request.bytes(), 2, INTERFACE, &mut rng);

        assert_eq!(report.disposition, IngressDisposition::Processed);
        assert_eq!(report.actions.packets.len(), 1);
        let suppressed = suppress_actions(report.actions);
        assert_eq!(
            suppressed,
            SuppressedActions {
                events: 0,
                packets: 1,
                unroutable_packets: 0,
            }
        );
    }

    #[test]
    fn receive_only_constructor_disables_inbound_links() {
        let (mut ingress, request) = link_request_for_receive_only();
        let step = wake_frame(&mut ingress, physical_packet(request.bytes(), 10), 12, 2);

        assert!(matches!(
            step.frame,
            Some(ReceiveOnlyIngressOutcome::Packet {
                disposition: IngressDisposition::Rejected(
                    IngressDropReason::DestinationDoesNotAcceptLinks
                ),
                suppressed: SuppressedActions {
                    events: 0,
                    packets: 0,
                    unroutable_packets: 0,
                },
                ..
            })
        ));
        let metrics = ingress.metrics();
        assert_eq!(metrics.rete_ingress_calls, 1);
        assert_eq!(metrics.suppressed, SuppressedActions::default());
        assert_eq!(metrics.node.capacity.links.used, 0);
        assert_eq!(metrics.frames_handed_off, 1);
        assert_eq!(metrics.last_handoff_latency_ticks, 2);
        assert_eq!(metrics.maximum_handoff_latency_ticks, 2);
    }

    #[test]
    fn local_data_event_is_counted_and_cannot_escape() {
        let receiver_identity = Identity::from_seed(b"rx-only data receiver").unwrap();
        let mut sender = InitialEmbeddedNode::new(
            Identity::from_seed(b"rx-only data sender").unwrap(),
            "reticulum-rs-firmware",
            &["data-sender"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        sender
            .register_peer(
                &receiver_identity,
                "reticulum-rs-firmware",
                &["data-receiver"],
                1,
            )
            .unwrap();
        let mut ingress = InitialReceiveOnlyIngress::new(
            receiver_identity,
            "reticulum-rs-firmware",
            &["data-receiver"],
            TIMEOUT,
            0,
            MAINTENANCE,
            INTERFACE,
        )
        .unwrap();
        let packet = sender
            .send_data(
                &ingress.destination_hash(),
                b"bounded local event",
                4,
                &mut CounterRng::default(),
            )
            .unwrap();

        let step = wake_frame(&mut ingress, physical_packet(packet.bytes(), 10), 12, 77);
        assert!(matches!(
            step.frame,
            Some(ReceiveOnlyIngressOutcome::Packet {
                disposition: IngressDisposition::Processed,
                suppressed: SuppressedActions {
                    events: 1,
                    packets: 0,
                    unroutable_packets: 0,
                },
                ..
            })
        ));
        assert_eq!(ingress.metrics().last_rete_ingress_seconds, Some(77));
    }

    #[test]
    fn pending_fragment_expires_without_another_frame() {
        let mut ingress = ingress(b"rx-only expiry identity", "rx-expiry", 0, MAINTENANCE);
        let frame = raw_frame(&[0x31, 1, 2], 20);

        assert!(matches!(
            wake_frame(&mut ingress, frame, 20, 1).frame,
            Some(ReceiveOnlyIngressOutcome::AwaitingContinuation {
                deadline_ticks: 120,
                ..
            })
        ));
        assert_eq!(ingress.fragment_deadline_ticks(), Some(120));
        assert_eq!(ingress.next_wake_ticks(), 120);
        assert!(wake_timer(&mut ingress, 119, 1).expired_fragment.is_none());
        assert!(wake_timer(&mut ingress, 120, 1).expired_fragment.is_some());
        assert_eq!(ingress.fragment_deadline_ticks(), None);
        assert_eq!(ingress.metrics().receive.pending_expired, 1);
    }

    #[test]
    fn exact_frame_timer_tie_expires_ticks_and_drops_collision_frame() {
        let maintenance = NonZeroU64::new(100).unwrap();
        let mut ingress = ingress(b"rx-only exact tie", "rx-tie", 0, maintenance);
        let first = wake_frame(&mut ingress, raw_frame(&[0x31, 1, 2], 0), 0, 0);
        assert!(matches!(
            first.frame,
            Some(ReceiveOnlyIngressOutcome::AwaitingContinuation {
                deadline_ticks: 100,
                ..
            })
        ));

        let tied = wake_frame(&mut ingress, raw_frame(&[0x31, 3, 4], 99), 100, 5);
        assert!(tied.expired_fragment.is_some());
        assert_eq!(
            tied.maintenance,
            Some(SuppressedActions {
                events: 1,
                packets: 0,
                unroutable_packets: 0,
            })
        );
        assert_eq!(
            tied.frame,
            Some(ReceiveOnlyIngressOutcome::DroppedRawFrame(
                RawFrameDropReason::PendingDeadlineElapsed {
                    deadline_ticks: 100,
                },
            ))
        );
        assert_eq!(ingress.fragment_deadline_ticks(), None);
        assert_eq!(ingress.metrics().rete_ingress_calls, 0);
        assert_eq!(ingress.metrics().rete_tick_calls, 1);
        assert_eq!(ingress.metrics().pending_deadline_collisions, 1);
    }

    #[test]
    fn timer_first_wake_retains_collision_watermark_across_depth_two_queue() {
        let mut ingress = ingress(b"rx-only timer first", "rx-timer-first", 0, MAINTENANCE);
        let first = wake_frame(&mut ingress, raw_frame(&[0x31, 1, 2], 0), 0, 0);
        assert!(matches!(
            first.frame,
            Some(ReceiveOnlyIngressOutcome::AwaitingContinuation {
                deadline_ticks: 100,
                ..
            })
        ));

        let timer = wake_timer(&mut ingress, 100, 5);
        assert!(timer.expired_fragment.is_some());
        assert_eq!(
            ingress.metrics().expired_fragment_watermark_ticks,
            Some(100)
        );

        for (received_at_ticks, payload) in [(99, &[0x31, 3][..]), (100, &[0x31, 4][..])] {
            let step = wake_frame(&mut ingress, raw_frame(payload, received_at_ticks), 101, 5);
            assert_eq!(
                step.frame,
                Some(ReceiveOnlyIngressOutcome::DroppedRawFrame(
                    RawFrameDropReason::PendingDeadlineElapsed {
                        deadline_ticks: 100,
                    },
                ))
            );
        }
        assert_eq!(ingress.metrics().pending_deadline_collisions, 2);
        assert_eq!(
            ingress.metrics().expired_fragment_watermark_ticks,
            Some(100)
        );

        let new_first = wake_frame(&mut ingress, raw_frame(&[0x41, 9], 101), 102, 5);
        assert!(matches!(
            new_first.frame,
            Some(ReceiveOnlyIngressOutcome::AwaitingContinuation {
                sequence: 4,
                deadline_ticks: 201,
                ..
            })
        ));
        assert_eq!(
            ingress.metrics().expired_fragment_watermark_ticks,
            Some(100)
        );
    }

    #[test]
    fn delayed_first_half_deadline_is_anchored_to_radio_capture_time() {
        let mut ingress = ingress(b"rx-only capture anchor", "rx-capture", 0, MAINTENANCE);
        let first = wake_frame(&mut ingress, raw_frame(&[0x51, 1], 0), 99, 1);
        assert!(matches!(
            first.frame,
            Some(ReceiveOnlyIngressOutcome::AwaitingContinuation {
                deadline_ticks: 100,
                ..
            })
        ));

        let late = wake_frame(&mut ingress, raw_frame(&[0x51, 2], 100), 100, 1);
        assert!(late.expired_fragment.is_some());
        assert!(matches!(
            late.frame,
            Some(ReceiveOnlyIngressOutcome::DroppedRawFrame(
                RawFrameDropReason::PendingDeadlineElapsed {
                    deadline_ticks: 100,
                }
            ))
        ));
        assert_eq!(ingress.metrics().rete_ingress_calls, 0);
    }

    #[test]
    fn ready_frames_cannot_starve_maintenance() {
        let maintenance = NonZeroU64::new(10).unwrap();
        let mut ingress = ingress(b"rx-only ready channel", "rx-ready", 0, maintenance);

        assert!(
            wake_frame(&mut ingress, raw_frame(&[], 1), 1, 1)
                .maintenance
                .is_none()
        );
        assert!(
            wake_frame(&mut ingress, raw_frame(&[], 10), 10, 2)
                .maintenance
                .is_some()
        );
        assert!(
            wake_frame(&mut ingress, raw_frame(&[], 19), 19, 3)
                .maintenance
                .is_none()
        );
        assert!(
            wake_frame(&mut ingress, raw_frame(&[], 20), 20, 4)
                .maintenance
                .is_some()
        );
        let metrics = ingress.metrics();
        assert_eq!(metrics.rete_tick_calls, 2);
        assert_eq!(metrics.last_rete_tick_seconds, Some(4));
    }

    #[test]
    fn replacement_cancels_old_fragment_deadline() {
        let mut ingress = ingress(b"rx-only replacement", "rx-replace", 0, MAINTENANCE);
        let first = wake_frame(&mut ingress, raw_frame(&[0x11, 1], 10), 10, 1);
        assert!(matches!(
            first.frame,
            Some(ReceiveOnlyIngressOutcome::AwaitingContinuation {
                deadline_ticks: 110,
                replaced_pending: false,
                ..
            })
        ));
        let replacement = wake_frame(&mut ingress, raw_frame(&[0x21, 2], 20), 20, 1);
        assert!(matches!(
            replacement.frame,
            Some(ReceiveOnlyIngressOutcome::AwaitingContinuation {
                deadline_ticks: 120,
                replaced_pending: true,
                ..
            })
        ));
        assert_eq!(ingress.next_wake_ticks(), 120);

        assert!(wake_timer(&mut ingress, 110, 1).expired_fragment.is_none());
        assert_eq!(ingress.fragment_deadline_ticks(), Some(120));
        assert!(wake_timer(&mut ingress, 120, 1).expired_fragment.is_some());
        assert_eq!(ingress.fragment_deadline_ticks(), None);
    }

    #[test]
    fn stale_and_future_raw_frames_are_dropped_before_reassembly() {
        let mut ingress = ingress(b"rx-only raw age", "rx-age", 0, MAINTENANCE);
        let stale = wake_frame(&mut ingress, raw_frame(&[0xA0, 1], 0), 100, 1);
        assert_eq!(
            stale.frame,
            Some(ReceiveOnlyIngressOutcome::DroppedRawFrame(
                RawFrameDropReason::Stale {
                    age_ticks: 100,
                    maximum_age_ticks: 100,
                },
            ))
        );
        let future = wake_frame(&mut ingress, raw_frame(&[0xA0, 1], 102), 101, 1);
        assert_eq!(
            future.frame,
            Some(ReceiveOnlyIngressOutcome::DroppedRawFrame(
                RawFrameDropReason::FutureTimestamp {
                    received_at_ticks: 102,
                    now_ticks: 101,
                },
            ))
        );
        let metrics = ingress.metrics();
        assert_eq!(metrics.raw_frames_dropped, 2);
        assert_eq!(metrics.stale_raw_frames, 1);
        assert_eq!(metrics.future_receive_timestamps, 1);
        assert_eq!(metrics.receive.frames_seen, 0);
        assert_eq!(metrics.rete_ingress_calls, 0);
    }

    #[test]
    fn out_of_order_capture_timestamp_cannot_complete_pending_fragment() {
        let mut ingress = ingress(b"rx-only timestamp order", "rx-order", 0, MAINTENANCE);
        let first = wake_frame(&mut ingress, raw_frame(&[0x61, 1], 10), 10, 1);
        assert!(matches!(
            first.frame,
            Some(ReceiveOnlyIngressOutcome::AwaitingContinuation {
                deadline_ticks: 110,
                ..
            })
        ));

        let reversed = wake_frame(&mut ingress, raw_frame(&[0x61, 2], 9), 11, 1);
        assert_eq!(
            reversed.frame,
            Some(ReceiveOnlyIngressOutcome::DroppedRawFrame(
                RawFrameDropReason::OutOfOrderTimestamp {
                    received_at_ticks: 9,
                    previous_received_at_ticks: 10,
                },
            ))
        );
        assert_eq!(ingress.fragment_deadline_ticks(), Some(110));
        assert_eq!(ingress.metrics().out_of_order_receive_timestamps, 1);
        assert_eq!(ingress.metrics().rete_ingress_calls, 0);
    }

    #[test]
    fn packet_over_base_mtu_never_reaches_rete() {
        let mut ingress = ingress(b"rx-only mtu identity", "rx-mtu", 0, MAINTENANCE);
        let packet = [0x5A; RNS_MTU + 1];
        let mut first = [0; SX1262_FRAME_MTU];
        first[0] = 0x41;
        first[1..].copy_from_slice(&packet[..RNODE_LORA_DATA_PER_FRAME]);
        let mut second = [0; SX1262_FRAME_MTU];
        let second_payload = &packet[RNODE_LORA_DATA_PER_FRAME..];
        second[0] = 0x41;
        second[1..=second_payload.len()].copy_from_slice(second_payload);

        assert!(matches!(
            wake_frame(
                &mut ingress,
                RawReceivedFrame::new(first, u8::MAX, FrameSignal::new(-80, 4), 1,),
                1,
                1,
            )
            .frame,
            Some(ReceiveOnlyIngressOutcome::AwaitingContinuation { .. })
        ));
        assert_eq!(
            wake_frame(
                &mut ingress,
                RawReceivedFrame::new(
                    second,
                    u8::try_from(second_payload.len() + 1).unwrap(),
                    FrameSignal::new(-82, 2),
                    2,
                ),
                2,
                1,
            )
            .frame,
            Some(ReceiveOnlyIngressOutcome::Rejected(
                TimedReceiveError::RnsPacketTooLong {
                    actual: RNS_MTU + 1,
                    maximum: RNS_MTU,
                }
            ))
        );
        assert_eq!(ingress.metrics().rete_ingress_calls, 0);
    }

    #[test]
    fn tick_and_transport_seconds_never_enter_the_same_clock_domain() {
        let maintenance = NonZeroU64::new(50).unwrap();
        let mut ingress = ingress(b"rx-only clock units", "rx-clock", 100, maintenance);
        assert_eq!(ingress.next_wake_ticks(), 150);

        let tick = wake_timer(&mut ingress, 150, 7);
        assert_eq!(
            tick.maintenance,
            Some(SuppressedActions {
                events: 1,
                packets: 0,
                unroutable_packets: 0,
            })
        );
        assert_eq!(ingress.next_wake_ticks(), 200);
        assert_eq!(ingress.metrics().last_rete_tick_seconds, Some(7));

        let packet = wake_frame(&mut ingress, physical_packet(&[0], 151), 151, 8);
        assert!(matches!(
            packet.frame,
            Some(ReceiveOnlyIngressOutcome::Packet { .. })
        ));
        assert_eq!(ingress.metrics().last_rete_ingress_seconds, Some(8));
        assert_eq!(ingress.next_wake_ticks(), 200);
    }

    #[test]
    fn delayed_maintenance_advances_once_without_drift_or_catch_up_loop() {
        let mut schedule = ReceiveOnlySchedule::new(0, NonZeroU64::new(10).unwrap());
        assert!(schedule.maintenance_due(35));
        schedule.maintenance_completed(35);
        assert_eq!(schedule.next_maintenance_ticks(), 40);
        assert!(!schedule.maintenance_due(39));
        assert!(schedule.maintenance_due(40));
    }

    #[test]
    fn suppression_and_latency_counters_saturate() {
        let mut ingress = ingress(b"rx-only saturation", "rx-saturation", 0, MAINTENANCE);
        ingress.frames_handed_off = u64::MAX;
        ingress.raw_frames_dropped = u64::MAX;
        ingress.future_receive_timestamps = u64::MAX;
        ingress.out_of_order_receive_timestamps = u64::MAX;
        ingress.rete_ingress_calls = u64::MAX;
        ingress.rete_tick_calls = u64::MAX;
        ingress.suppressed = SuppressedActions {
            events: u64::MAX,
            packets: u64::MAX,
            unroutable_packets: u64::MAX,
        };

        accumulate_suppressed(
            &mut ingress.suppressed,
            SuppressedActions {
                events: 1,
                packets: 1,
                unroutable_packets: 1,
            },
        );
        let outcome = wake_frame(&mut ingress, physical_packet(&[0], 1), 1, 2);
        assert!(matches!(
            outcome.frame,
            Some(ReceiveOnlyIngressOutcome::Packet { .. })
        ));
        let future = wake_frame(&mut ingress, raw_frame(&[], 3), 2, 2);
        assert!(matches!(
            future.frame,
            Some(ReceiveOnlyIngressOutcome::DroppedRawFrame(
                RawFrameDropReason::FutureTimestamp { .. }
            ))
        ));
        let reversed = wake_frame(&mut ingress, raw_frame(&[], 0), 2, 2);
        assert!(matches!(
            reversed.frame,
            Some(ReceiveOnlyIngressOutcome::DroppedRawFrame(
                RawFrameDropReason::OutOfOrderTimestamp { .. }
            ))
        ));
        let metrics = ingress.metrics();
        assert_eq!(metrics.frames_handed_off, u64::MAX);
        assert_eq!(metrics.raw_frames_dropped, u64::MAX);
        assert_eq!(metrics.future_receive_timestamps, u64::MAX);
        assert_eq!(metrics.out_of_order_receive_timestamps, u64::MAX);
        assert_eq!(metrics.rete_ingress_calls, u64::MAX);
        assert_eq!(metrics.rete_tick_calls, u64::MAX);
        assert_eq!(metrics.suppressed.events, u64::MAX);
        assert_eq!(metrics.suppressed.packets, u64::MAX);
        assert_eq!(metrics.suppressed.unroutable_packets, u64::MAX);
    }
}
