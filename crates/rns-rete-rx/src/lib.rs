//! Product-only RNode receive adapter and opaque Rete façade.
//!
//! Firmware targets depend on this crate instead of `reticulum-rns-rete`.
//! This crate alone composes physical RNode reassembly with the interface-neutral
//! Rete owner. The owning newtype exposes only the scalar receive/maintenance
//! operations needed by the Phase-1 target. It has no send, announce, Link,
//! channel, transport-mode, inner-owner or conversion surface.

#![no_std]
#![forbid(unsafe_code)]

use core::num::NonZeroU64;

use rand_core::{CryptoRng, RngCore};
use receive_only::{ReceiveOnlyIngress, ReceiveOnlyIngressError};

mod receive_only;

pub use receive_only::{
    RETE_MAINTENANCE_INTERVAL_SECONDS, RawFrameDropReason, ReceiveOnlyClockSample,
    ReceiveOnlyIngressMetrics, ReceiveOnlyIngressOutcome, ReceiveOnlyStep, ReceiveOnlyWake,
    SuppressedActions,
};
pub use reticulum_rns_rete::{IngressDisposition, metadata};

/// Opaque identity accepted by the receive-only Rete owner.
pub struct ReceiveOnlyIdentity(reticulum_rns_rete::Identity);

impl ReceiveOnlyIdentity {
    /// Public X25519-plus-Ed25519 key bytes used for HIL peer discovery.
    pub fn public_key(&self) -> [u8; 64] {
        self.0.public_key()
    }
}

/// Failure to import Reticulum's combined 64-byte private-key representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveOnlyIdentityError {
    /// Rete rejected the supplied key length or key material.
    InvalidPrivateKey,
}

/// Import an identity without exposing Rete's general-purpose identity owner.
pub fn receive_only_identity_from_private_key(
    private_key: &[u8],
) -> Result<ReceiveOnlyIdentity, ReceiveOnlyIdentityError> {
    reticulum_rns_rete::identity_from_private_key(private_key)
        .map(ReceiveOnlyIdentity)
        .map_err(|_| ReceiveOnlyIdentityError::InvalidPrivateKey)
}

/// Stable, façade-owned destination hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiveOnlyDestinationHash([u8; 16]);

