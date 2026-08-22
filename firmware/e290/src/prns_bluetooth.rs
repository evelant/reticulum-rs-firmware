//! E290 composition of PRNS Bluetooth Auto's public Trouble backend.
//!
//! This module contains only ESP32-S3 controller setup, static ownership, and
//! PSRAM placement. Peer discovery, central/peripheral negotiation, GATT/L2CAP
//! fallback, Columba compatibility, interface lifecycle, and backpressure all
//! remain owned by unmodified PRNS.

use core::future::Future;
use core::pin::Pin;

use allocator_api2::boxed::Box;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use esp_radio::ble::controller::BleConnector;
use personal_rns::bluetooth_auto::{BluetoothAuto, BluetoothAutoShared, BluetoothAutoStatus};
use personal_rns::identity::IDENTITY_SECRET_KEY_LEN;
use personal_rns::interfaces::bluetooth_auto::{
    BLE_HW_MTU, BleIdentity, BleRoleCapabilities, Endpoint, Esp32Host, LinkCapabilities,
    MAX_ADVERTISEMENT_LEN, Psm, encode_advertisement,
};
use prns_interfaces_embassy::bluetooth_auto::GattCharacteristic;
use prns_interfaces_embassy::bluetooth_auto::{
    self, BleHub, CooperativeTransport, GATT_VALUE_CAP, GattServer, L2CAP_PSM, PEER_CAPACITY,
    ReticulumGattCharacteristics, ReticulumGattUuids, TroubleController, TroubleStack, acceptor,
    dialer, host_runner, serve_slot,
};
use sha2::{Digest, Sha256};
use static_cell::StaticCell;
use trouble_host::prelude::*;

use super::E290PrnsPsramAlloc;
use crate::prns_target::E290BleFleet;
use reticulum_e290_firmware::prns_node::BLUETOOTH_PEER_CAPACITY;

type Transport = CooperativeTransport<BleConnector<'static>>;
type HostStack = TroubleStack<Transport>;
const BLE_IDENTITY_DOMAIN: &[u8] = b"reticulum-e290/prns-bluetooth-auto/v1";
const BLE_CONTROLLER_TASK_PRIORITY: u8 = 28;
const BLE_CONTROLLER_ACTIVITY_CAPACITY: u8 = (BLUETOOTH_PEER_CAPACITY + 1) as u8;

const _: () = assert!(PEER_CAPACITY == BLUETOOTH_PEER_CAPACITY);
const _: () = assert!(PEER_CAPACITY == 4);
const _: () = assert!(BLE_CONTROLLER_ACTIVITY_CAPACITY <= 10);

/// ESP controller sizing used by PRNS's qualified S3 Bluetooth composition.
pub(crate) fn controller_config() -> esp_radio::ble::Config {
    esp_radio::ble::Config::default()
        .with_task_priority(BLE_CONTROLLER_TASK_PRIORITY)
        .with_task_stack_size(4096)
        .with_max_connections(BLE_CONTROLLER_ACTIVITY_CAPACITY)
}

/// Derive a stable, unlinkable Bluetooth Auto identity from durable node key material.
///
/// Bluetooth Auto identities are public tie-breaker identifiers, not Reticulum
/// identities or bearer credentials. Domain-separated hashing avoids exposing
/// any node-secret bytes and avoids a bespoke Bluetooth partition.
pub(crate) fn identity_from_node_secret(
    node_secret: &[u8; IDENTITY_SECRET_KEY_LEN],
) -> BleIdentity {
    let mut digest = Sha256::new();
    digest.update(BLE_IDENTITY_DOMAIN);
    digest.update(node_secret);
    let digest = digest.finalize();
    let mut identity = [0u8; personal_rns::interfaces::bluetooth_auto::BLE_IDENTITY_LEN];
    identity.copy_from_slice(&digest[..personal_rns::interfaces::bluetooth_auto::BLE_IDENTITY_LEN]);
    BleIdentity::new(identity)
}

fn allocate_psram<T>(value: T) -> Option<&'static mut T> {
    match Box::try_new_in(value, E290PrnsPsramAlloc) {
        Ok(value) => Some(Box::leak(value)),
        Err(_) => None,
    }
}

