use std::{fmt, io::Read};

use reticulum_device_api_session::{
    BearerBinding, ClientCredential, ClientParameters, CredentialGeneration, CredentialId, DeviceId,
};
use zeroize::Zeroizing;

/// Exact byte length of the existing host activated-credential state image.
pub const ACTIVATED_CREDENTIAL_STATE_BYTES: usize = 96;

const STATE_MAGIC: [u8; 8] = *b"RDPKEY1\0";
const STATE_FORMAT_VERSION: u16 = 1;
const STATE_ACTIVE: u8 = 2;

/// Canonically decoded active credential and expected device identity.
///
/// This owner deliberately implements neither `Clone` nor `Debug`; consuming
/// it into [`DeviceClient`](crate::DeviceClient) moves its zeroizing PSK into
/// the session handshake.
pub struct ActivatedCredential {
    device_id: DeviceId,
    credential_id: CredentialId,
    generation: CredentialGeneration,
    psk: Zeroizing<[u8; 32]>,
}

impl ActivatedCredential {
    /// Decode exactly one canonical activated-state image.
    pub fn decode(bytes: &[u8]) -> Result<Self, CredentialStateError> {
        if bytes.len() != ACTIVATED_CREDENTIAL_STATE_BYTES {
            return Err(CredentialStateError::Length {
                actual: bytes.len(),
            });
        }
        if bytes[..8] != STATE_MAGIC || bytes[8..10] != STATE_FORMAT_VERSION.to_le_bytes() {
            return Err(CredentialStateError::UnsupportedFormat);
        }
        if bytes[10] != STATE_ACTIVE {
            return Err(CredentialStateError::NotActive { state: bytes[10] });
        }
        if bytes[11..16].iter().any(|byte| *byte != 0) || bytes[88..].iter().any(|byte| *byte != 0)
        {
            return Err(CredentialStateError::NonCanonicalReservedBytes);
        }

        let device_bytes: [u8; 16] = bytes[16..32]
            .try_into()
            .expect("fixed credential state field has exact length");
        if device_bytes.iter().all(|byte| *byte == 0) {
            return Err(CredentialStateError::ZeroDeviceId);
        }
        let credential_bytes: [u8; 16] = bytes[32..48]
            .try_into()
            .expect("fixed credential state field has exact length");
        if credential_bytes.iter().all(|byte| *byte == 0) {
            return Err(CredentialStateError::ZeroCredentialId);
        }
        let generation = u64::from_le_bytes(
            bytes[48..56]
                .try_into()
                .expect("fixed credential state field has exact length"),
        );
        if generation == 0 {
            return Err(CredentialStateError::ZeroGeneration);
        }
        let mut psk = Zeroizing::new([0_u8; 32]);
        psk.copy_from_slice(&bytes[56..88]);
        if psk.iter().all(|byte| *byte == 0) {
            return Err(CredentialStateError::ZeroPsk);
        }

        Ok(Self {
            device_id: DeviceId::new(device_bytes),
            credential_id: CredentialId::new(credential_bytes),
            generation: CredentialGeneration::new(generation),
            psk,
        })
    }

    /// Read and decode exactly one activated state image, rejecting trailing bytes.
    ///
    /// This method validates contents only. Applications loading secrets from a
    /// filesystem must separately enforce owner-only access and safe path/file
    /// identity before passing the reader here.
    pub fn read_from(reader: &mut impl Read) -> Result<Self, CredentialStateError> {
        let mut bytes = Zeroizing::new([0_u8; ACTIVATED_CREDENTIAL_STATE_BYTES]);
        reader
            .read_exact(&mut bytes[..])
            .map_err(CredentialStateError::Io)?;
        let mut trailing = [0_u8; 1];
        match reader
            .read(&mut trailing)
            .map_err(CredentialStateError::Io)?
        {
            0 => Self::decode(&bytes[..]),
            _ => Err(CredentialStateError::TrailingBytes),
        }
    }

    /// Expected stable device identifier authenticated by the handshake.
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Opaque credential identifier selected for authentication.
    pub const fn credential_id(&self) -> CredentialId {
        self.credential_id
    }

    /// Active device-owned credential generation.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }

    pub(crate) fn into_handshake(self) -> (ClientParameters, ClientCredential) {
        (
            ClientParameters::new(self.device_id, BearerBinding::UsbSerialJtag),
            ClientCredential::from_zeroizing(self.credential_id, self.generation, self.psk),
        )
    }
}

/// Canonical activated-credential decoding failure.
#[derive(Debug)]
pub enum CredentialStateError {
    /// Input did not have the exact format length.
    Length {
        /// Observed input bytes.
        actual: usize,
    },
    /// Magic or format version was not the supported version.
    UnsupportedFormat,
    /// State does not represent an activated credential.
    NotActive {
        /// Observed state byte.
        state: u8,
    },
    /// A reserved byte was nonzero.
    NonCanonicalReservedBytes,
    /// Device ID was all zeroes.
    ZeroDeviceId,
    /// Credential ID was all zeroes.
    ZeroCredentialId,
    /// Credential generation was zero.
    ZeroGeneration,
    /// PSK was all zeroes.
    ZeroPsk,
    /// A reader contained bytes after the exact image.
    TrailingBytes,
    /// Reading the image failed.
    Io(std::io::Error),
}

impl fmt::Display for CredentialStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length { actual } => write!(
                formatter,
                "activated credential is {actual} bytes; exactly {ACTIVATED_CREDENTIAL_STATE_BYTES} are required"
            ),
            Self::UnsupportedFormat => {
                formatter.write_str("unsupported activated credential format")
            }
            Self::NotActive { state } => {
                write!(formatter, "credential state {state} is not active")
            }
            Self::NonCanonicalReservedBytes => {
                formatter.write_str("activated credential has nonzero reserved bytes")
            }
            Self::ZeroDeviceId => formatter.write_str("activated credential has a zero device ID"),
            Self::ZeroCredentialId => {
                formatter.write_str("activated credential has a zero credential ID")
            }
            Self::ZeroGeneration => {
                formatter.write_str("activated credential has a zero generation")
            }
            Self::ZeroPsk => formatter.write_str("activated credential has a zero PSK"),
            Self::TrailingBytes => {
                formatter.write_str("bytes follow the activated credential image")
            }
            Self::Io(error) => write!(formatter, "could not read activated credential: {error}"),
        }
    }
}

impl std::error::Error for CredentialStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
