//! Fixed, host-checkable policy for the opt-in E290 Wi-Fi API proof.
//!
//! This is a local administration bearer, not a Reticulum packet interface.
//! The proof profile deliberately exposes one raw TCP client at a time and
//! replaces, rather than accompanies, the ordinary USB API bearer.

/// Prefix used for the board-specific SoftAP SSID.
pub const SOFTAP_SSID_PREFIX: &[u8] = b"reticulum-e290-";
/// Number of trailing eFuse MAC bytes encoded into the SoftAP SSID.
pub const SOFTAP_MAC_SUFFIX_BYTES: usize = 3;
/// Exact bytes in `reticulum-e290-` plus six lowercase hexadecimal digits.
pub const SOFTAP_SSID_BYTES: usize = SOFTAP_SSID_PREFIX.len() + SOFTAP_MAC_SUFFIX_BYTES * 2;

/// Development SoftAP channel.
pub const SOFTAP_CHANNEL: u8 = 6;
/// Maximum stations admitted by the proof SoftAP.
pub const SOFTAP_MAX_STATIONS: u16 = 1;
/// WPA2-Personal passphrase for the opt-in development proof.
///
/// This value is intentionally public and fixed so an operator can join the
/// first build-qualified image without a second provisioning channel. It
/// prevents an accidentally open access point, but is not a production secret
/// or a replacement for application-layer confidentiality.
pub const SOFTAP_DEVELOPMENT_PASSPHRASE: &str = "reticulum-e290-dev";

/// Static IPv4 address served by the E290.
pub const GATEWAY_IPV4: [u8; 4] = [192, 168, 4, 1];
/// Static IPv4 prefix length served by the E290.
pub const GATEWAY_PREFIX_LEN: u8 = 24;
/// Raw authenticated RDA1 TCP service port.
///
/// This deliberately does not reuse the conventional RNode TCP port.
pub const RDA1_TCP_PORT: u16 = 29_716;

/// Embassy network socket metadata slots.
pub const NETWORK_STACK_RESOURCES: usize = 4;
/// TCP receive bytes retained for the one accepted client.
pub const TCP_RX_BUFFER_BYTES: usize = 1_536;
/// TCP transmit bytes retained for the one accepted client.
pub const TCP_TX_BUFFER_BYTES: usize = 1_536;
/// Maximum RDA1 bytes consumed or enqueued in one API task turn.
pub const MAX_RDA1_BYTES_PER_POLL: usize = 64;
/// Idle TCP timeout in seconds.
pub const TCP_IDLE_TIMEOUT_SECONDS: u64 = 300;
/// TCP keepalive interval in seconds.
pub const TCP_KEEPALIVE_SECONDS: u64 = 30;
/// Poll interval while handoff or socket readiness is pending.
pub const API_POLL_INTERVAL_MS: u64 = 1;
/// Delay before retrying a terminated DHCP server run.
pub const DHCP_RETRY_INTERVAL_MS: u64 = 500;
/// Maximum DHCP leases retained by the proof server.
pub const DHCP_LEASES: usize = 4;

/// Build the unique SoftAP SSID from the final three bytes of the eFuse MAC.
pub const fn softap_ssid(mac: [u8; 6]) -> [u8; SOFTAP_SSID_BYTES] {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut ssid = [0_u8; SOFTAP_SSID_BYTES];
    let mut prefix = 0;
    while prefix < SOFTAP_SSID_PREFIX.len() {
        ssid[prefix] = SOFTAP_SSID_PREFIX[prefix];
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < SOFTAP_MAC_SUFFIX_BYTES {
        let byte = mac[mac.len() - SOFTAP_MAC_SUFFIX_BYTES + suffix];
        let output = SOFTAP_SSID_PREFIX.len() + suffix * 2;
        ssid[output] = HEX[(byte >> 4) as usize];
        ssid[output + 1] = HEX[(byte & 0x0f) as usize];
        suffix += 1;
    }
    ssid
}

const _: () = assert!(SOFTAP_SSID_BYTES <= 32);
const _: () = assert!(SOFTAP_CHANNEL >= 1 && SOFTAP_CHANNEL <= 11);
const _: () = assert!(SOFTAP_MAX_STATIONS == 1);
const _: () =
    assert!(SOFTAP_DEVELOPMENT_PASSPHRASE.len() >= 8 && SOFTAP_DEVELOPMENT_PASSPHRASE.len() <= 63);
const _: () = assert!(GATEWAY_PREFIX_LEN == 24);
const _: () = assert!(RDA1_TCP_PORT != 0);
const _: () = assert!(NETWORK_STACK_RESOURCES >= 3);
const _: () = assert!(TCP_RX_BUFFER_BYTES >= 1_500);
const _: () = assert!(TCP_TX_BUFFER_BYTES >= 1_500);
const _: () = assert!(MAX_RDA1_BYTES_PER_POLL > 0);
const _: () = assert!(DHCP_LEASES > 0);

#[cfg(test)]
mod tests {
    use super::{
        GATEWAY_IPV4, GATEWAY_PREFIX_LEN, RDA1_TCP_PORT, SOFTAP_CHANNEL,
        SOFTAP_DEVELOPMENT_PASSPHRASE, SOFTAP_MAX_STATIONS, softap_ssid,
    };

    #[test]
    fn profile_is_static_single_client_and_board_specific() {
        assert_eq!(GATEWAY_IPV4, [192, 168, 4, 1]);
        assert_eq!(GATEWAY_PREFIX_LEN, 24);
        assert_eq!(RDA1_TCP_PORT, 29_716);
        assert_eq!(SOFTAP_CHANNEL, 6);
        assert_eq!(SOFTAP_MAX_STATIONS, 1);
        assert_eq!(SOFTAP_DEVELOPMENT_PASSPHRASE, "reticulum-e290-dev");
        assert!((8..=63).contains(&SOFTAP_DEVELOPMENT_PASSPHRASE.len()));
        assert_eq!(
            &softap_ssid([0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88]),
            b"reticulum-e290-e13e88"
        );
        assert_ne!(
            softap_ssid([0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88]),
            softap_ssid([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88])
        );
    }
}
