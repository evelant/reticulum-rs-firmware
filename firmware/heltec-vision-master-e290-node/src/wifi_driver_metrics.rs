//! Product-owned accounting around the Embassy network driver boundary.
//!
//! The upstream Wi-Fi driver deliberately keeps its in-flight credit counter
//! private. These counters do not duplicate that state. They record what the
//! Embassy runner can observe at the public driver boundary so a stalled TCP
//! write or raw-DNS egress deadline can be classified without patching
//! `esp-radio`: whether transmit tokens remained available, whether tokens were
//! consumed, and whether ARP, IPv4, or other Ethernet frames crossed the
//! boundary in either direction.

use core::sync::atomic::{AtomicU32, Ordering};

const ETHERNET_HEADER_BYTES: usize = 14;
const ETHERTYPE_OFFSET: usize = 12;
const ETHERTYPE_ARP: u16 = 0x0806;
const ETHERTYPE_IPV4: u16 = 0x0800;

/// Coarse, payload-free classification of one Ethernet frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetFrameKind {
    /// Address Resolution Protocol.
    Arp,
    /// Internet Protocol version 4.
    Ipv4,
    /// A short frame or any other EtherType.
    Other,
}

/// Classify one Ethernet frame without retaining or logging its contents.
pub fn classify_ethernet_frame(frame: &[u8]) -> EthernetFrameKind {
    if frame.len() < ETHERNET_HEADER_BYTES {
        return EthernetFrameKind::Other;
    }
    match u16::from_be_bytes([frame[ETHERTYPE_OFFSET], frame[ETHERTYPE_OFFSET + 1]]) {
        ETHERTYPE_ARP => EthernetFrameKind::Arp,
        ETHERTYPE_IPV4 => EthernetFrameKind::Ipv4,
        _ => EthernetFrameKind::Other,
    }
}

/// Copy-safe cumulative driver-boundary counters.
///
/// Counters intentionally wrap. [`Self::wrapping_delta_since`] preserves a
/// correct bounded interval across one wrap, which is sufficient for the
/// short TCP and DNS diagnostic deadlines that consume these snapshots.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WifiDriverMetricsSnapshot {
    /// Direct transmit polls that returned a token.
    pub tx_poll_some: u32,
    /// Direct transmit polls that found no token available.
    pub tx_poll_none: u32,
    /// Receive polls that returned an RX token and paired reply TX token.
    pub rx_poll_some: u32,
    /// Receive polls that found no frame available.
    pub rx_poll_none: u32,
    /// TX tokens consumed by the network stack.
    pub tx_token_consumes: u32,
    /// Ethernet bytes passed to consumed TX tokens.
    pub tx_bytes: u32,
    /// Consumed outgoing ARP frames.
    pub tx_arp_frames: u32,
    /// Consumed outgoing IPv4 frames.
    pub tx_ipv4_frames: u32,
    /// Consumed outgoing short or other-EtherType frames.
    pub tx_other_frames: u32,
    /// RX tokens consumed by the network stack.
    pub rx_token_consumes: u32,
    /// Ethernet bytes delivered through consumed RX tokens.
    pub rx_bytes: u32,
    /// Consumed incoming ARP frames.
    pub rx_arp_frames: u32,
    /// Consumed incoming IPv4 frames.
    pub rx_ipv4_frames: u32,
    /// Consumed incoming short or other-EtherType frames.
    pub rx_other_frames: u32,
    /// Link-state polls that observed an up link.
    pub link_up_polls: u32,
    /// Link-state polls that observed a down link.
    pub link_down_polls: u32,
}

