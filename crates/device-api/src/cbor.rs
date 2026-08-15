//! Allocation-free, strict CBOR codec for the logical device API.

use minicbor::{Decoder, Encoder, data::Type, encode::write::Cursor};

use crate::model::{
    API_VERSION_MAJOR, ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilityAvailability,
    CapabilitySnapshot, DestinationHash, DeviceRequest, DeviceResponse, DiagnosticInterfaceKind,
    DiagnosticInterfaceRecord, DiagnosticInterfaceState, DiagnosticLoraDataTxEvidence,
    DiagnosticLoraLastDataTx, DiagnosticLoraLastRx, DiagnosticLoraLastTx, DiagnosticLoraTxFamily,
    DiagnosticLoraTxOutcome, EncodedPacketSha256, IdempotencyKey, IdentityHash, IdentitySummary,
    IngressObservation, IngressSignal, LoraDiagnostics, MAX_BODY_BYTES, MAX_DIAGNOSTIC_INTERFACES,
    MAX_MESSAGE_BYTES, MAX_RADIO_TRACE_PAGE_ENTRIES, MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES,
    ManualServiceAnnounceDisposition, NodeDiagnosticsSnapshot, OP_IDENTITY_SUMMARY,
    OP_MANUAL_SERVICE_ANNOUNCE, OP_NODE_DIAGNOSTICS, OP_RADIO_TRACE_PAGE, OP_RETICULUM_PROBE_POLL,
    OP_RETICULUM_PROBE_START, OP_ROUTE_DIAGNOSTICS_PAGE, OP_SUBMISSION_STATUS,
    OP_SYSTEM_CAPABILITIES, PreparedPacketDetails, ProbeFailure, ProbeId, ProbePhase,
    ProbePollRequest, ProbePollResponse, ProbeStartAccepted, ProbeStartOutcome, ProbeStartRequest,
    ProbeSuccess, RESPONSE_ERROR, RadioTraceAppliedLoraProfile, RadioTraceAttemptOutcome,
    RadioTraceAttemptTerminal, RadioTraceAttemptToken, RadioTraceCursor, RadioTraceDataTx,
    RadioTraceEvent, RadioTraceEventKind, RadioTraceInboundProof, RadioTraceInboundProofPacket,
    RadioTraceInboundProofStage, RadioTraceLogicalRx, RadioTracePacketEvidence, RadioTracePage,
    RadioTracePageRequest, RadioTraceRouteSelected, RadioTraceTxOutcome, RequestEnvelope,
    RequestId, ResponseEnvelope, RnsDiagnostics, RouteDiagnosticEntry, RouteDiagnosticResolution,
    RouteDiagnosticsPage, RouteDiagnosticsRequest, SubmissionFailure, SubmissionId,
    SubmissionState, SubmissionStatus,
};
#[cfg(feature = "network-config")]
use crate::model::{
    GatewayPolicy, LoraRadioProfile, LoraTransmitPowerDbm, MAX_RETICULUM_DNS_DHCP_SERVERS,
    MAX_RETICULUM_DNS_RAW_ATTEMPTS, MAX_WIFI_NETWORK_PROFILES, MAX_WIFI_SSID_BYTES,
    NetworkConfigMutation, NetworkConfigMutationOutcome, NetworkConfigMutationRequest,
    NetworkConfigSnapshot, NetworkRuntimeStatus, OP_NETWORK_CONFIG_GET, OP_NETWORK_CONFIG_MUTATE,
    OP_NETWORK_STATUS, ReticulumDnsDiagnostics, ReticulumDnsPrimaryOutcome, ReticulumDnsRawAttempt,
    ReticulumDnsRawOutcome, ReticulumDnsRawSetupState, ReticulumDnsRawSource,
    ReticulumDnsResolution, ReticulumDnsResolutionSource, ReticulumTcpFailure,
    ReticulumTcpPeerConfigSummary, ReticulumTcpPeerHostConfigSummary, ReticulumTcpPeerHostUpdate,
    ReticulumTcpPeerHostname, ReticulumTcpPeerIpv4Address, ReticulumTcpPeerState,
    ReticulumTcpPeerUpdate, RmapConfig, RmapDeferredReason, RmapEgressConfirmation,
    RmapInitialTcpGateState, RmapLocation, RmapQueueOutcome, RmapRuntimeStatus, RmapStampPhase,
    WifiCredentialUpdate, WifiNetworkConfigSummary, WifiNetworkProfileId, WifiNetworkUpdate,
    WifiSsid, WifiStationState,
};
#[cfg(feature = "lxmf")]
use crate::model::{
    LxmfBasicSendAccepted, LxmfDiscoveredPeer, LxmfMailboxStatus, LxmfMessageHandle,
    LxmfMessageLocation, LxmfMessageSummary, LxmfPeerDiscoveryCursor, LxmfPeerDiscoveryIncarnation,
    LxmfPeerDiscoveryPage, LxmfPeerGeneration, LxmfReadChunk, LxmfReadLength,
    MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES, MAX_LXMF_PEER_APP_DATA_BYTES,
    MAX_LXMF_READ_CHUNK_BYTES, OP_LXMF_BASIC_SEND, OP_LXMF_MAILBOX_ACKNOWLEDGE,
    OP_LXMF_MAILBOX_STATUS, OP_LXMF_NEXT, OP_LXMF_PEER_NEXT, OP_LXMF_READ,
};
#[cfg(feature = "nomad")]
use crate::model::{
    MAX_NOMAD_PAGE_BYTES, MAX_NOMAD_PAGE_PATH_BYTES, NomadFetchFailure, NomadFetchId,
    NomadFetchPhase, NomadFetchPollRequest, NomadFetchPollResponse, NomadFetchStartAccepted,
    NomadFetchStartOutcome, NomadFetchStartRequest, NomadPage, NomadPagePath,
    NomadRequestTimestampUnixMs, OP_NOMAD_FETCH_POLL, OP_NOMAD_FETCH_START,
};
#[cfg(feature = "rns-data")]
use crate::model::{MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES, OP_SUBMIT_RNS_DATA, SubmissionAccepted};

const MAX_MAP_ENTRIES: u64 = 32;
/// Maximum container/tag nesting accepted while validating an operation body
/// or skipping an unknown field value.
pub const MAX_CBOR_NESTING_DEPTH: usize = 8;

/// A known field whose absence, duplication, length, or value is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredField {
    /// Envelope version map at key 0.
    EnvelopeVersion,
    /// Envelope request identifier at key 1.
    EnvelopeRequestId,
    /// Envelope operation/response kind at key 2.
    EnvelopeKind,
    /// Envelope operation-specific body at key 3.
    EnvelopeBody,
    /// Version major at key 0.
    VersionMajor,
    /// Version minor at key 1.
    VersionMinor,
    /// Submission identifier at body key 0.
    SubmissionId,
    /// Submission destination hash at body key 0.
    SubmitDestination,
    /// Submission payload at body key 1.
    SubmitPayload,
    /// Submission idempotency key at body key 2.
    SubmitIdempotencyKey,
    /// Capability API version at body key 0.
    CapabilityApiVersion,
    /// Capability raw packet-output flag at body key 1.
    CapabilityPacketOutput,
    /// Capability direct-radio-TX availability at body key 2.
    CapabilityDirectRadioTx,
    /// Capability outbound RNS DATA submission flag at body key 3.
    CapabilitySubmitRnsData,
    /// Capability logical message limit at body key 4.
    CapabilityMaxMessageBytes,
    /// Capability body limit at body key 5.
    CapabilityMaxBodyBytes,
    /// Capability RNS DATA submission payload limit at body key 6.
    CapabilityMaxSubmitPayloadBytes,
    /// Capability LXMF read availability at body key 9.
    CapabilityLxmf,
    /// Capability maximum LXMF read chunk bytes at body key 10.
    CapabilityMaxLxmfReadChunkBytes,
    /// Capability basic LXMF send availability at body key 11.
    CapabilityLxmfBasicSend,
    /// Capability maximum basic LXMF title bytes at body key 12.
    CapabilityMaxLxmfBasicTitleBytes,
    /// Capability maximum basic LXMF content bytes at body key 13.
    CapabilityMaxLxmfBasicContentBytes,
    /// Capability nearby LXMF peer-discovery availability at body key 14.
    CapabilityLxmfPeerDiscovery,
    /// Capability maximum nearby-peer application data at body key 15.
    CapabilityMaxLxmfPeerAppDataBytes,
    /// Capability bounded NomadNet fetch availability at body key 16.
    CapabilityNomad,
    /// Capability maximum NomadNet page path at body key 17.
    CapabilityMaxNomadPagePathBytes,
    /// Capability maximum NomadNet page body at body key 18.
    CapabilityMaxNomadPageBytes,
    /// Capability network-configuration availability at body key 19.
    CapabilityNetworkConfig,
    /// Capability manual-service-announce availability at body key 20.
    CapabilityManualServiceAnnounce,
    /// Capability Reticulum probe availability at body key 21.
    CapabilityReticulumProbe,
    /// Manual service announce admission disposition at body key 0.
    ManualServiceAnnounceDisposition,
    /// Known Reticulum destination at probe-start key 0.
    ProbeStartDestination,
    /// Principal-scoped idempotency key at probe-start key 1.
    ProbeStartIdempotencyKey,
    /// Boot-scoped nonzero probe identifier.
    ProbeId,
    /// Fresh-versus-replayed probe-start result at body key 1.
    ProbeStartOutcome,
    /// Probe poll state at body key 0.
    ProbePollState,
    /// State-specific probe poll value at body key 1.
    ProbePollValue,
    /// Non-terminal probe phase at body key 1.
    ProbePhase,
    /// Terminal probe failure at body key 1.
    ProbeFailure,
    /// Probe round-trip duration at success key 0.
    ProbeRoundTripMs,
    /// Probe Reticulum hop count at success key 1.
    ProbeHops,
    /// Returning-proof ingress evidence at success key 2.
    ProbeIngressObservation,
    /// Device-local interface at ingress-observation key 0.
    IngressInterface,
    /// Receiver-local RSSI at ingress-observation key 1.
    IngressRssi,
    /// Receiver-local SNR at ingress-observation key 2.
    IngressSnr,
    /// Network mutation discriminator at request body key 0.
    NetworkConfigMutationKind,
    /// Network mutation-specific value at request body key 1.
    NetworkConfigMutationValue,
    /// Expected complete configuration revision at request body key 2.
    NetworkConfigExpectedRevision,
    /// Network mutation idempotency key at request body key 3.
    NetworkConfigIdempotencyKey,
    /// Redacted Wi-Fi profile array at network-config body key 1.
    NetworkConfigWifiProfiles,
    /// Optional Reticulum TCP peer record at network-config body key 2.
    NetworkConfigTcpPeer,
    /// Global Wi-Fi transport enabled flag at network-config body key 3.
    NetworkConfigWifiTransportEnabled,
    /// Scheduled ordinary announce flag at network-config body key 4.
    NetworkConfigAutomaticAnnouncesEnabled,
    /// RMAP discovery enabled flag at network-config body key 5.
    NetworkConfigRmapDiscoveryEnabled,
    /// RMAP location-sharing flag at network-config body key 6.
    NetworkConfigRmapShareLocation,
    /// Optional phone position at network-config body key 7.
    NetworkConfigRmapPhoneLocation,
    /// Optional hostname Reticulum TCP peer at network-config body key 8.
    NetworkConfigTcpHostPeer,
    /// Complete LoRa radio profile at network-config body key 9.
    NetworkConfigLoraProfile,
    /// LoRa center frequency at profile key 0.
    LoraProfileFrequencyHz,
    /// LoRa bandwidth at profile key 1.
    LoraProfileBandwidthHz,
    /// LoRa spreading factor at profile key 2.
    LoraProfileSpreadingFactor,
    /// LoRa coding-rate denominator at profile key 3.
    LoraProfileCodingRate,
    /// LoRa requested power at profile key 4.
    LoraProfileTxPowerDbm,
    /// Opaque nonzero Wi-Fi profile identity.
    WifiNetworkProfileId,
    /// Network configuration revision at body key 0.
    NetworkConfigRevision,
    /// Network mutation outcome discriminator at body key 0.
    NetworkConfigMutationOutcome,
    /// Whether an applied configuration needs a reboot.
    NetworkConfigRebootRequired,
    /// Wi-Fi enabled flag at profile key 0.
    WifiEnabled,
    /// Wi-Fi station-selection priority.
    WifiPriority,
    /// Wi-Fi SSID at profile key 1.
    WifiSsid,
    /// Wi-Fi credential update or configured flag.
    WifiCredential,
    /// Wi-Fi credential-update discriminator at key 0.
    WifiCredentialUpdateKind,
    /// Replacement Wi-Fi passphrase at credential-update key 1.
    WifiPassphrase,
    /// Reticulum TCP peer enabled flag at peer key 0.
    ReticulumTcpPeerEnabled,
    /// Reticulum TCP peer IPv4 address at peer key 1.
    ReticulumTcpPeerIpv4Address,
    /// Reticulum TCP peer DNS hostname at peer key 1.
    ReticulumTcpPeerHostname,
    /// Reticulum TCP peer port at peer key 2.
    ReticulumTcpPeerPort,
    /// Gateway policy Wi-Fi transport flag at key 0.
    GatewayPolicyWifiTransportEnabled,
    /// Gateway policy automatic-announce flag at key 1.
    GatewayPolicyAutomaticAnnouncesEnabled,
    /// RMAP discovery flag at key 0.
    RmapDiscoveryEnabled,
    /// RMAP location-sharing flag at key 1.
    RmapShareLocation,
    /// Optional RMAP phone location at key 2.
    RmapPhoneLocation,
    /// RMAP latitude in microdegrees at location key 0.
    RmapLatitudeE6,
    /// RMAP longitude in microdegrees at location key 1.
    RmapLongitudeE6,
    /// Applied configuration revision at network-status key 1.
    NetworkAppliedRevision,
    /// Live Wi-Fi station state at network-status key 2.
    NetworkWifiState,
    /// Optional active Wi-Fi profile at network-status key 3.
    NetworkActiveWifiProfile,
    /// Optional connected SSID at network-status key 4.
    NetworkConnectedSsid,
    /// Optional IPv4 address at network-status key 5.
    NetworkIpv4Address,
    /// Optional whole-dBm RSSI at network-status key 6.
    NetworkRssiDbm,
    /// Live Reticulum TCP peer state at network-status key 7.
    NetworkTcpPeerState,
    /// Optional last outbound Reticulum TCP failure at network-status key 8.
    NetworkLastTcpFailure,
    /// Optional bounded DNS diagnostics at network-status key 9.
    NetworkDnsDiagnostics,
    /// Optional RMAP publication diagnostics at network-status key 10.
    NetworkRmapStatus,
    /// Exact compact RMAP status array.
    RmapRuntimeStatus,
    /// Applied-configuration flag at RMAP status index 0.
    RmapRuntimeConfigApplied,
    /// Stamp phase at RMAP status index 1.
    RmapRuntimeStampPhase,
    /// Stamp-attempt count at RMAP status index 2.
    RmapRuntimeStampAttempts,
    /// Initial TCP gate state at RMAP status index 3.
    RmapRuntimeInitialTcpGate,
    /// Accepted publication count at RMAP status index 4.
    RmapRuntimeQueuedCount,
    /// Last queue outcome at RMAP status index 5.
    RmapRuntimeLastQueueOutcome,
    /// Optional last queue-attempt uptime at RMAP status index 6.
    RmapRuntimeLastQueueAt,
    /// Physical-egress evidence at RMAP status index 7.
    RmapRuntimeEgressConfirmation,
    /// Optional next-due delay at RMAP status index 8.
    RmapRuntimeNextDue,
    /// Optional deferral reason at RMAP status index 9.
    RmapRuntimeDeferredReason,
    /// Optional DHCP gateway at DNS-diagnostics key 0.
    ReticulumDnsGatewayIpv4,
    /// Fixed DHCP resolver slots at DNS-diagnostics key 1.
    ReticulumDnsDhcpServers,
    /// System resolver outcome at DNS-diagnostics key 2.
    ReticulumDnsPrimaryOutcome,
    /// Raw UDP socket state at DNS-diagnostics key 3.
    ReticulumDnsRawSetupState,
    /// Fixed raw resolver-attempt slots at DNS-diagnostics key 4.
    ReticulumDnsRawAttempts,
    /// Optional successful resolution at DNS-diagnostics key 5.
    ReticulumDnsResolution,
    /// Raw resolver source at raw-attempt key 0.
    ReticulumDnsRawSource,
    /// Raw resolver IPv4 address at raw-attempt key 1.
    ReticulumDnsRawServer,
    /// Raw resolver outcome at raw-attempt key 2.
    ReticulumDnsRawOutcome,
    /// Nonzero DNS response code at raw-attempt key 3.
    ReticulumDnsResponseCode,
    /// Resolved TCP peer IPv4 address at resolution key 0.
    ReticulumDnsResolvedIpv4,
    /// Successful resolution source at resolution key 1.
    ReticulumDnsResolutionSource,
    /// Optional exact successful resolver at resolution key 2.
    ReticulumDnsResolutionResolver,
    /// Identity summary primary destination hash at body key 0.
    IdentityPrimaryDestination,
    /// Optional identity summary `lxmf.delivery` destination hash at body key 1.
    IdentityLxmfDeliveryDestination,
    /// Optional exclusive LXMF listing cursor at request body key 0.
    LxmfAfterHandle,
    /// Stable committed LXMF message handle.
    LxmfHandle,
    /// Python-compatible LXMF message ID.
    LxmfMessageId,
    /// Local LXMF delivery destination.
    LxmfDestination,
    /// Authenticated LXMF source destination.
    LxmfSource,
    /// Exact LXMF timestamp bits.
    LxmfTimestampBits,
    /// Complete normalized LXMF wire length.
    LxmfNormalizedWireLength,
    /// Decoded LXMF title length.
    LxmfTitleLength,
    /// Decoded LXMF content length.
    LxmfContentLength,
    /// Encoded LXMF fields-map length.
    LxmfFieldsEncodedLength,
    /// SHA-256 of exact normalized LXMF wire bytes.
    LxmfExactWireSha256,
    /// First-arrival observation at LXMF summary key 10.
    IngressObservation,
    /// Zero-based LXMF read offset.
    LxmfReadOffset,
    /// Requested maximum LXMF read bytes.
    LxmfReadMaxBytes,
    /// Exact bytes returned by an LXMF read.
    LxmfReadBytes,
    /// Latest committed LXMF handle at mailbox-status key 0.
    LxmfMailboxLatest,
    /// Durable collection watermark at mailbox-status key 1.
    LxmfMailboxAcknowledgedThrough,
    /// Exact uncollected message count at mailbox-status key 2.
    LxmfMailboxUncollectedCount,
    /// Basic LXMF send destination at request body key 0.
    LxmfBasicSendDestination,
    /// Basic LXMF send Unix-millisecond timestamp at request body key 1.
    LxmfBasicSendTimestampUnixMs,
    /// Basic LXMF send binary title at request body key 2.
    LxmfBasicSendTitle,
    /// Basic LXMF send binary content at request body key 3.
    LxmfBasicSendContent,
    /// Basic LXMF send idempotency key at request body key 4.
    LxmfBasicSendIdempotencyKey,
    /// Optional LXMF message location at request body key 5.
    LxmfBasicSendLocation,
    /// Message latitude in microdegrees at location key 0.
    LxmfLocationLatitudeE6,
    /// Message longitude in microdegrees at location key 1.
    LxmfLocationLongitudeE6,
    /// Message altitude in centimetres at location key 2.
    LxmfLocationAltitudeCm,
    /// Message speed in centimetres per second at location key 3.
    LxmfLocationSpeedCmPerSecond,
    /// Message bearing in centidegrees at location key 4.
    LxmfLocationBearingCentidegrees,
    /// Message horizontal accuracy in centimetres at location key 5.
    LxmfLocationAccuracyCm,
    /// Message location-fix time in Unix seconds at location key 6.
    LxmfLocationUpdatedAtUnixSeconds,
    /// Peer-discovery cursor incarnation at request/response body key 0.
    LxmfPeerCursorIncarnation,
    /// Peer-discovery cursor exclusive generation at request/response body key 1.
    LxmfPeerCursorGeneration,
    /// Latest peer observation generation at response body key 2.
    LxmfPeerLatestGeneration,
    /// Oldest retained peer generation at response body key 3.
    LxmfPeerOldestGeneration,
    /// Peer-discovery history-gap flag at response body key 4.
    LxmfPeerHistoryGap,
    /// Optional peer record at response body key 5.
    LxmfPeerRecord,
    /// Announced `lxmf.delivery` destination at peer key 0.
    LxmfPeerDestination,
    /// Announce-authenticating identity hash at peer key 1.
    LxmfPeerIdentityHash,
    /// Authenticated announce application data at peer key 2.
    LxmfPeerAppData,
    /// Latest observed Reticulum hop count at peer key 3.
    LxmfPeerHops,
    /// Product-owned observing-interface scalar at peer key 4.
    LxmfPeerInterfaceId,
    /// Optional whole-dBm RSSI at peer key 5.
    LxmfPeerRssiDbm,
    /// Optional whole-dB SNR at peer key 6.
    LxmfPeerSnrDb,
    /// Saturating observation age in milliseconds at peer key 7.
    LxmfPeerObservedAge,
    /// Nonzero peer observation generation at peer key 8.
    LxmfPeerGeneration,
    /// NomadNet fetch destination at start-request body key 0.
    NomadFetchDestination,
    /// NomadNet page path at start-request body key 1.
    NomadFetchPath,
    /// NomadNet request timestamp at start-request body key 2.
    NomadFetchTimestampUnixMs,
    /// NomadNet idempotency key at start-request body key 3.
    NomadFetchIdempotencyKey,
    /// Opaque NomadNet fetch identifier.
    NomadFetchId,
    /// Fresh-versus-replayed start outcome at response body key 1.
    NomadFetchStartOutcome,
    /// NomadNet poll response state at body key 0.
    NomadFetchState,
    /// State-specific NomadNet poll response value at body key 1.
    NomadFetchValue,
    /// Non-terminal NomadNet fetch phase at body key 1.
    NomadFetchPhase,
    /// Ready NomadNet Micron page at body key 1.
    NomadFetchPage,
    /// Terminal NomadNet fetch failure at body key 1.
    NomadFetchFailure,
    /// Node uptime at diagnostics body key 0.
    NodeDiagnosticsUptime,
    /// Fixed interface slots at diagnostics body key 1.
    NodeDiagnosticsInterfaces,
    /// Optional LoRa record at diagnostics body key 2.
    NodeDiagnosticsLora,
    /// Reticulum counters at diagnostics body key 3.
    NodeDiagnosticsRns,
    /// Observed peer count at diagnostics body key 4.
    NodeDiagnosticsObservedPeers,
    /// Retained route count at diagnostics body key 5.
    NodeDiagnosticsRetainedRoutes,
    /// Usable route count at diagnostics body key 6.
    NodeDiagnosticsUsableRoutes,
    /// Interface identifier at interface key 0.
    DiagnosticInterfaceId,
    /// Interface family at interface key 1.
    DiagnosticInterfaceKind,
    /// Interface state at interface key 2.
    DiagnosticInterfaceState,
    /// Interface generation at interface key 3.
    DiagnosticInterfaceGeneration,
    /// Logical interface MTU at interface key 4.
    DiagnosticInterfaceLogicalMtu,
    /// Optional interface bitrate at interface key 5.
    DiagnosticInterfaceBitrate,
    /// Applied LoRa transmit power at LoRa key 0.
    DiagnosticLoraTxPower,
    /// Applied LoRa frequency at LoRa key 1.
    DiagnosticLoraFrequency,
    /// Applied LoRa bandwidth at LoRa key 2.
    DiagnosticLoraBandwidth,
    /// Applied LoRa spreading factor at LoRa key 3.
    DiagnosticLoraSpreadingFactor,
    /// Applied LoRa coding-rate denominator at LoRa key 4.
    DiagnosticLoraCodingRate,
    /// Received LoRa physical frames at LoRa key 5.
    DiagnosticLoraRxPhysicalFrames,
    /// Reconstructed LoRa packets at LoRa key 6.
    DiagnosticLoraRxPackets,
    /// LoRa receive errors at LoRa key 7.
    DiagnosticLoraRxErrors,
    /// LoRa receive drops at LoRa key 8.
    DiagnosticLoraRxDrops,
    /// Terminal LoRa jobs at LoRa key 9.
    DiagnosticLoraTxTerminalJobs,
    /// Successful LoRa jobs at LoRa key 10.
    DiagnosticLoraTxSuccesses,
    /// Completed LoRa physical frames at LoRa key 11.
    DiagnosticLoraTxCompletedFrames,
    /// LoRa channel-access rejections at LoRa key 12.
    DiagnosticLoraTxAccessRejects,
    /// Other LoRa transmission failures at LoRa key 13.
    DiagnosticLoraTxFailures,
    /// Busy LoRa CAD outcomes at LoRa key 14.
    DiagnosticLoraCadBusy,
    /// Clear LoRa CAD outcomes at LoRa key 15.
    DiagnosticLoraCadClear,
    /// Optional most-recent LoRa receive at LoRa key 16.
    DiagnosticLoraLastRx,
    /// Optional most-recent LoRa transmit result at LoRa key 17.
    DiagnosticLoraLastTx,
    /// Optional retained most-recent DATA transmit result at LoRa key 18.
    DiagnosticLoraLastDataTx,
    /// Age at last-radio-event key 0.
    DiagnosticLoraEventAge,
    /// RSSI at last-receive key 1.
    DiagnosticLoraLastRxRssi,
    /// SNR at last-receive key 2.
    DiagnosticLoraLastRxSnr,
    /// Outcome at last-transmit key 1.
    DiagnosticLoraLastTxOutcome,
    /// Packet-owner family at last-transmit key 2.
    DiagnosticLoraLastTxFamily,
    /// Selected Reticulum interface at last-transmit key 3.
    DiagnosticLoraLastTxInterface,
    /// Complete encoded DATA packet length at last-transmit key 4.
    DiagnosticLoraLastTxPacketLength,
    /// Complete encoded DATA packet SHA-256 at last-transmit key 5.
    DiagnosticLoraLastTxPacketSha256,
    /// Received Reticulum packets at RNS key 0.
    DiagnosticRnsReceived,
    /// Forwarded Reticulum packets at RNS key 1.
    DiagnosticRnsForwarded,
    /// Reticulum deduplication drops at RNS key 2.
    DiagnosticRnsDedupDrops,
    /// Invalid Reticulum drops at RNS key 3.
    DiagnosticRnsInvalidDrops,
    /// Received Reticulum announces at RNS key 4.
    DiagnosticRnsAnnouncesReceived,
    /// Learned Reticulum paths at RNS key 5.
    DiagnosticRnsPathsLearned,
    /// Expired Reticulum paths at RNS key 6.
    DiagnosticRnsPathsExpired,
    /// Established Reticulum links at RNS key 7.
    DiagnosticRnsLinksEstablished,
    /// Closed Reticulum links at RNS key 8.
    DiagnosticRnsLinksClosed,
    /// Failed Reticulum links at RNS key 9.
    DiagnosticRnsLinksFailed,
    /// Optional exclusive route cursor at request body key 0.
    RouteDiagnosticsAfter,
    /// Route-table revision at response body key 0.
    RouteDiagnosticsRevision,
    /// Total retained route count at response body key 1.
    RouteDiagnosticsTotalCount,
    /// Fixed route record slots at response body key 2.
    RouteDiagnosticsEntries,
    /// Optional continuation cursor at response body key 3.
    RouteDiagnosticsNextCursor,
    /// Route destination at route key 0.
    RouteDiagnosticDestination,
    /// Optional next-hop identity at route key 1.
    RouteDiagnosticNextHop,
    /// Route hop count at route key 2.
    RouteDiagnosticHops,
    /// Optional retained interface at route key 3.
    RouteDiagnosticInterface,
    /// Current route resolution at route key 4.
    RouteDiagnosticResolution,
    /// Optional learned age at route key 5.
    RouteDiagnosticLearnedAge,
    /// Optional last-used age at route key 6.
    RouteDiagnosticLastUsedAge,
    /// Optional remaining lifetime at route key 7.
    RouteDiagnosticExpiresIn,
    /// Optional exclusive radio-trace cursor at request key 0.
    RadioTraceAfterCursor,
    /// Boot identifier at radio-trace cursor key 0.
    RadioTraceCursorBootId,
    /// Exclusive sequence at radio-trace cursor key 1.
    RadioTraceCursorSequence,
    /// Radio-trace boot identifier at page key 0.
    RadioTraceBootId,
    /// Applied LoRa profile at page key 1.
    RadioTraceAppliedLoraProfile,
    /// Oldest retained radio-trace sequence at page key 2.
    RadioTraceOldestSequence,
    /// Next allocatable radio-trace sequence at page key 3.
    RadioTraceNextSequence,
    /// Radio-trace history-loss marker at page key 4.
    RadioTraceHistoryLost,
    /// Fixed radio-trace event slots at page key 5.
    RadioTraceEntries,
    /// Optional radio-trace continuation cursor at page key 6.
    RadioTraceNextCursor,
    /// Configuration fingerprint at applied-profile key 0.
    RadioTraceProfileFingerprint,
    /// Carrier frequency at applied-profile key 1.
    RadioTraceProfileFrequency,
    /// LoRa bandwidth at applied-profile key 2.
    RadioTraceProfileBandwidth,
    /// Preamble symbols at applied-profile key 3.
    RadioTraceProfilePreamble,
    /// Requested transmit power at applied-profile key 4.
    RadioTraceProfilePower,
    /// Spreading factor at applied-profile key 5.
    RadioTraceProfileSpreadingFactor,
    /// Coding-rate denominator at applied-profile key 6.
    RadioTraceProfileCodingRate,
    /// Explicit-header flag at applied-profile key 7.
    RadioTraceProfileExplicitHeader,
    /// Packet CRC flag at applied-profile key 8.
    RadioTraceProfileCrc,
    /// IQ-inversion flag at applied-profile key 9.
    RadioTraceProfileIqInverted,
    /// Boot-scoped event sequence at event key 0.
    RadioTraceEventSequence,
    /// Monotonic observation time at event key 1.
    RadioTraceEventObservedAt,
    /// Event discriminator at event key 2.
    RadioTraceEventKind,
    /// Event-specific value at event key 3.
    RadioTraceEventValue,
    /// Product-owned interface at packet-evidence key 0.
    RadioTracePacketInterface,
    /// Complete encoded packet length at packet-evidence key 1.
    RadioTracePacketLength,
    /// Complete encoded packet SHA-256 at packet-evidence key 2.
    RadioTracePacketSha256,
    /// Optional proof-correlation token at packet-evidence key 3.
    RadioTracePacketAttemptToken,
    /// Detailed terminal DATA outcome at event-value key 4.
    RadioTraceTxOutcome,
    /// Planned physical frame count at event-value key 5.
    RadioTraceTxPlannedFrames,
    /// Completed physical frame count at event-value key 6.
    RadioTraceTxCompletedFrames,
    /// Byte-exposure authorization marker at event-value key 7.
    RadioTraceTxAuthorizationObserved,
    /// Per-frame TxDone timestamps at event-value key 8.
    RadioTraceTxFrameCompletedAt,
    /// Receiver-local RSSI at event-value key 4.
    RadioTraceRxRssi,
    /// Receiver-local SNR at event-value key 5.
    RadioTraceRxSnr,
    /// Routed destination at route-event key 4.
    RadioTraceRouteDestination,
    /// Optional next-hop identity at route-event key 5.
    RadioTraceRouteNextHop,
    /// Hop count at route-event key 6.
    RadioTraceRouteHops,
    /// Resolution category at route-event key 7.
    RadioTraceRouteResolution,
    /// Durable submission identifier at route-event key 8.
    RadioTraceRouteSubmissionId,
    /// Attempt token at attempt-terminal key 0.
    RadioTraceTerminalAttemptToken,
    /// Attempt outcome at attempt-terminal key 1.
    RadioTraceTerminalOutcome,
    /// Optional proof ingress at attempt-terminal key 2.
    RadioTraceTerminalProofIngress,
    /// Correlation token at inbound-proof key 0.
    RadioTraceInboundProofCorrelationToken,
    /// Durable receiver lifecycle stage at inbound-proof key 1.
    RadioTraceInboundProofStage,
    /// Optional validated LXMF message identifier at inbound-proof key 2.
    RadioTraceInboundProofMessageId,
    /// Optional encoded packet SHA-256 at inbound-proof key 3.
    RadioTraceInboundProofPacketSha256,
    /// Optional encoded packet length at inbound-proof key 4.
    RadioTraceInboundProofPacketLength,
    /// Optional receive or proof-return interface at inbound-proof key 5.
    RadioTraceInboundProofInterface,
    /// Optional receiver signal pair at inbound-proof key 6.
    RadioTraceInboundProofSignal,
    /// Optional physical proof-dispatch outcome at inbound-proof key 7.
    RadioTraceInboundProofDispatchOutcome,
    /// Submission state at body key 1.
    SubmissionState,
    /// State-specific prepared packet length at body key 2.
    SubmissionPacketLength,
    /// State-specific encoded-packet SHA-256 at body key 3.
    SubmissionEncodedPacketSha256,
    /// Failed-state submission category at body key 4.
    SubmissionFailure,
    /// API error code at body key 0.
    ErrorCode,
    /// Optional API error operation at body key 1.
    ErrorOperation,
}

