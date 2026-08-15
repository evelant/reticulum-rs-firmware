//! Bounded product model for the Wi-Fi station interface.
//!
//! This module owns no controller, socket, flash, or device-API capability. It
//! selects one immutable boot-time profile from the durable configuration and
//! provides a copy-safe, passphrase-free runtime status projection.

use core::{cell::RefCell, str};

use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use reticulum_network_config_store::{
    MAX_SSID_LENGTH, MAX_WPA2_PASSWORD_LENGTH, NetworkConfig, WifiProfile,
};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Embassy network-stack socket/resource slots for DHCP, DNS, and TCP.
pub const NETWORK_STACK_RESOURCES: usize = 3;
/// Association/DHCP deadline before the station enters bounded backoff.
pub const ASSOCIATION_TIMEOUT_SECONDS: u64 = 30;
/// Initial reconnect backoff.
pub const INITIAL_RECONNECT_BACKOFF_SECONDS: u64 = 1;
/// Maximum reconnect backoff.
pub const MAXIMUM_RECONNECT_BACKOFF_SECONDS: u64 = 30;

/// Upstream-tested esp-radio receive-queue depth retained for robust LAN
/// unicast while BLE and Wi-Fi coexist.
pub const WIFI_RX_QUEUE_SIZE: usize = 5;
/// Upstream-tested esp-radio transmit-queue depth.
pub const WIFI_TX_QUEUE_SIZE: usize = 3;
/// Persistent 1.6 KiB receive-buffer count from the pinned driver baseline.
///
/// Wi-Fi station builds provide 120 KiB of strict internal heap so the product
/// can retain the upstream receive pool for burst tolerance and BLE
/// coexistence reliability.
pub const WIFI_STATIC_RX_BUFFERS: u8 = 10;
/// Bounded dynamic receive-buffer count from the pinned driver baseline.
pub const WIFI_DYNAMIC_RX_BUFFERS: u16 = 32;
/// No additional persistent static transmit buffers in the driver baseline.
pub const WIFI_STATIC_TX_BUFFERS: u8 = 0;
/// Bounded dynamic transmit-buffer count from the pinned driver baseline.
pub const WIFI_DYNAMIC_TX_BUFFERS: u16 = 32;
/// Receive aggregation is retained for AP compatibility and burst tolerance.
pub const WIFI_AMPDU_RX_ENABLED: bool = true;
/// Transmit aggregation is retained for AP compatibility and burst tolerance.
pub const WIFI_AMPDU_TX_ENABLED: bool = true;
/// Receive aggregation window from the pinned driver baseline.
pub const WIFI_RX_BA_WINDOW: u8 = 6;
/// Maximum 2.4 GHz Wi-Fi transmit power in quarter-dBm units.
///
/// The esp-radio default is only 20 (5 dBm). Sixty selects 15 dBm while
/// staying below the upstream warning threshold near 65 where some boards
/// exhibit association failures.
pub const WIFI_MAX_TX_POWER_QUARTER_DBM: i8 = 60;

/// Complete boot-time station decision for one available configuration.
///
/// A disabled plan still retains the applied revision so a reboot can converge
/// desired and runtime state without inventing a Wi-Fi connection.
pub enum WifiStationBootPlan {
    /// No enabled Wi-Fi profile exists in this applied revision.
    Disabled {
        /// Durable configuration revision applied by this boot.
        applied_revision: u64,
    },
    /// Connect with the selected immutable WPA2-Personal profile.
    Connect(WifiStationBootstrap),
}

impl WifiStationBootPlan {
    /// Select the deterministic station profile, or retain a disabled applied
    /// revision when no profile is enabled.
    pub fn select(applied_revision: u64, configuration: &NetworkConfig) -> Self {
        match WifiStationBootstrap::select(applied_revision, configuration) {
            Some(bootstrap) => Self::Connect(bootstrap),
            None => Self::Disabled { applied_revision },
        }
    }
}

/// One selected immutable station profile copied out of durable boot state.
///
/// This type intentionally implements neither `Clone` nor `Debug`. Its
/// passphrase is zeroized when the station task terminates or replaces it.
pub struct WifiStationBootstrap {
    applied_revision: u64,
    profile_id: [u8; 16],
    ssid: [u8; MAX_SSID_LENGTH],
    ssid_length: u8,
    password: [u8; MAX_WPA2_PASSWORD_LENGTH],
    password_length: u8,
}

impl WifiStationBootstrap {
    /// Select the highest-priority enabled profile.
    ///
    /// Equal priorities use the lexicographically smallest opaque ID so flash
    /// slot order cannot change the selected network.
    pub fn select(applied_revision: u64, configuration: &NetworkConfig) -> Option<Self> {
        if !configuration.wifi_transport_enabled() {
            return None;
        }
        let selected = configuration
            .wifi_profiles()
            .filter(|profile| profile.enabled())
            .max_by(|left, right| compare_profiles(left, right))?;
        Some(Self::from_profile(applied_revision, selected))
    }

