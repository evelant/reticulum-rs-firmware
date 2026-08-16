use super::{
    DISPLAY_LABEL_CAPACITY, DisplayCommand, DisplayCompositionState, DisplayHomeSnapshot,
    DisplayLabel, DisplayLabelError, DisplaySetupState, DisplayState, DisplayViewKind,
    PairingPasskey, PairingPasskeyError, PairingSecretClearReason, PairingSecretDisposition,
    PairingWindowSeconds, PairingWindowSecondsError,
};

fn label() -> DisplayLabel {
    DisplayLabel::new("Reticulum E290").expect("test label must fit")
}

fn home(setup: DisplaySetupState) -> DisplayHomeSnapshot {
    DisplayHomeSnapshot::new(
        label(),
        DisplayLabel::new("e13f88").expect("test suffix fits"),
        setup,
        DisplayCompositionState::Configured,
        DisplayCompositionState::Configured,
        DisplayCompositionState::Configured,
        DisplayCompositionState::Configured,
    )
}

fn show_pairing(number: u32) -> DisplayCommand {
    DisplayCommand::ShowPairing {
        label: label(),
        passkey: PairingPasskey::from_number(number).expect("test passkey must fit"),
        expires_after_seconds: PairingWindowSeconds::new(60)
            .expect("test pairing window must be nonzero"),
    }
}

#[test]
fn labels_are_bounded_valid_single_line_utf8() {
    let unicode = DisplayLabel::new("Reticulum mesh").expect("valid label");
    assert_eq!(unicode.as_str(), "Reticulum mesh");
    assert_eq!(unicode.len(), 14);
    assert!(!unicode.is_empty());

    assert_eq!(DisplayLabel::new(""), Err(DisplayLabelError::Empty));
    assert_eq!(
        DisplayLabel::new("line one\nline two"),
        Err(DisplayLabelError::UnsupportedCharacter)
    );
    assert_eq!(
        DisplayLabel::new("line one\u{2028}line two"),
        Err(DisplayLabelError::UnsupportedCharacter)
    );
    assert_eq!(
        DisplayLabel::new("line one\u{2029}line two"),
        Err(DisplayLabelError::UnsupportedCharacter)
    );
    assert_eq!(
        DisplayLabel::new("x".repeat(DISPLAY_LABEL_CAPACITY + 1).as_str()),
        Err(DisplayLabelError::TooLong)
    );
    assert_eq!(
        DisplayLabel::from_bytes(&[0xff]),
        Err(DisplayLabelError::InvalidUtf8)
    );
}

#[test]
fn home_snapshot_retains_an_optional_device_name() {
    let unnamed = home(DisplaySetupState::Paired);
    assert_eq!(unnamed.device_name(), None);

    let name = DisplayLabel::new("Field node").expect("fixture name fits");
    let named = unnamed.with_device_name(Some(name));
    assert_eq!(named.device_name(), Some(name));
    assert_eq!(named.device_suffix(), unnamed.device_suffix());
    assert_eq!(named.label(), unnamed.label());
}

#[test]
fn passkeys_require_exactly_six_decimal_digits_and_keep_leading_zeroes() {
    assert_eq!(
        PairingPasskey::from_ascii(b"12345").err(),
        Some(PairingPasskeyError::InvalidLength)
    );
    assert_eq!(
        PairingPasskey::from_ascii(b"12345x").err(),
        Some(PairingPasskeyError::NonDecimalDigit)
    );
    assert_eq!(
        PairingPasskey::from_number(1_000_000).err(),
        Some(PairingPasskeyError::OutOfRange)
    );

    let ascii = PairingPasskey::from_ascii(b"012345").expect("valid passkey");
    ascii.expose_ascii(|digits| assert_eq!(digits, b"012345"));
    let numeric = PairingPasskey::from_number(42).expect("valid passkey");
    numeric.expose_ascii(|digits| assert_eq!(digits, b"000042"));
}

#[test]
fn pairing_window_is_nonzero_bounded_and_retained_with_the_secret_view() {
    assert_eq!(PairingWindowSeconds::new(0), Err(PairingWindowSecondsError));
    let maximum = PairingWindowSeconds::new(u16::MAX).expect("the complete u16 range is bounded");
    assert_eq!(maximum.get(), u16::MAX);

    let mut state = DisplayState::new();
    let _ = state.apply(show_pairing(123_456));
    assert_eq!(
        state.view().pairing_window(),
        Some(PairingWindowSeconds::new(60).expect("nonzero window"))
    );
}

#[test]
fn passkey_zeroization_clears_every_owned_digit() {
    let mut passkey = PairingPasskey::from_ascii(b"987654").expect("valid passkey");
    passkey.clear();
    assert_eq!(passkey.digits, [0; 6]);
}

