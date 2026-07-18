//! Host-checkable product policy for the first permanent E290 node image.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod announce_time;
pub mod config;
pub mod durability_boot;
pub mod durability_policy;
pub mod partition_contract;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod live_admission_test_support;
#[cfg(test)]
mod live_admission_tests;

use reticulum_storage_actor::{JournalBinding, StorageDeviceId};

/// Derive the coordinator's physical-flash identifier from the E290 eFuse MAC.
pub const fn storage_device_id_from_eui48(mac: [u8; 6]) -> StorageDeviceId {
    StorageDeviceId::new([
        b'e', b'2', b'9', b'0', b'-', b'f', b'l', b'a', b's', b'h', mac[0], mac[1], mac[2], mac[3],
        mac[4], mac[5],
    ])
}

/// Bind the physical journal layout to one coordinator-owned storage device.
pub const fn node_journal_binding(device: StorageDeviceId) -> JournalBinding {
    JournalBinding::new(
        device,
        partition_contract::NODE_JOURNAL_OFFSET as usize,
        partition_contract::NODE_JOURNAL_LEN as usize,
        reticulum_storage_journal::PHYSICAL_FORMAT_VERSION,
    )
}

const _: () = assert!(
    partition_contract::NODE_IDENTITY_LEN as usize
        == reticulum_device_identity_store::PARTITION_SIZE
);
const _: () = assert!(
    partition_contract::ANNOUNCE_CLOCK_LEN as usize == reticulum_announce_clock::PARTITION_SIZE
);
const _: () = assert!(
    partition_contract::NODE_JOURNAL_LEN as usize == reticulum_storage_journal::PARTITION_SIZE
);

#[cfg(test)]
mod tests {
    extern crate std;

    use crate::{config, node_journal_binding, partition_contract, storage_device_id_from_eui48};
    use reticulum_node_core::{NodeConfig, NodeCore, NodeIdentity, NodeInstanceId};

    #[test]
    fn permanent_partition_contract_preserves_exact_store_boundaries() {
        assert_eq!(partition_contract::API_CREDENTIALS_OFFSET, 0x0061_4000);
        assert_eq!(partition_contract::API_CREDENTIALS_LEN, 0x0000_2000);
        assert_eq!(partition_contract::DEVICE_CONFIG_OFFSET, 0x0061_6000);
        assert_eq!(partition_contract::DEVICE_CONFIG_LEN, 0x0001_a000);
        assert_eq!(partition_contract::NODE_JOURNAL_OFFSET, 0x0063_0000);
        assert_eq!(partition_contract::NODE_JOURNAL_LEN, 0x0010_0000);
        assert_eq!(
            partition_contract::NODE_JOURNAL_LEN as usize,
            reticulum_storage_journal::PARTITION_SIZE
        );
        assert_eq!(
            partition_contract::ANNOUNCE_CLOCK_OFFSET + partition_contract::ANNOUNCE_CLOCK_LEN,
            partition_contract::API_CREDENTIALS_OFFSET
        );
        assert_eq!(
            partition_contract::API_CREDENTIALS_OFFSET + partition_contract::API_CREDENTIALS_LEN,
            partition_contract::DEVICE_CONFIG_OFFSET
        );
        assert_eq!(
            partition_contract::DEVICE_CONFIG_OFFSET + partition_contract::DEVICE_CONFIG_LEN,
            0x0063_0000
        );
        assert_eq!(
            partition_contract::NODE_JOURNAL_OFFSET + partition_contract::NODE_JOURNAL_LEN,
            0x0073_0000
        );
        assert_eq!(
            partition_contract::API_CREDENTIALS_LABEL_BYTES,
            *b"api_credentials\0"
        );
        assert_eq!(
            partition_contract::DEVICE_CONFIG_LABEL_BYTES,
            *b"device_config\0\0\0"
        );
        assert_eq!(
            partition_contract::NODE_JOURNAL_LABEL_BYTES,
            *b"node_journal\0\0\0\0"
        );
        let device = storage_device_id_from_eui48([0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88]);
        assert_eq!(device.as_bytes(), b"e290-flash\xac\xa7\x04\xe1\x3e\x88");
        let binding = node_journal_binding(device);
        assert_eq!(binding.device(), device);
        assert_eq!(binding.absolute_offset(), 0x0063_0000);
        assert_eq!(binding.length(), 0x0010_0000);
        assert_eq!(
            binding.layout_version(),
            reticulum_storage_journal::PHYSICAL_FORMAT_VERSION
        );
    }

    #[test]
    fn primary_destination_contract_matches_released_python_vector() {
        let private_key = decode_hex::<64>(
            "408b27d3097eea5a46bf2ab6433a7234a33d5e49957b13ec7acc2ca08e1a13c7\
             5272c90c8d3385d47ede5420a7a9623aad817d9f8a70bd100a0acea7400daa59",
        );
        let identity = NodeIdentity::from_private_key(&private_key).unwrap();
        assert_eq!(
            identity.identity_hash(),
            decode_hex::<16>("fd9f121e293bf4a415dd74366ff75f69")
        );
        let node = NodeCore::<4, 1, 4, 2, 0>::new(
            identity,
            config::RNS_APPLICATION_NAME,
            &config::RNS_PRIMARY_ASPECTS,
            NodeInstanceId::new([0x5a; 16]),
            NodeConfig::transport(),
        )
        .unwrap();
        assert_eq!(
            node.destination_hash().as_bytes(),
            &decode_hex::<16>("3ab9afdbfea4ba1e1806384282afbaec")
        );
    }

    #[test]
    fn destination_name_components_are_python_compatible() {
        assert!(!config::RNS_APPLICATION_NAME.contains('.'));
        assert!(
            config::RNS_PRIMARY_ASPECTS
                .iter()
                .all(|component| !component.contains('.'))
        );
    }

    fn decode_hex<const N: usize>(source: &str) -> [u8; N] {
        let compact = source
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<std::vec::Vec<_>>();
        assert_eq!(compact.len(), N * 2);
        let mut output = [0_u8; N];
        for (index, byte) in output.iter_mut().enumerate() {
            *byte = (nibble(compact[index * 2]) << 4) | nibble(compact[index * 2 + 1]);
        }
        output
    }

    fn nibble(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => panic!("test vector contains non-hex byte"),
        }
    }
}
