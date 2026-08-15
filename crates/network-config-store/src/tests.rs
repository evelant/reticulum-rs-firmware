use super::*;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use std::{format, vec, vec::Vec};

const DEVICE_CONFIG_OFFSET: usize = 0x618000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeError {
    Bounds,
    Alignment,
    Injected,
}

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
            Self::Injected => NorFlashErrorKind::Other,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum WriteFault {
    Partial(usize),
    LostReply,
}

#[derive(Clone, Copy, Debug)]
struct ArmedWriteFault {
    successful_writes_before_fault: usize,
    fault: WriteFault,
}

#[derive(Clone)]
struct FakeNor {
    bytes: Vec<u8>,
    reads: usize,
    writes: Vec<(usize, usize)>,
    erases: Vec<(usize, usize)>,
    write_fault: Option<ArmedWriteFault>,
}

impl FakeNor {
    fn erased() -> Self {
        Self {
            bytes: vec![0xff; PARTITION_SIZE],
            reads: 0,
            writes: Vec::new(),
            erases: Vec::new(),
            write_fault: None,
        }
    }

    fn fail_write_after(&mut self, successful_writes_before_fault: usize, fault: WriteFault) {
        self.write_fault = Some(ArmedWriteFault {
            successful_writes_before_fault,
            fault,
        });
    }

    fn program(&mut self, offset: usize, bytes: &[u8]) {
        for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            *stored &= *supplied;
        }
    }
}

impl ErrorType for FakeNor {
    type Error = FakeError;
}

impl ReadNorFlash for FakeNor {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, bytes.len()).map_err(map_check_error)?;
        self.reads += 1;
        let offset = offset as usize;
        bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for FakeNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = SECTOR_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        let offset = offset as usize;
        self.writes.push((offset, bytes.len()));
        let trigger = self
            .write_fault
            .is_some_and(|fault| fault.successful_writes_before_fault == 0);
        if !trigger {
            if let Some(fault) = &mut self.write_fault {
                fault.successful_writes_before_fault -= 1;
            }
            self.program(offset, bytes);
            return Ok(());
        }
        let fault = self.write_fault.take().expect("armed write fault");
        match fault.fault {
            WriteFault::Partial(length) => {
                let length = length.min(bytes.len());
                self.program(offset, &bytes[..length]);
                Err(FakeError::Injected)
            }
            WriteFault::LostReply => {
                self.program(offset, bytes);
                Err(FakeError::Injected)
            }
        }
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        let range = from as usize..to as usize;
        self.erases.push((range.start, range.end));
        self.bytes[range].fill(0xff);
        Ok(())
    }
}

impl MultiwriteNorFlash for FakeNor {}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::Injected,
    }
}

const fn device(byte: u8) -> NetworkConfigStoreDeviceId {
    NetworkConfigStoreDeviceId::new([byte; 16])
}