impl WifiDriverMetricsSnapshot {
    /// Return the wrapping counter delta from an earlier snapshot.
    pub const fn wrapping_delta_since(self, earlier: Self) -> Self {
        Self {
            tx_poll_some: self.tx_poll_some.wrapping_sub(earlier.tx_poll_some),
            tx_poll_none: self.tx_poll_none.wrapping_sub(earlier.tx_poll_none),
            rx_poll_some: self.rx_poll_some.wrapping_sub(earlier.rx_poll_some),
            rx_poll_none: self.rx_poll_none.wrapping_sub(earlier.rx_poll_none),
            tx_token_consumes: self
                .tx_token_consumes
                .wrapping_sub(earlier.tx_token_consumes),
            tx_bytes: self.tx_bytes.wrapping_sub(earlier.tx_bytes),
            tx_arp_frames: self.tx_arp_frames.wrapping_sub(earlier.tx_arp_frames),
            tx_ipv4_frames: self.tx_ipv4_frames.wrapping_sub(earlier.tx_ipv4_frames),
            tx_other_frames: self.tx_other_frames.wrapping_sub(earlier.tx_other_frames),
            rx_token_consumes: self
                .rx_token_consumes
                .wrapping_sub(earlier.rx_token_consumes),
            rx_bytes: self.rx_bytes.wrapping_sub(earlier.rx_bytes),
            rx_arp_frames: self.rx_arp_frames.wrapping_sub(earlier.rx_arp_frames),
            rx_ipv4_frames: self.rx_ipv4_frames.wrapping_sub(earlier.rx_ipv4_frames),
            rx_other_frames: self.rx_other_frames.wrapping_sub(earlier.rx_other_frames),
            link_up_polls: self.link_up_polls.wrapping_sub(earlier.link_up_polls),
            link_down_polls: self.link_down_polls.wrapping_sub(earlier.link_down_polls),
        }
    }
}

/// Lock-free cumulative metrics shared by the network runner and TCP actor.
pub struct WifiDriverMetrics {
    tx_poll_some: AtomicU32,
    tx_poll_none: AtomicU32,
    rx_poll_some: AtomicU32,
    rx_poll_none: AtomicU32,
    tx_token_consumes: AtomicU32,
    tx_bytes: AtomicU32,
    tx_arp_frames: AtomicU32,
    tx_ipv4_frames: AtomicU32,
    tx_other_frames: AtomicU32,
    rx_token_consumes: AtomicU32,
    rx_bytes: AtomicU32,
    rx_arp_frames: AtomicU32,
    rx_ipv4_frames: AtomicU32,
    rx_other_frames: AtomicU32,
    link_up_polls: AtomicU32,
    link_down_polls: AtomicU32,
}

impl WifiDriverMetrics {
    /// Construct zeroed counters suitable for static storage.
    pub const fn new() -> Self {
        Self {
            tx_poll_some: AtomicU32::new(0),
            tx_poll_none: AtomicU32::new(0),
            rx_poll_some: AtomicU32::new(0),
            rx_poll_none: AtomicU32::new(0),
            tx_token_consumes: AtomicU32::new(0),
            tx_bytes: AtomicU32::new(0),
            tx_arp_frames: AtomicU32::new(0),
            tx_ipv4_frames: AtomicU32::new(0),
            tx_other_frames: AtomicU32::new(0),
            rx_token_consumes: AtomicU32::new(0),
            rx_bytes: AtomicU32::new(0),
            rx_arp_frames: AtomicU32::new(0),
            rx_ipv4_frames: AtomicU32::new(0),
            rx_other_frames: AtomicU32::new(0),
            link_up_polls: AtomicU32::new(0),
            link_down_polls: AtomicU32::new(0),
        }
    }

    /// Read one diagnostic snapshot.
    ///
    /// The runner may advance counters between individual relaxed loads. The
    /// consumer only uses monotonic deltas over multi-second deadlines, so no
    /// cross-counter transactional guarantee is required.
    pub fn snapshot(&self) -> WifiDriverMetricsSnapshot {
        WifiDriverMetricsSnapshot {
            tx_poll_some: self.tx_poll_some.load(Ordering::Relaxed),
            tx_poll_none: self.tx_poll_none.load(Ordering::Relaxed),
            rx_poll_some: self.rx_poll_some.load(Ordering::Relaxed),
            rx_poll_none: self.rx_poll_none.load(Ordering::Relaxed),
            tx_token_consumes: self.tx_token_consumes.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            tx_arp_frames: self.tx_arp_frames.load(Ordering::Relaxed),
            tx_ipv4_frames: self.tx_ipv4_frames.load(Ordering::Relaxed),
            tx_other_frames: self.tx_other_frames.load(Ordering::Relaxed),
            rx_token_consumes: self.rx_token_consumes.load(Ordering::Relaxed),
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            rx_arp_frames: self.rx_arp_frames.load(Ordering::Relaxed),
            rx_ipv4_frames: self.rx_ipv4_frames.load(Ordering::Relaxed),
            rx_other_frames: self.rx_other_frames.load(Ordering::Relaxed),
            link_up_polls: self.link_up_polls.load(Ordering::Relaxed),
            link_down_polls: self.link_down_polls.load(Ordering::Relaxed),
        }
    }
}

