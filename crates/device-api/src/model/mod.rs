//! Network-engine-independent logical API model and authorization vocabulary.

use core::{convert::Infallible, marker::PhantomData, num::NonZeroU16, ops::BitOr};

mod appliance_label;
pub use appliance_label::*;
#[cfg(feature = "lxmf")]
mod lxmf;
#[cfg(feature = "lxmf")]
pub use lxmf::*;
#[cfg(feature = "nomad")]
mod nomad;
#[cfg(feature = "nomad")]
pub use nomad::*;
#[cfg(feature = "rns-data")]
mod rns_data;
#[cfg(feature = "rns-data")]
pub use rns_data::*;
#[cfg(feature = "network-config")]
mod network_config;
#[cfg(feature = "network-config")]
pub use network_config::*;

/// Current incompatible device API generation.
pub const API_VERSION_MAJOR: u16 = 6;
/// Current compatible feature revision within the API generation.
pub const API_VERSION_MINOR: u16 = 0;

/// Maximum size of one decoded or encoded logical CBOR message.
pub const MAX_MESSAGE_BYTES: usize = 512;
/// Maximum encoded size of the operation-specific body within a message.
///
/// The remaining 32 bytes accommodate the versioned envelope around the body.
pub const MAX_BODY_BYTES: usize = 480;
/// Maximum payload accepted by the RNS DATA submission request.
pub const MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES: usize = 383;
/// Structural per-field title limit accepted by the basic-LXMF codec.
///
/// The encoded body and product composer can impose a lower limit on a
/// particular title/content combination.
pub const MAX_LXMF_BASIC_TITLE_BYTES: usize = 295;
/// Structural per-field content limit accepted by the basic-LXMF codec.
///
/// The encoded body and product composer can impose a lower limit on a
/// particular title/content combination.
pub const MAX_LXMF_BASIC_CONTENT_BYTES: usize = 295;
/// Maximum authenticated announce application data returned for one nearby LXMF peer.
pub const MAX_LXMF_PEER_APP_DATA_BYTES: usize = 256;
/// Largest UTF-8 NomadNet request path accepted by the fetch API.
pub const MAX_NOMAD_PAGE_PATH_BYTES: usize = 128;
/// Largest valid UTF-8 Micron page returned by the fetch API.
pub const MAX_NOMAD_PAGE_BYTES: usize = 400;
/// Maximum Wi-Fi SSID length in bytes.
pub const MAX_WIFI_SSID_BYTES: usize = 32;
/// Maximum saved Wi-Fi station profiles exposed by the device API.
pub const MAX_WIFI_NETWORK_PROFILES: usize = 4;
/// Maximum interface records returned by one node-diagnostics snapshot.
pub const MAX_DIAGNOSTIC_INTERFACES: usize = 4;
/// Maximum UTF-8 bytes retained for one PRNS interface failure reason.
pub const MAX_DIAGNOSTIC_FAILURE_REASON_BYTES: usize = 48;
/// Maximum route records returned by one diagnostics page.
pub const MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES: usize = 4;
/// Maximum radio-trace events returned by one diagnostics page.
pub const MAX_RADIO_TRACE_PAGE_ENTRIES: usize = 2;
/// Maximum ASCII DNS hostname length for one outbound Reticulum TCP peer.
pub const MAX_RETICULUM_TCP_PEER_HOSTNAME_BYTES: usize = 96;
/// Maximum encoded UTF-8 product-owned appliance label length.
pub const MAX_APPLIANCE_LABEL_BYTES: usize = 32;
/// Minimum WPA2-Personal passphrase length in bytes.
pub const MIN_WIFI_PASSPHRASE_BYTES: usize = 8;
/// Maximum WPA2-Personal passphrase length in bytes.
pub const MAX_WIFI_PASSPHRASE_BYTES: usize = 63;
/// Conventional Reticulum TCP interface port.
pub const DEFAULT_RETICULUM_TCP_PORT: u16 = 4242;
/// Largest JavaScript-safe whole-millisecond request timestamp.
///
/// Converting extreme accepted values to binary64 seconds can lose
/// millisecond precision. Contemporary Unix dates retain millisecond
/// precision; the wire bound promises integer interchange, not exact
/// binary64 spacing across the complete range.
pub const MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS: u64 = (1_u64 << 53) - 1;

/// `system.capabilities` operation number.
pub const OP_SYSTEM_CAPABILITIES: u16 = 0x0001;
/// `submission.status` operation number.
pub const OP_SUBMISSION_STATUS: u16 = 0x0002;
/// `identity.summary` operation number.
pub const OP_IDENTITY_SUMMARY: u16 = 0x0003;
/// Error response kind used instead of a successful operation number.
pub const RESPONSE_ERROR: u16 = 0x0000;
/// Queue the node's ordinary Reticulum service announces immediately.
pub const OP_MANUAL_SERVICE_ANNOUNCE: u16 = 0xf00d;
/// Read one bounded cross-interface node diagnostics snapshot.
pub const OP_NODE_DIAGNOSTICS: u16 = 0xf00e;
/// Read one bounded lexicographically ordered Reticulum route page.
pub const OP_ROUTE_DIAGNOSTICS_PAGE: u16 = 0xf00f;
/// Begin one boot-scoped Reticulum path-and-proof probe.
pub const OP_RETICULUM_PROBE_START: u16 = 0xf012;
/// Poll one principal-owned Reticulum path-and-proof probe.
pub const OP_RETICULUM_PROBE_POLL: u16 = 0xf013;
/// Read one bounded boot-scoped packet-correlated radio trace page.
pub const OP_RADIO_TRACE_PAGE: u16 = 0xf014;

/// Major/minor logical protocol version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApiVersion {
    /// Incompatible protocol generation.
    pub major: u16,
    /// Compatible feature revision within a major generation.
    pub minor: u16,
}

impl ApiVersion {
    /// Version implemented by this crate.
    pub const CURRENT: Self = Self {
        major: API_VERSION_MAJOR,
        minor: API_VERSION_MINOR,
    };
}

/// Client-chosen identifier echoed in the corresponding response.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(pub u64);

/// Authenticated local-client principal derived from device-owned authority.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PrincipalId(pub [u8; 16]);

/// Client-chosen key used to deduplicate a state-changing operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IdempotencyKey(pub [u8; 16]);

/// Complete 128-bit Reticulum destination hash.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DestinationHash(pub [u8; 16]);

/// Public 128-bit hash of a Reticulum identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IdentityHash([u8; 16]);

impl IdentityHash {
    /// Construct a public identity hash from all wire bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow all public hash bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Opaque identity of one Reticulum packet interface.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReticulumInterfaceId([u8; 8]);

impl ReticulumInterfaceId {
    /// Construct an interface identity from all PRNS/RNS bytes.
    pub const fn new(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete opaque identity.
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

/// Physical or logical transport family represented by diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticInterfaceKind {
    /// A LoRa packet-radio interface.
    LoRa,
    /// An outbound Reticulum TCP client interface.
    TcpClient,
    /// Another transport family not yet represented by a stable category.
    Other,
    /// An inbound Reticulum TCP server interface.
    TcpServer,
}

impl DiagnosticInterfaceKind {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::LoRa => 0,
            Self::TcpClient => 1,
            Self::Other => 2,
            Self::TcpServer => 3,
        }
    }
}

/// Current usable state of one configured interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticInterfaceState {
    /// The interface owner is starting.
    Initializing,
    /// The interface is connected.
    Connected,
    /// The interface is usable with degraded service.
    Degraded,
    /// The interface is reconnecting.
    Reconnecting,
    /// The interface owner failed.
    Failed,
    /// The interface is disconnected.
    Disconnected,
    /// The interface is deliberately disabled.
    Disabled,
    /// The interface owner cannot classify its current state.
    Unknown,
}

impl DiagnosticInterfaceState {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Initializing => 0,
            Self::Connected => 1,
            Self::Degraded => 2,
            Self::Reconnecting => 3,
            Self::Failed => 4,
            Self::Disconnected => 5,
            Self::Disabled => 6,
            Self::Unknown => 255,
        }
    }
}

/// PRNS interface forwarding mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticInterfaceMode {
    /// Ordinary full interface.
    Full,
    /// Point-to-point interface.
    PointToPoint,
    /// Access-point interface.
    AccessPoint,
    /// Roaming interface.
    Roaming,
    /// Boundary interface.
    Boundary,
    /// Gateway interface.
    Gateway,
    /// Internal transport interface.
    Internal,
}

impl DiagnosticInterfaceMode {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Full => 0,
            Self::PointToPoint => 1,
            Self::AccessPoint => 2,
            Self::Roaming => 3,
            Self::Boundary => 4,
            Self::Gateway => 5,
            Self::Internal => 6,
        }
    }
}

/// Bounded UTF-8 PRNS interface failure reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticInterfaceFailureReason {
    bytes: [u8; MAX_DIAGNOSTIC_FAILURE_REASON_BYTES],
    len: u8,
}

impl DiagnosticInterfaceFailureReason {
    /// Copy one complete reason when it fits the stable device boundary.
    pub fn try_from_str(reason: &str) -> Option<Self> {
        if reason.len() > MAX_DIAGNOSTIC_FAILURE_REASON_BYTES {
            return None;
        }
        let mut bytes = [0; MAX_DIAGNOSTIC_FAILURE_REASON_BYTES];
        bytes[..reason.len()].copy_from_slice(reason.as_bytes());
        Some(Self {
            bytes,
            len: reason.len() as u8,
        })
    }

    /// Validated UTF-8 reason.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("the constructor stores validated UTF-8")
    }
}

/// One fixed-capacity interface record in a node diagnostics snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticInterfaceRecord {
    id: ReticulumInterfaceId,
    mode: DiagnosticInterfaceMode,
    state: DiagnosticInterfaceState,
    failure_reason: Option<DiagnosticInterfaceFailureReason>,
    rx_bytes: u64,
    tx_bytes: u64,
    destinations: u32,
    links: u32,
    transported_links: u32,
    supervisor: Option<ReticulumInterfaceId>,
}

impl DiagnosticInterfaceRecord {
    /// Construct one complete interface record.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        id: ReticulumInterfaceId,
        mode: DiagnosticInterfaceMode,
        state: DiagnosticInterfaceState,
        failure_reason: Option<DiagnosticInterfaceFailureReason>,
        rx_bytes: u64,
        tx_bytes: u64,
        destinations: u32,
        links: u32,
        transported_links: u32,
        supervisor: Option<ReticulumInterfaceId>,
    ) -> Self {
        Self {
            id,
            mode,
            state,
            failure_reason,
            rx_bytes,
            tx_bytes,
            destinations,
            links,
            transported_links,
            supervisor,
        }
    }

    /// Product-owned interface identifier.
    pub const fn id(self) -> ReticulumInterfaceId {
        self.id
    }

    /// PRNS forwarding mode.
    pub const fn mode(self) -> DiagnosticInterfaceMode {
        self.mode
    }

    /// Current usable state.
    pub const fn state(self) -> DiagnosticInterfaceState {
        self.state
    }

    /// Current PRNS failure reason, when supplied and bounded.
    pub const fn failure_reason(self) -> Option<DiagnosticInterfaceFailureReason> {
        self.failure_reason
    }

    /// Bytes received by this interface owner.
    pub const fn rx_bytes(self) -> u64 {
        self.rx_bytes
    }

    /// Bytes transmitted by this interface owner.
    pub const fn tx_bytes(self) -> u64 {
        self.tx_bytes
    }

    /// Destinations currently attributed to this interface by PRNS.
    pub const fn destinations(self) -> u32 {
        self.destinations
    }

    /// Links currently attributed to this interface by PRNS.
    pub const fn links(self) -> u32 {
        self.links
    }

    /// Transported links currently attributed to this interface by PRNS.
    pub const fn transported_links(self) -> u32 {
        self.transported_links
    }

    /// Supervisor interface for a fleet member, otherwise `None`.
    pub const fn supervisor(self) -> Option<ReticulumInterfaceId> {
        self.supervisor
    }
}

/// Stable terminal category for the most recent LoRa transmission job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticLoraTxOutcome {
    /// Every physical frame in the job completed successfully.
    Completed,
    /// Channel-access policy rejected the job before successful completion.
    AccessRejected,
    /// Radio setup, transmission, or completion failed.
    Failed,
}

impl DiagnosticLoraTxOutcome {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Completed => 0,
            Self::AccessRejected => 1,
            Self::Failed => 2,
        }
    }
}

/// Packet-owner family of one terminal LoRa dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticLoraTxFamily {
    /// Destination DATA associated with a durable application attempt.
    Data,
    /// Ordinary Reticulum control or application packet.
    Ordinary,
}

impl DiagnosticLoraTxFamily {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Data => 0,
            Self::Ordinary => 1,
        }
    }
}

/// Prepared-packet identity for one terminal LoRa DATA dispatch.
///
/// This evidence is available for pre-authorization failures and therefore
/// does not assert RF exposure. Length and digest intentionally match the
/// message timeline's encoded-packet evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticLoraDataTxEvidence {
    interface_id: ReticulumInterfaceId,
    encoded_packet_len: NonZeroU16,
    encoded_packet_sha256: EncodedPacketSha256,
}

impl DiagnosticLoraDataTxEvidence {
    /// Construct exact prepared DATA packet evidence.
    ///
    /// A complete encoded Reticulum packet cannot be empty.
    pub const fn try_new(
        interface_id: ReticulumInterfaceId,
        encoded_packet_len: u16,
        encoded_packet_sha256: EncodedPacketSha256,
    ) -> Option<Self> {
        let Some(encoded_packet_len) = NonZeroU16::new(encoded_packet_len) else {
            return None;
        };
        Some(Self {
            interface_id,
            encoded_packet_len,
            encoded_packet_sha256,
        })
    }

    /// Exact Reticulum interface selected for this dispatch attempt.
    pub const fn interface_id(self) -> ReticulumInterfaceId {
        self.interface_id
    }

    /// Complete encoded interface-packet length.
    pub const fn encoded_packet_len(self) -> u16 {
        self.encoded_packet_len.get()
    }

