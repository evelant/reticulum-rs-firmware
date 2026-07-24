//! Allocation-free protocol state for one bounded NomadNet page fetch.
//!
//! This crate deliberately stops before Reticulum ownership. A sole node
//! adapter maps [`FetchAction`] values to path discovery, Link establishment,
//! and one anonymous RNS request, then reports only exactly correlated scalar
//! observations back to [`NomadClient`]. No Rete state, packet allocation,
//! executor, transport, or device API type crosses this boundary.
//!
//! The first product slice accepts the decoded response body supplied by the
//! Reticulum adapter when it contains at most [`MAX_PAGE_BYTES`] of valid UTF-8
//! Micron. It does not enable RNS Resources, Identify, forms, discovery, or
//! Micron rendering. Request timeout is an explicit two-owner transaction:
//! [`NomadClient::request_timeout_candidate`] observes a due request without
//! mutation, and [`NomadClient::confirm_request_timeout`] commits failure only
//! after the caller has canceled the exact native Reticulum request.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::fmt;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// Length of a complete Reticulum destination hash.
pub const DESTINATION_HASH_LENGTH: usize = 16;
/// Length of an RNS Link identifier.
pub const LINK_ID_LENGTH: usize = 16;
/// Length of a truncated RNS request identifier.
pub const REQUEST_ID_LENGTH: usize = 16;
/// Largest UTF-8 NomadNet request path retained by this slice.
pub const MAX_PAGE_PATH_BYTES: usize = 128;
/// Largest direct NomadNet page body admitted by this slice.
pub const MAX_PAGE_BYTES: usize = 400;
/// Initial NomadNet page requested by a minimal client.
pub const DEFAULT_INDEX_PATH: &str = "/page/index.mu";
/// Default time allowed for a confirmed outbound request to receive a response.
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;

const _: () = assert!(MAX_PAGE_PATH_BYTES <= u8::MAX as usize);
const _: () = assert!(MAX_PAGE_BYTES <= u16::MAX as usize);

/// Complete `nomadnetwork.node` destination hash.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DestinationHash([u8; DESTINATION_HASH_LENGTH]);

impl DestinationHash {
    /// Construct a destination hash from all protocol bytes.
    pub const fn new(bytes: [u8; DESTINATION_HASH_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all destination-hash bytes.
    pub const fn as_bytes(&self) -> &[u8; DESTINATION_HASH_LENGTH] {
        &self.0
    }
}

/// Complete identifier of one established RNS Link.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LinkId([u8; LINK_ID_LENGTH]);

impl LinkId {
    /// Construct a Link identifier from all protocol bytes.
    pub const fn new(bytes: [u8; LINK_ID_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all Link-identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; LINK_ID_LENGTH] {
        &self.0
    }
}

/// Truncated RNS request identifier used for response correlation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId([u8; REQUEST_ID_LENGTH]);

impl RequestId {
    /// Construct a request identifier from all protocol bytes.
    pub const fn new(bytes: [u8; REQUEST_ID_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all request-identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; REQUEST_ID_LENGTH] {
        &self.0
    }
}

/// Executor-independent monotonic time in whole milliseconds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicMillis(u64);

impl MonotonicMillis {
    /// Construct a monotonic-millisecond sample.
    pub const fn new(milliseconds: u64) -> Self {
        Self(milliseconds)
    }

    /// Return the raw monotonic-millisecond value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A validated, fixed-capacity NomadNet request path.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct PagePath {
    bytes: [u8; MAX_PAGE_PATH_BYTES],
    len: u8,
}

impl PagePath {
    /// Validate and copy one absolute UTF-8 NomadNet path.
    ///
    /// The path must begin with `/`, contain no NUL byte, and fit the bounded
    /// path storage. Interpretation beyond those transport-safe constraints is
    /// owned by the remote NomadNet node.
    pub fn new(path: &str) -> Result<Self, PathError> {
        let source = path.as_bytes();
        if source.is_empty() || source[0] != b'/' || source.contains(&0) {
            return Err(PathError::Invalid);
        }
        if source.len() > MAX_PAGE_PATH_BYTES {
            return Err(PathError::TooLong {
                actual: source.len(),
                maximum: MAX_PAGE_PATH_BYTES,
            });
        }

        let mut bytes = [0; MAX_PAGE_PATH_BYTES];
        bytes[..source.len()].copy_from_slice(source);
        Ok(Self {
            bytes,
            len: source.len() as u8,
        })
    }

    /// Construct the initial `/page/index.mu` request path.
    pub fn index() -> Self {
        Self::new(DEFAULT_INDEX_PATH).expect("the fixed index path fits PagePath")
    }

    /// Borrow the complete UTF-8 path.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("PagePath is constructed only from str")
    }

    /// Return the path length in bytes.
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether this path contains no bytes.
    ///
    /// A constructed path is never empty; this method is provided for normal
    /// collection-style inspection.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl fmt::Debug for PagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PagePath")
            .field(&self.as_str())
            .finish()
    }
}

/// Why a supplied NomadNet path cannot enter bounded protocol state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathError {
    /// The path was empty, relative, or contained a NUL byte.
    Invalid,
    /// The path exceeds fixed path storage.
    TooLong {
        /// Supplied path length in bytes.
        actual: usize,
        /// Largest admitted path length in bytes.
        maximum: usize,
    },
}

/// Configuration for one-fetch protocol state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FetchConfig {
    request_timeout_ms: u64,
}