#[cfg(any(test, all(target_arch = "xtensa", feature = "wifi-station-proof")))]
impl WifiDriverMetrics {
    fn record_tx_poll(&self, available: bool) {
        if available {
            self.tx_poll_some.fetch_add(1, Ordering::Relaxed);
        } else {
            self.tx_poll_none.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_rx_poll(&self, available: bool) {
        if available {
            self.rx_poll_some.fetch_add(1, Ordering::Relaxed);
        } else {
            self.rx_poll_none.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_link_poll(&self, up: bool) {
        if up {
            self.link_up_polls.fetch_add(1, Ordering::Relaxed);
        } else {
            self.link_down_polls.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_tx_frame(&self, frame: &[u8]) {
        self.tx_token_consumes.fetch_add(1, Ordering::Relaxed);
        self.tx_bytes
            .fetch_add(frame.len() as u32, Ordering::Relaxed);
        match classify_ethernet_frame(frame) {
            EthernetFrameKind::Arp => &self.tx_arp_frames,
            EthernetFrameKind::Ipv4 => &self.tx_ipv4_frames,
            EthernetFrameKind::Other => &self.tx_other_frames,
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    fn record_rx_frame(&self, frame: &[u8]) {
        self.rx_token_consumes.fetch_add(1, Ordering::Relaxed);
        self.rx_bytes
            .fetch_add(frame.len() as u32, Ordering::Relaxed);
        match classify_ethernet_frame(frame) {
            EthernetFrameKind::Arp => &self.rx_arp_frames,
            EthernetFrameKind::Ipv4 => &self.rx_ipv4_frames,
            EthernetFrameKind::Other => &self.rx_other_frames,
        }
        .fetch_add(1, Ordering::Relaxed);
    }
}

impl Default for WifiDriverMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Sole metrics instance for the product Wi-Fi station driver.
pub static WIFI_DRIVER_METRICS: WifiDriverMetrics = WifiDriverMetrics::new();

#[cfg(all(target_arch = "xtensa", feature = "wifi-station-proof"))]
mod instrumented {
    use core::task::Context;

    use embassy_net::driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};

    use super::WifiDriverMetrics;

    /// Transparent Embassy network driver wrapper with product-owned metrics.
    pub struct InstrumentedWifiDriver<D> {
        inner: D,
        metrics: &'static WifiDriverMetrics,
    }

    impl<D> InstrumentedWifiDriver<D> {
        /// Wrap one driver and retain its metrics authority.
        pub const fn new(inner: D, metrics: &'static WifiDriverMetrics) -> Self {
            Self { inner, metrics }
        }
    }

    /// Receive token that records only byte counts and coarse EtherType.
    pub struct InstrumentedRxToken<T> {
        inner: T,
        metrics: &'static WifiDriverMetrics,
    }

    impl<T: RxToken> RxToken for InstrumentedRxToken<T> {
        fn consume<R, F>(self, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R,
        {
            self.inner.consume(|frame| {
                self.metrics.record_rx_frame(frame);
                f(frame)
            })
        }
    }

    /// Transmit token that records only byte counts and coarse EtherType.
    pub struct InstrumentedTxToken<T> {
        inner: T,
        metrics: &'static WifiDriverMetrics,
    }

    impl<T: TxToken> TxToken for InstrumentedTxToken<T> {
        fn consume<R, F>(self, len: usize, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R,
        {
            self.inner.consume(len, |frame| {
                let result = f(frame);
                self.metrics.record_tx_frame(frame);
                result
            })
        }
    }

    impl<D: Driver> Driver for InstrumentedWifiDriver<D> {
        type RxToken<'a>
            = InstrumentedRxToken<D::RxToken<'a>>
        where
            Self: 'a;
        type TxToken<'a>
            = InstrumentedTxToken<D::TxToken<'a>>
        where
            Self: 'a;

        fn receive(
            &mut self,
            context: &mut Context<'_>,
        ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
            let metrics = self.metrics;
            match self.inner.receive(context) {
                Some((rx, tx)) => {
                    metrics.record_rx_poll(true);
                    Some((
                        InstrumentedRxToken { inner: rx, metrics },
                        InstrumentedTxToken { inner: tx, metrics },
                    ))
                }
                None => {
                    metrics.record_rx_poll(false);
                    None
                }
            }
        }

        fn transmit(&mut self, context: &mut Context<'_>) -> Option<Self::TxToken<'_>> {
            let metrics = self.metrics;
            match self.inner.transmit(context) {
                Some(token) => {
                    metrics.record_tx_poll(true);
                    Some(InstrumentedTxToken {
                        inner: token,
                        metrics,
                    })
                }
                None => {
                    metrics.record_tx_poll(false);
                    None
                }
            }
        }

        fn link_state(&mut self, context: &mut Context<'_>) -> LinkState {
            let state = self.inner.link_state(context);
            self.metrics.record_link_poll(state == LinkState::Up);
            state
        }

        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }

        fn hardware_address(&self) -> HardwareAddress {
            self.inner.hardware_address()
        }
    }
}

#[cfg(all(target_arch = "xtensa", feature = "wifi-station-proof"))]
pub use instrumented::InstrumentedWifiDriver;

#[cfg(test)]
mod tests {
    use super::{
        ETHERNET_HEADER_BYTES, ETHERTYPE_ARP, ETHERTYPE_IPV4, ETHERTYPE_OFFSET, EthernetFrameKind,
        WifiDriverMetrics, WifiDriverMetricsSnapshot, classify_ethernet_frame,
    };

    fn ethernet_frame(ethertype: u16, payload_bytes: usize) -> std::vec::Vec<u8> {
        let mut frame = std::vec![0; ETHERNET_HEADER_BYTES + payload_bytes];
        frame[ETHERTYPE_OFFSET..ETHERTYPE_OFFSET + 2].copy_from_slice(&ethertype.to_be_bytes());
        frame
    }

    #[test]
    fn ethernet_classification_is_payload_free_and_explicit() {
        assert_eq!(
            classify_ethernet_frame(&ethernet_frame(ETHERTYPE_ARP, 28)),
            EthernetFrameKind::Arp
        );
        assert_eq!(
            classify_ethernet_frame(&ethernet_frame(ETHERTYPE_IPV4, 20)),
            EthernetFrameKind::Ipv4
        );
        assert_eq!(
            classify_ethernet_frame(&ethernet_frame(0x86dd, 40)),
            EthernetFrameKind::Other
        );
        assert_eq!(
            classify_ethernet_frame(&[0; ETHERNET_HEADER_BYTES - 1]),
            EthernetFrameKind::Other
        );
    }

    #[test]
    fn accounting_separates_poll_availability_direction_and_ethertype() {
        let metrics = WifiDriverMetrics::new();
        let before = metrics.snapshot();

        metrics.record_tx_poll(true);
        metrics.record_tx_poll(false);
        metrics.record_rx_poll(true);
        metrics.record_rx_poll(false);
        metrics.record_link_poll(true);
        metrics.record_link_poll(false);
        metrics.record_tx_frame(&ethernet_frame(ETHERTYPE_ARP, 28));
        metrics.record_tx_frame(&ethernet_frame(ETHERTYPE_IPV4, 20));
        metrics.record_rx_frame(&ethernet_frame(ETHERTYPE_ARP, 28));
        metrics.record_rx_frame(&ethernet_frame(0x86dd, 40));

        assert_eq!(
            metrics.snapshot().wrapping_delta_since(before),
            WifiDriverMetricsSnapshot {
                tx_poll_some: 1,
                tx_poll_none: 1,
                rx_poll_some: 1,
                rx_poll_none: 1,
                tx_token_consumes: 2,
                tx_bytes: 76,
                tx_arp_frames: 1,
                tx_ipv4_frames: 1,
                tx_other_frames: 0,
                rx_token_consumes: 2,
                rx_bytes: 96,
                rx_arp_frames: 1,
                rx_ipv4_frames: 0,
                rx_other_frames: 1,
                link_up_polls: 1,
                link_down_polls: 1,
            }
        );
    }

    #[test]
    fn snapshot_deltas_remain_correct_across_counter_wrap() {
        let earlier = WifiDriverMetricsSnapshot {
            tx_poll_some: u32::MAX,
            rx_bytes: u32::MAX - 1,
            ..WifiDriverMetricsSnapshot::default()
        };
        let later = WifiDriverMetricsSnapshot {
            tx_poll_some: 1,
            rx_bytes: 3,
            ..WifiDriverMetricsSnapshot::default()
        };
        let delta = later.wrapping_delta_since(earlier);
        assert_eq!(delta.tx_poll_some, 2);
        assert_eq!(delta.rx_bytes, 5);
    }
}
