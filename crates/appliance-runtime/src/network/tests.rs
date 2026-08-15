use super::*;

fn profile_id() -> device_api::WifiNetworkProfileId {
    device_api::WifiNetworkProfileId::new([0x44; 16]).unwrap()
}

#[test]
fn projections_preserve_non_utf8_ssids_and_exact_network_state() {
    let wifi =
        device_api::WifiNetworkConfigSummary::new(profile_id(), true, 200, b"field\xff", true)
            .unwrap();
    let peer = device_api::ReticulumTcpPeerConfigSummary::new(
        true,
        device_api::ReticulumTcpPeerIpv4Address::new([192, 0, 2, 9]).unwrap(),
        4242,
    )
    .unwrap();
    let config = device_api::NetworkConfigSnapshot::with_defaults(
        9,
        [Some(wifi), None, None, None],
        Some(peer),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(NetworkConfigView::from(config)).unwrap(),
        serde_json::json!({
            "revision": 9,
            "wifi_profiles": [{
                "profile_id": "44".repeat(16),
                "enabled": true,
                "priority": 200,
                "ssid": {"encoding": "hex", "value": "6669656c64ff"},
                "credential_configured": true
            }],
            "tcp_peer": {
                "enabled": true,
                "ipv4_address": "192.0.2.9",
                "port": 4242
            },
            "wifi_transport_enabled": true,
            "automatic_announces_enabled": true,
            "rmap_discovery_enabled": false,
            "rmap_share_location": false,
            "rmap_phone_location": null,
            "lora_tx_power_dbm": 14,
            "lora_profile": {
                "frequency_hz": 915_000_000,
                "bandwidth_hz": 125_000,
                "spreading_factor": 7,
                "coding_rate_denominator": 5,
                "tx_power_dbm": 14
            }
        })
    );

    let status = device_api::NetworkRuntimeStatus::new(
        9,
        8,
        device_api::WifiStationState::Connecting,
        Some(profile_id()),
        Some(b"field\xff"),
        Some([198, 51, 100, 7]),
        Some(-81),
        device_api::ReticulumTcpPeerState::WaitingForNetwork,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(NetworkRuntimeStatusView::from(status)).unwrap(),
        serde_json::json!({
            "configured_revision": 9,
            "applied_revision": 8,
            "wifi_state": "connecting",
            "active_wifi_profile": "44".repeat(16),
            "connected_ssid": {"encoding": "hex", "value": "6669656c64ff"},
            "ipv4_address": "198.51.100.7",
            "rssi_dbm": -81,
            "tcp_peer_state": "waiting_for_network",
            "last_tcp_failure": null,
            "dns_diagnostics": null,
            "rmap_status": null
        })
    );
}

#[test]
fn tcp_backoff_and_last_failure_remain_typed_at_the_json_boundary() {
    let status = device_api::NetworkRuntimeStatus::new_with_tcp_failure(
        11,
        11,
        device_api::WifiStationState::Connected,
        None,
        Some(b"field"),
        Some([192, 0, 2, 7]),
        Some(-75),
        device_api::ReticulumTcpPeerState::Backoff,
        Some(device_api::ReticulumTcpFailure::DnsNoIpv4Result),
    )
    .unwrap();

    assert_eq!(
        serde_json::to_value(NetworkRuntimeStatusView::from(status)).unwrap(),
        serde_json::json!({
            "configured_revision": 11,
            "applied_revision": 11,
            "wifi_state": "connected",
            "active_wifi_profile": null,
            "connected_ssid": {"encoding": "utf8", "value": "field"},
            "ipv4_address": "192.0.2.7",
            "rssi_dbm": -75,
            "tcp_peer_state": "backoff",
            "last_tcp_failure": "dns_no_ipv4_result",
            "dns_diagnostics": null,
            "rmap_status": null
        })
    );
}