async fn serve_owned_slot(
    idx: usize,
    hub: &'static BleHub,
    stack: &'static HostStack,
    server: &'static GattServer,
    gatt: ReticulumGattOwned,
) {
    let ReticulumGattOwned {
        control,
        data,
        columba_rx,
        columba_tx,
        service_uuid,
        control_uuid,
        data_uuid,
        columba_rx_uuid,
        columba_tx_uuid,
        columba_identity_uuid,
    } = gatt;
    serve_slot(
        idx,
        hub,
        stack,
        server,
        ReticulumGattCharacteristics {
            control: &control,
            data: &data,
            columba_rx: &columba_rx,
            columba_tx: &columba_tx,
        },
        ReticulumGattUuids {
            service: &service_uuid,
            control: &control_uuid,
            data: &data_uuid,
            columba_rx: &columba_rx_uuid,
            columba_tx: &columba_tx_uuid,
            columba_identity: &columba_identity_uuid,
        },
    )
    .await;
}

#[embassy_executor::task(pool_size = 4)]
async fn serve_slot_task(run: Pin<&'static mut dyn Future<Output = ()>>) {
    run.await;
}

struct ReticulumGattOwned {
    control: GattCharacteristic,
    data: GattCharacteristic,
    columba_rx: GattCharacteristic,
    columba_tx: GattCharacteristic,
    service_uuid: Uuid,
    control_uuid: Uuid,
    data_uuid: Uuid,
    columba_rx_uuid: Uuid,
    columba_tx_uuid: Uuid,
    columba_identity_uuid: Uuid,
}

/// Run the sole ESP controller and PRNS Bluetooth Auto supervisor.
pub(crate) async fn run(
    connector: BleConnector<'static>,
    mac: [u8; 6],
    ble_identity: BleIdentity,
    fleet: E290BleFleet,
    shared: &'static BluetoothAutoShared<{ BLUETOOTH_PEER_CAPACITY }>,
    spawner: Spawner,
) {
    let controller = TroubleController::<Transport>::new(CooperativeTransport::new(connector));
    static RESOURCES: StaticCell<HostResources<DefaultPacketPool, PEER_CAPACITY, PEER_CAPACITY>> =
        StaticCell::new();
    let resources = RESOURCES.init(HostResources::new());

    let mut address = mac;
    address[5] |= 0b1100_0000;
    static STACK: StaticCell<HostStack> = StaticCell::new();
    let stack: &'static HostStack = STACK.init(
        trouble_host::new(controller, resources).set_random_address(Address::random(address)),
    );
    let Host {
        mut peripheral,
        central,
        runner,
        ..
    } = stack.build();

    let control_store = match allocate_psram([0; GATT_VALUE_CAP]) {
        Some(value) => value,
        None => return log_allocation_failure("control-value"),
    };
    let data_store = match allocate_psram([0; GATT_VALUE_CAP]) {
        Some(value) => value,
        None => return log_allocation_failure("data-value"),
    };
    let columba_rx_store = match allocate_psram([0; GATT_VALUE_CAP]) {
        Some(value) => value,
        None => return log_allocation_failure("columba-rx-value"),
    };
    let columba_tx_store = match allocate_psram([0; GATT_VALUE_CAP]) {
        Some(value) => value,
        None => return log_allocation_failure("columba-tx-value"),
    };
    let columba_identity_store = match allocate_psram([0; GATT_VALUE_CAP]) {
        Some(value) => value,
        None => return log_allocation_failure("columba-identity-value"),
    };
    let Some((table, control, data, columba_rx, columba_tx)) =
        bluetooth_auto::reticulum_attribute_table(
            control_store,
            data_store,
            columba_rx_store,
            columba_tx_store,
            columba_identity_store,
            ble_identity,
        )
    else {
        log::error!("e290-prns bluetooth-auto status=DISABLED reason=attribute-table-capacity");
        return;
    };
    static SERVER: StaticCell<GattServer> = StaticCell::new();
    let server: &'static GattServer = SERVER.init(AttributeServer::new(table));

    let mut adv_data = [0u8; MAX_ADVERTISEMENT_LEN];
    let adv_len = match encode_advertisement(&mut adv_data, BleRoleCapabilities::DualRole) {
        Some(len) => len,
        None => {
            log::error!("e290-prns bluetooth-auto status=DISABLED reason=advertisement-capacity");
            return;
        }
    };

    let service_uuid = bluetooth_auto::service_uuid();
    let control_uuid = bluetooth_auto::control_uuid();
    let data_uuid = bluetooth_auto::data_uuid();
    let columba_rx_uuid = bluetooth_auto::columba_rx_uuid();
    let columba_tx_uuid = bluetooth_auto::columba_tx_uuid();
    let columba_identity_uuid = bluetooth_auto::columba_identity_uuid();

    static HUB: StaticCell<BleHub> = StaticCell::new();
    let hub: &'static BleHub = HUB.init(BleHub::new(BluetoothAutoStatus::new(shared)));
    hub.set_local_address(address);

    let supervisor = BluetoothAuto::new(
        hub.backend(),
        ble_identity,
        Endpoint::Esp32(Esp32Host::Esp32),
        LinkCapabilities {
            l2cap: Psm::new(L2CAP_PSM),
            link_mtu: BLE_HW_MTU as u16,
        },
        shared,
    );

    for idx in 0..PEER_CAPACITY {
        let gatt = ReticulumGattOwned {
            control: control.clone(),
            data: data.clone(),
            columba_rx: columba_rx.clone(),
            columba_tx: columba_tx.clone(),
            service_uuid: service_uuid.clone(),
            control_uuid: control_uuid.clone(),
            data_uuid: data_uuid.clone(),
            columba_rx_uuid: columba_rx_uuid.clone(),
            columba_tx_uuid: columba_tx_uuid.clone(),
            columba_identity_uuid: columba_identity_uuid.clone(),
        };
        let run = match allocate_psram(serve_owned_slot(idx, hub, stack, server, gatt)) {
            Some(run) => run,
            None => {
                log::error!(
                    "e290-prns bluetooth-auto status=DISABLED reason=slot-future-allocation slot={idx}"
                );
                return;
            }
        };
        let run: Pin<&'static mut dyn Future<Output = ()>> =
            // SAFETY: the PSRAM allocation is leaked and therefore never moves or is freed.
            unsafe { Pin::new_unchecked(run) };
        match serve_slot_task(run) {
            Ok(token) => spawner.spawn(token),
            Err(_) => {
                log::error!(
                    "e290-prns bluetooth-auto status=DISABLED reason=slot-task-capacity slot={idx}"
                );
                return;
            }
        }
    }

    log::info!(
        "e290-prns bluetooth-auto status=READY peers={} roles=central,peripheral transports=gatt,l2cap",
        PEER_CAPACITY
    );
    let host = host_runner(hub, runner);
    let radio = join(
        acceptor(hub, &mut peripheral, &adv_data[..adv_len]),
        dialer(hub, central),
    );
    let plane = join(radio, supervisor.run(fleet));
    join(host, plane).await;
}