/// Failure to decode exactly one bounded logical API message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// Input exceeds [`MAX_MESSAGE_BYTES`].
    MessageTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Encoded operation body exceeds [`MAX_BODY_BYTES`].
    BodyTooLarge {
        /// Supplied encoded body byte count.
        actual: usize,
        /// Accepted encoded body byte count.
        max: usize,
    },
    /// Input is not the expected, definite-map CBOR shape.
    Malformed,
    /// An indefinite-length byte string, text string, array, or map appeared.
    IndefiniteLength,
    /// A body or unknown field exceeds [`MAX_CBOR_NESTING_DEPTH`].
    NestingTooDeep {
        /// Attempted container/tag nesting depth.
        actual: usize,
        /// Accepted container/tag nesting depth.
        max: usize,
    },
    /// One definite map contains too many fields for bounded processing.
    TooManyMapEntries {
        /// Declared number of fields.
        actual: u64,
        /// Accepted number of fields.
        max: u64,
    },
    /// Complete CBOR item was followed by additional bytes.
    TrailingData,
    /// Required known field was absent.
    MissingField(RequiredField),
    /// Known field appeared more than once.
    DuplicateField(RequiredField),
    /// Fixed-width byte string had the wrong length.
    InvalidByteStringLength {
        /// Field being decoded.
        field: RequiredField,
        /// Required byte count.
        expected: usize,
        /// Supplied byte count.
        actual: usize,
    },
    /// A fixed-capacity diagnostic array had the wrong number of slots.
    InvalidArrayLength {
        /// Field being decoded.
        field: RequiredField,
        /// Required slot count.
        expected: u64,
        /// Supplied slot count.
        actual: u64,
    },
    /// Application payload exceeds its semantic limit.
    PayloadTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// An LXMF read response exceeded its fixed owned chunk limit.
    LxmfReadChunkTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// A basic LXMF title exceeded its individual semantic limit.
    LxmfBasicTitleTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Basic LXMF content exceeded its individual semantic limit.
    LxmfBasicContentTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Nearby LXMF announce application data exceeded its fixed response owner.
    LxmfPeerAppDataTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// A NomadNet page path exceeded its fixed request limit.
    NomadPagePathTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// A NomadNet page path was empty, relative, or contained a NUL byte.
    InvalidNomadPagePath,
    /// A ready NomadNet page exceeded its fixed response owner.
    NomadPageTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// A ready NomadNet page was not valid UTF-8.
    InvalidNomadPageUtf8,
    /// A NomadNet fetch identifier contained the reserved zero sequence.
    InvalidNomadFetchId,
    /// A Reticulum probe identifier used the reserved all-zero value.
    InvalidProbeId,
    /// A LoRa last-TX record carried partial or contradictory DATA evidence.
    InvalidLoraLastTx,
    /// A probe poll response carried contradictory state-specific fields.
    InvalidProbePollResponse,
    /// A Wi-Fi SSID was empty or exceeded its fixed byte limit.
    InvalidWifiSsid,
    /// A redacted response exceeded the fixed saved-profile count.
    TooManyWifiNetworkProfiles {
        /// Declared profile count.
        actual: u64,
        /// Accepted profile count.
        max: u64,
    },
    /// A WPA2-Personal passphrase violated its fixed byte limits.
    InvalidWifiPassphrase,
    /// A Wi-Fi profile used the reserved all-zero identity.
    InvalidWifiNetworkProfileId,
    /// A Reticulum TCP peer IPv4 destination was not unicast.
    InvalidReticulumTcpPeerIpv4Address,
    /// A Reticulum TCP peer DNS hostname was malformed or exceeded its bound.
    InvalidReticulumTcpPeerHostname,
    /// A Reticulum TCP peer selected reserved port zero.
    InvalidReticulumTcpPeerPort,
    /// A phone-sourced RMAP latitude or longitude was outside world bounds.
    InvalidRmapLocation,
    /// An LXMF message location contained an out-of-range coordinate.
    InvalidLxmfMessageLocation,
    /// A requested LoRa transmit power was not one of the qualified values.
    InvalidLoraTransmitPowerDbm,
    /// A complete LoRa radio profile contained an invalid numeric field.
    InvalidLoraRadioProfile,
    /// A redacted desired-network snapshot violated ordering or revision rules.
    InvalidNetworkConfigSnapshot,
    /// A network mutation outcome carried contradictory state-specific fields.
    InvalidNetworkConfigMutationOutcome,
    /// A route page was sparse, unordered, or carried an invalid continuation.
    InvalidRouteDiagnosticsPage,
    /// A radio-trace page violated its sequence or pagination invariants.
    InvalidRadioTracePage,
    /// A radio-trace DATA event violated its physical-frame invariants.
    InvalidRadioTraceDataTx,
    /// A radio-trace route event omitted required attempt correlation.
    InvalidRadioTraceRouteSelected,
    /// A radio-trace terminal event carried partial proof-ingress signal data.
    InvalidRadioTraceAttemptTerminal,
    /// A radio-trace inbound-proof stage carried contradictory evidence.
    InvalidRadioTraceInboundProof,
    /// A DNS diagnostic contained contradictory state-specific fields.
    InvalidReticulumDnsDiagnostics,
    /// An LXMF summary contained a semantically impossible value combination.
    InvalidLxmfMessageSummary,
    /// An LXMF read response did not fit its declared complete message boundary.
    InvalidLxmfReadChunk,
    /// LXMF mailbox cursors or their encoded count contradicted one another.
    InvalidLxmfMailboxStatus,
    /// Envelope selected an incompatible protocol major version.
    UnsupportedVersion(ApiVersion),
    /// Request selected an unknown or unavailable operation.
    UnsupportedOperation(u16),
    /// Response selected an unknown response kind.
    UnsupportedResponseKind(u16),
    /// Submission state and state-specific fields contradict one another.
    InvalidSubmissionStatus,
    /// Known numeric enum field contained an unknown value.
    InvalidValue {
        /// Field being decoded.
        field: RequiredField,
        /// Unsupported numeric value.
        value: u64,
    },
}

/// Failure to encode a bounded logical API message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// Caller-provided output buffer cannot hold the canonical message.
    OutputTooSmall,
    /// Application payload exceeds its semantic limit.
    PayloadTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Encoded operation body exceeds [`MAX_BODY_BYTES`].
    BodyTooLarge {
        /// Required encoded body byte count.
        actual: usize,
        /// Accepted encoded body byte count.
        max: usize,
    },
    /// A basic LXMF title exceeded its individual semantic limit.
    LxmfBasicTitleTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Basic LXMF content exceeded its individual semantic limit.
    LxmfBasicContentTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Envelope selected an incompatible protocol major version.
    UnsupportedVersion(ApiVersion),
}

macro_rules! put {
    ($expression:expr) => {
        $expression.map_err(|_| EncodeError::OutputTooSmall)?
    };
}

/// Encode one request as canonical, definite-map CBOR into `output`.
///
/// The returned count never exceeds [`MAX_MESSAGE_BYTES`].
pub fn encode_request(
    envelope: &RequestEnvelope<'_>,
    output: &mut [u8],
) -> Result<usize, EncodeError> {
    check_encode_version(envelope.version)?;
    #[cfg(feature = "rns-data")]
    if let DeviceRequest::SubmitRnsData { payload, .. } = envelope.request
        && payload.len() > MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES
    {
        return Err(EncodeError::PayloadTooLarge {
            actual: payload.len(),
            max: MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES,
        });
    }
    #[cfg(feature = "lxmf")]
    if let DeviceRequest::LxmfBasicSend {
        timestamp_unix_ms,
        title,
        content,
        location,
        ..
    } = envelope.request
    {
        if title.len() > MAX_LXMF_BASIC_TITLE_BYTES {
            return Err(EncodeError::LxmfBasicTitleTooLarge {
                actual: title.len(),
                max: MAX_LXMF_BASIC_TITLE_BYTES,
            });
        }
        if content.len() > MAX_LXMF_BASIC_CONTENT_BYTES {
            return Err(EncodeError::LxmfBasicContentTooLarge {
                actual: content.len(),
                max: MAX_LXMF_BASIC_CONTENT_BYTES,
            });
        }
        let body_len =
            lxmf_basic_send_body_len(timestamp_unix_ms, title.len(), content.len(), location);
        if body_len > MAX_BODY_BYTES {
            return Err(EncodeError::BodyTooLarge {
                actual: body_len,
                max: MAX_BODY_BYTES,
            });
        }
    }

    let capacity = output.len().min(MAX_MESSAGE_BYTES);
    let mut encoder = Encoder::new(Cursor::new(&mut output[..capacity]));
    put!(encoder.map(4));
    put!(encoder.u8(0));
    encode_version(&mut encoder, envelope.version)?;
    put!(encoder.u8(1));
    put!(encoder.u64(envelope.request_id.0));
    put!(encoder.u8(2));
    put!(encoder.u16(envelope.request.operation()));
    put!(encoder.u8(3));
    match envelope.request {
        DeviceRequest::SystemCapabilities => {
            put!(encoder.map(0));
        }
        DeviceRequest::IdentitySummary => {
            put!(encoder.map(0));
        }
        DeviceRequest::SubmissionStatus { id } => {
            put!(encoder.map(1));
            put!(encoder.u8(0));
            put!(encoder.u64(id.0));
        }
        #[cfg(feature = "lxmf")]
        DeviceRequest::LxmfNext { after } => {
            put!(encoder.map(u64::from(after.is_some())));
            if let Some(after) = after {
                put!(encoder.u8(0));
                put!(encoder.u64(after.get()));
            }
        }
        #[cfg(feature = "lxmf")]
        DeviceRequest::LxmfRead {
            handle,
            offset,
            max_bytes,
        } => {
            put!(encoder.map(3));
            put!(encoder.u8(0));
            put!(encoder.u64(handle.get()));
            put!(encoder.u8(1));
            put!(encoder.u32(offset));
            put!(encoder.u8(2));
            put!(encoder.u16(max_bytes.get()));
        }
        #[cfg(feature = "lxmf")]
        DeviceRequest::LxmfMailboxStatus => {
            put!(encoder.map(0));
        }
        #[cfg(feature = "lxmf")]
        DeviceRequest::LxmfMailboxAcknowledge { through } => {
            put!(encoder.map(1));
            put!(encoder.u8(0));
            put!(encoder.u64(through.get()));
        }
        #[cfg(feature = "lxmf")]
        DeviceRequest::LxmfBasicSend {
            destination,
            timestamp_unix_ms,
            title,
            content,
            location,
            idempotency_key,
        } => {
            put!(encoder.map(5 + u64::from(location.is_some())));
            put!(encoder.u8(0));
            put!(encoder.bytes(&destination.0));
            put!(encoder.u8(1));
            put!(encoder.u64(timestamp_unix_ms));
            put!(encoder.u8(2));
            put!(encoder.bytes(title));
            put!(encoder.u8(3));
            put!(encoder.bytes(content));
            put!(encoder.u8(4));
            put!(encoder.bytes(&idempotency_key.0));
            if let Some(location) = location {
                put!(encoder.u8(5));
                encode_lxmf_message_location(&mut encoder, location)?;
            }
        }
        #[cfg(feature = "lxmf")]
        DeviceRequest::LxmfPeerNext { after } => {
            put!(encoder.map(if after.is_some() { 2 } else { 0 }));
            if let Some(after) = after {
                put!(encoder.u8(0));
                put!(encoder.bytes(after.incarnation().as_bytes()));
                put!(encoder.u8(1));
                put!(encoder.u64(after.after_generation()));
            }
        }
        #[cfg(feature = "nomad")]
        DeviceRequest::NomadFetchStart(request) => {
            put!(encoder.map(4));
            put!(encoder.u8(0));
            put!(encoder.bytes(&request.destination().0));
            put!(encoder.u8(1));
            put!(encoder.str(request.path().as_str()));
            put!(encoder.u8(2));
            put!(encoder.u64(request.timestamp_unix_ms().get()));
            put!(encoder.u8(3));
            put!(encoder.bytes(&request.idempotency_key().0));
        }
        #[cfg(feature = "nomad")]
        DeviceRequest::NomadFetchPoll(request) => {
            put!(encoder.map(1));
            put!(encoder.u8(0));
            put!(encoder.bytes(request.id.as_bytes()));
        }
        DeviceRequest::ReticulumProbeStart(request) => {
            put!(encoder.map(2));
            put!(encoder.u8(0));
            put!(encoder.bytes(&request.destination().0));
            put!(encoder.u8(1));
            put!(encoder.bytes(&request.idempotency_key().0));
        }
        DeviceRequest::ReticulumProbePoll(request) => {
            put!(encoder.map(1));
            put!(encoder.u8(0));
            put!(encoder.bytes(request.id().as_bytes()));
        }
        #[cfg(feature = "network-config")]
        DeviceRequest::NetworkConfigGet | DeviceRequest::NetworkStatus => {
            put!(encoder.map(0));
        }
        #[cfg(feature = "network-config")]
        DeviceRequest::NetworkConfigMutate(request) => {
            encode_network_config_mutation_request(&mut encoder, request)?;
        }
        DeviceRequest::NodeDiagnostics => {
            put!(encoder.map(0));
        }
        DeviceRequest::RouteDiagnosticsPage(request) => {
            put!(encoder.map(u64::from(request.after().is_some())));
            if let Some(after) = request.after() {
                put!(encoder.u8(0));
                put!(encoder.bytes(&after.0));
            }
        }
        DeviceRequest::RadioTracePage(request) => {
            put!(encoder.map(u64::from(request.after().is_some())));
            if let Some(after) = request.after() {
                put!(encoder.u8(0));
                encode_radio_trace_cursor(&mut encoder, after)?;
            }
        }
        DeviceRequest::ManualServiceAnnounce => {
            put!(encoder.map(0));
        }
        #[cfg(feature = "rns-data")]
        DeviceRequest::SubmitRnsData {
            destination,
            payload,
            idempotency_key,
        } => {
            put!(encoder.map(3));
            put!(encoder.u8(0));
            put!(encoder.bytes(&destination.0));
            put!(encoder.u8(1));
            put!(encoder.bytes(payload));
            put!(encoder.u8(2));
            put!(encoder.bytes(&idempotency_key.0));
        }
        DeviceRequest::__Borrowed(never, _) => match never {},
    }
    Ok(encoder.writer().position())
}