impl FetchConfig {
    /// Construct a configuration with a nonzero request timeout.
    pub const fn new(request_timeout_ms: u64) -> Option<Self> {
        if request_timeout_ms == 0 {
            None
        } else {
            Some(Self { request_timeout_ms })
        }
    }

    /// Time allowed after confirmed dispatch before the request times out.
    pub const fn request_timeout_ms(self) -> u64 {
        self.request_timeout_ms
    }
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
        }
    }
}

/// One cached established Link and the remote destination it serves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CachedLink {
    destination: DestinationHash,
    link: LinkId,
}

impl CachedLink {
    /// Construct an exact destination-to-Link association.
    pub const fn new(destination: DestinationHash, link: LinkId) -> Self {
        Self { destination, link }
    }

    /// Remote destination served by the Link.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Established Link identifier.
    pub const fn link(self) -> LinkId {
        self.link
    }
}

/// Caller action needed to advance the active page fetch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchAction {
    /// Ask the Reticulum owner to begin path discovery.
    RequestPath {
        /// Explicit remote `nomadnetwork.node` destination.
        destination: DestinationHash,
    },
    /// Ask the Reticulum owner to prepare outbound Link establishment.
    EstablishLink {
        /// Explicit remote `nomadnetwork.node` destination.
        destination: DestinationHash,
    },
    /// Prepare one anonymous request with a MessagePack `nil` data value.
    ///
    /// The adapter owns the resulting packet and must return its exact request
    /// identifier through [`NomadClient::request_prepared`]. The request timer
    /// does not begin until [`NomadClient::confirm_request_dispatch`] reports
    /// the exact packet's first real interface dispatch.
    PrepareAnonymousRequest {
        /// Explicit remote `nomadnetwork.node` destination.
        destination: DestinationHash,
        /// Established Link on which to issue the request.
        link: LinkId,
        /// Exact requested page path.
        path: PagePath,
    },
}

/// Coarse externally visible phase of the one-fetch state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchPhase {
    /// No fetch or unread terminal outcome is retained.
    Idle,
    /// Path discovery is ready to dispatch or awaiting a result.
    PathLookup,
    /// Link establishment is ready to dispatch or awaiting a result.
    LinkEstablishment,
    /// The anonymous request is ready for packet preparation.
    RequestPreparation,
    /// A prepared request is awaiting dispatch confirmation or cancellation.
    AwaitingDispatchConfirmation,
    /// A confirmed request is awaiting its exactly correlated response.
    AwaitingResponse,
    /// A bounded UTF-8 page is retained until taken.
    Ready,
    /// A terminal failure is retained until taken.
    Failed,
}

/// Why a new fetch did not begin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartError {
    /// Another fetch or unread terminal outcome already owns the single slot.
    Busy,
}

/// Failure stage reported by the Link-owning adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkFailureStage {
    /// An outbound Link could not be prepared.
    Preparation,
    /// The Link request could not be dispatched.
    Dispatch,
    /// The remote Link did not establish successfully.
    Establishment,
    /// A previously established Link closed or disappeared.
    Closed,
    /// Request preparation discovered that a cached Link is absent or inactive.
    Unavailable,
}

/// Adapter-owned diagnostic for a Link failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkFailure {
    stage: LinkFailureStage,
    code: u16,
}

impl LinkFailure {
    /// Construct a Link failure from its semantic stage and bounded local code.
    pub const fn new(stage: LinkFailureStage, code: u16) -> Self {
        Self { stage, code }
    }

    /// Stage at which Link processing failed.
    pub const fn stage(self) -> LinkFailureStage {
        self.stage
    }

    /// Adapter-defined bounded diagnostic code.
    pub const fn code(self) -> u16 {
        self.code
    }
}

/// Failure stage reported by the request-owning adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestFailureStage {
    /// The RNS request packet could not be prepared.
    Preparation,
    /// The prepared request could not be dispatched.
    Dispatch,
    /// The remote request operation failed after dispatch.
    Remote,
}

