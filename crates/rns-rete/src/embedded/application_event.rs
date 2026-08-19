//! Protocol application-event projection and its bounded owner.

use super::*;

/// Product-owned reason a pending Link request failed.
///
/// This deliberately mirrors the complete pinned native reason set without
/// exposing Rete's enum in the firmware-facing event surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationRequestFailReason {
    /// The request timed out.
    Timeout,
    /// The Link closed before a response arrived.
    LinkClosed,
    /// The response Resource transfer failed.
    ResourceFailed,
}

/// Payload-free classification for one [`ApplicationEvent`].
///
/// The stable labels expose no packet body, key material, or identifier bytes,
/// so callers can record explicit delivery/discard policy without formatting
/// the owned event itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApplicationEventKind {
    /// A valid announce was received.
    AnnounceReceived,
    /// Decrypted DATA was addressed to a local destination.
    DataReceived,
    /// A valid proof covered one sent packet.
    ProofReceived,
    /// One sent-packet receipt expired.
    ReceiptFailed,
    /// A locally owned Link became active.
    LinkEstablished,
    /// An authenticated LRRTT updated one active Link.
    LinkRttUpdated,
    /// Decrypted best-effort data arrived on an active Link.
    LinkData,
    /// One or more reliable Channel messages arrived on a Link.
    ChannelMessages,
    /// A Link request arrived.
    RequestReceived,
    /// A Link request carrying an encoded non-binary/string MessagePack value arrived.
    RequestValueReceived,
    /// A Link response arrived.
    ResponseReceived,
    /// A locally owned Link closed.
    LinkClosed,
    /// A remote peer authenticated its identity on a Link.
    LinkIdentified,
    /// A Resource advertisement was retained by the native core.
    ResourceOffered,
    /// Resource transfer progress was reported.
    ResourceProgress,
    /// A Resource transfer completed.
    ResourceComplete,
    /// A Resource transfer failed.
    ResourceFailed,
    /// A peer rejected one Resource sent by this node.
    ResourceRejected,
    /// A pending Link request failed.
    RequestFailed,
    /// Response-as-Resource progress was reported for a pending request.
    RequestProgress,
    /// Periodic protocol maintenance completed.
    Tick,
}

impl ApplicationEventKind {
    /// Return a stable payload-free label suitable for logs and metrics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnnounceReceived => "announce_received",
            Self::DataReceived => "data_received",
            Self::ProofReceived => "proof_received",
            Self::ReceiptFailed => "receipt_failed",
            Self::LinkEstablished => "link_established",
            Self::LinkRttUpdated => "link_rtt_updated",
            Self::LinkData => "link_data",
            Self::ChannelMessages => "channel_messages",
            Self::RequestReceived => "request_received",
            Self::RequestValueReceived => "request_value_received",
            Self::ResponseReceived => "response_received",
            Self::LinkClosed => "link_closed",
            Self::LinkIdentified => "link_identified",
            Self::ResourceOffered => "resource_offered",
            Self::ResourceProgress => "resource_progress",
            Self::ResourceComplete => "resource_complete",
            Self::ResourceFailed => "resource_failed",
            Self::ResourceRejected => "resource_rejected",
            Self::RequestFailed => "request_failed",
            Self::RequestProgress => "request_progress",
            Self::Tick => "tick",
        }
    }
}

impl core::fmt::Display for ApplicationEventKind {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Authoritative association between one retained owned Link and its destination.
///
/// Firmware can inspect this binding but cannot construct or alter it. The
/// owning [`EmbeddedNode`] creates bindings only from Rete's retained Link
/// state, keeping consumer-supplied Link and destination values out of the
/// application-event trust boundary. For a responder Link, `destination` is
/// the exact registered local destination at which the Link was accepted.
///
/// ```compile_fail
/// use reticulum_rns_rete::{ApplicationLinkBinding, ApplicationLinkRole};
///
/// let _forged = ApplicationLinkBinding {
///     link: [0_u8; 16],
///     destination: [0_u8; 16],
///     role: ApplicationLinkRole::Responder,
/// };
/// ```
#[must_use = "a Link binding is authoritative application-event provenance"]
pub struct ApplicationLinkBinding {
    pub(crate) link: [u8; rete_core::TRUNCATED_HASH_LEN],
    pub(crate) destination: [u8; rete_core::TRUNCATED_HASH_LEN],
    pub(crate) role: ApplicationLinkRole,
}

/// Local endpoint role of one retained owned Link.
///
/// The role is copied only from Rete's retained Link state. It lets application
/// policy distinguish a Link accepted at a local destination from one this
/// node initiated without exposing mutable native state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApplicationLinkRole {
    /// This node initiated the Link.
    Initiator,
    /// This node accepted the Link at a local destination.
    Responder,
}