#[test]
fn dns_diagnostics_preserve_sparse_slots_sources_and_response_codes() {
    let diagnostics = device_api::ReticulumDnsDiagnostics::new(
        Some([192, 168, 50, 1]),
        [Some([192, 168, 50, 1]), None, Some([192, 0, 2, 53])],
        device_api::ReticulumDnsPrimaryOutcome::LookupFailed,
        device_api::ReticulumDnsRawSetupState::Ready,
        [
            Some(device_api::ReticulumDnsRawAttempt::new(
                device_api::ReticulumDnsRawSource::Dhcp,
                [192, 168, 50, 1],
                device_api::ReticulumDnsRawOutcome::Timeout,
            )),
            None,
            Some(device_api::ReticulumDnsRawAttempt::new(
                device_api::ReticulumDnsRawSource::Public,
                [1, 1, 1, 1],
                device_api::ReticulumDnsRawOutcome::response_code_outcome(3).unwrap(),
            )),
            Some(device_api::ReticulumDnsRawAttempt::new(
                device_api::ReticulumDnsRawSource::Public,
                [9, 9, 9, 9],
                device_api::ReticulumDnsRawOutcome::Resolved,
            )),
            None,
        ],
        Some(device_api::ReticulumDnsResolution::new(
            [217, 154, 9, 220],
            device_api::ReticulumDnsResolutionSource::RawPublic,
            Some([9, 9, 9, 9]),
        )),
    );
    let status = device_api::NetworkRuntimeStatus::new_with_tcp_diagnostics(
        12,
        12,
        device_api::WifiStationState::Connected,
        None,
        Some(b"field"),
        Some([192, 168, 50, 42]),
        Some(-64),
        device_api::ReticulumTcpPeerState::Connecting,
        Some(device_api::ReticulumTcpFailure::DnsLookupFailed),
        Some(diagnostics),
    )
    .unwrap();

    assert_eq!(
        serde_json::to_value(NetworkRuntimeStatusView::from(status)).unwrap(),
        serde_json::json!({
            "configured_revision": 12,
            "applied_revision": 12,
            "wifi_state": "connected",
            "active_wifi_profile": null,
            "connected_ssid": {"encoding": "utf8", "value": "field"},
            "ipv4_address": "192.168.50.42",
            "rssi_dbm": -64,
            "tcp_peer_state": "connecting",
            "last_tcp_failure": "dns_lookup_failed",
            "dns_diagnostics": {
                "gateway_ipv4": "192.168.50.1",
                "dhcp_servers": ["192.168.50.1", null, "192.0.2.53"],
                "primary_outcome": "lookup_failed",
                "raw_setup_state": "ready",
                "raw_attempts": [
                    {
                        "source": "dhcp",
                        "server": "192.168.50.1",
                        "outcome": {"kind": "timeout"}
                    },
                    null,
                    {
                        "source": "public",
                        "server": "1.1.1.1",
                        "outcome": {"kind": "response_code", "code": 3}
                    },
                    {
                        "source": "public",
                        "server": "9.9.9.9",
                        "outcome": {"kind": "resolved"}
                    },
                    null
                ],
                "resolution": {
                    "address": "217.154.9.220",
                    "source": "raw_public",
                    "resolver": "9.9.9.9"
                }
            },
            "rmap_status": null
        })
    );
}

#[test]
fn rmap_status_preserves_gate_queue_cadence_and_failure_evidence() {
    let status = device_api::NetworkRuntimeStatus::new(
        13,
        12,
        device_api::WifiStationState::Connected,
        None,
        None,
        Some([192, 0, 2, 42]),
        Some(-58),
        device_api::ReticulumTcpPeerState::Connected,
    )
    .unwrap()
    .with_rmap_status(device_api::RmapRuntimeStatus::new(
        false,
        device_api::RmapStampPhase::Ready,
        8_192,
        device_api::RmapInitialTcpGateState::Open,
        1,
        device_api::RmapQueueOutcome::OrdinaryAdmissionDeferred,
        Some(77),
        device_api::RmapEgressConfirmation::NotObserved,
        Some(60),
        Some(device_api::RmapDeferredReason::OrdinaryQueueRejected),
    ));

    let value = serde_json::to_value(NetworkRuntimeStatusView::from(status)).unwrap();
    assert_eq!(
        value["rmap_status"],
        serde_json::json!({
            "config_applied": false,
            "stamp_phase": "ready",
            "stamp_attempts": 8192,
            "initial_tcp_gate": "open",
            "queued_count": 1,
            "last_queue_outcome": "ordinary_admission_deferred",
            "last_queue_attempt_at_uptime_seconds": 77,
            "egress_confirmation": "not_observed",
            "next_due_in_seconds": 60,
            "deferred_reason": "ordinary_queue_rejected"
        })
    );
}