    /// SHA-256 over every byte in the complete encoded interface packet.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }
}

/// Conservative signal metadata for the most recently accepted logical LoRa
/// packet.
///
/// A single-frame packet reports that frame. A split packet reports the
/// field-wise weaker RSSI and SNR across both frames. A later invalid or
/// over-MTU physical frame does not replace this record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticLoraLastRx {
    age_ms: u64,
    rssi_dbm: i16,
    snr_db: i16,
}

impl DiagnosticLoraLastRx {
    /// Construct one conservative signal observation for an accepted packet.
    pub const fn new(age_ms: u64, rssi_dbm: i16, snr_db: i16) -> Self {
        Self {
            age_ms,
            rssi_dbm,
            snr_db,
        }
    }

    /// Saturating observation age at snapshot time.
    pub const fn age_ms(self) -> u64 {
        self.age_ms
    }

    /// Whole-dBm received signal strength.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Whole-dB signal-to-noise ratio.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }
}

/// Terminal metadata for the most recent LoRa transmission job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticLoraLastTx {
    age_ms: u64,
    outcome: DiagnosticLoraTxOutcome,
    family: Option<DiagnosticLoraTxFamily>,
    data: Option<DiagnosticLoraDataTxEvidence>,
}

/// Most recent terminal DATA dispatch retained across later ordinary packets.
///
/// This dedicated type makes the LoRa key-18 slot incapable of containing an
/// ordinary last-TX record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticLoraLastDataTx {
    age_ms: u64,
    outcome: DiagnosticLoraTxOutcome,
    data: DiagnosticLoraDataTxEvidence,
}

impl DiagnosticLoraLastDataTx {
    /// Construct one retained DATA terminal observation.
    pub const fn new(
        age_ms: u64,
        outcome: DiagnosticLoraTxOutcome,
        data: DiagnosticLoraDataTxEvidence,
    ) -> Self {
        Self {
            age_ms,
            outcome,
            data,
        }
    }

    /// Saturating terminal-event age at snapshot time.
    pub const fn age_ms(self) -> u64 {
        self.age_ms
    }

    /// Stable terminal result category.
    pub const fn outcome(self) -> DiagnosticLoraTxOutcome {
        self.outcome
    }

    /// Exact prepared DATA packet evidence.
    pub const fn data_evidence(self) -> DiagnosticLoraDataTxEvidence {
        self.data
    }
}

impl DiagnosticLoraLastTx {
    /// Construct a DATA observation whose detailed packet evidence was lost.
    pub const fn data_without_evidence(age_ms: u64, outcome: DiagnosticLoraTxOutcome) -> Self {
        Self {
            age_ms,
            outcome,
            family: Some(DiagnosticLoraTxFamily::Data),
            data: None,
        }
    }

    /// Construct one ordinary-packet terminal observation.
    pub const fn ordinary(age_ms: u64, outcome: DiagnosticLoraTxOutcome) -> Self {
        Self {
            age_ms,
            outcome,
            family: Some(DiagnosticLoraTxFamily::Ordinary),
            data: None,
        }
    }

    /// Construct one DATA terminal observation with prepared-packet evidence.
    pub const fn data(
        age_ms: u64,
        outcome: DiagnosticLoraTxOutcome,
        data: DiagnosticLoraDataTxEvidence,
    ) -> Self {
        Self {
            age_ms,
            outcome,
            family: Some(DiagnosticLoraTxFamily::Data),
            data: Some(data),
        }
    }

    pub(crate) const fn from_wire(
        age_ms: u64,
        outcome: DiagnosticLoraTxOutcome,
        family: Option<DiagnosticLoraTxFamily>,
        data: Option<DiagnosticLoraDataTxEvidence>,
    ) -> Self {
        Self {
            age_ms,
            outcome,
            family,
            data,
        }
    }

    /// Saturating terminal-event age at snapshot time.
    pub const fn age_ms(self) -> u64 {
        self.age_ms
    }

    /// Stable terminal result category.
    pub const fn outcome(self) -> DiagnosticLoraTxOutcome {
        self.outcome
    }

    /// Packet-owner family, absent only when decoding a malformed record.
    pub const fn family(self) -> Option<DiagnosticLoraTxFamily> {
        self.family
    }

    /// Prepared DATA packet evidence, present only for a DATA record.
    pub const fn data_evidence(self) -> Option<DiagnosticLoraDataTxEvidence> {
        self.data
    }
}

/// Bounded LoRa radio and scheduler diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoraDiagnostics {
    applied_tx_power_dbm: i16,
    frequency_hz: u32,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    rx_physical_frames: u64,
    rx_packets: u64,
    rx_errors: u64,
    rx_drops: u64,
    tx_terminal_jobs: u64,
    tx_successes: u64,
    tx_completed_frames: u64,
    tx_access_rejects: u64,
    tx_failures: u64,
    cad_busy: u64,
    cad_clear: u64,
    last_rx: Option<DiagnosticLoraLastRx>,
    last_tx: Option<DiagnosticLoraLastTx>,
    last_data_tx: Option<DiagnosticLoraLastDataTx>,
}

impl LoraDiagnostics {
    /// Construct one complete LoRa diagnostics record.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        applied_tx_power_dbm: i16,
        frequency_hz: u32,
        bandwidth_hz: u32,
        spreading_factor: u8,
        coding_rate_denominator: u8,
        rx_physical_frames: u64,
        rx_packets: u64,
        rx_errors: u64,
        rx_drops: u64,
        tx_terminal_jobs: u64,
        tx_successes: u64,
        tx_completed_frames: u64,
        tx_access_rejects: u64,
        tx_failures: u64,
        cad_busy: u64,
        cad_clear: u64,
        last_rx: Option<DiagnosticLoraLastRx>,
        last_tx: Option<DiagnosticLoraLastTx>,
        last_data_tx: Option<DiagnosticLoraLastDataTx>,
    ) -> Self {
        Self {
            applied_tx_power_dbm,
            frequency_hz,
            bandwidth_hz,
            spreading_factor,
            coding_rate_denominator,
            rx_physical_frames,
            rx_packets,
            rx_errors,
            rx_drops,
            tx_terminal_jobs,
            tx_successes,
            tx_completed_frames,
            tx_access_rejects,
            tx_failures,
            cad_busy,
            cad_clear,
            last_rx,
            last_tx,
            last_data_tx,
        }
    }

    /// Applied whole-dBm radio output setting.
    pub const fn applied_tx_power_dbm(self) -> i16 {
        self.applied_tx_power_dbm
    }

    /// Applied carrier center frequency.
    pub const fn frequency_hz(self) -> u32 {
        self.frequency_hz
    }

    /// Applied LoRa bandwidth.
    pub const fn bandwidth_hz(self) -> u32 {
        self.bandwidth_hz
    }

    /// Applied LoRa spreading factor.
    pub const fn spreading_factor(self) -> u8 {
        self.spreading_factor
    }

    /// Denominator of the applied LoRa coding rate.
    pub const fn coding_rate_denominator(self) -> u8 {
        self.coding_rate_denominator
    }

    /// Physical receive frames presented by the radio.
    pub const fn rx_physical_frames(self) -> u64 {
        self.rx_physical_frames
    }

    /// Reticulum packets reconstructed from received physical frames.
    pub const fn rx_packets(self) -> u64 {
        self.rx_packets
    }

    /// Receive operations ending in radio or decode error.
    pub const fn rx_errors(self) -> u64 {
        self.rx_errors
    }

    /// Received frames or packets dropped after radio delivery.
    pub const fn rx_drops(self) -> u64 {
        self.rx_drops
    }

    /// Transmission jobs reaching a terminal result.
    pub const fn tx_terminal_jobs(self) -> u64 {
        self.tx_terminal_jobs
    }

    /// Terminal jobs that completed successfully.
    pub const fn tx_successes(self) -> u64 {
        self.tx_successes
    }

    /// Physical frames completed across successful or partially completed jobs.
    pub const fn tx_completed_frames(self) -> u64 {
        self.tx_completed_frames
    }

    /// Jobs rejected by channel-access policy.
    pub const fn tx_access_rejects(self) -> u64 {
        self.tx_access_rejects
    }

    /// Jobs ending in another radio or scheduler failure.
    pub const fn tx_failures(self) -> u64 {
        self.tx_failures
    }

    /// Channel-activity detections reporting a busy channel.
    pub const fn cad_busy(self) -> u64 {
        self.cad_busy
    }

    /// Channel-activity detections reporting a clear channel.
    pub const fn cad_clear(self) -> u64 {
        self.cad_clear
    }

    /// Conservative signal observation for the most recently accepted packet.
    pub const fn last_rx(self) -> Option<DiagnosticLoraLastRx> {
        self.last_rx
    }

    /// Most recent terminal transmission observation.
    pub const fn last_tx(self) -> Option<DiagnosticLoraLastTx> {
        self.last_tx
    }

    /// Most recent DATA terminal observation retained across ordinary TX.
    ///
    /// Producers may omit this duplicate when [`Self::last_tx`] is itself a
    /// DATA record; host projections recover that equivalent view.
    pub const fn last_data_tx(self) -> Option<DiagnosticLoraLastDataTx> {
        self.last_data_tx
    }
}

/// Reticulum transport and path-table counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RnsDiagnostics {
    received: u64,
    forwarded: u64,
    dedup_drops: u64,
    invalid_drops: u64,
    announces_received: u64,
    paths_learned: u64,
    paths_expired: u64,
    links_established: u64,
    links_closed: u64,
    links_failed: u64,
    route_revision: u64,
}

impl RnsDiagnostics {
    /// Construct one complete Reticulum diagnostics record.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        received: u64,
        forwarded: u64,
        dedup_drops: u64,
        invalid_drops: u64,
        announces_received: u64,
        paths_learned: u64,
        paths_expired: u64,
        links_established: u64,
        links_closed: u64,
        links_failed: u64,
        route_revision: u64,
    ) -> Self {
        Self {
            received,
            forwarded,
            dedup_drops,
            invalid_drops,
            announces_received,
            paths_learned,
            paths_expired,
            links_established,
            links_closed,
            links_failed,
            route_revision,
        }
    }

    /// Packets admitted by the Reticulum owner.
    pub const fn received(self) -> u64 {
        self.received
    }

    /// Packets forwarded by the Reticulum owner.
    pub const fn forwarded(self) -> u64 {
        self.forwarded
    }

    /// Duplicate packets dropped before processing.
    pub const fn dedup_drops(self) -> u64 {
        self.dedup_drops
    }

    /// Structurally or cryptographically invalid packets dropped.
    pub const fn invalid_drops(self) -> u64 {
        self.invalid_drops
    }

    /// Valid announces admitted by the Reticulum owner.
    pub const fn announces_received(self) -> u64 {
        self.announces_received
    }

    /// Route records learned or replaced.
    pub const fn paths_learned(self) -> u64 {
        self.paths_learned
    }

    /// Route records expired or removed.
    pub const fn paths_expired(self) -> u64 {
        self.paths_expired
    }

    /// Stable route-table generation used by diagnostics pagination.
    ///
    /// This is the generation of the retained route snapshot served by the
    /// firmware, not a live path-counter sum.
    pub const fn route_revision(self) -> u64 {
        self.route_revision
    }

    /// Links reaching the established state.
    pub const fn links_established(self) -> u64 {
        self.links_established
    }

    /// Established links closed normally.
    pub const fn links_closed(self) -> u64 {
        self.links_closed
    }

    /// Link establishment attempts ending in failure.
    pub const fn links_failed(self) -> u64 {
        self.links_failed
    }
}

/// Authenticated, bounded cross-interface node diagnostics snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeDiagnosticsSnapshot {
    uptime_ms: u64,
    interfaces: [Option<DiagnosticInterfaceRecord>; MAX_DIAGNOSTIC_INTERFACES],
    lora: Option<LoraDiagnostics>,
    route_count: u32,
    link_count: u32,
}

impl NodeDiagnosticsSnapshot {
    /// Construct one complete node diagnostics snapshot.
    pub const fn new(
        uptime_ms: u64,
        interfaces: [Option<DiagnosticInterfaceRecord>; MAX_DIAGNOSTIC_INTERFACES],
        lora: Option<LoraDiagnostics>,
        route_count: u32,
        link_count: u32,
    ) -> Self {
        Self {
            uptime_ms,
            interfaces,
            lora,
            route_count,
            link_count,
        }
    }

    /// Milliseconds since this node incarnation started.
    pub const fn uptime_ms(self) -> u64 {
        self.uptime_ms
    }

    /// Fixed optional interface slots.
    pub const fn interfaces(
        &self,
    ) -> &[Option<DiagnosticInterfaceRecord>; MAX_DIAGNOSTIC_INTERFACES] {
        &self.interfaces
    }

    /// LoRa-specific diagnostics when a LoRa owner is present.
    pub const fn lora(self) -> Option<LoraDiagnostics> {
        self.lora
    }

    /// Routes visible in PRNS at the instant of the live query.
    pub const fn route_count(self) -> u32 {
        self.route_count
    }

    /// Active links visible in PRNS at the instant of the live query.
    pub const fn link_count(self) -> u32 {
        self.link_count
    }
}

/// Exclusive boot-scoped cursor for radio trace pagination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceCursor {
    boot_id: u64,
    after_sequence: u64,
}

impl RadioTraceCursor {
    /// Bind an exclusive event sequence to the boot that allocated it.
    pub const fn new(boot_id: u64, after_sequence: u64) -> Self {
        Self {
            boot_id,
            after_sequence,
        }
    }

    /// Opaque node-incarnation identifier scoping the sequence.
    pub const fn boot_id(self) -> u64 {
        self.boot_id
    }

    /// Exclusive event sequence within this boot.
    pub const fn after_sequence(self) -> u64 {
        self.after_sequence
    }
}

/// Optional exclusive boot-and-sequence cursor for a radio trace page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTracePageRequest {
    after: Option<RadioTraceCursor>,
}

impl RadioTracePageRequest {
    /// Construct a request beginning after `after`, or at the oldest retained
    /// event when no boot-scoped cursor is supplied.
    pub const fn new(after: Option<RadioTraceCursor>) -> Self {
        Self { after }
    }

