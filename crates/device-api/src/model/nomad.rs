//! NomadNet page-fetch model.

use super::*;

/// Begin one bounded authenticated NomadNet page fetch.
#[cfg(feature = "nomad")]
pub const OP_NOMAD_FETCH_START: u16 = 0xf008;
/// Poll one principal-owned bounded NomadNet page fetch.
#[cfg(feature = "nomad")]
pub const OP_NOMAD_FETCH_POLL: u16 = 0xf009;
/// Validated borrowed UTF-8 NomadNet request path.
///
/// The path is absolute, contains no NUL byte, and remains borrowed directly
/// from the decoded request message.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NomadPagePath<'a>(&'a str);

#[cfg(feature = "nomad")]
impl<'a> NomadPagePath<'a> {
    /// Validate one bounded absolute NomadNet path.
    pub fn new(path: &'a str) -> Result<Self, InvalidNomadPagePath> {
        let bytes = path.as_bytes();
        if bytes.is_empty() || bytes[0] != b'/' || bytes.contains(&0) {
            return Err(InvalidNomadPagePath::Invalid);
        }
        if bytes.len() > MAX_NOMAD_PAGE_PATH_BYTES {
            return Err(InvalidNomadPagePath::TooLong {
                actual: bytes.len(),
            });
        }
        Ok(Self(path))
    }

    /// Borrow the complete validated path.
    pub const fn as_str(self) -> &'a str {
        self.0
    }

    /// Path length in UTF-8 bytes.
    pub const fn len(self) -> usize {
        self.0.len()
    }

    /// Whether the path is empty.
    ///
    /// A constructed path is never empty; this method supports conventional
    /// collection-style inspection.
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }
}

/// Why a NomadNet request path was rejected.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidNomadPagePath {
    /// The path was empty, relative, or contained a NUL byte.
    Invalid,
    /// The path exceeded the fixed UTF-8 byte limit.
    TooLong {
        /// Rejected path length in bytes.
        actual: usize,
    },
}

#[cfg(feature = "nomad")]
impl InvalidNomadPagePath {
    /// Largest accepted UTF-8 path length.
    pub const fn maximum(self) -> usize {
        MAX_NOMAD_PAGE_PATH_BYTES
    }
}

/// Caller-selected Unix timestamp for one anonymous NomadNet request.
///
/// The inclusive range is lossless in JSON and JavaScript integer
/// interchange. Conversion to the Reticulum binary64-seconds wire timestamp
/// can lose millisecond precision at extreme dates.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NomadRequestTimestampUnixMs(u64);

#[cfg(feature = "nomad")]
impl NomadRequestTimestampUnixMs {
    /// Validate a nonzero whole-millisecond Unix timestamp.
    pub const fn new(value: u64) -> Result<Self, InvalidNomadRequestTimestamp> {
        if value == 0 || value > MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS {
            Err(InvalidNomadRequestTimestamp { actual: value })
        } else {
            Ok(Self(value))
        }
    }

    /// Complete validated millisecond value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A NomadNet request timestamp was zero or outside the exact millisecond range.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidNomadRequestTimestamp {
    actual: u64,
}

#[cfg(feature = "nomad")]
impl InvalidNomadRequestTimestamp {
    /// Rejected millisecond value.
    pub const fn actual(self) -> u64 {
        self.actual
    }

    /// Largest accepted millisecond value.
    pub const fn maximum(self) -> u64 {
        MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS
    }
}

/// Opaque boot-scoped identifier for one principal-owned NomadNet fetch.
///
/// The first eight bytes identify the boot incarnation. The final eight bytes
/// contain a nonzero big-endian sequence. Clients compare and return all 16
/// bytes without deriving authority from either component.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NomadFetchId([u8; 16]);

#[cfg(feature = "nomad")]
impl NomadFetchId {
    /// Construct a boot-scoped identifier from its two exact components.
    pub const fn new(incarnation: [u8; 8], sequence: u64) -> Result<Self, InvalidNomadFetchId> {
        if sequence == 0 {
            return Err(InvalidNomadFetchId);
        }
        let sequence = sequence.to_be_bytes();
        Ok(Self([
            incarnation[0],
            incarnation[1],
            incarnation[2],
            incarnation[3],
            incarnation[4],
            incarnation[5],
            incarnation[6],
            incarnation[7],
            sequence[0],
            sequence[1],
            sequence[2],
            sequence[3],
            sequence[4],
            sequence[5],
            sequence[6],
            sequence[7],
        ]))
    }

    /// Validate all opaque bytes received from the wire.
    pub const fn from_bytes(bytes: [u8; 16]) -> Result<Self, InvalidNomadFetchId> {
        let sequence = u64::from_be_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]);
        if sequence == 0 {
            Err(InvalidNomadFetchId)
        } else {
            Ok(Self(bytes))
        }
    }

    /// Borrow all opaque identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Boot-incarnation component.
    pub const fn incarnation(self) -> [u8; 8] {
        [
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5], self.0[6], self.0[7],
        ]
    }

    /// Nonzero sequence component.
    pub const fn sequence(self) -> u64 {
        u64::from_be_bytes([
            self.0[8], self.0[9], self.0[10], self.0[11], self.0[12], self.0[13], self.0[14],
            self.0[15],
        ])
    }
}

/// A fetch identifier's sequence component was zero.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidNomadFetchId;

/// Complete borrowed request to begin one bounded NomadNet page fetch.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadFetchStartRequest<'a> {
    destination: DestinationHash,
    path: NomadPagePath<'a>,
    timestamp_unix_ms: NomadRequestTimestampUnixMs,
    idempotency_key: IdempotencyKey,
}