impl ApplicationLinkRole {
    pub(crate) fn from_retained(role: LinkRole) -> Self {
        match role {
            LinkRole::Initiator => Self::Initiator,
            LinkRole::Responder => Self::Responder,
        }
    }
}

impl ApplicationLinkBinding {
    pub(crate) fn from_retained_link(link: &rete_transport::Link) -> Self {
        Self {
            link: *link.link_id.as_bytes(),
            destination: *link.destination_hash.as_bytes(),
            role: ApplicationLinkRole::from_retained(link.role),
        }
    }

    /// Stable identifier of the retained owned Link.
    pub const fn link(&self) -> &[u8; rete_core::TRUNCATED_HASH_LEN] {
        &self.link
    }

    /// Destination to which the retained owned Link was established.
    ///
    /// This is an authoritative local destination when [`Self::role`] returns
    /// [`ApplicationLinkRole::Responder`]. For an initiator Link, it names the
    /// remote destination.
    pub const fn destination(&self) -> &[u8; rete_core::TRUNCATED_HASH_LEN] {
        &self.destination
    }

    /// Whether this node initiated or accepted the retained Link.
    pub const fn role(&self) -> ApplicationLinkRole {
        self.role
    }
}

impl core::fmt::Debug for ApplicationLinkBinding {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ApplicationLinkBinding")
            .field("link", &self.link)
            .field("destination", &self.destination)
            .field("role", &self.role)
            .finish()
    }
}