/// Adapter-owned diagnostic for a request failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestFailure {
    stage: RequestFailureStage,
    code: u16,
}

impl RequestFailure {
    /// Construct a request failure from its semantic stage and bounded code.
    pub const fn new(stage: RequestFailureStage, code: u16) -> Self {
        Self { stage, code }
    }

    /// Stage at which request processing failed.
    pub const fn stage(self) -> RequestFailureStage {
        self.stage
    }

    /// Adapter-defined bounded diagnostic code.
    pub const fn code(self) -> u16 {
        self.code
    }
}

/// Terminal failure for one page fetch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchFailure {
    /// Reticulum path discovery completed without a usable path.
    NoPath {
        /// Destination for which no path became available.
        destination: DestinationHash,
    },
    /// Link preparation, dispatch, establishment, or retention failed.
    Link {
        /// Destination whose Link failed.
        destination: DestinationHash,
        /// Typed adapter diagnostic.
        failure: LinkFailure,
    },
    /// Request preparation, dispatch, or remote processing failed.
    Request {
        /// Link on which the request was or would have been issued.
        link: LinkId,
        /// Exact request ID when packet preparation had completed.
        request: Option<RequestId>,
        /// Typed adapter diagnostic.
        failure: RequestFailure,
    },
    /// A confirmed request exceeded its bounded response window.
    Timeout {
        /// Exact Link carrying the timed-out request.
        link: LinkId,
        /// Exact timed-out request.
        request: RequestId,
        /// First real dispatch time supplied by the adapter.
        dispatched_at: MonotonicMillis,
        /// Saturating response deadline.
        deadline: MonotonicMillis,
    },
    /// The decoded response body exceeds the direct bound.
    TooLarge {
        /// Decoded response body size.
        actual: usize,
        /// Largest admitted direct body.
        maximum: usize,
    },
    /// The decoded response body is not valid UTF-8.
    InvalidUtf8,
}

/// Correlation disposition for an asynchronous adapter observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationDisposition {
    /// The exact active operation consumed the observation.
    Applied,
    /// The observation names another destination, Link, or request.
    Unrelated,
    /// No active operation can consume this observation in the current phase.
    WrongPhase,
}

/// Why an explicit action-control transition was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlError {
    /// The active operation is not at the required transition.
    WrongPhase,
    /// The supplied destination does not match the active destination.
    DestinationMismatch,
    /// The supplied Link does not match the active Link.
    LinkMismatch,
    /// The supplied request does not match the prepared request.
    RequestMismatch,
    /// An idempotent dispatch confirmation supplied a different timestamp.
    DispatchTimeMismatch,
    /// A timeout candidate names a different response deadline.
    DeadlineMismatch,
}

/// Prepared request awaiting exact dispatch confirmation or cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedRequest {
    destination: DestinationHash,
    link: LinkId,
    request: RequestId,
    path: PagePath,
}

impl PreparedRequest {
    /// Remote destination of the prepared request.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Link on which the request packet was prepared.
    pub const fn link(self) -> LinkId {
        self.link
    }

    /// Exact RNS request identifier derived during preparation.
    pub const fn request(self) -> RequestId {
        self.request
    }

    /// Exact requested page path.
    pub const fn path(self) -> PagePath {
        self.path
    }
}

/// Exact confirmed request whose response deadline is due.
///
/// This is only a read-only protocol observation. It does not release native
/// Reticulum request state and does not mutate [`NomadClient`]. The caller must
/// first cancel the exact confirmed native request, then return this unchanged
/// candidate to [`NomadClient::confirm_request_timeout`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestTimeoutCandidate {
    link: LinkId,
    request: RequestId,
    dispatched_at: MonotonicMillis,
    deadline: MonotonicMillis,
}

impl RequestTimeoutCandidate {
    /// Link carrying the confirmed request.
    pub const fn link(self) -> LinkId {
        self.link
    }

    /// Exact confirmed request identifier.
    pub const fn request(self) -> RequestId {
        self.request
    }

    /// First real interface-dispatch time.
    pub const fn dispatched_at(self) -> MonotonicMillis {
        self.dispatched_at
    }

    /// Saturating response deadline observed as due.
    pub const fn deadline(self) -> MonotonicMillis {
        self.deadline
    }
}

/// Fixed-capacity valid UTF-8 Micron page bytes.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Page {
    bytes: [u8; MAX_PAGE_BYTES],
    len: u16,
}