#[test]
fn a_new_passkey_explicitly_replaces_the_prior_secret_owner() {
    let mut state = DisplayState::new();
    let installed = state.apply(show_pairing(111_111));
    assert_eq!(
        installed.secret_disposition(),
        PairingSecretDisposition::Installed
    );
    let replaced = state.apply(show_pairing(222_222));
    assert_eq!(
        replaced.secret_disposition(),
        PairingSecretDisposition::Replaced
    );
    state
        .view()
        .expose_pairing_passkey(|digits| assert_eq!(digits, Some(b"222222")));
}

#[test]
fn home_retains_the_exact_selector_and_boot_composition_without_live_claims() {
    let snapshot = home(DisplaySetupState::PairingRequired);
    assert_eq!(snapshot.label().as_str(), "Reticulum E290");
    assert_eq!(snapshot.device_suffix().as_str(), "e13f88");
    assert_eq!(snapshot.setup(), DisplaySetupState::PairingRequired);
    assert_eq!(snapshot.lora(), DisplayCompositionState::Configured);
    assert_eq!(snapshot.ble(), DisplayCompositionState::Configured);
    assert_eq!(snapshot.lxmf(), DisplayCompositionState::Configured);
    assert_eq!(snapshot.nomad(), DisplayCompositionState::Configured);
    assert_eq!(snapshot.uncollected_messages(), 0);

    let paired = snapshot.with_setup(DisplaySetupState::Paired);
    assert_eq!(paired.setup(), DisplaySetupState::Paired);
    assert_eq!(paired.device_suffix(), snapshot.device_suffix());
    assert_eq!(paired.lora(), snapshot.lora());

    let with_mail = paired.with_uncollected_messages(123);
    assert_eq!(with_mail.uncollected_messages(), 123);
    assert_eq!(with_mail.setup(), DisplaySetupState::Paired);
    assert_eq!(with_mail.device_suffix(), snapshot.device_suffix());
}

#[test]
fn setup_composition_distinguishes_fresh_pairing_from_unavailable_authority() {
    assert_eq!(
        DisplaySetupState::from_application_state(Some(0), false),
        DisplaySetupState::PairingRequired
    );
    assert_eq!(
        DisplaySetupState::from_application_state(Some(2), true),
        DisplaySetupState::Paired
    );
    assert_eq!(
        DisplaySetupState::from_application_state(None, true),
        DisplaySetupState::PairingRequired
    );
    assert_eq!(
        DisplaySetupState::from_application_state(None, false),
        DisplaySetupState::Unavailable
    );
    assert_eq!(
        DisplaySetupState::Paired.with_ble_bond(true, false),
        DisplaySetupState::BluetoothRecoveryRequired
    );
    assert_eq!(
        DisplaySetupState::PairingRequired.with_ble_bond(true, false),
        DisplaySetupState::PairingRequired
    );
    assert_eq!(
        DisplaySetupState::Paired.with_ble_bond(true, true),
        DisplaySetupState::Paired
    );
    assert_eq!(
        DisplaySetupState::Paired.with_ble_bond(false, false),
        DisplaySetupState::Paired
    );
}

#[test]
fn every_terminal_pairing_path_clears_the_secret_and_selects_a_safe_view() {
    for (reason, expected) in [
        (
            PairingSecretClearReason::TimedOut,
            DisplayViewKind::PairingTimedOut,
        ),
        (
            PairingSecretClearReason::Failed,
            DisplayViewKind::PairingFailed,
        ),
        (PairingSecretClearReason::Reboot, DisplayViewKind::Blank),
    ] {
        let mut state = DisplayState::new();
        let _ = state.apply(show_pairing(123_456));
        let transition = state.apply(DisplayCommand::ClearPairingSecret {
            label: label(),
            reason,
        });
        assert_eq!(transition.previous(), DisplayViewKind::Pairing);
        assert_eq!(transition.current(), expected);
        assert_eq!(
            transition.secret_disposition(),
            PairingSecretDisposition::Cleared(reason)
        );
        assert!(!state.owns_pairing_secret());
        state
            .view()
            .expose_pairing_passkey(|digits| assert_eq!(digits, None));
    }
}

#[test]
fn home_replacement_clears_the_secret_and_can_report_durable_pairing() {
    let mut state = DisplayState::new();
    let _ = state.apply(show_pairing(654_321));
    let paired = home(DisplaySetupState::Paired);
    let transition = state.apply(DisplayCommand::ShowHome { snapshot: paired });
    assert_eq!(transition.current(), DisplayViewKind::Home);
    assert_eq!(
        transition.secret_disposition(),
        PairingSecretDisposition::ClearedByViewReplacement
    );
    assert!(!state.owns_pairing_secret());
    assert_eq!(state.view().home(), Some(paired));
}

#[test]
fn reboot_state_starts_blank_without_a_secret() {
    let state = DisplayState::new();
    assert_eq!(state.view().kind(), DisplayViewKind::Blank);
    assert!(!state.owns_pairing_secret());
}
