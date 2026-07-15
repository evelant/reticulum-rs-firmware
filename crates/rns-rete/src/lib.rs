//! Evaluation-only integration of the pinned Rete RNS candidate.
//!
//! This crate proves the candidate graph on host and bare-metal targets while
//! leaving Rete's native events, errors and allocation behavior visible. It is
//! not yet the production-bounded adapter.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use rete_core::Identity;
use rete_stack::NodeCore;
use rete_transport::HeaplessStorage;
use reticulum_rns_conformance::{CandidateMetadata, CandidateRole};

/// Reviewed Rete source revision.
pub const SOURCE_REVISION: &str = "9bcb7d3e482b7df100622f2a0d9e53ba3bb7a743";

/// Initial table capacities used only to obtain comparable Phase-0 numbers.
pub mod probe_capacity {
    pub const PATHS: usize = 64;
    pub const ANNOUNCES: usize = 16;
    pub const DEDUPLICATION_ENTRIES: usize = 128;
    pub const LINKS: usize = 4;
}

/// Explicitly sized Rete node used by the first conformance probes.
pub type ProbeNode = NodeCore<
    HeaplessStorage<
        { probe_capacity::PATHS },
        { probe_capacity::ANNOUNCES },
        { probe_capacity::DEDUPLICATION_ENTRIES },
        { probe_capacity::LINKS },
    >,
>;

/// Metadata emitted with every Rete conformance result.
pub const fn metadata() -> CandidateMetadata {
    CandidateMetadata {
        id: "rete",
        source: "https://github.com/s-retlaw/rete",
        revision: SOURCE_REVISION,
        license: "Apache-2.0",
        role: CandidateRole::Lead,
        accepted: false,
    }
}

/// Construct a deterministic node for host/vector probes only.
///
/// Production firmware must create or load an identity from qualified entropy;
/// it must never call this helper or ship a fixture seed.
pub fn new_conformance_node(seed: &[u8; 32]) -> Result<ProbeNode, rete_core::Error> {
    let identity = Identity::from_seed(seed)?;
    ProbeNode::new(identity, "reticulum", &["phase0"])
}

/// Known allocation findings that must be closed before acceptance.
pub const KNOWN_ALLOCATION_GAPS: &[&str] = &[
    "NodeCore and NodeEvent contain network-sized Vec allocations",
    "Resource receive and completion retain whole payload buffers",
    "some full-table insertion failures are not surfaced",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_core_agrees_with_project_mtu() {
        assert_eq!(rete_core::MTU, reticulum_rns_conformance::RNS_MTU);
    }

    #[test]
    fn deterministic_probe_node_constructs() {
        let node = new_conformance_node(&[0x52; 32]).unwrap();
        assert_ne!(node.dest_hash().as_ref(), &[0u8; 16]);
    }

    #[test]
    fn candidate_is_not_prematurely_marked_accepted() {
        assert!(!metadata().accepted);
        assert!(!KNOWN_ALLOCATION_GAPS.is_empty());
    }
}
