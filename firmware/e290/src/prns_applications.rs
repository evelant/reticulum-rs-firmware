//! Product application registrations expressed through PRNS's public recipe.
//!
//! These rows are protocol destinations, not flash partitions or firmware
//! variants. Management and OTA deliberately share one destination because
//! they have the same identified-Link authorization and Resource policy.
//! LXMF, Nomad, and optional RMAP retain separate expanded names and policies.

use heapless::Vec;
use personal_rns::engine::{MAX_SEND_REQUEST_DATA_LEN, RatchetPolicy};
use personal_rns::identity::{IDENTITY_SECRET_KEY_LEN, Zeroizing};
use personal_rns::routing::links::resources::ResourceStrategy;
use personal_rns::routing::{LinkRequestPolicy, ProofStrategy};
use personal_rns::runtime::{PreConfiguredDestination, ServeMyRequestEndpoints};
use personal_rns::units::ByteLimit;
use personal_rns::wire::DestinationHash;
pub use reticulum_device_api::{MANAGEMENT_APPLICATION_NAME, MANAGEMENT_ASPECTS};

use crate::prns_storage::APPLICATION_DESTINATION_CAPACITY;

/// Application name defined by the LXMF delivery protocol.
pub const LXMF_APPLICATION_NAME: &str = "lxmf";
/// Aspects defined by the LXMF delivery protocol.
pub const LXMF_ASPECTS: [&str; 1] = ["delivery"];
/// Application name defined by Nomad Network nodes.
pub const NOMAD_APPLICATION_NAME: &str = "nomadnetwork";
/// Aspects defined by Nomad Network nodes.
pub const NOMAD_ASPECTS: [&str; 1] = ["node"];
/// Application name defined by the RMAP discovery protocol.
pub const RMAP_APPLICATION_NAME: &str = "rnstransport";
/// Aspects defined by the RMAP interface discovery protocol.
pub const RMAP_ASPECTS: [&str; 2] = ["discovery", "interface"];

/// Product services that change the destination catalog at boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationProfile {
    /// Whether this image runs the LXMF application.
    pub lxmf: bool,
    /// Whether this image announces the optional RMAP application.
    pub rmap: bool,
}

impl ApplicationProfile {
    /// Construct a boot-time application profile.
    pub const fn new(lxmf: bool, rmap: bool) -> Self {
        Self { lxmf, rmap }
    }

    /// Number of protocol destinations registered by this profile.
    pub const fn destination_count(self) -> usize {
        2 + self.lxmf as usize + self.rmap as usize
    }
}

/// Stable hashes and the exact PRNS recipe rows that produce them.
pub struct ApplicationCatalog<'a> {
    /// Exact rows passed to PRNS during node assembly.
    pub destinations: Vec<PreConfiguredDestination<'a>, APPLICATION_DESTINATION_CAPACITY>,
    /// Shared management and OTA destination hash.
    pub management: DestinationHash,
    /// Nomad Network node destination hash.
    pub nomad: DestinationHash,
    /// LXMF delivery destination hash when LXMF is enabled.
    pub lxmf: Option<DestinationHash>,
    /// RMAP discovery destination hash when RMAP is enabled.
    pub rmap: Option<DestinationHash>,
}

/// Failure to express the product application catalog through PRNS's recipe.
#[derive(Debug, PartialEq, Eq)]
pub enum ApplicationCatalogError {
    /// PRNS rejected an application name or aspect combination.
    InvalidExpandedName,
    /// The product's generic destination registry is too small.
    Capacity,
}

/// Build the application-independent destination catalog for this boot.
///
/// All Single destinations derive from the same durable Reticulum identity,
/// just as Python applications normally create several named destinations
/// from one identity. No destination is allocated to a named flash region.
pub fn application_catalog<'a>(
    identity: &[u8; IDENTITY_SECRET_KEY_LEN],
    lxmf_announce_app_data: &'a [u8],
    profile: ApplicationProfile,
) -> Result<ApplicationCatalog<'a>, ApplicationCatalogError> {
    let management = single_destination(
        identity,
        MANAGEMENT_APPLICATION_NAME,
        MANAGEMENT_ASPECTS,
        b"",
        ProofStrategy::ProveAll,
        LinkRequestPolicy::AcceptAll,
        RatchetPolicy::Ratcheted,
        // Every Link starts closed to Resources. The product OTA coordinator
        // uses PRNS's public per-Link strategy command only after an enrolled
        // management requester opens and arms an exact update chunk.
        ResourceStrategy::AcceptNone,
        ByteLimit::Maximum(MAX_SEND_REQUEST_DATA_LEN as u64),
        ServeMyRequestEndpoints::Yes,
    );
    let management_hash = destination_hash(&management)?;

    let nomad = single_destination(
        identity,
        NOMAD_APPLICATION_NAME,
        &NOMAD_ASPECTS,
        b"Metalbeard",
        ProofStrategy::ProveNone,
        LinkRequestPolicy::AcceptAll,
        RatchetPolicy::NoRatchets,
        ResourceStrategy::AcceptNone,
        ByteLimit::Maximum(1),
        ServeMyRequestEndpoints::Yes,
    );
    let nomad_hash = destination_hash(&nomad)?;

    let mut destinations = Vec::new();
    destinations
        .push(management)
        .map_err(|_| ApplicationCatalogError::Capacity)?;
    destinations
        .push(nomad)
        .map_err(|_| ApplicationCatalogError::Capacity)?;

    let lxmf = if profile.lxmf {
        let destination = single_destination(
            identity,
            LXMF_APPLICATION_NAME,
            &LXMF_ASPECTS,
            lxmf_announce_app_data,
            ProofStrategy::ProveAll,
            LinkRequestPolicy::AcceptAll,
            RatchetPolicy::Ratcheted,
            ResourceStrategy::AcceptNone,
            ByteLimit::Maximum(0),
            ServeMyRequestEndpoints::No,
        );
        let hash = destination_hash(&destination)?;
        destinations
            .push(destination)
            .map_err(|_| ApplicationCatalogError::Capacity)?;
        Some(hash)
    } else {
        None
    };

    let rmap = if profile.rmap {
        let destination = single_destination(
            identity,
            RMAP_APPLICATION_NAME,
            &RMAP_ASPECTS,
            b"",
            ProofStrategy::ProveNone,
            LinkRequestPolicy::AcceptNone,
            RatchetPolicy::NoRatchets,
            ResourceStrategy::AcceptNone,
            ByteLimit::Maximum(0),
            ServeMyRequestEndpoints::No,
        );
        let hash = destination_hash(&destination)?;
        destinations
            .push(destination)
            .map_err(|_| ApplicationCatalogError::Capacity)?;
        Some(hash)
    } else {
        None
    };

    debug_assert_eq!(destinations.len(), profile.destination_count());
    Ok(ApplicationCatalog {
        destinations,
        management: management_hash,
        nomad: nomad_hash,
        lxmf,
        rmap,
    })
}