    /// Exclusive boot-and-event-sequence cursor.
    pub const fn after(self) -> Option<RadioTraceCursor> {
        self.after
    }
}

/// Immutable LoRa configuration applied for one radio-trace boot.
///
/// The complete board-owned fingerprint is retained alongside human-readable
/// modulation fields so exported traces can detect any configuration mismatch
/// without reverse-engineering the fingerprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceAppliedLoraProfile {
    configuration_fingerprint: [u8; 16],
    frequency_hz: u32,
    bandwidth_hz: u32,
    preamble_symbols: u16,
    requested_power_dbm: i16,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    explicit_header: bool,
    crc: bool,
    iq_inverted: bool,
}

impl RadioTraceAppliedLoraProfile {
    /// Construct the exact immutable profile owned by the running radio actor.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        configuration_fingerprint: [u8; 16],
        frequency_hz: u32,
        bandwidth_hz: u32,
        preamble_symbols: u16,
        requested_power_dbm: i16,
        spreading_factor: u8,
        coding_rate_denominator: u8,
        explicit_header: bool,
        crc: bool,
        iq_inverted: bool,
    ) -> Self {
        Self {
            configuration_fingerprint,
            frequency_hz,
            bandwidth_hz,
            preamble_symbols,
            requested_power_dbm,
            spreading_factor,
            coding_rate_denominator,
            explicit_header,
            crc,
            iq_inverted,
        }
    }

    /// Complete board-owned immutable configuration fingerprint.
    pub const fn configuration_fingerprint(self) -> [u8; 16] {
        self.configuration_fingerprint
    }

    /// Applied carrier center frequency in whole hertz.
    pub const fn frequency_hz(self) -> u32 {
        self.frequency_hz
    }

    /// Applied LoRa bandwidth in whole hertz.
    pub const fn bandwidth_hz(self) -> u32 {
        self.bandwidth_hz
    }

    /// Applied preamble length in symbols.
    pub const fn preamble_symbols(self) -> u16 {
        self.preamble_symbols
    }

    /// Requested radio output in whole dBm, without an antenna-path claim.
    pub const fn requested_power_dbm(self) -> i16 {
        self.requested_power_dbm
    }

    /// Applied LoRa spreading-factor number.
    pub const fn spreading_factor(self) -> u8 {
        self.spreading_factor
    }

    /// Denominator of the applied `4/x` LoRa coding rate.
    pub const fn coding_rate_denominator(self) -> u8 {
        self.coding_rate_denominator
    }

    /// Whether the explicit packet header is enabled.
    pub const fn explicit_header(self) -> bool {
        self.explicit_header
    }

    /// Whether the packet CRC is enabled.
    pub const fn crc(self) -> bool {
        self.crc
    }

    /// Whether LoRa IQ polarity is inverted.
    pub const fn iq_inverted(self) -> bool {
        self.iq_inverted
    }
}

/// Hop-invariant Reticulum proof-correlation hash for one traced packet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RadioTraceAttemptToken([u8; 32]);

impl RadioTraceAttemptToken {
    /// Construct a token from all proof-correlation hash bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow all proof-correlation hash bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Packet identity common to transmit and receive trace events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTracePacketEvidence {
    interface_id: ReticulumInterfaceId,
    packet_len: NonZeroU16,
    encoded_packet_sha256: EncodedPacketSha256,
    attempt_token: Option<RadioTraceAttemptToken>,
}

impl RadioTracePacketEvidence {
    /// Construct complete packet evidence, rejecting an impossible empty
    /// encoded Reticulum packet.
    pub const fn try_new(
        interface_id: ReticulumInterfaceId,
        packet_len: u16,
        encoded_packet_sha256: EncodedPacketSha256,
        attempt_token: Option<RadioTraceAttemptToken>,
    ) -> Option<Self> {
        let Some(packet_len) = NonZeroU16::new(packet_len) else {
            return None;
        };
        Some(Self {
            interface_id,
            packet_len,
            encoded_packet_sha256,
            attempt_token,
        })
    }

    /// Product-owned Reticulum interface identifier.
    pub const fn interface_id(self) -> ReticulumInterfaceId {
        self.interface_id
    }

    /// Complete encoded interface-packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len.get()
    }

    /// SHA-256 over every complete encoded interface-packet byte.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }

    /// Hop-invariant Reticulum proof-correlation hash when derivable.
    pub const fn attempt_token(self) -> Option<RadioTraceAttemptToken> {
        self.attempt_token
    }
}

/// Detailed terminal result of one traced LoRa DATA dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTraceTxOutcome {
    /// Every planned physical frame completed successfully.
    Transmitted,
    /// Initial channel access rejected the logical packet.
    AccessRejected,
    /// The node owner denied the exact permit request.
    PermitDenied,
    /// A matching authorization arrived after its deadline.
    AuthorizationExpired,
    /// Fresh post-grant channel access rejected the logical packet.
    PostGrantAccessRejected,
    /// Airtime could not be calculated or admitted.
    AirtimeRejected,
    /// A dispatch deadline could not be represented.
    DeadlineConversionOverflow,
    /// The sole radio was already inactive.
    RadioInactive,
    /// Router and dispatcher configuration identities differed.
    InterfaceConfigurationMismatch,
    /// Immutable radio configuration changed before permit negotiation.
    RadioConfigurationChangedBeforePermit,
    /// Immutable radio configuration changed after permit negotiation.
    RadioConfigurationChangedAfterPermit,
    /// Channel-activity detection failed.
    CadFault,
    /// Physical transmission failed.
    TxFault,
    /// A permit exchange could not be reconciled.
    ControlPlaneRecovery,
    /// Authorized framing or byte exposure violated an invariant.
    FrameInvariantRecovery,
    /// A dropped CAD or transmit future was explicitly reconciled.
    CancelledRadioOperation,
}

impl RadioTraceTxOutcome {
    /// Stable numeric representation within this operation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Transmitted => 0,
            Self::AccessRejected => 1,
            Self::PermitDenied => 2,
            Self::AuthorizationExpired => 3,
            Self::PostGrantAccessRejected => 4,
            Self::AirtimeRejected => 5,
            Self::DeadlineConversionOverflow => 6,
            Self::RadioInactive => 7,
            Self::InterfaceConfigurationMismatch => 8,
            Self::RadioConfigurationChangedBeforePermit => 9,
            Self::RadioConfigurationChangedAfterPermit => 10,
            Self::CadFault => 11,
            Self::TxFault => 12,
            Self::ControlPlaneRecovery => 13,
            Self::FrameInvariantRecovery => 14,
            Self::CancelledRadioOperation => 15,
        }
    }
}

/// One terminal LoRa DATA dispatch trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceDataTx {
    packet: RadioTracePacketEvidence,
    outcome: RadioTraceTxOutcome,
    planned_frames: u8,
    completed_frames: u8,
    authorization_observed: bool,
    frame_completed_at_us: [Option<u64>; 2],
}

impl RadioTraceDataTx {
    /// Construct a consistent terminal DATA dispatch trace.
    pub const fn try_new(
        packet: RadioTracePacketEvidence,
        outcome: RadioTraceTxOutcome,
        planned_frames: u8,
        completed_frames: u8,
        authorization_observed: bool,
        frame_completed_at_us: [Option<u64>; 2],
    ) -> Result<Self, InvalidRadioTraceDataTx> {
        if planned_frames == 0 || planned_frames > 2 {
            return Err(InvalidRadioTraceDataTx::InvalidPlannedFrames);
        }
        if completed_frames > planned_frames {
            return Err(InvalidRadioTraceDataTx::CompletedFramesExceedPlanned);
        }
        let timestamp_count = match frame_completed_at_us {
            [None, Some(_)] => {
                return Err(InvalidRadioTraceDataTx::SparseCompletionTimestamps);
            }
            [Some(_), Some(_)] => 2,
            [Some(_), None] => 1,
            [None, None] => 0,
        };
        if timestamp_count != completed_frames {
            return Err(InvalidRadioTraceDataTx::CompletionTimestampCountMismatch);
        }
        Ok(Self {
            packet,
            outcome,
            planned_frames,
            completed_frames,
            authorization_observed,
            frame_completed_at_us,
        })
    }

    /// Complete prepared packet identity.
    pub const fn packet(self) -> RadioTracePacketEvidence {
        self.packet
    }

    /// Detailed terminal dispatch category.
    pub const fn outcome(self) -> RadioTraceTxOutcome {
        self.outcome
    }

    /// Physical frames planned for the logical packet.
    pub const fn planned_frames(self) -> u8 {
        self.planned_frames
    }

    /// Physical frames whose radio completion was observed.
    pub const fn completed_frames(self) -> u8 {
        self.completed_frames
    }

    /// Whether the exact byte-exposure authorization was observed.
    pub const fn authorization_observed(self) -> bool {
        self.authorization_observed
    }

    /// Per-frame radio-completion monotonic timestamps in physical order.
    pub const fn frame_completed_at_us(self) -> [Option<u64>; 2] {
        self.frame_completed_at_us
    }
}

/// A DATA dispatch trace violated a physical-frame invariant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRadioTraceDataTx {
    /// An RNode logical packet must plan one or two physical frames.
    InvalidPlannedFrames,
    /// Reported completed frames exceeded the planned frame count.
    CompletedFramesExceedPlanned,
    /// A populated completion timestamp followed an empty slot.
    SparseCompletionTimestamps,
    /// Completion timestamp count differed from the completed frame count.
    CompletionTimestampCountMismatch,
}

/// One complete logical LoRa packet accepted by the receive pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceLogicalRx {
    packet: RadioTracePacketEvidence,
    rssi_dbm: i16,
    snr_db: i16,
}

impl RadioTraceLogicalRx {
    /// Construct receiver-local evidence for one accepted logical packet.
    pub const fn new(packet: RadioTracePacketEvidence, rssi_dbm: i16, snr_db: i16) -> Self {
        Self {
            packet,
            rssi_dbm,
            snr_db,
        }
    }

    /// Complete received packet identity.
    pub const fn packet(self) -> RadioTracePacketEvidence {
        self.packet
    }

    /// Conservative whole-packet received signal strength in dBm.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Conservative whole-packet signal-to-noise ratio in dB.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }
}

/// One exact DATA route selected before radio dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceRouteSelected {
    submission_id: SubmissionId,
    destination: DestinationHash,
    next_hop_identity: Option<IdentityHash>,
    hops: u8,
    resolution: RouteDiagnosticResolution,
    packet: RadioTracePacketEvidence,
}

impl RadioTraceRouteSelected {
    /// Construct an exact route decision and prepared-packet identity.
    pub const fn try_new(
        submission_id: SubmissionId,
        destination: DestinationHash,
        next_hop_identity: Option<IdentityHash>,
        hops: u8,
        resolution: RouteDiagnosticResolution,
        packet: RadioTracePacketEvidence,
    ) -> Result<Self, InvalidRadioTraceRouteSelected> {
        if submission_id.0 == 0 {
            return Err(InvalidRadioTraceRouteSelected::ZeroSubmissionId);
        }
        if packet.attempt_token().is_none() {
            return Err(InvalidRadioTraceRouteSelected::MissingAttemptToken);
        }
        Ok(Self {
            submission_id,
            destination,
            next_hop_identity,
            hops,
            resolution,
            packet,
        })
    }

    /// Durable device submission correlated with this exact prepared packet.
    pub const fn submission_id(self) -> SubmissionId {
        self.submission_id
    }

    /// Complete routed destination hash.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Public identity hash selected as next hop, when known.
    pub const fn next_hop_identity(self) -> Option<IdentityHash> {
        self.next_hop_identity
    }

    /// Selected Reticulum hop count.
    pub const fn hops(self) -> u8 {
        self.hops
    }

    /// Exact or broadcast route-resolution result at selection time.
    pub const fn resolution(self) -> RouteDiagnosticResolution {
        self.resolution
    }

    /// Complete routed prepared-packet identity, including its attempt token.
    pub const fn packet(self) -> RadioTracePacketEvidence {
        self.packet
    }
}

/// A route-selection trace omitted required correlation evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRadioTraceRouteSelected {
    /// Device submission identifiers reserve zero.
    ZeroSubmissionId,
    /// A destination-DATA route must retain its proof-correlation token.
    MissingAttemptToken,
}

/// Terminal application-visible state of one proof-correlated DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTraceAttemptOutcome {
    /// A valid Reticulum delivery proof was accepted.
    Delivered,
    /// The receipt expired without a proof.
    DeliveryTimeout,
    /// The complete serialized route ended definitely unsent.
    Unsent,
}

impl RadioTraceAttemptOutcome {
    /// Stable numeric representation within this operation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Delivered => 0,
            Self::DeliveryTimeout => 1,
            Self::Unsent => 2,
        }
    }
}

/// One proof-correlated attempt reaching an application-visible terminal state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceAttemptTerminal {
    attempt_token: RadioTraceAttemptToken,
    outcome: RadioTraceAttemptOutcome,
    proof_ingress: Option<IngressObservation>,
}

impl RadioTraceAttemptTerminal {
    /// Construct one immutable attempt terminal trace.
    pub const fn new(
        attempt_token: RadioTraceAttemptToken,
        outcome: RadioTraceAttemptOutcome,
        proof_ingress: Option<IngressObservation>,
    ) -> Self {
        Self {
            attempt_token,
            outcome,
            proof_ingress,
        }
    }

    /// Complete hop-invariant Reticulum proof-correlation hash.
    pub const fn attempt_token(self) -> RadioTraceAttemptToken {
        self.attempt_token
    }

    /// Application-visible terminal result.
    pub const fn outcome(self) -> RadioTraceAttemptOutcome {
        self.outcome
    }

    /// First-arrival interface and optional signal for an accepted proof.
    pub const fn proof_ingress(self) -> Option<IngressObservation> {
        self.proof_ingress
    }
}

/// Receiver-side immediate DATA-to-proof lifecycle stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTraceInboundProofStage {
    /// A complete DATA packet was reconstructed by the receiving interface.
    DataLogicalRx,
    /// The ordinary transmit coordinator accepted the proof packet.
    OrdinaryQueued,
    /// The selected interface reported physical proof TxDone.
    PhysicalTxDone,
    /// The selected interface returned a terminal result without TxDone.
    PhysicalTxFailed,
}