#[test]
fn secret_bearing_upsert_maps_to_borrowed_device_request_without_formatting_secret() {
    let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "upsert_wifi",
            "profile_id": "55".repeat(16),
            "enabled": true,
            "priority": 240,
            "ssid": {"encoding": "hex", "value": "6d657368ff"},
            "credential": {
                "kind": "replace",
                "passphrase": "correct horse battery staple"
            }
        },
        "expected_revision": 7,
        "idempotency_key": "66".repeat(16)
    }))
    .unwrap();
    let observed = request
        .with_device_request(|request| match request.mutation() {
            device_api::NetworkConfigMutation::UpsertWifi {
                profile_id,
                network,
            } => (
                *profile_id.as_bytes(),
                network.enabled(),
                network.priority(),
                network.ssid().as_bytes().to_vec(),
                network.credential().replacement().unwrap().to_vec(),
                request.expected_revision(),
                request.idempotency_key().0,
            ),
            _ => panic!("expected Wi-Fi upsert"),
        })
        .unwrap();
    assert_eq!(observed.0, [0x55; 16]);
    assert!(observed.1);
    assert_eq!(observed.2, 240);
    assert_eq!(observed.3, b"mesh\xff");
    assert_eq!(observed.4, b"correct horse battery staple");
    assert_eq!(observed.5, 7);
    assert_eq!(observed.6, [0x66; 16]);
}

#[test]
fn mutation_outcomes_are_typed_and_validation_errors_are_secret_free() {
    assert_eq!(
        serde_json::to_value(NetworkConfigMutationOutcome::Applied {
            revision: 10,
            reboot_required: true,
        })
        .unwrap(),
        serde_json::json!({
            "outcome": "applied",
            "revision": 10,
            "reboot_required": true
        })
    );
    assert_eq!(
        serde_json::to_value(NetworkConfigMutationOutcome::RevisionConflict {
            current_revision: 11,
        })
        .unwrap(),
        serde_json::json!({
            "outcome": "revision_conflict",
            "current_revision": 11
        })
    );

    let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "upsert_wifi",
            "profile_id": "55".repeat(16),
            "enabled": true,
            "priority": 1,
            "ssid": {"encoding": "utf8", "value": "mesh"},
            "credential": {
                "kind": "replace",
                "passphrase": "TOP-SECRET"
            }
        },
        "expected_revision": 1,
        "idempotency_key": "invalid"
    }))
    .unwrap();
    let error = request.with_device_request(|_| ()).unwrap_err().to_string();
    assert!(!error.contains("TOP-SECRET"));
    assert_eq!(
        error,
        "idempotency key must contain exactly 32 hexadecimal characters"
    );
}

#[test]
fn tcp_peer_mutation_preserves_exact_ipv4_and_rejects_non_peer_addresses() {
    let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "replace_tcp_peer",
            "peer": {
                "enabled": true,
                "ipv4_address": "198.51.100.42",
                "port": 4242
            }
        },
        "expected_revision": 12,
        "idempotency_key": "77".repeat(16)
    }))
    .unwrap();
    let observed = request
        .with_device_request(|request| match request.mutation() {
            device_api::NetworkConfigMutation::ReplaceTcpPeer(Some(peer)) => (
                peer.enabled(),
                peer.ipv4_address().octets(),
                peer.port(),
                request.expected_revision(),
            ),
            _ => panic!("expected TCP peer replacement"),
        })
        .unwrap();
    assert_eq!(observed, (true, [198, 51, 100, 42], 4242, 12));

    let multicast: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "replace_tcp_peer",
            "peer": {
                "enabled": true,
                "ipv4_address": "239.1.2.3",
                "port": 4242
            }
        },
        "expected_revision": 12,
        "idempotency_key": "77".repeat(16)
    }))
    .unwrap();
    assert_eq!(
        multicast.with_device_request(|_| ()),
        Err(NetworkRequestError::InvalidIpv4Address)
    );
}

