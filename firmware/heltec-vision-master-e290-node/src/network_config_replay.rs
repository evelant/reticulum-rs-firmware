//! Exact, secret-safe same-boot correlation for network mutations.
//!
//! A mutation receipt retains only this domain-separated digest, never a
//! passphrase or a `Debug`-visible request. The digest is correlation evidence,
//! not a password verifier or durable replay authority.

use reticulum_device_api::{
    GatewayPolicy, IdempotencyKey, NetworkConfigMutation, NetworkConfigMutationRequest,
    PrincipalId, ReticulumTcpPeerHostUpdate, ReticulumTcpPeerUpdate, RmapConfig,
    WifiCredentialUpdate, WifiNetworkUpdate,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/e290/network-config-mutation/v1";
const DIGEST_FLUSH_TRAILER: [u8; 64] = [0xa6; 64];

/// Domain-separated fingerprint of one complete semantic network mutation.
///
/// The type intentionally implements neither `Debug` nor byte access. It is
/// used only to reject reuse of an idempotency key for different content.
#[derive(Clone, Copy)]
pub struct NetworkConfigMutationFingerprint([u8; 32]);

impl NetworkConfigMutationFingerprint {
    /// Fingerprint every semantic field, including replacement credential
    /// bytes, without retaining those bytes.
    pub fn new(mutation: NetworkConfigMutation<'_>) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(DIGEST_DOMAIN);
        match mutation {
            NetworkConfigMutation::UpsertWifi {
                profile_id,
                network,
            } => {
                hasher.update([0]);
                hasher.update(profile_id.as_bytes());
                update_wifi(&mut hasher, network);
            }
            NetworkConfigMutation::RemoveWifi { profile_id } => {
                hasher.update([1]);
                hasher.update(profile_id.as_bytes());
            }
            NetworkConfigMutation::ReplaceTcpPeer(peer) => {
                hasher.update([2]);
                match peer {
                    Some(peer) => {
                        hasher.update([1]);
                        update_tcp_peer(&mut hasher, peer);
                    }
                    None => hasher.update([0]),
                }
            }
            NetworkConfigMutation::ReplaceTcpHostPeer(peer) => {
                hasher.update([3]);
                match peer {
                    Some(peer) => {
                        hasher.update([1]);
                        update_tcp_host_peer(&mut hasher, peer);
                    }
                    None => hasher.update([0]),
                }
            }
            NetworkConfigMutation::SetGatewayPolicy(policy) => {
                hasher.update([4]);
                update_gateway_policy(&mut hasher, policy);
            }
            NetworkConfigMutation::SetRmapConfig(config) => {
                hasher.update([5]);
                update_rmap_config(&mut hasher, config);
            }
            NetworkConfigMutation::SetLoraTxPower(power) => {
                hasher.update([6, power.get()]);
            }
            NetworkConfigMutation::SetLoraProfile(profile) => {
                hasher.update([7]);
                hasher.update(profile.frequency_hz().to_be_bytes());
                hasher.update(profile.bandwidth_hz().to_be_bytes());
                hasher.update([
                    profile.spreading_factor(),
                    profile.coding_rate_denominator(),
                    profile.tx_power_dbm().get(),
                ]);
            }
        }
        // A complete public block displaces any password tail from sha2's
        // internal input buffer before finalization.
        hasher.update(DIGEST_FLUSH_TRAILER);
        Self(hasher.finalize().into())
    }

    /// Compare fingerprints without a data-dependent early exit.
    pub fn matches(self, other: Self) -> bool {
        bool::from(self.0.ct_eq(&other.0))
    }
}

/// Same-boot evidence for resolving one ambiguous network mutation response.
///
/// The receipt intentionally implements neither `Debug` nor persistence. It
/// binds the authenticated principal, correlation key, revisions, and exact
/// semantic mutation fingerprint.
#[derive(Clone, Copy)]
pub struct NetworkConfigMutationReceipt {
    principal: PrincipalId,
    idempotency_key: IdempotencyKey,
    expected_revision: u64,
    applied_revision: u64,
    mutation: NetworkConfigMutationFingerprint,
}

impl NetworkConfigMutationReceipt {
    /// Record one mutation only after its intended successor is authoritative.
    pub fn new(
        principal: PrincipalId,
        request: NetworkConfigMutationRequest<'_>,
        applied_revision: u64,
    ) -> Self {
        Self {
            principal,
            idempotency_key: request.idempotency_key(),
            expected_revision: request.expected_revision(),
            applied_revision,
            mutation: NetworkConfigMutationFingerprint::new(request.mutation()),
        }
    }

    /// Recognize only the exact same principal, correlation values, and
    /// semantic mutation at the still-current applied revision.
    pub fn matches(
        self,
        principal: PrincipalId,
        request: NetworkConfigMutationRequest<'_>,
        current_revision: u64,
    ) -> bool {
        self.principal == principal
            && self.idempotency_key == request.idempotency_key()
            && self.expected_revision == request.expected_revision()
            && self.applied_revision == current_revision
            && self
                .mutation
                .matches(NetworkConfigMutationFingerprint::new(request.mutation()))
    }
}

fn update_wifi(hasher: &mut Sha256, network: WifiNetworkUpdate<'_>) {
    hasher.update([u8::from(network.enabled()), network.priority()]);
    let ssid = network.ssid();
    hasher.update([ssid.as_bytes().len() as u8]);
    hasher.update(ssid.as_bytes());
    match network.credential() {
        WifiCredentialUpdate::Keep => hasher.update([0]),
        WifiCredentialUpdate::Replace(passphrase) => {
            hasher.update([1, passphrase.len() as u8]);
            hasher.update(passphrase);
        }
    }
}

fn update_tcp_peer(hasher: &mut Sha256, peer: ReticulumTcpPeerUpdate) {
    hasher.update([u8::from(peer.enabled())]);
    hasher.update(peer.ipv4_address().octets());
    hasher.update(peer.port().to_be_bytes());
}

fn update_tcp_host_peer(hasher: &mut Sha256, peer: ReticulumTcpPeerHostUpdate<'_>) {
    hasher.update([u8::from(peer.enabled())]);
    let hostname = peer.hostname().as_str().as_bytes();
    hasher.update([hostname.len() as u8]);
    hasher.update(hostname);
    hasher.update(peer.port().to_be_bytes());
}

fn update_gateway_policy(hasher: &mut Sha256, policy: GatewayPolicy) {
    hasher.update([
        u8::from(policy.wifi_transport_enabled()),
        u8::from(policy.automatic_announces_enabled()),
    ]);
}

fn update_rmap_config(hasher: &mut Sha256, config: RmapConfig) {
    hasher.update([
        u8::from(config.discovery_enabled()),
        u8::from(config.share_location()),
    ]);
    match config.phone_location() {
        Some(location) => {
            hasher.update([1]);
            hasher.update(location.latitude_e6().to_be_bytes());
            hasher.update(location.longitude_e6().to_be_bytes());
        }
        None => hasher.update([0]),
    }
}

#[cfg(test)]
mod tests {
    use reticulum_device_api::{
        IdempotencyKey, LoraRadioProfile, LoraTransmitPowerDbm, NetworkConfigMutation,
        NetworkConfigMutationRequest, PrincipalId, ReticulumTcpPeerIpv4Address,
        ReticulumTcpPeerUpdate, WifiCredentialUpdate, WifiNetworkProfileId, WifiNetworkUpdate,
        WifiSsid,
    };

    use super::{NetworkConfigMutationFingerprint, NetworkConfigMutationReceipt};

    fn profile_id(byte: u8) -> WifiNetworkProfileId {
        WifiNetworkProfileId::new([byte; 16]).unwrap()
    }

    fn wifi<'a>(ssid: &'a [u8], credential: WifiCredentialUpdate<'a>) -> WifiNetworkUpdate<'a> {
        WifiNetworkUpdate::new(true, 200, WifiSsid::new(ssid).unwrap(), credential)
    }

    #[test]
    fn exact_wifi_retry_matches_without_retaining_a_secret_owner() {
        let left = NetworkConfigMutationFingerprint::new(NetworkConfigMutation::UpsertWifi {
            profile_id: profile_id(1),
            network: wifi(
                b"mesh",
                WifiCredentialUpdate::replace(b"password-one").unwrap(),
            ),
        });
        let right = NetworkConfigMutationFingerprint::new(NetworkConfigMutation::UpsertWifi {
            profile_id: profile_id(1),
            network: wifi(
                b"mesh",
                WifiCredentialUpdate::replace(b"password-one").unwrap(),
            ),
        });
        assert!(left.matches(right));
    }

    #[test]
    fn every_semantic_difference_changes_replay_correlation() {
        let baseline = NetworkConfigMutationFingerprint::new(NetworkConfigMutation::UpsertWifi {
            profile_id: profile_id(1),
            network: wifi(
                b"mesh",
                WifiCredentialUpdate::replace(b"password-one").unwrap(),
            ),
        });
        let different_secret =
            NetworkConfigMutationFingerprint::new(NetworkConfigMutation::UpsertWifi {
                profile_id: profile_id(1),
                network: wifi(
                    b"mesh",
                    WifiCredentialUpdate::replace(b"password-two").unwrap(),
                ),
            });
        let keep = NetworkConfigMutationFingerprint::new(NetworkConfigMutation::UpsertWifi {
            profile_id: profile_id(1),
            network: wifi(b"mesh", WifiCredentialUpdate::Keep),
        });
        let different_profile =
            NetworkConfigMutationFingerprint::new(NetworkConfigMutation::RemoveWifi {
                profile_id: profile_id(2),
            });
        let peer =
            NetworkConfigMutationFingerprint::new(NetworkConfigMutation::ReplaceTcpPeer(Some(
                ReticulumTcpPeerUpdate::new(
                    true,
                    ReticulumTcpPeerIpv4Address::new([192, 0, 2, 4]).unwrap(),
                    4242,
                )
                .unwrap(),
            )));
        let power = NetworkConfigMutationFingerprint::new(NetworkConfigMutation::SetLoraTxPower(
            LoraTransmitPowerDbm::DBM_22,
        ));

        assert!(!baseline.matches(different_secret));
        assert!(!baseline.matches(keep));
        assert!(!baseline.matches(different_profile));
        assert!(!baseline.matches(peer));
        assert!(!baseline.matches(power));
    }

    #[test]
    fn every_lora_power_setpoint_has_distinct_replay_correlation() {
        let fingerprints = [
            NetworkConfigMutationFingerprint::new(NetworkConfigMutation::SetLoraTxPower(
                LoraTransmitPowerDbm::DBM_14,
            )),
            NetworkConfigMutationFingerprint::new(NetworkConfigMutation::SetLoraTxPower(
                LoraTransmitPowerDbm::DBM_17,
            )),
            NetworkConfigMutationFingerprint::new(NetworkConfigMutation::SetLoraTxPower(
                LoraTransmitPowerDbm::DBM_20,
            )),
            NetworkConfigMutationFingerprint::new(NetworkConfigMutation::SetLoraTxPower(
                LoraTransmitPowerDbm::DBM_22,
            )),
        ];

        for (index, fingerprint) in fingerprints.iter().enumerate() {
            assert!(
                fingerprints
                    .iter()
                    .skip(index + 1)
                    .all(|other| !fingerprint.matches(*other))
            );
        }
    }

    #[test]
    fn every_lora_profile_field_changes_replay_correlation() {
        fn profile(frequency: u32, bandwidth: u32, sf: u8, cr: u8, power: u8) -> LoraRadioProfile {
            LoraRadioProfile::new(
                frequency,
                bandwidth,
                sf,
                cr,
                LoraTransmitPowerDbm::new(power).unwrap(),
            )
            .unwrap()
        }
        let profiles = [
            profile(915_000_000, 125_000, 7, 5, 14),
            profile(914_875_000, 125_000, 7, 5, 14),
            profile(915_000_000, 250_000, 7, 5, 14),
            profile(915_000_000, 125_000, 8, 5, 14),
            profile(915_000_000, 125_000, 7, 6, 14),
            profile(915_000_000, 125_000, 7, 5, 17),
        ];
        let fingerprints = profiles.map(|profile| {
            NetworkConfigMutationFingerprint::new(NetworkConfigMutation::SetLoraProfile(profile))
        });
        for (index, fingerprint) in fingerprints.iter().enumerate() {
            assert!(
                fingerprints
                    .iter()
                    .skip(index + 1)
                    .all(|other| !fingerprint.matches(*other))
            );
        }
    }

    #[test]
    fn reused_key_with_a_different_current_noop_is_not_an_exact_retry() {
        let principal = PrincipalId([3; 16]);
        let key = IdempotencyKey([4; 16]);
        let original = NetworkConfigMutationRequest::new(
            NetworkConfigMutation::RemoveWifi {
                profile_id: profile_id(1),
            },
            8,
            key,
        );
        let receipt = NetworkConfigMutationReceipt::new(principal, original, 9);
        let different_absent_profile = NetworkConfigMutationRequest::new(
            NetworkConfigMutation::RemoveWifi {
                profile_id: profile_id(2),
            },
            8,
            key,
        );

        assert!(receipt.matches(principal, original, 9));
        assert!(!receipt.matches(principal, different_absent_profile, 9));
    }
}