impl RadioTraceInboundProofStage {
    /// Stable numeric representation within this operation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::DataLogicalRx => 0,
            Self::OrdinaryQueued => 4,
            Self::PhysicalTxDone => 5,
            Self::PhysicalTxFailed => 6,
        }
    }
}

/// Packet identity retained for a receiver-side proof stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceInboundProofPacket {
    packet_len: NonZeroU16,
    encoded_packet_sha256: EncodedPacketSha256,
}

impl RadioTraceInboundProofPacket {
    /// Construct non-empty complete packet evidence.
    pub const fn try_new(
        packet_len: u16,
        encoded_packet_sha256: EncodedPacketSha256,
    ) -> Option<Self> {
        let Some(packet_len) = NonZeroU16::new(packet_len) else {
            return None;
        };
        Some(Self {
            packet_len,
            encoded_packet_sha256,
        })
    }

    /// Complete encoded packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len.get()
    }

    /// SHA-256 over all encoded packet bytes.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }
}

/// One correlated receiver-side DATA-to-proof lifecycle observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceInboundProof {
    correlation_token: RadioTraceAttemptToken,
    stage: RadioTraceInboundProofStage,
    message_id: Option<[u8; 32]>,
    packet: Option<RadioTraceInboundProofPacket>,
    interface_id: Option<ReticulumInterfaceId>,
    signal: Option<IngressSignal>,
    dispatch_outcome: Option<RadioTraceTxOutcome>,
}

impl RadioTraceInboundProof {
    /// Validate and construct one immutable proof-lifecycle observation.
    #[allow(clippy::too_many_arguments)]
    pub const fn try_new(
        correlation_token: RadioTraceAttemptToken,
        stage: RadioTraceInboundProofStage,
        message_id: Option<[u8; 32]>,
        packet: Option<RadioTraceInboundProofPacket>,
        interface_id: Option<ReticulumInterfaceId>,
        signal: Option<IngressSignal>,
        dispatch_outcome: Option<RadioTraceTxOutcome>,
    ) -> Result<Self, InvalidRadioTraceInboundProof> {
        if signal.is_some() && interface_id.is_none() {
            return Err(InvalidRadioTraceInboundProof::SignalWithoutInterface);
        }
        match (stage, dispatch_outcome) {
            (
                RadioTraceInboundProofStage::PhysicalTxDone,
                Some(RadioTraceTxOutcome::Transmitted),
            ) => {}
            (RadioTraceInboundProofStage::PhysicalTxDone, _) => {
                return Err(InvalidRadioTraceInboundProof::InvalidPhysicalTxDoneOutcome);
            }
            (
                RadioTraceInboundProofStage::PhysicalTxFailed,
                None | Some(RadioTraceTxOutcome::Transmitted),
            ) => {
                return Err(InvalidRadioTraceInboundProof::InvalidPhysicalTxFailedOutcome);
            }
            (RadioTraceInboundProofStage::PhysicalTxFailed, Some(_)) => {}
            (_, Some(_)) => {
                return Err(InvalidRadioTraceInboundProof::UnexpectedDispatchOutcome);
            }
            (_, None) => {}
        }
        Ok(Self {
            correlation_token,
            stage,
            message_id,
            packet,
            interface_id,
            signal,
            dispatch_outcome,
        })
    }

    /// Complete hash of the covered inbound DATA packet.
    pub const fn correlation_token(self) -> RadioTraceAttemptToken {
        self.correlation_token
    }

    /// Immediate receiver lifecycle stage.
    pub const fn stage(self) -> RadioTraceInboundProofStage {
        self.stage
    }

    /// Validated LXMF message identifier, once known.
    pub const fn message_id(self) -> Option<[u8; 32]> {
        self.message_id
    }

    /// DATA or proof packet identity owned at this stage, when retained.
    pub const fn packet(self) -> Option<RadioTraceInboundProofPacket> {
        self.packet
    }

    /// Exact receive or proof-return interface, when known.
    pub const fn interface_id(self) -> Option<ReticulumInterfaceId> {
        self.interface_id
    }

    /// Receiver-local DATA signal, when retained.
    pub const fn signal(self) -> Option<IngressSignal> {
        self.signal
    }

    /// Exact terminal dispatcher result for physical proof-TX stages.
    pub const fn dispatch_outcome(self) -> Option<RadioTraceTxOutcome> {
        self.dispatch_outcome
    }
}

/// A receiver proof-lifecycle observation contradicted its stage or evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRadioTraceInboundProof {
    /// Signal values require a concrete receive interface.
    SignalWithoutInterface,
    /// TxDone requires the transmitted dispatcher outcome.
    InvalidPhysicalTxDoneOutcome,
    /// Physical failure requires a non-transmitted terminal outcome.
    InvalidPhysicalTxFailedOutcome,
    /// A non-physical stage cannot carry a dispatcher terminal outcome.
    UnexpectedDispatchOutcome,
}

/// Event-specific payload for one radio trace record.
///
/// Additional bounded event families can be introduced by later protocol
/// API revisions without changing event identity or page pagination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RadioTraceEventKind {
    /// One terminal destination-DATA dispatch.
    DataTx(RadioTraceDataTx),
    /// One complete logical packet accepted by LoRa receive.
    LogicalRx(RadioTraceLogicalRx),
    /// One exact route selected for a destination-DATA attempt.
    RouteSelected(RadioTraceRouteSelected),
    /// One proof-correlated DATA attempt reaching terminal state.
    AttemptTerminal(RadioTraceAttemptTerminal),
    /// One receiver-side durable DATA-to-proof lifecycle stage.
    InboundProof(RadioTraceInboundProof),
}

impl RadioTraceEventKind {
    /// Numeric event discriminator within this operation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::DataTx(_) => 0,
            Self::LogicalRx(_) => 1,
            Self::RouteSelected(_) => 2,
            Self::AttemptTerminal(_) => 3,
            Self::InboundProof(_) => 4,
        }
    }
}

/// One immutable boot-scoped radio trace record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceEvent {
    sequence: u64,
    observed_at_us: u64,
    kind: RadioTraceEventKind,
}

impl RadioTraceEvent {
    /// Construct one event with its boot-scoped identity and monotonic time.
    pub const fn new(sequence: u64, observed_at_us: u64, kind: RadioTraceEventKind) -> Self {
        Self {
            sequence,
            observed_at_us,
            kind,
        }
    }

    /// Monotonic event identity within the page's boot identifier.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Microseconds since this node incarnation started.
    pub const fn observed_at_us(self) -> u64 {
        self.observed_at_us
    }

    /// Event-specific trace evidence.
    pub const fn kind(self) -> RadioTraceEventKind {
        self.kind
    }
}

/// One bounded ascending page from the boot-scoped radio trace ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTracePage {
    boot_id: u64,
    applied_lora_profile: RadioTraceAppliedLoraProfile,
    oldest_sequence: u64,
    next_sequence: u64,
    history_lost: bool,
    entries: [Option<RadioTraceEvent>; MAX_RADIO_TRACE_PAGE_ENTRIES],
    next_cursor: Option<RadioTraceCursor>,
}

impl RadioTracePage {
    /// Construct one dense, strictly ascending page.
    ///
    /// `oldest_sequence == next_sequence` represents an empty ring. A present
    /// continuation cursor must equal the last returned sequence and is used
    /// as the following request's exclusive cursor.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        boot_id: u64,
        applied_lora_profile: RadioTraceAppliedLoraProfile,
        oldest_sequence: u64,
        next_sequence: u64,
        history_lost: bool,
        entries: [Option<RadioTraceEvent>; MAX_RADIO_TRACE_PAGE_ENTRIES],
        next_cursor: Option<RadioTraceCursor>,
    ) -> Result<Self, InvalidRadioTracePage> {
        if oldest_sequence > next_sequence {
            return Err(InvalidRadioTracePage::InvalidSequenceWindow);
        }
        let mut previous = None;
        let mut saw_empty = false;
        let mut maximum_event_bytes = 0_u16;
        for entry in entries {
            match entry {
                Some(entry) => {
                    if saw_empty {
                        return Err(InvalidRadioTracePage::SparseEntries);
                    }
                    if entry.sequence < oldest_sequence || entry.sequence >= next_sequence {
                        return Err(InvalidRadioTracePage::EventOutsideWindow);
                    }
                    if let Some(previous) = previous
                        && entry.sequence <= previous
                    {
                        return Err(InvalidRadioTracePage::NotStrictlyOrdered);
                    }
                    maximum_event_bytes += match entry.kind {
                        RadioTraceEventKind::DataTx(_) => 117,
                        RadioTraceEventKind::LogicalRx(_) => 100,
                        RadioTraceEventKind::RouteSelected(_) => 140,
                        RadioTraceEventKind::AttemptTerminal(_) => 68,
                        RadioTraceEventKind::InboundProof(_) => 137,
                    };
                    previous = Some(entry.sequence);
                }
                None => saw_empty = true,
            }
        }
        // These exact per-kind maxima include the event envelope and
        // worst-width scalar encodings. The remaining page/profile fields use
        // at most 72 bytes, plus 18 when a continuation cursor replaces null.
        let event_budget = if next_cursor.is_some() { 358 } else { 376 };
        if maximum_event_bytes > event_budget {
            return Err(InvalidRadioTracePage::EventCombinationExceedsWireBudget);
        }
        if let Some(next_cursor) = next_cursor
            && (next_cursor.boot_id != boot_id || previous != Some(next_cursor.after_sequence))
        {
            return Err(InvalidRadioTracePage::InvalidNextCursor);
        }
        Ok(Self {
            boot_id,
            applied_lora_profile,
            oldest_sequence,
            next_sequence,
            history_lost,
            entries,
            next_cursor,
        })
    }

    /// Opaque node-incarnation identifier scoping all event sequences.
    pub const fn boot_id(self) -> u64 {
        self.boot_id
    }

    /// Immutable LoRa configuration applied for this boot.
    pub const fn applied_lora_profile(self) -> RadioTraceAppliedLoraProfile {
        self.applied_lora_profile
    }

    /// Oldest event sequence still retained, or `next_sequence` when empty.
    pub const fn oldest_sequence(self) -> u64 {
        self.oldest_sequence
    }

    /// Sequence that will be allocated to the next event.
    pub const fn next_sequence(self) -> u64 {
        self.next_sequence
    }

    /// Whether events preceding this page's starting position were overwritten.
    pub const fn history_lost(self) -> bool {
        self.history_lost
    }

    /// Dense ascending fixed-capacity event slots.
    pub const fn entries(&self) -> &[Option<RadioTraceEvent>; MAX_RADIO_TRACE_PAGE_ENTRIES] {
        &self.entries
    }

    /// Exclusive sequence cursor for the following page, when more remain.
    pub const fn next_cursor(self) -> Option<RadioTraceCursor> {
        self.next_cursor
    }
}

/// A radio trace page violated its pagination invariants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRadioTracePage {
    /// The retained sequence window ran backwards.
    InvalidSequenceWindow,
    /// A populated event followed an empty fixed-capacity slot.
    SparseEntries,
    /// Event sequences were not strictly ascending.
    NotStrictlyOrdered,
    /// An event did not belong to the advertised retained window.
    EventOutsideWindow,
    /// Continuation cursor did not equal the last returned event sequence.
    InvalidNextCursor,
    /// The selected event combination exceeds the frozen response-body limit.
    EventCombinationExceedsWireBudget,
}

/// Optional exclusive destination cursor for a route diagnostics page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteDiagnosticsRequest {
    after: Option<DestinationHash>,
}

impl RouteDiagnosticsRequest {
    /// Construct a request beginning after `after`, or at the first route.
    pub const fn new(after: Option<DestinationHash>) -> Self {
        Self { after }
    }

    /// Exclusive lexicographic destination cursor.
    pub const fn after(self) -> Option<DestinationHash> {
        self.after
    }
}

/// Resolution recorded by one packet-correlated radio-trace route event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteDiagnosticResolution {
    /// An exact retained route was usable.
    ExactReady,
    /// An exact retained route selected an offline interface.
    ExactOffline,
    /// An exact retained route was incomplete.
    ExactMissing,
    /// Broadcast fallback was selected.
    BroadcastReady,
    /// No route or broadcast fallback was available.
    BroadcastUnavailable,
}

impl RouteDiagnosticResolution {
    /// Stable numeric wire representation for retained trace events.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::ExactReady => 0,
            Self::ExactOffline => 1,
            Self::ExactMissing => 2,
            Self::BroadcastReady => 3,
            Self::BroadcastUnavailable => 4,
        }
    }
}

/// Next hop selected by PRNS for one live route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteDiagnosticNextHop {
    /// Destination is reached directly on the receiving interface.
    Direct,
    /// Destination is reached through this transport identity.
    Via(IdentityHash),
}

/// One retained route or route-resolution diagnostics record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteDiagnosticEntry {
    destination: DestinationHash,
    next_hop: RouteDiagnosticNextHop,
    hops: u8,
    interface: ReticulumInterfaceId,
    learned_age_ms: u64,
    last_activity_age_ms: u64,
    expires_in_ms: u64,
}

impl RouteDiagnosticEntry {
    /// Construct one complete route diagnostics record.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        destination: DestinationHash,
        next_hop: RouteDiagnosticNextHop,
        hops: u8,
        interface: ReticulumInterfaceId,
        learned_age_ms: u64,
        last_activity_age_ms: u64,
        expires_in_ms: u64,
    ) -> Self {
        Self {
            destination,
            next_hop,
            hops,
            interface,
            learned_age_ms,
            last_activity_age_ms,
            expires_in_ms,
        }
    }

    /// Complete route destination hash.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Direct or transported PRNS next hop.
    pub const fn next_hop(self) -> RouteDiagnosticNextHop {
        self.next_hop
    }

    /// Reticulum hop count.
    pub const fn hops(self) -> u8 {
        self.hops
    }

    /// Receiving interface selected by the route.
    pub const fn interface(self) -> ReticulumInterfaceId {
        self.interface
    }

    /// Saturating age since the route was learned, when tracked.
    pub const fn learned_age_ms(self) -> u64 {
        self.learned_age_ms
    }

    /// Saturating age since the route was used, when tracked.
    pub const fn last_activity_age_ms(self) -> u64 {
        self.last_activity_age_ms
    }

    /// Remaining lifetime before expiry, when tracked.
    pub const fn expires_in_ms(self) -> u64 {
        self.expires_in_ms
    }
}