impl Page {
    fn from_utf8(bytes: &[u8]) -> Result<Self, FetchFailure> {
        if bytes.len() > MAX_PAGE_BYTES {
            return Err(FetchFailure::TooLarge {
                actual: bytes.len(),
                maximum: MAX_PAGE_BYTES,
            });
        }
        if core::str::from_utf8(bytes).is_err() {
            return Err(FetchFailure::InvalidUtf8);
        }
        let mut retained = [0; MAX_PAGE_BYTES];
        retained[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            bytes: retained,
            len: bytes.len() as u16,
        })
    }

    /// Borrow the complete page body.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// Borrow the complete page body as UTF-8.
    pub fn as_str(&self) -> Result<&str, core::str::Utf8Error> {
        core::str::from_utf8(self.as_bytes())
    }

    /// Return the retained page length in bytes.
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the valid page body is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl fmt::Debug for Page {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Page")
            .field("len", &self.len())
            .field("text", &self.as_str())
            .finish()
    }
}

/// Terminal result retained by the one-fetch slot.
#[allow(
    clippy::large_enum_variant,
    reason = "the no-alloc outcome must own either the fixed 400-byte page or a scalar failure"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchOutcome {
    /// One bounded UTF-8 Micron page is ready for the caller.
    Ready(Page),
    /// The fetch ended with a typed failure.
    Failed(FetchFailure),
}

#[allow(
    clippy::large_enum_variant,
    reason = "the tagged union lets the no-alloc owner reuse one fixed buffer across all phases"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FetchState {
    Idle,
    NeedPath {
        destination: DestinationHash,
        path: PagePath,
    },
    WaitingPath {
        destination: DestinationHash,
        path: PagePath,
    },
    NeedLink {
        destination: DestinationHash,
        path: PagePath,
    },
    WaitingLink {
        destination: DestinationHash,
        path: PagePath,
        link: LinkId,
    },
    NeedRequest {
        destination: DestinationHash,
        path: PagePath,
        link: LinkId,
    },
    Prepared {
        request: PreparedRequest,
    },
    WaitingResponse {
        destination: DestinationHash,
        path: PagePath,
        link: LinkId,
        request: RequestId,
        dispatched_at: MonotonicMillis,
        deadline: MonotonicMillis,
    },
    Ready(Page),
    Failed(FetchFailure),
}

/// Allocation-free owner of one active or unread terminal NomadNet fetch.
pub struct NomadClient {
    config: FetchConfig,
    cached_link: Option<CachedLink>,
    state: FetchState,
}

impl NomadClient {
    /// Construct an idle client with no cached Link.
    pub const fn new(config: FetchConfig) -> Self {
        Self {
            config,
            cached_link: None,
            state: FetchState::Idle,
        }
    }

    /// Return the current coarse fetch phase.
    pub const fn phase(&self) -> FetchPhase {
        match self.state {
            FetchState::Idle => FetchPhase::Idle,
            FetchState::NeedPath { .. } | FetchState::WaitingPath { .. } => FetchPhase::PathLookup,
            FetchState::NeedLink { .. } | FetchState::WaitingLink { .. } => {
                FetchPhase::LinkEstablishment
            }
            FetchState::NeedRequest { .. } => FetchPhase::RequestPreparation,
            FetchState::Prepared { .. } => FetchPhase::AwaitingDispatchConfirmation,
            FetchState::WaitingResponse { .. } => FetchPhase::AwaitingResponse,
            FetchState::Ready(_) => FetchPhase::Ready,
            FetchState::Failed(_) => FetchPhase::Failed,
        }
    }

    /// Return the single cached established Link, when present.
    pub const fn cached_link(&self) -> Option<CachedLink> {
        self.cached_link
    }

    /// Seed or replace the single established-Link cache while idle.
    ///
    /// The caller must already own and have authenticated this Link. Active or
    /// unread terminal state rejects replacement with [`StartError::Busy`].
    pub fn seed_cached_link(&mut self, link: CachedLink) -> Result<(), StartError> {
        if !matches!(self.state, FetchState::Idle) {
            return Err(StartError::Busy);
        }
        self.cached_link = Some(link);
        Ok(())
    }

    /// Begin one page fetch for an explicit destination and validated path.
    pub fn start(
        &mut self,
        destination: DestinationHash,
        path: PagePath,
    ) -> Result<(), StartError> {
        if !matches!(self.state, FetchState::Idle) {
            return Err(StartError::Busy);
        }

        self.state = match self.cached_link {
            Some(cached) if cached.destination == destination => FetchState::NeedRequest {
                destination,
                path,
                link: cached.link,
            },
            _ => FetchState::NeedPath { destination, path },
        };
        Ok(())
    }