const fn binding_for(device: NetworkConfigStoreDeviceId) -> NetworkConfigStoreBinding {
    NetworkConfigStoreBinding::new(
        device,
        DEVICE_CONFIG_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

fn bound(flash: FakeNor) -> BoundNetworkConfigStore<FakeNor> {
    BoundNetworkConfigStore::new(flash, binding_for(device(0x51)))
}

fn id(byte: u8) -> WifiProfileId {
    WifiProfileId::new([byte; 16]).expect("nonzero profile ID")
}

fn profile(byte: u8, ssid: &[u8], password: &[u8], enabled: bool, priority: u8) -> WifiProfile {
    WifiProfile::new(id(byte), ssid, password, enabled, priority).expect("valid profile")
}

fn default_peer(ipv4: [u8; 4], enabled: bool) -> OutboundTcpPeer {
    OutboundTcpPeer::with_default_port(ipv4, enabled).expect("valid peer")
}

fn dns_peer(hostname: &[u8], enabled: bool) -> OutboundTcpPeer {
    OutboundTcpPeer::dns_with_default_port(hostname, enabled).expect("valid DNS peer")
}

fn first_config() -> NetworkConfig {
    let mut config = NetworkConfig::empty();
    config
        .insert_wifi_profile(profile(
            1,
            b"mesh-lab",
            b"correct horse battery staple",
            true,
            20,
        ))
        .expect("first profile");
    config.set_tcp_peer(Some(default_peer([192, 168, 1, 20], true)));
    config
}

fn assert_first_config(config: &NetworkConfig) {
    assert_eq!(config.wifi_profile_count(), 1);
    let profile = config.wifi_profiles().next().expect("one profile");
    assert_eq!(profile.id(), id(1));
    assert_eq!(profile.ssid(), b"mesh-lab");
    assert_eq!(profile.password(), b"correct horse battery staple");
    assert!(profile.enabled());
    assert_eq!(profile.priority(), 20);
    assert_eq!(
        config.tcp_peer(),
        Some(default_peer([192, 168, 1, 20], true))
    );
    assert!(config.wifi_transport_enabled());
    assert!(config.automatic_announces_enabled());
    assert!(!config.rmap_discovery_enabled());
    assert!(!config.rmap_share_location());
    assert_eq!(config.phone_location(), None);
    assert_eq!(config.lora_tx_power(), LoraTxPower::Dbm14);
}

fn provision_first() -> (MountedNetworkConfigStore, BoundNetworkConfigStore<FakeNor>) {
    let mut access = bound(FakeNor::erased());
    let mounted = provision_erased(&mut access, &first_config()).expect("explicit provision");
    (mounted, access)
}

#[test]
fn semantic_model_enforces_wifi_and_tcp_bounds() {
    assert_eq!(
        WifiProfileId::new([0; 16]),
        Err(NetworkConfigModelError::ZeroWifiProfileId)
    );
    assert!(matches!(
        WifiProfile::new(id(1), b"", b"12345678", true, 0),
        Err(NetworkConfigModelError::InvalidSsidLength)
    ));
    assert!(matches!(
        WifiProfile::new(id(1), &[b's'; 33], b"12345678", true, 0),
        Err(NetworkConfigModelError::InvalidSsidLength)
    ));
    assert!(matches!(
        WifiProfile::new(id(1), b"ssid", b"1234567", true, 0),
        Err(NetworkConfigModelError::InvalidWpa2PasswordLength)
    ));
    assert!(matches!(
        WifiProfile::new(id(1), b"ssid", &[b'p'; 64], true, 0),
        Err(NetworkConfigModelError::InvalidWpa2PasswordLength)
    ));
    assert!(matches!(
        WifiProfile::new(id(1), b"ssid", b"bad\npassword", true, 0),
        Err(NetworkConfigModelError::InvalidWpa2PasswordCharacter)
    ));
    assert!(WifiProfile::new(id(1), b"ssid", b" !\"#$%&'()*+,-./", true, 0).is_ok());
    assert_eq!(
        OutboundTcpPeer::new([1, 2, 3, 4], 0, true),
        Err(NetworkConfigModelError::InvalidTcpPort)
    );
    assert_eq!(
        default_peer([1, 2, 3, 4], true).port(),
        DEFAULT_RETICULUM_TCP_PORT
    );
    assert_eq!(
        OutboundTcpPeer::new([0, 1, 2, 3], 4242, true),
        Err(NetworkConfigModelError::CurrentNetworkTcpPeerAddress)
    );
    assert_eq!(
        OutboundTcpPeer::new([127, 0, 0, 1], 4242, true),
        Err(NetworkConfigModelError::LoopbackTcpPeerAddress)
    );
    assert_eq!(
        OutboundTcpPeer::with_default_port([224, 0, 0, 1], true),
        Err(NetworkConfigModelError::MulticastTcpPeerAddress)
    );
    assert_eq!(
        OutboundTcpPeer::new([255, 255, 255, 255], 4242, true),
        Err(NetworkConfigModelError::LimitedBroadcastTcpPeerAddress)
    );
    assert_eq!(
        OutboundTcpPeer::new([240, 0, 0, 1], 4242, true),
        Err(NetworkConfigModelError::ReservedTcpPeerAddress)
    );
    let ipv4_peer = default_peer([1, 2, 3, 4], true);
    assert_eq!(ipv4_peer.ipv4(), Some([1, 2, 3, 4]));
    assert_eq!(ipv4_peer.dns_hostname(), None);
    assert_eq!(
        ipv4_peer.address(),
        OutboundTcpPeerAddress::Ipv4([1, 2, 3, 4])
    );

    let dns_peer = dns_peer(b"rmap.world", true);
    assert_eq!(dns_peer.ipv4(), None);
    assert_eq!(dns_peer.dns_hostname(), Some(&b"rmap.world"[..]));
    assert_eq!(
        dns_peer.address(),
        OutboundTcpPeerAddress::Dns(DnsHostname::new(b"rmap.world").expect("valid hostname"))
    );
    assert_eq!(
        OutboundTcpPeer::with_dns_hostname(b"rmap.world", 0, true),
        Err(NetworkConfigModelError::InvalidTcpPort)
    );
    assert_eq!(
        OutboundTcpPeer::dns_with_default_port(b"", true),
        Err(NetworkConfigModelError::InvalidDnsHostnameLength)
    );
    assert_eq!(
        OutboundTcpPeer::dns_with_default_port(&[b'a'; MAX_DNS_HOSTNAME_LENGTH + 1], true),
        Err(NetworkConfigModelError::InvalidDnsHostnameLength)
    );
    assert_eq!(
        OutboundTcpPeer::dns_with_default_port(b"bad_host.example", true),
        Err(NetworkConfigModelError::InvalidDnsHostnameCharacter)
    );
    assert_eq!(
        OutboundTcpPeer::dns_with_default_port(b"bad..example", true),
        Err(NetworkConfigModelError::InvalidDnsHostnameLabel)
    );
    assert_eq!(
        OutboundTcpPeer::dns_with_default_port(b"-bad.example", true),
        Err(NetworkConfigModelError::InvalidDnsHostnameLabel)
    );
    assert_eq!(
        OutboundTcpPeer::dns_with_default_port(&[b'a'; 64], true),
        Err(NetworkConfigModelError::InvalidDnsHostnameLabel)
    );

    assert_eq!(
        PhoneLocation::new(MIN_LATITUDE_E6, MAX_LONGITUDE_E6),
        Ok(PhoneLocation::new(MIN_LATITUDE_E6, MAX_LONGITUDE_E6).expect("boundary location"))
    );
    assert_eq!(
        PhoneLocation::new(MIN_LATITUDE_E6 - 1, 0),
        Err(NetworkConfigModelError::InvalidLatitude)
    );
    assert_eq!(
        PhoneLocation::new(0, MAX_LONGITUDE_E6 + 1),
        Err(NetworkConfigModelError::InvalidLongitude)
    );
    for (dbm, expected) in [
        (14, LoraTxPower::Dbm14),
        (17, LoraTxPower::Dbm17),
        (20, LoraTxPower::Dbm20),
        (22, LoraTxPower::Dbm22),
    ] {
        assert_eq!(LoraTxPower::try_from_dbm(dbm), Ok(expected));
        assert_eq!(LoraTxPower::try_from(dbm), Ok(expected));
        assert_eq!(expected.requested_dbm(), dbm);
    }
    assert_eq!(
        LoraTxPower::try_from_dbm(21),
        Err(NetworkConfigModelError::InvalidLoraTxPower)
    );
    assert_eq!(LoraTxPower::default(), LoraTxPower::Dbm14);
    assert_eq!(NetworkConfig::empty().lora_tx_power(), LoraTxPower::Dbm14);
    assert_eq!(
        NetworkConfig::empty().lora_profile(),
        LoraRadioProfile::DEFAULT
    );
    assert_eq!(
        LoraRadioProfile::new(0, 125_000, 7, 5, LoraTxPower::Dbm14),
        Err(NetworkConfigModelError::InvalidLoraFrequency)
    );
    assert_eq!(
        LoraRadioProfile::new(915_000_000, 100_000, 7, 5, LoraTxPower::Dbm14),
        Err(NetworkConfigModelError::InvalidLoraBandwidth)
    );
    assert_eq!(
        LoraRadioProfile::new(915_000_000, 125_000, 6, 5, LoraTxPower::Dbm14),
        Err(NetworkConfigModelError::InvalidLoraSpreadingFactor)
    );
    assert_eq!(
        LoraRadioProfile::new(915_000_000, 125_000, 7, 9, LoraTxPower::Dbm14),
        Err(NetworkConfigModelError::InvalidLoraCodingRate)
    );

    let mut config = NetworkConfig::empty();
    for byte in 1..=4 {
        config
            .insert_wifi_profile(profile(byte, b"ssid", b"12345678", true, byte))
            .expect("within capacity");
    }
    assert!(matches!(
        config.insert_wifi_profile(profile(1, b"other", b"abcdefgh", true, 0)),
        Err(NetworkConfigModelError::DuplicateWifiProfileId)
    ));
    assert!(matches!(
        config.insert_wifi_profile(profile(5, b"fifth", b"abcdefgh", true, 0)),
        Err(NetworkConfigModelError::WifiProfileCapacityExceeded)
    ));
    config
        .upsert_wifi_profile(profile(2, b"replacement", b"abcdefgh", false, 99))
        .expect("replacement does not consume capacity");
    let replacement = config
        .wifi_profiles()
        .find(|profile| profile.id() == id(2))
        .expect("replacement exists");
    assert_eq!(replacement.ssid(), b"replacement");
    assert!(!replacement.enabled());
}

#[test]
fn redacted_projection_contains_no_password_or_password_length() {
    let mut config = first_config();
    config.set_wifi_transport_enabled(false);
    config.set_automatic_announces_enabled(false);
    config.set_rmap_discovery_enabled(true);
    config.set_rmap_share_location(true);
    let location = PhoneLocation::new(42_360_082, -71_058_880).expect("Boston location");
    config.set_phone_location(Some(location));
    config.set_lora_tx_power(LoraTxPower::Dbm22);
    let redacted = config.redacted();
    let profile = redacted.wifi_profiles().next().expect("projection");
    assert_eq!(profile.ssid(), b"mesh-lab");
    assert!(profile.password_configured());
    assert_eq!(profile.priority(), 20);
    let debug = format!("{redacted:?}");
    assert!(!debug.contains("correct horse battery staple"));
    assert!(!debug.contains("28"));
    assert!(!redacted.wifi_transport_enabled());
    assert!(!redacted.automatic_announces_enabled());
    assert!(redacted.rmap_discovery_enabled());
    assert!(redacted.rmap_share_location());
    assert_eq!(redacted.phone_location(), Some(location));
    assert_eq!(redacted.lora_tx_power(), LoraTxPower::Dbm22);
}

#[test]
fn semantic_equality_includes_every_policy_location_and_radio_field() {
    let left = first_config();
    let mut right = first_config();
    assert!(configuration_eq(&left, &right));

    right.set_wifi_transport_enabled(false);
    assert!(!configuration_eq(&left, &right));
    right.set_wifi_transport_enabled(true);
    right.set_automatic_announces_enabled(false);
    assert!(!configuration_eq(&left, &right));
    right.set_automatic_announces_enabled(true);
    right.set_rmap_discovery_enabled(true);
    assert!(!configuration_eq(&left, &right));
    right.set_rmap_discovery_enabled(false);
    right.set_rmap_share_location(true);
    assert!(!configuration_eq(&left, &right));
    right.set_rmap_share_location(false);
    right.set_phone_location(Some(PhoneLocation::new(1, -1).expect("valid location")));
    assert!(!configuration_eq(&left, &right));
    right.set_phone_location(None);
    right.set_lora_tx_power(LoraTxPower::Dbm17);
    assert!(!configuration_eq(&left, &right));
    right.set_lora_profile(left.lora_profile());
    right.set_lora_profile(
        LoraRadioProfile::new(914_875_000, 250_000, 9, 7, LoraTxPower::Dbm14)
            .expect("valid radio profile"),
    );
    assert!(!configuration_eq(&left, &right));
}

#[test]
fn semantic_snapshot_round_trips_dns_policy_phone_location_and_radio_profile() {
    let mut config = first_config();
    config.set_tcp_peer(Some(dns_peer(b"rmap.world", true)));
    config.set_wifi_transport_enabled(false);
    config.set_automatic_announces_enabled(false);
    config.set_rmap_discovery_enabled(true);
    config.set_rmap_share_location(true);
    let location = PhoneLocation::new(-33_868_820, 151_209_296).expect("Sydney location");
    config.set_phone_location(Some(location));
    let radio_profile = LoraRadioProfile::new(914_875_000, 250_000, 9, 7, LoraTxPower::Dbm20)
        .expect("valid radio profile");
    config.set_lora_profile(radio_profile);

    let mut access = bound(FakeNor::erased());
    let mounted = provision_erased(&mut access, &config).expect("current provision");
    assert_eq!(
        read_u16(&access.backend().bytes, 10),
        SEMANTIC_FORMAT_VERSION
    );
    assert_eq!(
        mounted.configuration().tcp_peer(),
        Some(dns_peer(b"rmap.world", true))
    );
    assert!(!mounted.configuration().wifi_transport_enabled());
    assert!(!mounted.configuration().automatic_announces_enabled());
    assert!(mounted.configuration().rmap_discovery_enabled());
    assert!(mounted.configuration().rmap_share_location());
    assert_eq!(mounted.configuration().phone_location(), Some(location));
    assert_eq!(mounted.configuration().lora_tx_power(), LoraTxPower::Dbm20);
    assert_eq!(mounted.configuration().lora_profile(), radio_profile);
    assert_eq!(access.backend().bytes[LORA_TX_POWER_DBM_OFFSET], 20);

    let cold = mount(&mut access).expect("cold mount");
    assert!(configuration_eq(cold.configuration(), &config));
}

#[test]
fn semantic_snapshot_round_trips_each_supported_lora_power() {
    for power in [
        LoraTxPower::Dbm14,
        LoraTxPower::Dbm17,
        LoraTxPower::Dbm20,
        LoraTxPower::Dbm22,
    ] {
        let mut config = first_config();
        config.set_lora_tx_power(power);
        let mut access = bound(FakeNor::erased());

        let mounted = provision_erased(&mut access, &config).expect("current provision");
        assert_eq!(mounted.configuration().lora_tx_power(), power);
        assert_eq!(
            i32::from(access.backend().bytes[LORA_TX_POWER_DBM_OFFSET]),
            power.requested_dbm()
        );
        let cold = mount(&mut access).expect("cold mount");
        assert_eq!(cold.configuration().lora_tx_power(), power);
    }
}

#[test]
fn semantic_snapshot_rejects_an_unsupported_lora_power_byte() {
    let mut access = bound(FakeNor::erased());
    let _ = provision_erased(&mut access, &first_config()).expect("current provision");
    access.backend_mut().bytes[LORA_TX_POWER_DBM_OFFSET] = 21;
    let digest = snapshot_digest(&access.backend().bytes[..PROTECTED_SIZE]);
    access.backend_mut().bytes[DIGEST_OFFSET..COMMIT_OFFSET].copy_from_slice(&digest);

    assert!(matches!(
        mount(&mut access),
        Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::CommittedSnapshotCorrupt {
                sector: NetworkConfigStoreSector::A
            }
        ))
    ));
}