/// One bounded lexicographically ordered page of route diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteDiagnosticsPage {
    entries: [Option<RouteDiagnosticEntry>; MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES],
    next_cursor: Option<DestinationHash>,
}

impl RouteDiagnosticsPage {
    /// Construct one dense, strictly ordered route page.
    ///
    /// A present next cursor must equal the last returned destination so the
    /// following request remains an unambiguous exclusive continuation.
    pub fn new(
        entries: [Option<RouteDiagnosticEntry>; MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES],
        next_cursor: Option<DestinationHash>,
    ) -> Result<Self, InvalidRouteDiagnosticsPage> {
        let mut previous: Option<DestinationHash> = None;
        let mut saw_empty = false;
        for entry in entries {
            match entry {
                Some(entry) => {
                    if saw_empty {
                        return Err(InvalidRouteDiagnosticsPage::SparseEntries);
                    }
                    if let Some(previous) = previous
                        && entry.destination.0 <= previous.0
                    {
                        return Err(InvalidRouteDiagnosticsPage::NotStrictlyOrdered);
                    }
                    previous = Some(entry.destination);
                }
                None => saw_empty = true,
            }
        }
        if let Some(next_cursor) = next_cursor
            && previous != Some(next_cursor)
        {
            return Err(InvalidRouteDiagnosticsPage::InvalidNextCursor);
        }
        Ok(Self {
            entries,
            next_cursor,
        })
    }

    /// Dense ordered fixed-capacity route slots.
    pub const fn entries(
        &self,
    ) -> &[Option<RouteDiagnosticEntry>; MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES] {
        &self.entries
    }

    /// Exclusive destination cursor for a following page, when more remain.
    pub const fn next_cursor(self) -> Option<DestinationHash> {
        self.next_cursor
    }
}

/// A route diagnostics page violated a pagination invariant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRouteDiagnosticsPage {
    /// A populated entry followed an empty fixed-capacity slot.
    SparseEntries,
    /// Destinations were not strictly increasing in lexicographic byte order.
    NotStrictlyOrdered,
    /// The continuation cursor did not equal the last returned destination.
    InvalidNextCursor,
}

/// Public, copy-only summary of the node's Reticulum destinations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentitySummary {
    /// Primary destination served by the node.
    primary_destination: DestinationHash,
    /// Optional local `lxmf.delivery` destination served by the node.
    lxmf_delivery_destination: Option<DestinationHash>,
}

impl IdentitySummary {
    /// Construct the public summary for a node's primary destination.
    pub const fn new(primary_destination: DestinationHash) -> Self {
        Self {
            primary_destination,
            lxmf_delivery_destination: None,
        }
    }

    /// Construct a summary that also advertises the local `lxmf.delivery` destination.
    pub const fn with_lxmf_delivery_destination(
        primary_destination: DestinationHash,
        lxmf_delivery_destination: DestinationHash,
    ) -> Self {
        Self {
            primary_destination,
            lxmf_delivery_destination: Some(lxmf_delivery_destination),
        }
    }

    /// Primary destination served by the node.
    pub const fn primary_destination(self) -> DestinationHash {
        self.primary_destination
    }

    /// Local `lxmf.delivery` destination when that service is active.
    pub const fn lxmf_delivery_destination(self) -> Option<DestinationHash> {
        self.lxmf_delivery_destination
    }
}

/// Device-assigned identifier for a submitted operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubmissionId(pub u64);

/// Permissions derived from device-owned authority, never from CBOR input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Permissions(u32);

impl Permissions {
    /// No authenticated operation permissions.
    pub const NONE: Self = Self(0);
    /// Read submission state belonging to the authenticated principal.
    pub const READ_SUBMISSION_STATUS: Self = Self(1 << 0);
    /// Submit outbound RNS DATA through the node's transport-neutral router.
    ///
    /// The bit remains part of the stable persisted permission vocabulary even
    /// when this build omits the operation itself.
    pub const SUBMIT_RNS_DATA: Self = Self(1 << 1);
    /// Mutate saved Wi-Fi and Reticulum TCP network configuration.
    ///
    /// This bit remains part of the stable persisted permission vocabulary
    /// even when a build omits the network operations.
    pub const MANAGE_NETWORK_CONFIG: Self = Self(1 << 2);

    const KNOWN_BITS: u32 =
        Self::READ_SUBMISSION_STATUS.0 | Self::SUBMIT_RNS_DATA.0 | Self::MANAGE_NETWORK_CONFIG.0;

    /// Whether all bits in `required` are present.
    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    /// Raw representation for session-policy adapters and diagnostics.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Decode the stable persisted permission vocabulary without feature drift.
    pub const fn from_bits(bits: u32) -> Result<Self, UnknownPermissionBits> {
        let unknown = bits & !Self::KNOWN_BITS;
        if unknown == 0 {
            Ok(Self(bits))
        } else {
            Err(UnknownPermissionBits { unknown })
        }
    }
}

/// Persisted permission bits unknown to this device-API schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownPermissionBits {
    unknown: u32,
}

impl UnknownPermissionBits {
    /// Bits outside the stable permission vocabulary.
    pub const fn unknown(self) -> u32 {
        self.unknown
    }
}

impl BitOr for Permissions {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Device-owned credential facts that authorized one dispatch attempt.
///
/// This value is supplied out of band with the trusted dispatch context and is
/// never decoded from the device-API wire message. Its public constructor lets
/// trusted integration code move these scalar facts between portable crates;
/// it is not an unforgeable authorization capability against linked Rust code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchProvenance {
    credential_id: [u8; 16],
    credential_generation: u64,
    authority_revision: u64,
    policy_version: u32,
}

/// Invalid device-owned facts supplied for dispatch provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchProvenanceError {
    /// The all-zero credential identifier is reserved for erased state.
    ZeroCredentialId,
    /// Credential generation zero is reserved for erased state.
    ZeroCredentialGeneration,
    /// Authority revision zero is reserved for erased state.
    ZeroAuthorityRevision,
    /// Authorization-policy version zero is reserved for erased state.
    ZeroPolicyVersion,
    /// A credential generation cannot originate after the observed authority.
    GenerationExceedsAuthorityRevision {
        /// Candidate credential generation.
        credential_generation: u64,
        /// Candidate complete-authority revision.
        authority_revision: u64,
    },
}

impl DispatchProvenance {
    /// Validate and construct provenance from credential-authority state.
    pub const fn new(
        credential_id: [u8; 16],
        credential_generation: u64,
        authority_revision: u64,
        policy_version: u32,
    ) -> Result<Self, DispatchProvenanceError> {
        let mut byte = 0;
        let mut has_nonzero_id_byte = false;
        while byte < credential_id.len() {
            if credential_id[byte] != 0 {
                has_nonzero_id_byte = true;
                break;
            }
            byte += 1;
        }
        if !has_nonzero_id_byte {
            return Err(DispatchProvenanceError::ZeroCredentialId);
        }
        if credential_generation == 0 {
            return Err(DispatchProvenanceError::ZeroCredentialGeneration);
        }
        if authority_revision == 0 {
            return Err(DispatchProvenanceError::ZeroAuthorityRevision);
        }
        if policy_version == 0 {
            return Err(DispatchProvenanceError::ZeroPolicyVersion);
        }
        if credential_generation > authority_revision {
            return Err(
                DispatchProvenanceError::GenerationExceedsAuthorityRevision {
                    credential_generation,
                    authority_revision,
                },
            );
        }
        Ok(Self {
            credential_id,
            credential_generation,
            authority_revision,
            policy_version,
        })
    }

    /// Opaque identifier of the credential revalidated for this attempt.
    pub const fn credential_id(self) -> [u8; 16] {
        self.credential_id
    }

    /// Exact credential generation revalidated for this attempt.
    pub const fn credential_generation(self) -> u64 {
        self.credential_generation
    }

    /// Complete credential-authority revision observed at revalidation.
    pub const fn authority_revision(self) -> u64 {
        self.authority_revision
    }

    /// Authorization-policy version applied by the credential record.
    pub const fn policy_version(self) -> u32 {
        self.policy_version
    }
}

/// Trusted authentication and authorization facts supplied out of band.
#[derive(Debug, Eq, PartialEq)]
pub struct DispatchContext {
    /// Principal derived from device-owned authenticated credential state, if any.
    principal: Option<PrincipalId>,
    /// Permissions granted to that authenticated principal.
    permissions: Permissions,
    /// Credential-authority facts captured by exact pre-dispatch revalidation.
    provenance: Option<DispatchProvenance>,
}

impl DispatchContext {
    /// Context for a connection without an authenticated application session.
    pub const UNAUTHENTICATED: Self = Self {
        principal: None,
        permissions: Permissions::NONE,
        provenance: None,
    };

    /// Construct a trusted context for an authenticated principal.
    pub const fn authenticated(
        principal: PrincipalId,
        permissions: Permissions,
        provenance: DispatchProvenance,
    ) -> Self {
        Self {
            principal: Some(principal),
            permissions,
            provenance: Some(provenance),
        }
    }

    /// Device-owned authenticated principal, if this context has one.
    pub const fn principal(&self) -> Option<PrincipalId> {
        self.principal
    }

    /// Device-owned permissions bound to this dispatch attempt.
    pub const fn permissions(&self) -> Permissions {
        self.permissions
    }

    /// Credential-authority facts for this authenticated attempt, if any.
    pub const fn provenance(&self) -> Option<DispatchProvenance> {
        self.provenance
    }
}

/// Permission category required by an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredPermission {
    /// Read submission status.
    ReadSubmissionStatus,
    /// Submit outbound RNS DATA through the unstable transport-neutral path.
    SubmitRnsData,
    /// Mutate saved Wi-Fi and Reticulum TCP network configuration.
    ManageNetworkConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorizationRequirement {
    Public,
    Authenticated,
    Permission(RequiredPermission),
}

impl RequiredPermission {
    const fn bits(self) -> Permissions {
        match self {
            Self::ReadSubmissionStatus => Permissions::READ_SUBMISSION_STATUS,
            Self::SubmitRnsData => Permissions::SUBMIT_RNS_DATA,
            Self::ManageNetworkConfig => Permissions::MANAGE_NETWORK_CONFIG,
        }
    }
}

/// Authorization failure established without consulting untrusted message data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationError {
    /// The operation requires an authenticated principal.
    AuthenticationRequired,
    /// The authenticated principal lacks the operation permission.
    PermissionDenied(RequiredPermission),
}