/// One product-owned application event projected from the pinned Rete core.
///
/// Every protocol identifier is copied into a stable byte array. Allocation-
/// backed bodies move directly from the native event without cloning. The enum
/// intentionally does not implement `Clone`: each payload has one exact owner
/// that the caller must retain, deliver, or explicitly discard.
#[must_use = "application events must be retained, delivered, or explicitly discarded"]
pub enum ApplicationEvent {
    /// A valid announce was received.
    AnnounceReceived {
        /// Destination hash of the announcing identity.
        destination: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Identity hash of the announcer.
        identity: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Hop count at receipt.
        hops: u8,
        /// Owned announce application data.
        app_data: Option<Vec<u8>>,
        /// Interface provenance and optional physical-link signal values.
        ingress: Option<IngressObservation>,
    },
    /// Decrypted DATA addressed to one local destination.
    DataReceived {
        /// Addressed destination hash.
        destination: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved plaintext owner.
        payload: Vec<u8>,
        /// Interface provenance and optional physical-link signal values.
        ingress: Option<IngressObservation>,
    },
    /// A valid proof covered one sent packet.
    ProofReceived {
        /// Complete covered packet hash.
        packet_hash: [u8; 32],
        /// Interface provenance and optional physical-link signal values.
        ingress: Option<IngressObservation>,
    },
    /// One sent-packet receipt expired.
    ReceiptFailed {
        /// Complete expired packet hash.
        packet_hash: [u8; 32],
    },
    /// A locally owned Link became active.
    LinkEstablished {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
    /// An authenticated LRRTT updated one active Link.
    LinkRttUpdated {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Updated round-trip time in seconds.
        rtt_seconds: f64,
    },
    /// Decrypted best-effort data arrived on an active Link.
    LinkData {
        /// Authoritative retained Link-to-destination association.
        binding: ApplicationLinkBinding,
        /// Exact moved plaintext owner.
        data: Vec<u8>,
        /// Reticulum Link context byte.
        context: u8,
        /// Interface provenance and optional physical-link signal values.
        ingress: Option<IngressObservation>,
    },
    /// One or more reliable Channel messages arrived on a Link.
    ChannelMessages {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved `(message_type, payload)` owners.
        messages: Vec<(u16, Vec<u8>)>,
    },
    /// A Link request arrived.
    RequestReceived {
        /// Authoritative retained Link-to-destination association.
        ///
        /// Server-side dispatch can authorize the exact local destination by
        /// requiring [`ApplicationLinkRole::Responder`] and comparing
        /// [`ApplicationLinkBinding::destination`].
        binding: ApplicationLinkBinding,
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request-path hash bytes.
        path: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved request body.
        data: Vec<u8>,
    },
    /// A Link request carrying an encoded non-binary/string MessagePack value arrived.
    ///
    /// Binary and string request values continue through [`Self::RequestReceived`].
    /// This variant preserves `nil`, maps, arrays, scalars, and extension values
    /// without conflating canonical anonymous `nil` with an empty byte string.
    RequestValueReceived {
        /// Authoritative retained Link-to-destination association.
        ///
        /// Server-side dispatch can authorize the exact local destination by
        /// requiring [`ApplicationLinkRole::Responder`] and comparing
        /// [`ApplicationLinkBinding::destination`].
        binding: ApplicationLinkBinding,
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request-path hash bytes.
        path: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Timestamp from the request wire format, in Unix seconds.
        requested_at: f64,
        /// Exact moved MessagePack encoding of the validated request value.
        encoded_value: Vec<u8>,
    },
    /// A Link response arrived.
    ResponseReceived {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved response body.
        data: Vec<u8>,
    },
    /// A locally owned Link closed.
    LinkClosed {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
    /// A remote peer authenticated its identity on a Link.
    LinkIdentified {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable remote identity hash bytes.
        identity: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Remote public key. Debug output is redacted.
        public_key: [u8; 64],
    },
    /// A Resource advertisement was retained by the native core.
    ///
    /// The current embedded ingress gate rejects every Resource context, so
    /// this variant documents the future lossless surface without enabling it.
    ResourceOffered {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Truncated Resource hash.
        resource_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Advertised transfer size.
        total_size: usize,
    },
    /// Resource transfer progress.
    ResourceProgress {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Truncated Resource hash.
        resource_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Received parts.
        current: usize,
        /// Expected parts.
        total: usize,
    },
    /// A Resource transfer completed.
    ResourceComplete {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Truncated Resource hash.
        resource_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved assembled body.
        data: Vec<u8>,
    },
    /// A Resource transfer failed.
    ResourceFailed {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Truncated Resource hash.
        resource_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
    /// A peer rejected one Resource sent by this node.
    ResourceRejected {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Truncated Resource hash.
        resource_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
    /// A pending Link request failed.
    RequestFailed {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Typed terminal reason.
        reason: ApplicationRequestFailReason,
    },
    /// Response-as-Resource progress for a pending Link request.
    RequestProgress {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Received parts.
        current: usize,
        /// Expected parts.
        total: usize,
    },
    /// Periodic protocol maintenance completed.
    Tick {
        /// Paths expired by this tick.
        expired_paths: usize,
        /// Links closed by establishment or stale-session maintenance.
        closed_links: usize,
    },
}

impl ApplicationEvent {
    /// Classify this event without inspecting or formatting owned payloads.
    ///
    /// This match intentionally has no wildcard. Adding a project event must
    /// therefore define its explicit logging and discard-policy classification.
    pub fn kind(&self) -> ApplicationEventKind {
        match self {
            Self::AnnounceReceived { .. } => ApplicationEventKind::AnnounceReceived,
            Self::DataReceived { .. } => ApplicationEventKind::DataReceived,
            Self::ProofReceived { .. } => ApplicationEventKind::ProofReceived,
            Self::ReceiptFailed { .. } => ApplicationEventKind::ReceiptFailed,
            Self::LinkEstablished { .. } => ApplicationEventKind::LinkEstablished,
            Self::LinkRttUpdated { .. } => ApplicationEventKind::LinkRttUpdated,
            Self::LinkData { .. } => ApplicationEventKind::LinkData,
            Self::ChannelMessages { .. } => ApplicationEventKind::ChannelMessages,
            Self::RequestReceived { .. } => ApplicationEventKind::RequestReceived,
            Self::RequestValueReceived { .. } => ApplicationEventKind::RequestValueReceived,
            Self::ResponseReceived { .. } => ApplicationEventKind::ResponseReceived,
            Self::LinkClosed { .. } => ApplicationEventKind::LinkClosed,
            Self::LinkIdentified { .. } => ApplicationEventKind::LinkIdentified,
            Self::ResourceOffered { .. } => ApplicationEventKind::ResourceOffered,
            Self::ResourceProgress { .. } => ApplicationEventKind::ResourceProgress,
            Self::ResourceComplete { .. } => ApplicationEventKind::ResourceComplete,
            Self::ResourceFailed { .. } => ApplicationEventKind::ResourceFailed,
            Self::ResourceRejected { .. } => ApplicationEventKind::ResourceRejected,
            Self::RequestFailed { .. } => ApplicationEventKind::RequestFailed,
            Self::RequestProgress { .. } => ApplicationEventKind::RequestProgress,
            Self::Tick { .. } => ApplicationEventKind::Tick,
        }
    }
}

impl core::fmt::Debug for ApplicationEvent {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::AnnounceReceived {
                destination,
                identity,
                hops,
                app_data,
                ingress,
            } => formatter
                .debug_struct("AnnounceReceived")
                .field("destination", destination)
                .field("identity", identity)
                .field("hops", hops)
                .field("app_data_len", &app_data.as_ref().map(Vec::len))
                .field("ingress", ingress)
                .finish(),
            Self::DataReceived {
                destination,
                payload,
                ingress,
            } => formatter
                .debug_struct("DataReceived")
                .field("destination", destination)
                .field("payload_len", &payload.len())
                .field("ingress", ingress)
                .finish(),
            Self::ProofReceived {
                packet_hash,
                ingress,
            } => formatter
                .debug_struct("ProofReceived")
                .field("packet_hash", packet_hash)
                .field("ingress", ingress)
                .finish(),
            Self::ReceiptFailed { packet_hash } => formatter
                .debug_struct("ReceiptFailed")
                .field("packet_hash", packet_hash)
                .finish(),
            Self::LinkEstablished { link } => formatter
                .debug_struct("LinkEstablished")
                .field("link", link)
                .finish(),
            Self::LinkRttUpdated { link, rtt_seconds } => formatter
                .debug_struct("LinkRttUpdated")
                .field("link", link)
                .field("rtt_seconds", rtt_seconds)
                .finish(),
            Self::LinkData {
                binding,
                data,
                context,
                ingress,
            } => formatter
                .debug_struct("LinkData")
                .field("binding", binding)
                .field("data_len", &data.len())
                .field("context", context)
                .field("ingress", ingress)
                .finish(),
            Self::ChannelMessages { link, messages } => formatter
                .debug_struct("ChannelMessages")
                .field("link", link)
                .field("message_count", &messages.len())
                .field(
                    "payload_bytes",
                    &messages
                        .iter()
                        .map(|(_, payload)| payload.len())
                        .sum::<usize>(),
                )
                .finish(),
            Self::RequestReceived {
                binding,
                request,
                path,
                data,
            } => formatter
                .debug_struct("RequestReceived")
                .field("binding", binding)
                .field("request", request)
                .field("path", path)
                .field("data_len", &data.len())
                .finish(),
            Self::RequestValueReceived {
                binding,
                request,
                path,
                requested_at,
                encoded_value,
            } => formatter
                .debug_struct("RequestValueReceived")
                .field("binding", binding)
                .field("request", request)
                .field("path", path)
                .field("requested_at", requested_at)
                .field("encoded_value_len", &encoded_value.len())
                .finish(),
            Self::ResponseReceived {
                link,
                request,
                data,
            } => formatter
                .debug_struct("ResponseReceived")
                .field("link", link)
                .field("request", request)
                .field("data_len", &data.len())
                .finish(),
            Self::LinkClosed { link } => formatter
                .debug_struct("LinkClosed")
                .field("link", link)
                .finish(),
            Self::LinkIdentified { link, identity, .. } => formatter
                .debug_struct("LinkIdentified")
                .field("link", link)
                .field("identity", identity)
                .field("public_key", &"[redacted; 64 bytes]")
                .finish(),
            Self::ResourceOffered {
                link,
                resource_hash,
                total_size,
            } => formatter
                .debug_struct("ResourceOffered")
                .field("link", link)
                .field("resource_hash", resource_hash)
                .field("total_size", total_size)
                .finish(),
            Self::ResourceProgress {
                link,
                resource_hash,
                current,
                total,
            } => formatter
                .debug_struct("ResourceProgress")
                .field("link", link)
                .field("resource_hash", resource_hash)
                .field("current", current)
                .field("total", total)
                .finish(),
            Self::ResourceComplete {
                link,
                resource_hash,
                data,
            } => formatter
                .debug_struct("ResourceComplete")
                .field("link", link)
                .field("resource_hash", resource_hash)
                .field("data_len", &data.len())
                .finish(),
            Self::ResourceFailed {
                link,
                resource_hash,
            } => formatter
                .debug_struct("ResourceFailed")
                .field("link", link)
                .field("resource_hash", resource_hash)
                .finish(),
            Self::ResourceRejected {
                link,
                resource_hash,
            } => formatter
                .debug_struct("ResourceRejected")
                .field("link", link)
                .field("resource_hash", resource_hash)
                .finish(),
            Self::RequestFailed {
                link,
                request,
                reason,
            } => formatter
                .debug_struct("RequestFailed")
                .field("link", link)
                .field("request", request)
                .field("reason", reason)
                .finish(),
            Self::RequestProgress {
                link,
                request,
                current,
                total,
            } => formatter
                .debug_struct("RequestProgress")
                .field("link", link)
                .field("request", request)
                .field("current", current)
                .field("total", total)
                .finish(),
            Self::Tick {
                expired_paths,
                closed_links,
            } => formatter
                .debug_struct("Tick")
                .field("expired_paths", expired_paths)
                .field("closed_links", closed_links)
                .finish(),
        }
    }
}

fn require_application_link_binding(
    link_id: &LinkId,
    binding: Option<ApplicationLinkBinding>,
) -> Result<ApplicationLinkBinding, ApplicationEventProjectionError> {
    match binding {
        Some(binding) if binding.link() == link_id.as_bytes() => Ok(binding),
        Some(_) | None => Err(ApplicationEventProjectionError::LinkStateNotRetained {
            link: *link_id.as_bytes(),
        }),
    }
}

/// Consume one pinned native event into the exhaustive project-owned surface.
///
/// This match intentionally has no wildcard so a Rete update that adds an event
/// cannot compile until its ownership and redaction policy is reviewed here.
pub(crate) fn project_application_event(
    event: NativeNodeEvent,
    link_binding: Option<ApplicationLinkBinding>,
) -> Result<ApplicationEvent, ApplicationEventProjectionError> {
    Ok(match event {
        NativeNodeEvent::AnnounceReceived {
            dest_hash,
            identity_hash,
            hops,
            app_data,
        } => ApplicationEvent::AnnounceReceived {
            destination: *dest_hash.as_bytes(),
            identity: *identity_hash.as_bytes(),
            hops,
            app_data,
            ingress: None,
        },
        NativeNodeEvent::DataReceived { dest_hash, payload } => ApplicationEvent::DataReceived {
            destination: *dest_hash.as_bytes(),
            payload,
            ingress: None,
        },
        NativeNodeEvent::ProofReceived { packet_hash } => ApplicationEvent::ProofReceived {
            packet_hash,
            ingress: None,
        },
        NativeNodeEvent::ReceiptFailed { packet_hash } => {
            ApplicationEvent::ReceiptFailed { packet_hash }
        }
        NativeNodeEvent::LinkEstablished { link_id } => ApplicationEvent::LinkEstablished {
            link: *link_id.as_bytes(),
        },
        NativeNodeEvent::LinkRttUpdated { link_id, rtt } => ApplicationEvent::LinkRttUpdated {
            link: *link_id.as_bytes(),
            rtt_seconds: rtt,
        },
        NativeNodeEvent::LinkData {
            link_id,
            data,
            context,
        } => ApplicationEvent::LinkData {
            binding: require_application_link_binding(&link_id, link_binding)?,
            data,
            context,
            ingress: None,
        },
        NativeNodeEvent::ChannelMessages { link_id, messages } => {
            ApplicationEvent::ChannelMessages {
                link: *link_id.as_bytes(),
                messages,
            }
        }
        NativeNodeEvent::RequestReceived {
            link_id,
            request_id,
            path_hash,
            data,
        } => ApplicationEvent::RequestReceived {
            binding: require_application_link_binding(&link_id, link_binding)?,
            request: *request_id.as_bytes(),
            path: *path_hash.as_bytes(),
            data,
        },
        NativeNodeEvent::RequestValueReceived {
            link_id,
            request_id,
            path_hash,
            requested_at,
            value,
        } => ApplicationEvent::RequestValueReceived {
            binding: require_application_link_binding(&link_id, link_binding)?,
            request: *request_id.as_bytes(),
            path: *path_hash.as_bytes(),
            requested_at,
            encoded_value: value,
        },
        NativeNodeEvent::ResponseReceived {
            link_id,
            request_id,
            data,
        } => ApplicationEvent::ResponseReceived {
            link: *link_id.as_bytes(),
            request: *request_id.as_bytes(),
            data,
        },
        NativeNodeEvent::LinkClosed { link_id } => ApplicationEvent::LinkClosed {
            link: *link_id.as_bytes(),
        },
        NativeNodeEvent::LinkIdentified {
            link_id,
            identity_hash,
            public_key,
        } => ApplicationEvent::LinkIdentified {
            link: *link_id.as_bytes(),
            identity: *identity_hash.as_bytes(),
            public_key,
        },
        NativeNodeEvent::ResourceOffered {
            link_id,
            resource_hash,
            total_size,
        } => ApplicationEvent::ResourceOffered {
            link: *link_id.as_bytes(),
            resource_hash,
            total_size,
        },
        NativeNodeEvent::ResourceProgress {
            link_id,
            resource_hash,
            current,
            total,
        } => ApplicationEvent::ResourceProgress {
            link: *link_id.as_bytes(),
            resource_hash,
            current,
            total,
        },
        NativeNodeEvent::ResourceComplete {
            link_id,
            resource_hash,
            data,
        } => ApplicationEvent::ResourceComplete {
            link: *link_id.as_bytes(),
            resource_hash,
            data,
        },
        NativeNodeEvent::ResourceFailed {
            link_id,
            resource_hash,
        } => ApplicationEvent::ResourceFailed {
            link: *link_id.as_bytes(),
            resource_hash,
        },
        NativeNodeEvent::ResourceRejected {
            link_id,
            resource_hash,
        } => ApplicationEvent::ResourceRejected {
            link: *link_id.as_bytes(),
            resource_hash,
        },
        NativeNodeEvent::RequestFailed {
            link_id,
            request_id,
            reason,
        } => ApplicationEvent::RequestFailed {
            link: *link_id.as_bytes(),
            request: *request_id.as_bytes(),
            reason: match reason {
                NativeRequestFailReason::Timeout => ApplicationRequestFailReason::Timeout,
                NativeRequestFailReason::LinkClosed => ApplicationRequestFailReason::LinkClosed,
                NativeRequestFailReason::ResourceFailed => {
                    ApplicationRequestFailReason::ResourceFailed
                }
            },
        },
        NativeNodeEvent::RequestProgress {
            link_id,
            request_id,
            current,
            total,
        } => ApplicationEvent::RequestProgress {
            link: *link_id.as_bytes(),
            request: *request_id.as_bytes(),
            current,
            total,
        },
        NativeNodeEvent::Tick {
            expired_paths,
            closed_links,
        } => ApplicationEvent::Tick {
            expired_paths,
            closed_links,
        },
    })
}

pub(crate) fn project_local_close_event(event: NativeNodeEvent) -> ApplicationEvent {
    match project_application_event(event, None) {
        Ok(event @ ApplicationEvent::LinkClosed { .. }) => event,
        Ok(event) => panic!(
            "pinned Rete close_link emitted unexpected application event {}",
            event.kind()
        ),
        Err(error) => {
            panic!("pinned Rete close_link emitted an unprojectable application event: {error:?}")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplicationEventProjectionError {
    LinkStateNotRetained {
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
}

/// Read-only application-event collection owned by one [`NodeActions`]
/// envelope.
///
/// Its vector and optional retained-proof owner move as one opaque value and
/// cannot be separated or mutated by downstream safe code. The public field
/// shape still supports existing read-only `.as_slice()`, `.len()`,
/// `.is_empty()`, `.first()`, `.iter()`, and indexing call sites.
pub struct ApplicationEvents {
    pub(crate) events: Vec<ApplicationEvent>,
    pub(crate) retained_proof: Option<RetainedApplicationProof>,
}

impl ApplicationEvents {
    pub(crate) fn without_retained_proofs(events: Vec<ApplicationEvent>) -> Self {
        Self {
            events,
            retained_proof: None,
        }
    }

    pub(crate) fn retained(
        events: Vec<ApplicationEvent>,
        retained_proof: RetainedApplicationProof,
    ) -> Self {
        Self {
            events,
            retained_proof: Some(retained_proof),
        }
    }

    pub(crate) const fn retained_proof(&self) -> Option<&RetainedApplicationProof> {
        self.retained_proof.as_ref()
    }

    pub(crate) fn into_parts(self) -> (Vec<ApplicationEvent>, Option<RetainedApplicationProof>) {
        (self.events, self.retained_proof)
    }

    pub(crate) fn owns_no_actions(&self) -> bool {
        let Self {
            events,
            retained_proof,
        } = self;
        events.is_empty() && retained_proof.is_none()
    }

    pub(crate) fn retained_proof_count(&self) -> usize {
        usize::from(self.retained_proof.is_some())
    }

    #[cfg(test)]
    pub(crate) fn clear_semantic_events_for_test(&mut self) {
        self.events.clear();
    }

    #[cfg(test)]
    pub(crate) fn replace_semantic_event_for_test(
        &mut self,
        index: usize,
        event: ApplicationEvent,
    ) {
        self.events[index] = event;
    }

    /// All transport-neutral events in original order.
    pub fn as_slice(&self) -> &[ApplicationEvent] {
        self.events.as_slice()
    }

    /// Number of transport-neutral events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether no transport-neutral event is owned.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// First event, if present.
    pub fn first(&self) -> Option<&ApplicationEvent> {
        self.events.first()
    }

    /// Event at one exact index, if present.
    pub fn get(&self, index: usize) -> Option<&ApplicationEvent> {
        self.events.get(index)
    }

    /// Iterate over immutable events in original order.
    pub fn iter(&self) -> core::slice::Iter<'_, ApplicationEvent> {
        self.events.iter()
    }

    /// Stable allocation pointer for read-only ownership-correlation tests.
    pub fn as_ptr(&self) -> *const ApplicationEvent {
        self.events.as_ptr()
    }

    /// Current event-vector capacity for read-only ownership tests.
    pub fn capacity(&self) -> usize {
        self.events.capacity()
    }
}

impl Default for ApplicationEvents {
    fn default() -> Self {
        Self::without_retained_proofs(Vec::new())
    }
}

impl core::fmt::Debug for ApplicationEvents {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ApplicationEvents")
            .field("len", &self.events.len())
            .field("retained_proof_bound", &self.retained_proof.is_some())
            .field("events", &"<redacted>")
            .finish()
    }
}

impl core::ops::Index<usize> for ApplicationEvents {
    type Output = ApplicationEvent;

    fn index(&self, index: usize) -> &Self::Output {
        &self.events[index]
    }
}