#[test]
fn every_non_current_semantic_version_is_rejected() {
    for version in 1..SEMANTIC_FORMAT_VERSION {
        let mut access = bound(FakeNor::erased());
        let mut record = encode_record(
            access.binding(),
            NetworkConfigStoreSector::A,
            NonZeroU64::new(1).expect("nonzero generation"),
            0,
            ZERO_DIGEST,
            &first_config(),
        );
        put_u16(&mut record[..], 10, version);
        let digest = snapshot_digest(&record[..PROTECTED_SIZE]);
        record[DIGEST_OFFSET..COMMIT_OFFSET].copy_from_slice(&digest);
        access.backend_mut().program(0, &record[..]);

        assert!(matches!(
            mount(&mut access),
            Err(NetworkConfigStoreError::Fault(
                NetworkConfigStoreFault::UnsupportedSemanticVersion(actual)
            )) if actual == version
        ));
    }
}

#[test]
fn erased_and_programmed_uncommitted_media_are_never_auto_formatted() {
    let mut access = bound(FakeNor::erased());
    assert!(matches!(
        mount(&mut access),
        Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::UnformattedErased
        ))
    ));
    assert!(access.backend().writes.is_empty());
    assert!(access.backend().erases.is_empty());

    access.backend_mut().bytes[17] = 0x7f;
    assert!(matches!(
        mount(&mut access),
        Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::UnformattedNonErased
        ))
    ));
    assert!(matches!(
        provision_erased(&mut access, &first_config()),
        Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::UnformattedNonErased
        ))
    ));
    assert!(access.backend().writes.is_empty());
    assert!(access.backend().erases.is_empty());
}