/// Logical request body. It contains no transport, session, or engine owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DeviceRequest<'a> {
    /// Read API version, safety capabilities, and hard codec limits.
    SystemCapabilities,
    /// Read the node's public primary Reticulum destination.
    IdentitySummary,
    /// Read the product-owned appliance label.
    ApplianceLabelGet,
    /// Compare-and-swap the product-owned appliance label.
    ApplianceLabelMutate(ApplianceLabelMutationRequest<'a>),
    /// Read status for a previously accepted submission.
    SubmissionStatus {
        /// Device-assigned submission identifier.
        id: SubmissionId,
    },
    /// Read the next committed LXMF summary after an optional stable handle.
    #[cfg(feature = "lxmf")]
    LxmfNext {
        /// Exclusive physical-commit-order cursor; `None` selects the first message.
        after: Option<LxmfMessageHandle>,
    },
    /// Read one bounded chunk of an exact committed normalized LXMF wire message.
    #[cfg(feature = "lxmf")]
    LxmfRead {
        /// Stable committed-message handle.
        handle: LxmfMessageHandle,
        /// Zero-based byte offset in the normalized wire representation.
        offset: u32,
        /// Maximum bytes requested in this response.
        max_bytes: LxmfReadLength,
    },
    /// Read the durable collection watermark for the LXMF mailbox.
    #[cfg(feature = "lxmf")]
    LxmfMailboxStatus,
    /// Advance the durable collection watermark through one committed message.
    ///
    /// Repeating an already-applied watermark is an idempotent success.
    #[cfg(feature = "lxmf")]
    LxmfMailboxAcknowledge {
        /// Highest committed message durably imported by the authenticated client.
        through: LxmfMessageHandle,
    },
    /// Compose and durably submit a basic LXMF message using the device-owned source.
    ///
    /// Empty title and content values are valid. The codec bounds fields and
    /// message size; product composition applies its additional carrier rules.
    #[cfg(feature = "lxmf")]
    LxmfBasicSend {
        /// Complete remote `lxmf.delivery` destination hash.
        destination: DestinationHash,
        /// Caller-selected Unix timestamp in milliseconds.
        ///
        /// The bearer-neutral codec accepts `u64`; the current product composer
        /// accepts exactly `1..=8_796_093_022_207_999`.
        timestamp_unix_ms: u64,
        /// Borrowed binary title; interpretation belongs to the LXMF application.
        ///
        /// The codec's 295-byte field bound is not a guarantee that every
        /// title/content combination fits the encoded body or product carrier.
        title: &'a [u8],
        /// Borrowed binary content; interpretation belongs to the LXMF application.
        ///
        /// The codec's 295-byte field bound is not a guarantee that every
        /// title/content combination fits the encoded body or product carrier.
        content: &'a [u8],
        /// Optional phone location frozen into the signed LXMF payload.
        location: Option<LxmfMessageLocation>,
        /// Deduplication key scoped by the authenticated principal and composed message.
        idempotency_key: IdempotencyKey,
    },
    /// Read one nearby `lxmf.delivery` peer from the volatile bounded projection.
    #[cfg(feature = "lxmf")]
    LxmfPeerNext {
        /// Optional exclusive cursor scoped to one device boot/incarnation.
        ///
        /// `None` starts from the oldest retained record. The wire requires
        /// both cursor fields together, preventing ambiguous partial cursors.
        after: Option<LxmfPeerDiscoveryCursor>,
    },
    /// Begin one authenticated bounded NomadNet page fetch.
    #[cfg(feature = "nomad")]
    NomadFetchStart(NomadFetchStartRequest<'a>),
    /// Poll one authenticated principal-owned NomadNet page fetch.
    #[cfg(feature = "nomad")]
    NomadFetchPoll(NomadFetchPollRequest),
    /// Begin one authenticated boot-scoped Reticulum path-and-proof probe.
    ReticulumProbeStart(ProbeStartRequest),
    /// Poll one authenticated principal-owned Reticulum path-and-proof probe.
    ReticulumProbePoll(ProbePollRequest),
    /// Read the complete desired configuration with Wi-Fi secrets redacted.
    #[cfg(feature = "network-config")]
    NetworkConfigGet,
    /// Mutate one saved Wi-Fi profile or the single Reticulum TCP peer.
    #[cfg(feature = "network-config")]
    NetworkConfigMutate(NetworkConfigMutationRequest<'a>),
    /// Read live Wi-Fi station and Reticulum TCP peer state.
    #[cfg(feature = "network-config")]
    NetworkStatus,
    /// Read one bounded cross-interface node diagnostics snapshot.
    NodeDiagnostics,
    /// Read one bounded lexicographically ordered route diagnostics page.
    RouteDiagnosticsPage(RouteDiagnosticsRequest),
    /// Read one bounded boot-scoped packet-correlated radio trace page.
    RadioTracePage(RadioTracePageRequest),
    /// Queue the node's ordinary primary, LXMF, and NomadNet service announces.
    ManualServiceAnnounce,
    /// Durably submit outbound RNS DATA without selecting a physical transport.
    #[cfg(feature = "rns-data")]
    SubmitRnsData {
        /// Complete Reticulum destination hash.
        destination: DestinationHash,
        /// Borrowed application data; never allocated or copied by decoding.
        payload: &'a [u8],
        /// Deduplication key scoped by the authenticated principal and content.
        idempotency_key: IdempotencyKey,
    },
    /// Uninhabited marker keeping the decode lifetime stable without the
    /// RNS DATA operation.
    #[doc(hidden)]
    __Borrowed(Infallible, PhantomData<&'a [u8]>),
}

impl DeviceRequest<'_> {
    /// Operation number encoded on the wire.
    pub const fn operation(&self) -> u16 {
        match self {
            Self::SystemCapabilities => OP_SYSTEM_CAPABILITIES,
            Self::IdentitySummary => OP_IDENTITY_SUMMARY,
            Self::ApplianceLabelGet => OP_APPLIANCE_LABEL_GET,
            Self::ApplianceLabelMutate(_) => OP_APPLIANCE_LABEL_MUTATE,
            Self::SubmissionStatus { .. } => OP_SUBMISSION_STATUS,
            #[cfg(feature = "lxmf")]
            Self::LxmfNext { .. } => OP_LXMF_NEXT,
            #[cfg(feature = "lxmf")]
            Self::LxmfRead { .. } => OP_LXMF_READ,
            #[cfg(feature = "lxmf")]
            Self::LxmfMailboxStatus => OP_LXMF_MAILBOX_STATUS,
            #[cfg(feature = "lxmf")]
            Self::LxmfMailboxAcknowledge { .. } => OP_LXMF_MAILBOX_ACKNOWLEDGE,
            #[cfg(feature = "lxmf")]
            Self::LxmfBasicSend { .. } => OP_LXMF_BASIC_SEND,
            #[cfg(feature = "lxmf")]
            Self::LxmfPeerNext { .. } => OP_LXMF_PEER_NEXT,
            #[cfg(feature = "nomad")]
            Self::NomadFetchStart(_) => OP_NOMAD_FETCH_START,
            #[cfg(feature = "nomad")]
            Self::NomadFetchPoll(_) => OP_NOMAD_FETCH_POLL,
            Self::ReticulumProbeStart(_) => OP_RETICULUM_PROBE_START,
            Self::ReticulumProbePoll(_) => OP_RETICULUM_PROBE_POLL,
            #[cfg(feature = "network-config")]
            Self::NetworkConfigGet => OP_NETWORK_CONFIG_GET,
            #[cfg(feature = "network-config")]
            Self::NetworkConfigMutate(_) => OP_NETWORK_CONFIG_MUTATE,
            #[cfg(feature = "network-config")]
            Self::NetworkStatus => OP_NETWORK_STATUS,
            Self::NodeDiagnostics => OP_NODE_DIAGNOSTICS,
            Self::RouteDiagnosticsPage(_) => OP_ROUTE_DIAGNOSTICS_PAGE,
            Self::RadioTracePage(_) => OP_RADIO_TRACE_PAGE,
            Self::ManualServiceAnnounce => OP_MANUAL_SERVICE_ANNOUNCE,
            #[cfg(feature = "rns-data")]
            Self::SubmitRnsData { .. } => OP_SUBMIT_RNS_DATA,
            Self::__Borrowed(never, _) => match *never {},
        }
    }

    /// Whether this operation can change node state.
    pub const fn is_mutating(&self) -> bool {
        match self {
            Self::SystemCapabilities
            | Self::IdentitySummary
            | Self::ApplianceLabelGet
            | Self::SubmissionStatus { .. } => false,
            Self::ApplianceLabelMutate(_) => true,
            #[cfg(feature = "lxmf")]
            Self::LxmfNext { .. }
            | Self::LxmfRead { .. }
            | Self::LxmfMailboxStatus
            | Self::LxmfPeerNext { .. } => false,
            #[cfg(feature = "lxmf")]
            Self::LxmfMailboxAcknowledge { .. } => true,
            #[cfg(feature = "lxmf")]
            Self::LxmfBasicSend { .. } => true,
            #[cfg(feature = "nomad")]
            Self::NomadFetchStart(_) => true,
            #[cfg(feature = "nomad")]
            Self::NomadFetchPoll(_) => false,
            Self::ReticulumProbeStart(_) => true,
            Self::ReticulumProbePoll(_) => false,
            #[cfg(feature = "network-config")]
            Self::NetworkConfigGet | Self::NetworkStatus => false,
            #[cfg(feature = "network-config")]
            Self::NetworkConfigMutate(_) => true,
            Self::NodeDiagnostics | Self::RouteDiagnosticsPage(_) | Self::RadioTracePage(_) => {
                false
            }
            Self::ManualServiceAnnounce => true,
            #[cfg(feature = "rns-data")]
            Self::SubmitRnsData { .. } => true,
            Self::__Borrowed(never, _) => match *never {},
        }
    }

    const fn authorization_requirement(&self) -> AuthorizationRequirement {
        match self {
            Self::SystemCapabilities | Self::IdentitySummary => AuthorizationRequirement::Public,
            Self::ApplianceLabelGet | Self::ApplianceLabelMutate(_) => {
                AuthorizationRequirement::Authenticated
            }
            Self::SubmissionStatus { .. } => {
                AuthorizationRequirement::Permission(RequiredPermission::ReadSubmissionStatus)
            }
            #[cfg(feature = "lxmf")]
            Self::LxmfNext { .. }
            | Self::LxmfRead { .. }
            | Self::LxmfMailboxStatus
            | Self::LxmfMailboxAcknowledge { .. }
            | Self::LxmfPeerNext { .. } => AuthorizationRequirement::Authenticated,
            #[cfg(feature = "nomad")]
            Self::NomadFetchStart(_) | Self::NomadFetchPoll(_) => {
                AuthorizationRequirement::Authenticated
            }
            Self::ReticulumProbeStart(_) => {
                AuthorizationRequirement::Permission(RequiredPermission::SubmitRnsData)
            }
            Self::ReticulumProbePoll(_) => AuthorizationRequirement::Authenticated,
            #[cfg(feature = "network-config")]
            Self::NetworkConfigGet | Self::NetworkStatus => AuthorizationRequirement::Authenticated,
            #[cfg(feature = "network-config")]
            Self::NetworkConfigMutate(_) => {
                AuthorizationRequirement::Permission(RequiredPermission::ManageNetworkConfig)
            }
            Self::NodeDiagnostics | Self::RouteDiagnosticsPage(_) | Self::RadioTracePage(_) => {
                AuthorizationRequirement::Authenticated
            }
            Self::ManualServiceAnnounce => AuthorizationRequirement::Authenticated,
            #[cfg(feature = "lxmf")]
            Self::LxmfBasicSend { .. } => {
                AuthorizationRequirement::Permission(RequiredPermission::SubmitRnsData)
            }
            #[cfg(feature = "rns-data")]
            Self::SubmitRnsData { .. } => {
                AuthorizationRequirement::Permission(RequiredPermission::SubmitRnsData)
            }
            Self::__Borrowed(never, _) => match *never {},
        }
    }
}

/// Logical request envelope decoded from exactly one CBOR item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestEnvelope<'a> {
    /// Protocol version selected by the client.
    pub version: ApiVersion,
    /// Client request identifier.
    pub request_id: RequestId,
    /// Operation-specific request.
    pub request: DeviceRequest<'a>,
}

/// Apply common authentication and permission policy to a decoded request.
///
/// The principal is intentionally absent from [`RequestEnvelope`]. Callers
/// must obtain `context` from their separately authenticated session.
pub const fn authorize_request(
    context: &DispatchContext,
    request: &DeviceRequest<'_>,
) -> Result<(), AuthorizationError> {
    match request.authorization_requirement() {
        AuthorizationRequirement::Public => Ok(()),
        AuthorizationRequirement::Authenticated => {
            if context.principal.is_some() {
                Ok(())
            } else {
                Err(AuthorizationError::AuthenticationRequired)
            }
        }
        AuthorizationRequirement::Permission(required) => {
            if context.principal.is_none() {
                return Err(AuthorizationError::AuthenticationRequired);
            }
            if !context.permissions.contains(required.bits()) {
                return Err(AuthorizationError::PermissionDenied(required));
            }
            Ok(())
        }
    }
}

/// Runtime availability of a logical capability.
///
/// This is a closed wire vocabulary. Adding a numeric value requires a
/// new API major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CapabilityAvailability {
    /// This build cannot perform the capability.
    Unavailable = 0,
    /// Code exists, but profile or runtime policy has disabled it.
    Disabled = 1,
    /// The capability is present and enabled.
    Available = 2,
}

impl CapabilityAvailability {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Device-owned capability and codec-limit handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilitySnapshot {
    /// Highest API version spoken by this device.
    pub(crate) api_version: ApiVersion,
    /// Whether any public operation can return raw prepared packet bytes.
    pub(crate) packet_output: bool,
    /// Availability of raw/direct physical-radio transmission to local clients.
    pub(crate) direct_radio_tx: CapabilityAvailability,
    /// Whether this snapshot advertises transport-neutral outbound RNS DATA submission.
    pub(crate) submit_rns_data: bool,
    /// Hard maximum logical CBOR message size.
    pub(crate) max_message_bytes: u16,
    /// Hard maximum encoded operation-body size.
    pub(crate) max_body_bytes: u16,
    /// Maximum RNS DATA submission payload.
    pub(crate) max_submit_rns_data_payload_bytes: u16,
    /// Runtime availability of committed LXMF discovery and bounded reads.
    pub(crate) lxmf: CapabilityAvailability,
    /// Maximum exact normalized wire bytes returned by one LXMF read.
    pub(crate) max_lxmf_read_chunk_bytes: u16,
    /// Runtime availability of source-free basic LXMF composition and submission.
    pub(crate) lxmf_basic_send: CapabilityAvailability,
    /// Structural per-field title limit advertised by the logical codec.
    ///
    /// Product composition and carrier limits can reduce the accepted
    /// title/content combination.
    pub(crate) max_lxmf_basic_title_bytes: u16,
    /// Structural per-field content limit advertised by the logical codec.
    ///
    /// Product composition and carrier limits can reduce the accepted
    /// title/content combination.
    pub(crate) max_lxmf_basic_content_bytes: u16,
    /// Runtime availability of bounded nearby `lxmf.delivery` peer discovery.
    pub(crate) lxmf_peer_discovery: CapabilityAvailability,
    /// Maximum authenticated announce application data returned with one peer.
    pub(crate) max_lxmf_peer_app_data_bytes: u16,
    /// Runtime availability of bounded authenticated NomadNet page fetch.
    pub(crate) nomad: CapabilityAvailability,
    /// Maximum UTF-8 request-path bytes accepted by NomadNet fetch.
    pub(crate) max_nomad_page_path_bytes: u16,
    /// Maximum valid UTF-8 Micron page bytes returned by NomadNet fetch.
    pub(crate) max_nomad_page_bytes: u16,
    /// Runtime availability of redacted network configuration and status.
    pub(crate) network_config: CapabilityAvailability,
    /// Runtime availability of authenticated ordinary service announces.
    pub(crate) manual_service_announce: CapabilityAvailability,
    /// Runtime availability of authenticated Reticulum path-and-proof probes.
    pub(crate) reticulum_probe: CapabilityAvailability,
    /// Runtime availability of live PRNS node, interface, and route inspection.
    pub(crate) route_diagnostics: CapabilityAvailability,
}

impl CapabilitySnapshot {
    /// Snapshot for this crate's current build.
    ///
    /// Packet output and direct-radio TX remain deliberately unavailable in
    /// every feature composition. Outbound RNS submission is a separate,
    /// transport-neutral capability.
    pub const fn current() -> Self {
        Self {
            api_version: ApiVersion::CURRENT,
            packet_output: false,
            direct_radio_tx: CapabilityAvailability::Unavailable,
            submit_rns_data: cfg!(feature = "rns-data"),
            max_message_bytes: MAX_MESSAGE_BYTES as u16,
            max_body_bytes: MAX_BODY_BYTES as u16,
            max_submit_rns_data_payload_bytes: MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES as u16,
            lxmf: if cfg!(feature = "lxmf") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_lxmf_read_chunk_bytes: if cfg!(feature = "lxmf") { 416 } else { 0 },
            lxmf_basic_send: if cfg!(feature = "lxmf") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_lxmf_basic_title_bytes: if cfg!(feature = "lxmf") {
                MAX_LXMF_BASIC_TITLE_BYTES as u16
            } else {
                0
            },
            max_lxmf_basic_content_bytes: if cfg!(feature = "lxmf") {
                MAX_LXMF_BASIC_CONTENT_BYTES as u16
            } else {
                0
            },
            lxmf_peer_discovery: if cfg!(feature = "lxmf") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_lxmf_peer_app_data_bytes: if cfg!(feature = "lxmf") {
                MAX_LXMF_PEER_APP_DATA_BYTES as u16
            } else {
                0
            },
            nomad: if cfg!(feature = "nomad") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_nomad_page_path_bytes: if cfg!(feature = "nomad") {
                MAX_NOMAD_PAGE_PATH_BYTES as u16
            } else {
                0
            },
            max_nomad_page_bytes: if cfg!(feature = "nomad") {
                MAX_NOMAD_PAGE_BYTES as u16
            } else {
                0
            },
            network_config: if cfg!(feature = "network-config") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            manual_service_announce: CapabilityAvailability::Available,
            reticulum_probe: CapabilityAvailability::Available,
            route_diagnostics: CapabilityAvailability::Available,
        }
    }