/// Decode exactly one request while borrowing any byte-string payload.
pub fn decode_request(input: &[u8]) -> Result<RequestEnvelope<'_>, DecodeError> {
    check_message_size(input)?;
    let mut decoder = Decoder::new(input);
    let entries = decode_map_len(&mut decoder)?;

    let mut version = None;
    let mut request_id = None;
    let mut operation = None;
    let mut body = None;

    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(version.is_some(), RequiredField::EnvelopeVersion)?;
                version = Some(decode_version(&mut decoder)?);
            }
            1 => {
                reject_duplicate(request_id.is_some(), RequiredField::EnvelopeRequestId)?;
                request_id = Some(RequestId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            2 => {
                reject_duplicate(operation.is_some(), RequiredField::EnvelopeKind)?;
                operation = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(body.is_some(), RequiredField::EnvelopeBody)?;
                body = Some(capture_body(input, &mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_item(&decoder, input)?;

    let version = require(version, RequiredField::EnvelopeVersion)?;
    check_version(version)?;
    let request_id = require(request_id, RequiredField::EnvelopeRequestId)?;
    let operation = require(operation, RequiredField::EnvelopeKind)?;
    let body = require(body, RequiredField::EnvelopeBody)?;
    let request = decode_request_body(operation, body)?;
    Ok(RequestEnvelope {
        version,
        request_id,
        request,
    })
}

/// Encode one response as canonical, definite-map CBOR into `output`.
pub fn encode_response(
    envelope: &ResponseEnvelope,
    output: &mut [u8],
) -> Result<usize, EncodeError> {
    check_encode_version(envelope.version)?;
    if let DeviceResponse::SystemCapabilities(capabilities) = envelope.response {
        check_encode_version(capabilities.api_version)?;
    }
    let capacity = output.len().min(MAX_MESSAGE_BYTES);
    let mut encoder = Encoder::new(Cursor::new(&mut output[..capacity]));
    put!(encoder.map(4));
    put!(encoder.u8(0));
    encode_version(&mut encoder, envelope.version)?;
    put!(encoder.u8(1));
    put!(encoder.u64(envelope.request_id.0));
    put!(encoder.u8(2));
    put!(encoder.u16(envelope.response.kind()));
    put!(encoder.u8(3));
    match envelope.response {
        DeviceResponse::SystemCapabilities(capabilities) => {
            encode_capabilities(&mut encoder, capabilities)?;
        }
        DeviceResponse::IdentitySummary(summary) => {
            encode_identity_summary(&mut encoder, summary)?;
        }
        DeviceResponse::SubmissionStatus(status) => {
            encode_submission_status(&mut encoder, status)?;
        }
        #[cfg(feature = "lxmf")]
        DeviceResponse::LxmfNext(summary) => {
            encode_lxmf_summary(&mut encoder, summary)?;
        }
        #[cfg(feature = "lxmf")]
        DeviceResponse::LxmfRead(chunk) => {
            encode_lxmf_read_chunk(&mut encoder, &chunk)?;
        }
        #[cfg(feature = "lxmf")]
        DeviceResponse::LxmfMailboxStatus(status)
        | DeviceResponse::LxmfMailboxAcknowledged(status) => {
            encode_lxmf_mailbox_status(&mut encoder, status)?;
        }
        #[cfg(feature = "lxmf")]
        DeviceResponse::LxmfBasicSendAccepted(accepted) => {
            encode_lxmf_basic_send_accepted(&mut encoder, accepted)?;
        }
        #[cfg(feature = "lxmf")]
        DeviceResponse::LxmfPeerNext(page) => {
            encode_lxmf_peer_discovery_page(&mut encoder, &page)?;
        }
        #[cfg(feature = "nomad")]
        DeviceResponse::NomadFetchStartAccepted(accepted) => {
            encode_nomad_fetch_start_accepted(&mut encoder, accepted)?;
        }
        #[cfg(feature = "nomad")]
        DeviceResponse::NomadFetchPoll(response) => {
            encode_nomad_fetch_poll(&mut encoder, &response)?;
        }
        #[cfg(feature = "network-config")]
        DeviceResponse::NetworkConfig(config) => {
            encode_network_config(&mut encoder, config)?;
        }
        #[cfg(feature = "network-config")]
        DeviceResponse::NetworkConfigMutation(outcome) => {
            encode_network_config_mutation_outcome(&mut encoder, outcome)?;
        }
        #[cfg(feature = "network-config")]
        DeviceResponse::NetworkStatus(status) => {
            encode_network_status(&mut encoder, status)?;
        }
        DeviceResponse::NodeDiagnostics(snapshot) => {
            encode_node_diagnostics(&mut encoder, &snapshot)?;
        }
        DeviceResponse::RouteDiagnosticsPage(page) => {
            encode_route_diagnostics_page(&mut encoder, &page)?;
        }
        DeviceResponse::RadioTracePage(page) => {
            encode_radio_trace_page(&mut encoder, &page)?;
        }
        DeviceResponse::ManualServiceAnnounce(disposition) => {
            put!(encoder.map(1));
            put!(encoder.u8(0));
            put!(encoder.u8(disposition.wire_code()));
        }
        DeviceResponse::ReticulumProbeStartAccepted(accepted) => {
            encode_probe_start_accepted(&mut encoder, accepted)?;
        }
        DeviceResponse::ReticulumProbePoll(response) => {
            encode_probe_poll(&mut encoder, response)?;
        }
        #[cfg(feature = "rns-data")]
        DeviceResponse::SubmitRnsDataAccepted(accepted) => {
            encode_submission_accepted(&mut encoder, accepted)?;
        }
        DeviceResponse::Error(error) => encode_error(&mut encoder, error)?,
    }
    Ok(encoder.writer().position())
}

/// Decode exactly one response and reject duplicate known fields.
pub fn decode_response(input: &[u8]) -> Result<ResponseEnvelope, DecodeError> {
    check_message_size(input)?;
    let mut decoder = Decoder::new(input);
    let entries = decode_map_len(&mut decoder)?;

    let mut version = None;
    let mut request_id = None;
    let mut kind = None;
    let mut body = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(version.is_some(), RequiredField::EnvelopeVersion)?;
                version = Some(decode_version(&mut decoder)?);
            }
            1 => {
                reject_duplicate(request_id.is_some(), RequiredField::EnvelopeRequestId)?;
                request_id = Some(RequestId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            2 => {
                reject_duplicate(kind.is_some(), RequiredField::EnvelopeKind)?;
                kind = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(body.is_some(), RequiredField::EnvelopeBody)?;
                body = Some(capture_body(input, &mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_item(&decoder, input)?;

    let version = require(version, RequiredField::EnvelopeVersion)?;
    check_version(version)?;
    let request_id = require(request_id, RequiredField::EnvelopeRequestId)?;
    let kind = require(kind, RequiredField::EnvelopeKind)?;
    let body = require(body, RequiredField::EnvelopeBody)?;
    let response = match kind {
        OP_SYSTEM_CAPABILITIES => DeviceResponse::SystemCapabilities(decode_capabilities(body)?),
        OP_IDENTITY_SUMMARY => DeviceResponse::IdentitySummary(decode_identity_summary(body)?),
        OP_SUBMISSION_STATUS => DeviceResponse::SubmissionStatus(decode_submission_status(body)?),
        #[cfg(feature = "lxmf")]
        OP_LXMF_NEXT => DeviceResponse::LxmfNext(decode_lxmf_summary(body)?),
        #[cfg(feature = "lxmf")]
        OP_LXMF_READ => DeviceResponse::LxmfRead(decode_lxmf_read_chunk(body)?),
        #[cfg(feature = "lxmf")]
        OP_LXMF_MAILBOX_STATUS => {
            DeviceResponse::LxmfMailboxStatus(decode_lxmf_mailbox_status(body)?)
        }
        #[cfg(feature = "lxmf")]
        OP_LXMF_MAILBOX_ACKNOWLEDGE => {
            DeviceResponse::LxmfMailboxAcknowledged(decode_lxmf_mailbox_status(body)?)
        }
        #[cfg(feature = "lxmf")]
        OP_LXMF_BASIC_SEND => {
            DeviceResponse::LxmfBasicSendAccepted(decode_lxmf_basic_send_accepted(body)?)
        }
        #[cfg(feature = "lxmf")]
        OP_LXMF_PEER_NEXT => DeviceResponse::LxmfPeerNext(decode_lxmf_peer_discovery_page(body)?),
        #[cfg(feature = "nomad")]
        OP_NOMAD_FETCH_START => {
            DeviceResponse::NomadFetchStartAccepted(decode_nomad_fetch_start_accepted(body)?)
        }
        #[cfg(feature = "nomad")]
        OP_NOMAD_FETCH_POLL => DeviceResponse::NomadFetchPoll(decode_nomad_fetch_poll(body)?),
        #[cfg(feature = "network-config")]
        OP_NETWORK_CONFIG_GET => DeviceResponse::NetworkConfig(decode_network_config(body)?),
        #[cfg(feature = "network-config")]
        OP_NETWORK_CONFIG_MUTATE => {
            DeviceResponse::NetworkConfigMutation(decode_network_config_mutation_outcome(body)?)
        }
        #[cfg(feature = "network-config")]
        OP_NETWORK_STATUS => DeviceResponse::NetworkStatus(decode_network_status(body)?),
        OP_NODE_DIAGNOSTICS => DeviceResponse::NodeDiagnostics(decode_node_diagnostics(body)?),
        OP_ROUTE_DIAGNOSTICS_PAGE => {
            DeviceResponse::RouteDiagnosticsPage(decode_route_diagnostics_page(body)?)
        }
        OP_RADIO_TRACE_PAGE => {
            DeviceResponse::RadioTracePage(decode_radio_trace_page_compact(body)?)
        }
        OP_MANUAL_SERVICE_ANNOUNCE => {
            DeviceResponse::ManualServiceAnnounce(decode_manual_service_announce_disposition(body)?)
        }
        OP_RETICULUM_PROBE_START => {
            DeviceResponse::ReticulumProbeStartAccepted(decode_probe_start_accepted(body)?)
        }
        OP_RETICULUM_PROBE_POLL => DeviceResponse::ReticulumProbePoll(decode_probe_poll(body)?),
        #[cfg(feature = "rns-data")]
        OP_SUBMIT_RNS_DATA => {
            DeviceResponse::SubmitRnsDataAccepted(decode_submission_accepted(body)?)
        }
        RESPONSE_ERROR => DeviceResponse::Error(decode_error(body)?),
        other => return Err(DecodeError::UnsupportedResponseKind(other)),
    };
    Ok(ResponseEnvelope {
        version,
        request_id,
        response,
    })
}

type SliceEncoder<'a> = Encoder<Cursor<&'a mut [u8]>>;

#[cfg(feature = "lxmf")]
const fn cbor_u64_len(value: u64) -> usize {
    match value {
        0..=23 => 1,
        24..=0xff => 2,
        0x100..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

#[cfg(feature = "lxmf")]
const fn cbor_i32_len(value: i32) -> usize {
    if value >= 0 {
        cbor_u64_len(value as u64)
    } else {
        cbor_u64_len((-1_i64 - value as i64) as u64)
    }
}

#[cfg(feature = "lxmf")]
const fn cbor_bytes_len(length: usize) -> usize {
    let header = match length {
        0..=23 => 1,
        24..=0xff => 2,
        0x100..=0xffff => 3,
        _ => 5,
    };
    header + length
}

#[cfg(feature = "lxmf")]
const fn lxmf_message_location_len(location: LxmfMessageLocation) -> usize {
    // Seven-entry map header + seven one-byte integer keys + canonical values.
    1 + 7
        + cbor_i32_len(location.latitude_e6())
        + cbor_i32_len(location.longitude_e6())
        + cbor_i32_len(location.altitude_cm())
        + cbor_u64_len(location.speed_cm_per_second() as u64)
        + cbor_i32_len(location.bearing_centidegrees())
        + cbor_u64_len(location.accuracy_cm() as u64)
        + cbor_u64_len(location.updated_at_unix_seconds() as u64)
}

#[cfg(feature = "lxmf")]
const fn lxmf_basic_send_body_len(
    timestamp_unix_ms: u64,
    title: usize,
    content: usize,
    location: Option<LxmfMessageLocation>,
) -> usize {
    // map header + five single-byte keys + two fixed 16-byte strings
    1 + 5
        + 17
        + cbor_u64_len(timestamp_unix_ms)
        + cbor_bytes_len(title)
        + cbor_bytes_len(content)
        + 17
        + match location {
            Some(location) => 1 + lxmf_message_location_len(location),
            None => 0,
        }
}

#[cfg(feature = "lxmf")]
fn encode_lxmf_message_location(
    encoder: &mut SliceEncoder<'_>,
    location: LxmfMessageLocation,
) -> Result<(), EncodeError> {
    put!(encoder.map(7));
    put!(encoder.u8(0));
    put!(encoder.i32(location.latitude_e6()));
    put!(encoder.u8(1));
    put!(encoder.i32(location.longitude_e6()));
    put!(encoder.u8(2));
    put!(encoder.i32(location.altitude_cm()));
    put!(encoder.u8(3));
    put!(encoder.u32(location.speed_cm_per_second()));
    put!(encoder.u8(4));
    put!(encoder.i32(location.bearing_centidegrees()));
    put!(encoder.u8(5));
    put!(encoder.u16(location.accuracy_cm()));
    put!(encoder.u8(6));
    put!(encoder.u32(location.updated_at_unix_seconds()));
    Ok(())
}

fn encode_version(encoder: &mut SliceEncoder<'_>, version: ApiVersion) -> Result<(), EncodeError> {
    check_encode_version(version)?;
    put!(encoder.map(2));
    put!(encoder.u8(0));
    put!(encoder.u16(version.major));
    put!(encoder.u8(1));
    put!(encoder.u16(version.minor));
    Ok(())
}

fn encode_capabilities(
    encoder: &mut SliceEncoder<'_>,
    capabilities: CapabilitySnapshot,
) -> Result<(), EncodeError> {
    put!(encoder.map(20));
    put!(encoder.u8(0));
    encode_version(encoder, capabilities.api_version)?;
    put!(encoder.u8(1));
    put!(encoder.bool(capabilities.packet_output));
    put!(encoder.u8(2));
    put!(encoder.u8(capabilities.direct_radio_tx.wire_code()));
    put!(encoder.u8(3));
    put!(encoder.bool(capabilities.submit_rns_data));
    put!(encoder.u8(4));
    put!(encoder.u16(capabilities.max_message_bytes));
    put!(encoder.u8(5));
    put!(encoder.u16(capabilities.max_body_bytes));
    put!(encoder.u8(6));
    put!(encoder.u16(capabilities.max_submit_rns_data_payload_bytes));
    put!(encoder.u8(9));
    put!(encoder.u8(capabilities.lxmf.wire_code()));
    put!(encoder.u8(10));
    put!(encoder.u16(capabilities.max_lxmf_read_chunk_bytes));
    put!(encoder.u8(11));
    put!(encoder.u8(capabilities.lxmf_basic_send.wire_code()));
    put!(encoder.u8(12));
    put!(encoder.u16(capabilities.max_lxmf_basic_title_bytes));
    put!(encoder.u8(13));
    put!(encoder.u16(capabilities.max_lxmf_basic_content_bytes));
    put!(encoder.u8(14));
    put!(encoder.u8(capabilities.lxmf_peer_discovery.wire_code(),));
    put!(encoder.u8(15));
    put!(encoder.u16(capabilities.max_lxmf_peer_app_data_bytes));
    put!(encoder.u8(16));
    put!(encoder.u8(capabilities.nomad.wire_code()));
    put!(encoder.u8(17));
    put!(encoder.u16(capabilities.max_nomad_page_path_bytes));
    put!(encoder.u8(18));
    put!(encoder.u16(capabilities.max_nomad_page_bytes));
    put!(encoder.u8(19));
    put!(encoder.u8(capabilities.network_config.wire_code()));
    put!(encoder.u8(20));
    put!(encoder.u8(capabilities.manual_service_announce.wire_code()));
    put!(encoder.u8(21));
    put!(encoder.u8(capabilities.reticulum_probe.wire_code()));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_network_config_mutation_request(
    encoder: &mut SliceEncoder<'_>,
    request: NetworkConfigMutationRequest<'_>,
) -> Result<(), EncodeError> {
    put!(encoder.map(4));
    put!(encoder.u8(0));
    match request.mutation() {
        NetworkConfigMutation::UpsertWifi { .. } => {
            put!(encoder.u8(0));
        }
        NetworkConfigMutation::RemoveWifi { .. } => {
            put!(encoder.u8(1));
        }
        NetworkConfigMutation::ReplaceTcpPeer(_) => {
            put!(encoder.u8(2));
        }
        NetworkConfigMutation::ReplaceTcpHostPeer(_) => {
            put!(encoder.u8(3));
        }
        NetworkConfigMutation::SetGatewayPolicy(_) => {
            put!(encoder.u8(4));
        }
        NetworkConfigMutation::SetRmapConfig(_) => {
            put!(encoder.u8(5));
        }
        NetworkConfigMutation::SetLoraTxPower(_) => {
            put!(encoder.u8(6));
        }
        NetworkConfigMutation::SetLoraProfile(_) => {
            put!(encoder.u8(7));
        }
    }
    put!(encoder.u8(1));
    match request.mutation() {
        NetworkConfigMutation::UpsertWifi {
            profile_id,
            network,
        } => {
            put!(encoder.map(2));
            put!(encoder.u8(0));
            put!(encoder.bytes(profile_id.as_bytes()));
            put!(encoder.u8(1));
            encode_wifi_network_update(encoder, network)?;
        }
        NetworkConfigMutation::RemoveWifi { profile_id } => {
            put!(encoder.map(1));
            put!(encoder.u8(0));
            put!(encoder.bytes(profile_id.as_bytes()));
        }
        NetworkConfigMutation::ReplaceTcpPeer(peer) => match peer {
            Some(peer) => encode_tcp_peer_update(encoder, peer)?,
            None => {
                put!(encoder.null());
            }
        },
        NetworkConfigMutation::ReplaceTcpHostPeer(peer) => match peer {
            Some(peer) => encode_tcp_host_peer_update(encoder, peer)?,
            None => {
                put!(encoder.null());
            }
        },
        NetworkConfigMutation::SetGatewayPolicy(policy) => {
            encode_gateway_policy(encoder, policy)?;
        }
        NetworkConfigMutation::SetRmapConfig(config) => {
            encode_rmap_config(encoder, config)?;
        }
        NetworkConfigMutation::SetLoraTxPower(power) => {
            put!(encoder.u8(power.get()));
        }
        NetworkConfigMutation::SetLoraProfile(profile) => {
            encode_lora_radio_profile(encoder, profile)?;
        }
    }
    put!(encoder.u8(2));
    put!(encoder.u64(request.expected_revision()));
    put!(encoder.u8(3));
    put!(encoder.bytes(&request.idempotency_key().0));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_wifi_network_summary(
    encoder: &mut SliceEncoder<'_>,
    wifi: WifiNetworkConfigSummary,
) -> Result<(), EncodeError> {
    put!(encoder.map(5));
    put!(encoder.u8(0));
    put!(encoder.bytes(wifi.profile_id().as_bytes()));
    put!(encoder.u8(1));
    put!(encoder.bool(wifi.enabled()));
    put!(encoder.u8(2));
    put!(encoder.bytes(wifi.ssid().as_bytes()));
    put!(encoder.u8(3));
    put!(encoder.bool(wifi.credential_configured()));
    put!(encoder.u8(4));
    put!(encoder.u8(wifi.priority()));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_tcp_peer_summary(
    encoder: &mut SliceEncoder<'_>,
    peer: ReticulumTcpPeerConfigSummary,
) -> Result<(), EncodeError> {
    put!(encoder.map(3));
    put!(encoder.u8(0));
    put!(encoder.bool(peer.enabled()));
    put!(encoder.u8(1));
    put!(encoder.bytes(&peer.ipv4_address().octets()));
    put!(encoder.u8(2));
    put!(encoder.u16(peer.port()));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_tcp_host_peer_summary(
    encoder: &mut SliceEncoder<'_>,
    peer: ReticulumTcpPeerHostConfigSummary,
) -> Result<(), EncodeError> {
    put!(encoder.map(3));
    put!(encoder.u8(0));
    put!(encoder.bool(peer.enabled()));
    put!(encoder.u8(1));
    put!(encoder.str(peer.hostname().as_str()));
    put!(encoder.u8(2));
    put!(encoder.u16(peer.port()));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_wifi_network_update(
    encoder: &mut SliceEncoder<'_>,
    wifi: WifiNetworkUpdate<'_>,
) -> Result<(), EncodeError> {
    put!(encoder.map(4));
    put!(encoder.u8(0));
    put!(encoder.bool(wifi.enabled()));
    put!(encoder.u8(1));
    put!(encoder.bytes(wifi.ssid().as_bytes()));
    put!(encoder.u8(2));
    let credential = wifi.credential();
    put!(encoder.map(if credential.replacement().is_some() {
        2
    } else {
        1
    }));
    put!(encoder.u8(0));
    put!(encoder.u8(credential.wire_code()));
    if let Some(passphrase) = credential.replacement() {
        put!(encoder.u8(1));
        put!(encoder.bytes(passphrase));
    }
    put!(encoder.u8(3));
    put!(encoder.u8(wifi.priority()));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_tcp_peer_update(
    encoder: &mut SliceEncoder<'_>,
    peer: ReticulumTcpPeerUpdate,
) -> Result<(), EncodeError> {
    put!(encoder.map(3));
    put!(encoder.u8(0));
    put!(encoder.bool(peer.enabled()));
    put!(encoder.u8(1));
    put!(encoder.bytes(&peer.ipv4_address().octets()));
    put!(encoder.u8(2));
    put!(encoder.u16(peer.port()));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_tcp_host_peer_update(
    encoder: &mut SliceEncoder<'_>,
    peer: ReticulumTcpPeerHostUpdate<'_>,
) -> Result<(), EncodeError> {
    put!(encoder.map(3));
    put!(encoder.u8(0));
    put!(encoder.bool(peer.enabled()));
    put!(encoder.u8(1));
    put!(encoder.str(peer.hostname().as_str()));
    put!(encoder.u8(2));
    put!(encoder.u16(peer.port()));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_gateway_policy(
    encoder: &mut SliceEncoder<'_>,
    policy: GatewayPolicy,
) -> Result<(), EncodeError> {
    put!(encoder.map(2));
    put!(encoder.u8(0));
    put!(encoder.bool(policy.wifi_transport_enabled()));
    put!(encoder.u8(1));
    put!(encoder.bool(policy.automatic_announces_enabled()));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_rmap_location(
    encoder: &mut SliceEncoder<'_>,
    location: RmapLocation,
) -> Result<(), EncodeError> {
    put!(encoder.map(2));
    put!(encoder.u8(0));
    put!(encoder.i32(location.latitude_e6()));
    put!(encoder.u8(1));
    put!(encoder.i32(location.longitude_e6()));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_rmap_config(
    encoder: &mut SliceEncoder<'_>,
    config: RmapConfig,
) -> Result<(), EncodeError> {
    put!(encoder.map(3));
    put!(encoder.u8(0));
    put!(encoder.bool(config.discovery_enabled()));
    put!(encoder.u8(1));
    put!(encoder.bool(config.share_location()));
    put!(encoder.u8(2));
    match config.phone_location() {
        Some(location) => encode_rmap_location(encoder, location)?,
        None => {
            put!(encoder.null());
        }
    }
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_lora_radio_profile(
    encoder: &mut SliceEncoder<'_>,
    profile: LoraRadioProfile,
) -> Result<(), EncodeError> {
    put!(encoder.map(5));
    put!(encoder.u8(0));
    put!(encoder.u32(profile.frequency_hz()));
    put!(encoder.u8(1));
    put!(encoder.u32(profile.bandwidth_hz()));
    put!(encoder.u8(2));
    put!(encoder.u8(profile.spreading_factor()));
    put!(encoder.u8(3));
    put!(encoder.u8(profile.coding_rate_denominator()));
    put!(encoder.u8(4));
    put!(encoder.u8(profile.tx_power_dbm().get()));
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_network_config(
    encoder: &mut SliceEncoder<'_>,
    config: NetworkConfigSnapshot,
) -> Result<(), EncodeError> {
    put!(encoder.map(10));
    put!(encoder.u8(0));
    put!(encoder.u64(config.revision));
    put!(encoder.u8(1));
    let profile_count = config
        .wifi_profiles()
        .iter()
        .filter(|profile| profile.is_some())
        .count();
    put!(encoder.array(profile_count as u64));
    for profile in config.wifi_profiles().iter().copied().flatten() {
        encode_wifi_network_summary(encoder, profile)?;
    }
    put!(encoder.u8(2));
    match config.tcp_peer() {
        Some(peer) => encode_tcp_peer_summary(encoder, peer)?,
        None => {
            put!(encoder.null());
        }
    }
    put!(encoder.u8(3));
    put!(encoder.bool(config.wifi_transport_enabled()));
    put!(encoder.u8(4));
    put!(encoder.bool(config.automatic_announces_enabled()));
    put!(encoder.u8(5));
    put!(encoder.bool(config.rmap_discovery_enabled()));
    put!(encoder.u8(6));
    put!(encoder.bool(config.rmap_share_location()));
    put!(encoder.u8(7));
    match config.rmap_phone_location() {
        Some(location) => encode_rmap_location(encoder, location)?,
        None => {
            put!(encoder.null());
        }
    }
    put!(encoder.u8(8));
    match config.tcp_host_peer() {
        Some(peer) => encode_tcp_host_peer_summary(encoder, peer)?,
        None => {
            put!(encoder.null());
        }
    }
    put!(encoder.u8(9));
    encode_lora_radio_profile(encoder, config.lora_profile())?;
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_network_config_mutation_outcome(
    encoder: &mut SliceEncoder<'_>,
    outcome: NetworkConfigMutationOutcome,
) -> Result<(), EncodeError> {
    match outcome {
        NetworkConfigMutationOutcome::Applied {
            revision,
            reboot_required,
        } => {
            put!(encoder.map(3));
            put!(encoder.u8(0));
            put!(encoder.u8(0));
            put!(encoder.u8(1));
            put!(encoder.u64(revision));
            put!(encoder.u8(2));
            put!(encoder.bool(reboot_required));
        }
        NetworkConfigMutationOutcome::RevisionConflict { current_revision } => {
            put!(encoder.map(2));
            put!(encoder.u8(0));
            put!(encoder.u8(1));
            put!(encoder.u8(1));
            put!(encoder.u64(current_revision));
        }
    }
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_network_status(
    encoder: &mut SliceEncoder<'_>,
    status: NetworkRuntimeStatus,
) -> Result<(), EncodeError> {
    put!(encoder.map(
        8 + u64::from(status.last_tcp_failure.is_some())
            + u64::from(status.dns_diagnostics.is_some())
            + u64::from(status.rmap_status.is_some())
    ));
    put!(encoder.u8(0));
    put!(encoder.u64(status.configured_revision));
    put!(encoder.u8(1));
    put!(encoder.u64(status.applied_revision));
    put!(encoder.u8(2));
    put!(encoder.u8(status.wifi_state.wire_code()));
    put!(encoder.u8(3));
    match status.active_wifi_profile {
        Some(profile_id) => {
            put!(encoder.bytes(profile_id.as_bytes()));
        }
        None => {
            put!(encoder.null());
        }
    }
    put!(encoder.u8(4));
    match status.connected_ssid() {
        Some(ssid) => {
            put!(encoder.bytes(ssid.as_bytes()));
        }
        None => {
            put!(encoder.null());
        }
    }
    put!(encoder.u8(5));
    match status.ipv4_address {
        Some(address) => {
            put!(encoder.bytes(&address));
        }
        None => {
            put!(encoder.null());
        }
    }
    put!(encoder.u8(6));
    match status.rssi_dbm {
        Some(rssi) => {
            put!(encoder.i16(rssi));
        }
        None => {
            put!(encoder.null());
        }
    }
    put!(encoder.u8(7));
    put!(encoder.u8(status.tcp_peer_state.wire_code()));
    if let Some(failure) = status.last_tcp_failure {
        put!(encoder.u8(8));
        put!(encoder.u8(failure.wire_code()));
    }
    if let Some(diagnostics) = status.dns_diagnostics {
        put!(encoder.u8(9));
        encode_reticulum_dns_diagnostics(encoder, diagnostics)?;
    }
    if let Some(rmap) = status.rmap_status {
        put!(encoder.u8(10));
        encode_rmap_runtime_status(encoder, rmap)?;
    }
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_rmap_runtime_status(
    encoder: &mut SliceEncoder<'_>,
    status: RmapRuntimeStatus,
) -> Result<(), EncodeError> {
    put!(encoder.array(10));
    put!(encoder.bool(status.config_applied));
    put!(encoder.u8(status.stamp_phase.wire_code()));
    put!(encoder.u64(status.stamp_attempts));
    put!(encoder.u8(status.initial_tcp_gate.wire_code()));
    put!(encoder.u32(status.queued_count));
    put!(encoder.u8(status.last_queue_outcome.wire_code()));
    match status.last_queue_attempt_at_uptime_seconds {
        Some(at) => {
            put!(encoder.u64(at));
        }
        None => {
            put!(encoder.null());
        }
    }
    put!(encoder.u8(status.egress_confirmation.wire_code()));
    match status.next_due_in_seconds {
        Some(delay) => {
            put!(encoder.u64(delay));
        }
        None => {
            put!(encoder.null());
        }
    }
    match status.deferred_reason {
        Some(reason) => {
            put!(encoder.u8(reason.wire_code()));
        }
        None => {
            put!(encoder.null());
        }
    }
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_reticulum_dns_diagnostics(
    encoder: &mut SliceEncoder<'_>,
    diagnostics: ReticulumDnsDiagnostics,
) -> Result<(), EncodeError> {
    put!(encoder.map(6));
    put!(encoder.u8(0));
    encode_optional_ipv4(encoder, diagnostics.gateway_ipv4)?;
    put!(encoder.u8(1));
    put!(encoder.array(MAX_RETICULUM_DNS_DHCP_SERVERS as u64));
    for server in diagnostics.dhcp_servers {
        encode_optional_ipv4(encoder, server)?;
    }
    put!(encoder.u8(2));
    put!(encoder.u8(diagnostics.primary_outcome.wire_code()));
    put!(encoder.u8(3));
    put!(encoder.u8(diagnostics.raw_setup_state.wire_code()));
    put!(encoder.u8(4));
    put!(encoder.array(MAX_RETICULUM_DNS_RAW_ATTEMPTS as u64));
    for attempt in diagnostics.raw_attempts {
        match attempt {
            Some(attempt) => encode_reticulum_dns_raw_attempt(encoder, attempt)?,
            None => {
                put!(encoder.null());
            }
        }
    }
    put!(encoder.u8(5));
    match diagnostics.resolution {
        Some(resolution) => encode_reticulum_dns_resolution(encoder, resolution)?,
        None => {
            put!(encoder.null());
        }
    }
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_reticulum_dns_raw_attempt(
    encoder: &mut SliceEncoder<'_>,
    attempt: ReticulumDnsRawAttempt,
) -> Result<(), EncodeError> {
    put!(encoder.map(3 + u64::from(attempt.outcome.response_code().is_some())));
    put!(encoder.u8(0));
    put!(encoder.u8(attempt.source.wire_code()));
    put!(encoder.u8(1));
    put!(encoder.bytes(&attempt.server));
    put!(encoder.u8(2));
    put!(encoder.u8(attempt.outcome.wire_code()));
    if let Some(response_code) = attempt.outcome.response_code() {
        put!(encoder.u8(3));
        put!(encoder.u8(response_code));
    }
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_reticulum_dns_resolution(
    encoder: &mut SliceEncoder<'_>,
    resolution: ReticulumDnsResolution,
) -> Result<(), EncodeError> {
    put!(encoder.map(3));
    put!(encoder.u8(0));
    put!(encoder.bytes(&resolution.address));
    put!(encoder.u8(1));
    put!(encoder.u8(resolution.source.wire_code()));
    put!(encoder.u8(2));
    encode_optional_ipv4(encoder, resolution.resolver)?;
    Ok(())
}

#[cfg(feature = "network-config")]
fn encode_optional_ipv4(
    encoder: &mut SliceEncoder<'_>,
    address: Option<[u8; 4]>,
) -> Result<(), EncodeError> {
    match address {
        Some(address) => {
            put!(encoder.bytes(&address));
        }
        None => {
            put!(encoder.null());
        }
    }
    Ok(())
}

fn encode_identity_summary(
    encoder: &mut SliceEncoder<'_>,
    summary: IdentitySummary,
) -> Result<(), EncodeError> {
    put!(encoder.map(1 + u64::from(summary.lxmf_delivery_destination().is_some())));
    put!(encoder.u8(0));
    put!(encoder.bytes(&summary.primary_destination().0));
    if let Some(destination) = summary.lxmf_delivery_destination() {
        put!(encoder.u8(1));
        put!(encoder.bytes(&destination.0));
    }
    Ok(())
}

fn encode_ingress_observation(
    encoder: &mut SliceEncoder<'_>,
    ingress: IngressObservation,
) -> Result<(), EncodeError> {
    put!(encoder.map(1 + 2 * u64::from(ingress.signal().is_some())));
    put!(encoder.u8(0));
    put!(encoder.u8(ingress.interface_id()));
    if let Some(signal) = ingress.signal() {
        put!(encoder.u8(1));
        put!(encoder.i16(signal.rssi_dbm()));
        put!(encoder.u8(2));
        put!(encoder.i16(signal.snr_db()));
    }
    Ok(())
}

fn encode_probe_start_accepted(
    encoder: &mut SliceEncoder<'_>,
    accepted: ProbeStartAccepted,
) -> Result<(), EncodeError> {
    put!(encoder.map(2));
    put!(encoder.u8(0));
    put!(encoder.bytes(accepted.id().as_bytes()));
    put!(encoder.u8(1));
    put!(encoder.u8(accepted.outcome().wire_code()));
    Ok(())
}

fn encode_probe_poll(
    encoder: &mut SliceEncoder<'_>,
    response: ProbePollResponse,
) -> Result<(), EncodeError> {
    put!(encoder.map(2));
    put!(encoder.u8(0));
    match response {
        ProbePollResponse::Pending(_) => {
            put!(encoder.u8(0));
        }
        ProbePollResponse::Succeeded(_) => {
            put!(encoder.u8(1));
        }
        ProbePollResponse::Failed(_) => {
            put!(encoder.u8(2));
        }
    }
    put!(encoder.u8(1));
    match response {
        ProbePollResponse::Pending(phase) => {
            put!(encoder.u8(phase.wire_code()));
        }
        ProbePollResponse::Succeeded(success) => {
            put!(encoder.map(3));
            put!(encoder.u8(0));
            put!(encoder.u32(success.round_trip_ms()));
            put!(encoder.u8(1));
            put!(encoder.u8(success.hops()));
            put!(encoder.u8(2));
            encode_ingress_observation(encoder, success.ingress_observation())?;
        }
        ProbePollResponse::Failed(failure) => {
            put!(encoder.u8(failure.wire_code()));
        }
    }
    Ok(())
}

#[cfg(feature = "lxmf")]
fn encode_lxmf_summary(
    encoder: &mut SliceEncoder<'_>,
    summary: LxmfMessageSummary,
) -> Result<(), EncodeError> {
    put!(encoder.map(10 + u64::from(summary.ingress_observation().is_some())));
    put!(encoder.u8(0));
    put!(encoder.u64(summary.handle().get()));
    put!(encoder.u8(1));
    put!(encoder.bytes(summary.message_id()));
    put!(encoder.u8(2));
    put!(encoder.bytes(&summary.destination().0));
    put!(encoder.u8(3));
    put!(encoder.bytes(&summary.source().0));
    put!(encoder.u8(4));
    put!(encoder.u64(summary.timestamp_bits()));
    put!(encoder.u8(5));
    put!(encoder.u32(summary.normalized_wire_len()));
    put!(encoder.u8(6));
    put!(encoder.u32(summary.title_len()));
    put!(encoder.u8(7));
    put!(encoder.u32(summary.content_len()));
    put!(encoder.u8(8));
    put!(encoder.u32(summary.fields_encoded_len()));
    put!(encoder.u8(9));
    put!(encoder.bytes(summary.exact_wire_sha256()));
    if let Some(ingress) = summary.ingress_observation() {
        put!(encoder.u8(10));
        encode_ingress_observation(encoder, ingress)?;
    }
    Ok(())
}

#[cfg(feature = "lxmf")]
fn encode_lxmf_read_chunk(
    encoder: &mut SliceEncoder<'_>,
    chunk: &LxmfReadChunk,
) -> Result<(), EncodeError> {
    put!(encoder.map(4));
    put!(encoder.u8(0));
    put!(encoder.u64(chunk.handle().get()));
    put!(encoder.u8(1));
    put!(encoder.u32(chunk.offset()));
    put!(encoder.u8(2));
    put!(encoder.u32(chunk.total_len()));
    put!(encoder.u8(3));
    put!(encoder.bytes(chunk.bytes()));
    Ok(())
}

#[cfg(feature = "lxmf")]
fn encode_lxmf_mailbox_status(
    encoder: &mut SliceEncoder<'_>,
    status: LxmfMailboxStatus,
) -> Result<(), EncodeError> {
    put!(encoder.map(
        1 + u64::from(status.latest().is_some())
            + u64::from(status.acknowledged_through().is_some())
    ));
    if let Some(latest) = status.latest() {
        put!(encoder.u8(0));
        put!(encoder.u64(latest.get()));
    }
    if let Some(acknowledged) = status.acknowledged_through() {
        put!(encoder.u8(1));
        put!(encoder.u64(acknowledged.get()));
    }
    put!(encoder.u8(2));
    put!(encoder.u32(status.uncollected_count()));
    Ok(())
}

#[cfg(feature = "lxmf")]
fn encode_lxmf_basic_send_accepted(
    encoder: &mut SliceEncoder<'_>,
    accepted: LxmfBasicSendAccepted,
) -> Result<(), EncodeError> {
    put!(encoder.map(2));
    put!(encoder.u8(0));
    put!(encoder.u64(accepted.id.0));
    put!(encoder.u8(1));
    put!(encoder.bytes(accepted.message_id()));
    Ok(())
}

#[cfg(feature = "lxmf")]
fn encode_lxmf_peer_discovery_page(
    encoder: &mut SliceEncoder<'_>,
    page: &LxmfPeerDiscoveryPage,
) -> Result<(), EncodeError> {
    let entries = 3
        + u64::from(page.latest_generation().is_some())
        + u64::from(page.oldest_retained_generation().is_some())
        + u64::from(page.peer().is_some());
    put!(encoder.map(entries));
    put!(encoder.u8(0));
    put!(encoder.bytes(page.next_cursor().incarnation().as_bytes(),));
    put!(encoder.u8(1));
    put!(encoder.u64(page.next_cursor().after_generation()));
    if let Some(latest) = page.latest_generation() {
        put!(encoder.u8(2));
        put!(encoder.u64(latest.get()));
    }
    if let Some(oldest) = page.oldest_retained_generation() {
        put!(encoder.u8(3));
        put!(encoder.u64(oldest.get()));
    }
    put!(encoder.u8(4));
    put!(encoder.bool(page.history_gap()));
    if let Some(peer) = page.peer() {
        put!(encoder.u8(5));
        encode_lxmf_discovered_peer(encoder, peer)?;
    }
    Ok(())
}

#[cfg(feature = "lxmf")]
fn encode_lxmf_discovered_peer(
    encoder: &mut SliceEncoder<'_>,
    peer: &LxmfDiscoveredPeer,
) -> Result<(), EncodeError> {
    let entries = 7 + u64::from(peer.rssi_dbm().is_some()) + u64::from(peer.snr_db().is_some());
    put!(encoder.map(entries));
    put!(encoder.u8(0));
    put!(encoder.bytes(&peer.destination().0));
    put!(encoder.u8(1));
    put!(encoder.bytes(peer.identity_hash().as_bytes()));
    put!(encoder.u8(2));
    put!(encoder.bytes(peer.app_data()));
    put!(encoder.u8(3));
    put!(encoder.u8(peer.hops()));
    put!(encoder.u8(4));
    put!(encoder.u8(peer.interface_id()));
    if let Some(rssi_dbm) = peer.rssi_dbm() {
        put!(encoder.u8(5));
        put!(encoder.i16(rssi_dbm));
    }
    if let Some(snr_db) = peer.snr_db() {
        put!(encoder.u8(6));
        put!(encoder.i16(snr_db));
    }
    put!(encoder.u8(7));
    put!(encoder.u64(peer.observed_age_ms()));
    put!(encoder.u8(8));
    put!(encoder.u64(peer.generation().get()));
    Ok(())
}

#[cfg(feature = "nomad")]
fn encode_nomad_fetch_start_accepted(
    encoder: &mut SliceEncoder<'_>,
    accepted: NomadFetchStartAccepted,
) -> Result<(), EncodeError> {
    put!(encoder.map(2));
    put!(encoder.u8(0));
    put!(encoder.bytes(accepted.id.as_bytes()));
    put!(encoder.u8(1));
    put!(encoder.u8(accepted.outcome.wire_code()));
    Ok(())
}

#[cfg(feature = "nomad")]
fn encode_nomad_fetch_poll(
    encoder: &mut SliceEncoder<'_>,
    response: &NomadFetchPollResponse,
) -> Result<(), EncodeError> {
    put!(encoder.map(2));
    put!(encoder.u8(0));
    put!(encoder.u8(response.wire_code()));
    put!(encoder.u8(1));
    match response {
        NomadFetchPollResponse::Pending(phase) => {
            put!(encoder.u8(phase.wire_code()));
        }
        NomadFetchPollResponse::Ready(page) => {
            put!(encoder.bytes(page.as_bytes()));
        }
        NomadFetchPollResponse::Failed(failure) => {
            put!(encoder.u8(failure.wire_code()));
        }
    }
    Ok(())
}

fn encode_node_diagnostics(
    encoder: &mut SliceEncoder<'_>,
    snapshot: &NodeDiagnosticsSnapshot,
) -> Result<(), EncodeError> {
    put!(encoder.map(6 + u64::from(snapshot.lora().is_some())));
    put!(encoder.u8(0));
    put!(encoder.u64(snapshot.uptime_ms()));
    put!(encoder.u8(1));
    put!(encoder.array(MAX_DIAGNOSTIC_INTERFACES as u64));
    for interface in snapshot.interfaces() {
        match interface {
            Some(interface) => encode_diagnostic_interface(encoder, *interface)?,
            None => {
                put!(encoder.null());
            }
        }
    }
    if let Some(lora) = snapshot.lora() {
        put!(encoder.u8(2));
        encode_lora_diagnostics(encoder, lora)?;
    }
    put!(encoder.u8(3));
    encode_rns_diagnostics(encoder, snapshot.rns())?;
    put!(encoder.u8(4));
    put!(encoder.u32(snapshot.observed_peer_count()));
    put!(encoder.u8(5));
    put!(encoder.u32(snapshot.retained_route_count()));
    put!(encoder.u8(6));
    put!(encoder.u32(snapshot.usable_route_count()));
    Ok(())
}

fn encode_diagnostic_interface(
    encoder: &mut SliceEncoder<'_>,
    interface: DiagnosticInterfaceRecord,
) -> Result<(), EncodeError> {
    put!(encoder.map(5 + u64::from(interface.bitrate().is_some())));
    put!(encoder.u8(0));
    put!(encoder.u8(interface.id()));
    put!(encoder.u8(1));
    put!(encoder.u8(interface.kind().wire_code()));
    put!(encoder.u8(2));
    put!(encoder.u8(interface.state().wire_code()));
    put!(encoder.u8(3));
    put!(encoder.u64(interface.generation()));
    put!(encoder.u8(4));
    put!(encoder.u16(interface.logical_mtu()));
    if let Some(bitrate) = interface.bitrate() {
        put!(encoder.u8(5));
        put!(encoder.u32(bitrate));
    }
    Ok(())
}

fn encode_lora_diagnostics(
    encoder: &mut SliceEncoder<'_>,
    lora: LoraDiagnostics,
) -> Result<(), EncodeError> {
    put!(encoder.map(
        16 + u64::from(lora.last_rx().is_some())
            + u64::from(lora.last_tx().is_some())
            + u64::from(lora.last_data_tx().is_some())
    ));
    put!(encoder.u8(0));
    put!(encoder.i16(lora.applied_tx_power_dbm()));
    put!(encoder.u8(1));
    put!(encoder.u32(lora.frequency_hz()));
    put!(encoder.u8(2));
    put!(encoder.u32(lora.bandwidth_hz()));
    put!(encoder.u8(3));
    put!(encoder.u8(lora.spreading_factor()));
    put!(encoder.u8(4));
    put!(encoder.u8(lora.coding_rate_denominator()));
    put!(encoder.u8(5));
    put!(encoder.u64(lora.rx_physical_frames()));
    put!(encoder.u8(6));
    put!(encoder.u64(lora.rx_packets()));
    put!(encoder.u8(7));
    put!(encoder.u64(lora.rx_errors()));
    put!(encoder.u8(8));
    put!(encoder.u64(lora.rx_drops()));
    put!(encoder.u8(9));
    put!(encoder.u64(lora.tx_terminal_jobs()));
    put!(encoder.u8(10));
    put!(encoder.u64(lora.tx_successes()));
    put!(encoder.u8(11));
    put!(encoder.u64(lora.tx_completed_frames()));
    put!(encoder.u8(12));
    put!(encoder.u64(lora.tx_access_rejects()));
    put!(encoder.u8(13));
    put!(encoder.u64(lora.tx_failures()));
    put!(encoder.u8(14));
    put!(encoder.u64(lora.cad_busy()));
    put!(encoder.u8(15));
    put!(encoder.u64(lora.cad_clear()));
    if let Some(last_rx) = lora.last_rx() {
        put!(encoder.u8(16));
        put!(encoder.map(3));
        put!(encoder.u8(0));
        put!(encoder.u64(last_rx.age_ms()));
        put!(encoder.u8(1));
        put!(encoder.i16(last_rx.rssi_dbm()));
        put!(encoder.u8(2));
        put!(encoder.i16(last_rx.snr_db()));
    }
    if let Some(last_tx) = lora.last_tx() {
        put!(encoder.u8(17));
        encode_diagnostic_lora_last_tx(encoder, last_tx)?;
    }
    if let Some(last_data_tx) = lora.last_data_tx() {
        put!(encoder.u8(18));
        encode_diagnostic_lora_last_data_tx(encoder, last_data_tx)?;
    }
    Ok(())
}

fn encode_diagnostic_lora_last_data_tx(
    encoder: &mut SliceEncoder<'_>,
    last_tx: DiagnosticLoraLastDataTx,
) -> Result<(), EncodeError> {
    let data = last_tx.data_evidence();
    put!(encoder.map(6));
    put!(encoder.u8(0));
    put!(encoder.u64(last_tx.age_ms()));
    put!(encoder.u8(1));
    put!(encoder.u8(last_tx.outcome().wire_code()));
    put!(encoder.u8(2));
    put!(encoder.u8(DiagnosticLoraTxFamily::Data.wire_code()));
    put!(encoder.u8(3));
    put!(encoder.u8(data.interface_id()));
    put!(encoder.u8(4));
    put!(encoder.u16(data.encoded_packet_len()));
    put!(encoder.u8(5));
    put!(encoder.bytes(data.encoded_packet_sha256().as_bytes()));
    Ok(())
}

fn encode_diagnostic_lora_last_tx(
    encoder: &mut SliceEncoder<'_>,
    last_tx: DiagnosticLoraLastTx,
) -> Result<(), EncodeError> {
    let data = last_tx.data_evidence();
    put!(encoder.map(2 + u64::from(last_tx.family().is_some()) + 3 * u64::from(data.is_some())));
    put!(encoder.u8(0));
    put!(encoder.u64(last_tx.age_ms()));
    put!(encoder.u8(1));
    put!(encoder.u8(last_tx.outcome().wire_code()));
    if let Some(family) = last_tx.family() {
        put!(encoder.u8(2));
        put!(encoder.u8(family.wire_code()));
    }
    if let Some(data) = data {
        put!(encoder.u8(3));
        put!(encoder.u8(data.interface_id()));
        put!(encoder.u8(4));
        put!(encoder.u16(data.encoded_packet_len()));
        put!(encoder.u8(5));
        put!(encoder.bytes(data.encoded_packet_sha256().as_bytes()));
    }
    Ok(())
}

fn encode_rns_diagnostics(
    encoder: &mut SliceEncoder<'_>,
    rns: RnsDiagnostics,
) -> Result<(), EncodeError> {
    put!(encoder.map(10));
    put!(encoder.u8(0));
    put!(encoder.u64(rns.received()));
    put!(encoder.u8(1));
    put!(encoder.u64(rns.forwarded()));
    put!(encoder.u8(2));
    put!(encoder.u64(rns.dedup_drops()));
    put!(encoder.u8(3));
    put!(encoder.u64(rns.invalid_drops()));
    put!(encoder.u8(4));
    put!(encoder.u64(rns.announces_received()));
    put!(encoder.u8(5));
    put!(encoder.u64(rns.paths_learned()));
    put!(encoder.u8(6));
    put!(encoder.u64(rns.paths_expired()));
    put!(encoder.u8(7));
    put!(encoder.u64(rns.links_established()));
    put!(encoder.u8(8));
    put!(encoder.u64(rns.links_closed()));
    put!(encoder.u8(9));
    put!(encoder.u64(rns.links_failed()));
    Ok(())
}

fn encode_route_diagnostics_page(
    encoder: &mut SliceEncoder<'_>,
    page: &RouteDiagnosticsPage,
) -> Result<(), EncodeError> {
    put!(encoder.map(3 + u64::from(page.next_cursor().is_some())));
    put!(encoder.u8(0));
    put!(encoder.u64(page.revision()));
    put!(encoder.u8(1));
    put!(encoder.u32(page.total_count()));
    put!(encoder.u8(2));
    put!(encoder.array(MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES as u64));
    for entry in page.entries() {
        match entry {
            Some(entry) => encode_route_diagnostic_entry(encoder, *entry)?,
            None => {
                put!(encoder.null());
            }
        }
    }
    if let Some(next_cursor) = page.next_cursor() {
        put!(encoder.u8(3));
        put!(encoder.bytes(&next_cursor.0));
    }
    Ok(())
}

fn encode_route_diagnostic_entry(
    encoder: &mut SliceEncoder<'_>,
    entry: RouteDiagnosticEntry,
) -> Result<(), EncodeError> {
    let entries = 3
        + u64::from(entry.next_hop_identity().is_some())
        + u64::from(entry.retained_interface().is_some())
        + u64::from(entry.learned_age_ms().is_some())
        + u64::from(entry.last_used_age_ms().is_some())
        + u64::from(entry.expires_in_ms().is_some());
    put!(encoder.map(entries));
    put!(encoder.u8(0));
    put!(encoder.bytes(&entry.destination().0));
    if let Some(next_hop) = entry.next_hop_identity() {
        put!(encoder.u8(1));
        put!(encoder.bytes(next_hop.as_bytes()));
    }
    put!(encoder.u8(2));
    put!(encoder.u8(entry.hops()));
    if let Some(interface) = entry.retained_interface() {
        put!(encoder.u8(3));
        put!(encoder.u8(interface));
    }
    put!(encoder.u8(4));
    put!(encoder.u8(entry.resolution().wire_code()));
    if let Some(age) = entry.learned_age_ms() {
        put!(encoder.u8(5));
        put!(encoder.u64(age));
    }
    if let Some(age) = entry.last_used_age_ms() {
        put!(encoder.u8(6));
        put!(encoder.u64(age));
    }
    if let Some(remaining) = entry.expires_in_ms() {
        put!(encoder.u8(7));
        put!(encoder.u64(remaining));
    }
    Ok(())
}

fn encode_radio_trace_page(
    encoder: &mut SliceEncoder<'_>,
    page: &RadioTracePage,
) -> Result<(), EncodeError> {
    put!(encoder.array(7));
    put!(encoder.u64(page.boot_id()));
    encode_radio_trace_applied_profile(encoder, page.applied_lora_profile())?;
    put!(encoder.u64(page.oldest_sequence()));
    put!(encoder.u64(page.next_sequence()));
    put!(encoder.bool(page.history_lost()));
    put!(encoder.array(MAX_RADIO_TRACE_PAGE_ENTRIES as u64));
    for entry in page.entries() {
        match entry {
            Some(entry) => encode_radio_trace_event(encoder, *entry)?,
            None => {
                put!(encoder.null());
            }
        }
    }
    match page.next_cursor() {
        Some(next_cursor) => encode_radio_trace_cursor(encoder, next_cursor)?,
        None => {
            put!(encoder.null());
        }
    }
    Ok(())
}

fn encode_radio_trace_cursor(
    encoder: &mut SliceEncoder<'_>,
    cursor: RadioTraceCursor,
) -> Result<(), EncodeError> {
    put!(encoder.array(2));
    put!(encoder.u64(cursor.boot_id()));
    put!(encoder.u64(cursor.after_sequence()));
    Ok(())
}

fn encode_radio_trace_applied_profile(
    encoder: &mut SliceEncoder<'_>,
    profile: RadioTraceAppliedLoraProfile,
) -> Result<(), EncodeError> {
    put!(encoder.array(10));
    put!(encoder.bytes(&profile.configuration_fingerprint()));
    put!(encoder.u32(profile.frequency_hz()));
    put!(encoder.u32(profile.bandwidth_hz()));
    put!(encoder.u16(profile.preamble_symbols()));
    put!(encoder.i16(profile.requested_power_dbm()));
    put!(encoder.u8(profile.spreading_factor()));
    put!(encoder.u8(profile.coding_rate_denominator()));
    put!(encoder.bool(profile.explicit_header()));
    put!(encoder.bool(profile.crc()));
    put!(encoder.bool(profile.iq_inverted()));
    Ok(())
}

fn encode_radio_trace_event(
    encoder: &mut SliceEncoder<'_>,
    event: RadioTraceEvent,
) -> Result<(), EncodeError> {
    put!(encoder.array(4));
    put!(encoder.u64(event.sequence()));
    put!(encoder.u64(event.observed_at_us()));
    put!(encoder.u8(event.kind().wire_code()));
    match event.kind() {
        RadioTraceEventKind::DataTx(tx) => encode_radio_trace_data_tx(encoder, tx),
        RadioTraceEventKind::LogicalRx(rx) => encode_radio_trace_logical_rx(encoder, rx),
        RadioTraceEventKind::RouteSelected(route) => {
            encode_radio_trace_route_selected(encoder, route)
        }
        RadioTraceEventKind::AttemptTerminal(terminal) => {
            encode_radio_trace_attempt_terminal(encoder, terminal)
        }
        RadioTraceEventKind::InboundProof(proof) => {
            encode_radio_trace_inbound_proof(encoder, proof)
        }
    }
}

fn encode_radio_trace_packet_evidence(
    encoder: &mut SliceEncoder<'_>,
    packet: RadioTracePacketEvidence,
) -> Result<(), EncodeError> {
    put!(encoder.u8(packet.interface_id()));
    put!(encoder.u16(packet.packet_len()));
    put!(encoder.bytes(packet.encoded_packet_sha256().as_bytes()));
    match packet.attempt_token() {
        Some(attempt_token) => {
            put!(encoder.bytes(attempt_token.as_bytes()));
        }
        None => {
            put!(encoder.null());
        }
    }
    Ok(())
}

fn encode_radio_trace_data_tx(
    encoder: &mut SliceEncoder<'_>,
    tx: RadioTraceDataTx,
) -> Result<(), EncodeError> {
    put!(encoder.array(9));
    encode_radio_trace_packet_evidence(encoder, tx.packet())?;
    put!(encoder.u8(tx.outcome().wire_code()));
    put!(encoder.u8(tx.planned_frames()));
    put!(encoder.u8(tx.completed_frames()));
    put!(encoder.bool(tx.authorization_observed()));
    put!(encoder.array(2));
    for completed_at_us in tx.frame_completed_at_us() {
        match completed_at_us {
            Some(completed_at_us) => {
                put!(encoder.u64(completed_at_us));
            }
            None => {
                put!(encoder.null());
            }
        }
    }
    Ok(())
}

fn encode_radio_trace_logical_rx(
    encoder: &mut SliceEncoder<'_>,
    rx: RadioTraceLogicalRx,
) -> Result<(), EncodeError> {
    put!(encoder.array(6));
    encode_radio_trace_packet_evidence(encoder, rx.packet())?;
    put!(encoder.i16(rx.rssi_dbm()));
    put!(encoder.i16(rx.snr_db()));
    Ok(())
}

fn encode_radio_trace_route_selected(
    encoder: &mut SliceEncoder<'_>,
    route: RadioTraceRouteSelected,
) -> Result<(), EncodeError> {
    put!(encoder.array(9));
    encode_radio_trace_packet_evidence(encoder, route.packet())?;
    put!(encoder.bytes(&route.destination().0));
    match route.next_hop_identity() {
        Some(next_hop) => {
            put!(encoder.bytes(next_hop.as_bytes()));
        }
        None => {
            put!(encoder.null());
        }
    }
    put!(encoder.u8(route.hops()));
    put!(encoder.u8(route.resolution().wire_code()));
    put!(encoder.u64(route.submission_id().0));
    Ok(())
}

fn encode_radio_trace_attempt_terminal(
    encoder: &mut SliceEncoder<'_>,
    terminal: RadioTraceAttemptTerminal,
) -> Result<(), EncodeError> {
    put!(encoder.array(3));
    put!(encoder.bytes(terminal.attempt_token().as_bytes()));
    put!(encoder.u8(terminal.outcome().wire_code()));
    match terminal.proof_ingress() {
        Some(ingress) => encode_ingress_observation(encoder, ingress)?,
        None => {
            put!(encoder.null());
        }
    }
    Ok(())
}

fn encode_radio_trace_inbound_proof(
    encoder: &mut SliceEncoder<'_>,
    proof: RadioTraceInboundProof,
) -> Result<(), EncodeError> {
    put!(encoder.array(8));
    put!(encoder.bytes(proof.correlation_token().as_bytes()));
    put!(encoder.u8(proof.stage().wire_code()));
    match proof.message_id() {
        Some(message_id) => {
            put!(encoder.bytes(&message_id));
        }
        None => {
            put!(encoder.null());
        }
    }
    match proof.packet() {
        Some(packet) => {
            put!(encoder.bytes(packet.encoded_packet_sha256().as_bytes()));
            put!(encoder.u16(packet.packet_len()));
        }
        None => {
            put!(encoder.null());
            put!(encoder.null());
        }
    }
    match proof.interface_id() {
        Some(interface) => {
            put!(encoder.u8(interface));
        }
        None => {
            put!(encoder.null());
        }
    }
    match proof.signal() {
        Some(signal) => {
            put!(encoder.array(2));
            put!(encoder.i16(signal.rssi_dbm()));
            put!(encoder.i16(signal.snr_db()));
        }
        None => {
            put!(encoder.null());
        }
    }
    match proof.dispatch_outcome() {
        Some(outcome) => {
            put!(encoder.u8(outcome.wire_code()));
        }
        None => {
            put!(encoder.null());
        }
    }
    Ok(())
}

fn encode_submission_status(
    encoder: &mut SliceEncoder<'_>,
    status: SubmissionStatus,
) -> Result<(), EncodeError> {
    let entries = match status.state {
        SubmissionState::Queued | SubmissionState::Preparing | SubmissionState::Cancelled => 2,
        SubmissionState::AwaitingDelivery(_) | SubmissionState::Delivered(_) => 4,
        SubmissionState::Failed(_) => 3,
    };
    put!(encoder.map(entries));
    put!(encoder.u8(0));
    put!(encoder.u64(status.id.0));
    put!(encoder.u8(1));
    put!(encoder.u8(status.state.wire_code()));
    match status.state {
        SubmissionState::AwaitingDelivery(details) | SubmissionState::Delivered(details) => {
            put!(encoder.u8(2));
            put!(encoder.u16(details.packet_len));
            put!(encoder.u8(3));
            put!(encoder.bytes(details.encoded_packet_sha256.as_bytes()));
        }
        SubmissionState::Failed(failure) => {
            put!(encoder.u8(4));
            put!(encoder.u8(failure.wire_code()));
        }
        SubmissionState::Queued | SubmissionState::Preparing | SubmissionState::Cancelled => {}
    }
    Ok(())
}

#[cfg(feature = "rns-data")]
fn encode_submission_accepted(
    encoder: &mut SliceEncoder<'_>,
    accepted: SubmissionAccepted,
) -> Result<(), EncodeError> {
    put!(encoder.map(1));
    put!(encoder.u8(0));
    put!(encoder.u64(accepted.id.0));
    Ok(())
}

fn encode_error(
    encoder: &mut SliceEncoder<'_>,
    error: ApiErrorResponse,
) -> Result<(), EncodeError> {
    put!(encoder.map(1 + u64::from(error.operation.is_some())));
    put!(encoder.u8(0));
    put!(encoder.u16(error.code.wire_code()));
    if let Some(operation) = error.operation {
        put!(encoder.u8(1));
        put!(encoder.u16(operation));
    }
    Ok(())
}

fn check_message_size(input: &[u8]) -> Result<(), DecodeError> {
    if input.len() > MAX_MESSAGE_BYTES {
        return Err(DecodeError::MessageTooLarge {
            actual: input.len(),
            max: MAX_MESSAGE_BYTES,
        });
    }
    Ok(())
}

fn decode_map_len(decoder: &mut Decoder<'_>) -> Result<u64, DecodeError> {
    let entries = decoder
        .map()
        .map_err(|_| DecodeError::Malformed)?
        .ok_or(DecodeError::IndefiniteLength)?;
    if entries > MAX_MAP_ENTRIES {
        return Err(DecodeError::TooManyMapEntries {
            actual: entries,
            max: MAX_MAP_ENTRIES,
        });
    }
    Ok(entries)
}

fn skip_strict(decoder: &mut Decoder<'_>, depth: usize) -> Result<(), DecodeError> {
    match decoder.datatype().map_err(|_| DecodeError::Malformed)? {
        Type::BytesIndef | Type::StringIndef | Type::ArrayIndef | Type::MapIndef => {
            Err(DecodeError::IndefiniteLength)
        }
        Type::Array => {
            let child_depth = enter_nesting(depth)?;
            let entries = decoder
                .array()
                .map_err(|_| DecodeError::Malformed)?
                .ok_or(DecodeError::IndefiniteLength)?;
            for _ in 0..entries {
                skip_strict(decoder, child_depth)?;
            }
            Ok(())
        }
        Type::Map => {
            let child_depth = enter_nesting(depth)?;
            let entries = decoder
                .map()
                .map_err(|_| DecodeError::Malformed)?
                .ok_or(DecodeError::IndefiniteLength)?;
            for _ in 0..entries {
                skip_strict(decoder, child_depth)?;
                skip_strict(decoder, child_depth)?;
            }
            Ok(())
        }
        Type::Tag => {
            let child_depth = enter_nesting(depth)?;
            decoder.tag().map_err(|_| DecodeError::Malformed)?;
            skip_strict(decoder, child_depth)
        }
        Type::Break | Type::Unknown(_) => Err(DecodeError::Malformed),
        Type::Bool
        | Type::Null
        | Type::Undefined
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::Int
        | Type::F16
        | Type::F32
        | Type::F64
        | Type::Simple
        | Type::Bytes
        | Type::String => decoder.skip().map_err(|_| DecodeError::Malformed),
    }
}

fn enter_nesting(depth: usize) -> Result<usize, DecodeError> {
    let actual = depth + 1;
    if actual > MAX_CBOR_NESTING_DEPTH {
        Err(DecodeError::NestingTooDeep {
            actual,
            max: MAX_CBOR_NESTING_DEPTH,
        })
    } else {
        Ok(actual)
    }
}

fn capture_body<'a>(input: &'a [u8], decoder: &mut Decoder<'a>) -> Result<&'a [u8], DecodeError> {
    let start = decoder.position();
    skip_strict(decoder, 0)?;
    let end = decoder.position();
    let size = end - start;
    if size > MAX_BODY_BYTES {
        return Err(DecodeError::BodyTooLarge {
            actual: size,
            max: MAX_BODY_BYTES,
        });
    }
    Ok(&input[start..end])
}

fn finish_item(decoder: &Decoder<'_>, input: &[u8]) -> Result<(), DecodeError> {
    if decoder.position() == input.len() {
        Ok(())
    } else {
        Err(DecodeError::TrailingData)
    }
}

fn finish_body(decoder: &Decoder<'_>, body: &[u8]) -> Result<(), DecodeError> {
    if decoder.position() == body.len() {
        Ok(())
    } else {
        Err(DecodeError::Malformed)
    }
}

fn reject_duplicate(present: bool, field: RequiredField) -> Result<(), DecodeError> {
    if present {
        Err(DecodeError::DuplicateField(field))
    } else {
        Ok(())
    }
}

fn require<T>(value: Option<T>, field: RequiredField) -> Result<T, DecodeError> {
    value.ok_or(DecodeError::MissingField(field))
}

fn check_version(version: ApiVersion) -> Result<(), DecodeError> {
    if version.major == API_VERSION_MAJOR {
        Ok(())
    } else {
        Err(DecodeError::UnsupportedVersion(version))
    }
}

fn check_encode_version(version: ApiVersion) -> Result<(), EncodeError> {
    if version.major == API_VERSION_MAJOR {
        Ok(())
    } else {
        Err(EncodeError::UnsupportedVersion(version))
    }
}

fn decode_version(decoder: &mut Decoder<'_>) -> Result<ApiVersion, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut major = None;
    let mut minor = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(major.is_some(), RequiredField::VersionMajor)?;
                major = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(minor.is_some(), RequiredField::VersionMinor)?;
                minor = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    Ok(ApiVersion {
        major: require(major, RequiredField::VersionMajor)?,
        minor: require(minor, RequiredField::VersionMinor)?,
    })
}

fn decode_request_body<'a>(
    operation: u16,
    body: &'a [u8],
) -> Result<DeviceRequest<'a>, DecodeError> {
    match operation {
        OP_SYSTEM_CAPABILITIES => decode_capabilities_request(body),
        OP_IDENTITY_SUMMARY => decode_identity_summary_request(body),
        OP_SUBMISSION_STATUS => decode_status_request(body),
        #[cfg(feature = "rns-data")]
        OP_SUBMIT_RNS_DATA => decode_submit_request(body),
        #[cfg(feature = "lxmf")]
        OP_LXMF_NEXT => decode_lxmf_next_request(body),
        #[cfg(feature = "lxmf")]
        OP_LXMF_READ => decode_lxmf_read_request(body),
        #[cfg(feature = "lxmf")]
        OP_LXMF_MAILBOX_STATUS => decode_empty_request(body, DeviceRequest::LxmfMailboxStatus),
        #[cfg(feature = "lxmf")]
        OP_LXMF_MAILBOX_ACKNOWLEDGE => decode_lxmf_mailbox_acknowledge_request(body),
        #[cfg(feature = "lxmf")]
        OP_LXMF_BASIC_SEND => decode_lxmf_basic_send_request(body),
        #[cfg(feature = "lxmf")]
        OP_LXMF_PEER_NEXT => decode_lxmf_peer_next_request(body),
        #[cfg(feature = "nomad")]
        OP_NOMAD_FETCH_START => decode_nomad_fetch_start_request(body),
        #[cfg(feature = "nomad")]
        OP_NOMAD_FETCH_POLL => decode_nomad_fetch_poll_request(body),
        OP_RETICULUM_PROBE_START => decode_probe_start_request(body),
        OP_RETICULUM_PROBE_POLL => decode_probe_poll_request(body),
        #[cfg(feature = "network-config")]
        OP_NETWORK_CONFIG_GET => decode_empty_request(body, DeviceRequest::NetworkConfigGet),
        #[cfg(feature = "network-config")]
        OP_NETWORK_CONFIG_MUTATE => decode_network_config_mutation_request(body),
        #[cfg(feature = "network-config")]
        OP_NETWORK_STATUS => decode_empty_request(body, DeviceRequest::NetworkStatus),
        OP_NODE_DIAGNOSTICS => decode_empty_request(body, DeviceRequest::NodeDiagnostics),
        OP_ROUTE_DIAGNOSTICS_PAGE => decode_route_diagnostics_request(body),
        OP_RADIO_TRACE_PAGE => decode_radio_trace_request(body),
        OP_MANUAL_SERVICE_ANNOUNCE => {
            decode_empty_request(body, DeviceRequest::ManualServiceAnnounce)
        }
        other => Err(DecodeError::UnsupportedOperation(other)),
    }
}

fn decode_empty_request(
    body: &[u8],
    request: DeviceRequest<'static>,
) -> Result<DeviceRequest<'static>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    for _ in 0..entries {
        decoder.u64().map_err(|_| DecodeError::Malformed)?;
        skip_strict(&mut decoder, 0)?;
    }
    finish_body(&decoder, body)?;
    Ok(request)
}

fn decode_probe_start_request(body: &[u8]) -> Result<DeviceRequest<'static>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut destination = None;
    let mut idempotency_key = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(destination.is_some(), RequiredField::ProbeStartDestination)?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::ProbeStartDestination,
                )?));
            }
            1 => {
                reject_duplicate(
                    idempotency_key.is_some(),
                    RequiredField::ProbeStartIdempotencyKey,
                )?;
                idempotency_key = Some(IdempotencyKey(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::ProbeStartIdempotencyKey,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::ReticulumProbeStart(ProbeStartRequest::new(
        require(destination, RequiredField::ProbeStartDestination)?,
        require(idempotency_key, RequiredField::ProbeStartIdempotencyKey)?,
    )))
}

fn decode_probe_poll_request(body: &[u8]) -> Result<DeviceRequest<'static>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::ProbeId)?;
                id = Some(
                    ProbeId::new(decode_fixed_bytes::<16>(
                        &mut decoder,
                        RequiredField::ProbeId,
                    )?)
                    .map_err(|_| DecodeError::InvalidProbeId)?,
                );
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::ReticulumProbePoll(ProbePollRequest::new(
        require(id, RequiredField::ProbeId)?,
    )))
}

fn decode_route_diagnostics_request(body: &[u8]) -> Result<DeviceRequest<'static>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut after = None;
    let mut after_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(after_seen, RequiredField::RouteDiagnosticsAfter)?;
                after_seen = true;
                after = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::RouteDiagnosticsAfter,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::RouteDiagnosticsPage(
        RouteDiagnosticsRequest::new(after),
    ))
}

fn decode_radio_trace_request(body: &[u8]) -> Result<DeviceRequest<'static>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut after = None;
    let mut after_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(after_seen, RequiredField::RadioTraceAfterCursor)?;
                after_seen = true;
                after = Some(decode_radio_trace_cursor_compact(&mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::RadioTracePage(RadioTracePageRequest::new(
        after,
    )))
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_next_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut after = None;
    let mut after_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(after_seen, RequiredField::LxmfAfterHandle)?;
                after_seen = true;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                after =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfAfterHandle,
                            value,
                        })?,
                    );
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::LxmfNext { after })
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_read_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut handle = None;
    let mut offset = None;
    let mut max_bytes = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(handle.is_some(), RequiredField::LxmfHandle)?;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                handle =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfHandle,
                            value,
                        })?,
                    );
            }
            1 => {
                reject_duplicate(offset.is_some(), RequiredField::LxmfReadOffset)?;
                offset = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(max_bytes.is_some(), RequiredField::LxmfReadMaxBytes)?;
                let value = decoder.u16().map_err(|_| DecodeError::Malformed)?;
                max_bytes =
                    Some(
                        LxmfReadLength::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfReadMaxBytes,
                            value: u64::from(value),
                        })?,
                    );
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::LxmfRead {
        handle: require(handle, RequiredField::LxmfHandle)?,
        offset: require(offset, RequiredField::LxmfReadOffset)?,
        max_bytes: require(max_bytes, RequiredField::LxmfReadMaxBytes)?,
    })
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_mailbox_acknowledge_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut through = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    through.is_some(),
                    RequiredField::LxmfMailboxAcknowledgedThrough,
                )?;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                through =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfMailboxAcknowledgedThrough,
                            value,
                        })?,
                    );
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::LxmfMailboxAcknowledge {
        through: require(through, RequiredField::LxmfMailboxAcknowledgedThrough)?,
    })
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_basic_send_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut destination = None;
    let mut timestamp_unix_ms = None;
    let mut title = None;
    let mut content = None;
    let mut idempotency_key = None;
    let mut location_seen = false;
    let mut location = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    destination.is_some(),
                    RequiredField::LxmfBasicSendDestination,
                )?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::LxmfBasicSendDestination,
                )?));
            }
            1 => {
                reject_duplicate(
                    timestamp_unix_ms.is_some(),
                    RequiredField::LxmfBasicSendTimestampUnixMs,
                )?;
                timestamp_unix_ms = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(title.is_some(), RequiredField::LxmfBasicSendTitle)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if bytes.len() > MAX_LXMF_BASIC_TITLE_BYTES {
                    return Err(DecodeError::LxmfBasicTitleTooLarge {
                        actual: bytes.len(),
                        max: MAX_LXMF_BASIC_TITLE_BYTES,
                    });
                }
                title = Some(bytes);
            }
            3 => {
                reject_duplicate(content.is_some(), RequiredField::LxmfBasicSendContent)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if bytes.len() > MAX_LXMF_BASIC_CONTENT_BYTES {
                    return Err(DecodeError::LxmfBasicContentTooLarge {
                        actual: bytes.len(),
                        max: MAX_LXMF_BASIC_CONTENT_BYTES,
                    });
                }
                content = Some(bytes);
            }
            4 => {
                reject_duplicate(
                    idempotency_key.is_some(),
                    RequiredField::LxmfBasicSendIdempotencyKey,
                )?;
                idempotency_key = Some(IdempotencyKey(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::LxmfBasicSendIdempotencyKey,
                )?));
            }
            5 => {
                reject_duplicate(location_seen, RequiredField::LxmfBasicSendLocation)?;
                location_seen = true;
                location = Some(decode_lxmf_message_location(&mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::LxmfBasicSend {
        destination: require(destination, RequiredField::LxmfBasicSendDestination)?,
        timestamp_unix_ms: require(
            timestamp_unix_ms,
            RequiredField::LxmfBasicSendTimestampUnixMs,
        )?,
        title: require(title, RequiredField::LxmfBasicSendTitle)?,
        content: require(content, RequiredField::LxmfBasicSendContent)?,
        location,
        idempotency_key: require(idempotency_key, RequiredField::LxmfBasicSendIdempotencyKey)?,
    })
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_message_location(
    decoder: &mut Decoder<'_>,
) -> Result<LxmfMessageLocation, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut latitude_e6 = None;
    let mut longitude_e6 = None;
    let mut altitude_cm = None;
    let mut speed_cm_per_second = None;
    let mut bearing_centidegrees = None;
    let mut accuracy_cm = None;
    let mut updated_at_unix_seconds = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(latitude_e6.is_some(), RequiredField::LxmfLocationLatitudeE6)?;
                latitude_e6 = Some(decoder.i32().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    longitude_e6.is_some(),
                    RequiredField::LxmfLocationLongitudeE6,
                )?;
                longitude_e6 = Some(decoder.i32().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(altitude_cm.is_some(), RequiredField::LxmfLocationAltitudeCm)?;
                altitude_cm = Some(decoder.i32().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(
                    speed_cm_per_second.is_some(),
                    RequiredField::LxmfLocationSpeedCmPerSecond,
                )?;
                speed_cm_per_second = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(
                    bearing_centidegrees.is_some(),
                    RequiredField::LxmfLocationBearingCentidegrees,
                )?;
                bearing_centidegrees = Some(decoder.i32().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(accuracy_cm.is_some(), RequiredField::LxmfLocationAccuracyCm)?;
                accuracy_cm = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(
                    updated_at_unix_seconds.is_some(),
                    RequiredField::LxmfLocationUpdatedAtUnixSeconds,
                )?;
                updated_at_unix_seconds = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    LxmfMessageLocation::new(
        require(latitude_e6, RequiredField::LxmfLocationLatitudeE6)?,
        require(longitude_e6, RequiredField::LxmfLocationLongitudeE6)?,
        require(altitude_cm, RequiredField::LxmfLocationAltitudeCm)?,
        require(
            speed_cm_per_second,
            RequiredField::LxmfLocationSpeedCmPerSecond,
        )?,
        require(
            bearing_centidegrees,
            RequiredField::LxmfLocationBearingCentidegrees,
        )?,
        require(accuracy_cm, RequiredField::LxmfLocationAccuracyCm)?,
        require(
            updated_at_unix_seconds,
            RequiredField::LxmfLocationUpdatedAtUnixSeconds,
        )?,
    )
    .map_err(|_| DecodeError::InvalidLxmfMessageLocation)
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_peer_next_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut incarnation = None;
    let mut generation = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    incarnation.is_some(),
                    RequiredField::LxmfPeerCursorIncarnation,
                )?;
                incarnation = Some(LxmfPeerDiscoveryIncarnation::new(decode_fixed_bytes::<8>(
                    &mut decoder,
                    RequiredField::LxmfPeerCursorIncarnation,
                )?));
            }
            1 => {
                reject_duplicate(
                    generation.is_some(),
                    RequiredField::LxmfPeerCursorGeneration,
                )?;
                generation = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let after = match (incarnation, generation) {
        (None, None) => None,
        (Some(incarnation), Some(generation)) => {
            Some(LxmfPeerDiscoveryCursor::new(incarnation, generation))
        }
        (None, Some(_)) => {
            return Err(DecodeError::MissingField(
                RequiredField::LxmfPeerCursorIncarnation,
            ));
        }
        (Some(_), None) => {
            return Err(DecodeError::MissingField(
                RequiredField::LxmfPeerCursorGeneration,
            ));
        }
    };
    Ok(DeviceRequest::LxmfPeerNext { after })
}

#[cfg(feature = "nomad")]
fn decode_nomad_fetch_start_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut destination = None;
    let mut path = None;
    let mut timestamp = None;
    let mut idempotency_key = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(destination.is_some(), RequiredField::NomadFetchDestination)?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::NomadFetchDestination,
                )?));
            }
            1 => {
                reject_duplicate(path.is_some(), RequiredField::NomadFetchPath)?;
                let value = decoder.str().map_err(|_| DecodeError::Malformed)?;
                path = Some(NomadPagePath::new(value).map_err(|error| match error {
                    crate::InvalidNomadPagePath::Invalid => DecodeError::InvalidNomadPagePath,
                    crate::InvalidNomadPagePath::TooLong { actual } => {
                        DecodeError::NomadPagePathTooLarge {
                            actual,
                            max: MAX_NOMAD_PAGE_PATH_BYTES,
                        }
                    }
                })?);
            }
            2 => {
                reject_duplicate(
                    timestamp.is_some(),
                    RequiredField::NomadFetchTimestampUnixMs,
                )?;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                timestamp = Some(NomadRequestTimestampUnixMs::new(value).map_err(|_| {
                    DecodeError::InvalidValue {
                        field: RequiredField::NomadFetchTimestampUnixMs,
                        value,
                    }
                })?);
            }
            3 => {
                reject_duplicate(
                    idempotency_key.is_some(),
                    RequiredField::NomadFetchIdempotencyKey,
                )?;
                idempotency_key = Some(IdempotencyKey(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::NomadFetchIdempotencyKey,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::NomadFetchStart(NomadFetchStartRequest::new(
        require(destination, RequiredField::NomadFetchDestination)?,
        require(path, RequiredField::NomadFetchPath)?,
        require(timestamp, RequiredField::NomadFetchTimestampUnixMs)?,
        require(idempotency_key, RequiredField::NomadFetchIdempotencyKey)?,
    )))
}

#[cfg(feature = "nomad")]
fn decode_nomad_fetch_poll_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::NomadFetchId)?;
                id = Some(decode_nomad_fetch_id(&mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::NomadFetchPoll(NomadFetchPollRequest {
        id: require(id, RequiredField::NomadFetchId)?,
    }))
}