#[test]
fn explicit_provision_commits_marker_last_and_remounts_exact_configuration() {
    let mut access = bound(FakeNor::erased());
    let mounted = provision_erased(&mut access, &first_config()).expect("provision");
    assert_eq!(mounted.revision().generation().get(), 1);
    assert_eq!(mounted.revision().sector(), NetworkConfigStoreSector::A);
    assert_eq!(mounted.cleanup(), NetworkConfigStoreCleanup::Clean);
    assert_first_config(mounted.configuration());
    assert_eq!(
        access.backend().writes,
        vec![(0, COMMIT_OFFSET), (COMMIT_OFFSET, COMMIT_SIZE)]
    );
    assert!(access.backend().erases.is_empty());

    let remounted = mount(&mut access).expect("cold remount");
    assert_eq!(remounted.revision(), mounted.revision());
    assert_first_config(remounted.configuration());
}

#[test]
fn successor_uses_inactive_bank_then_cleanup_preserves_new_authority() {
    let (mounted, mut access) = provision_first();
    let predecessor = mounted.revision();
    let mut updated = mounted.into_configuration();
    updated
        .insert_wifi_profile(profile(2, b"field-backup", b"another password", false, 5))
        .expect("second profile");
    updated.set_tcp_peer(Some(
        OutboundTcpPeer::new([10, 0, 0, 7], 9_999, true).expect("peer"),
    ));
    updated.set_lora_tx_power(LoraTxPower::Dbm17);

    let successor = commit_successor(&mut access, predecessor, &updated).expect("generation two");
    assert_eq!(successor.revision().generation().get(), 2);
    assert_eq!(successor.revision().sector(), NetworkConfigStoreSector::B);
    assert_eq!(
        successor.cleanup(),
        NetworkConfigStoreCleanup::EraseInactive {
            sector: NetworkConfigStoreSector::A
        }
    );
    assert_eq!(successor.configuration().wifi_profile_count(), 2);
    assert_eq!(
        successor.configuration().tcp_peer(),
        Some(OutboundTcpPeer::new([10, 0, 0, 7], 9_999, true).expect("peer"))
    );
    assert_eq!(
        successor.configuration().lora_tx_power(),
        LoraTxPower::Dbm17
    );

    let cold = mount(&mut access).expect("cold generation-two remount");
    assert_eq!(cold.revision(), successor.revision());
    let cleaned = cleanup(&mut access, cold.revision()).expect("erase predecessor");
    assert_eq!(cleaned.cleanup(), NetworkConfigStoreCleanup::Clean);
    assert_eq!(cleaned.revision(), successor.revision());
    assert_eq!(
        access.backend().erases,
        vec![(SECTOR_SIZE, PARTITION_SIZE), (0, SECTOR_SIZE),]
    );
}

