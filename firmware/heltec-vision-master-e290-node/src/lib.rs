//! Host-checkable product policy for the first permanent E290 node image.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod announce_time;
pub mod authenticated_api_node;
pub mod ble_api_profile;
#[cfg(feature = "ble-api-proof")]
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
#[cfg(feature = "wifi-tcp-proof")]
pub mod dns_wire;
pub mod durability_boot;
pub mod durability_policy;
#[cfg(all(
    feature = "rns-inbox-commit-fault-hil",
    any(test, target_arch = "xtensa")
))]
pub mod inbox_admission_fault_hil;
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
pub mod partition_contract;
pub mod radio_diagnostics;
pub mod radio_trace;
pub mod reticulum_probe;
pub mod rmap_discovery;
#[cfg(feature = "runtime-measurement-hil")]
pub mod runtime_measurement;
pub mod session_admission_handoff;
pub mod usb_authenticated_session;
pub mod usb_pairing_policy;
pub mod usb_pairing_records;
pub mod wifi_api_profile;
pub mod wifi_driver_metrics;
#[cfg(feature = "wifi-station-proof")]
pub mod wifi_station_profile;
#[cfg(feature = "wifi-tcp-proof")]
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
use reticulum_rns_inbox_store::{InboxStoreBinding, InboxStoreDeviceId};
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