#[cfg(feature = "network-config")]
fn decode_network_config_mutation_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut kind = None;
    let mut value = None;
    let mut expected_revision = None;
    let mut idempotency_key = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(kind.is_some(), RequiredField::NetworkConfigMutationKind)?;
                kind = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(value.is_some(), RequiredField::NetworkConfigMutationValue)?;
                value = Some(capture_body(body, &mut decoder)?);
            }
            2 => {
                reject_duplicate(
                    expected_revision.is_some(),
                    RequiredField::NetworkConfigExpectedRevision,
                )?;
                expected_revision = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(
                    idempotency_key.is_some(),
                    RequiredField::NetworkConfigIdempotencyKey,
                )?;
                idempotency_key = Some(IdempotencyKey(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::NetworkConfigIdempotencyKey,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let mutation = decode_network_config_mutation(
        require(kind, RequiredField::NetworkConfigMutationKind)?,
        require(value, RequiredField::NetworkConfigMutationValue)?,
    )?;
    Ok(DeviceRequest::NetworkConfigMutate(
        NetworkConfigMutationRequest::new(
            mutation,
            require(
                expected_revision,
                RequiredField::NetworkConfigExpectedRevision,
            )?,
            require(idempotency_key, RequiredField::NetworkConfigIdempotencyKey)?,
        ),
    ))
}

#[cfg(feature = "network-config")]
fn decode_network_config_mutation<'a>(
    kind: u8,
    value: &'a [u8],
) -> Result<NetworkConfigMutation<'a>, DecodeError> {
    match kind {
        0 => {
            let mut decoder = Decoder::new(value);
            let entries = decode_map_len(&mut decoder)?;
            let mut profile_id = None;
            let mut network = None;
            for _ in 0..entries {
                let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                match key {
                    0 => {
                        reject_duplicate(
                            profile_id.is_some(),
                            RequiredField::WifiNetworkProfileId,
                        )?;
                        profile_id = Some(decode_wifi_profile_id(&mut decoder)?);
                    }
                    1 => {
                        reject_duplicate(
                            network.is_some(),
                            RequiredField::NetworkConfigWifiProfiles,
                        )?;
                        network = Some(decode_wifi_network_update(&mut decoder)?);
                    }
                    _ => skip_strict(&mut decoder, 0)?,
                }
            }
            finish_body(&decoder, value)?;
            Ok(NetworkConfigMutation::UpsertWifi {
                profile_id: require(profile_id, RequiredField::WifiNetworkProfileId)?,
                network: require(network, RequiredField::NetworkConfigWifiProfiles)?,
            })
        }
        1 => {
            let mut decoder = Decoder::new(value);
            let entries = decode_map_len(&mut decoder)?;
            let mut profile_id = None;
            for _ in 0..entries {
                let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                match key {
                    0 => {
                        reject_duplicate(
                            profile_id.is_some(),
                            RequiredField::WifiNetworkProfileId,
                        )?;
                        profile_id = Some(decode_wifi_profile_id(&mut decoder)?);
                    }
                    _ => skip_strict(&mut decoder, 0)?,
                }
            }
            finish_body(&decoder, value)?;
            Ok(NetworkConfigMutation::RemoveWifi {
                profile_id: require(profile_id, RequiredField::WifiNetworkProfileId)?,
            })
        }
        2 => {
            let mut decoder = Decoder::new(value);
            let peer = if matches!(
                decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                Type::Null
            ) {
                decoder.null().map_err(|_| DecodeError::Malformed)?;
                None
            } else {
                Some(decode_tcp_peer_update(&mut decoder)?)
            };
            finish_body(&decoder, value)?;
            Ok(NetworkConfigMutation::ReplaceTcpPeer(peer))
        }
        3 => {
            let mut decoder = Decoder::new(value);
            let peer = if matches!(
                decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                Type::Null
            ) {
                decoder.null().map_err(|_| DecodeError::Malformed)?;
                None
            } else {
                Some(decode_tcp_host_peer_update(&mut decoder)?)
            };
            finish_body(&decoder, value)?;
            Ok(NetworkConfigMutation::ReplaceTcpHostPeer(peer))
        }
        4 => {
            let mut decoder = Decoder::new(value);
            let policy = decode_gateway_policy(&mut decoder)?;
            finish_body(&decoder, value)?;
            Ok(NetworkConfigMutation::SetGatewayPolicy(policy))
        }
        5 => {
            let mut decoder = Decoder::new(value);
            let config = decode_rmap_config(&mut decoder)?;
            finish_body(&decoder, value)?;
            Ok(NetworkConfigMutation::SetRmapConfig(config))
        }
        6 => {
            let mut decoder = Decoder::new(value);
            let power = decoder.u8().map_err(|_| DecodeError::Malformed)?;
            finish_body(&decoder, value)?;
            Ok(NetworkConfigMutation::SetLoraTxPower(
                LoraTransmitPowerDbm::new(power)
                    .map_err(|_| DecodeError::InvalidLoraTransmitPowerDbm)?,
            ))
        }
        7 => {
            let mut decoder = Decoder::new(value);
            let profile = decode_lora_radio_profile(&mut decoder)?;
            finish_body(&decoder, value)?;
            Ok(NetworkConfigMutation::SetLoraProfile(profile))
        }
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::NetworkConfigMutationKind,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_wifi_profile_id(decoder: &mut Decoder<'_>) -> Result<WifiNetworkProfileId, DecodeError> {
    let bytes = decode_fixed_bytes::<16>(decoder, RequiredField::WifiNetworkProfileId)?;
    WifiNetworkProfileId::new(bytes).map_err(|_| DecodeError::InvalidWifiNetworkProfileId)
}

#[cfg(feature = "network-config")]
fn decode_wifi_network_update<'a>(
    decoder: &mut Decoder<'a>,
) -> Result<WifiNetworkUpdate<'a>, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut enabled = None;
    let mut ssid = None;
    let mut credential = None;
    let mut priority = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(enabled.is_some(), RequiredField::WifiEnabled)?;
                enabled = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(ssid.is_some(), RequiredField::WifiSsid)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                ssid = Some(WifiSsid::new(bytes).map_err(|_| DecodeError::InvalidWifiSsid)?);
            }
            2 => {
                reject_duplicate(credential.is_some(), RequiredField::WifiCredential)?;
                credential = Some(decode_wifi_credential_update(decoder)?);
            }
            3 => {
                reject_duplicate(priority.is_some(), RequiredField::WifiPriority)?;
                priority = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    Ok(WifiNetworkUpdate::new(
        require(enabled, RequiredField::WifiEnabled)?,
        require(priority, RequiredField::WifiPriority)?,
        require(ssid, RequiredField::WifiSsid)?,
        require(credential, RequiredField::WifiCredential)?,
    ))
}