#[test]
fn torn_successor_prefix_leaves_predecessor_authoritative_after_restart() {
    let (mounted, mut access) = provision_first();
    let predecessor = mounted.revision();
    let mut updated = mounted.into_configuration();
    updated.set_tcp_peer(Some(default_peer([10, 1, 2, 3], true)));
    updated.set_lora_tx_power(LoraTxPower::Dbm22);
    access
        .backend_mut()
        .fail_write_after(0, WriteFault::Partial(137));

    assert!(matches!(
        commit_successor(&mut access, predecessor, &updated),
        Err(NetworkConfigStoreError::Backend(FakeError::Injected))
    ));
    let recovered = mount(&mut access).expect("predecessor survives torn prefix");
    assert_eq!(recovered.revision(), predecessor);
    assert_eq!(
        recovered.cleanup(),
        NetworkConfigStoreCleanup::EraseInactive {
            sector: NetworkConfigStoreSector::B
        }
    );
    assert_first_config(recovered.configuration());
}

#[test]
fn lost_commit_reply_is_resolved_by_read_only_remount() {
    let (mounted, mut access) = provision_first();
    let predecessor = mounted.revision();
    let mut updated = mounted.into_configuration();
    updated.set_tcp_peer(Some(default_peer([172, 16, 0, 4], true)));
    updated.set_lora_tx_power(LoraTxPower::Dbm20);
    access
        .backend_mut()
        .fail_write_after(1, WriteFault::LostReply);

    assert!(matches!(
        commit_successor(&mut access, predecessor, &updated),
        Err(NetworkConfigStoreError::Backend(FakeError::Injected))
    ));
    let recovered = mount(&mut access).expect("fully programmed successor remounts");
    assert_eq!(recovered.revision().generation().get(), 2);
    assert_eq!(recovered.revision().sector(), NetworkConfigStoreSector::B);
    assert_eq!(
        recovered.configuration().tcp_peer(),
        Some(default_peer([172, 16, 0, 4], true))
    );
    assert_eq!(
        recovered.configuration().lora_tx_power(),
        LoraTxPower::Dbm20
    );
}