fn log_allocation_failure(stage: &str) {
    log::error!(
        "e290-prns bluetooth-auto status=DISABLED reason=external-allocation stage={stage}"
    );
}

#[embassy_executor::task]
pub(crate) async fn task(run: Pin<&'static mut dyn Future<Output = ()>>) {
    run.await;
}

/// Place the large controller/supervisor future in mapped PSRAM and spawn it.
pub(crate) fn spawn(
    spawner: Spawner,
    connector: BleConnector<'static>,
    mac: [u8; 6],
    identity: BleIdentity,
    fleet: E290BleFleet,
    shared: &'static BluetoothAutoShared<{ BLUETOOTH_PEER_CAPACITY }>,
) -> bool {
    let run = match allocate_psram(run(connector, mac, identity, fleet, shared, spawner)) {
        Some(run) => run,
        None => {
            log_allocation_failure("owner-future");
            return false;
        }
    };
    let run: Pin<&'static mut dyn Future<Output = ()>> =
        // SAFETY: the PSRAM allocation is leaked and therefore never moves or is freed.
        unsafe { Pin::new_unchecked(run) };
    match task(run) {
        Ok(token) => {
            spawner.spawn(token);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetooth_identity_is_stable_domain_separated_and_not_secret_prefix() {
        let secret = [0x5a; IDENTITY_SECRET_KEY_LEN];
        let first = identity_from_node_secret(&secret);
        let second = identity_from_node_secret(&secret);
        assert_eq!(first, second);
        assert_ne!(first.as_bytes(), &secret[..first.as_bytes().len()]);
    }
}
