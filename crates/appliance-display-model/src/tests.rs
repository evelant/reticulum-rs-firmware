use super::{
    DISPLAY_LABEL_CAPACITY, DisplayCommand, DisplayCompositionState, DisplayHomeSnapshot,
    DisplayLabel, DisplayLabelError, DisplaySetupState, DisplayState, DisplayViewKind,
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
fn home_snapshot_retains_an_optional_appliance_label() {
    let unnamed = home(DisplaySetupState::Enrolled);
    assert_eq!(unnamed.appliance_label(), None);

    let name = DisplayLabel::new("Field node").expect("fixture name fits");
    let named = unnamed.with_appliance_label(Some(name));
    assert_eq!(named.appliance_label(), Some(name));
    assert_eq!(named.device_suffix(), unnamed.device_suffix());
    assert_eq!(named.label(), unnamed.label());
}

#[test]
fn home_retains_enrollment_and_boot_composition_without_live_claims() {
    let snapshot = home(DisplaySetupState::EnrollmentRequired);
    assert_eq!(snapshot.label().as_str(), "Reticulum E290");
    assert_eq!(snapshot.device_suffix().as_str(), "e13f88");
    assert_eq!(snapshot.setup(), DisplaySetupState::EnrollmentRequired);
    assert_eq!(snapshot.lora(), DisplayCompositionState::Configured);
    assert_eq!(snapshot.ble(), DisplayCompositionState::Configured);
    assert_eq!(snapshot.lxmf(), DisplayCompositionState::Configured);
    assert_eq!(snapshot.nomad(), DisplayCompositionState::Configured);
    assert_eq!(snapshot.uncollected_messages(), 0);

    let enrolled = snapshot.with_setup(DisplaySetupState::Enrolled);
    assert_eq!(enrolled.setup(), DisplaySetupState::Enrolled);
    assert_eq!(enrolled.device_suffix(), snapshot.device_suffix());
    assert_eq!(enrolled.lora(), snapshot.lora());

    let with_mail = enrolled.with_uncollected_messages(123);
    assert_eq!(with_mail.uncollected_messages(), 123);
    assert_eq!(with_mail.setup(), DisplaySetupState::Enrolled);
    assert_eq!(with_mail.device_suffix(), snapshot.device_suffix());
}

#[test]
fn desired_views_replace_semantic_state() {
    let mut state = DisplayState::new();
    assert_eq!(state.view().kind(), DisplayViewKind::Blank);

    let booting = state.apply(DisplayCommand::ShowBooting { label: label() });
    assert_eq!(booting.previous(), DisplayViewKind::Blank);
    assert_eq!(booting.current(), DisplayViewKind::Booting);
    assert_eq!(state.view().label(), Some(&label()));

    let snapshot = home(DisplaySetupState::Enrolled);
    let ready = state.apply(DisplayCommand::ShowHome { snapshot });
    assert_eq!(ready.previous(), DisplayViewKind::Booting);
    assert_eq!(ready.current(), DisplayViewKind::Home);
    assert_eq!(state.view().home(), Some(snapshot));
}