#[test]
fn committed_corruption_fails_closed_even_when_predecessor_is_valid() {
    let (mounted, mut access) = provision_first();
    let predecessor = mounted.revision();
    let mut updated = mounted.into_configuration();
    updated.set_tcp_peer(Some(default_peer([10, 20, 30, 40], true)));
    let successor = commit_successor(&mut access, predecessor, &updated).expect("generation two");
    assert_eq!(successor.revision().sector(), NetworkConfigStoreSector::B);
    access.backend_mut().bytes[SECTOR_SIZE + DIGEST_OFFSET] ^= 1;

    assert!(matches!(
        mount(&mut access),
        Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::CommittedSnapshotCorrupt {
                sector: NetworkConfigStoreSector::B
            }
        ))
    ));
}

#[test]
fn committed_snapshot_is_bound_to_device_and_absolute_range() {
    let (_mounted, access) = provision_first();
    let backend = access.into_backend();
    let mut wrong_device = BoundNetworkConfigStore::new(backend.clone(), binding_for(device(0x52)));
    assert!(matches!(
        mount(&mut wrong_device),
        Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::DeviceBindingMismatch {
                sector: NetworkConfigStoreSector::A
            }
        ))
    ));

    let wrong_range_binding = NetworkConfigStoreBinding::new(
        device(0x51),
        DEVICE_CONFIG_OFFSET + PARTITION_SIZE,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    );
    let mut wrong_range = BoundNetworkConfigStore::new(backend, wrong_range_binding);
    assert!(matches!(
        mount(&mut wrong_range),
        Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::DeviceBindingMismatch {
                sector: NetworkConfigStoreSector::A
            }
        ))
    ));
}