#[test]
fn hostname_gateway_and_rmap_state_project_without_losing_fixed_point_coordinates() {
    let wifi =
        device_api::WifiNetworkConfigSummary::new(profile_id(), true, 200, b"field", true).unwrap();
    let peer =
        device_api::ReticulumTcpPeerHostConfigSummary::new(true, "rmap.world", 4242).unwrap();
    let location = device_api::RmapLocation::new(42_360_100, -71_058_900).unwrap();
    let config = device_api::NetworkConfigSnapshot::new(
        13,
        [Some(wifi), None, None, None],
        None,
        Some(peer),
        device_api::GatewayPolicy::new(false, false),
        device_api::RmapConfig::new(true, true, Some(location)),
        device_api::LoraRadioProfile::DEFAULT
            .with_tx_power(device_api::LoraTransmitPowerDbm::DBM_22),
    )
    .unwrap();

    assert_eq!(
        serde_json::to_value(NetworkConfigView::from(config)).unwrap(),
        serde_json::json!({
            "revision": 13,
            "wifi_profiles": [{
                "profile_id": "44".repeat(16),
                "enabled": true,
                "priority": 200,
                "ssid": {"encoding": "utf8", "value": "field"},
                "credential_configured": true
            }],
            "tcp_peer": {
                "enabled": true,
                "hostname": "rmap.world",
                "port": 4242
            },
            "wifi_transport_enabled": false,
            "automatic_announces_enabled": false,
            "rmap_discovery_enabled": true,
            "rmap_share_location": true,
            "rmap_phone_location": {
                "latitude_e6": 42_360_100,
                "longitude_e6": -71_058_900
            },
            "lora_tx_power_dbm": 22,
            "lora_profile": {
                "frequency_hz": 915_000_000,
                "bandwidth_hz": 125_000,
                "spreading_factor": 7,
                "coding_rate_denominator": 5,
                "tx_power_dbm": 22
            }
        })
    );
}

#[test]
fn hostname_gateway_and_rmap_mutations_map_to_api_1_8_requests() {
    let hostname: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "replace_tcp_host_peer",
            "peer": {
                "enabled": true,
                "hostname": "node.reticulumnet.nl",
                "port": 4242
            }
        },
        "expected_revision": 12,
        "idempotency_key": "88".repeat(16)
    }))
    .unwrap();
    let observed = hostname
        .with_device_request(|request| match request.mutation() {
            device_api::NetworkConfigMutation::ReplaceTcpHostPeer(Some(peer)) => (
                peer.enabled(),
                peer.hostname().as_str().to_owned(),
                peer.port(),
                request.expected_revision(),
            ),
            _ => panic!("expected hostname TCP peer replacement"),
        })
        .unwrap();
    assert_eq!(
        observed,
        (true, "node.reticulumnet.nl".to_owned(), 4242, 12)
    );

    let gateway: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "set_gateway_policy",
            "wifi_transport_enabled": false,
            "automatic_announces_enabled": true
        },
        "expected_revision": 13,
        "idempotency_key": "99".repeat(16)
    }))
    .unwrap();
    let observed = gateway
        .with_device_request(|request| match request.mutation() {
            device_api::NetworkConfigMutation::SetGatewayPolicy(policy) => (
                policy.wifi_transport_enabled(),
                policy.automatic_announces_enabled(),
            ),
            _ => panic!("expected gateway policy"),
        })
        .unwrap();
    assert_eq!(observed, (false, true));

    let rmap: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "set_rmap_config",
            "discovery_enabled": true,
            "share_location": true,
            "phone_location": {
                "latitude_e6": 42_360_100,
                "longitude_e6": -71_058_900
            }
        },
        "expected_revision": 14,
        "idempotency_key": "aa".repeat(16)
    }))
    .unwrap();
    let observed = rmap
        .with_device_request(|request| match request.mutation() {
            device_api::NetworkConfigMutation::SetRmapConfig(config) => {
                let location = config.phone_location().unwrap();
                (
                    config.discovery_enabled(),
                    config.share_location(),
                    location.latitude_e6(),
                    location.longitude_e6(),
                )
            }
            _ => panic!("expected RMAP policy"),
        })
        .unwrap();
    assert_eq!(observed, (true, true, 42_360_100, -71_058_900));
}

