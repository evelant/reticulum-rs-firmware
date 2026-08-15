//! Host-checkable product policy for the E290 appliance firmware.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod announce_time;
pub mod authenticated_api_node;
pub mod authenticated_session;
pub mod ble_api_profile;
#[cfg(feature = "appliance")]
pub mod ble_bond_handoff;
pub mod causal_pairing_frontier;
pub mod config;
pub mod credential_boot;
pub mod credential_pairing;
pub mod credential_runtime;
pub mod cross_store_gate;
pub mod diagnostics_api;
pub mod display_coordinator;
pub mod display_handoff;
#[cfg(feature = "display")]
pub mod display_render;
#[cfg(feature = "gateway")]
pub mod dns_wire;
pub mod durability_boot;
pub mod durability_policy;
pub mod live_pairing_handoff;
pub mod live_pairing_node;
pub mod lxmf_delivery;
pub mod network_config_replay;
pub mod nomad_api;
pub mod nomad_coordinator;
pub mod nomad_responder;
pub mod nomad_runtime;
pub mod pairing_control_handoff;
pub mod pairing_control_mapping;
pub mod pairing_policy;
pub mod pairing_records;
pub mod partition_contract;
pub mod radio_diagnostics;
pub mod radio_trace;
pub mod reticulum_probe;
pub mod rmap_discovery;
pub mod session_admission_handoff;
pub mod wifi_driver_metrics;
#[cfg(feature = "gateway")]
pub mod wifi_station_profile;
#[cfg(feature = "gateway")]
pub mod wifi_tcp_profile;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod live_admission_test_support;
#[cfg(test)]
mod live_admission_tests;

use reticulum_device_api_credential_store::{CredentialStoreBinding, CredentialStoreDeviceId};
use reticulum_lxmf_mailbox_store::{MailboxStoreBinding, MailboxStoreDeviceId};
use reticulum_lxmf_store::{LxmfStoreBinding, LxmfStoreDeviceId};
use reticulum_network_config_store::{NetworkConfigStoreBinding, NetworkConfigStoreDeviceId};
use reticulum_storage_actor::{JournalBinding, StorageDeviceId};

/// Derive the coordinator's physical-flash identifier from the E290 eFuse MAC.
pub const fn storage_device_id_from_eui48(mac: [u8; 6]) -> StorageDeviceId {
    StorageDeviceId::new([
        b'e', b'2', b'9', b'0', b'-', b'f', b'l', b'a', b's', b'h', mac[0], mac[1], mac[2], mac[3],
        mac[4], mac[5],
    ])
}