    /// Return the currently required caller action, if one is ready.
    pub const fn action(&self) -> Option<FetchAction> {
        match self.state {
            FetchState::NeedPath { destination, .. } => {
                Some(FetchAction::RequestPath { destination })
            }
            FetchState::NeedLink { destination, .. } => {
                Some(FetchAction::EstablishLink { destination })
            }
            FetchState::NeedRequest {
                destination,
                path,
                link,
            } => Some(FetchAction::PrepareAnonymousRequest {
                destination,
                link,
                path,
            }),
            _ => None,
        }
    }

    /// Confirm that the exact path-discovery action entered its owner.
    pub fn confirm_path_request(
        &mut self,
        destination: DestinationHash,
    ) -> Result<(), ControlError> {
        match self.state {
            FetchState::NeedPath {
                destination: expected,
                path,
            } => {
                if destination != expected {
                    return Err(ControlError::DestinationMismatch);
                }
                self.state = FetchState::WaitingPath { destination, path };
                Ok(())
            }
            FetchState::WaitingPath {
                destination: expected,
                ..
            } => {
                if destination == expected {
                    Ok(())
                } else {
                    Err(ControlError::DestinationMismatch)
                }
            }
            _ => Err(ControlError::WrongPhase),
        }
    }

    /// Cancel the exact in-flight path action so it can be prepared again.
    pub fn cancel_path_request(
        &mut self,
        destination: DestinationHash,
    ) -> Result<(), ControlError> {
        match self.state {
            FetchState::WaitingPath {
                destination: expected,
                path,
            } => {
                if destination != expected {
                    return Err(ControlError::DestinationMismatch);
                }
                self.state = FetchState::NeedPath { destination, path };
                Ok(())
            }
            _ => Err(ControlError::WrongPhase),
        }
    }

    /// Report that an exact path-discovery operation found a usable path.
    pub fn path_available(&mut self, destination: DestinationHash) -> ObservationDisposition {
        match self.state {
            FetchState::WaitingPath {
                destination: expected,
                path,
            } => {
                if destination != expected {
                    return ObservationDisposition::Unrelated;
                }
                self.state = FetchState::NeedLink { destination, path };
                ObservationDisposition::Applied
            }
            _ => ObservationDisposition::WrongPhase,
        }
    }

    /// Report that an exact path-discovery operation ended without a path.
    pub fn path_unavailable(&mut self, destination: DestinationHash) -> ObservationDisposition {
        match self.state {
            FetchState::WaitingPath {
                destination: expected,
                ..
            } => {
                if destination != expected {
                    return ObservationDisposition::Unrelated;
                }
                self.state = FetchState::Failed(FetchFailure::NoPath { destination });
                ObservationDisposition::Applied
            }
            _ => ObservationDisposition::WrongPhase,
        }
    }

    /// Confirm that the exact Link-establishment action entered its owner.
    pub fn confirm_link_request(
        &mut self,
        destination: DestinationHash,
        link: LinkId,
    ) -> Result<(), ControlError> {
        match self.state {
            FetchState::NeedLink {
                destination: expected,
                path,
            } => {
                if destination != expected {
                    return Err(ControlError::DestinationMismatch);
                }
                self.state = FetchState::WaitingLink {
                    destination,
                    path,
                    link,
                };
                Ok(())
            }
            FetchState::WaitingLink {
                destination: expected,
                link: expected_link,
                ..
            } => {
                if destination != expected {
                    Err(ControlError::DestinationMismatch)
                } else if link != expected_link {
                    Err(ControlError::LinkMismatch)
                } else {
                    Ok(())
                }
            }
            _ => Err(ControlError::WrongPhase),
        }
    }

    /// Cancel the exact in-flight Link action so it can be prepared again.
    pub fn cancel_link_request(
        &mut self,
        destination: DestinationHash,
        link: LinkId,
    ) -> Result<(), ControlError> {
        match self.state {
            FetchState::WaitingLink {
                destination: expected,
                path,
                link: expected_link,
            } => {
                if destination != expected {
                    return Err(ControlError::DestinationMismatch);
                }
                if link != expected_link {
                    return Err(ControlError::LinkMismatch);
                }
                self.state = FetchState::NeedLink { destination, path };
                Ok(())
            }
            _ => Err(ControlError::WrongPhase),
        }
    }

    /// Report that the exact destination established one outbound Link.
    pub fn link_established(
        &mut self,
        destination: DestinationHash,
        link: LinkId,
    ) -> ObservationDisposition {
        match self.state {
            FetchState::WaitingLink {
                destination: expected,
                path,
                link: expected_link,
            } => {
                if destination != expected {
                    return ObservationDisposition::Unrelated;
                }
                if link != expected_link {
                    return ObservationDisposition::Unrelated;
                }
                self.cached_link = Some(CachedLink::new(destination, link));
                self.state = FetchState::NeedRequest {
                    destination,
                    path,
                    link,
                };
                ObservationDisposition::Applied
            }
            _ => ObservationDisposition::WrongPhase,
        }
    }