/// Bind the durable inbound qualification store to the same physical flash ID.
pub const fn rns_inbox_binding(device: StorageDeviceId) -> InboxStoreBinding {
    let bytes = device.as_bytes();
    InboxStoreBinding::new(
        InboxStoreDeviceId::new([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]),
        partition_contract::MESSAGE_STORE_OFFSET as usize,
        partition_contract::MESSAGE_STORE_LEN as usize,
        reticulum_rns_inbox_store::PHYSICAL_FORMAT_VERSION,
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
const _: () = assert!(
    partition_contract::MESSAGE_STORE_LEN as usize == reticulum_rns_inbox_store::PARTITION_SIZE
);
const _: () = assert!(
    reticulum_rns_inbox_store::MAX_PAYLOAD_SIZE
        == reticulum_device_api::MAX_RNS_INBOX_PAYLOAD_BYTES
);

#[cfg(test)]
mod tests {
    extern crate std;

    use crate::{
        api_credentials_binding, config, device_api_id_from_eui48, lxmf_store_binding,
        network_config_binding, node_journal_binding, partition_contract, rns_inbox_binding,
        storage_device_id_from_eui48,
    };
    use reticulum_node_core::{NodeConfig, NodeCore, NodeIdentity, NodeInstanceId};
    use std::vec::Vec;

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
        assert_eq!(partition_contract::NODE_JOURNAL_OFFSET, 0x0063_0000);
        assert_eq!(partition_contract::NODE_JOURNAL_LEN, 0x0010_0000);
        assert_eq!(partition_contract::MESSAGE_STORE_OFFSET, 0x0073_0000);
        assert_eq!(partition_contract::MESSAGE_STORE_LEN, 0x0020_0000);
        assert_eq!(partition_contract::LXMF_STORE_OFFSET, 0x0093_0000);
        assert_eq!(partition_contract::LXMF_STORE_LEN, 0x0020_0000);
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
            partition_contract::MESSAGE_STORE_OFFSET
        );
        assert_eq!(
            partition_contract::MESSAGE_STORE_OFFSET + partition_contract::MESSAGE_STORE_LEN,
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
            partition_contract::MESSAGE_STORE_LABEL_BYTES,
            *b"message_store\0\0\0"
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
        let inbox_binding = rns_inbox_binding(device);
        assert_eq!(inbox_binding.device().as_bytes(), device.as_bytes());
        assert_eq!(inbox_binding.absolute_offset(), 0x0073_0000);
        assert_eq!(inbox_binding.length(), 0x0020_0000);
        assert_eq!(
            inbox_binding.format_version(),
            reticulum_rns_inbox_store::PHYSICAL_FORMAT_VERSION
        );
        let lxmf_binding = lxmf_store_binding(device);
        assert_eq!(lxmf_binding.device().as_bytes(), device.as_bytes());
        assert_eq!(lxmf_binding.absolute_offset(), 0x0093_0000);
        assert_eq!(lxmf_binding.length(), 0x0020_0000);
        assert_eq!(
            lxmf_binding.format_version(),
            reticulum_lxmf_store::PHYSICAL_FORMAT_VERSION
        );
    }

    #[test]
    fn credential_recovery_precedes_every_other_boot_flash_mutation() {
        let source = include_str!("main.rs");
        let flash_open = source
            .find("ProductFlashOwner::open(flash, storage_device_id)")
            .expect("main must construct the checked sole flash owner");
        let credential_boot = source
            .find("let credential_boot = flash_owner.boot_credentials();")
            .expect("main must mount and recover credentials");
        assert!(flash_open < credential_boot);
        let ble_bond_boot = source
            .find("flash_owner.boot_ble_bond()")
            .expect("main must read-only mount BLE bond authority");
        assert!(
            credential_boot < ble_bond_boot,
            "credential recovery must precede BLE bond boot mount"
        );

        for later_operation in [
            "flash_owner.inspect_identity()",
            "flash_owner.provision_node_journal(journal_policy)",
            "flash_owner.reserve_announce_epoch(fresh_clock_policy)",
            "flash_owner.boot_identity(&mut bootstrap_rng)",
            "flash_owner.mount_node_runtime(",
            "flash_owner.mount_inbox()",
            "flash_owner.mount_lxmf(lxmf_index)",
        ] {
            let later = source
                .find(later_operation)
                .unwrap_or_else(|| panic!("main is missing {later_operation}"));
            assert!(
                credential_boot < later,
                "credential boot must precede {later_operation}"
            );
            assert!(
                ble_bond_boot < later,
                "read-only BLE bond mount must precede {later_operation}"
            );
        }
    }

    #[test]
    fn ble_bond_boot_recovery_samples_the_sole_gpio_owner_before_wireless_startup() {
        let main = include_str!("main.rs");
        assert_eq!(
            main.matches("peripherals.GPIO21").count(),
            1,
            "USB and BLE profiles must share one cfg-selected GPIO21 construction"
        );
        assert!(main.contains(
            "#[cfg(not(feature = \"wifi-api-proof\"))]\n    let pairing_button = Input::new("
        ));
        assert!(main.contains(
            "#[cfg(all(feature = \"ble-api-proof\", feature = \"wifi-api-proof\"))]\ncompile_error!"
        ));

        let gpio_owner = main
            .find("let pairing_button = Input::new(")
            .expect("the product must construct its sole GPIO21 input");
        let first_sample = main
            .find("let boot_ble_bond_first_sample_asserted = pairing_button.is_low();")
            .expect("BLE recovery must capture the first boot sample");
        let rtos_start = main
            .find("esp_rtos::start(timers.timer0, software_interrupts.software_interrupt0);")
            .expect("the monotonic timer must be started explicitly");
        let gesture_observation = main
            .find(
                "observe_boot_ble_bond_recovery(&pairing_button, boot_ble_bond_first_sample_asserted)",
            )
            .expect("main must observe the retained boot gesture");
        let ble_controller = main
            .find("ble_api_task::initialize_controller(peripherals.BT)")
            .expect("the BLE profile must initialize its controller");
        let physical_clear = main
            .find("flash_owner.clear_boot_ble_bond_for_physical_recovery(boot)")
            .expect("an authorized gesture must reach the bond-only clear");

        assert!(gpio_owner < first_sample);
        assert!(
            first_sample < rtos_start,
            "the first physical sample must precede timer-dependent startup"
        );
        assert!(
            rtos_start < gesture_observation,
            "async gesture observation must not run before the RTOS time driver"
        );
        assert!(gesture_observation < ble_controller);
        assert!(ble_controller < physical_clear);

        let helper_start = main
            .find("async fn observe_boot_ble_bond_recovery(")
            .expect("the bounded boot gesture helper must exist");
        let helper_end = main[helper_start..]
            .find("\n#[cfg(feature = \"ble-api-proof\")]\nconst _: () = assert!")
            .map(|offset| helper_start + offset)
            .expect("the bounded boot gesture helper must end before its static timing audit");
        let helper = &main[helper_start..helper_end];
        assert!(helper.contains(
            "BootBleBondRecoveryGesture::new(started_at_ms, first_boot_sample_asserted)"
        ));
        assert!(helper.contains("pairing_button.is_low()"));
        assert!(helper.contains(
            "Timer::after(Duration::from_millis(BLE_BOND_BOOT_RECOVERY_SAMPLE_MS)).await"
        ));
    }

    #[test]
    fn authorized_ble_bond_clear_replaces_the_only_restored_bond_owner() {
        let main = include_str!("main.rs");
        let mount = main
            .find("let boot_ble_bond = match flash_owner.boot_ble_bond()")
            .expect("main must retain the mounted boot bond owner");
        let authorization = main
            .find("Ok(boot) if boot_ble_bond_recovery_authorized =>")
            .expect("only an authorized gesture may select physical recovery");
        let clear = main
            .find("flash_owner.clear_boot_ble_bond_for_physical_recovery(boot)")
            .expect("physical recovery must consume the mounted owner");
        let recovered_owner = main
            .find("Ok(recovered) =>")
            .expect("successful recovery must return a replacement owner");
        let retain_recovered_owner = main[recovered_owner..]
            .find("Ok(recovered)")
            .map(|offset| recovered_owner + offset)
            .expect("the post-clear owner must become boot bond authority");
        let restore = main
            .find("let (restored_ble_bond, boot_ble_bond_store_available) = match boot_ble_bond")
            .expect("BLE restoration must consume the selected boot authority");
        let into_bond = main
            .find("(boot.into_bond(), true)")
            .expect("the selected authority must be the sole restored bond source");
        let ble_owner = main
            .find("ble_api_task::BlePhysicalOwners::new(")
            .expect("the restored bond must eventually move into the BLE task");

        assert!(mount < authorization);
        assert!(authorization < clear);
        assert!(clear < recovered_owner);
        assert!(recovered_owner <= retain_recovered_owner);
        assert!(retain_recovered_owner < restore);
        assert!(restore < into_bond);
        assert!(into_bond < ble_owner);
        assert_eq!(
            main.matches(".into_bond()").count(),
            1,
            "no pre-clear or parallel boot owner may feed Trouble host restoration"
        );
    }

    #[test]
    fn home_snapshot_uses_application_authority_and_one_display_owner() {
        let main = include_str!("main.rs");
        let count = main
            .find("let active_credential_count = storage_coordinator.active_credential_count();")
            .expect("Home setup must read the publishable application authority");
        let home_start = main
            .find("let display_home = {")
            .expect("main must compose one complete non-secret Home snapshot");
        let home_end = main[home_start..]
            .find("// Finish the qualified boot-composition Home render")
            .map(|offset| home_start + offset)
            .expect("Home composition must precede the physical render gate");
        let home = &main[home_start..home_end];
        assert!(count < home_start);
        let setup_start = home
            .find("let application_setup = DisplaySetupState::from_application_state(")
            .expect("Home setup must derive from application credential authority");
        let setup_end = home[setup_start..]
            .find(");")
            .map(|offset| setup_start + offset)
            .expect("the application setup expression must remain bounded");
        let setup = &home[setup_start..setup_end];
        assert!(setup.contains("active_credential_count,"));
        assert!(setup.contains("credential_pairing_policy_available,"));
        assert!(
            !setup.contains("bond"),
            "Bluetooth security state must not define application setup"
        );
        let ble_availability = main
            .find(
                "let ble_display_available = ble_connector.is_some() && boot_ble_bond_store_available;",
            )
            .expect("Home BLE composition must require controller and bond-store availability");
        assert!(ble_availability < home_start);
        assert!(home.contains("let ble = if ble_display_available {"));
        assert!(home.contains(
            "application_setup.with_ble_bond(ble_display_available, durable_ble_bond_present)"
        ));
        assert!(home.contains("display_device_suffix(base_mac_eui48)"));
        assert!(home.contains("DisplayCompositionState::Configured"));

        let home_request = main
            .find("let home_request = publisher.publish_latest(DisplayCommand::ShowHome {")
            .expect("startup must publish the complete Home snapshot");
        let home_request_end = main[home_request..]
            .find("});")
            .map(|offset| home_request + offset)
            .expect("the startup Home request must remain bounded");
        assert!(main[home_request..home_request_end].contains("snapshot: display_home,"));
        let home_completion = main
            .find("wait_for_rendered_completion(request_id, DisplayViewKind::Home)")
            .expect("startup must physically gate the initial Home view");
        let ble_owner = main
            .find("ble_api_task::BlePhysicalOwners::new(")
            .expect("BLE onboarding must receive the sole display publisher");
        assert!(home_request < home_completion);
        assert!(home_completion < ble_owner);
        assert!(
            main[ble_owner..].contains("display_home,"),
            "BLE onboarding must retain a copy of the exact rendered Home snapshot"
        );

        let helper_start = main
            .find("fn display_device_suffix")
            .expect("the board must expose its discovery suffix");
        let helper_end = main[helper_start..]
            .find("\n}\n")
            .map(|offset| helper_start + offset)
            .expect("the suffix helper must be bounded");
        let helper = &main[helper_start..helper_end];
        assert!(helper.contains("reticulum_device_api_ble::local_name(base_mac)"));
        assert!(helper.contains("reticulum_device_api_ble::LOCAL_NAME_PREFIX.len()"));

        let storage = include_str!("platform_storage.rs");
        assert!(storage.contains("pub(crate) fn active_credential_count(&self) -> Option<usize>"));
        assert!(
            storage.contains("self.credential_runtime.active_credential_count()"),
            "the flash coordinator must delegate to mounted credential authority"
        );
    }

    #[test]
    fn ble_bond_boot_is_read_only_and_commit_failures_remount_fail_closed() {
        let storage = include_str!("platform_storage.rs");
        let boot_start = storage
            .find("pub(crate) fn boot_ble_bond")
            .expect("the sole flash owner must expose BLE bond boot mount");
        let boot_end = storage[boot_start..]
            .find("pub(crate) fn clear_boot_ble_bond_for_physical_recovery")
            .map(|offset| boot_start + offset)
            .expect("the explicit physical-recovery method must follow ordinary boot mount");
        let boot = &storage[boot_start..boot_end];
        assert!(boot.contains("mount_ble_bond_store(&mut region)?"));
        assert!(!boot.contains("commit_ble_bond_store"));
        assert!(!boot.contains("recover_empty"));
        assert!(!boot.contains("cleanup("));

        let commit_start = storage
            .find("pub(crate) fn commit_ble_bond")
            .expect("the coordinator must expose owning BLE bond commit");
        let commit_end = storage[commit_start..]
            .find("pub(crate) const fn credential_boot_state")
            .map(|offset| commit_start + offset)
            .expect("credential accessors must follow bond commit");
        let commit = &storage[commit_start..commit_end];
        let write = commit
            .find("commit_ble_bond_store(&mut region, bond)")
            .expect("commit must use the portable exact-verification operation");
        let reconcile = commit
            .find("mount_ble_bond_store(&mut region)")
            .expect("every failed commit must immediately remount");
        assert!(write < reconcile);
        assert!(commit.contains("ReconciledRebootRequired"));
        assert!(commit.contains("ProductBleBondStoreState::unavailable()"));
    }

    #[test]
    fn growth_oriented_protocol_owners_are_external_when_composed() {
        assert_eq!(
            config::LXMF_INDEX_SLOTS,
            partition_contract::LXMF_STORE_LEN as usize / reticulum_lxmf_store::EXTENT_SIZE
        );
        assert_eq!(config::LXMF_INDEX_SLOTS, 512);

        let main = include_str!("main.rs");
        assert_eq!(main.matches("Vec::new_in(ExternalMemory)").count(), 2);
        assert_eq!(main.matches("Box::try_new_in(").count(), 2);
        assert!(main.contains("Box::try_new_in(E290FrameBuffer::new_white(), ExternalMemory)"));
        assert!(main.contains("stage=display-placement status=PASS"));
        assert!(main.contains("Box::<ProductSubmissionRuntime, _>::try_new_uninit_in("));
        assert_eq!(main.matches("try_new_uninit_in(").count(), 2);
        assert!(main.contains("Box::leak(runtime)"));
        assert!(main.contains("Some(Box::leak(runtime))"));
        assert!(main.contains("node_task::ApplicationVolatileState::new()"));
        assert!(main.contains(
            "let application_volatile: &'static mut node_task::ApplicationVolatileState"
        ));
        assert!(main.contains("let mut delayed_proof_storage = Vec::new_in(ExternalMemory)"));
        assert!(main.contains(".try_reserve_exact(config::LXMF_DELAYED_PROOF_SLOTS)"));
        assert!(main.contains("delayed_proof_storage.len() != config::LXMF_DELAYED_PROOF_SLOTS"));
        assert!(main.contains("let delayed_proof_storage: &'static mut [DelayedProofSlot]"));
        assert!(!main.contains("static LXMF_DELAYED_PROOF_STORAGE:"));
        assert!(main.contains(".try_reserve_exact(config::LXMF_INDEX_SLOTS)"));
        assert!(main.contains("lxmf_index.len() != config::LXMF_INDEX_SLOTS"));
        assert!(main.contains("let lxmf_index: &'static mut [LxmfStoreIndexSlot]"));
        assert!(main.contains("flash_owner.mount_lxmf(lxmf_index)"));
        let node_helper_start = main
            .find("fn construct_node_or_inert(")
            .expect("node composition must retain one bounded startup helper");
        let node_helper_end = main[node_helper_start..]
            .find("fn construct_ordinary_coordinator_or_inert(")
            .map(|offset| node_helper_start + offset)
            .expect("the ordinary helper must follow in-place node construction");
        let node_helper = &main[node_helper_start..node_helper_end];
        assert!(
            node_helper.contains("Box::<ProductSupervisor, ExternalMemory>::try_new_uninit_in(")
        );
        assert!(node_helper.contains("ExternalMemory,"));
        assert!(node_helper.contains("ProductSupervisor::begin_node_in("));
        assert!(node_helper.contains("psram_start_address: usize"));
        assert!(node_helper.contains("psram_end_address: usize"));
        let supervisor_allocation = node_helper
            .find("Box::<ProductSupervisor, ExternalMemory>::try_new_uninit_in(")
            .expect("the startup helper must allocate uninitialized supervisor storage externally");
        let supervisor_bounds = node_helper
            .find("supervisor_start < psram_start_address")
            .expect("the external supervisor must be checked against the PSRAM lower bound");
        assert!(node_helper.contains("supervisor_end > psram_end_address"));
        let supervisor_leak = node_helper
            .find("let supervisor_storage = Box::leak(supervisor_storage)")
            .expect("the startup helper must retain boot-lifetime supervisor storage");
        let node_in_place = node_helper
            .find("ProductSupervisor::begin_node_in(")
            .expect("the node must initialize at its final supervisor address");
        assert!(supervisor_allocation < supervisor_bounds);
        assert!(supervisor_bounds < supervisor_leak);
        assert!(supervisor_leak < node_in_place);
        let supervisor_helper_start = main
            .find("fn construct_supervisor_or_inert(")
            .expect("supervisor composition must retain one bounded startup helper");
        let supervisor_helper_end = main[supervisor_helper_start..]
            .find("#[esp_hal::main]")
            .map(|offset| supervisor_helper_start + offset)
            .expect("the synchronous entrypoint must follow the supervisor helper");
        let supervisor_helper = &main[supervisor_helper_start..supervisor_helper_end];
        assert!(supervisor_helper.contains("stage: ProductSupervisorInit<'static>"));
        assert!(supervisor_helper.contains("match stage.finish("));
        assert!(!supervisor_helper.contains("Box::try_new_in("));
        assert!(supervisor_helper.contains("-> (\n    &'static mut ProductSupervisor,"));
        assert!(!main.contains("static SUPERVISOR:"));
        let psram_allocator = main
            .find("esp_alloc::psram_allocator!(&psram);")
            .expect("external allocation must follow PSRAM registration");
        let radio_initialization = main
            .find("let radio = match E290Radio::new(")
            .expect("the concrete LoRa actor must still be constructed");
        let node_composition = main
            .find("let mut supervisor_stage =")
            .expect("node construction must retain its bounded startup helper");
        assert!(main[node_composition..].starts_with("let mut supervisor_stage ="));
        assert!(
            main[node_composition..]
                .find("construct_node_or_inert(")
                .is_some_and(|offset| offset < 128)
        );
        let supervisor_composition = main
            .find("let (supervisor, actors) = construct_supervisor_or_inert(")
            .expect("supervisor construction must retain its bounded startup helper");
        let supervisor_call_end = main[supervisor_composition..]
            .find("        );")
            .map(|offset| supervisor_composition + offset)
            .expect("the supervisor helper call must remain bounded");
        let supervisor_call = &main[supervisor_composition..supervisor_call_end];
        assert!(supervisor_call.contains("supervisor_stage,"));
        assert!(!supervisor_call.contains("psram_start_address,"));
        assert!(!supervisor_call.contains("psram_end_address,"));
        let node_task_construction = main
            .find("let node_task = match node_task::run(")
            .expect("the sole node task must retain the supervisor");
        assert!(psram_allocator < radio_initialization);
        assert!(radio_initialization < node_composition);
        assert!(node_composition < supervisor_composition);
        assert!(supervisor_composition < node_task_construction);
        assert!(radio_initialization < node_task_construction);
        assert!(main.contains("activate_lxmf_delivery(node, lxmf_service_available)"));
        assert!(main.contains("activate_nomad_responder(node)"));
        assert!(main.contains("lxmf_delivery_admission={lxmf_delivery_admission}"));
        assert!(main.contains(
            "durability=required-for-all-carriers accepts_links=true data_profile=opportunistic+responder-direct-link"
        ));
        assert!(main.contains("resource_ingress=disabled"));

        let lxmf_delivery = include_str!("lxmf_delivery.rs");
        assert!(lxmf_delivery.contains("node.set_destination_accepts_links(&destination, true)"));

        let platform_storage = include_str!("platform_storage.rs");
        let direct_mount = platform_storage
            .find("ProductSubmissionRuntime::mount_into(")
            .expect("runtime must replay directly into its external allocation");
        let typed_box = platform_storage
            .find("unsafe { storage.assume_init() }")
            .expect("successful direct replay must convert the initialized allocation");
        assert!(direct_mount < typed_box);
        assert!(main.contains("DelayedProofOwner::new(delayed_proof_storage)"));
        assert!(main.contains("application_volatile_placement=external-psram"));
        assert!(main.contains("lxmf_delayed_proof_placement=external-psram"));

        let node_task = include_str!("node_task.rs");
        assert!(node_task.contains("pub(crate) struct ApplicationVolatileState"));
        assert!(node_task.contains("retries: LxmfRetrySet"));
        assert!(node_task.contains("proof_holder: LxmfProofActionsHolder"));
        assert!(node_task.contains("authority_faults: LxmfAuthorityFault"));
        assert!(node_task.contains("admission_retry_delay_ms(attempt, sequence)"));
        assert!(node_task.contains("retries.wake_admission_for_source(destination_hash, now_ms)"));
        assert!(node_task.contains("nomad: ProductNomadRuntimeState"));
        assert!(node_task.contains("application_volatile: &'static mut ApplicationVolatileState"));
        assert!(node_task.contains("enum OrdinaryProtocolDispatch"));
        assert!(node_task.contains("OrdinaryProtocolDispatch::Nomad"));
        assert!(node_task.contains("NomadEventObservation::Applied"));
        assert!(node_task.contains("5 => {"));
        assert!(node_task.contains("let nomad_progressed = drive_one_nomad_command("));
        assert!(
            node_task.contains("fresh_nomad_turn_armed = config::next_fresh_nomad_turn_armed(")
        );
        assert!(node_task.contains("ApplicationEvent::RequestValueReceived { .. } =>"));
        assert!(node_task.contains("ScheduledAnnounce::NomadNode"));
        assert!(node_task.contains("Some(NOMAD_NODE_ANNOUNCE_APP_DATA.as_bytes())"));
        assert!(node_task.contains("classify_nomad_responder_event(&nomad_destination"));
        assert!(node_task.contains("supervisor.prepare_response_actions("));
        assert!(node_task.contains("ApplicationEvent::LinkData {"));
        assert!(node_task.contains("*context == APPLICATION_LINK_CONTEXT_NONE"));
        assert!(node_task.contains("binding.role() == ApplicationLinkRole::Responder"));
        assert!(node_task.contains("binding.destination() == lxmf.as_bytes()"));
        assert!(!node_task.contains(
            "let mut lxmf_retries = LxmfRetrySet::<{ config::APPLICATION_EVENT_SLOTS }>::new()"
        ));

        let non_lxmf_start = node_task
            .find("fn drive_non_lxmf_application_event(")
            .expect("permanent task must classify non-LXMF application events");
        let responder_helper_start = node_task
            .find("fn drive_prepared_nomad_response(")
            .expect("permanent task must retain prepared Nomad responses");
        let non_lxmf = &node_task[non_lxmf_start..responder_helper_start];
        let retry_reservation = non_lxmf
            .find("let response_retry_slot = if retry_actions_a.is_none()")
            .expect("Nomad response preparation must reserve an exact retry owner");
        let response_preparation = non_lxmf
            .find("supervisor.prepare_response_actions(")
            .expect("Nomad responder must use the native response wrapper");
        assert!(
            retry_reservation < response_preparation,
            "a response retry owner must be selected before crypto preparation"
        );

        let lxmf_helper_start = node_task
            .find("fn drive_lxmf_event(")
            .expect("LXMF application-event consumer must remain composed");
        let responder_helper = &node_task[responder_helper_start..lxmf_helper_start];
        let response_handoff = responder_helper
            .find("supervisor.try_offer_actions(")
            .expect("prepared response must enter the ordinary supervisor");
        let response_retry_retention = responder_helper
            .find("*response_retry_slot = Some(retained);")
            .expect("a busy supervisor must return the exact response to its reserved owner");
        let request_acknowledgement = responder_helper
            .find("match lease.acknowledge()")
            .expect("request event must be acknowledged after response handoff");
        assert!(
            response_handoff < response_retry_retention
                && response_retry_retention < request_acknowledgement,
            "request acknowledgement must follow either response ownership transfer"
        );
        assert!(
            !responder_helper.contains("with_protocol_dispatch"),
            "inbound responses must not enter outbound Nomad request reconciliation"
        );
        assert!(
            !responder_helper.contains(".clone()"),
            "prepared response ownership must move exactly once"
        );

        let storage = include_str!("platform_storage.rs");
        assert!(storage.contains("runtime: Option<&'static mut ProductSubmissionRuntime>"));
        assert!(storage.contains("lxmf: Option<MountedLxmfStore<'static>>"));
        assert!(storage.contains("DurableIngressProofMode::Required"));
        assert!(!storage.contains("DurableIngressProofMode::Optional"));
        let offer = storage
            .split("pub(crate) fn offer_authorized_frame(")
            .nth(1)
            .and_then(|tail| tail.split("/// Advance at most one durable").next())
            .expect("the journal projection offer must have a stable source boundary");
        let gate = offer
            .find("journal_projection_gate(self.lxmf_mutation_pending())")
            .expect("LXMF pending mutation must gate journal projection");
        let mutation = offer
            .find("offer_authorized_frame(observation)")
            .expect("the runtime projection call must remain explicit");
        assert!(gate < mutation);
        assert!(offer.contains("return Some(Ok(FrameOfferProgress::Retain));"));
    }

    #[test]
    fn usb_boot_quarantine_precedes_hal_and_requires_clean_reenumeration() {
        let main = include_str!("main.rs");
        assert!(!main.contains("#[esp_rtos::main]"));
        let synchronous_entry = main
            .split("#[esp_hal::main]")
            .nth(1)
            .and_then(|tail| tail.split("#[embassy_executor::task]").next())
            .expect("the product must expose one synchronous earliest entrypoint");
        let quarantine = synchronous_entry
            .find("usb_pairing_task::quarantine_usb_at_boot()")
            .expect("the earliest entrypoint must quarantine USB");
        let executor = synchronous_entry
            .find("esp_rtos::embassy::Executor::new()")
            .expect("the earliest entrypoint must construct the product executor");
        assert!(quarantine < executor);

        let product = main
            .split("async fn product_main(")
            .nth(1)
            .expect("the product composition must remain one explicit async task");
        assert!(product.contains("usb_boot_quarantine: usb_pairing_task::BootUsbQuarantine"));
        assert!(product.contains("let peripherals = esp_hal::init(hal);"));
        let usb_task = product
            .find("let usb_pairing_task = match usb_pairing_task::run(")
            .expect("the product composition must construct one USB task");
        assert!(product[usb_task..].contains("usb_boot_quarantine,"));

        let usb = include_str!("usb_pairing_task.rs");
        let boot = usb
            .split("pub(crate) fn quarantine_usb_at_boot()")
            .nth(1)
            .and_then(|tail| tail.split("#[esp_hal::handler]").next())
            .expect("the USB owner must expose one bounded boot quarantine");
        let pad_off = boot
            .find(".usb_pad_enable()\n            .clear_bit()")
            .expect("boot quarantine must detach the USB pad");
        let memory_down = boot
            .find("write.usb_mem_pd().set_bit()")
            .expect("boot quarantine must power down endpoint RAM");
        let memory_up = boot
            .find("write.usb_mem_pd().clear_bit()")
            .expect("boot quarantine must restore scrubbed endpoint RAM");
        let token = boot
            .find("BootUsbQuarantine { _private: () }")
            .expect("boot quarantine must return its sole proof token");
        assert!(pad_off < memory_down);
        assert!(memory_down < memory_up);
        assert!(memory_up < token);
        assert!(boot.contains("USB_EPOCH_BLOCKED.store(true, Ordering::Release)"));

        let run = usb
            .split("pub async fn run(")
            .nth(1)
            .expect("the USB bearer task must exist");
        let driver = run
            .find("UsbSerialJtag::<Blocking>::new(usb_device)")
            .expect("the detached task must claim the HAL owner");
        let handler = run
            .find("usb_serial.set_interrupt_handler(usb_bus_reset_interrupt)")
            .expect("the detached task must install its reset ISR");
        let dwell = run
            .find("Timer::after(Duration::from_millis(USB_REATTACH_DWELL_MILLIS)).await")
            .expect("the detached task must provide a host-visible dwell");
        let arm = run
            .find("USB_REATTACH_EXPECTED.store(true, Ordering::Release)")
            .expect("the detached task must arm one clean enumeration reset");
        assert!(driver < handler);
        assert!(handler < dwell);
        assert!(dwell < arm);
        assert!(run.contains("USB_CANONICAL_ATTACHED_PAD_CONFIGURATION"));
        assert!(run.contains("esp_hal::efuse::USB_EXCHG_PINS"));
        assert!(run.contains("consumed_bus_reset || boot_reattach_pending"));

        let clean_reset = run
            .find("USB_CLEAN_RESET_GENERATION.load(Ordering::Acquire) == reset_generation")
            .expect("the task must recognize its exact clean reset generation");
        let unblock = run
            .find("USB_EPOCH_BLOCKED.store(false, Ordering::Release)")
            .expect("the task must explicitly unblock the clean epoch");
        assert!(clean_reset < unblock);
    }

    #[test]
    fn usb_preauthentication_bearer_is_a_third_non_interface_owner() {
        let main = include_str!("main.rs");
        assert!(main.contains("PairingControlHandoff::new()).split()"));
        assert!(main.contains("LivePairingHandoff::new()).split()"));
        assert!(main.contains("peripherals.GPIO21"));
        assert!(main.contains("InputConfig::default().with_pull(Pull::Up)"));
        assert!(main.contains("peripherals.USB_DEVICE"));
        assert!(main.contains("spawner.spawn(usb_pairing_task);"));
        assert!(main.contains("3 + if display_task_spawned { 1 } else { 0 }"));
        assert!(
            main.contains("interfaces=1 primary_transport=lora future_transport_actors=deferred")
        );
        assert!(main.contains("esp_println::logger::init_logger(log::LevelFilter::Info)"));

        let usb = include_str!("usb_pairing_task.rs");
        assert_eq!(
            usb.matches("let mut decoder = StreamDecoder::new();")
                .count(),
            1
        );
        assert_eq!(
            usb.matches("let mut sequence_gate: Option<ExactNextSequenceGate>")
                .count(),
            1
        );
        assert!(usb.contains("decode_usb_pre_authentication_request"));
        assert!(usb.contains("awaiting_live_reply"));
        assert!(usb.contains("handle_live_reply"));
        assert!(usb.contains("fn usb_bus_reset_interrupt()"));
        assert!(usb.contains("#[ram]\nfn usb_bus_reset_interrupt()"));
        assert!(usb.contains("usb_serial.set_interrupt_handler(usb_bus_reset_interrupt)"));
        assert!(usb.contains("reset_generation: u32"));
        assert!(usb.contains("TX_EPOCH_ARMED.store(true, Ordering::Release)"));
        assert!(usb.contains("USB_PAD_FORCED_OFF.store(true, Ordering::Release)"));
        assert!(usb.contains("USB_EPOCH_BLOCKED.store(true, Ordering::Release)"));
        assert!(usb.contains("USB_EPOCH_BLOCKED.store(false, Ordering::Release)"));
        assert!(usb.contains("USB_REATTACH_EXPECTED.store(true, Ordering::Release)"));
        assert!(usb.contains("USB_CLEAN_RESET_GENERATION"));
        assert!(usb.contains("previous_generation.wrapping_add(1)"));
        assert!(usb.contains("let raced_clean_reset ="));
        assert!(usb.contains("USB_REATTACH_DWELL_MILLIS"));
        assert!(usb.contains("USB_ATTACHED_PAD_CONFIGURATION.store("));
        assert!(usb.contains(".pad_pull_override()"));
        assert!(usb.contains(".dp_pullup()"));
        assert!(usb.contains(".dm_pullup()"));
        assert!(usb.contains(
            "let eligible_sof = saw_sof && disconnect_pending.is_none() && !epoch_blocked;"
        ));
        assert!(usb.contains("fn try_send_in_epoch<T, E>("));
        assert!(usb.contains("critical_section::with(|_|"));
        assert!(usb.contains(".usb_bus_reset()\n                .bit_is_set()"));
        let reattach = usb
            .split("if pad_reenable_pending")
            .nth(1)
            .and_then(|tail| tail.split("if consumed_bus_reset").next())
            .expect("USB task must expose one guarded reattach attempt");
        let enable_pad = reattach
            .find(".usb_pad_enable()\n                        .bit(attached & USB_PAD_ENABLE_BIT != 0)")
            .expect("reattach must restore the canonical scrubbed pad configuration");
        let reattach_guard = reattach
            .find("let reattached = critical_section::with(|_| {")
            .expect("reattach must linearize its marker and pad enable against the ISR");
        let arm_clean_reset = reattach
            .find("USB_REATTACH_EXPECTED.store(true, Ordering::Release)")
            .expect("reattach must arm exactly one expected clean reset");
        assert!(reattach_guard < arm_clean_reset);
        assert!(arm_clean_reset < enable_pad);
        let send_epoch = usb
            .split("fn try_send_in_epoch<T, E>(")
            .nth(1)
            .and_then(|tail| tail.split("fn retire_transmission(").next())
            .expect("USB task must expose one reset-linearized handoff helper");
        let send_epoch_check = send_epoch
            .find("usb_epoch_current(reset_generation)")
            .expect("handoff must check its admitted epoch");
        let pending_reset_check = send_epoch
            .find(".usb_bus_reset()")
            .expect("handoff must reject a pending reset interrupt");
        let enqueue = send_epoch
            .find("Some(send(")
            .expect("handoff must enqueue inside the guarded section");
        assert!(send_epoch_check < pending_reset_check);
        assert!(pending_reset_check < enqueue);
        assert!(usb.contains("write.usb_mem_pd().set_bit()"));
        assert!(usb.contains("write.usb_mem_pd().clear_bit()"));
        assert!(usb.contains("fn tx_epoch_current(reset_generation: u32)"));
        assert!(usb.contains("&& !USB_EPOCH_BLOCKED.load(Ordering::Acquire)"));
        let receive = usb
            .split("fn receive_decode_event(")
            .nth(1)
            .and_then(|tail| tail.split("fn handle_pre_authentication_record(").next())
            .expect("USB task must expose one bounded shared-stream receive step");
        let first_rx_epoch_check = receive
            .find("usb_epoch_current(reset_generation)")
            .expect("RX must check its reset generation before touching the FIFO");
        let read = receive
            .find("rx.read_byte()")
            .expect("RX must use one bounded FIFO read");
        let last_rx_epoch_check = receive
            .rfind("usb_epoch_current(reset_generation)")
            .expect("RX must recheck its reset generation after touching the FIFO");
        assert!(first_rx_epoch_check < read);
        assert!(read < last_rx_epoch_check);
        let pre_authentication = usb
            .split("fn handle_pre_authentication_record(")
            .nth(1)
            .and_then(|tail| tail.split("fn handle_reply(").next())
            .expect("USB task must retain independent pre-authentication admission");
        let accept = pre_authentication
            .find(".accept(context.connection, sequence)")
            .expect("RX must retain exact-next sequence admission");
        let last_pre_authentication_epoch_check = pre_authentication
            .rfind("usb_epoch_current(context.reset_generation)")
            .expect("RX must recheck its reset generation after sequence admission");
        assert!(accept < last_pre_authentication_epoch_check);
        let transmission = usb
            .split("fn step_transmission(")
            .nth(1)
            .and_then(|tail| tail.split("fn discard_rx_fifo(").next())
            .expect("USB task must expose one bounded transmission step");
        let first_epoch_check = transmission
            .find("tx_epoch_current(pending.reset_generation)")
            .expect("TX must check its reset generation before touching the FIFO");
        let write = transmission
            .find("tx.write_byte_nb(*byte)")
            .expect("TX must use one nonblocking FIFO write");
        let flush = transmission
            .find("tx.flush_tx_nb()")
            .expect("TX must explicitly transfer hardware ownership");
        let last_epoch_check = transmission
            .rfind("tx_epoch_current(pending.reset_generation)")
            .expect("TX must recheck reset generation after WR_DONE");
        assert!(first_epoch_check < write);
        assert!(write < flush);
        assert!(flush < last_epoch_check);
        assert!(usb.contains("discard_rx_fifo(&mut rx)"));
        assert!(usb.contains("rx.drain_rx_fifo(&mut discarded)"));
        assert!(usb.contains("let _ = tx.flush_tx_nb();"));
        assert!(!usb.contains("flush_tx_nb().is_ok()"));
        for forbidden in [
            "ProductStorageCoordinator",
            "ProductSupervisor",
            "InterfaceFabric",
            "NodeCore",
            "E290Radio",
            "FlashStorage",
        ] {
            assert!(
                !usb.contains(forbidden),
                "USB pairing owner reached forbidden product capability {forbidden}"
            );
        }
    }

    #[test]
    fn wifi_api_proof_replaces_usb_and_stays_outside_the_interface_fabric() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("wifi-api-proof = ["));
        for dependency in [
            "\"dep:edge-dhcp\"",
            "\"dep:edge-nal-embassy\"",
            "\"dep:embassy-net\"",
            "\"dep:esp-radio\"",
            "\"esp-radio/unstable\"",
            "\"esp-radio/wifi\"",
            "\"esp-rtos/esp-radio\"",
        ] {
            assert!(
                manifest.contains(dependency),
                "Wi-Fi proof must retain its opt-in dependency: {dependency}"
            );
        }

        let main = include_str!("main.rs");
        assert!(main.contains("#[cfg(feature = \"wifi-api-proof\")]\nmod wifi_api_task;"));
        assert!(main.contains("SessionBearerBinding::Wifi"));
        assert!(main.contains("SessionSuite::WifiQualification"));
        assert!(main.contains("peripherals.WIFI"));
        assert!(main.contains("AlphaUsbSerialJtagOwner::new(peripherals.USB_DEVICE)"));
        assert!(main.contains("alpha_usb_serial_jtag_owner,"));
        assert!(main.contains("spawner.spawn(usb_pairing_task);"));
        assert!(main.contains("spawner.spawn(wifi_api_task);"));
        assert!(main.contains("local_api_profile=wifi-api-proof usb=serial-jtag-logging"));

        let wifi = include_str!("wifi_api_task.rs");
        assert!(wifi.contains("AccessPointConfig::default()"));
        assert!(wifi.contains(".with_auth_method(AuthenticationMethod::Wpa2Personal)"));
        assert!(wifi.contains(".with_password(profile::SOFTAP_DEVELOPMENT_PASSPHRASE.into())"));
        assert!(wifi.contains("Interface::access_point()"));
        assert!(wifi.contains("WifiController::new("));
        assert!(!wifi.contains("esp_radio::wifi::new("));
        assert!(!wifi.contains("softap_open=true"));
        assert!(wifi.contains("embassy_net::Config::ipv4_static"));
        assert!(wifi.contains("pub async fn run_dhcp"));
        assert!(wifi.contains("TcpSocket::new"));
        assert!(wifi.contains("port: profile::RDA1_TCP_PORT"));
        assert!(wifi.contains("UsbAuthenticatedSession::new(session_parameters)"));
        assert!(wifi.contains("Only one TCP connection is accepted at a time."));
        assert!(!wifi.contains("PacketInterfaceId"));
        assert!(!wifi.contains("register_interface"));
    }

    #[test]
    fn wifi_station_proof_is_ble_managed_and_remains_transport_free() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("wifi-station-proof = ["));
        for dependency in [
            "\"ble-api-proof\"",
            "\"dep:embassy-net\"",
            "\"embassy-net/dhcpv4\"",
            "\"esp-radio/coex\"",
            "\"esp-radio/wifi\"",
        ] {
            assert!(
                manifest.contains(dependency),
                "Wi-Fi station proof must retain its coexistence dependency: {dependency}"
            );
        }

        let main = include_str!("main.rs");
        assert!(main.contains("#[cfg(feature = \"wifi-station-proof\")]\nmod wifi_station_task;"));
        assert!(main.contains("storage_coordinator.wifi_station_boot_plan()"));
        assert!(main.contains("WIFI_STATION_STATUS.init(WifiStationStatusCell::new())"));
        assert!(main.contains("wifi_station_task::start("));
        assert!(
            main.contains("esp_alloc::heap_allocator!(size: config::WIFI_INTERNAL_HEAP_BYTES);")
        );

        let config = include_str!("config.rs");
        assert!(config.contains("pub const WIFI_INTERNAL_HEAP_BYTES: usize = 48 * 1024;"));
        assert!(config.contains("INTERNAL_HEAP_BYTES + WIFI_INTERNAL_HEAP_BYTES, 120 * 1024"));

        let station = include_str!("wifi_station_task.rs");
        assert!(station.contains("WifiConfig::Station("));
        assert!(station.contains("AuthenticationMethod::Wpa2Personal"));
        assert!(station.contains("ControllerConfig::default()"));
        assert!(station.contains(".with_country_info(CountryInfo::from(*b\"US\"))"));
        assert!(station.contains("Interface::station()"));
        assert!(
            station
                .contains("InstrumentedWifiDriver::new(station_interface, &WIFI_DRIVER_METRICS)")
        );
        assert!(station.contains("WifiController::new("));
        assert!(!station.contains("esp_radio::wifi::new("));
        assert!(station.contains("controller.set_max_tx_power(WIFI_MAX_TX_POWER_QUARTER_DBM)"));
        assert!(station.contains("embassy_net::Config::dhcpv4"));
        assert!(station.contains("join("));
        assert!(station.contains("controller.connect_async()"));
        assert!(station.contains("controller.wait_for_disconnect_async()"));
        for forbidden in [
            "ProductStorageCoordinator",
            "FlashStorage",
            "DeviceApiHandoff",
            "PacketInterfaceId",
            "InterfaceFabric",
            "register_interface",
            "TcpSocket",
        ] {
            assert!(
                !station.contains(forbidden),
                "station coexistence proof reached deferred capability {forbidden}"
            );
        }
    }

    #[test]
    fn wifi_driver_metrics_wrap_the_public_boundary_without_frame_logging() {
        let metrics = include_str!("wifi_driver_metrics.rs");
        for required in [
            "impl<D: Driver> Driver for InstrumentedWifiDriver<D>",
            "fn receive(",
            "fn transmit(",
            "fn record_tx_frame(",
            "fn record_rx_frame(",
            "const ETHERTYPE_ARP: u16 = 0x0806;",
            "const ETHERTYPE_IPV4: u16 = 0x0800;",
            "pub static WIFI_DRIVER_METRICS",
            "wrapping_delta_since",
        ] {
            assert!(
                metrics.contains(required),
                "Wi-Fi boundary metrics lost required accounting: {required}"
            );
        }
        for forbidden in ["log::", "println!", "dump_packet", "frame={:?}"] {
            assert!(
                !metrics.contains(forbidden),
                "driver boundary must not log per-frame data: {forbidden}"
            );
        }

        let tcp = include_str!("wifi_tcp_task.rs");
        assert!(tcp.contains("let attempt_metrics_started = WIFI_DRIVER_METRICS.snapshot();"));
        assert!(tcp.contains("\"raw-dns-attempt\""));
        assert!(tcp.contains("\"raw-dns-cycle\""));
        assert!(tcp.contains(
            "log_driver_metrics_delta(\"raw-dns-cycle\", \"exhausted\", cycle_metrics_started)"
        ));
    }

    #[test]
    fn wifi_tcp_border_profile_owns_exact_interface_two_outside_station_and_management() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("wifi-tcp-proof = ["));
        assert!(manifest.contains("\"wifi-station-proof\""));
        assert!(manifest.contains("\"dep:rete-core\""));

        let config = include_str!("config.rs");
        assert!(config.contains(
            "#[cfg(feature = \"wifi-tcp-proof\")]\npub const INTERFACE_SLOTS: usize = 2;"
        ));
        assert!(config.contains("pub const fn tcp_interface_properties()"));

        let main = include_str!("main.rs");
        assert!(main.contains("#[cfg(feature = \"wifi-tcp-proof\")]\nmod wifi_tcp_task;"));
        assert!(main.contains("LoRaAndTcpAuthorizationPolicy::new(lora_policy)"));
        let tcp_registration = main
            .find("let tcp_offline_descriptor = match supervisor.register_interface(")
            .expect("the TCP profile must register exactly one second interface");
        assert!(main[tcp_registration..].contains("tcp_actor.queue_id(),"));
        assert!(main.contains("wifi_tcp_task::WifiTcpActorPorts::new("));
        assert!(main.contains("wifi_tcp_task::run("));
        assert!(main.contains("if let Some(wifi_tcp_task) = wifi_tcp_task"));

        let tcp = include_str!("wifi_tcp_task.rs");
        for required in [
            "TcpSocket::new",
            "HdlcDecoder",
            "hdlc::encode",
            "InterfaceLifecycleState::Ready",
            "InterfaceLifecycleState::Offline",
            "InterfaceTxActorHandoff",
            "InterfaceIngressActorHandoff",
            "tcp_stream_requirements",
            "while !tcp_network_ready(stack.is_link_up(), stack.is_config_up())",
        ] {
            assert!(
                tcp.contains(required),
                "TCP actor lost required interface behavior: {required}"
            );
        }
        assert!(
            tcp.matches("stack.wait_link_down()").count() >= 4,
            "TCP eligibility must end on physical link loss while idle, receiving, or writing"
        );
        let write = tcp
            .split("async fn write_frame(")
            .nth(1)
            .and_then(|tail| tail.split("async fn abort_socket(").next())
            .expect("TCP frame writer must remain explicit");
        assert!(write.contains("stack.wait_config_down()"));
        assert!(write.contains("stack.wait_link_down()"));
        assert_eq!(
            tcp.matches("owner.complete_transmitted(COMPLETION_TRANSMITTED)")
                .count(),
            1,
            "a successful ordinary TCP write must retain physical-transmit evidence"
        );
        assert_eq!(
            tcp.matches("owner.complete_transmitted(COMPLETION_TRANSMITTED, now())")
                .count(),
            1,
            "a successful TCP DATA write must timestamp its transmission evidence"
        );
        assert!(
            !tcp.contains("owner.complete(COMPLETION_TRANSMITTED)"),
            "a successful TCP write must not erase the physical-transmit distinction"
        );
        for forbidden in [
            "ProductStorageCoordinator",
            "FlashStorage",
            "DeviceApiHandoff",
            "WifiController",
            "SoleRadioTxDispatcher",
        ] {
            assert!(
                !tcp.contains(forbidden),
                "TCP packet actor crossed an independent owner boundary: {forbidden}"
            );
        }
    }

    #[test]
    fn ble_api_proof_is_one_confirmed_stream_bearer_outside_the_interface_fabric() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("ble-api-proof = ["));
        for dependency in [
            "\"dep:esp-radio\"",
            "\"dep:trouble-host\"",
            "\"esp-radio/ble\"",
            "\"esp-radio/unstable\"",
            "\"esp-rtos/esp-radio\"",
            "trouble-host = { git = \"https://github.com/embassy-rs/trouble.git\"",
            "esp-radio = { workspace = true",
        ] {
            assert!(
                manifest.contains(dependency),
                "BLE proof must retain its pinned opt-in dependency: {dependency}"
            );
        }
        let esp_radio_dependency = manifest
            .split("esp-radio = { workspace = true")
            .nth(1)
            .and_then(|tail| tail.split("esp-storage =").next())
            .expect("the target graph must expose one bounded esp-radio dependency");
        assert!(!esp_radio_dependency.contains("\"wifi\""));
        assert!(!esp_radio_dependency.contains("\"ble\""));

        let workspace_manifest = include_str!("../../../Cargo.toml");
        assert!(workspace_manifest.contains(
            "esp-radio = { git = \"https://github.com/esp-rs/esp-hal.git\", rev = \
             \"b50efcb0dcd94b58ec337e511891057aa1f2e8fb\", version = \
             \"=1.0.0-beta.0\""
        ));

        let main = include_str!("main.rs");
        assert!(main.contains("#[cfg(feature = \"ble-api-proof\")]\nmod ble_api_task;"));
        assert!(
            main.contains(
                "ble-api-proof and wifi-api-proof are mutually exclusive local API bearers"
            )
        );
        assert!(main.contains("SessionBearerBinding::BleGatt"));
        assert!(main.contains("SessionSuite::BleGattQualification"));
        assert!(main.contains("peripherals.BT"));
        assert!(main.contains("spawner.spawn(ble_api_task);"));
        assert!(main.contains("local_api_profile=ble-api-proof"));
        assert!(main.contains("usb=serial-jtag-logging"));
        assert!(main.contains("task=ble-api lora_routing=continue"));
        let ble_task = main
            .find("Some(connector) => match ble_api_task::run(")
            .expect("the product composition must construct one BLE task");
        assert!(
            main[ble_task..]
                .contains("BleAdvertisingParameters::new(identity_hash, base_mac_eui48),"),
            "BLE identity must rotate with durable node reprovisioning while its human name remains MAC-suffixed"
        );

        let ble = include_str!("ble_api_task.rs");
        let button_sample = ble
            .find("// Sample once per owner iteration")
            .expect("BLE GPIO21 sampling must remain explicit");
        let connection_wait = ble[button_sample..]
            .find("match select(")
            .map(|offset| button_sample + offset)
            .expect("BLE owner must retain its bounded connection/timer wait");
        assert!(
            button_sample < connection_wait,
            "GPIO21 must be sampled before GATT event selection can restart the idle timer"
        );
        assert!(ble.contains("static BLE_RESOURCES: StaticCell<"));
        assert!(ble.contains("HostResources<DefaultPacketPool"));
        assert!(ble.contains("#[gatt_service(uuid = gatt_profile::SERVICE_UUID_U128)]"));
        assert!(ble.contains("permissions(write = authenticated)"));
        assert!(ble.contains("#[characteristic(uuid = gatt_profile::TX_UUID_U128, indicate)]"));
        assert!(ble.contains("let mut table = AttributeTable::new();"));
        assert!(ble.contains("table.add_service(Service::new(service::GATT)).build();"));
        assert!(ble.contains("DEPLOYED_RX_HANDLE: u16 = 9"));
        assert!(ble.contains("DEPLOYED_TX_HANDLE: u16 = 11"));
        assert!(ble.contains("DEPLOYED_TX_CCCD_HANDLE: u16 = 12"));
        assert!(ble.contains("DEPLOYED_SECURITY_CONFIRMATION_HANDLE: u16 = 14"));
        assert!(ble.contains("reason=gatt-database-layout"));
        assert!(ble.contains("Some(PermissionLevel::AuthenticationRequired)"));
        assert!(ble.contains("Some(PermissionLevel::Allowed)"));
        assert!(ble.contains("status=GATT-NOT-ALLOWED requested_handle={}"));
        assert!(ble.contains("status=APPLICATION-SESSION-READY"));
        assert!(
            !ble.contains("Server::new_with_config(GapConfig::Peripheral"),
            "the compile-only Trouble central feature must not shift the deployed peripheral GATT handles"
        );
        assert!(
            !ble.contains("permissions(read = authenticated)"),
            "polling WAIT must not initiate SMP before firmware observes GPIO21"
        );
        assert!(!ble.contains("write_without_response"));
        assert!(ble.contains("StreamDecoder::new()"));
        assert!(ble.contains("UsbAuthenticatedSession::new(session_parameters)"));
        assert!(ble.contains("AdStructure::CompleteServiceUuids128(&SERVICE_UUIDS)"));
        assert!(ble.contains(".with_max_connections(profile::CONTROLLER_ACTIVITY_MAX as u8)"));
        assert!(ble.contains("it counts the advertiser and ACL link as distinct"));
        assert!(ble.contains("Address::random(advertising.static_random_address())"));
        assert!(ble.contains("gatt_profile::local_name(advertising.local_name_mac())"));
        let ble_profile = include_str!("ble_api_profile.rs");
        assert!(ble_profile.contains("pub const CONNECTIONS_MAX: usize ="));
        assert!(ble_profile.contains("pub const CONTROLLER_ACTIVITY_MAX: usize = 2;"));
        assert!(ble_profile.contains("CONTROLLER_ACTIVITY_MAX == CONNECTIONS_MAX + 1"));
        assert!(ble.contains(".with_scan(false)"));
        assert!(ble.contains("CCCD_SUBSCRIBE_TIMEOUT_MS"));
        assert!(ble.contains("PreAuthenticationDeadline::new()"));
        assert!(ble_profile.contains("PRE_AUTHENTICATION_TIMEOUT_MS"));
        assert!(ble.contains("BLE_SECURITY_PAIRING_TIMEOUT_MS"));
        assert!(ble.contains("reason=pre-authentication-timeout"));
        assert!(ble.contains("struct ServeConnectionOutcome"));
        assert!(ble.contains("disconnected_event_observed = true;"));
        assert!(ble.contains("async fn drain_connection<P: PacketPool>"));
        assert!(ble.contains("raw.is_connected()"));
        assert!(ble.contains("raw.disconnect();"));
        assert!(ble.contains("raw.next()"));
        assert!(ble.contains("DISCONNECT_DRAIN_RECHECK_INTERVAL_MS"));
        assert!(ble.contains("DISCONNECT_DRAIN_PROLONGED_LOG_MS"));
        assert!(ble.contains("completion=awaiting-disconnected-event"));
        assert!(!ble.contains("DISCONNECT-DRAIN-DEADLINE"));
        assert!(ble.contains("authoritative_bond_identity"));
        assert!(ble.contains("matches_authoritative_identity("));
        assert!(ble.contains("fresh_security_pending_durability = true;"));
        assert!(ble.contains("fresh_security_pending_durability = false;"));
        assert!(ble.contains("purge_non_authoritative_bonds"));
        assert!(ble.contains("status=PAIRING-RETRY-READY"));
        assert!(ble.contains("reason=unexpected-fresh-pairing-complete"));
        assert!(ble.contains("FreshSecurityDisposition::classify("));
        assert!(ble_profile.contains("TimedOutBeforeSend,"));
        assert!(ble.contains("BleBondExchangeResult::timed_out(pending.is_none())"));
        assert!(ble.contains("outcome.bond_reboot_required()"));
        assert!(ble.contains("fresh_security_disposition.scrub_non_authoritative_bonds()"));
        assert!(ble.contains("fresh_security_disposition.disable_until_reboot()"));
        assert!(ble.contains("PairingDisplayState::Pending(_) | PairingDisplayState::Rendered"));
        assert!(ble.contains("status=PAIRING-EXCLUSIVE-PENDING"));
        assert!(ble.contains("PairingExclusiveCloseDisposition::DrainBeforeClose"));
        assert!(ble.contains("request.accept(None, stack).await"));
        assert!(ble.contains("HANDOFF_EXCHANGE_TIMEOUT_MS"));
        assert!(ble.contains("pairing_transmission.is_some()"));
        assert!(ble.contains("ButtonObservationFlight::new()"));
        assert!(ble.contains("button_observation.poll(pairing_handoff)"));
        assert!(ble.contains("button_observation.try_schedule_and_poll("));
        assert!(
            !ble.contains("PairingControlCommand::ObserveButton"),
            "BLE GPIO sampling must not await a pairing-control round trip"
        );

        let restored_start = ble
            .find("security=restored-authenticated-bond")
            .and_then(|log| ble[..log].rfind("if raw"))
            .expect("restored authenticated bonds must own an explicit pairing-exclusive path");
        let restored_end = ble[restored_start..]
            .find("continue;")
            .map(|offset| restored_start + offset)
            .expect("restored authenticated pairing entry must end before fresh SMP");
        let restored = &ble[restored_start..restored_end];
        let home_display = restored
            .find("DisplayCommand::ShowHome")
            .expect("restored pairing must replace any stale terminal display");
        let ready_confirmation = restored
            .find("gatt_profile::SECURITY_CONFIRMATION_READY_VALUE")
            .expect("restored pairing must publish authenticated readiness");
        assert!(
            home_display < ready_confirmation,
            "the stale display must be replaced before the client observes RDY1"
        );
        assert!(restored.contains("restored-pairing-display-home-fault"));

        let pairing_response_start = ble
            .find("async fn handle_pairing_record")
            .expect("the bearer must own one pairing response encoder");
        let pairing_response_end = ble[pairing_response_start..]
            .find("const fn matches_control_response")
            .map(|offset| pairing_response_start + offset)
            .expect("the pairing encoder must precede response matching helpers");
        let pairing_response = &ble[pairing_response_start..pairing_response_end];
        let durable_activation = pairing_response
            .find("ActivateResult::Activated")
            .expect("the encoded response must classify durable activation");
        let response_encoded = pairing_response
            .find("FramedRecord::encode(&record)")
            .expect("the durable result must be framed for the client");
        assert!(durable_activation < response_encoded);

        let retain_helper_start = ble
            .find("fn retain_activated_application_home")
            .expect("durable activation must update cached Home authority");
        let retain_helper_end = ble[retain_helper_start..]
            .find("fn pairing_time")
            .map(|offset| retain_helper_start + offset)
            .expect("the cached Home helper must remain bounded");
        let retain_helper = &ble[retain_helper_start..retain_helper_end];
        assert!(retain_helper.contains("transmission.activation_succeeded"));
        assert!(retain_helper.contains(".with_setup(DisplaySetupState::Paired)"));
        assert!(retain_helper.contains("*display_home_after_pairing = true"));

        let retain_calls: Vec<_> = ble
            .match_indices("retain_activated_application_home(")
            .map(|(offset, _)| offset)
            .filter(|offset| *offset < retain_helper_start)
            .collect();
        assert_eq!(
            retain_calls.len(),
            2,
            "both accepted pairing-record paths must retain durable setup truth"
        );
        for retain_call in retain_calls {
            let pairing_record = ble[..retain_call]
                .rfind("handle_pairing_record(")
                .expect("Home retention must directly follow an accepted pairing response");
            let transmission_owner = ble[retain_call..]
                .find("pairing_transmission = Some(transmission);")
                .map(|offset| retain_call + offset)
                .expect("the encoded pairing response must retain its transmission owner");
            assert!(pairing_record < retain_call);
            assert!(
                retain_call < transmission_owner,
                "durable Home truth must precede fallible ATT response delivery"
            );
        }

        let confirmation_start = ble
            .find("if transmission.frame.is_complete()")
            .expect("normal connection close must wait for complete confirmed delivery");
        let confirmation_end = ble[confirmation_start..]
            .find("break 'connection;")
            .map(|offset| confirmation_start + offset)
            .expect("confirmed successful activation must close the onboarding connection");
        let confirmation = &ble[confirmation_start..confirmation_end];
        assert!(confirmation.contains("transmission.activation_succeeded"));
        assert!(!confirmation.contains("with_setup(DisplaySetupState::Paired)"));
        assert!(!confirmation.contains("display_home_after_pairing = true"));
        let terminal_start = ble
            .rfind("if display_home_after_pairing || display_home_after_bond_recovery {")
            .expect("connection cleanup must own the successful Home transition");
        let terminal_end = ble[terminal_start..]
            .find("let _ = display_state;")
            .map(|offset| terminal_start + offset)
            .expect("terminal display transition must precede outcome construction");
        let terminal = &ble[terminal_start..terminal_end];
        assert!(
            terminal.contains("DisplayCommand::ShowHome"),
            "successful application pairing must publish the paired Home snapshot"
        );
        assert!(
            terminal.contains("DisplayCommand::ClearPairingSecret"),
            "failed and timed-out pairing must retain their terminal display path"
        );
        assert!(
            terminal.contains("display_clear_reason.or_else"),
            "an explicit failure or timeout must render terminal even without a fresh passkey"
        );
        assert!(!ble.contains("PairingSecretClearReason::Succeeded"));

        let drain = ble
            .find("drain_connection(&gatt_connection")
            .expect("the old link must be drained before another advertiser");
        let release = ble
            .find("drop(gatt_connection);")
            .expect("the final Trouble connection reference must be released explicitly");
        let advertise = ble
            .find("let gatt_connection = match advertise(")
            .expect("the bearer must own one advertiser entrypoint");
        assert!(advertise < drain);
        assert!(drain < release);
        assert_eq!(ble.matches("drop(gatt_connection);").count(), 1);

        let cccd = ble
            .find("write.handle() == tx_cccd")
            .expect("the bearer must observe the indication CCCD");
        let begin = ble
            .find("session.begin_connection(connection_id)")
            .expect("the bearer must explicitly begin one authenticated connection");
        assert!(begin < cccd);
        let arm = ble
            .find("indication.arm(fragment.len())")
            .expect("the bearer must retain the exact indicated value before sending");
        let indicate = ble
            .find("tx.indicate(connection, &fragment, true)")
            .expect("the bearer must send indications");
        let confirmation = ble
            .find("indication.confirm()")
            .expect("the bearer must retain Trouble's confirmed indication result");
        let advance = ble
            .find("session.advance_tx(acknowledged)")
            .expect("the bearer must advance only a confirmed fragment");
        assert!(arm < indicate);
        assert!(indicate < confirmation);
        assert!(confirmation < advance);
        assert_eq!(ble.matches("session.advance_tx(").count(), 1);
        assert!(ble.contains("profile::negotiated_att_value_bytes(raw.att_mtu())"));
        assert!(ble.contains("data.len() <= maximum_fragment_bytes"));
        assert!(ble.contains("INDICATION_CONFIRM_TIMEOUT_MS"));
        assert!(!ble.contains("PacketInterfaceId"));
        assert!(!ble.contains("register_interface"));
    }

    #[test]
    fn minimal_usb_session_uses_node_owned_admission_and_authenticated_dispatch() {
        let main = include_str!("main.rs");
        assert!(main.contains("DeviceApiHandoff<CriticalSectionRawMutex, AuthenticatedGrant>"));
        assert!(main.contains("AUTHENTICATED_API.init(DeviceApiHandoff::new()).split()"));
        assert!(main.contains("SESSION_ADMISSION"));
        assert!(main.contains("SessionAdmissionHandoff::new()"));
        assert!(main.contains("node_authenticated_api,"));
        assert!(main.contains("usb_authenticated_api,"));
        assert!(main.contains(
            "authenticated_local_api=node-dispatch bearer_session=usb-authenticated-single-flight"
        ));

        let node = include_str!("node_task.rs");
        assert!(node.contains("enum AuthenticatedApiNodeState"));
        assert!(node.contains("*supervisor.destination_hash().as_bytes()"));
        assert!(node.contains("let authenticated_api_progressed = step_authenticated_api("));
        assert!(node.contains("progressed |= authenticated_api_progressed;"));
        assert!(node.contains("storage.dispatch_authenticated_request("));
        assert!(node.contains("ProductNomadFetchPort::new("));
        assert!(node.contains("&mut nomad_port,"));
        assert!(node.contains("peer_discovery_incarnation,"));
        assert!(node.contains("discovered_peers,"));
        assert!(node.contains("AuthenticatedApiNodeState::PendingReply(pressure.into_inner())"));
        assert!(node.contains("AuthenticatedApiNodeState::Quarantined {"));
        assert!(node.contains("request: failure.into_request()"));

        let storage = include_str!("platform_storage.rs");
        assert!(storage.contains("struct ProductSubmissionPort<'a>"));
        assert!(storage.contains("struct ProductAuthenticatedApiPort<'a>"));
        assert!(storage.contains(".select_ordinary_session(at, connection, credential_id)"));
        assert!(storage.contains("credential_runtime.dispatch_authenticated_request_with_probe("));
        let storage_without_whitespace =
            storage.split_whitespace().collect::<std::string::String>();
        assert!(storage_without_whitespace.contains(
            "credential_runtime.dispatch_authenticated_request_with_probe(request,identity,&mutport,nomad_port,probe_port,)"
        ));
        assert!(storage.contains("impl SubmissionPort for ProductAuthenticatedApiPort<'_>"));
        assert!(storage.contains("impl InboundMailboxPort for ProductAuthenticatedApiPort<'_>"));
        assert!(storage.contains("impl LxmfInboxPort for ProductAuthenticatedApiPort<'_>"));
        assert!(storage.contains("impl LxmfComposePort for ProductAuthenticatedApiPort<'_>"));
        assert!(storage.contains("impl PeerDiscoveryPort for ProductAuthenticatedApiPort<'_>"));
        assert!(storage.contains("impl NetworkConfigPort for ProductAuthenticatedApiPort<'_>"));
        assert!(storage.contains("network_config_radio_applied_revision"));
        assert!(storage.contains("network_config_last_mutation"));
        assert!(storage.contains("journal_actor_mutation_pending"));
        assert!(storage.contains("journal_projector_mutation_pending"));
        assert!(storage.contains("provision_network_config_erased("));
        assert!(storage.contains("commit_network_config_successor("));
        assert!(storage.contains("mount_network_config_store(&mut access)"));
        assert!(storage.contains("NetworkConfigStoreFault::UnformattedErased"));
        assert!(storage.contains("request.destination()"));
        assert!(storage.contains("request.authorization()"));
        assert!(storage.contains(".prepare_basic_direct_lxmf_into("));
        assert!(storage.contains("LxmfMessageIntent::new("));

        let authenticated = include_str!("authenticated_api_node.rs");
        assert!(authenticated.contains("dispatch_with_inbox_lxmf_peer_discovery_and_nomad("));
        assert!(
            authenticated
                .contains("dispatch_with_inbox_lxmf_peer_discovery_nomad_and_network_config(")
        );
        assert!(authenticated.contains("N: NomadFetchPort"));

        let usb = include_str!("usb_pairing_task.rs");
        assert!(usb.contains(
            "authenticated_api: BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>"
        ));
        assert!(usb.contains("UsbAuthenticatedSession::new(session_parameters)"));
        assert!(usb.contains("authenticated_session.try_send_admission_command("));
        assert!(usb.contains("authenticated_session.try_send_request("));
        assert!(usb.contains("authenticated_session.accept_node_reply(reply)"));
        assert!(usb.contains("fn try_in_epoch<R>("));
        let admission_reply = usb
            .split("while let Some(reply) = session_admission.try_receive_reply()")
            .nth(1)
            .and_then(|tail| {
                tail.split("while let Some(reply) = authenticated_api.replies().try_receive()")
                    .next()
            })
            .expect("admission replies must have one reset-linearized acceptance step");
        let admission_guard = admission_reply
            .find("critical_section::with(|_| {")
            .expect("admission acceptance must exclude the reset ISR");
        let admission_accept = admission_reply
            .find("authenticated_session.accept_admission_reply(")
            .expect("admission acceptance must retain its exact reply");
        let admission_arm = admission_reply
            .find("synchronize_tx_epoch(&transmission, &authenticated_session)")
            .expect("admission acceptance must publish its TX owner before the ISR resumes");
        assert!(admission_guard < admission_accept);
        assert!(admission_accept < admission_arm);
        let api_reply = usb
            .split("while let Some(reply) = authenticated_api.replies().try_receive()")
            .nth(1)
            .and_then(|tail| tail.split("if transmission.is_some()").next())
            .expect("API replies must have one reset-linearized acceptance step");
        let api_guard = api_reply
            .find("critical_section::with(|_| {")
            .expect("API reply acceptance must exclude the reset ISR");
        let api_accept = api_reply
            .find("authenticated_session.accept_node_reply(reply)")
            .expect("API reply acceptance must retain its exact reply");
        let api_arm = api_reply
            .find("synchronize_tx_epoch(&transmission, &authenticated_session)")
            .expect("API reply acceptance must publish its TX owner before the ISR resumes");
        assert!(api_guard < api_accept);
        assert!(api_accept < api_arm);
        assert!(!usb.contains("ServerSession"));
        assert!(!usb.contains("ServerHelloFlight::begin"));
        assert!(!usb.contains("SessionEpochAllocator"));
        let session = include_str!("usb_authenticated_session.rs");
        assert!(session.contains("ServerSession"));
        assert!(session.contains("ServerHelloFlight"));
        assert!(session.contains("SessionEpochAllocator"));
        assert!(session.contains("one handshake attempt per accepted connection"));
        assert!(session.contains("one authenticated request"));
        assert_eq!(config::NODE_FAIR_LANES, 8);
    }

    #[test]
    fn transient_flash_ownership_does_not_hide_the_lxmf_inbox_capability() {
        let storage = include_str!("platform_storage.rs");
        let inbox = storage
            .split("impl LxmfInboxPort for ProductAuthenticatedApiPort<'_> {")
            .nth(1)
            .and_then(|tail| tail.split("impl LxmfComposePort for").next())
            .expect("the product LXMF inbox port must remain explicit");
        let availability = inbox
            .split("fn availability(&mut self) -> CapabilityAvailability {")
            .nth(1)
            .and_then(|tail| tail.split("fn next(").next())
            .expect("LXMF availability must precede inbox operations");
        assert!(availability.contains("CapabilityAvailability::Available"));
        assert!(!availability.contains("has_pending_mutation"));
        assert!(!availability.contains("CapabilityAvailability::Disabled"));

        let read = inbox
            .split("fn read(")
            .nth(1)
            .and_then(|tail| tail.split("fn mailbox_status(").next())
            .expect("LXMF wire reads must remain explicit");
        for retained_owner in [
            "credential_physical_mutation_outstanding",
            "journal_actor_mutation_pending",
            "journal_projector_mutation_pending",
            "lxmf_mutation_pending",
        ] {
            assert!(read.contains(retained_owner));
        }
        assert!(read.contains("return Err(LxmfInboxPortError::Busy);"));
    }

    #[test]
    fn proof_probe_responder_keeps_prove_all_delivery_on_the_immediate_action_path() {
        let source = include_str!("node_task.rs");
        let responder = source
            .split("if destination == proof_probe_destination.as_bytes()")
            .nth(1)
            .and_then(|tail| tail.split("ApplicationEvent::DataReceived {").next())
            .expect("the proof-probe responder branch must remain explicit");
        assert!(responder.contains("match lease.acknowledge()"));
        assert!(!responder.contains("try_reserve_delayed"));
        assert!(responder.contains(
            "proof_policy=prove-all proof_delivery=immediate-action durable-inbox=not-used"
        ));
    }

    #[test]
    fn protocol_timer_maintenance_keeps_a_fair_turn_under_action_pressure() {
        let source = include_str!("node_task.rs");
        let tick_lane = source
            .split("config::ActionRetrySlot::None =>")
            .nth(1)
            .and_then(|tail| tail.split("\n                3 => {").next())
            .expect("the protocol timer lane must remain explicit");
        assert!(tick_lane.contains("if now >= next_tick_seconds"));
        assert!(!tick_lane.contains("pending_protocol_dispatch.is_none()"));
        assert!(!tick_lane.contains("!supervisor.ingress_actions_pending()"));
        assert!(tick_lane.contains("handle_action_offer_failure"));
        assert!(tick_lane.contains("retry_actions_a = Some(retained)"));
    }

    #[test]
    fn routine_application_event_handoff_stays_below_production_info_level() {
        let source = include_str!("node_task.rs");
        let drained = source
            .split("NodeInterfaceApplicationEventDrain::Drained(report) => {")
            .nth(1)
            .and_then(|tail| {
                tail.split("NodeInterfaceApplicationEventDrain::Retained(reason)")
                    .next()
            })
            .expect("the routine application-event drain branch must remain explicit");
        assert!(drained.contains("debug!("));
        assert!(!drained.contains("info!("));
        assert!(drained.contains("status=ADMITTED"));

        let retained = source
            .split("NodeInterfaceApplicationEventDrain::Retained(reason) => {")
            .nth(1)
            .and_then(|tail| tail.split("let ordinary_lane_available").next())
            .expect("the terminal application-event drain branch must remain explicit");
        assert!(retained.contains("error!("));
        assert!(retained.contains("status=QUARANTINED"));
    }

    #[test]
    fn node_owns_causal_pairing_frontier_before_other_flash_mutation_work() {
        let source = include_str!("node_task.rs");
        let pairing = source
            .find("progressed |= step_pairing_frontier(")
            .expect("node loop must step the causal pairing frontier");
        let frame = source
            .find("let Some(observation) = pending_frame_acknowledgement.take()")
            .expect("node loop must retain its authorized-frame owner");
        let journal = source
            .find("let submission_step = storage.drive_submission_step(")
            .expect("node loop must retain its journal drive");
        assert!(pairing < frame);
        assert!(pairing < journal);
        assert!(source.contains("CredentialInitializationStatus::InFlight"));
        assert!(source.contains("drive_initialization_and_schedule"));
        assert!(source.contains("select_pairing_command_lane"));
        assert!(source.contains("admit_live_pairing_command"));
        assert!(source.contains("drive_live_pairing_and_schedule"));
        let live_admission = source
            .find("progressed |= admit_live_pairing_command")
            .expect("live request admission must be explicit");
        let later_control = source
            .find("handle_pairing_control_command(storage, state, command, now_millis)")
            .expect("scalar control handling must be explicit");
        let timeout = source
            .find("progressed |= poll_pairing_timeout")
            .expect("one timeout poll must follow both command lanes");
        assert!(live_admission < later_control);
        assert!(later_control < timeout);
    }

    #[test]
    fn protocol_dispatch_invariant_failure_offlines_the_routed_interface_before_drain() {
        let source = include_str!("node_task.rs");
        let routed = source
            .find("enter_protocol_dispatch_fail_stop(")
            .expect("protocol dispatch rejection must enter the interface fail-stop");
        let disposition = source
            .find("ProtocolDispatchConfirmationDisposition::TerminalFailClosedDrain")
            .expect("protocol dispatch rejection must retain its terminal policy");
        assert!(disposition < routed);

        let helper = source
            .split("fn enter_protocol_dispatch_fail_stop(")
            .nth(1)
            .and_then(|tail| tail.split("fn step_ingress(").next())
            .expect("protocol dispatch fail-stop helper must remain explicit");
        let offline = helper
            .find("supervisor.disable_interface(routed_descriptor.lease())")
            .expect("protocol dispatch fail-stop must offline the routed transport");
        let drain = helper
            .rfind("*fail_closed_draining = true;")
            .expect("protocol dispatch fail-stop must retain exact-owner drainage");
        assert!(offline < drain);
    }

    #[test]
    fn path_discovery_clocks_only_confirmed_transmission_and_retries_final_nontransmission() {
        let source = include_str!("node_task.rs");
        let actor_queued = source
            .split("status=ACTOR-QUEUED")
            .nth(1)
            .and_then(|tail| tail.split("OrdinaryRouterStep::RouteRejected").next())
            .expect("router acceptance must retain a pre-transmission state");
        assert!(actor_queued.contains("response_clock=not-started"));
        assert!(!actor_queued.contains("acknowledge_path_request_dispatched"));

        let rejected = source
            .split("OrdinaryRouterStep::RouteRejected { slot, reason }")
            .nth(1)
            .and_then(|tail| {
                tail.split("OrdinaryRouterStep::Completion(completion)")
                    .next()
            })
            .expect("one rejected interface hop must remain nonterminal");
        assert!(rejected.contains("status=HOP-REJECTED"));
        assert!(rejected.contains("await-next-route-or-terminal-return"));
        assert!(!rejected.contains("disable_submission_for_path_fault("));

        let completion = source
            .split("OrdinaryRouterStep::Completion(completion)")
            .nth(1)
            .and_then(|tail| {
                tail.split("if let NodeInterfaceSupervisorTransition::Ordinary(\n                        OrdinaryRouterStep::Completion(")
                    .next()
            })
            .expect("path completion must have an explicit transmission policy");
        assert!(completion.contains("OrdinaryTransmissionOutcome::Transmitted"));
        assert!(completion.contains("acknowledge_path_request_dispatched(offer, completed_at)"));
        assert!(completion.contains("response_clock=started"));
        assert!(completion.contains("storage.retry_path_request_dispatch("));
        assert!(completion.contains("response_clock=not-started"));
        assert!(completion.contains("status=HOP-NOT-TRANSMITTED"));
    }

    #[test]
    fn active_owner_durability_failure_uses_node_owned_disable_policy() {
        let source = include_str!("node_task.rs");
        let helper = source
            .split("fn enter_active_owner_durability_fail_stop(")
            .nth(1)
            .and_then(|tail| tail.split("fn enter_protocol_dispatch_fail_stop(").next())
            .expect("active-owner durability fail-stop helper must remain explicit");
        let offline = helper
            .find("supervisor.disable_interface(online.lease())")
            .expect("node policy must offline the affected registry lease");
        let drain = helper
            .rfind("*fail_closed_draining = true;")
            .expect("node policy must retain exact-owner drainage");
        assert!(offline < drain);
        assert!(!helper.contains("InterfaceLifecycleState"));
    }

    #[test]
    fn lora_ingress_signal_reaches_transport_neutral_peer_observation() {
        let radio = include_str!("radio_task.rs");
        assert!(radio.contains("packet_len, signal, .."));
        assert!(radio.contains("buffer.seal_with_signal("));
        assert!(radio.contains("IngressSignalObservation::new("));
        assert!(radio.contains("signal.rssi_dbm"));
        assert!(radio.contains("signal.snr_db"));

        let tcp = include_str!("wifi_tcp_task.rs");
        assert!(tcp.contains("buffer.seal(frame.len())"));
        assert!(!tcp.contains("seal_with_signal"));

        let node = include_str!("node_task.rs");
        let announce = node
            .split("ApplicationEvent::AnnounceReceived {")
            .nth(1)
            .and_then(|tail| tail.split("ApplicationEvent::Tick {").next())
            .expect("announce peer discovery must remain explicit");
        assert!(announce.contains("let Some(ingress) = *ingress"));
        assert!(announce.contains("let observed_interface = ingress.interface().0;"));
        assert!(announce.contains("let signal = ingress.signal().map_or("));
        assert!(announce.contains("SignalObservations::UNKNOWN"));
        assert!(announce.contains("Some(signal.rssi_dbm())"));
        assert!(announce.contains("Some(signal.snr_db())"));
        assert!(announce.contains("ObservedInterfaceId::new(observed_interface)"));
        assert!(!announce.contains("ObservedInterfaceId::new(LORA_INTERFACE.get())"));
    }

    #[test]
    fn concrete_lora_actor_owns_generation_bound_ready_and_offline_handshakes() {
        let main = include_str!("main.rs");
        assert!(!main.contains("RADIO_READY"));
        assert!(!main.contains("LORA_ONLINE"));
        assert!(main.contains("let (tx_interface, ingress, lifecycle) = interface.into_parts();"));
        assert!(main.contains("radio_task::run("));
        assert!(main.contains("radio_diagnostics,"));

        let node = include_str!("node_task.rs");
        assert!(node.contains("NodeInterfaceSupervisorTransition::Lifecycle(lifecycle)"));
        assert!(!node.contains("set_interface_online"));

        let radio = include_str!("radio_task.rs");
        let ready = radio
            .find("InterfaceLifecycleState::Ready")
            .expect("actor startup must request Ready");
        let actor_loop = radio
            .find("let mut previous_radio_loop_us")
            .expect("actor loop must retain its measurement frontier");
        assert!(ready < actor_loop);

        let helper = radio
            .split("async fn fail_stop(")
            .nth(1)
            .expect("actor fail-stop helper must remain explicit");
        let offline = helper
            .find("InterfaceLifecycleState::Offline")
            .expect("actor fail-stop must request Offline");
        let resume = helper
            .find("lifecycle.finish_pending_request().await")
            .expect("actor fail-stop must resume a retained lifecycle exchange");
        let retry = helper
            .find("action=retry-offline-no-further-radio-operations")
            .expect("actor fail-stop must retry a rejected Offline exchange");
        let acknowledged = helper
            .find("status=FAIL-STOPPED lifecycle=OFFLINE")
            .expect("actor fail-stop must observe authoritative Offline state");
        let retention = helper
            .rfind("loop {")
            .expect("actor fail-stop must retain every exact owner forever");
        assert!(offline < resume);
        assert!(resume < retry);
        assert!(retry < acknowledged);
        assert!(acknowledged < retention);
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