#[cfg(feature = "network-config")]
fn decode_wifi_credential_update<'a>(
    decoder: &mut Decoder<'a>,
) -> Result<WifiCredentialUpdate<'a>, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut kind = None;
    let mut passphrase = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(kind.is_some(), RequiredField::WifiCredentialUpdateKind)?;
                kind = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(passphrase.is_some(), RequiredField::WifiPassphrase)?;
                passphrase = Some(decoder.bytes().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    match require(kind, RequiredField::WifiCredentialUpdateKind)? {
        0 if passphrase.is_none() => Ok(WifiCredentialUpdate::Keep),
        1 => WifiCredentialUpdate::replace(require(passphrase, RequiredField::WifiPassphrase)?)
            .map_err(|_| DecodeError::InvalidWifiPassphrase),
        value => Err(DecodeError::InvalidValue {
            field: RequiredField::WifiCredentialUpdateKind,
            value: u64::from(value),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_tcp_peer_update(
    decoder: &mut Decoder<'_>,
) -> Result<ReticulumTcpPeerUpdate, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut enabled = None;
    let mut ipv4_address = None;
    let mut port = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(enabled.is_some(), RequiredField::ReticulumTcpPeerEnabled)?;
                enabled = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    ipv4_address.is_some(),
                    RequiredField::ReticulumTcpPeerIpv4Address,
                )?;
                ipv4_address = Some(
                    ReticulumTcpPeerIpv4Address::new(decode_fixed_bytes::<4>(
                        decoder,
                        RequiredField::ReticulumTcpPeerIpv4Address,
                    )?)
                    .map_err(|_| DecodeError::InvalidReticulumTcpPeerIpv4Address)?,
                );
            }
            2 => {
                reject_duplicate(port.is_some(), RequiredField::ReticulumTcpPeerPort)?;
                port = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    ReticulumTcpPeerUpdate::new(
        require(enabled, RequiredField::ReticulumTcpPeerEnabled)?,
        require(ipv4_address, RequiredField::ReticulumTcpPeerIpv4Address)?,
        require(port, RequiredField::ReticulumTcpPeerPort)?,
    )
    .map_err(|_| DecodeError::InvalidReticulumTcpPeerPort)
}

#[cfg(feature = "network-config")]
fn decode_tcp_host_peer_update<'a>(
    decoder: &mut Decoder<'a>,
) -> Result<ReticulumTcpPeerHostUpdate<'a>, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut enabled = None;
    let mut hostname = None;
    let mut port = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(enabled.is_some(), RequiredField::ReticulumTcpPeerEnabled)?;
                enabled = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(hostname.is_some(), RequiredField::ReticulumTcpPeerHostname)?;
                let value = decoder.str().map_err(|_| DecodeError::Malformed)?;
                hostname = Some(
                    ReticulumTcpPeerHostname::new(value)
                        .map_err(|_| DecodeError::InvalidReticulumTcpPeerHostname)?,
                );
            }
            2 => {
                reject_duplicate(port.is_some(), RequiredField::ReticulumTcpPeerPort)?;
                port = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    ReticulumTcpPeerHostUpdate::new(
        require(enabled, RequiredField::ReticulumTcpPeerEnabled)?,
        require(hostname, RequiredField::ReticulumTcpPeerHostname)?,
        require(port, RequiredField::ReticulumTcpPeerPort)?,
    )
    .map_err(|_| DecodeError::InvalidReticulumTcpPeerPort)
}

#[cfg(feature = "network-config")]
fn decode_gateway_policy(decoder: &mut Decoder<'_>) -> Result<GatewayPolicy, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut wifi_transport_enabled = None;
    let mut automatic_announces_enabled = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    wifi_transport_enabled.is_some(),
                    RequiredField::GatewayPolicyWifiTransportEnabled,
                )?;
                wifi_transport_enabled = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    automatic_announces_enabled.is_some(),
                    RequiredField::GatewayPolicyAutomaticAnnouncesEnabled,
                )?;
                automatic_announces_enabled =
                    Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    Ok(GatewayPolicy::new(
        require(
            wifi_transport_enabled,
            RequiredField::GatewayPolicyWifiTransportEnabled,
        )?,
        require(
            automatic_announces_enabled,
            RequiredField::GatewayPolicyAutomaticAnnouncesEnabled,
        )?,
    ))
}

#[cfg(feature = "network-config")]
fn decode_rmap_location(decoder: &mut Decoder<'_>) -> Result<RmapLocation, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut latitude_e6 = None;
    let mut longitude_e6 = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(latitude_e6.is_some(), RequiredField::RmapLatitudeE6)?;
                latitude_e6 = Some(decoder.i32().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(longitude_e6.is_some(), RequiredField::RmapLongitudeE6)?;
                longitude_e6 = Some(decoder.i32().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    RmapLocation::new(
        require(latitude_e6, RequiredField::RmapLatitudeE6)?,
        require(longitude_e6, RequiredField::RmapLongitudeE6)?,
    )
    .map_err(|_| DecodeError::InvalidRmapLocation)
}

#[cfg(feature = "network-config")]
fn decode_rmap_config(decoder: &mut Decoder<'_>) -> Result<RmapConfig, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut discovery_enabled = None;
    let mut share_location = None;
    let mut phone_location_seen = false;
    let mut phone_location = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    discovery_enabled.is_some(),
                    RequiredField::RmapDiscoveryEnabled,
                )?;
                discovery_enabled = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(share_location.is_some(), RequiredField::RmapShareLocation)?;
                share_location = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(phone_location_seen, RequiredField::RmapPhoneLocation)?;
                phone_location_seen = true;
                if matches!(
                    decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                    Type::Null
                ) {
                    decoder.null().map_err(|_| DecodeError::Malformed)?;
                } else {
                    phone_location = Some(decode_rmap_location(decoder)?);
                }
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    if !phone_location_seen {
        return Err(DecodeError::MissingField(RequiredField::RmapPhoneLocation));
    }
    Ok(RmapConfig::new(
        require(discovery_enabled, RequiredField::RmapDiscoveryEnabled)?,
        require(share_location, RequiredField::RmapShareLocation)?,
        phone_location,
    ))
}

#[cfg(feature = "network-config")]
fn decode_lora_radio_profile(decoder: &mut Decoder<'_>) -> Result<LoraRadioProfile, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut frequency_hz = None;
    let mut bandwidth_hz = None;
    let mut spreading_factor = None;
    let mut coding_rate_denominator = None;
    let mut tx_power_dbm = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    frequency_hz.is_some(),
                    RequiredField::LoraProfileFrequencyHz,
                )?;
                frequency_hz = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    bandwidth_hz.is_some(),
                    RequiredField::LoraProfileBandwidthHz,
                )?;
                bandwidth_hz = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(
                    spreading_factor.is_some(),
                    RequiredField::LoraProfileSpreadingFactor,
                )?;
                spreading_factor = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(
                    coding_rate_denominator.is_some(),
                    RequiredField::LoraProfileCodingRate,
                )?;
                coding_rate_denominator = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(tx_power_dbm.is_some(), RequiredField::LoraProfileTxPowerDbm)?;
                tx_power_dbm = Some(
                    LoraTransmitPowerDbm::new(decoder.u8().map_err(|_| DecodeError::Malformed)?)
                        .map_err(|_| DecodeError::InvalidLoraTransmitPowerDbm)?,
                );
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    LoraRadioProfile::new(
        require(frequency_hz, RequiredField::LoraProfileFrequencyHz)?,
        require(bandwidth_hz, RequiredField::LoraProfileBandwidthHz)?,
        require(spreading_factor, RequiredField::LoraProfileSpreadingFactor)?,
        require(
            coding_rate_denominator,
            RequiredField::LoraProfileCodingRate,
        )?,
        require(tx_power_dbm, RequiredField::LoraProfileTxPowerDbm)?,
    )
    .map_err(|_| DecodeError::InvalidLoraRadioProfile)
}

fn decode_capabilities_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    for _ in 0..entries {
        decoder.u64().map_err(|_| DecodeError::Malformed)?;
        skip_strict(&mut decoder, 0)?;
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::SystemCapabilities)
}

fn decode_identity_summary_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    for _ in 0..entries {
        decoder.u64().map_err(|_| DecodeError::Malformed)?;
        skip_strict(&mut decoder, 0)?;
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::IdentitySummary)
}

fn decode_status_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::SubmissionId)?;
                id = Some(SubmissionId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::SubmissionStatus {
        id: require(id, RequiredField::SubmissionId)?,
    })
}

#[cfg(feature = "rns-data")]
fn decode_submit_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut destination = None;
    let mut payload = None;
    let mut idempotency_key = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(destination.is_some(), RequiredField::SubmitDestination)?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::SubmitDestination,
                )?));
            }
            1 => {
                reject_duplicate(payload.is_some(), RequiredField::SubmitPayload)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if bytes.len() > MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES {
                    return Err(DecodeError::PayloadTooLarge {
                        actual: bytes.len(),
                        max: MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES,
                    });
                }
                payload = Some(bytes);
            }
            2 => {
                reject_duplicate(
                    idempotency_key.is_some(),
                    RequiredField::SubmitIdempotencyKey,
                )?;
                idempotency_key = Some(IdempotencyKey(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::SubmitIdempotencyKey,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::SubmitRnsData {
        destination: require(destination, RequiredField::SubmitDestination)?,
        payload: require(payload, RequiredField::SubmitPayload)?,
        idempotency_key: require(idempotency_key, RequiredField::SubmitIdempotencyKey)?,
    })
}

fn decode_capabilities(body: &[u8]) -> Result<CapabilitySnapshot, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut api_version = None;
    let mut packet_output = None;
    let mut direct_radio_tx = None;
    let mut submit = None;
    let mut max_message = None;
    let mut max_body = None;
    let mut max_payload = None;
    let mut lxmf = None;
    let mut max_lxmf_read_chunk = None;
    let mut lxmf_basic_send = None;
    let mut max_lxmf_basic_title = None;
    let mut max_lxmf_basic_content = None;
    let mut lxmf_peer_discovery = None;
    let mut max_lxmf_peer_app_data = None;
    let mut nomad = None;
    let mut max_nomad_page_path = None;
    let mut max_nomad_page = None;
    let mut network_config = None;
    let mut manual_service_announce = None;
    let mut reticulum_probe = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(api_version.is_some(), RequiredField::CapabilityApiVersion)?;
                api_version = Some(decode_version(&mut decoder)?);
            }
            1 => {
                reject_duplicate(
                    packet_output.is_some(),
                    RequiredField::CapabilityPacketOutput,
                )?;
                packet_output = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(
                    direct_radio_tx.is_some(),
                    RequiredField::CapabilityDirectRadioTx,
                )?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                direct_radio_tx = Some(decode_direct_radio_availability(value)?);
            }
            3 => {
                reject_duplicate(submit.is_some(), RequiredField::CapabilitySubmitRnsData)?;
                submit = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(
                    max_message.is_some(),
                    RequiredField::CapabilityMaxMessageBytes,
                )?;
                max_message = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(max_body.is_some(), RequiredField::CapabilityMaxBodyBytes)?;
                max_body = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(
                    max_payload.is_some(),
                    RequiredField::CapabilityMaxSubmitPayloadBytes,
                )?;
                max_payload = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            9 => {
                reject_duplicate(lxmf.is_some(), RequiredField::CapabilityLxmf)?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                lxmf = Some(decode_capability_availability(
                    value,
                    RequiredField::CapabilityLxmf,
                )?);
            }
            10 => {
                reject_duplicate(
                    max_lxmf_read_chunk.is_some(),
                    RequiredField::CapabilityMaxLxmfReadChunkBytes,
                )?;
                max_lxmf_read_chunk = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            11 => {
                reject_duplicate(
                    lxmf_basic_send.is_some(),
                    RequiredField::CapabilityLxmfBasicSend,
                )?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                lxmf_basic_send = Some(decode_capability_availability(
                    value,
                    RequiredField::CapabilityLxmfBasicSend,
                )?);
            }
            12 => {
                reject_duplicate(
                    max_lxmf_basic_title.is_some(),
                    RequiredField::CapabilityMaxLxmfBasicTitleBytes,
                )?;
                max_lxmf_basic_title = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            13 => {
                reject_duplicate(
                    max_lxmf_basic_content.is_some(),
                    RequiredField::CapabilityMaxLxmfBasicContentBytes,
                )?;
                max_lxmf_basic_content = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            14 => {
                reject_duplicate(
                    lxmf_peer_discovery.is_some(),
                    RequiredField::CapabilityLxmfPeerDiscovery,
                )?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                lxmf_peer_discovery = Some(decode_capability_availability(
                    value,
                    RequiredField::CapabilityLxmfPeerDiscovery,
                )?);
            }
            15 => {
                reject_duplicate(
                    max_lxmf_peer_app_data.is_some(),
                    RequiredField::CapabilityMaxLxmfPeerAppDataBytes,
                )?;
                max_lxmf_peer_app_data = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            16 => {
                reject_duplicate(nomad.is_some(), RequiredField::CapabilityNomad)?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                nomad = Some(decode_capability_availability(
                    value,
                    RequiredField::CapabilityNomad,
                )?);
            }
            17 => {
                reject_duplicate(
                    max_nomad_page_path.is_some(),
                    RequiredField::CapabilityMaxNomadPagePathBytes,
                )?;
                max_nomad_page_path = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            18 => {
                reject_duplicate(
                    max_nomad_page.is_some(),
                    RequiredField::CapabilityMaxNomadPageBytes,
                )?;
                max_nomad_page = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            19 => {
                reject_duplicate(
                    network_config.is_some(),
                    RequiredField::CapabilityNetworkConfig,
                )?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                network_config = Some(decode_capability_availability(
                    value,
                    RequiredField::CapabilityNetworkConfig,
                )?);
            }
            20 => {
                reject_duplicate(
                    manual_service_announce.is_some(),
                    RequiredField::CapabilityManualServiceAnnounce,
                )?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                manual_service_announce = Some(decode_capability_availability(
                    value,
                    RequiredField::CapabilityManualServiceAnnounce,
                )?);
            }
            21 => {
                reject_duplicate(
                    reticulum_probe.is_some(),
                    RequiredField::CapabilityReticulumProbe,
                )?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                reticulum_probe = Some(decode_capability_availability(
                    value,
                    RequiredField::CapabilityReticulumProbe,
                )?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let api_version = require(api_version, RequiredField::CapabilityApiVersion)?;
    check_version(api_version)?;
    Ok(CapabilitySnapshot {
        api_version,
        packet_output: require(packet_output, RequiredField::CapabilityPacketOutput)?,
        direct_radio_tx: require(direct_radio_tx, RequiredField::CapabilityDirectRadioTx)?,
        submit_rns_data: require(submit, RequiredField::CapabilitySubmitRnsData)?,
        max_message_bytes: require(max_message, RequiredField::CapabilityMaxMessageBytes)?,
        max_body_bytes: require(max_body, RequiredField::CapabilityMaxBodyBytes)?,
        max_submit_rns_data_payload_bytes: require(
            max_payload,
            RequiredField::CapabilityMaxSubmitPayloadBytes,
        )?,
        lxmf: require(lxmf, RequiredField::CapabilityLxmf)?,
        max_lxmf_read_chunk_bytes: require(
            max_lxmf_read_chunk,
            RequiredField::CapabilityMaxLxmfReadChunkBytes,
        )?,
        lxmf_basic_send: require(lxmf_basic_send, RequiredField::CapabilityLxmfBasicSend)?,
        max_lxmf_basic_title_bytes: require(
            max_lxmf_basic_title,
            RequiredField::CapabilityMaxLxmfBasicTitleBytes,
        )?,
        max_lxmf_basic_content_bytes: require(
            max_lxmf_basic_content,
            RequiredField::CapabilityMaxLxmfBasicContentBytes,
        )?,
        lxmf_peer_discovery: require(
            lxmf_peer_discovery,
            RequiredField::CapabilityLxmfPeerDiscovery,
        )?,
        max_lxmf_peer_app_data_bytes: require(
            max_lxmf_peer_app_data,
            RequiredField::CapabilityMaxLxmfPeerAppDataBytes,
        )?,
        nomad: require(nomad, RequiredField::CapabilityNomad)?,
        max_nomad_page_path_bytes: require(
            max_nomad_page_path,
            RequiredField::CapabilityMaxNomadPagePathBytes,
        )?,
        max_nomad_page_bytes: require(max_nomad_page, RequiredField::CapabilityMaxNomadPageBytes)?,
        network_config: require(network_config, RequiredField::CapabilityNetworkConfig)?,
        manual_service_announce: require(
            manual_service_announce,
            RequiredField::CapabilityManualServiceAnnounce,
        )?,
        reticulum_probe: require(reticulum_probe, RequiredField::CapabilityReticulumProbe)?,
    })
}

#[cfg(feature = "network-config")]
fn decode_network_config(body: &[u8]) -> Result<NetworkConfigSnapshot, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut revision = None;
    let mut wifi_profiles = None;
    let mut tcp_peer_seen = false;
    let mut tcp_peer = None;
    let mut wifi_transport_enabled = None;
    let mut automatic_announces_enabled = None;
    let mut rmap_discovery_enabled = None;
    let mut rmap_share_location = None;
    let mut rmap_phone_location_seen = false;
    let mut rmap_phone_location = None;
    let mut tcp_host_peer_seen = false;
    let mut tcp_host_peer = None;
    let mut lora_profile = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(revision.is_some(), RequiredField::NetworkConfigRevision)?;
                revision = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    wifi_profiles.is_some(),
                    RequiredField::NetworkConfigWifiProfiles,
                )?;
                wifi_profiles = Some(decode_wifi_network_profiles(&mut decoder)?);
            }
            2 => {
                reject_duplicate(tcp_peer_seen, RequiredField::NetworkConfigTcpPeer)?;
                tcp_peer_seen = true;
                if matches!(
                    decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                    Type::Null
                ) {
                    decoder.null().map_err(|_| DecodeError::Malformed)?;
                } else {
                    tcp_peer = Some(decode_tcp_peer_summary(&mut decoder)?);
                }
            }
            3 => {
                reject_duplicate(
                    wifi_transport_enabled.is_some(),
                    RequiredField::NetworkConfigWifiTransportEnabled,
                )?;
                wifi_transport_enabled = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(
                    automatic_announces_enabled.is_some(),
                    RequiredField::NetworkConfigAutomaticAnnouncesEnabled,
                )?;
                automatic_announces_enabled =
                    Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(
                    rmap_discovery_enabled.is_some(),
                    RequiredField::NetworkConfigRmapDiscoveryEnabled,
                )?;
                rmap_discovery_enabled = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(
                    rmap_share_location.is_some(),
                    RequiredField::NetworkConfigRmapShareLocation,
                )?;
                rmap_share_location = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            7 => {
                reject_duplicate(
                    rmap_phone_location_seen,
                    RequiredField::NetworkConfigRmapPhoneLocation,
                )?;
                rmap_phone_location_seen = true;
                if matches!(
                    decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                    Type::Null
                ) {
                    decoder.null().map_err(|_| DecodeError::Malformed)?;
                } else {
                    rmap_phone_location = Some(decode_rmap_location(&mut decoder)?);
                }
            }
            8 => {
                reject_duplicate(tcp_host_peer_seen, RequiredField::NetworkConfigTcpHostPeer)?;
                tcp_host_peer_seen = true;
                if matches!(
                    decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                    Type::Null
                ) {
                    decoder.null().map_err(|_| DecodeError::Malformed)?;
                } else {
                    tcp_host_peer = Some(decode_tcp_host_peer_summary(&mut decoder)?);
                }
            }
            9 => {
                reject_duplicate(
                    lora_profile.is_some(),
                    RequiredField::NetworkConfigLoraProfile,
                )?;
                lora_profile = Some(decode_lora_radio_profile(&mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    if !tcp_peer_seen {
        return Err(DecodeError::MissingField(
            RequiredField::NetworkConfigTcpPeer,
        ));
    }
    if !tcp_host_peer_seen {
        return Err(DecodeError::MissingField(
            RequiredField::NetworkConfigTcpHostPeer,
        ));
    }
    if !rmap_phone_location_seen {
        return Err(DecodeError::MissingField(
            RequiredField::NetworkConfigRmapPhoneLocation,
        ));
    }
    NetworkConfigSnapshot::new(
        require(revision, RequiredField::NetworkConfigRevision)?,
        require(wifi_profiles, RequiredField::NetworkConfigWifiProfiles)?,
        tcp_peer,
        tcp_host_peer,
        GatewayPolicy::new(
            require(
                wifi_transport_enabled,
                RequiredField::NetworkConfigWifiTransportEnabled,
            )?,
            require(
                automatic_announces_enabled,
                RequiredField::NetworkConfigAutomaticAnnouncesEnabled,
            )?,
        ),
        RmapConfig::new(
            require(
                rmap_discovery_enabled,
                RequiredField::NetworkConfigRmapDiscoveryEnabled,
            )?,
            require(
                rmap_share_location,
                RequiredField::NetworkConfigRmapShareLocation,
            )?,
            rmap_phone_location,
        ),
        require(lora_profile, RequiredField::NetworkConfigLoraProfile)?,
    )
    .map_err(|_| DecodeError::InvalidNetworkConfigSnapshot)
}

#[cfg(feature = "network-config")]
fn decode_wifi_network_profiles(
    decoder: &mut Decoder<'_>,
) -> Result<[Option<WifiNetworkConfigSummary>; MAX_WIFI_NETWORK_PROFILES], DecodeError> {
    let entries = decoder
        .array()
        .map_err(|_| DecodeError::Malformed)?
        .ok_or(DecodeError::IndefiniteLength)?;
    if entries > MAX_WIFI_NETWORK_PROFILES as u64 {
        return Err(DecodeError::TooManyWifiNetworkProfiles {
            actual: entries,
            max: MAX_WIFI_NETWORK_PROFILES as u64,
        });
    }
    let mut profiles: [Option<WifiNetworkConfigSummary>; MAX_WIFI_NETWORK_PROFILES] =
        [None; MAX_WIFI_NETWORK_PROFILES];
    for index in 0..entries as usize {
        let profile = decode_wifi_network_summary(decoder)?;
        if profiles[..index]
            .iter()
            .flatten()
            .any(|candidate| candidate.profile_id() == profile.profile_id())
        {
            return Err(DecodeError::DuplicateField(
                RequiredField::WifiNetworkProfileId,
            ));
        }
        profiles[index] = Some(profile);
    }
    Ok(profiles)
}

#[cfg(feature = "network-config")]
fn decode_wifi_network_summary(
    decoder: &mut Decoder<'_>,
) -> Result<WifiNetworkConfigSummary, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut profile_id = None;
    let mut enabled = None;
    let mut ssid = None;
    let mut credential_configured = None;
    let mut priority = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(profile_id.is_some(), RequiredField::WifiNetworkProfileId)?;
                profile_id = Some(decode_wifi_profile_id(decoder)?);
            }
            1 => {
                reject_duplicate(enabled.is_some(), RequiredField::WifiEnabled)?;
                enabled = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(ssid.is_some(), RequiredField::WifiSsid)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if bytes.is_empty() || bytes.len() > MAX_WIFI_SSID_BYTES {
                    return Err(DecodeError::InvalidWifiSsid);
                }
                ssid = Some(bytes);
            }
            3 => {
                reject_duplicate(
                    credential_configured.is_some(),
                    RequiredField::WifiCredential,
                )?;
                credential_configured = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(priority.is_some(), RequiredField::WifiPriority)?;
                priority = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    WifiNetworkConfigSummary::new(
        require(profile_id, RequiredField::WifiNetworkProfileId)?,
        require(enabled, RequiredField::WifiEnabled)?,
        require(priority, RequiredField::WifiPriority)?,
        require(ssid, RequiredField::WifiSsid)?,
        require(credential_configured, RequiredField::WifiCredential)?,
    )
    .map_err(|_| DecodeError::InvalidWifiSsid)
}

#[cfg(feature = "network-config")]
fn decode_tcp_peer_summary(
    decoder: &mut Decoder<'_>,
) -> Result<ReticulumTcpPeerConfigSummary, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut enabled = None;
    let mut ipv4_address = None;
    let mut port = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(enabled.is_some(), RequiredField::ReticulumTcpPeerEnabled)?;
                enabled = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    ipv4_address.is_some(),
                    RequiredField::ReticulumTcpPeerIpv4Address,
                )?;
                ipv4_address = Some(
                    ReticulumTcpPeerIpv4Address::new(decode_fixed_bytes::<4>(
                        decoder,
                        RequiredField::ReticulumTcpPeerIpv4Address,
                    )?)
                    .map_err(|_| DecodeError::InvalidReticulumTcpPeerIpv4Address)?,
                );
            }
            2 => {
                reject_duplicate(port.is_some(), RequiredField::ReticulumTcpPeerPort)?;
                port = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    ReticulumTcpPeerConfigSummary::new(
        require(enabled, RequiredField::ReticulumTcpPeerEnabled)?,
        require(ipv4_address, RequiredField::ReticulumTcpPeerIpv4Address)?,
        require(port, RequiredField::ReticulumTcpPeerPort)?,
    )
    .map_err(|_| DecodeError::InvalidReticulumTcpPeerPort)
}

#[cfg(feature = "network-config")]
fn decode_tcp_host_peer_summary(
    decoder: &mut Decoder<'_>,
) -> Result<ReticulumTcpPeerHostConfigSummary, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut enabled = None;
    let mut hostname = None;
    let mut port = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(enabled.is_some(), RequiredField::ReticulumTcpPeerEnabled)?;
                enabled = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(hostname.is_some(), RequiredField::ReticulumTcpPeerHostname)?;
                let value = decoder.str().map_err(|_| DecodeError::Malformed)?;
                ReticulumTcpPeerHostname::new(value)
                    .map_err(|_| DecodeError::InvalidReticulumTcpPeerHostname)?;
                hostname = Some(value);
            }
            2 => {
                reject_duplicate(port.is_some(), RequiredField::ReticulumTcpPeerPort)?;
                port = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    ReticulumTcpPeerHostConfigSummary::new(
        require(enabled, RequiredField::ReticulumTcpPeerEnabled)?,
        require(hostname, RequiredField::ReticulumTcpPeerHostname)?,
        require(port, RequiredField::ReticulumTcpPeerPort)?,
    )
    .map_err(|error| match error {
        crate::InvalidReticulumTcpPeerHostConfig::InvalidHostname(_) => {
            DecodeError::InvalidReticulumTcpPeerHostname
        }
        crate::InvalidReticulumTcpPeerHostConfig::InvalidPort => {
            DecodeError::InvalidReticulumTcpPeerPort
        }
    })
}

#[cfg(feature = "network-config")]
fn decode_network_config_mutation_outcome(
    body: &[u8],
) -> Result<NetworkConfigMutationOutcome, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut outcome = None;
    let mut revision = None;
    let mut reboot_required = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    outcome.is_some(),
                    RequiredField::NetworkConfigMutationOutcome,
                )?;
                outcome = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(revision.is_some(), RequiredField::NetworkConfigRevision)?;
                revision = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(
                    reboot_required.is_some(),
                    RequiredField::NetworkConfigRebootRequired,
                )?;
                reboot_required = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let revision = require(revision, RequiredField::NetworkConfigRevision)?;
    match require(outcome, RequiredField::NetworkConfigMutationOutcome)? {
        0 => Ok(NetworkConfigMutationOutcome::Applied {
            revision,
            reboot_required: require(reboot_required, RequiredField::NetworkConfigRebootRequired)?,
        }),
        1 if reboot_required.is_none() => Ok(NetworkConfigMutationOutcome::RevisionConflict {
            current_revision: revision,
        }),
        1 => Err(DecodeError::InvalidNetworkConfigMutationOutcome),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::NetworkConfigMutationOutcome,
            value: u64::from(other),
        }),
    }
}

fn decode_node_diagnostics(body: &[u8]) -> Result<NodeDiagnosticsSnapshot, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut uptime_ms = None;
    let mut interfaces = None;
    let mut lora_seen = false;
    let mut lora = None;
    let mut rns = None;
    let mut observed_peer_count = None;
    let mut retained_route_count = None;
    let mut usable_route_count = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(uptime_ms.is_some(), RequiredField::NodeDiagnosticsUptime)?;
                uptime_ms = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    interfaces.is_some(),
                    RequiredField::NodeDiagnosticsInterfaces,
                )?;
                interfaces = Some(decode_diagnostic_interfaces(&mut decoder)?);
            }
            2 => {
                reject_duplicate(lora_seen, RequiredField::NodeDiagnosticsLora)?;
                lora_seen = true;
                if matches!(
                    decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                    Type::Null
                ) {
                    decoder.null().map_err(|_| DecodeError::Malformed)?;
                } else {
                    lora = Some(decode_lora_diagnostics(&mut decoder)?);
                }
            }
            3 => {
                reject_duplicate(rns.is_some(), RequiredField::NodeDiagnosticsRns)?;
                rns = Some(decode_rns_diagnostics(&mut decoder)?);
            }
            4 => {
                reject_duplicate(
                    observed_peer_count.is_some(),
                    RequiredField::NodeDiagnosticsObservedPeers,
                )?;
                observed_peer_count = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(
                    retained_route_count.is_some(),
                    RequiredField::NodeDiagnosticsRetainedRoutes,
                )?;
                retained_route_count = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(
                    usable_route_count.is_some(),
                    RequiredField::NodeDiagnosticsUsableRoutes,
                )?;
                usable_route_count = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(NodeDiagnosticsSnapshot::new(
        require(uptime_ms, RequiredField::NodeDiagnosticsUptime)?,
        require(interfaces, RequiredField::NodeDiagnosticsInterfaces)?,
        lora,
        require(rns, RequiredField::NodeDiagnosticsRns)?,
        require(
            observed_peer_count,
            RequiredField::NodeDiagnosticsObservedPeers,
        )?,
        require(
            retained_route_count,
            RequiredField::NodeDiagnosticsRetainedRoutes,
        )?,
        require(
            usable_route_count,
            RequiredField::NodeDiagnosticsUsableRoutes,
        )?,
    ))
}