    fn from_profile(applied_revision: u64, profile: &WifiProfile) -> Self {
        let mut ssid = [0_u8; MAX_SSID_LENGTH];
        ssid[..profile.ssid().len()].copy_from_slice(profile.ssid());
        let mut password = [0_u8; MAX_WPA2_PASSWORD_LENGTH];
        password[..profile.password().len()].copy_from_slice(profile.password());
        Self {
            applied_revision,
            profile_id: *profile.id().as_bytes(),
            ssid,
            ssid_length: profile.ssid().len() as u8,
            password,
            password_length: profile.password().len() as u8,
        }
    }

    /// Durable configuration generation applied by this boot.
    pub const fn applied_revision(&self) -> u64 {
        self.applied_revision
    }

    /// Opaque configured Wi-Fi profile ID.
    pub const fn profile_id(&self) -> [u8; 16] {
        self.profile_id
    }

    /// Exact configured SSID bytes.
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..usize::from(self.ssid_length)]
    }

    /// Validated printable-ASCII WPA2 passphrase.
    pub fn password(&self) -> &str {
        str::from_utf8(&self.password[..usize::from(self.password_length)])
            .expect("network config accepts only printable ASCII passphrases")
    }
}

impl Zeroize for WifiStationBootstrap {
    fn zeroize(&mut self) {
        self.password.zeroize();
        self.password_length.zeroize();
    }
}

impl ZeroizeOnDrop for WifiStationBootstrap {}

impl Drop for WifiStationBootstrap {
    fn drop(&mut self) {
        self.zeroize();
    }
}

fn compare_profiles(left: &WifiProfile, right: &WifiProfile) -> core::cmp::Ordering {
    left.priority()
        .cmp(&right.priority())
        .then_with(|| right.id().as_bytes().cmp(left.id().as_bytes()))
}

/// Volatile station lifecycle visible through the management API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiStationPhase {
    /// No enabled durable station profile was selected.
    Disabled,
    /// Controller is attempting association.
    Associating,
    /// Link is associated and DHCP configuration is pending.
    Dhcp,
    /// Station has a usable IPv4 configuration.
    Online,
    /// A bounded delay precedes the next association attempt.
    Backoff,
    /// Controller initialization failed for this boot.
    Faulted,
}

/// Passphrase-free latest-value station status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiStationStatus {
    /// Durable configuration generation applied by the running task.
    pub applied_revision: u64,
    /// Selected opaque profile ID.
    pub profile_id: Option<[u8; 16]>,
    /// Current station lifecycle.
    pub phase: WifiStationPhase,
    /// DHCP-assigned IPv4 address.
    pub ipv4: Option<[u8; 4]>,
    /// Most recently observed whole-dBm RSSI.
    pub rssi_dbm: Option<i8>,
    ssid: [u8; MAX_SSID_LENGTH],
    ssid_length: u8,
}

impl WifiStationStatus {
    /// Initial status when the additive station profile is not active.
    pub const DISABLED: Self = Self {
        applied_revision: 0,
        profile_id: None,
        phase: WifiStationPhase::Disabled,
        ipv4: None,
        rssi_dbm: None,
        ssid: [0; MAX_SSID_LENGTH],
        ssid_length: 0,
    };

    /// Disabled runtime that has nevertheless applied a durable revision.
    pub const fn disabled_at(applied_revision: u64) -> Self {
        Self {
            applied_revision,
            ..Self::DISABLED
        }
    }

    /// Construct one status for the selected boot profile.
    pub fn for_bootstrap(bootstrap: &WifiStationBootstrap, phase: WifiStationPhase) -> Self {
        let mut ssid = [0; MAX_SSID_LENGTH];
        ssid[..bootstrap.ssid().len()].copy_from_slice(bootstrap.ssid());
        Self {
            applied_revision: bootstrap.applied_revision,
            profile_id: Some(bootstrap.profile_id),
            phase,
            ipv4: None,
            rssi_dbm: None,
            ssid,
            ssid_length: bootstrap.ssid().len() as u8,
        }
    }

    /// Exact SSID selected at boot, exposed only while the actor is online.
    pub fn connected_ssid(&self) -> Option<&[u8]> {
        (self.phase == WifiStationPhase::Online)
            .then_some(&self.ssid[..usize::from(self.ssid_length)])
    }
}

/// Blocking latest-value cell shared by the station actor and node API owner.
pub struct WifiStationStatusCell {
    state: Mutex<CriticalSectionRawMutex, RefCell<WifiStationStatus>>,
}

impl WifiStationStatusCell {
    /// Construct a disabled status cell.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(RefCell::new(WifiStationStatus::DISABLED)),
        }
    }

    /// Replace the complete passphrase-free status.
    pub fn publish(&self, status: WifiStationStatus) {
        self.state.lock(|state| *state.borrow_mut() = status);
    }

    /// Copy the latest complete status.
    pub fn snapshot(&self) -> WifiStationStatus {
        self.state.lock(|state| *state.borrow())
    }
}

impl Default for WifiStationStatusCell {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use reticulum_network_config_store::{NetworkConfig, WifiProfile, WifiProfileId};