    /// Report terminal Link preparation failure before a Link ID existed.
    pub fn link_preparation_failed(
        &mut self,
        destination: DestinationHash,
        failure: LinkFailure,
    ) -> ObservationDisposition {
        match self.state {
            FetchState::NeedLink {
                destination: expected,
                ..
            } => {
                if destination != expected {
                    return ObservationDisposition::Unrelated;
                }
                self.state = FetchState::Failed(FetchFailure::Link {
                    destination,
                    failure,
                });
                ObservationDisposition::Applied
            }
            _ => ObservationDisposition::WrongPhase,
        }
    }

    /// Report terminal failure for an exact prepared Link.
    pub fn link_failed(
        &mut self,
        destination: DestinationHash,
        link: LinkId,
        failure: LinkFailure,
    ) -> ObservationDisposition {
        match self.state {
            FetchState::WaitingLink {
                destination: expected_destination,
                link: expected_link,
                ..
            } => {
                if destination != expected_destination || link != expected_link {
                    return ObservationDisposition::Unrelated;
                }
                self.state = FetchState::Failed(FetchFailure::Link {
                    destination,
                    failure,
                });
                ObservationDisposition::Applied
            }
            _ => ObservationDisposition::WrongPhase,
        }
    }

    /// Report that an established Link closed or disappeared.
    ///
    /// An exact cached Link is forgotten. If the active fetch still depends on
    /// it, the fetch also ends with a typed closed-Link failure.
    pub fn link_closed(&mut self, link: LinkId, code: u16) -> ObservationDisposition {
        let cached = self.cached_link;
        if cached.is_some_and(|candidate| candidate.link == link) {
            self.cached_link = None;
        }

        let destination = match self.state {
            FetchState::WaitingLink {
                destination,
                link: expected,
                ..
            }
            | FetchState::NeedRequest {
                destination,
                link: expected,
                ..
            }
            | FetchState::WaitingResponse {
                destination,
                link: expected,
                ..
            } if expected == link => Some(destination),
            FetchState::Prepared { request } if request.link == link => Some(request.destination),
            _ => None,
        };

        if let Some(destination) = destination {
            self.state = FetchState::Failed(FetchFailure::Link {
                destination,
                failure: LinkFailure::new(LinkFailureStage::Closed, code),
            });
            ObservationDisposition::Applied
        } else if cached.is_some_and(|candidate| candidate.link == link) {
            ObservationDisposition::Applied
        } else {
            ObservationDisposition::Unrelated
        }
    }

    /// Report that request preparation found the exact cached Link unavailable.
    ///
    /// This observation is intended for an adapter's `LinkNotFound` or
    /// `LinkNotActive` preparation result. It invalidates the matching cached
    /// Link before retaining a terminal outcome. After that outcome is taken,
    /// a later fetch for the same destination starts with path discovery
    /// instead of retrying the stale Link.
    pub fn request_link_unavailable(&mut self, link: LinkId, code: u16) -> ObservationDisposition {
        match self.state {
            FetchState::NeedRequest {
                destination,
                link: expected,
                ..
            } => {
                if link != expected {
                    return ObservationDisposition::Unrelated;
                }
                if self
                    .cached_link
                    .is_some_and(|candidate| candidate.link == link)
                {
                    self.cached_link = None;
                }
                self.state = FetchState::Failed(FetchFailure::Link {
                    destination,
                    failure: LinkFailure::new(LinkFailureStage::Unavailable, code),
                });
                ObservationDisposition::Applied
            }
            _ => ObservationDisposition::WrongPhase,
        }
    }

    /// Bind the exact request ID returned by anonymous packet preparation.
    pub fn request_prepared(
        &mut self,
        link: LinkId,
        request: RequestId,
    ) -> Result<PreparedRequest, ControlError> {
        match self.state {
            FetchState::NeedRequest {
                destination,
                path,
                link: expected,
            } => {
                if link != expected {
                    return Err(ControlError::LinkMismatch);
                }
                let prepared = PreparedRequest {
                    destination,
                    link,
                    request,
                    path,
                };
                self.state = FetchState::Prepared { request: prepared };
                Ok(prepared)
            }
            FetchState::Prepared { request: prepared } => {
                if link != prepared.link {
                    Err(ControlError::LinkMismatch)
                } else if request != prepared.request {
                    Err(ControlError::RequestMismatch)
                } else {
                    Ok(prepared)
                }
            }
            _ => Err(ControlError::WrongPhase),
        }
    }