#[test]
fn stale_revision_is_rejected_before_any_new_mutation() {
    let (mounted, mut access) = provision_first();
    let stale = mounted.revision();
    let mut generation_two = mounted.into_configuration();
    generation_two.set_tcp_peer(Some(default_peer([192, 0, 2, 1], true)));
    let current = commit_successor(&mut access, stale, &generation_two).expect("generation two");
    let writes_before = access.backend().writes.len();
    let erases_before = access.backend().erases.len();

    assert!(matches!(
        commit_successor(&mut access, stale, current.configuration()),
        Err(NetworkConfigStoreError::Fault(
            NetworkConfigStoreFault::StaleRevision
        ))
    ));
    assert_eq!(access.backend().writes.len(), writes_before);
    assert_eq!(access.backend().erases.len(), erases_before);
}

#[test]
fn binding_shape_fails_before_any_io() {
    let invalid = NetworkConfigStoreBinding::new(
        device(0x51),
        DEVICE_CONFIG_OFFSET + 1,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    );
    let mut access = BoundNetworkConfigStore::new(FakeNor::erased(), invalid);
    assert!(matches!(
        mount(&mut access),
        Err(NetworkConfigStoreError::Binding(
            NetworkConfigStoreBindingError::ReadAlignmentMismatch { .. }
        ))
    ));
    assert_eq!(access.backend().reads, 0);
}