    /// Snapshot restricted to operations implemented by a higher dispatch layer.
    ///
    /// `submit_rns_data` can disable the codec-build capability,
    /// but cannot enable an operation omitted from this crate's build. This
    /// keeps Cargo feature unification in another dependency edge from making a
    /// dispatcher advertise an operation that it did not compile locally.
    pub const fn for_dispatch(submit_rns_data: bool) -> Self {
        let mut snapshot = Self::current();
        snapshot.submit_rns_data &= submit_rns_data;
        snapshot.lxmf = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_read_chunk_bytes = 0;
        snapshot.lxmf_basic_send = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_basic_title_bytes = 0;
        snapshot.max_lxmf_basic_content_bytes = 0;
        snapshot.lxmf_peer_discovery = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_peer_app_data_bytes = 0;
        snapshot.nomad = CapabilityAvailability::Unavailable;
        snapshot.max_nomad_page_path_bytes = 0;
        snapshot.max_nomad_page_bytes = 0;
        snapshot.network_config = CapabilityAvailability::Unavailable;
        snapshot.manual_service_announce = CapabilityAvailability::Unavailable;
        snapshot.reticulum_probe = CapabilityAvailability::Unavailable;
        snapshot.route_diagnostics = CapabilityAvailability::Unavailable;
        snapshot
    }

    /// Restrict submission and NomadNet fetch to two independent dispatcher ports.
    pub const fn for_dispatch_with_nomad(
        submit_rns_data: bool,
        nomad: CapabilityAvailability,
    ) -> Self {
        Self::for_dispatch(submit_rns_data).with_dispatch_nomad(nomad)
    }

    /// Add the independently owned NomadNet port to an existing dispatcher snapshot.
    ///
    /// This preserves every capability already selected by the higher
    /// dispatcher. It cannot enable NomadNet fetch when that codec feature was
    /// omitted from this crate's build.
    pub const fn with_dispatch_nomad(mut self, nomad: CapabilityAvailability) -> Self {
        if cfg!(feature = "nomad") {
            self.nomad = nomad;
            let available = !matches!(nomad, CapabilityAvailability::Unavailable);
            self.max_nomad_page_path_bytes = if available {
                MAX_NOMAD_PAGE_PATH_BYTES as u16
            } else {
                0
            };
            self.max_nomad_page_bytes = if available {
                MAX_NOMAD_PAGE_BYTES as u16
            } else {
                0
            };
        }
        self
    }

    /// Add the independently owned network-configuration port to a dispatcher snapshot.
    ///
    /// This cannot enable the port when the codec feature was omitted from
    /// this crate's build.
    pub const fn with_dispatch_network_config(
        mut self,
        network_config: CapabilityAvailability,
    ) -> Self {
        if cfg!(feature = "network-config") {
            self.network_config = network_config;
        }
        self
    }

    /// Add authenticated ordinary service announces to a dispatcher snapshot.
    pub const fn with_dispatch_manual_service_announce(
        mut self,
        manual_service_announce: CapabilityAvailability,
    ) -> Self {
        self.manual_service_announce = manual_service_announce;
        self
    }

    /// Add the independently owned Reticulum probe port to a dispatcher snapshot.
    pub const fn with_dispatch_reticulum_probe(
        mut self,
        reticulum_probe: CapabilityAvailability,
    ) -> Self {
        self.reticulum_probe = reticulum_probe;
        self
    }

    /// Add live PRNS node, interface, and route inspection to a dispatcher snapshot.
    pub const fn with_dispatch_route_diagnostics(
        mut self,
        route_diagnostics: CapabilityAvailability,
    ) -> Self {
        self.route_diagnostics = route_diagnostics;
        self
    }

    /// Restrict submission and LXMF capabilities to a higher dispatcher.
    pub const fn for_dispatch_with_lxmf(
        submit_rns_data: bool,
        lxmf: CapabilityAvailability,
    ) -> Self {
        let mut snapshot = Self::for_dispatch(submit_rns_data);
        if cfg!(feature = "lxmf") {
            snapshot.lxmf = lxmf;
            snapshot.max_lxmf_read_chunk_bytes =
                if matches!(lxmf, CapabilityAvailability::Unavailable) {
                    0
                } else {
                    416
                };
        }
        snapshot
    }

    /// Restrict submission, LXMF reads, and basic LXMF send to one dispatcher.
    pub const fn for_dispatch_with_lxmf_and_basic_send(
        submit_rns_data: bool,
        lxmf: CapabilityAvailability,
        lxmf_basic_send: CapabilityAvailability,
    ) -> Self {
        let mut snapshot = Self::for_dispatch_with_lxmf(submit_rns_data, lxmf);
        if cfg!(feature = "lxmf") {
            snapshot.lxmf_basic_send = lxmf_basic_send;
            let available = !matches!(lxmf_basic_send, CapabilityAvailability::Unavailable);
            snapshot.max_lxmf_basic_title_bytes = if available {
                MAX_LXMF_BASIC_TITLE_BYTES as u16
            } else {
                0
            };
            snapshot.max_lxmf_basic_content_bytes = if available {
                MAX_LXMF_BASIC_CONTENT_BYTES as u16
            } else {
                0
            };
        }
        snapshot
    }

    /// Restrict submission, LXMF, send, and peer discovery to one dispatcher.
    #[allow(clippy::too_many_arguments)]
    pub const fn for_dispatch_with_lxmf_basic_send_and_peer_discovery(
        submit_rns_data: bool,
        lxmf: CapabilityAvailability,
        lxmf_basic_send: CapabilityAvailability,
        lxmf_peer_discovery: CapabilityAvailability,
        max_lxmf_peer_app_data_bytes: u16,
    ) -> Self {
        let mut snapshot =
            Self::for_dispatch_with_lxmf_and_basic_send(submit_rns_data, lxmf, lxmf_basic_send);
        if cfg!(feature = "lxmf") {
            snapshot.lxmf_peer_discovery = lxmf_peer_discovery;
            snapshot.max_lxmf_peer_app_data_bytes =
                if matches!(lxmf_peer_discovery, CapabilityAvailability::Unavailable) {
                    0
                } else if max_lxmf_peer_app_data_bytes > MAX_LXMF_PEER_APP_DATA_BYTES as u16 {
                    MAX_LXMF_PEER_APP_DATA_BYTES as u16
                } else {
                    max_lxmf_peer_app_data_bytes
                };
        }
        snapshot
    }

    /// Highest API version spoken by this device.
    pub const fn api_version(self) -> ApiVersion {
        self.api_version
    }

    /// Whether any public operation can return raw prepared packet bytes.
    pub const fn packet_output(self) -> bool {
        self.packet_output
    }

    /// Availability of raw/direct physical-radio transmission to local clients.
    ///
    /// This does not describe transport-neutral RNS submission, which may
    /// route over LoRa or another enabled Reticulum interface.
    pub const fn direct_radio_tx(self) -> CapabilityAvailability {
        self.direct_radio_tx
    }

    /// Whether this snapshot advertises transport-neutral RNS DATA submission.
    pub const fn submit_rns_data(self) -> bool {
        self.submit_rns_data
    }

    /// Hard maximum logical CBOR message size.
    pub const fn max_message_bytes(self) -> u16 {
        self.max_message_bytes
    }

    /// Hard maximum encoded operation-body size.
    pub const fn max_body_bytes(self) -> u16 {
        self.max_body_bytes
    }

    /// Maximum RNS DATA submission payload.
    pub const fn max_submit_rns_data_payload_bytes(self) -> u16 {
        self.max_submit_rns_data_payload_bytes
    }

    /// Runtime availability of committed LXMF discovery and bounded reads.
    pub const fn lxmf(self) -> CapabilityAvailability {
        self.lxmf
    }

    /// Maximum exact normalized wire bytes returned by one LXMF read.
    pub const fn max_lxmf_read_chunk_bytes(self) -> u16 {
        self.max_lxmf_read_chunk_bytes
    }

    /// Runtime availability of source-free basic LXMF composition and submission.
    pub const fn lxmf_basic_send(self) -> CapabilityAvailability {
        self.lxmf_basic_send
    }

    /// Structural codec limit for one source-free basic-LXMF title.
    ///
    /// The encoded-body and product-carrier limits can reject a smaller
    /// title/content combination.
    pub const fn max_lxmf_basic_title_bytes(self) -> u16 {
        self.max_lxmf_basic_title_bytes
    }

    /// Structural codec limit for one source-free basic-LXMF content value.
    ///
    /// The encoded-body and product-carrier limits can reject a smaller
    /// title/content combination.
    pub const fn max_lxmf_basic_content_bytes(self) -> u16 {
        self.max_lxmf_basic_content_bytes
    }

    /// Runtime availability of bounded nearby `lxmf.delivery` peer discovery.
    pub const fn lxmf_peer_discovery(self) -> CapabilityAvailability {
        self.lxmf_peer_discovery
    }

    /// Maximum authenticated announce application data returned with one peer.
    pub const fn max_lxmf_peer_app_data_bytes(self) -> u16 {
        self.max_lxmf_peer_app_data_bytes
    }

    /// Runtime availability of bounded authenticated NomadNet page fetch.
    pub const fn nomad(self) -> CapabilityAvailability {
        self.nomad
    }

    /// Maximum UTF-8 request-path bytes accepted by NomadNet fetch.
    pub const fn max_nomad_page_path_bytes(self) -> u16 {
        self.max_nomad_page_path_bytes
    }

    /// Maximum valid UTF-8 Micron page bytes returned by NomadNet fetch.
    pub const fn max_nomad_page_bytes(self) -> u16 {
        self.max_nomad_page_bytes
    }

    /// Runtime availability of redacted network configuration and status.
    pub const fn network_config(self) -> CapabilityAvailability {
        self.network_config
    }

    /// Runtime availability of authenticated ordinary service announces.
    pub const fn manual_service_announce(self) -> CapabilityAvailability {
        self.manual_service_announce
    }

    /// Runtime availability of authenticated Reticulum path-and-proof probes.
    pub const fn reticulum_probe(self) -> CapabilityAvailability {
        self.reticulum_probe
    }

    /// Runtime availability of live PRNS node, interface, and route inspection.
    pub const fn route_diagnostics(self) -> CapabilityAvailability {
        self.route_diagnostics
    }
}

/// Receiver-local physical signal values for one received Reticulum carrier.
///
/// RSSI and SNR are one indivisible observation: the wire carries both values
/// or neither value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IngressSignal {
    rssi_dbm: i16,
    snr_db: i16,
}

impl IngressSignal {
    /// Preserve receiver-reported RSSI and SNR.
    pub const fn new(rssi_dbm: i16, snr_db: i16) -> Self {
        Self { rssi_dbm, snr_db }
    }

    /// Receiver-reported RSSI in dBm.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Receiver-reported SNR in dB.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }
}

/// First-arrival interface and optional final-hop signal evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IngressObservation {
    interface_id: ReticulumInterfaceId,
    signal: Option<IngressSignal>,
}

impl IngressObservation {
    /// Construct one immutable first-arrival observation.
    pub const fn new(interface_id: ReticulumInterfaceId, signal: Option<IngressSignal>) -> Self {
        Self {
            interface_id,
            signal,
        }
    }

    /// Complete Reticulum interface identity that received the carrier.
    pub const fn interface_id(self) -> ReticulumInterfaceId {
        self.interface_id
    }

    /// Optional receiver-local physical signal values.
    pub const fn signal(self) -> Option<IngressSignal> {
        self.signal
    }
}

/// Boot-scoped nonzero identifier for one Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProbeId([u8; 16]);

impl ProbeId {
    /// Validate one complete opaque boot-scoped probe identifier.
    pub const fn new(bytes: [u8; 16]) -> Result<Self, InvalidProbeId> {
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                return Ok(Self(bytes));
            }
            index += 1;
        }
        Err(InvalidProbeId)
    }

    /// Borrow the complete public identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// The all-zero value cannot identify a Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidProbeId;

/// Request to begin one path-and-proof probe to a known Reticulum destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeStartRequest {
    destination: DestinationHash,
    idempotency_key: IdempotencyKey,
}

impl ProbeStartRequest {
    /// Construct one principal-scoped idempotent probe request.
    pub const fn new(destination: DestinationHash, idempotency_key: IdempotencyKey) -> Self {
        Self {
            destination,
            idempotency_key,
        }
    }

    /// Known remote Reticulum destination being measured.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Principal-scoped request deduplication key.
    pub const fn idempotency_key(self) -> IdempotencyKey {
        self.idempotency_key
    }
}

/// Request to read one principal-owned boot-scoped probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbePollRequest {
    id: ProbeId,
}

impl ProbePollRequest {
    /// Construct a poll request for one accepted probe.
    pub const fn new(id: ProbeId) -> Self {
        Self { id }
    }

    /// Boot-scoped probe identifier.
    pub const fn id(self) -> ProbeId {
        self.id
    }
}

/// Whether probe start admitted fresh work or replayed an exact prior request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProbeStartOutcome {
    /// Fresh probe work was accepted.
    Accepted = 0,
    /// An exact principal-scoped idempotent request was already accepted.
    Replayed = 1,
}