    /// Return the exact prepared request awaiting dispatch disposition.
    pub const fn prepared_request(&self) -> Option<PreparedRequest> {
        match self.state {
            FetchState::Prepared { request } => Some(request),
            _ => None,
        }
    }

    /// Confirm the exact prepared request's first real interface dispatch.
    ///
    /// This transition, and only this transition, starts the response timeout.
    /// Repeating the same confirmation is idempotent.
    ///
    /// The caller must confirm the adapter-owned native request first while
    /// this client remains [`FetchPhase::AwaitingDispatchConfirmation`]. Only
    /// then may it call this method. An error leaves the prepared state
    /// unchanged: the caller must cancel the native confirmed request before
    /// reporting the exact failure through
    /// [`Self::request_dispatch_failed`].
    pub fn confirm_request_dispatch(
        &mut self,
        link: LinkId,
        request: RequestId,
        dispatched_at: MonotonicMillis,
    ) -> Result<(), ControlError> {
        match self.state {
            FetchState::Prepared { request: prepared } => {
                ensure_request_correlation(prepared, link, request)?;
                let deadline = MonotonicMillis::new(
                    dispatched_at
                        .get()
                        .saturating_add(self.config.request_timeout_ms),
                );
                self.state = FetchState::WaitingResponse {
                    destination: prepared.destination,
                    path: prepared.path,
                    link,
                    request,
                    dispatched_at,
                    deadline,
                };
                Ok(())
            }
            FetchState::WaitingResponse {
                link: expected_link,
                request: expected_request,
                dispatched_at: expected_time,
                ..
            } => {
                if link != expected_link {
                    Err(ControlError::LinkMismatch)
                } else if request != expected_request {
                    Err(ControlError::RequestMismatch)
                } else if dispatched_at != expected_time {
                    Err(ControlError::DispatchTimeMismatch)
                } else {
                    Ok(())
                }
            }
            _ => Err(ControlError::WrongPhase),
        }
    }

    /// Cancel an undispatched exact request and return to packet preparation.
    ///
    /// The Reticulum adapter must cancel its own pending request before calling
    /// this transition. No response timeout exists before or after cancellation.
    pub fn cancel_request_dispatch(
        &mut self,
        link: LinkId,
        request: RequestId,
    ) -> Result<(), ControlError> {
        match self.state {
            FetchState::Prepared { request: prepared } => {
                ensure_request_correlation(prepared, link, request)?;
                self.state = FetchState::NeedRequest {
                    destination: prepared.destination,
                    path: prepared.path,
                    link,
                };
                Ok(())
            }
            _ => Err(ControlError::WrongPhase),
        }
    }

    /// Report terminal non-Link request preparation failure before an ID existed.
    ///
    /// An adapter that reports `LinkNotFound` or `LinkNotActive` must instead
    /// use [`Self::request_link_unavailable`] so the stale cached Link cannot
    /// poison later fetches.
    pub fn request_preparation_failed(
        &mut self,
        link: LinkId,
        failure: RequestFailure,
    ) -> ObservationDisposition {
        match self.state {
            FetchState::NeedRequest { link: expected, .. } => {
                if link != expected {
                    return ObservationDisposition::Unrelated;
                }
                self.state = FetchState::Failed(FetchFailure::Request {
                    link,
                    request: None,
                    failure,
                });
                ObservationDisposition::Applied
            }
            _ => ObservationDisposition::WrongPhase,
        }
    }

    /// Report terminal dispatch failure for an exact prepared request.
    ///
    /// When native dispatch confirmation succeeded but
    /// [`Self::confirm_request_dispatch`] rejected the scalar correlation, the
    /// caller must cancel native confirmed request state before calling this
    /// method. Until this exact transition applies, Nomad state remains
    /// prepared and can still correlate that rollback.
    pub fn request_dispatch_failed(
        &mut self,
        link: LinkId,
        request: RequestId,
        failure: RequestFailure,
    ) -> ObservationDisposition {
        match self.state {
            FetchState::Prepared { request: prepared } => {
                if link != prepared.link || request != prepared.request {
                    return ObservationDisposition::Unrelated;
                }
                self.state = FetchState::Failed(FetchFailure::Request {
                    link,
                    request: Some(request),
                    failure,
                });
                ObservationDisposition::Applied
            }
            _ => ObservationDisposition::WrongPhase,
        }
    }