#[test]
fn rmap_mutation_rejects_out_of_world_fixed_point_coordinates() {
    let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "set_rmap_config",
            "discovery_enabled": true,
            "share_location": true,
            "phone_location": {
                "latitude_e6": 90_000_001,
                "longitude_e6": 0
            }
        },
        "expected_revision": 14,
        "idempotency_key": "aa".repeat(16)
    }))
    .unwrap();
    assert_eq!(
        request.with_device_request(|_| ()),
        Err(NetworkRequestError::InvalidRmapLocation)
    );
}

#[test]
fn lora_transmit_power_mutation_accepts_only_qualified_radio_outputs() {
    let ts_config = ts_rs::Config::default();
    assert!(NetworkConfigView::decl(&ts_config).contains("lora_tx_power_dbm: 14 | 17 | 20 | 22"));
    assert!(
        NetworkConfigMutation::decl(&ts_config).contains("lora_tx_power_dbm: 14 | 17 | 20 | 22")
    );

    let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "set_lora_tx_power",
            "lora_tx_power_dbm": 22
        },
        "expected_revision": 15,
        "idempotency_key": "bb".repeat(16)
    }))
    .unwrap();
    let observed = request
        .with_device_request(|request| match request.mutation() {
            device_api::NetworkConfigMutation::SetLoraTxPower(power) => (
                power.get(),
                request.expected_revision(),
                request.idempotency_key().0,
            ),
            _ => panic!("expected LoRa transmit-power mutation"),
        })
        .unwrap();
    assert_eq!(observed, (22, 15, [0xbb; 16]));

    let invalid: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "set_lora_tx_power",
            "lora_tx_power_dbm": 21
        },
        "expected_revision": 15,
        "idempotency_key": "bb".repeat(16)
    }))
    .unwrap();
    assert_eq!(
        invalid.with_device_request(|_| ()),
        Err(NetworkRequestError::InvalidLoraTransmitPower)
    );
    assert_eq!(
        NetworkRequestError::InvalidLoraTransmitPower.to_string(),
        "LoRa transmit power must be one of 14, 17, 20, or 22 dBm"
    );
}

#[test]
fn lora_profile_mutation_preserves_one_atomic_tuple() {
    let ts_config = ts_rs::Config::default();
    assert!(LoraRadioProfileView::decl(&ts_config).contains("frequency_hz: number"));
    assert!(NetworkConfigMutation::decl(&ts_config).contains("set_lora_profile"));

    let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
        "mutation": {
            "kind": "set_lora_profile",
            "profile": {
                "frequency_hz": 914_875_000,
                "bandwidth_hz": 250_000,
                "spreading_factor": 9,
                "coding_rate_denominator": 7,
                "tx_power_dbm": 22
            }
        },
        "expected_revision": 16,
        "idempotency_key": "bc".repeat(16)
    }))
    .unwrap();
    let observed = request
        .with_device_request(|request| match request.mutation() {
            device_api::NetworkConfigMutation::SetLoraProfile(profile) => (
                profile.frequency_hz(),
                profile.bandwidth_hz(),
                profile.spreading_factor(),
                profile.coding_rate_denominator(),
                profile.tx_power_dbm().get(),
            ),
            _ => panic!("expected LoRa profile mutation"),
        })
        .unwrap();
    assert_eq!(observed, (914_875_000, 250_000, 9, 7, 22));
}

#[test]
fn tcp_peer_input_rejects_ambiguous_address_shapes() {
    assert!(
        serde_json::from_value::<ReticulumTcpPeerIpv4Input>(serde_json::json!({
            "enabled": true,
            "ipv4_address": "192.0.2.1",
            "hostname": "rmap.world",
            "port": 4242
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ReticulumTcpPeerHostnameInput>(serde_json::json!({
            "enabled": true,
            "ipv4_address": "192.0.2.1",
            "hostname": "rmap.world",
            "port": 4242
        }))
        .is_err()
    );
}