fn decode_diagnostic_interfaces(
    decoder: &mut Decoder<'_>,
) -> Result<[Option<DiagnosticInterfaceRecord>; MAX_DIAGNOSTIC_INTERFACES], DecodeError> {
    let entries = decode_exact_array_len(
        decoder,
        MAX_DIAGNOSTIC_INTERFACES,
        RequiredField::NodeDiagnosticsInterfaces,
    )?;
    let mut interfaces = [None; MAX_DIAGNOSTIC_INTERFACES];
    for interface in interfaces.iter_mut().take(entries) {
        if matches!(
            decoder.datatype().map_err(|_| DecodeError::Malformed)?,
            Type::Null
        ) {
            decoder.null().map_err(|_| DecodeError::Malformed)?;
        } else {
            *interface = Some(decode_diagnostic_interface(decoder)?);
        }
    }
    Ok(interfaces)
}

fn decode_diagnostic_interface(
    decoder: &mut Decoder<'_>,
) -> Result<DiagnosticInterfaceRecord, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut id = None;
    let mut kind = None;
    let mut state = None;
    let mut generation = None;
    let mut logical_mtu = None;
    let mut bitrate = None;
    let mut bitrate_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::DiagnosticInterfaceId)?;
                id = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(kind.is_some(), RequiredField::DiagnosticInterfaceKind)?;
                kind = Some(decode_diagnostic_interface_kind(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            2 => {
                reject_duplicate(state.is_some(), RequiredField::DiagnosticInterfaceState)?;
                state = Some(decode_diagnostic_interface_state(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            3 => {
                reject_duplicate(
                    generation.is_some(),
                    RequiredField::DiagnosticInterfaceGeneration,
                )?;
                generation = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(
                    logical_mtu.is_some(),
                    RequiredField::DiagnosticInterfaceLogicalMtu,
                )?;
                logical_mtu = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(bitrate_seen, RequiredField::DiagnosticInterfaceBitrate)?;
                bitrate_seen = true;
                bitrate = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    Ok(DiagnosticInterfaceRecord::new(
        require(id, RequiredField::DiagnosticInterfaceId)?,
        require(kind, RequiredField::DiagnosticInterfaceKind)?,
        require(state, RequiredField::DiagnosticInterfaceState)?,
        require(generation, RequiredField::DiagnosticInterfaceGeneration)?,
        require(logical_mtu, RequiredField::DiagnosticInterfaceLogicalMtu)?,
        bitrate,
    ))
}

fn decode_diagnostic_interface_kind(value: u8) -> Result<DiagnosticInterfaceKind, DecodeError> {
    match value {
        0 => Ok(DiagnosticInterfaceKind::LoRa),
        1 => Ok(DiagnosticInterfaceKind::Tcp),
        2 => Ok(DiagnosticInterfaceKind::Other),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::DiagnosticInterfaceKind,
            value: u64::from(other),
        }),
    }
}

fn decode_diagnostic_interface_state(value: u8) -> Result<DiagnosticInterfaceState, DecodeError> {
    match value {
        0 => Ok(DiagnosticInterfaceState::Offline),
        1 => Ok(DiagnosticInterfaceState::Online),
        2 => Ok(DiagnosticInterfaceState::Faulted),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::DiagnosticInterfaceState,
            value: u64::from(other),
        }),
    }
}

fn decode_lora_diagnostics(decoder: &mut Decoder<'_>) -> Result<LoraDiagnostics, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut applied_tx_power_dbm = None;
    let mut frequency_hz = None;
    let mut bandwidth_hz = None;
    let mut spreading_factor = None;
    let mut coding_rate_denominator = None;
    let mut rx_physical_frames = None;
    let mut rx_packets = None;
    let mut rx_errors = None;
    let mut rx_drops = None;
    let mut tx_terminal_jobs = None;
    let mut tx_successes = None;
    let mut tx_completed_frames = None;
    let mut tx_access_rejects = None;
    let mut tx_failures = None;
    let mut cad_busy = None;
    let mut cad_clear = None;
    let mut last_rx = None;
    let mut last_rx_seen = false;
    let mut last_tx = None;
    let mut last_tx_seen = false;
    let mut last_data_tx = None;
    let mut last_data_tx_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    applied_tx_power_dbm.is_some(),
                    RequiredField::DiagnosticLoraTxPower,
                )?;
                applied_tx_power_dbm = Some(decoder.i16().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    frequency_hz.is_some(),
                    RequiredField::DiagnosticLoraFrequency,
                )?;
                frequency_hz = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(
                    bandwidth_hz.is_some(),
                    RequiredField::DiagnosticLoraBandwidth,
                )?;
                bandwidth_hz = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(
                    spreading_factor.is_some(),
                    RequiredField::DiagnosticLoraSpreadingFactor,
                )?;
                spreading_factor = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(
                    coding_rate_denominator.is_some(),
                    RequiredField::DiagnosticLoraCodingRate,
                )?;
                coding_rate_denominator = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(
                    rx_physical_frames.is_some(),
                    RequiredField::DiagnosticLoraRxPhysicalFrames,
                )?;
                rx_physical_frames = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(rx_packets.is_some(), RequiredField::DiagnosticLoraRxPackets)?;
                rx_packets = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            7 => {
                reject_duplicate(rx_errors.is_some(), RequiredField::DiagnosticLoraRxErrors)?;
                rx_errors = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            8 => {
                reject_duplicate(rx_drops.is_some(), RequiredField::DiagnosticLoraRxDrops)?;
                rx_drops = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            9 => {
                reject_duplicate(
                    tx_terminal_jobs.is_some(),
                    RequiredField::DiagnosticLoraTxTerminalJobs,
                )?;
                tx_terminal_jobs = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            10 => {
                reject_duplicate(
                    tx_successes.is_some(),
                    RequiredField::DiagnosticLoraTxSuccesses,
                )?;
                tx_successes = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            11 => {
                reject_duplicate(
                    tx_completed_frames.is_some(),
                    RequiredField::DiagnosticLoraTxCompletedFrames,
                )?;
                tx_completed_frames = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            12 => {
                reject_duplicate(
                    tx_access_rejects.is_some(),
                    RequiredField::DiagnosticLoraTxAccessRejects,
                )?;
                tx_access_rejects = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            13 => {
                reject_duplicate(
                    tx_failures.is_some(),
                    RequiredField::DiagnosticLoraTxFailures,
                )?;
                tx_failures = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            14 => {
                reject_duplicate(cad_busy.is_some(), RequiredField::DiagnosticLoraCadBusy)?;
                cad_busy = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            15 => {
                reject_duplicate(cad_clear.is_some(), RequiredField::DiagnosticLoraCadClear)?;
                cad_clear = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            16 => {
                reject_duplicate(last_rx_seen, RequiredField::DiagnosticLoraLastRx)?;
                last_rx_seen = true;
                last_rx = Some(decode_diagnostic_lora_last_rx(decoder)?);
            }
            17 => {
                reject_duplicate(last_tx_seen, RequiredField::DiagnosticLoraLastTx)?;
                last_tx_seen = true;
                last_tx = Some(decode_diagnostic_lora_last_tx(decoder)?);
            }
            18 => {
                reject_duplicate(last_data_tx_seen, RequiredField::DiagnosticLoraLastDataTx)?;
                last_data_tx_seen = true;
                let decoded = decode_diagnostic_lora_last_tx(decoder)?;
                let Some(data) = decoded.data_evidence() else {
                    return Err(DecodeError::InvalidLoraLastTx);
                };
                if decoded.family() != Some(DiagnosticLoraTxFamily::Data) {
                    return Err(DecodeError::InvalidLoraLastTx);
                }
                last_data_tx = Some(DiagnosticLoraLastDataTx::new(
                    decoded.age_ms(),
                    decoded.outcome(),
                    data,
                ));
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    Ok(LoraDiagnostics::new(
        require(applied_tx_power_dbm, RequiredField::DiagnosticLoraTxPower)?,
        require(frequency_hz, RequiredField::DiagnosticLoraFrequency)?,
        require(bandwidth_hz, RequiredField::DiagnosticLoraBandwidth)?,
        require(
            spreading_factor,
            RequiredField::DiagnosticLoraSpreadingFactor,
        )?,
        require(
            coding_rate_denominator,
            RequiredField::DiagnosticLoraCodingRate,
        )?,
        require(
            rx_physical_frames,
            RequiredField::DiagnosticLoraRxPhysicalFrames,
        )?,
        require(rx_packets, RequiredField::DiagnosticLoraRxPackets)?,
        require(rx_errors, RequiredField::DiagnosticLoraRxErrors)?,
        require(rx_drops, RequiredField::DiagnosticLoraRxDrops)?,
        require(
            tx_terminal_jobs,
            RequiredField::DiagnosticLoraTxTerminalJobs,
        )?,
        require(tx_successes, RequiredField::DiagnosticLoraTxSuccesses)?,
        require(
            tx_completed_frames,
            RequiredField::DiagnosticLoraTxCompletedFrames,
        )?,
        require(
            tx_access_rejects,
            RequiredField::DiagnosticLoraTxAccessRejects,
        )?,
        require(tx_failures, RequiredField::DiagnosticLoraTxFailures)?,
        require(cad_busy, RequiredField::DiagnosticLoraCadBusy)?,
        require(cad_clear, RequiredField::DiagnosticLoraCadClear)?,
        last_rx,
        last_tx,
        last_data_tx,
    ))
}

fn decode_diagnostic_lora_last_rx(
    decoder: &mut Decoder<'_>,
) -> Result<DiagnosticLoraLastRx, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut age_ms = None;
    let mut rssi_dbm = None;
    let mut snr_db = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(age_ms.is_some(), RequiredField::DiagnosticLoraEventAge)?;
                age_ms = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(rssi_dbm.is_some(), RequiredField::DiagnosticLoraLastRxRssi)?;
                rssi_dbm = Some(decoder.i16().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(snr_db.is_some(), RequiredField::DiagnosticLoraLastRxSnr)?;
                snr_db = Some(decoder.i16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    Ok(DiagnosticLoraLastRx::new(
        require(age_ms, RequiredField::DiagnosticLoraEventAge)?,
        require(rssi_dbm, RequiredField::DiagnosticLoraLastRxRssi)?,
        require(snr_db, RequiredField::DiagnosticLoraLastRxSnr)?,
    ))
}

fn decode_diagnostic_lora_last_tx(
    decoder: &mut Decoder<'_>,
) -> Result<DiagnosticLoraLastTx, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut age_ms = None;
    let mut outcome = None;
    let mut family = None;
    let mut family_seen = false;
    let mut interface_id = None;
    let mut interface_id_seen = false;
    let mut packet_len = None;
    let mut packet_len_seen = false;
    let mut packet_sha256 = None;
    let mut packet_sha256_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(age_ms.is_some(), RequiredField::DiagnosticLoraEventAge)?;
                age_ms = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    outcome.is_some(),
                    RequiredField::DiagnosticLoraLastTxOutcome,
                )?;
                outcome = Some(decode_diagnostic_lora_tx_outcome(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            2 => {
                reject_duplicate(family_seen, RequiredField::DiagnosticLoraLastTxFamily)?;
                family_seen = true;
                family = Some(decode_diagnostic_lora_tx_family(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            3 => {
                reject_duplicate(
                    interface_id_seen,
                    RequiredField::DiagnosticLoraLastTxInterface,
                )?;
                interface_id_seen = true;
                interface_id = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(
                    packet_len_seen,
                    RequiredField::DiagnosticLoraLastTxPacketLength,
                )?;
                packet_len_seen = true;
                packet_len = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(
                    packet_sha256_seen,
                    RequiredField::DiagnosticLoraLastTxPacketSha256,
                )?;
                packet_sha256_seen = true;
                packet_sha256 = Some(decode_fixed_bytes::<32>(
                    decoder,
                    RequiredField::DiagnosticLoraLastTxPacketSha256,
                )?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    let data = match (interface_id, packet_len, packet_sha256) {
        (None, None, None) => None,
        (Some(interface_id), Some(packet_len), Some(packet_sha256)) => Some(
            DiagnosticLoraDataTxEvidence::try_new(
                interface_id,
                packet_len,
                EncodedPacketSha256::new(packet_sha256),
            )
            .ok_or(DecodeError::InvalidLoraLastTx)?,
        ),
        _ => return Err(DecodeError::InvalidLoraLastTx),
    };
    match (family, data) {
        (None, None) | (Some(DiagnosticLoraTxFamily::Ordinary), None) => {}
        (Some(DiagnosticLoraTxFamily::Data), Some(_)) => {}
        _ => return Err(DecodeError::InvalidLoraLastTx),
    }
    Ok(DiagnosticLoraLastTx::from_wire(
        require(age_ms, RequiredField::DiagnosticLoraEventAge)?,
        require(outcome, RequiredField::DiagnosticLoraLastTxOutcome)?,
        family,
        data,
    ))
}

fn decode_diagnostic_lora_tx_family(value: u8) -> Result<DiagnosticLoraTxFamily, DecodeError> {
    match value {
        0 => Ok(DiagnosticLoraTxFamily::Data),
        1 => Ok(DiagnosticLoraTxFamily::Ordinary),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::DiagnosticLoraLastTxFamily,
            value: u64::from(other),
        }),
    }
}

fn decode_diagnostic_lora_tx_outcome(value: u8) -> Result<DiagnosticLoraTxOutcome, DecodeError> {
    match value {
        0 => Ok(DiagnosticLoraTxOutcome::Completed),
        1 => Ok(DiagnosticLoraTxOutcome::AccessRejected),
        2 => Ok(DiagnosticLoraTxOutcome::Failed),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::DiagnosticLoraLastTxOutcome,
            value: u64::from(other),
        }),
    }
}

fn decode_rns_diagnostics(decoder: &mut Decoder<'_>) -> Result<RnsDiagnostics, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut received = None;
    let mut forwarded = None;
    let mut dedup_drops = None;
    let mut invalid_drops = None;
    let mut announces_received = None;
    let mut paths_learned = None;
    let mut paths_expired = None;
    let mut links_established = None;
    let mut links_closed = None;
    let mut links_failed = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(received.is_some(), RequiredField::DiagnosticRnsReceived)?;
                received = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(forwarded.is_some(), RequiredField::DiagnosticRnsForwarded)?;
                forwarded = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(
                    dedup_drops.is_some(),
                    RequiredField::DiagnosticRnsDedupDrops,
                )?;
                dedup_drops = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(
                    invalid_drops.is_some(),
                    RequiredField::DiagnosticRnsInvalidDrops,
                )?;
                invalid_drops = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(
                    announces_received.is_some(),
                    RequiredField::DiagnosticRnsAnnouncesReceived,
                )?;
                announces_received = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(
                    paths_learned.is_some(),
                    RequiredField::DiagnosticRnsPathsLearned,
                )?;
                paths_learned = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(
                    paths_expired.is_some(),
                    RequiredField::DiagnosticRnsPathsExpired,
                )?;
                paths_expired = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            7 => {
                reject_duplicate(
                    links_established.is_some(),
                    RequiredField::DiagnosticRnsLinksEstablished,
                )?;
                links_established = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            8 => {
                reject_duplicate(
                    links_closed.is_some(),
                    RequiredField::DiagnosticRnsLinksClosed,
                )?;
                links_closed = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            9 => {
                reject_duplicate(
                    links_failed.is_some(),
                    RequiredField::DiagnosticRnsLinksFailed,
                )?;
                links_failed = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    Ok(RnsDiagnostics::new(
        require(received, RequiredField::DiagnosticRnsReceived)?,
        require(forwarded, RequiredField::DiagnosticRnsForwarded)?,
        require(dedup_drops, RequiredField::DiagnosticRnsDedupDrops)?,
        require(invalid_drops, RequiredField::DiagnosticRnsInvalidDrops)?,
        require(
            announces_received,
            RequiredField::DiagnosticRnsAnnouncesReceived,
        )?,
        require(paths_learned, RequiredField::DiagnosticRnsPathsLearned)?,
        require(paths_expired, RequiredField::DiagnosticRnsPathsExpired)?,
        require(
            links_established,
            RequiredField::DiagnosticRnsLinksEstablished,
        )?,
        require(links_closed, RequiredField::DiagnosticRnsLinksClosed)?,
        require(links_failed, RequiredField::DiagnosticRnsLinksFailed)?,
    ))
}

fn decode_route_diagnostics_page(body: &[u8]) -> Result<RouteDiagnosticsPage, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut revision = None;
    let mut total_count = None;
    let mut route_entries = None;
    let mut next_cursor = None;
    let mut next_cursor_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(revision.is_some(), RequiredField::RouteDiagnosticsRevision)?;
                revision = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    total_count.is_some(),
                    RequiredField::RouteDiagnosticsTotalCount,
                )?;
                total_count = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(
                    route_entries.is_some(),
                    RequiredField::RouteDiagnosticsEntries,
                )?;
                route_entries = Some(decode_route_diagnostic_entries(&mut decoder)?);
            }
            3 => {
                reject_duplicate(next_cursor_seen, RequiredField::RouteDiagnosticsNextCursor)?;
                next_cursor_seen = true;
                next_cursor = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::RouteDiagnosticsNextCursor,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    RouteDiagnosticsPage::new(
        require(revision, RequiredField::RouteDiagnosticsRevision)?,
        require(total_count, RequiredField::RouteDiagnosticsTotalCount)?,
        require(route_entries, RequiredField::RouteDiagnosticsEntries)?,
        next_cursor,
    )
    .map_err(|_| DecodeError::InvalidRouteDiagnosticsPage)
}

fn decode_route_diagnostic_entries(
    decoder: &mut Decoder<'_>,
) -> Result<[Option<RouteDiagnosticEntry>; MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES], DecodeError> {
    let entries = decode_exact_array_len(
        decoder,
        MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES,
        RequiredField::RouteDiagnosticsEntries,
    )?;
    let mut routes = [None; MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES];
    for route in routes.iter_mut().take(entries) {
        if matches!(
            decoder.datatype().map_err(|_| DecodeError::Malformed)?,
            Type::Null
        ) {
            decoder.null().map_err(|_| DecodeError::Malformed)?;
        } else {
            *route = Some(decode_route_diagnostic_entry(decoder)?);
        }
    }
    Ok(routes)
}

fn decode_route_diagnostic_entry(
    decoder: &mut Decoder<'_>,
) -> Result<RouteDiagnosticEntry, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut destination = None;
    let mut next_hop_identity = None;
    let mut next_hop_seen = false;
    let mut hops = None;
    let mut retained_interface = None;
    let mut retained_interface_seen = false;
    let mut resolution = None;
    let mut learned_age_ms = None;
    let mut learned_age_seen = false;
    let mut last_used_age_ms = None;
    let mut last_used_age_seen = false;
    let mut expires_in_ms = None;
    let mut expires_in_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    destination.is_some(),
                    RequiredField::RouteDiagnosticDestination,
                )?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    decoder,
                    RequiredField::RouteDiagnosticDestination,
                )?));
            }
            1 => {
                reject_duplicate(next_hop_seen, RequiredField::RouteDiagnosticNextHop)?;
                next_hop_seen = true;
                next_hop_identity = Some(IdentityHash::new(decode_fixed_bytes::<16>(
                    decoder,
                    RequiredField::RouteDiagnosticNextHop,
                )?));
            }
            2 => {
                reject_duplicate(hops.is_some(), RequiredField::RouteDiagnosticHops)?;
                hops = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(
                    retained_interface_seen,
                    RequiredField::RouteDiagnosticInterface,
                )?;
                retained_interface_seen = true;
                retained_interface = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(
                    resolution.is_some(),
                    RequiredField::RouteDiagnosticResolution,
                )?;
                resolution = Some(decode_route_diagnostic_resolution(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            5 => {
                reject_duplicate(learned_age_seen, RequiredField::RouteDiagnosticLearnedAge)?;
                learned_age_seen = true;
                learned_age_ms = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(
                    last_used_age_seen,
                    RequiredField::RouteDiagnosticLastUsedAge,
                )?;
                last_used_age_seen = true;
                last_used_age_ms = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            7 => {
                reject_duplicate(expires_in_seen, RequiredField::RouteDiagnosticExpiresIn)?;
                expires_in_seen = true;
                expires_in_ms = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    Ok(RouteDiagnosticEntry::new(
        require(destination, RequiredField::RouteDiagnosticDestination)?,
        next_hop_identity,
        require(hops, RequiredField::RouteDiagnosticHops)?,
        retained_interface,
        require(resolution, RequiredField::RouteDiagnosticResolution)?,
        learned_age_ms,
        last_used_age_ms,
        expires_in_ms,
    ))
}

fn decode_route_diagnostic_resolution(value: u8) -> Result<RouteDiagnosticResolution, DecodeError> {
    match value {
        0 => Ok(RouteDiagnosticResolution::ExactReady),
        1 => Ok(RouteDiagnosticResolution::ExactOffline),
        2 => Ok(RouteDiagnosticResolution::ExactMissing),
        3 => Ok(RouteDiagnosticResolution::BroadcastReady),
        4 => Ok(RouteDiagnosticResolution::BroadcastUnavailable),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::RouteDiagnosticResolution,
            value: u64::from(other),
        }),
    }
}

fn decode_radio_trace_cursor_compact(
    decoder: &mut Decoder<'_>,
) -> Result<RadioTraceCursor, DecodeError> {
    decode_exact_array_len(decoder, 2, RequiredField::RadioTraceAfterCursor)?;
    let boot_id = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    let after_sequence = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    Ok(RadioTraceCursor::new(boot_id, after_sequence))
}

fn decode_radio_trace_page_compact(body: &[u8]) -> Result<RadioTracePage, DecodeError> {
    let mut decoder = Decoder::new(body);
    decode_exact_array_len(&mut decoder, 7, RequiredField::RadioTraceEntries)?;
    let boot_id = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    let applied_profile = decode_radio_trace_applied_profile_compact(&mut decoder)?;
    let oldest_sequence = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    let next_sequence = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    let history_lost = decoder.bool().map_err(|_| DecodeError::Malformed)?;
    let trace_entries = decode_radio_trace_entries_compact(&mut decoder)?;
    let next_cursor = if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        None
    } else {
        Some(decode_radio_trace_cursor_compact(&mut decoder)?)
    };
    finish_body(&decoder, body)?;
    RadioTracePage::new(
        boot_id,
        applied_profile,
        oldest_sequence,
        next_sequence,
        history_lost,
        trace_entries,
        next_cursor,
    )
    .map_err(|_| DecodeError::InvalidRadioTracePage)
}

fn decode_radio_trace_applied_profile_compact(
    decoder: &mut Decoder<'_>,
) -> Result<RadioTraceAppliedLoraProfile, DecodeError> {
    decode_exact_array_len(decoder, 10, RequiredField::RadioTraceAppliedLoraProfile)?;
    Ok(RadioTraceAppliedLoraProfile::new(
        decode_fixed_bytes::<16>(decoder, RequiredField::RadioTraceProfileFingerprint)?,
        decoder.u32().map_err(|_| DecodeError::Malformed)?,
        decoder.u32().map_err(|_| DecodeError::Malformed)?,
        decoder.u16().map_err(|_| DecodeError::Malformed)?,
        decoder.i16().map_err(|_| DecodeError::Malformed)?,
        decoder.u8().map_err(|_| DecodeError::Malformed)?,
        decoder.u8().map_err(|_| DecodeError::Malformed)?,
        decoder.bool().map_err(|_| DecodeError::Malformed)?,
        decoder.bool().map_err(|_| DecodeError::Malformed)?,
        decoder.bool().map_err(|_| DecodeError::Malformed)?,
    ))
}

fn decode_radio_trace_entries_compact(
    decoder: &mut Decoder<'_>,
) -> Result<[Option<RadioTraceEvent>; MAX_RADIO_TRACE_PAGE_ENTRIES], DecodeError> {
    decode_exact_array_len(
        decoder,
        MAX_RADIO_TRACE_PAGE_ENTRIES,
        RequiredField::RadioTraceEntries,
    )?;
    let mut events = [None; MAX_RADIO_TRACE_PAGE_ENTRIES];
    for event in &mut events {
        if matches!(
            decoder.datatype().map_err(|_| DecodeError::Malformed)?,
            Type::Null
        ) {
            decoder.null().map_err(|_| DecodeError::Malformed)?;
        } else {
            *event = Some(decode_radio_trace_event_compact(decoder)?);
        }
    }
    Ok(events)
}

fn decode_radio_trace_event_compact(
    decoder: &mut Decoder<'_>,
) -> Result<RadioTraceEvent, DecodeError> {
    decode_exact_array_len(decoder, 4, RequiredField::RadioTraceEventValue)?;
    let sequence = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    let observed_at_us = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    let kind_code = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    let kind = match kind_code {
        0 => RadioTraceEventKind::DataTx(decode_radio_trace_data_tx_compact(decoder)?),
        1 => RadioTraceEventKind::LogicalRx(decode_radio_trace_logical_rx_compact(decoder)?),
        2 => {
            RadioTraceEventKind::RouteSelected(decode_radio_trace_route_selected_compact(decoder)?)
        }
        3 => RadioTraceEventKind::AttemptTerminal(decode_radio_trace_attempt_terminal_compact(
            decoder,
        )?),
        4 => RadioTraceEventKind::InboundProof(decode_radio_trace_inbound_proof_compact(decoder)?),
        other => {
            return Err(DecodeError::InvalidValue {
                field: RequiredField::RadioTraceEventKind,
                value: u64::from(other),
            });
        }
    };
    Ok(RadioTraceEvent::new(sequence, observed_at_us, kind))
}

fn decode_radio_trace_packet_evidence_compact(
    decoder: &mut Decoder<'_>,
) -> Result<RadioTracePacketEvidence, DecodeError> {
    let interface_id = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    let packet_len = decoder.u16().map_err(|_| DecodeError::Malformed)?;
    let sha256 = EncodedPacketSha256::new(decode_fixed_bytes::<32>(
        decoder,
        RequiredField::RadioTracePacketSha256,
    )?);
    let attempt_token = if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        None
    } else {
        Some(RadioTraceAttemptToken::new(decode_fixed_bytes::<32>(
            decoder,
            RequiredField::RadioTracePacketAttemptToken,
        )?))
    };
    RadioTracePacketEvidence::try_new(interface_id, packet_len, sha256, attempt_token)
        .ok_or(DecodeError::InvalidRadioTraceDataTx)
}

fn decode_radio_trace_data_tx_compact(
    decoder: &mut Decoder<'_>,
) -> Result<RadioTraceDataTx, DecodeError> {
    decode_exact_array_len(decoder, 9, RequiredField::RadioTraceEventValue)?;
    let packet = decode_radio_trace_packet_evidence_compact(decoder)?;
    let outcome = decode_radio_trace_tx_outcome(decoder.u8().map_err(|_| DecodeError::Malformed)?)?;
    let planned_frames = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    let completed_frames = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    let authorization_observed = decoder.bool().map_err(|_| DecodeError::Malformed)?;
    let frame_completed_at_us = decode_radio_trace_frame_timestamps(decoder)?;
    RadioTraceDataTx::try_new(
        packet,
        outcome,
        planned_frames,
        completed_frames,
        authorization_observed,
        frame_completed_at_us,
    )
    .map_err(|_| DecodeError::InvalidRadioTraceDataTx)
}

fn decode_radio_trace_logical_rx_compact(
    decoder: &mut Decoder<'_>,
) -> Result<RadioTraceLogicalRx, DecodeError> {
    decode_exact_array_len(decoder, 6, RequiredField::RadioTraceEventValue)?;
    let packet = decode_radio_trace_packet_evidence_compact(decoder)?;
    let rssi = decoder.i16().map_err(|_| DecodeError::Malformed)?;
    let snr = decoder.i16().map_err(|_| DecodeError::Malformed)?;
    Ok(RadioTraceLogicalRx::new(packet, rssi, snr))
}

fn decode_radio_trace_route_selected_compact(
    decoder: &mut Decoder<'_>,
) -> Result<RadioTraceRouteSelected, DecodeError> {
    decode_exact_array_len(decoder, 9, RequiredField::RadioTraceEventValue)?;
    let packet = decode_radio_trace_packet_evidence_compact(decoder)?;
    let destination = DestinationHash(decode_fixed_bytes::<16>(
        decoder,
        RequiredField::RadioTraceRouteDestination,
    )?);
    let next_hop = if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        None
    } else {
        Some(IdentityHash::new(decode_fixed_bytes::<16>(
            decoder,
            RequiredField::RadioTraceRouteNextHop,
        )?))
    };
    let hops = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    let resolution =
        decode_radio_trace_route_resolution(decoder.u8().map_err(|_| DecodeError::Malformed)?)?;
    let submission_id = SubmissionId(decoder.u64().map_err(|_| DecodeError::Malformed)?);
    RadioTraceRouteSelected::try_new(
        submission_id,
        destination,
        next_hop,
        hops,
        resolution,
        packet,
    )
    .map_err(|_| DecodeError::InvalidRadioTraceRouteSelected)
}

fn decode_radio_trace_attempt_terminal_compact(
    decoder: &mut Decoder<'_>,
) -> Result<RadioTraceAttemptTerminal, DecodeError> {
    decode_exact_array_len(decoder, 3, RequiredField::RadioTraceEventValue)?;
    let attempt_token = RadioTraceAttemptToken::new(decode_fixed_bytes::<32>(
        decoder,
        RequiredField::RadioTraceTerminalAttemptToken,
    )?);
    let outcome =
        decode_radio_trace_attempt_outcome(decoder.u8().map_err(|_| DecodeError::Malformed)?)?;
    let proof_ingress = if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        None
    } else {
        Some(decode_ingress_observation(
            decoder,
            DecodeError::InvalidRadioTraceAttemptTerminal,
        )?)
    };
    Ok(RadioTraceAttemptTerminal::new(
        attempt_token,
        outcome,
        proof_ingress,
    ))
}

fn decode_radio_trace_inbound_proof_compact(
    decoder: &mut Decoder<'_>,
) -> Result<RadioTraceInboundProof, DecodeError> {
    decode_exact_array_len(decoder, 8, RequiredField::RadioTraceEventValue)?;
    let correlation_token = RadioTraceAttemptToken::new(decode_fixed_bytes::<32>(
        decoder,
        RequiredField::RadioTraceInboundProofCorrelationToken,
    )?);
    let stage =
        decode_radio_trace_inbound_proof_stage(decoder.u8().map_err(|_| DecodeError::Malformed)?)?;
    let message_id =
        decode_optional_fixed_bytes::<32>(decoder, RequiredField::RadioTraceInboundProofMessageId)?;
    let packet_sha256 = decode_optional_fixed_bytes::<32>(
        decoder,
        RequiredField::RadioTraceInboundProofPacketSha256,
    )?;
    let packet_len = if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        None
    } else {
        Some(decoder.u16().map_err(|_| DecodeError::Malformed)?)
    };
    let packet = match (packet_sha256, packet_len) {
        (None, None) => None,
        (Some(sha256), Some(packet_len)) => {
            RadioTraceInboundProofPacket::try_new(packet_len, EncodedPacketSha256::new(sha256))
        }
        _ => return Err(DecodeError::InvalidRadioTraceInboundProof),
    };
    let interface_id = if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        None
    } else {
        Some(decoder.u8().map_err(|_| DecodeError::Malformed)?)
    };
    let signal = if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        None
    } else {
        decode_exact_array_len(decoder, 2, RequiredField::RadioTraceInboundProofSignal)?;
        Some(IngressSignal::new(
            decoder.i16().map_err(|_| DecodeError::Malformed)?,
            decoder.i16().map_err(|_| DecodeError::Malformed)?,
        ))
    };
    let dispatch_outcome = if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        None
    } else {
        Some(decode_radio_trace_tx_outcome(
            decoder.u8().map_err(|_| DecodeError::Malformed)?,
        )?)
    };
    RadioTraceInboundProof::try_new(
        correlation_token,
        stage,
        message_id,
        packet,
        interface_id,
        signal,
        dispatch_outcome,
    )
    .map_err(|_| DecodeError::InvalidRadioTraceInboundProof)
}

fn decode_radio_trace_inbound_proof_stage(
    value: u8,
) -> Result<RadioTraceInboundProofStage, DecodeError> {
    match value {
        0 => Ok(RadioTraceInboundProofStage::DataLogicalRx),
        1 => Ok(RadioTraceInboundProofStage::DurableCommit),
        2 => Ok(RadioTraceInboundProofStage::ProofRetained),
        3 => Ok(RadioTraceInboundProofStage::ProofStaged),
        4 => Ok(RadioTraceInboundProofStage::OrdinaryQueued),
        5 => Ok(RadioTraceInboundProofStage::PhysicalTxDone),
        6 => Ok(RadioTraceInboundProofStage::PhysicalTxFailed),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::RadioTraceInboundProofStage,
            value: u64::from(other),
        }),
    }
}