#[cfg(feature = "nomad")]
impl<'a> NomadFetchStartRequest<'a> {
    /// Construct one invariant-preserving start request.
    pub const fn new(
        destination: DestinationHash,
        path: NomadPagePath<'a>,
        timestamp_unix_ms: NomadRequestTimestampUnixMs,
        idempotency_key: IdempotencyKey,
    ) -> Self {
        Self {
            destination,
            path,
            timestamp_unix_ms,
            idempotency_key,
        }
    }

    /// Complete remote `nomadnetwork.node` destination hash.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Exact validated remote page path.
    pub const fn path(self) -> NomadPagePath<'a> {
        self.path
    }

    /// Caller-selected request timestamp.
    pub const fn timestamp_unix_ms(self) -> NomadRequestTimestampUnixMs {
        self.timestamp_unix_ms
    }

    /// Principal-scoped idempotency key.
    pub const fn idempotency_key(self) -> IdempotencyKey {
        self.idempotency_key
    }
}

/// Request to poll one principal-owned NomadNet fetch.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadFetchPollRequest {
    /// Device-assigned fetch identifier.
    pub id: NomadFetchId,
}

/// Acceptance result for one authenticated NomadNet page fetch.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadFetchStartAccepted {
    /// Device-assigned principal-owned fetch identifier.
    pub id: NomadFetchId,
    /// Whether this request created a fresh fetch or replayed an identical one.
    pub outcome: NomadFetchStartOutcome,
}

/// Principal-scoped idempotency outcome for a successful fetch start.
///
/// This is a closed wire vocabulary.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NomadFetchStartOutcome {
    /// A fresh fetch was accepted.
    Accepted = 0,
    /// An identical request for this principal and idempotency key was replayed.
    Replayed = 1,
}

#[cfg(feature = "nomad")]
impl NomadFetchStartOutcome {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Non-terminal phase returned by a NomadNet fetch poll.
///
/// This is a closed wire vocabulary.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NomadFetchPhase {
    /// Path discovery is in progress.
    PathLookup = 0,
    /// Link establishment is in progress.
    LinkEstablishment = 1,
    /// The anonymous request is being prepared.
    RequestPreparation = 2,
    /// A prepared request awaits first-dispatch confirmation.
    AwaitingDispatchConfirmation = 3,
    /// A confirmed request awaits its exactly correlated response.
    AwaitingResponse = 4,
}

#[cfg(feature = "nomad")]
impl NomadFetchPhase {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Stable terminal failure returned by a NomadNet fetch poll.
///
/// Link identifiers, request identifiers, and adapter-local diagnostic codes
/// remain inside the product owner. This is a closed wire vocabulary.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NomadFetchFailure {
    /// Path discovery completed without a usable path.
    NoPath = 0,
    /// Link preparation, dispatch, establishment, or retention failed.
    Link = 1,
    /// Request preparation, dispatch, or remote processing failed.
    Request = 2,
    /// A confirmed request exceeded its bounded response window.
    Timeout = 3,
    /// The decoded page exceeded the fixed direct-response limit.
    PageTooLarge = 4,
    /// The decoded page was not valid UTF-8 Micron text.
    InvalidUtf8 = 5,
    /// The product owner detected an internal invariant or backend failure.
    Internal = 6,
}

#[cfg(feature = "nomad")]
impl NomadFetchFailure {
    /// Stable numeric wire representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// One owned bounded valid UTF-8 Micron page.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct NomadPage {
    bytes: [u8; MAX_NOMAD_PAGE_BYTES],
    len: u16,
}

#[cfg(feature = "nomad")]
impl NomadPage {
    /// Validate and copy one complete page body.
    pub fn new(bytes: &[u8]) -> Result<Self, InvalidNomadPage> {
        if bytes.len() > MAX_NOMAD_PAGE_BYTES {
            return Err(InvalidNomadPage::TooLarge {
                actual: bytes.len(),
            });
        }
        if core::str::from_utf8(bytes).is_err() {
            return Err(InvalidNomadPage::InvalidUtf8);
        }
        let mut owned = [0_u8; MAX_NOMAD_PAGE_BYTES];
        owned[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            bytes: owned,
            len: bytes.len() as u16,
        })
    }

    /// Borrow the complete page bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// Borrow the complete page as valid UTF-8.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).expect("NomadPage validates UTF-8 at construction")
    }

    /// Complete page length in bytes.
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the page is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(feature = "nomad")]
impl core::fmt::Debug for NomadPage {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("NomadPage")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

/// A candidate NomadNet page violated its fixed logical boundary.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidNomadPage {
    /// The page exceeded fixed response storage.
    TooLarge {
        /// Rejected byte count.
        actual: usize,
    },
    /// The page was not valid UTF-8.
    InvalidUtf8,
}

/// Result returned by polling one principal-owned NomadNet fetch.
#[cfg(feature = "nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// The ready page remains inline so this no-alloc boundary owns one complete
// response without indirection or a product-lifetime borrow.
#[allow(clippy::large_enum_variant)]
pub enum NomadFetchPollResponse {
    /// The fetch remains in progress.
    Pending(NomadFetchPhase),
    /// One complete bounded Micron page is ready.
    Ready(NomadPage),
    /// The fetch ended with a stable terminal failure.
    Failed(NomadFetchFailure),
}

#[cfg(feature = "nomad")]
impl NomadFetchPollResponse {
    /// Frozen state discriminator encoded at response body key zero.
    pub const fn wire_code(&self) -> u8 {
        match self {
            Self::Pending(_) => 0,
            Self::Ready(_) => 1,
            Self::Failed(_) => 2,
        }
    }
}