    /// Report remote failure for an exact confirmed request.
    pub fn request_failed(
        &mut self,
        link: LinkId,
        request: RequestId,
        failure: RequestFailure,
    ) -> ObservationDisposition {
        match self.state {
            FetchState::WaitingResponse {
                link: expected_link,
                request: expected_request,
                ..
            } => {
                if link != expected_link || request != expected_request {
                    return ObservationDisposition::Unrelated;
                }
                self.state = FetchState::Failed(FetchFailure::Request {
                    link,
                    request: Some(request),
                    failure,
                });
                ObservationDisposition::Applied
            }
            _ => ObservationDisposition::WrongPhase,
        }
    }

    /// Consume one exact confirmed decoded response body.
    ///
    /// The Reticulum adapter owns MessagePack response-envelope parsing and
    /// supplies only its unwrapped data bytes here. The body must be valid
    /// UTF-8 and at most [`MAX_PAGE_BYTES`]. Correlated invalid or oversized
    /// responses become terminal typed failures.
    pub fn response_received(
        &mut self,
        link: LinkId,
        request: RequestId,
        response_body: &[u8],
    ) -> ObservationDisposition {
        match self.state {
            FetchState::WaitingResponse {
                link: expected_link,
                request: expected_request,
                ..
            } => {
                if link != expected_link || request != expected_request {
                    return ObservationDisposition::Unrelated;
                }
                self.state = match Page::from_utf8(response_body) {
                    Ok(page) => FetchState::Ready(page),
                    Err(failure) => FetchState::Failed(failure),
                };
                ObservationDisposition::Applied
            }
            _ => ObservationDisposition::WrongPhase,
        }
    }

    /// Observe the exact confirmed request when its response deadline is due.
    ///
    /// This read-only operation cannot release Reticulum request state or
    /// change the fetch phase. A returned candidate must remain unchanged
    /// while the caller cancels the exact native confirmed request. Only a
    /// successful native cancellation authorizes
    /// [`Self::confirm_request_timeout`].
    pub fn request_timeout_candidate(
        &self,
        now: MonotonicMillis,
    ) -> Option<RequestTimeoutCandidate> {
        match self.state {
            FetchState::WaitingResponse {
                link,
                request,
                dispatched_at,
                deadline,
                ..
            } if now >= deadline => Some(RequestTimeoutCandidate {
                link,
                request,
                dispatched_at,
                deadline,
            }),
            _ => None,
        }
    }

    /// Commit one due timeout after exact native request cancellation.
    ///
    /// The complete candidate is revalidated against current state. Any stale
    /// phase, Link, request, dispatch time, or deadline leaves state unchanged.
    /// Callers must not invoke this transition unless cancellation of the
    /// candidate's exact native confirmed request has already succeeded.
    pub fn confirm_request_timeout(
        &mut self,
        candidate: RequestTimeoutCandidate,
    ) -> Result<(), ControlError> {
        match self.state {
            FetchState::WaitingResponse {
                link,
                request,
                dispatched_at,
                deadline,
                ..
            } => {
                if candidate.link != link {
                    return Err(ControlError::LinkMismatch);
                }
                if candidate.request != request {
                    return Err(ControlError::RequestMismatch);
                }
                if candidate.dispatched_at != dispatched_at {
                    return Err(ControlError::DispatchTimeMismatch);
                }
                if candidate.deadline != deadline {
                    return Err(ControlError::DeadlineMismatch);
                }
                self.state = FetchState::Failed(FetchFailure::Timeout {
                    link,
                    request,
                    dispatched_at,
                    deadline,
                });
                Ok(())
            }
            _ => Err(ControlError::WrongPhase),
        }
    }

    /// Borrow the ready page without releasing the single fetch slot.
    pub const fn ready_page(&self) -> Option<&Page> {
        match &self.state {
            FetchState::Ready(page) => Some(page),
            _ => None,
        }
    }

    /// Return the terminal failure without releasing the single fetch slot.
    pub const fn failure(&self) -> Option<FetchFailure> {
        match self.state {
            FetchState::Failed(failure) => Some(failure),
            _ => None,
        }
    }

    /// Take one terminal outcome and return the fetch slot to idle.
    ///
    /// A still-open cached Link is retained for a later fetch.
    pub fn take_outcome(&mut self) -> Option<FetchOutcome> {
        let outcome = match self.state {
            FetchState::Ready(page) => FetchOutcome::Ready(page),
            FetchState::Failed(failure) => FetchOutcome::Failed(failure),
            _ => return None,
        };
        self.state = FetchState::Idle;
        Some(outcome)
    }
}

fn ensure_request_correlation(
    prepared: PreparedRequest,
    link: LinkId,
    request: RequestId,
) -> Result<(), ControlError> {
    if link != prepared.link {
        Err(ControlError::LinkMismatch)
    } else if request != prepared.request {
        Err(ControlError::RequestMismatch)
    } else {
        Ok(())
    }
}