fn decode_radio_trace_tx_outcome(value: u8) -> Result<RadioTraceTxOutcome, DecodeError> {
    let outcome = match value {
        0 => RadioTraceTxOutcome::Transmitted,
        1 => RadioTraceTxOutcome::AccessRejected,
        2 => RadioTraceTxOutcome::PermitDenied,
        3 => RadioTraceTxOutcome::AuthorizationExpired,
        4 => RadioTraceTxOutcome::PostGrantAccessRejected,
        5 => RadioTraceTxOutcome::AirtimeRejected,
        6 => RadioTraceTxOutcome::DeadlineConversionOverflow,
        7 => RadioTraceTxOutcome::RadioInactive,
        8 => RadioTraceTxOutcome::InterfaceConfigurationMismatch,
        9 => RadioTraceTxOutcome::RadioConfigurationChangedBeforePermit,
        10 => RadioTraceTxOutcome::RadioConfigurationChangedAfterPermit,
        11 => RadioTraceTxOutcome::CadFault,
        12 => RadioTraceTxOutcome::TxFault,
        13 => RadioTraceTxOutcome::ControlPlaneRecovery,
        14 => RadioTraceTxOutcome::FrameInvariantRecovery,
        15 => RadioTraceTxOutcome::CancelledRadioOperation,
        other => {
            return Err(DecodeError::InvalidValue {
                field: RequiredField::RadioTraceTxOutcome,
                value: u64::from(other),
            });
        }
    };
    Ok(outcome)
}

fn decode_radio_trace_frame_timestamps(
    decoder: &mut Decoder<'_>,
) -> Result<[Option<u64>; 2], DecodeError> {
    decode_exact_array_len(decoder, 2, RequiredField::RadioTraceTxFrameCompletedAt)?;
    let mut timestamps = [None; 2];
    for timestamp in &mut timestamps {
        if matches!(
            decoder.datatype().map_err(|_| DecodeError::Malformed)?,
            Type::Null
        ) {
            decoder.null().map_err(|_| DecodeError::Malformed)?;
        } else {
            *timestamp = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
        }
    }
    Ok(timestamps)
}

fn decode_radio_trace_route_resolution(
    value: u8,
) -> Result<RouteDiagnosticResolution, DecodeError> {
    match value {
        0 => Ok(RouteDiagnosticResolution::ExactReady),
        1 => Ok(RouteDiagnosticResolution::ExactOffline),
        2 => Ok(RouteDiagnosticResolution::ExactMissing),
        3 => Ok(RouteDiagnosticResolution::BroadcastReady),
        4 => Ok(RouteDiagnosticResolution::BroadcastUnavailable),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::RadioTraceRouteResolution,
            value: u64::from(other),
        }),
    }
}

fn decode_radio_trace_attempt_outcome(value: u8) -> Result<RadioTraceAttemptOutcome, DecodeError> {
    match value {
        0 => Ok(RadioTraceAttemptOutcome::Delivered),
        1 => Ok(RadioTraceAttemptOutcome::DeliveryTimeout),
        2 => Ok(RadioTraceAttemptOutcome::Unsent),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::RadioTraceTerminalOutcome,
            value: u64::from(other),
        }),
    }
}