impl ReceiveOnlyDestinationHash {
    /// Borrow the complete 16-byte truncated hash.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl AsRef<[u8]> for ReceiveOnlyDestinationHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Stable, façade-owned identity hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiveOnlyIdentityHash([u8; 16]);

impl ReceiveOnlyIdentityHash {
    /// Borrow the complete 16-byte truncated hash.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl AsRef<[u8]> for ReceiveOnlyIdentityHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Product-local identifier for one receive interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiveOnlyInterfaceId(pub u8);

/// Flattened construction failure that does not expose the full Rete error API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveOnlyReteError {
    /// Rete rejected node or destination construction.
    Construction,
    /// The primary destination was unexpectedly unavailable for policy setup.
    PrimaryDestinationUnavailable,
}

/// Opaque receive-only Rete owner used by product firmware.
pub struct ReceiveOnlyRete<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
> {
    inner: ReceiveOnlyIngress<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>,
}

impl<const PATHS: usize, const ANNOUNCES: usize, const DEDUPLICATION: usize, const LINKS: usize>
    ReceiveOnlyRete<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>
{
    /// Construct a fixed-role endpoint with Link admission disabled.
    #[allow(
        clippy::too_many_arguments,
        reason = "clock domains, capacities and interface identity remain explicit"
    )]
    pub fn new(
        identity: ReceiveOnlyIdentity,
        app_name: &str,
        aspects: &[&str],
        fragment_timeout_ticks: NonZeroU64,
        initial_now_ticks: u64,
        maintenance_interval_ticks: NonZeroU64,
        interface: ReceiveOnlyInterfaceId,
    ) -> Result<Self, ReceiveOnlyReteError> {
        ReceiveOnlyIngress::new(
            identity.0,
            app_name,
            aspects,
            fragment_timeout_ticks,
            initial_now_ticks,
            maintenance_interval_ticks,
            reticulum_rns_rete::InterfaceId(interface.0),
        )
        .map(|inner| Self { inner })
        .map_err(|error| match error {
            ReceiveOnlyIngressError::Rete => ReceiveOnlyReteError::Construction,
            ReceiveOnlyIngressError::PrimaryDestinationUnavailable => {
                ReceiveOnlyReteError::PrimaryDestinationUnavailable
            }
        })
    }

    /// Primary destination hash for diagnostics and controlled HIL setup.
    pub fn destination_hash(&self) -> ReceiveOnlyDestinationHash {
        ReceiveOnlyDestinationHash(*self.inner.destination_hash().as_bytes())
    }

    /// Local identity hash without exposing private key ownership.
    pub fn identity_hash(&self) -> ReceiveOnlyIdentityHash {
        ReceiveOnlyIdentityHash(*self.inner.identity_hash().as_bytes())
    }

    /// Absolute deadline of the retained first split frame, if any.
    pub const fn fragment_deadline_ticks(&self) -> Option<u64> {
        self.inner.fragment_deadline_ticks()
    }

    /// Earliest absolute timer wake required by fragment or Rete maintenance.
    pub const fn next_wake_ticks(&self) -> u64 {
        self.inner.next_wake_ticks()
    }

    /// Service one frame-or-timer wake while suppressing every outbound action.
    pub fn on_wake<R: RngCore + CryptoRng>(
        &mut self,
        wake: ReceiveOnlyWake<'_>,
        now: ReceiveOnlyClockSample,
        rng: &mut R,
    ) -> ReceiveOnlyStep {
        self.inner.on_wake(wake, now, rng)
    }

    /// Return a fixed-size, scalar-only diagnostic snapshot.
    pub fn metrics(&self) -> ReceiveOnlyIngressMetrics {
        self.inner.metrics()
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use rand_core::{CryptoRng, RngCore};
    use reticulum_radio_interface::{FrameSignal, RawReceivedFrame, SX1262_FRAME_MTU};

    use super::*;

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

        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for byte in dest {
                *byte = self.0;
                self.0 = self.0.wrapping_add(1);
            }
        }

        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl CryptoRng for CounterRng {}

    #[test]
    fn facade_ingests_without_exposing_an_inner_owner() {
        let identity = receive_only_identity_from_private_key(&[0x42; 64]).unwrap();
        let public_key = identity.public_key();
        assert_ne!(public_key, [0; 64]);
        let mut owner = ReceiveOnlyRete::<16, 4, 32, 2>::new(
            identity,
            "rx-facade-test",
            &["unit"],
            NonZeroU64::new(100).unwrap(),
            0,
            NonZeroU64::new(5).unwrap(),
            ReceiveOnlyInterfaceId(1),
        )
        .unwrap();
        assert_ne!(owner.destination_hash().as_bytes(), &[0; 16]);
        assert_ne!(owner.identity_hash().as_bytes(), &[0; 16]);

        let mut bytes = [0; SX1262_FRAME_MTU];
        bytes[0] = 0;
        let frame = RawReceivedFrame::new(bytes, 1, FrameSignal::new(-90, 4), 1);
        let step = owner.on_wake(
            ReceiveOnlyWake::Frame(&frame),
            ReceiveOnlyClockSample {
                ticks: 1,
                transport_seconds: 0,
            },
            &mut CounterRng::default(),
        );
        assert!(step.frame.is_some());
        assert_eq!(owner.metrics().frames_handed_off, 1);
    }

    #[test]
    fn invalid_identity_error_is_flattened() {
        assert!(matches!(
            receive_only_identity_from_private_key(&[0; 63]),
            Err(ReceiveOnlyIdentityError::InvalidPrivateKey)
        ));
    }
}