#[allow(clippy::too_many_arguments)]
fn single_destination<'a>(
    identity: &[u8; IDENTITY_SECRET_KEY_LEN],
    app_name: &'a str,
    aspects: &'a [&'a str],
    announce_app_data: &'a [u8],
    proof: ProofStrategy,
    link_requests: LinkRequestPolicy,
    ratchet: RatchetPolicy,
    resource_strategy: ResourceStrategy,
    maximum_request_bytes: ByteLimit,
    request_endpoints: ServeMyRequestEndpoints,
) -> PreConfiguredDestination<'a> {
    PreConfiguredDestination::Single {
        app_name,
        aspects,
        identity: Zeroizing::new(*identity),
        announce_app_data,
        proof,
        link_requests,
        ratchet,
        resource_strategy,
        maximum_request_bytes,
        request_endpoints,
    }
}

fn destination_hash(
    destination: &PreConfiguredDestination<'_>,
) -> Result<DestinationHash, ApplicationCatalogError> {
    destination
        .destination_hash()
        .map_err(|_| ApplicationCatalogError::InvalidExpandedName)
}

const _: () = assert!(ApplicationProfile::new(true, true).destination_count() == 4);
const _: () = assert!(ApplicationProfile::new(true, false).destination_count() == 3);
const _: () = assert!(
    ApplicationProfile::new(true, true).destination_count() <= APPLICATION_DESTINATION_CAPACITY
);

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [u8; IDENTITY_SECRET_KEY_LEN] = [0x42; IDENTITY_SECRET_KEY_LEN];

    #[test]
    fn default_product_uses_three_protocol_destinations() {
        let catalog = application_catalog(
            &IDENTITY,
            b"lxmf app data",
            ApplicationProfile::new(true, false),
        )
        .unwrap();
        assert_eq!(catalog.destinations.len(), 3);
        assert!(catalog.lxmf.is_some());
        assert!(catalog.rmap.is_none());
        assert_ne!(catalog.management, catalog.nomad);
        assert_ne!(catalog.management, catalog.lxmf.unwrap());
    }

    #[test]
    fn optional_rmap_adds_a_row_without_changing_the_storage_profile() {
        let catalog =
            application_catalog(&IDENTITY, b"", ApplicationProfile::new(false, true)).unwrap();
        assert_eq!(catalog.destinations.len(), 3);
        assert!(catalog.lxmf.is_none());
        assert!(catalog.rmap.is_some());
        assert_eq!(APPLICATION_DESTINATION_CAPACITY, 16);
    }

    #[test]
    fn management_and_ota_share_one_identified_resource_destination() {
        let catalog =
            application_catalog(&IDENTITY, b"", ApplicationProfile::new(false, false)).unwrap();
        match &catalog.destinations[0] {
            PreConfiguredDestination::Single {
                app_name,
                aspects,
                proof,
                link_requests,
                ratchet,
                resource_strategy,
                maximum_request_bytes,
                request_endpoints,
                ..
            } => {
                assert_eq!(*app_name, MANAGEMENT_APPLICATION_NAME);
                assert_eq!(*aspects, MANAGEMENT_ASPECTS);
                assert_eq!(*proof, ProofStrategy::ProveAll);
                assert_eq!(*link_requests, LinkRequestPolicy::AcceptAll);
                assert_eq!(*ratchet, RatchetPolicy::Ratcheted);
                assert_eq!(*resource_strategy, ResourceStrategy::AcceptNone);
                assert_eq!(
                    *maximum_request_bytes,
                    ByteLimit::Maximum(MAX_SEND_REQUEST_DATA_LEN as u64)
                );
                assert_eq!(*request_endpoints, ServeMyRequestEndpoints::Yes);
            }
            PreConfiguredDestination::Plain { .. } => panic!("management must be Single"),
        }
    }

    #[test]
    fn lxmf_keeps_python_compatible_immediate_proofs() {
        let catalog =
            application_catalog(&IDENTITY, b"announce", ApplicationProfile::new(true, false))
                .unwrap();
        match &catalog.destinations[2] {
            PreConfiguredDestination::Single {
                proof,
                ratchet,
                resource_strategy,
                ..
            } => {
                assert_eq!(*proof, ProofStrategy::ProveAll);
                assert_eq!(*ratchet, RatchetPolicy::Ratcheted);
                assert_eq!(*resource_strategy, ResourceStrategy::AcceptNone);
            }
            PreConfiguredDestination::Plain { .. } => panic!("LXMF must be Single"),
        }
    }
}