fn decode_manual_service_announce_disposition(
    body: &[u8],
) -> Result<ManualServiceAnnounceDisposition, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut disposition = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    disposition.is_some(),
                    RequiredField::ManualServiceAnnounceDisposition,
                )?;
                disposition = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    match require(disposition, RequiredField::ManualServiceAnnounceDisposition)? {
        0 => Ok(ManualServiceAnnounceDisposition::Queued),
        1 => Ok(ManualServiceAnnounceDisposition::AlreadyPending),
        value => Err(DecodeError::InvalidValue {
            field: RequiredField::ManualServiceAnnounceDisposition,
            value: u64::from(value),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_network_status(body: &[u8]) -> Result<NetworkRuntimeStatus, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut configured_revision = None;
    let mut applied_revision = None;
    let mut wifi_state = None;
    let mut active_wifi_profile_seen = false;
    let mut active_wifi_profile = None;
    let mut connected_ssid_seen = false;
    let mut connected_ssid = None;
    let mut ipv4_seen = false;
    let mut ipv4_address = None;
    let mut rssi_seen = false;
    let mut rssi_dbm = None;
    let mut tcp_peer_state = None;
    let mut last_tcp_failure_seen = false;
    let mut last_tcp_failure = None;
    let mut dns_diagnostics_seen = false;
    let mut dns_diagnostics = None;
    let mut rmap_status_seen = false;
    let mut rmap_status = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    configured_revision.is_some(),
                    RequiredField::NetworkConfigRevision,
                )?;
                configured_revision = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(
                    applied_revision.is_some(),
                    RequiredField::NetworkAppliedRevision,
                )?;
                applied_revision = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(wifi_state.is_some(), RequiredField::NetworkWifiState)?;
                wifi_state = Some(decode_wifi_station_state(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            3 => {
                reject_duplicate(
                    active_wifi_profile_seen,
                    RequiredField::NetworkActiveWifiProfile,
                )?;
                active_wifi_profile_seen = true;
                if matches!(
                    decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                    Type::Null
                ) {
                    decoder.null().map_err(|_| DecodeError::Malformed)?;
                } else {
                    active_wifi_profile = Some(decode_wifi_profile_id(&mut decoder)?);
                }
            }
            4 => {
                reject_duplicate(connected_ssid_seen, RequiredField::NetworkConnectedSsid)?;
                connected_ssid_seen = true;
                if matches!(
                    decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                    Type::Null
                ) {
                    decoder.null().map_err(|_| DecodeError::Malformed)?;
                } else {
                    let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                    if bytes.is_empty() || bytes.len() > MAX_WIFI_SSID_BYTES {
                        return Err(DecodeError::InvalidWifiSsid);
                    }
                    connected_ssid = Some(bytes);
                }
            }
            5 => {
                reject_duplicate(ipv4_seen, RequiredField::NetworkIpv4Address)?;
                ipv4_seen = true;
                if matches!(
                    decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                    Type::Null
                ) {
                    decoder.null().map_err(|_| DecodeError::Malformed)?;
                } else {
                    ipv4_address = Some(decode_fixed_bytes::<4>(
                        &mut decoder,
                        RequiredField::NetworkIpv4Address,
                    )?);
                }
            }
            6 => {
                reject_duplicate(rssi_seen, RequiredField::NetworkRssiDbm)?;
                rssi_seen = true;
                if matches!(
                    decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                    Type::Null
                ) {
                    decoder.null().map_err(|_| DecodeError::Malformed)?;
                } else {
                    rssi_dbm = Some(decoder.i16().map_err(|_| DecodeError::Malformed)?);
                }
            }
            7 => {
                reject_duplicate(tcp_peer_state.is_some(), RequiredField::NetworkTcpPeerState)?;
                tcp_peer_state = Some(decode_tcp_peer_state(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            8 => {
                reject_duplicate(last_tcp_failure_seen, RequiredField::NetworkLastTcpFailure)?;
                last_tcp_failure_seen = true;
                last_tcp_failure = Some(decode_tcp_failure(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            9 => {
                reject_duplicate(dns_diagnostics_seen, RequiredField::NetworkDnsDiagnostics)?;
                dns_diagnostics_seen = true;
                dns_diagnostics = Some(decode_reticulum_dns_diagnostics(&mut decoder)?);
            }
            10 => {
                reject_duplicate(rmap_status_seen, RequiredField::NetworkRmapStatus)?;
                rmap_status_seen = true;
                rmap_status = Some(decode_rmap_runtime_status(&mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    if !active_wifi_profile_seen {
        return Err(DecodeError::MissingField(
            RequiredField::NetworkActiveWifiProfile,
        ));
    }
    if !connected_ssid_seen {
        return Err(DecodeError::MissingField(
            RequiredField::NetworkConnectedSsid,
        ));
    }
    if !ipv4_seen {
        return Err(DecodeError::MissingField(RequiredField::NetworkIpv4Address));
    }
    if !rssi_seen {
        return Err(DecodeError::MissingField(RequiredField::NetworkRssiDbm));
    }
    let status = NetworkRuntimeStatus::new_with_tcp_diagnostics(
        require(configured_revision, RequiredField::NetworkConfigRevision)?,
        require(applied_revision, RequiredField::NetworkAppliedRevision)?,
        require(wifi_state, RequiredField::NetworkWifiState)?,
        active_wifi_profile,
        connected_ssid,
        ipv4_address,
        rssi_dbm,
        require(tcp_peer_state, RequiredField::NetworkTcpPeerState)?,
        last_tcp_failure,
        dns_diagnostics,
    )
    .map_err(|_| DecodeError::InvalidWifiSsid)?;
    Ok(match rmap_status {
        Some(rmap) => status.with_rmap_status(rmap),
        None => status,
    })
}

#[cfg(feature = "network-config")]
fn decode_rmap_runtime_status(decoder: &mut Decoder<'_>) -> Result<RmapRuntimeStatus, DecodeError> {
    decode_exact_array_len(decoder, 10, RequiredField::RmapRuntimeStatus)?;
    let config_applied = decoder.bool().map_err(|_| DecodeError::Malformed)?;
    let stamp_phase = decode_rmap_stamp_phase(decoder.u8().map_err(|_| DecodeError::Malformed)?)?;
    let stamp_attempts = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    let initial_tcp_gate =
        decode_rmap_initial_tcp_gate(decoder.u8().map_err(|_| DecodeError::Malformed)?)?;
    let queued_count = decoder.u32().map_err(|_| DecodeError::Malformed)?;
    let last_queue_outcome =
        decode_rmap_queue_outcome(decoder.u8().map_err(|_| DecodeError::Malformed)?)?;
    let last_queue_attempt_at_uptime_seconds =
        decode_optional_u64(decoder, RequiredField::RmapRuntimeLastQueueAt)?;
    let egress_confirmation =
        decode_rmap_egress_confirmation(decoder.u8().map_err(|_| DecodeError::Malformed)?)?;
    let next_due_in_seconds = decode_optional_u64(decoder, RequiredField::RmapRuntimeNextDue)?;
    let deferred_reason = if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        None
    } else {
        Some(decode_rmap_deferred_reason(
            decoder.u8().map_err(|_| DecodeError::Malformed)?,
        )?)
    };
    Ok(RmapRuntimeStatus::new(
        config_applied,
        stamp_phase,
        stamp_attempts,
        initial_tcp_gate,
        queued_count,
        last_queue_outcome,
        last_queue_attempt_at_uptime_seconds,
        egress_confirmation,
        next_due_in_seconds,
        deferred_reason,
    ))
}

#[cfg(feature = "network-config")]
fn decode_reticulum_dns_diagnostics(
    decoder: &mut Decoder<'_>,
) -> Result<ReticulumDnsDiagnostics, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut gateway_seen = false;
    let mut gateway_ipv4 = None;
    let mut dhcp_servers = None;
    let mut primary_outcome = None;
    let mut raw_setup_state = None;
    let mut raw_attempts = None;
    let mut resolution_seen = false;
    let mut resolution = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(gateway_seen, RequiredField::ReticulumDnsGatewayIpv4)?;
                gateway_seen = true;
                gateway_ipv4 =
                    decode_optional_ipv4(decoder, RequiredField::ReticulumDnsGatewayIpv4)?;
            }
            1 => {
                reject_duplicate(
                    dhcp_servers.is_some(),
                    RequiredField::ReticulumDnsDhcpServers,
                )?;
                dhcp_servers = Some(decode_reticulum_dns_dhcp_servers(decoder)?);
            }
            2 => {
                reject_duplicate(
                    primary_outcome.is_some(),
                    RequiredField::ReticulumDnsPrimaryOutcome,
                )?;
                primary_outcome = Some(decode_reticulum_dns_primary_outcome(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            3 => {
                reject_duplicate(
                    raw_setup_state.is_some(),
                    RequiredField::ReticulumDnsRawSetupState,
                )?;
                raw_setup_state = Some(decode_reticulum_dns_raw_setup_state(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            4 => {
                reject_duplicate(
                    raw_attempts.is_some(),
                    RequiredField::ReticulumDnsRawAttempts,
                )?;
                raw_attempts = Some(decode_reticulum_dns_raw_attempts(decoder)?);
            }
            5 => {
                reject_duplicate(resolution_seen, RequiredField::ReticulumDnsResolution)?;
                resolution_seen = true;
                if matches!(
                    decoder.datatype().map_err(|_| DecodeError::Malformed)?,
                    Type::Null
                ) {
                    decoder.null().map_err(|_| DecodeError::Malformed)?;
                } else {
                    resolution = Some(decode_reticulum_dns_resolution(decoder)?);
                }
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    if !gateway_seen {
        return Err(DecodeError::MissingField(
            RequiredField::ReticulumDnsGatewayIpv4,
        ));
    }
    if !resolution_seen {
        return Err(DecodeError::MissingField(
            RequiredField::ReticulumDnsResolution,
        ));
    }
    Ok(ReticulumDnsDiagnostics::new(
        gateway_ipv4,
        require(dhcp_servers, RequiredField::ReticulumDnsDhcpServers)?,
        require(primary_outcome, RequiredField::ReticulumDnsPrimaryOutcome)?,
        require(raw_setup_state, RequiredField::ReticulumDnsRawSetupState)?,
        require(raw_attempts, RequiredField::ReticulumDnsRawAttempts)?,
        resolution,
    ))
}

#[cfg(feature = "network-config")]
fn decode_reticulum_dns_dhcp_servers(
    decoder: &mut Decoder<'_>,
) -> Result<[Option<[u8; 4]>; MAX_RETICULUM_DNS_DHCP_SERVERS], DecodeError> {
    let entries = decode_exact_array_len(
        decoder,
        MAX_RETICULUM_DNS_DHCP_SERVERS,
        RequiredField::ReticulumDnsDhcpServers,
    )?;
    let mut servers = [None; MAX_RETICULUM_DNS_DHCP_SERVERS];
    for server in servers.iter_mut().take(entries) {
        *server = decode_optional_ipv4(decoder, RequiredField::ReticulumDnsDhcpServers)?;
    }
    Ok(servers)
}

#[cfg(feature = "network-config")]
fn decode_reticulum_dns_raw_attempts(
    decoder: &mut Decoder<'_>,
) -> Result<[Option<ReticulumDnsRawAttempt>; MAX_RETICULUM_DNS_RAW_ATTEMPTS], DecodeError> {
    let entries = decode_exact_array_len(
        decoder,
        MAX_RETICULUM_DNS_RAW_ATTEMPTS,
        RequiredField::ReticulumDnsRawAttempts,
    )?;
    let mut attempts = [None; MAX_RETICULUM_DNS_RAW_ATTEMPTS];
    for attempt in attempts.iter_mut().take(entries) {
        if matches!(
            decoder.datatype().map_err(|_| DecodeError::Malformed)?,
            Type::Null
        ) {
            decoder.null().map_err(|_| DecodeError::Malformed)?;
        } else {
            *attempt = Some(decode_reticulum_dns_raw_attempt(decoder)?);
        }
    }
    Ok(attempts)
}

#[cfg(feature = "network-config")]
fn decode_reticulum_dns_raw_attempt(
    decoder: &mut Decoder<'_>,
) -> Result<ReticulumDnsRawAttempt, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut source = None;
    let mut server = None;
    let mut outcome_code = None;
    let mut response_code_seen = false;
    let mut response_code = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(source.is_some(), RequiredField::ReticulumDnsRawSource)?;
                source = Some(decode_reticulum_dns_raw_source(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            1 => {
                reject_duplicate(server.is_some(), RequiredField::ReticulumDnsRawServer)?;
                server = Some(decode_fixed_bytes::<4>(
                    decoder,
                    RequiredField::ReticulumDnsRawServer,
                )?);
            }
            2 => {
                reject_duplicate(
                    outcome_code.is_some(),
                    RequiredField::ReticulumDnsRawOutcome,
                )?;
                outcome_code = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(response_code_seen, RequiredField::ReticulumDnsResponseCode)?;
                response_code_seen = true;
                response_code = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    let outcome = decode_reticulum_dns_raw_outcome(
        require(outcome_code, RequiredField::ReticulumDnsRawOutcome)?,
        response_code,
        response_code_seen,
    )?;
    Ok(ReticulumDnsRawAttempt::new(
        require(source, RequiredField::ReticulumDnsRawSource)?,
        require(server, RequiredField::ReticulumDnsRawServer)?,
        outcome,
    ))
}

#[cfg(feature = "network-config")]
fn decode_reticulum_dns_resolution(
    decoder: &mut Decoder<'_>,
) -> Result<ReticulumDnsResolution, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut address = None;
    let mut source = None;
    let mut resolver_seen = false;
    let mut resolver = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(address.is_some(), RequiredField::ReticulumDnsResolvedIpv4)?;
                address = Some(decode_fixed_bytes::<4>(
                    decoder,
                    RequiredField::ReticulumDnsResolvedIpv4,
                )?);
            }
            1 => {
                reject_duplicate(
                    source.is_some(),
                    RequiredField::ReticulumDnsResolutionSource,
                )?;
                source = Some(decode_reticulum_dns_resolution_source(
                    decoder.u8().map_err(|_| DecodeError::Malformed)?,
                )?);
            }
            2 => {
                reject_duplicate(resolver_seen, RequiredField::ReticulumDnsResolutionResolver)?;
                resolver_seen = true;
                resolver =
                    decode_optional_ipv4(decoder, RequiredField::ReticulumDnsResolutionResolver)?;
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    if !resolver_seen {
        return Err(DecodeError::MissingField(
            RequiredField::ReticulumDnsResolutionResolver,
        ));
    }
    Ok(ReticulumDnsResolution::new(
        require(address, RequiredField::ReticulumDnsResolvedIpv4)?,
        require(source, RequiredField::ReticulumDnsResolutionSource)?,
        resolver,
    ))
}

#[cfg(feature = "network-config")]
fn decode_optional_ipv4(
    decoder: &mut Decoder<'_>,
    field: RequiredField,
) -> Result<Option<[u8; 4]>, DecodeError> {
    if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        Ok(None)
    } else {
        decode_fixed_bytes::<4>(decoder, field).map(Some)
    }
}

#[cfg(feature = "network-config")]
fn decode_optional_u64(
    decoder: &mut Decoder<'_>,
    _field: RequiredField,
) -> Result<Option<u64>, DecodeError> {
    if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        Ok(None)
    } else {
        decoder.u64().map(Some).map_err(|_| DecodeError::Malformed)
    }
}

fn decode_exact_array_len(
    decoder: &mut Decoder<'_>,
    expected: usize,
    field: RequiredField,
) -> Result<usize, DecodeError> {
    let actual = decoder
        .array()
        .map_err(|_| DecodeError::Malformed)?
        .ok_or(DecodeError::IndefiniteLength)?;
    if actual != expected as u64 {
        return Err(DecodeError::InvalidArrayLength {
            field,
            expected: expected as u64,
            actual,
        });
    }
    Ok(expected)
}

#[cfg(feature = "network-config")]
fn decode_wifi_station_state(value: u8) -> Result<WifiStationState, DecodeError> {
    match value {
        0 => Ok(WifiStationState::Disabled),
        1 => Ok(WifiStationState::Disconnected),
        2 => Ok(WifiStationState::Connecting),
        3 => Ok(WifiStationState::Connected),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::NetworkWifiState,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_tcp_peer_state(value: u8) -> Result<ReticulumTcpPeerState, DecodeError> {
    match value {
        0 => Ok(ReticulumTcpPeerState::Disabled),
        1 => Ok(ReticulumTcpPeerState::WaitingForNetwork),
        2 => Ok(ReticulumTcpPeerState::Connecting),
        3 => Ok(ReticulumTcpPeerState::Connected),
        4 => Ok(ReticulumTcpPeerState::Faulted),
        5 => Ok(ReticulumTcpPeerState::Backoff),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::NetworkTcpPeerState,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_tcp_failure(value: u8) -> Result<ReticulumTcpFailure, DecodeError> {
    match value {
        0 => Ok(ReticulumTcpFailure::DnsTimeout),
        1 => Ok(ReticulumTcpFailure::DnsLookupFailed),
        2 => Ok(ReticulumTcpFailure::DnsNoIpv4Result),
        3 => Ok(ReticulumTcpFailure::ConnectInvalidState),
        4 => Ok(ReticulumTcpFailure::ConnectReset),
        5 => Ok(ReticulumTcpFailure::ConnectTimeout),
        6 => Ok(ReticulumTcpFailure::ConnectNoRoute),
        7 => Ok(ReticulumTcpFailure::SocketClosed),
        8 => Ok(ReticulumTcpFailure::TransmitFailed),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::NetworkLastTcpFailure,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_rmap_stamp_phase(value: u8) -> Result<RmapStampPhase, DecodeError> {
    match value {
        0 => Ok(RmapStampPhase::Disabled),
        1 => Ok(RmapStampPhase::Searching),
        2 => Ok(RmapStampPhase::Ready),
        3 => Ok(RmapStampPhase::Exhausted),
        4 => Ok(RmapStampPhase::Faulted),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::RmapRuntimeStampPhase,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_rmap_initial_tcp_gate(value: u8) -> Result<RmapInitialTcpGateState, DecodeError> {
    match value {
        0 => Ok(RmapInitialTcpGateState::NotRequired),
        1 => Ok(RmapInitialTcpGateState::Waiting),
        2 => Ok(RmapInitialTcpGateState::Open),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::RmapRuntimeInitialTcpGate,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_rmap_queue_outcome(value: u8) -> Result<RmapQueueOutcome, DecodeError> {
    match value {
        0 => Ok(RmapQueueOutcome::NotAttempted),
        1 => Ok(RmapQueueOutcome::Accepted),
        2 => Ok(RmapQueueOutcome::AnnounceAdmissionDeferred),
        3 => Ok(RmapQueueOutcome::OrdinaryAdmissionDeferred),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::RmapRuntimeLastQueueOutcome,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_rmap_egress_confirmation(value: u8) -> Result<RmapEgressConfirmation, DecodeError> {
    match value {
        0 => Ok(RmapEgressConfirmation::NotApplicable),
        1 => Ok(RmapEgressConfirmation::NotObserved),
        2 => Ok(RmapEgressConfirmation::Confirmed),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::RmapRuntimeEgressConfirmation,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_rmap_deferred_reason(value: u8) -> Result<RmapDeferredReason, DecodeError> {
    match value {
        0 => Ok(RmapDeferredReason::DiscoveryModelInvalid),
        1 => Ok(RmapDeferredReason::PayloadEncodingFailed),
        2 => Ok(RmapDeferredReason::StampInitializationFailed),
        3 => Ok(RmapDeferredReason::DestinationActivationFailed),
        4 => Ok(RmapDeferredReason::StampSearchExhausted),
        5 => Ok(RmapDeferredReason::InitialTcpNotReady),
        6 => Ok(RmapDeferredReason::AnnouncePayloadTooLarge),
        7 => Ok(RmapDeferredReason::AnnounceQueueFull),
        8 => Ok(RmapDeferredReason::AnnounceConstructionRejected),
        9 => Ok(RmapDeferredReason::OrdinaryQueueRejected),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::RmapRuntimeDeferredReason,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_reticulum_dns_primary_outcome(
    value: u8,
) -> Result<ReticulumDnsPrimaryOutcome, DecodeError> {
    match value {
        0 => Ok(ReticulumDnsPrimaryOutcome::NotStarted),
        1 => Ok(ReticulumDnsPrimaryOutcome::Resolving),
        2 => Ok(ReticulumDnsPrimaryOutcome::Resolved),
        3 => Ok(ReticulumDnsPrimaryOutcome::NoServers),
        4 => Ok(ReticulumDnsPrimaryOutcome::Timeout),
        5 => Ok(ReticulumDnsPrimaryOutcome::LookupFailed),
        6 => Ok(ReticulumDnsPrimaryOutcome::NoIpv4Result),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::ReticulumDnsPrimaryOutcome,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_reticulum_dns_raw_setup_state(
    value: u8,
) -> Result<ReticulumDnsRawSetupState, DecodeError> {
    match value {
        0 => Ok(ReticulumDnsRawSetupState::NotStarted),
        1 => Ok(ReticulumDnsRawSetupState::Binding),
        2 => Ok(ReticulumDnsRawSetupState::Ready),
        3 => Ok(ReticulumDnsRawSetupState::BindFailed),
        4 => Ok(ReticulumDnsRawSetupState::EncodeFailed),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::ReticulumDnsRawSetupState,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_reticulum_dns_raw_source(value: u8) -> Result<ReticulumDnsRawSource, DecodeError> {
    match value {
        0 => Ok(ReticulumDnsRawSource::Dhcp),
        1 => Ok(ReticulumDnsRawSource::Public),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::ReticulumDnsRawSource,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_reticulum_dns_raw_outcome(
    value: u8,
    response_code: Option<u8>,
    response_code_seen: bool,
) -> Result<ReticulumDnsRawOutcome, DecodeError> {
    if value == 10 {
        if !response_code_seen {
            return Err(DecodeError::MissingField(
                RequiredField::ReticulumDnsResponseCode,
            ));
        }
        return response_code
            .and_then(ReticulumDnsRawOutcome::response_code_outcome)
            .ok_or(DecodeError::InvalidReticulumDnsDiagnostics);
    }
    if response_code_seen {
        return Err(DecodeError::InvalidReticulumDnsDiagnostics);
    }
    match value {
        0 => Ok(ReticulumDnsRawOutcome::NotStarted),
        1 => Ok(ReticulumDnsRawOutcome::SkippedDuplicate),
        2 => Ok(ReticulumDnsRawOutcome::SkippedLocalName),
        3 => Ok(ReticulumDnsRawOutcome::Sending),
        4 => Ok(ReticulumDnsRawOutcome::AwaitingResponse),
        5 => Ok(ReticulumDnsRawOutcome::Resolved),
        6 => Ok(ReticulumDnsRawOutcome::SendFailed),
        7 => Ok(ReticulumDnsRawOutcome::Timeout),
        8 => Ok(ReticulumDnsRawOutcome::NotAResponse),
        9 => Ok(ReticulumDnsRawOutcome::Truncated),
        11 => Ok(ReticulumDnsRawOutcome::QuestionMismatch),
        12 => Ok(ReticulumDnsRawOutcome::Malformed),
        13 => Ok(ReticulumDnsRawOutcome::NoIpv4Result),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::ReticulumDnsRawOutcome,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "network-config")]
fn decode_reticulum_dns_resolution_source(
    value: u8,
) -> Result<ReticulumDnsResolutionSource, DecodeError> {
    match value {
        0 => Ok(ReticulumDnsResolutionSource::SystemDns),
        1 => Ok(ReticulumDnsResolutionSource::RawDhcp),
        2 => Ok(ReticulumDnsResolutionSource::RawPublic),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::ReticulumDnsResolutionSource,
            value: u64::from(other),
        }),
    }
}

fn decode_identity_summary(body: &[u8]) -> Result<IdentitySummary, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut primary_destination = None;
    let mut lxmf_delivery_destination = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    primary_destination.is_some(),
                    RequiredField::IdentityPrimaryDestination,
                )?;
                primary_destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::IdentityPrimaryDestination,
                )?));
            }
            1 => {
                reject_duplicate(
                    lxmf_delivery_destination.is_some(),
                    RequiredField::IdentityLxmfDeliveryDestination,
                )?;
                lxmf_delivery_destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::IdentityLxmfDeliveryDestination,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let primary_destination = require(
        primary_destination,
        RequiredField::IdentityPrimaryDestination,
    )?;
    Ok(match lxmf_delivery_destination {
        Some(lxmf_delivery_destination) => IdentitySummary::with_lxmf_delivery_destination(
            primary_destination,
            lxmf_delivery_destination,
        ),
        None => IdentitySummary::new(primary_destination),
    })
}

fn decode_ingress_observation(
    decoder: &mut Decoder<'_>,
    invalid_pair: DecodeError,
) -> Result<IngressObservation, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut interface = None;
    let mut rssi = None;
    let mut snr = None;
    let mut rssi_seen = false;
    let mut snr_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(interface.is_some(), RequiredField::IngressInterface)?;
                interface = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(rssi_seen, RequiredField::IngressRssi)?;
                rssi_seen = true;
                rssi = Some(decoder.i16().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(snr_seen, RequiredField::IngressSnr)?;
                snr_seen = true;
                snr = Some(decoder.i16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    let signal = match (rssi, snr) {
        (None, None) => None,
        (Some(rssi), Some(snr)) => Some(IngressSignal::new(rssi, snr)),
        _ => return Err(invalid_pair),
    };
    Ok(IngressObservation::new(
        require(interface, RequiredField::IngressInterface)?,
        signal,
    ))
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_summary(body: &[u8]) -> Result<LxmfMessageSummary, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut handle = None;
    let mut message_id = None;
    let mut destination = None;
    let mut source = None;
    let mut timestamp_bits = None;
    let mut normalized_wire_len = None;
    let mut title_len = None;
    let mut content_len = None;
    let mut fields_encoded_len = None;
    let mut exact_wire_sha256 = None;
    let mut ingress = None;
    let mut ingress_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(handle.is_some(), RequiredField::LxmfHandle)?;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                handle =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfHandle,
                            value,
                        })?,
                    );
            }
            1 => {
                reject_duplicate(message_id.is_some(), RequiredField::LxmfMessageId)?;
                message_id = Some(decode_fixed_bytes::<32>(
                    &mut decoder,
                    RequiredField::LxmfMessageId,
                )?);
            }
            2 => {
                reject_duplicate(destination.is_some(), RequiredField::LxmfDestination)?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::LxmfDestination,
                )?));
            }
            3 => {
                reject_duplicate(source.is_some(), RequiredField::LxmfSource)?;
                source = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::LxmfSource,
                )?));
            }
            4 => {
                reject_duplicate(timestamp_bits.is_some(), RequiredField::LxmfTimestampBits)?;
                timestamp_bits = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(
                    normalized_wire_len.is_some(),
                    RequiredField::LxmfNormalizedWireLength,
                )?;
                normalized_wire_len = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(title_len.is_some(), RequiredField::LxmfTitleLength)?;
                title_len = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            7 => {
                reject_duplicate(content_len.is_some(), RequiredField::LxmfContentLength)?;
                content_len = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            8 => {
                reject_duplicate(
                    fields_encoded_len.is_some(),
                    RequiredField::LxmfFieldsEncodedLength,
                )?;
                fields_encoded_len = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            9 => {
                reject_duplicate(
                    exact_wire_sha256.is_some(),
                    RequiredField::LxmfExactWireSha256,
                )?;
                exact_wire_sha256 = Some(decode_fixed_bytes::<32>(
                    &mut decoder,
                    RequiredField::LxmfExactWireSha256,
                )?);
            }
            10 => {
                reject_duplicate(ingress_seen, RequiredField::IngressObservation)?;
                ingress_seen = true;
                ingress = Some(decode_ingress_observation(
                    &mut decoder,
                    DecodeError::InvalidLxmfMessageSummary,
                )?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    LxmfMessageSummary::new(
        require(handle, RequiredField::LxmfHandle)?,
        require(message_id, RequiredField::LxmfMessageId)?,
        require(destination, RequiredField::LxmfDestination)?,
        require(source, RequiredField::LxmfSource)?,
        require(timestamp_bits, RequiredField::LxmfTimestampBits)?,
        require(normalized_wire_len, RequiredField::LxmfNormalizedWireLength)?,
        require(title_len, RequiredField::LxmfTitleLength)?,
        require(content_len, RequiredField::LxmfContentLength)?,
        require(fields_encoded_len, RequiredField::LxmfFieldsEncodedLength)?,
        require(exact_wire_sha256, RequiredField::LxmfExactWireSha256)?,
    )
    .map(|summary| summary.with_ingress_observation(ingress))
    .map_err(|_| DecodeError::InvalidLxmfMessageSummary)
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_read_chunk(body: &[u8]) -> Result<LxmfReadChunk, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut handle = None;
    let mut offset = None;
    let mut total_len = None;
    let mut bytes = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(handle.is_some(), RequiredField::LxmfHandle)?;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                handle =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfHandle,
                            value,
                        })?,
                    );
            }
            1 => {
                reject_duplicate(offset.is_some(), RequiredField::LxmfReadOffset)?;
                offset = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(total_len.is_some(), RequiredField::LxmfNormalizedWireLength)?;
                total_len = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(bytes.is_some(), RequiredField::LxmfReadBytes)?;
                let decoded = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if decoded.len() > MAX_LXMF_READ_CHUNK_BYTES {
                    return Err(DecodeError::LxmfReadChunkTooLarge {
                        actual: decoded.len(),
                        max: MAX_LXMF_READ_CHUNK_BYTES,
                    });
                }
                bytes = Some(decoded);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    LxmfReadChunk::new(
        require(handle, RequiredField::LxmfHandle)?,
        require(offset, RequiredField::LxmfReadOffset)?,
        require(total_len, RequiredField::LxmfNormalizedWireLength)?,
        require(bytes, RequiredField::LxmfReadBytes)?,
    )
    .map_err(|error| match error {
        crate::InvalidLxmfReadChunk::TooLarge { actual } => DecodeError::LxmfReadChunkTooLarge {
            actual,
            max: MAX_LXMF_READ_CHUNK_BYTES,
        },
        crate::InvalidLxmfReadChunk::Empty | crate::InvalidLxmfReadChunk::OutsideMessage { .. } => {
            DecodeError::InvalidLxmfReadChunk
        }
    })
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_mailbox_status(body: &[u8]) -> Result<LxmfMailboxStatus, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut latest = None;
    let mut latest_seen = false;
    let mut acknowledged = None;
    let mut acknowledged_seen = false;
    let mut uncollected_count = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(latest_seen, RequiredField::LxmfMailboxLatest)?;
                latest_seen = true;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                latest =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfMailboxLatest,
                            value,
                        })?,
                    );
            }
            1 => {
                reject_duplicate(
                    acknowledged_seen,
                    RequiredField::LxmfMailboxAcknowledgedThrough,
                )?;
                acknowledged_seen = true;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                acknowledged =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfMailboxAcknowledgedThrough,
                            value,
                        })?,
                    );
            }
            2 => {
                reject_duplicate(
                    uncollected_count.is_some(),
                    RequiredField::LxmfMailboxUncollectedCount,
                )?;
                uncollected_count = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let encoded_count = require(
        uncollected_count,
        RequiredField::LxmfMailboxUncollectedCount,
    )?;
    let status = LxmfMailboxStatus::new(latest, acknowledged)
        .map_err(|_| DecodeError::InvalidLxmfMailboxStatus)?;
    if status.uncollected_count() != encoded_count {
        return Err(DecodeError::InvalidLxmfMailboxStatus);
    }
    Ok(status)
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_basic_send_accepted(body: &[u8]) -> Result<LxmfBasicSendAccepted, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    let mut message_id = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::SubmissionId)?;
                id = Some(SubmissionId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            1 => {
                reject_duplicate(message_id.is_some(), RequiredField::LxmfMessageId)?;
                message_id = Some(decode_fixed_bytes::<32>(
                    &mut decoder,
                    RequiredField::LxmfMessageId,
                )?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(LxmfBasicSendAccepted::new(
        require(id, RequiredField::SubmissionId)?,
        require(message_id, RequiredField::LxmfMessageId)?,
    ))
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_peer_discovery_page(body: &[u8]) -> Result<LxmfPeerDiscoveryPage, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut incarnation = None;
    let mut next_generation = None;
    let mut latest_generation = None;
    let mut oldest_generation = None;
    let mut history_gap = None;
    let mut peer = None;
    let mut peer_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    incarnation.is_some(),
                    RequiredField::LxmfPeerCursorIncarnation,
                )?;
                incarnation = Some(LxmfPeerDiscoveryIncarnation::new(decode_fixed_bytes::<8>(
                    &mut decoder,
                    RequiredField::LxmfPeerCursorIncarnation,
                )?));
            }
            1 => {
                reject_duplicate(
                    next_generation.is_some(),
                    RequiredField::LxmfPeerCursorGeneration,
                )?;
                next_generation = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(
                    latest_generation.is_some(),
                    RequiredField::LxmfPeerLatestGeneration,
                )?;
                latest_generation = Some(decode_lxmf_peer_generation(
                    &mut decoder,
                    RequiredField::LxmfPeerLatestGeneration,
                )?);
            }
            3 => {
                reject_duplicate(
                    oldest_generation.is_some(),
                    RequiredField::LxmfPeerOldestGeneration,
                )?;
                oldest_generation = Some(decode_lxmf_peer_generation(
                    &mut decoder,
                    RequiredField::LxmfPeerOldestGeneration,
                )?);
            }
            4 => {
                reject_duplicate(history_gap.is_some(), RequiredField::LxmfPeerHistoryGap)?;
                history_gap = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(peer_seen, RequiredField::LxmfPeerRecord)?;
                peer_seen = true;
                peer = Some(decode_lxmf_discovered_peer(&mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let next_cursor = LxmfPeerDiscoveryCursor::new(
        require(incarnation, RequiredField::LxmfPeerCursorIncarnation)?,
        require(next_generation, RequiredField::LxmfPeerCursorGeneration)?,
    );
    let history_gap = require(history_gap, RequiredField::LxmfPeerHistoryGap)?;
    if latest_generation.is_some() != oldest_generation.is_some() {
        return Err(DecodeError::Malformed);
    }
    if let (Some(oldest), Some(latest)) = (oldest_generation, latest_generation) {
        if oldest > latest || next_cursor.after_generation() > latest.get() {
            return Err(DecodeError::Malformed);
        }
        if let Some(peer) = &peer
            && (peer.generation().get() != next_cursor.after_generation()
                || peer.generation() < oldest
                || peer.generation() > latest)
        {
            return Err(DecodeError::Malformed);
        }
    } else if next_cursor.after_generation() != 0 || peer.is_some() {
        return Err(DecodeError::Malformed);
    }
    Ok(LxmfPeerDiscoveryPage::new(
        next_cursor,
        latest_generation,
        oldest_generation,
        history_gap,
        peer,
    ))
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_discovered_peer(
    decoder: &mut Decoder<'_>,
) -> Result<LxmfDiscoveredPeer, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut destination = None;
    let mut identity_hash = None;
    let mut app_data = None;
    let mut hops = None;
    let mut interface_id = None;
    let mut rssi_dbm = None;
    let mut rssi_seen = false;
    let mut snr_db = None;
    let mut snr_seen = false;
    let mut observed_age_ms = None;
    let mut generation = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(destination.is_some(), RequiredField::LxmfPeerDestination)?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    decoder,
                    RequiredField::LxmfPeerDestination,
                )?));
            }
            1 => {
                reject_duplicate(identity_hash.is_some(), RequiredField::LxmfPeerIdentityHash)?;
                identity_hash = Some(IdentityHash::new(decode_fixed_bytes::<16>(
                    decoder,
                    RequiredField::LxmfPeerIdentityHash,
                )?));
            }
            2 => {
                reject_duplicate(app_data.is_some(), RequiredField::LxmfPeerAppData)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if bytes.len() > MAX_LXMF_PEER_APP_DATA_BYTES {
                    return Err(DecodeError::LxmfPeerAppDataTooLarge {
                        actual: bytes.len(),
                        max: MAX_LXMF_PEER_APP_DATA_BYTES,
                    });
                }
                app_data = Some(bytes);
            }
            3 => {
                reject_duplicate(hops.is_some(), RequiredField::LxmfPeerHops)?;
                hops = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(interface_id.is_some(), RequiredField::LxmfPeerInterfaceId)?;
                interface_id = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(rssi_seen, RequiredField::LxmfPeerRssiDbm)?;
                rssi_seen = true;
                rssi_dbm = Some(decoder.i16().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(snr_seen, RequiredField::LxmfPeerSnrDb)?;
                snr_seen = true;
                snr_db = Some(decoder.i16().map_err(|_| DecodeError::Malformed)?);
            }
            7 => {
                reject_duplicate(
                    observed_age_ms.is_some(),
                    RequiredField::LxmfPeerObservedAge,
                )?;
                observed_age_ms = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            8 => {
                reject_duplicate(generation.is_some(), RequiredField::LxmfPeerGeneration)?;
                generation = Some(decode_lxmf_peer_generation(
                    decoder,
                    RequiredField::LxmfPeerGeneration,
                )?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    LxmfDiscoveredPeer::new(
        require(destination, RequiredField::LxmfPeerDestination)?,
        require(identity_hash, RequiredField::LxmfPeerIdentityHash)?,
        require(app_data, RequiredField::LxmfPeerAppData)?,
        require(hops, RequiredField::LxmfPeerHops)?,
        require(interface_id, RequiredField::LxmfPeerInterfaceId)?,
        rssi_dbm,
        snr_db,
        require(observed_age_ms, RequiredField::LxmfPeerObservedAge)?,
        require(generation, RequiredField::LxmfPeerGeneration)?,
    )
    .map_err(|too_large| DecodeError::LxmfPeerAppDataTooLarge {
        actual: too_large.actual(),
        max: too_large.maximum(),
    })
}

#[cfg(feature = "lxmf")]
fn decode_lxmf_peer_generation(
    decoder: &mut Decoder<'_>,
    field: RequiredField,
) -> Result<LxmfPeerGeneration, DecodeError> {
    let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
    LxmfPeerGeneration::new(value).map_err(|_| DecodeError::InvalidValue { field, value })
}

fn decode_probe_id(decoder: &mut Decoder<'_>) -> Result<ProbeId, DecodeError> {
    ProbeId::new(decode_fixed_bytes::<16>(decoder, RequiredField::ProbeId)?)
        .map_err(|_| DecodeError::InvalidProbeId)
}

fn decode_probe_start_accepted(body: &[u8]) -> Result<ProbeStartAccepted, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    let mut outcome = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::ProbeId)?;
                id = Some(decode_probe_id(&mut decoder)?);
            }
            1 => {
                reject_duplicate(outcome.is_some(), RequiredField::ProbeStartOutcome)?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                outcome = Some(match value {
                    0 => ProbeStartOutcome::Accepted,
                    1 => ProbeStartOutcome::Replayed,
                    other => {
                        return Err(DecodeError::InvalidValue {
                            field: RequiredField::ProbeStartOutcome,
                            value: u64::from(other),
                        });
                    }
                });
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(ProbeStartAccepted::new(
        require(id, RequiredField::ProbeId)?,
        require(outcome, RequiredField::ProbeStartOutcome)?,
    ))
}

fn decode_probe_poll(body: &[u8]) -> Result<ProbePollResponse, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut state = None;
    let mut value = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(state.is_some(), RequiredField::ProbePollState)?;
                state = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(value.is_some(), RequiredField::ProbePollValue)?;
                value = Some(capture_body(body, &mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let state = require(state, RequiredField::ProbePollState)?;
    let value = require(value, RequiredField::ProbePollValue)?;
    match state {
        0 => Ok(ProbePollResponse::Pending(decode_probe_phase(value)?)),
        1 => Ok(ProbePollResponse::Succeeded(decode_probe_success(value)?)),
        2 => Ok(ProbePollResponse::Failed(decode_probe_failure(value)?)),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::ProbePollState,
            value: u64::from(other),
        }),
    }
}

fn decode_probe_phase(body: &[u8]) -> Result<ProbePhase, DecodeError> {
    let mut decoder = Decoder::new(body);
    let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    finish_body(&decoder, body)?;
    match value {
        0 => Ok(ProbePhase::PathLookup),
        1 => Ok(ProbePhase::AwaitingDispatch),
        2 => Ok(ProbePhase::AwaitingProof),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::ProbePhase,
            value: u64::from(other),
        }),
    }
}

fn decode_probe_failure(body: &[u8]) -> Result<ProbeFailure, DecodeError> {
    let mut decoder = Decoder::new(body);
    let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    finish_body(&decoder, body)?;
    match value {
        0 => Ok(ProbeFailure::IdentityUnavailable),
        1 => Ok(ProbeFailure::NoPath),
        2 => Ok(ProbeFailure::Dispatch),
        3 => Ok(ProbeFailure::Timeout),
        4 => Ok(ProbeFailure::Internal),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::ProbeFailure,
            value: u64::from(other),
        }),
    }
}

fn decode_probe_success(body: &[u8]) -> Result<ProbeSuccess, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut round_trip_ms = None;
    let mut hops = None;
    let mut ingress = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(round_trip_ms.is_some(), RequiredField::ProbeRoundTripMs)?;
                round_trip_ms = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(hops.is_some(), RequiredField::ProbeHops)?;
                hops = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(ingress.is_some(), RequiredField::ProbeIngressObservation)?;
                ingress = Some(decode_ingress_observation(
                    &mut decoder,
                    DecodeError::InvalidProbePollResponse,
                )?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(ProbeSuccess::new(
        require(round_trip_ms, RequiredField::ProbeRoundTripMs)?,
        require(hops, RequiredField::ProbeHops)?,
        require(ingress, RequiredField::ProbeIngressObservation)?,
    ))
}

#[cfg(feature = "nomad")]
fn decode_nomad_fetch_start_accepted(body: &[u8]) -> Result<NomadFetchStartAccepted, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    let mut outcome = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::NomadFetchId)?;
                id = Some(decode_nomad_fetch_id(&mut decoder)?);
            }
            1 => {
                reject_duplicate(outcome.is_some(), RequiredField::NomadFetchStartOutcome)?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                outcome = Some(match value {
                    0 => NomadFetchStartOutcome::Accepted,
                    1 => NomadFetchStartOutcome::Replayed,
                    other => {
                        return Err(DecodeError::InvalidValue {
                            field: RequiredField::NomadFetchStartOutcome,
                            value: u64::from(other),
                        });
                    }
                });
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(NomadFetchStartAccepted {
        id: require(id, RequiredField::NomadFetchId)?,
        outcome: require(outcome, RequiredField::NomadFetchStartOutcome)?,
    })
}

#[cfg(feature = "nomad")]
fn decode_nomad_fetch_poll(body: &[u8]) -> Result<NomadFetchPollResponse, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut state = None;
    let mut value = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(state.is_some(), RequiredField::NomadFetchState)?;
                state = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(value.is_some(), RequiredField::NomadFetchValue)?;
                value = Some(capture_body(body, &mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let state = require(state, RequiredField::NomadFetchState)?;
    let value = require(value, RequiredField::NomadFetchValue)?;
    match state {
        0 => Ok(NomadFetchPollResponse::Pending(decode_nomad_fetch_phase(
            value,
        )?)),
        1 => {
            let mut decoder = Decoder::new(value);
            let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
            finish_body(&decoder, value)?;
            let page = NomadPage::new(bytes).map_err(|error| match error {
                crate::InvalidNomadPage::TooLarge { actual } => DecodeError::NomadPageTooLarge {
                    actual,
                    max: MAX_NOMAD_PAGE_BYTES,
                },
                crate::InvalidNomadPage::InvalidUtf8 => DecodeError::InvalidNomadPageUtf8,
            })?;
            Ok(NomadFetchPollResponse::Ready(page))
        }
        2 => Ok(NomadFetchPollResponse::Failed(decode_nomad_fetch_failure(
            value,
        )?)),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::NomadFetchState,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "nomad")]
fn decode_nomad_fetch_phase(body: &[u8]) -> Result<NomadFetchPhase, DecodeError> {
    let mut decoder = Decoder::new(body);
    let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    finish_body(&decoder, body)?;
    match value {
        0 => Ok(NomadFetchPhase::PathLookup),
        1 => Ok(NomadFetchPhase::LinkEstablishment),
        2 => Ok(NomadFetchPhase::RequestPreparation),
        3 => Ok(NomadFetchPhase::AwaitingDispatchConfirmation),
        4 => Ok(NomadFetchPhase::AwaitingResponse),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::NomadFetchPhase,
            value: u64::from(other),
        }),
    }
}

#[cfg(feature = "nomad")]
fn decode_nomad_fetch_failure(body: &[u8]) -> Result<NomadFetchFailure, DecodeError> {
    let mut decoder = Decoder::new(body);
    let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
    finish_body(&decoder, body)?;
    match value {
        0 => Ok(NomadFetchFailure::NoPath),
        1 => Ok(NomadFetchFailure::Link),
        2 => Ok(NomadFetchFailure::Request),
        3 => Ok(NomadFetchFailure::Timeout),
        4 => Ok(NomadFetchFailure::PageTooLarge),
        5 => Ok(NomadFetchFailure::InvalidUtf8),
        6 => Ok(NomadFetchFailure::Internal),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::NomadFetchFailure,
            value: u64::from(other),
        }),
    }
}

fn decode_submission_status(body: &[u8]) -> Result<SubmissionStatus, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    let mut state_code = None;
    let mut packet_len = None;
    let mut packet_hash = None;
    let mut failure = None;
    let mut packet_len_seen = false;
    let mut packet_hash_seen = false;
    let mut failure_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::SubmissionId)?;
                id = Some(SubmissionId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            1 => {
                reject_duplicate(state_code.is_some(), RequiredField::SubmissionState)?;
                state_code = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(packet_len_seen, RequiredField::SubmissionPacketLength)?;
                packet_len_seen = true;
                packet_len = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(
                    packet_hash_seen,
                    RequiredField::SubmissionEncodedPacketSha256,
                )?;
                packet_hash_seen = true;
                packet_hash = Some(decode_fixed_bytes::<32>(
                    &mut decoder,
                    RequiredField::SubmissionEncodedPacketSha256,
                )?);
            }
            4 => {
                reject_duplicate(failure_seen, RequiredField::SubmissionFailure)?;
                failure_seen = true;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                failure = Some(decode_submission_failure(value)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let state_code = require(state_code, RequiredField::SubmissionState)?;
    let state = decode_submission_state(state_code, packet_len, packet_hash, failure)?;
    Ok(SubmissionStatus {
        id: require(id, RequiredField::SubmissionId)?,
        state,
    })
}

#[cfg(feature = "rns-data")]
fn decode_submission_accepted(body: &[u8]) -> Result<SubmissionAccepted, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::SubmissionId)?;
                id = Some(SubmissionId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(SubmissionAccepted {
        id: require(id, RequiredField::SubmissionId)?,
    })
}

fn decode_error(body: &[u8]) -> Result<ApiErrorResponse, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut code = None;
    let mut operation = None;
    let mut operation_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(code.is_some(), RequiredField::ErrorCode)?;
                let value = decoder.u16().map_err(|_| DecodeError::Malformed)?;
                code = Some(decode_api_error(value)?);
            }
            1 => {
                reject_duplicate(operation_seen, RequiredField::ErrorOperation)?;
                operation_seen = true;
                operation = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(ApiErrorResponse {
        code: require(code, RequiredField::ErrorCode)?,
        operation,
    })
}

#[cfg(feature = "nomad")]
fn decode_nomad_fetch_id(decoder: &mut Decoder<'_>) -> Result<NomadFetchId, DecodeError> {
    NomadFetchId::from_bytes(decode_fixed_bytes::<16>(
        decoder,
        RequiredField::NomadFetchId,
    )?)
    .map_err(|_| DecodeError::InvalidNomadFetchId)
}

fn decode_fixed_bytes<const N: usize>(
    decoder: &mut Decoder<'_>,
    field: RequiredField,
) -> Result<[u8; N], DecodeError> {
    let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
    bytes
        .try_into()
        .map_err(|_| DecodeError::InvalidByteStringLength {
            field,
            expected: N,
            actual: bytes.len(),
        })
}

fn decode_optional_fixed_bytes<const N: usize>(
    decoder: &mut Decoder<'_>,
    field: RequiredField,
) -> Result<Option<[u8; N]>, DecodeError> {
    if matches!(
        decoder.datatype().map_err(|_| DecodeError::Malformed)?,
        Type::Null
    ) {
        decoder.null().map_err(|_| DecodeError::Malformed)?;
        Ok(None)
    } else {
        decode_fixed_bytes(decoder, field).map(Some)
    }
}

fn decode_direct_radio_availability(value: u8) -> Result<CapabilityAvailability, DecodeError> {
    decode_capability_availability(value, RequiredField::CapabilityDirectRadioTx)
}

fn decode_capability_availability(
    value: u8,
    field: RequiredField,
) -> Result<CapabilityAvailability, DecodeError> {
    match value {
        0 => Ok(CapabilityAvailability::Unavailable),
        1 => Ok(CapabilityAvailability::Disabled),
        2 => Ok(CapabilityAvailability::Available),
        other => Err(DecodeError::InvalidValue {
            field,
            value: u64::from(other),
        }),
    }
}

fn decode_submission_state(
    code: u8,
    packet_len: Option<u16>,
    packet_hash: Option<[u8; 32]>,
    failure: Option<SubmissionFailure>,
) -> Result<SubmissionState, DecodeError> {
    match (code, packet_len, packet_hash, failure) {
        (0, None, None, None) => Ok(SubmissionState::Queued),
        (1, None, None, None) => Ok(SubmissionState::Preparing),
        (2, Some(packet_len), Some(packet_hash), None) => {
            Ok(SubmissionState::AwaitingDelivery(PreparedPacketDetails {
                packet_len,
                encoded_packet_sha256: crate::EncodedPacketSha256::new(packet_hash),
            }))
        }
        (3, Some(packet_len), Some(packet_hash), None) => {
            Ok(SubmissionState::Delivered(PreparedPacketDetails {
                packet_len,
                encoded_packet_sha256: crate::EncodedPacketSha256::new(packet_hash),
            }))
        }
        (4, None, None, Some(failure)) => Ok(SubmissionState::Failed(failure)),
        (5, None, None, None) => Ok(SubmissionState::Cancelled),
        (0..=5, _, _, _) => Err(DecodeError::InvalidSubmissionStatus),
        (other, _, _, _) => Err(DecodeError::InvalidValue {
            field: RequiredField::SubmissionState,
            value: u64::from(other),
        }),
    }
}

fn decode_submission_failure(value: u8) -> Result<SubmissionFailure, DecodeError> {
    match value {
        0 => Ok(SubmissionFailure::NoPath),
        1 => Ok(SubmissionFailure::DeliveryTimeout),
        2 => Ok(SubmissionFailure::Rejected),
        3 => Ok(SubmissionFailure::Internal),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::SubmissionFailure,
            value: u64::from(other),
        }),
    }
}

fn decode_api_error(value: u16) -> Result<ApiErrorCode, DecodeError> {
    match value {
        1 => Ok(ApiErrorCode::UnsupportedOperation),
        2 => Ok(ApiErrorCode::UnsupportedVersion),
        3 => Ok(ApiErrorCode::AuthenticationRequired),
        4 => Ok(ApiErrorCode::PermissionDenied),
        5 => Ok(ApiErrorCode::NotFound),
        6 => Ok(ApiErrorCode::InvalidRequest),
        7 => Ok(ApiErrorCode::CapabilityUnavailable),
        8 => Ok(ApiErrorCode::Internal),
        9 => Ok(ApiErrorCode::CapacityExhausted),
        10 => Ok(ApiErrorCode::IdempotencyConflict),
        11 => Ok(ApiErrorCode::RetryLater),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::ErrorCode,
            value: u64::from(other),
        }),
    }
}