    use super::{
        WIFI_AMPDU_RX_ENABLED, WIFI_AMPDU_TX_ENABLED, WIFI_DYNAMIC_RX_BUFFERS,
        WIFI_DYNAMIC_TX_BUFFERS, WIFI_MAX_TX_POWER_QUARTER_DBM, WIFI_RX_BA_WINDOW,
        WIFI_RX_QUEUE_SIZE, WIFI_STATIC_RX_BUFFERS, WIFI_STATIC_TX_BUFFERS, WIFI_TX_QUEUE_SIZE,
        WifiStationBootPlan, WifiStationBootstrap, WifiStationPhase, WifiStationStatus,
        WifiStationStatusCell,
    };

    fn profile(id: u8, priority: u8, enabled: bool) -> WifiProfile {
        let mut bytes = [0_u8; 16];
        bytes[15] = id;
        WifiProfile::new(
            WifiProfileId::new(bytes).unwrap(),
            &[b'a', id],
            b"password",
            enabled,
            priority,
        )
        .unwrap()
    }

    #[test]
    fn selection_is_priority_then_opaque_id_and_excludes_disabled_profiles() {
        let mut config = NetworkConfig::empty();
        config.insert_wifi_profile(profile(3, 8, true)).unwrap();
        config.insert_wifi_profile(profile(2, 10, false)).unwrap();
        config.insert_wifi_profile(profile(5, 9, true)).unwrap();
        config.insert_wifi_profile(profile(4, 9, true)).unwrap();

        let selected = WifiStationBootstrap::select(7, &config).unwrap();
        assert_eq!(selected.applied_revision(), 7);
        assert_eq!(selected.profile_id()[15], 4);
        assert_eq!(selected.ssid(), b"a\x04");
        assert_eq!(selected.password(), "password");
    }

    #[test]
    fn border_node_keeps_the_upstream_receive_buffer_profile() {
        assert_eq!(WIFI_RX_QUEUE_SIZE, 5);
        assert_eq!(WIFI_TX_QUEUE_SIZE, 3);
        assert_eq!(WIFI_STATIC_RX_BUFFERS, 10);
        assert_eq!(WIFI_DYNAMIC_RX_BUFFERS, 32);
        assert_eq!(WIFI_STATIC_TX_BUFFERS, 0);
        assert_eq!(WIFI_DYNAMIC_TX_BUFFERS, 32);
        assert!(WIFI_AMPDU_RX_ENABLED);
        assert!(WIFI_AMPDU_TX_ENABLED);
        assert_eq!(WIFI_RX_BA_WINDOW, 6);
        assert_eq!(WIFI_MAX_TX_POWER_QUARTER_DBM, 60);
        assert!(WIFI_STATIC_RX_BUFFERS >= WIFI_RX_BA_WINDOW);
        assert!(u16::from(WIFI_RX_BA_WINDOW) < WIFI_DYNAMIC_RX_BUFFERS);
        assert!(u16::from(WIFI_RX_BA_WINDOW) < 2 * u16::from(WIFI_STATIC_RX_BUFFERS));
    }

    #[test]
    fn disabled_boot_plan_retains_the_applied_revision() {
        let config = NetworkConfig::empty();
        let WifiStationBootPlan::Disabled { applied_revision } =
            WifiStationBootPlan::select(19, &config)
        else {
            panic!("empty configuration selected a station profile")
        };
        assert_eq!(applied_revision, 19);
        assert_eq!(
            WifiStationStatus::disabled_at(applied_revision).applied_revision,
            19
        );
    }

    #[test]
    fn master_transport_switch_preserves_profiles_but_suppresses_association() {
        let mut config = NetworkConfig::empty();
        config.insert_wifi_profile(profile(7, 9, true)).unwrap();
        config.set_wifi_transport_enabled(false);

        let WifiStationBootPlan::Disabled { applied_revision } =
            WifiStationBootPlan::select(23, &config)
        else {
            panic!("globally disabled Wi-Fi selected a saved profile")
        };
        assert_eq!(applied_revision, 23);
        assert_eq!(config.wifi_profile_count(), 1);
    }

    #[test]
    fn runtime_ssid_is_the_immutable_boot_value_not_a_later_desired_projection() {
        let config = {
            let mut config = NetworkConfig::empty();
            config.insert_wifi_profile(profile(7, 9, true)).unwrap();
            config
        };
        let bootstrap = WifiStationBootstrap::select(11, &config).unwrap();
        let offline = WifiStationStatus::for_bootstrap(&bootstrap, WifiStationPhase::Dhcp);
        let online = WifiStationStatus::for_bootstrap(&bootstrap, WifiStationPhase::Online);
        assert_eq!(offline.connected_ssid(), None);
        assert_eq!(online.connected_ssid(), Some(&b"a\x07"[..]));
    }

    #[test]
    fn status_cell_publishes_one_complete_projection() {
        let cell = WifiStationStatusCell::new();
        assert_eq!(cell.snapshot(), WifiStationStatus::DISABLED);
        let status = WifiStationStatus {
            phase: WifiStationPhase::Online,
            ipv4: Some([192, 0, 2, 10]),
            rssi_dbm: Some(-42),
            ..WifiStationStatus::DISABLED
        };
        cell.publish(status);
        assert_eq!(cell.snapshot(), status);
    }
}