/// Derive the stable public device-API identifier from the E290 eFuse MAC.
///
/// This namespace is intentionally distinct from the physical flash binding.
/// Pairing and authenticated-session transcripts use these exact 16 bytes.
pub const fn device_api_id_from_eui48(mac: [u8; 6]) -> [u8; 16] {
    reticulum_device_api_ble::device_api_id(mac)
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

/// Bind the device-API credential store to the same physical E290 flash ID.
pub const fn api_credentials_binding(device: StorageDeviceId) -> CredentialStoreBinding {
    let bytes = device.as_bytes();
    CredentialStoreBinding::new(
        CredentialStoreDeviceId::new([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]),
        partition_contract::API_CREDENTIALS_OFFSET as usize,
        partition_contract::API_CREDENTIALS_LEN as usize,
        reticulum_device_api_credential_store::PHYSICAL_FORMAT_VERSION,
    )
}

/// Bind the network-configuration store to the first two sectors of the
/// product configuration arena on the same physical E290 flash device.
pub const fn network_config_binding(device: StorageDeviceId) -> NetworkConfigStoreBinding {
    let bytes = device.as_bytes();
    NetworkConfigStoreBinding::new(
        NetworkConfigStoreDeviceId::new([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]),
        partition_contract::NETWORK_CONFIG_OFFSET as usize,
        partition_contract::NETWORK_CONFIG_LEN as usize,
        reticulum_network_config_store::PHYSICAL_FORMAT_VERSION,
    )
}

/// Bind the append-only LXMF store to the same physical E290 flash ID.
pub const fn lxmf_store_binding(device: StorageDeviceId) -> LxmfStoreBinding {
    let bytes = device.as_bytes();
    LxmfStoreBinding::new(
        LxmfStoreDeviceId::new([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]),
        partition_contract::LXMF_STORE_OFFSET as usize,
        partition_contract::LXMF_STORE_LEN as usize,
        reticulum_lxmf_store::PHYSICAL_FORMAT_VERSION,
    )
}

/// Bind durable LXMF collection state inside the product configuration arena.
pub const fn lxmf_mailbox_store_binding(device: StorageDeviceId) -> MailboxStoreBinding {
    let bytes = device.as_bytes();
    MailboxStoreBinding::new(
        MailboxStoreDeviceId::new([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]),
        partition_contract::LXMF_MAILBOX_STATE_OFFSET as usize,
        partition_contract::LXMF_MAILBOX_STATE_LEN as usize,
        reticulum_lxmf_mailbox_store::PHYSICAL_FORMAT_VERSION,
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
const _: () = assert!(
    partition_contract::API_CREDENTIALS_LEN as usize
        == reticulum_device_api_credential_store::PARTITION_SIZE
);
const _: () =
    assert!(partition_contract::BLE_BOND_LEN as usize == reticulum_ble_bond_store::PARTITION_SIZE);
const _: () = assert!(
    partition_contract::NETWORK_CONFIG_LEN as usize
        == reticulum_network_config_store::PARTITION_SIZE
);
const _: () = assert!(
    partition_contract::LXMF_MAILBOX_STATE_LEN as usize
        == reticulum_lxmf_mailbox_store::PARTITION_SIZE
);
#[cfg(test)]
mod tests {
    extern crate std;

    use crate::{
        api_credentials_binding, config, device_api_id_from_eui48, lxmf_mailbox_store_binding,
        lxmf_store_binding, network_config_binding, node_journal_binding, partition_contract,
        storage_device_id_from_eui48,
    };
    use reticulum_node_core::{NodeConfig, NodeCore, NodeIdentity, NodeInstanceId};

    #[test]
    fn permanent_partition_contract_preserves_exact_store_boundaries() {
        assert_eq!(partition_contract::API_CREDENTIALS_OFFSET, 0x0061_4000);
        assert_eq!(partition_contract::API_CREDENTIALS_LEN, 0x0000_2000);
        assert_eq!(partition_contract::BLE_BOND_OFFSET, 0x0061_6000);
        assert_eq!(partition_contract::BLE_BOND_LEN, 0x0000_2000);
        assert_eq!(partition_contract::DEVICE_CONFIG_OFFSET, 0x0061_8000);
        assert_eq!(partition_contract::DEVICE_CONFIG_LEN, 0x0001_8000);
        assert_eq!(partition_contract::NETWORK_CONFIG_OFFSET, 0x0061_8000);
        assert_eq!(partition_contract::NETWORK_CONFIG_LEN, 0x0000_2000);
        assert_eq!(partition_contract::LXMF_MAILBOX_STATE_OFFSET, 0x0061_a000);
        assert_eq!(partition_contract::LXMF_MAILBOX_STATE_LEN, 0x0000_2000);
        assert_eq!(partition_contract::NODE_JOURNAL_OFFSET, 0x0063_0000);
        assert_eq!(partition_contract::NODE_JOURNAL_LEN, 0x0010_0000);
        assert_eq!(partition_contract::LXMF_STORE_OFFSET, 0x0073_0000);
        assert_eq!(partition_contract::LXMF_STORE_LEN, 0x0040_0000);
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
            partition_contract::BLE_BOND_OFFSET
        );
        assert_eq!(
            partition_contract::BLE_BOND_OFFSET + partition_contract::BLE_BOND_LEN,
            partition_contract::DEVICE_CONFIG_OFFSET
        );
        assert_eq!(
            partition_contract::DEVICE_CONFIG_OFFSET + partition_contract::DEVICE_CONFIG_LEN,
            0x0063_0000
        );
        assert_eq!(
            partition_contract::NODE_JOURNAL_OFFSET + partition_contract::NODE_JOURNAL_LEN,
            partition_contract::LXMF_STORE_OFFSET
        );
        assert_eq!(
            partition_contract::LXMF_STORE_OFFSET + partition_contract::LXMF_STORE_LEN,
            0x00b3_0000
        );
        assert_eq!(
            partition_contract::API_CREDENTIALS_LABEL_BYTES,
            *b"api_credentials\0"
        );
        assert_eq!(
            partition_contract::BLE_BOND_LABEL_BYTES,
            *b"ble_bond\0\0\0\0\0\0\0\0"
        );
        assert_eq!(
            partition_contract::DEVICE_CONFIG_LABEL_BYTES,
            *b"device_config\0\0\0"
        );
        assert_eq!(
            partition_contract::NODE_JOURNAL_LABEL_BYTES,
            *b"node_journal\0\0\0\0"
        );
        assert_eq!(
            partition_contract::LXMF_STORE_LABEL_BYTES,
            *b"lxmf_store\0\0\0\0\0\0"
        );
        let device = storage_device_id_from_eui48([0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88]);
        assert_eq!(device.as_bytes(), b"e290-flash\xac\xa7\x04\xe1\x3e\x88");
        assert_eq!(
            device_api_id_from_eui48([0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88]),
            *b"e290-api-1\xac\xa7\x04\xe1\x3e\x88"
        );
        let binding = node_journal_binding(device);
        assert_eq!(binding.device(), device);
        assert_eq!(binding.absolute_offset(), 0x0063_0000);
        assert_eq!(binding.length(), 0x0010_0000);
        assert_eq!(
            binding.layout_version(),
            reticulum_storage_journal::PHYSICAL_FORMAT_VERSION
        );
        let credential_binding = api_credentials_binding(device);
        assert_eq!(credential_binding.device().as_bytes(), device.as_bytes());
        assert_eq!(credential_binding.absolute_offset(), 0x0061_4000);
        assert_eq!(credential_binding.length(), 0x0000_2000);
        assert_eq!(
            credential_binding.layout_version(),
            reticulum_device_api_credential_store::PHYSICAL_FORMAT_VERSION
        );
        let network_binding = network_config_binding(device);
        assert_eq!(network_binding.device().as_bytes(), device.as_bytes());
        assert_eq!(network_binding.absolute_offset(), 0x0061_8000);
        assert_eq!(network_binding.length(), 0x0000_2000);
        assert_eq!(
            network_binding.format_version(),
            reticulum_network_config_store::PHYSICAL_FORMAT_VERSION
        );
        let lxmf_binding = lxmf_store_binding(device);
        assert_eq!(lxmf_binding.device().as_bytes(), device.as_bytes());
        assert_eq!(lxmf_binding.absolute_offset(), 0x0073_0000);
        assert_eq!(lxmf_binding.length(), 0x0040_0000);
        assert_eq!(
            lxmf_binding.format_version(),
            reticulum_lxmf_store::PHYSICAL_FORMAT_VERSION
        );
        let mailbox_binding = lxmf_mailbox_store_binding(device);
        assert_eq!(mailbox_binding.device().as_bytes(), device.as_bytes());
        assert_eq!(mailbox_binding.absolute_offset(), 0x0061_a000);
        assert_eq!(mailbox_binding.length(), 0x0000_2000);
        assert_eq!(
            mailbox_binding.format_version(),
            reticulum_lxmf_mailbox_store::PHYSICAL_FORMAT_VERSION
        );
    }

    #[test]
    fn current_storage_format_contract_is_intentional() {
        assert_eq!(reticulum_device_identity_store::PHYSICAL_FORMAT_VERSION, 1);
        assert_eq!(reticulum_announce_clock::PHYSICAL_FORMAT_VERSION, 1);
        assert_eq!(
            reticulum_device_api_credential_store::PHYSICAL_FORMAT_VERSION,
            1
        );
        assert_eq!(
            reticulum_device_api_credential_store::SEMANTIC_FORMAT_VERSION,
            2
        );
        assert_eq!(reticulum_ble_bond_store::PHYSICAL_FORMAT_VERSION, 1);
        assert_eq!(reticulum_ble_bond_store::SEMANTIC_FORMAT_VERSION, 2);
        assert_eq!(reticulum_network_config_store::PHYSICAL_FORMAT_VERSION, 1);
        assert_eq!(reticulum_network_config_store::SEMANTIC_FORMAT_VERSION, 5);
        assert_eq!(reticulum_lxmf_mailbox_store::PHYSICAL_FORMAT_VERSION, 2);
        assert_eq!(reticulum_storage_journal::PHYSICAL_FORMAT_VERSION, 2);
        assert_eq!(reticulum_storage_model::JOURNAL_SCHEMA_VERSION, 4);
        assert_eq!(reticulum_lxmf_store::PHYSICAL_FORMAT_VERSION, 3);
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