impl ProbeStartOutcome {
    /// Stable numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Successful admission of one boot-scoped Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeStartAccepted {
    id: ProbeId,
    outcome: ProbeStartOutcome,
}

impl ProbeStartAccepted {
    /// Construct one start response.
    pub const fn new(id: ProbeId, outcome: ProbeStartOutcome) -> Self {
        Self { id, outcome }
    }

    /// Boot-scoped probe identifier.
    pub const fn id(self) -> ProbeId {
        self.id
    }

    /// Fresh-versus-replayed admission result.
    pub const fn outcome(self) -> ProbeStartOutcome {
        self.outcome
    }
}

/// Non-terminal phase of one accepted Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProbePhase {
    /// The node is resolving a usable path to the known destination.
    PathLookup = 0,
    /// The probe is waiting for transport-neutral outbound dispatch.
    AwaitingDispatch = 1,
    /// The probe was dispatched and is waiting for its Reticulum proof.
    AwaitingProof = 2,
}

impl ProbePhase {
    /// Stable numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Terminal failure of one accepted Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProbeFailure {
    /// The destination's public identity was not available for proof validation.
    IdentityUnavailable = 0,
    /// No usable Reticulum path was available.
    NoPath = 1,
    /// Transport-neutral packet dispatch failed.
    Dispatch = 2,
    /// A path, dispatch, or proof deadline expired.
    Timeout = 3,
    /// A local invariant failed without exposing implementation details.
    Internal = 4,
}

impl ProbeFailure {
    /// Stable numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Successful end-to-end Reticulum probe measurement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeSuccess {
    round_trip_ms: u32,
    hops: u8,
    ingress: IngressObservation,
}

impl ProbeSuccess {
    /// Preserve the bounded round-trip, route, and proof-arrival evidence.
    pub const fn new(round_trip_ms: u32, hops: u8, ingress: IngressObservation) -> Self {
        Self {
            round_trip_ms,
            hops,
            ingress,
        }
    }

    /// Complete measured round-trip duration in milliseconds.
    pub const fn round_trip_ms(self) -> u32 {
        self.round_trip_ms
    }

    /// Reticulum hop count associated with the successful probe.
    pub const fn hops(self) -> u8 {
        self.hops
    }

    /// Receiver-local final-hop evidence for the returning proof.
    pub const fn ingress_observation(self) -> IngressObservation {
        self.ingress
    }
}

/// Current or terminal state of one boot-scoped Reticulum probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbePollResponse {
    /// Work remains in progress at the named phase.
    Pending(ProbePhase),
    /// A valid Reticulum proof completed the probe.
    Succeeded(ProbeSuccess),
    /// The probe ended with a bounded public failure category.
    Failed(ProbeFailure),
}

/// SHA-256 digest of every byte in one complete encoded Reticulum packet.
///
/// This is deliberately a distinct type from Reticulum's proof-correlation
/// hash, which covers only the protocol-defined hashable part of a packet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EncodedPacketSha256([u8; 32]);

impl EncodedPacketSha256 {
    /// Construct an encoded-packet digest from its complete bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Prepared-packet diagnostics that never expose the packet itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedPacketDetails {
    /// Encoded packet length.
    pub packet_len: u16,
    /// SHA-256 of every byte in the complete encoded packet.
    pub encoded_packet_sha256: EncodedPacketSha256,
}

/// Progress of an accepted submission, without prepared packet bytes.
///
/// State-specific data lives in the corresponding variant, so contradictory
/// combinations such as a queued submission with a packet hash or a failed
/// submission without a failure category cannot be represented. This is a
/// closed wire vocabulary; adding a numeric state requires a new API
/// major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionState {
    /// Accepted into a bounded intent queue.
    Queued,
    /// Currently being processed by the node owner.
    Preparing,
    /// Packet preparation completed and delivery is pending.
    ///
    /// The details are durable status metadata; this state does not imply that
    /// encoded packet bytes still occupy the node's private transmit outbox.
    AwaitingDelivery(PreparedPacketDetails),
    /// A later proof or application acknowledgement completed the submission.
    Delivered(PreparedPacketDetails),
    /// The application protocol completed without exposing a prepared RNS packet.
    ///
    /// LXMF uses this state after ordinary PRNS receipt settlement. PRNS owns
    /// the encrypted packet and its proof evidence, while the product status
    /// reports only completion of the durable application intent.
    ApplicationDelivered,
    /// Submission terminated with a typed failure.
    Failed(SubmissionFailure),
    /// Submission was cancelled before it became irreversible.
    Cancelled,
}

impl SubmissionState {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Queued => 0,
            Self::Preparing => 1,
            Self::AwaitingDelivery(_) => 2,
            Self::Delivered(_) => 3,
            Self::Failed(_) => 4,
            Self::Cancelled => 5,
            Self::ApplicationDelivered => 6,
        }
    }
}

/// Stable failure category suitable for a submission status response.
///
/// This is a closed wire vocabulary. Adding a numeric category requires
/// a new API major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SubmissionFailure {
    /// Destination is not currently reachable through a known path.
    NoPath = 0,
    /// An accepted submission received no required proof or acknowledgement
    /// before its delivery deadline.
    DeliveryTimeout = 1,
    /// Accepted work was later rejected by a downstream protocol or policy
    /// stage that could not decide at request admission.
    Rejected = 2,
    /// Processing failed for a non-client fault.
    Internal = 3,
}

impl SubmissionFailure {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Admission result for an authenticated manual ordinary service announce.
///
/// This is a closed wire vocabulary. Both outcomes are successful:
/// duplicate requests coalesce instead of consuming additional queue capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ManualServiceAnnounceDisposition {
    /// A fresh set of ordinary service announces was queued.
    Queued = 0,
    /// An equivalent ordinary service announce was already pending.
    AlreadyPending = 1,
}

impl ManualServiceAnnounceDisposition {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Scalar-only status for an accepted submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmissionStatus {
    /// Submission being described.
    pub id: SubmissionId,
    /// Current state.
    pub state: SubmissionState,
}

/// Typed API error returned in a logical response.
///
/// This is a closed wire vocabulary. Adding a numeric category requires a new
/// API major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ApiErrorCode {
    /// Client selected an unknown operation number.
    UnsupportedOperation = 1,
    /// Client selected an incompatible API major version.
    UnsupportedVersion = 2,
    /// Operation requires an authenticated application session.
    AuthenticationRequired = 3,
    /// Authenticated principal lacks the required permission.
    PermissionDenied = 4,
    /// Requested object does not exist for this principal.
    NotFound = 5,
    /// Request is semantically invalid after decoding.
    InvalidRequest = 6,
    /// Build or runtime profile cannot perform the operation.
    CapabilityUnavailable = 7,
    /// Device failed without a client-actionable category.
    Internal = 8,
    /// Operation was not accepted because a bounded queue or table is full.
    ///
    /// No submission identifier is allocated. Retrying later may succeed.
    CapacityExhausted = 9,
    /// This principal already used the supplied idempotency key for different
    /// request content.
    ///
    /// The conflicting request is not accepted. Repeating the original
    /// request content remains safe.
    IdempotencyConflict = 10,
    /// A transient device-owned resource is busy with another retained
    /// operation.
    ///
    /// The request was not rejected on semantic or capacity grounds. The
    /// authenticated session remains valid and retrying the exact operation
    /// after a short bounded delay is safe.
    RetryLater = 11,
}

impl ApiErrorCode {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u16 {
        self as u16
    }
}

/// Error response body with optional numeric operation context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApiErrorResponse {
    /// Stable machine-readable category.
    pub code: ApiErrorCode,
    /// Request operation related to the error, when known.
    pub operation: Option<u16>,
}

/// Successful or failed logical response body.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DeviceResponse {
    /// Result of `system.capabilities`.
    SystemCapabilities(CapabilitySnapshot),
    /// Result of `identity.summary`.
    IdentitySummary(IdentitySummary),
    /// Current durable product-owned appliance label.
    ApplianceLabel(ApplianceLabelSnapshot),
    /// Durable product-owned appliance-label mutation outcome.
    ApplianceLabelMutation(ApplianceLabelMutationOutcome),
    /// Result of `submission.status`.
    SubmissionStatus(SubmissionStatus),
    /// Next committed LXMF metadata entry in physical commit order.
    #[cfg(feature = "lxmf")]
    LxmfNext(LxmfMessageSummary),
    /// Bounded exact normalized-wire bytes for one committed LXMF message.
    #[cfg(feature = "lxmf")]
    LxmfRead(LxmfReadChunk),
    /// Durable LXMF collection state.
    #[cfg(feature = "lxmf")]
    LxmfMailboxStatus(LxmfMailboxStatus),
    /// Collection state after an idempotent monotonic acknowledgement.
    #[cfg(feature = "lxmf")]
    LxmfMailboxAcknowledged(LxmfMailboxStatus),
    /// Accepted source-free basic LXMF submission.
    #[cfg(feature = "lxmf")]
    LxmfBasicSendAccepted(LxmfBasicSendAccepted),
    /// One bounded page from nearby `lxmf.delivery` peer discovery.
    #[cfg(feature = "lxmf")]
    LxmfPeerNext(LxmfPeerDiscoveryPage),
    /// Accepted bounded NomadNet page fetch.
    #[cfg(feature = "nomad")]
    NomadFetchStartAccepted(NomadFetchStartAccepted),
    /// Current or terminal state of one bounded NomadNet page fetch.
    #[cfg(feature = "nomad")]
    NomadFetchPoll(NomadFetchPollResponse),
    /// Redacted desired Wi-Fi and Reticulum TCP configuration.
    #[cfg(feature = "network-config")]
    NetworkConfig(NetworkConfigSnapshot),
    /// Normal compare-and-swap desired-network mutation result.
    #[cfg(feature = "network-config")]
    NetworkConfigMutation(NetworkConfigMutationOutcome),
    /// Live Wi-Fi and Reticulum TCP state.
    #[cfg(feature = "network-config")]
    NetworkStatus(NetworkRuntimeStatus),
    /// Bounded cross-interface node diagnostics.
    NodeDiagnostics(NodeDiagnosticsSnapshot),
    /// Bounded lexicographically ordered route diagnostics.
    RouteDiagnosticsPage(RouteDiagnosticsPage),
    /// Bounded boot-scoped packet-correlated radio trace.
    RadioTracePage(RadioTracePage),
    /// Admission result for a manual ordinary service announce.
    ManualServiceAnnounce(ManualServiceAnnounceDisposition),
    /// Accepted boot-scoped Reticulum path-and-proof probe.
    ReticulumProbeStartAccepted(ProbeStartAccepted),
    /// Current or terminal state of one Reticulum path-and-proof probe.
    ReticulumProbePoll(ProbePollResponse),
    /// Accepted outbound RNS DATA submission.
    #[cfg(feature = "rns-data")]
    SubmitRnsDataAccepted(SubmissionAccepted),
    /// Typed request failure.
    Error(ApiErrorResponse),
}

impl DeviceResponse {
    /// Operation or response-kind number encoded on the wire.
    pub const fn kind(&self) -> u16 {
        match self {
            Self::SystemCapabilities(_) => OP_SYSTEM_CAPABILITIES,
            Self::IdentitySummary(_) => OP_IDENTITY_SUMMARY,
            Self::ApplianceLabel(_) => OP_APPLIANCE_LABEL_GET,
            Self::ApplianceLabelMutation(_) => OP_APPLIANCE_LABEL_MUTATE,
            Self::SubmissionStatus(_) => OP_SUBMISSION_STATUS,
            #[cfg(feature = "lxmf")]
            Self::LxmfNext(_) => OP_LXMF_NEXT,
            #[cfg(feature = "lxmf")]
            Self::LxmfRead(_) => OP_LXMF_READ,
            #[cfg(feature = "lxmf")]
            Self::LxmfMailboxStatus(_) => OP_LXMF_MAILBOX_STATUS,
            #[cfg(feature = "lxmf")]
            Self::LxmfMailboxAcknowledged(_) => OP_LXMF_MAILBOX_ACKNOWLEDGE,
            #[cfg(feature = "lxmf")]
            Self::LxmfBasicSendAccepted(_) => OP_LXMF_BASIC_SEND,
            #[cfg(feature = "lxmf")]
            Self::LxmfPeerNext(_) => OP_LXMF_PEER_NEXT,
            #[cfg(feature = "nomad")]
            Self::NomadFetchStartAccepted(_) => OP_NOMAD_FETCH_START,
            #[cfg(feature = "nomad")]
            Self::NomadFetchPoll(_) => OP_NOMAD_FETCH_POLL,
            #[cfg(feature = "network-config")]
            Self::NetworkConfig(_) => OP_NETWORK_CONFIG_GET,
            #[cfg(feature = "network-config")]
            Self::NetworkConfigMutation(_) => OP_NETWORK_CONFIG_MUTATE,
            #[cfg(feature = "network-config")]
            Self::NetworkStatus(_) => OP_NETWORK_STATUS,
            Self::NodeDiagnostics(_) => OP_NODE_DIAGNOSTICS,
            Self::RouteDiagnosticsPage(_) => OP_ROUTE_DIAGNOSTICS_PAGE,
            Self::RadioTracePage(_) => OP_RADIO_TRACE_PAGE,
            Self::ManualServiceAnnounce(_) => OP_MANUAL_SERVICE_ANNOUNCE,
            Self::ReticulumProbeStartAccepted(_) => OP_RETICULUM_PROBE_START,
            Self::ReticulumProbePoll(_) => OP_RETICULUM_PROBE_POLL,
            #[cfg(feature = "rns-data")]
            Self::SubmitRnsDataAccepted(_) => OP_SUBMIT_RNS_DATA,
            Self::Error(_) => RESPONSE_ERROR,
        }
    }
}

/// Logical response envelope encoded as exactly one CBOR item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResponseEnvelope {
    /// Protocol version selected by the device.
    pub version: ApiVersion,
    /// Request identifier copied from the request.
    pub request_id: RequestId,
    /// Operation-specific response.
    pub response: DeviceResponse,
}
