//! Allocation-free semantic display state for Reticulum appliances.
//!
//! This crate owns presentation meaning, not pixels or hardware. Producers
//! publish complete desired views so a slow e-paper actor may coalesce updates
//! without replaying intermediate commands. Pairing passkeys stay inside an
//! opaque validated owner and are zeroized whenever a view is cleared,
//! replaced, or dropped.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::{fmt, num::NonZeroU16, str};

use zeroize::Zeroize;

/// Maximum encoded UTF-8 bytes retained for one appliance display label.
pub const DISPLAY_LABEL_CAPACITY: usize = 32;
/// Largest relative pairing-window duration representable by the display ABI.
pub const MAX_PAIRING_WINDOW_SECONDS: u16 = u16::MAX;

const _: () = assert!(DISPLAY_LABEL_CAPACITY <= u8::MAX as usize);

/// A display label failed fixed-capacity or text validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayLabelError {
    /// The label was empty.
    Empty,
    /// The encoded UTF-8 label exceeded [`DISPLAY_LABEL_CAPACITY`].
    TooLong,
    /// The supplied byte slice was not valid UTF-8.
    InvalidUtf8,
    /// The label contained a control or separator unsuitable for one line.
    UnsupportedCharacter,
}

/// Fixed-capacity, validated UTF-8 text for one display line.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct DisplayLabel {
    bytes: [u8; DISPLAY_LABEL_CAPACITY],
    length: u8,
}

impl DisplayLabel {
    /// Validate and retain one single-line UTF-8 label.
    pub fn new(label: &str) -> Result<Self, DisplayLabelError> {
        Self::from_bytes(label.as_bytes())
    }

    /// Validate and retain UTF-8 bytes loaded from a bounded external source.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DisplayLabelError> {
        if bytes.is_empty() {
            return Err(DisplayLabelError::Empty);
        }
        if bytes.len() > DISPLAY_LABEL_CAPACITY {
            return Err(DisplayLabelError::TooLong);
        }
        let label = str::from_utf8(bytes).map_err(|_| DisplayLabelError::InvalidUtf8)?;
        if label
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
        {
            return Err(DisplayLabelError::UnsupportedCharacter);
        }

        let mut retained = [0; DISPLAY_LABEL_CAPACITY];
        retained[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            bytes: retained,
            length: bytes.len() as u8,
        })
    }

    /// Borrow the validated label.
    pub fn as_str(&self) -> &str {
        str::from_utf8(&self.bytes[..usize::from(self.length)])
            .expect("DisplayLabel construction validates UTF-8")
    }

    /// Return the encoded UTF-8 length.
    pub const fn len(&self) -> usize {
        self.length as usize
    }

    /// Report whether this validated label is empty.
    pub const fn is_empty(&self) -> bool {
        false
    }
}

impl fmt::Debug for DisplayLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DisplayLabel")
            .field(&self.as_str())
            .finish()
    }
}

impl fmt::Display for DisplayLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A six-digit pairing passkey failed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingPasskeyError {
    /// The ASCII representation did not contain exactly six bytes.
    InvalidLength,
    /// At least one byte was not an ASCII decimal digit.
    NonDecimalDigit,
    /// A numeric passkey exceeded the six-digit range.
    OutOfRange,
}

/// Opaque, validated six-digit pairing passkey.
///
/// The type deliberately implements neither `Copy`, `Clone`, nor `Debug`.
/// Renderers can borrow the six ASCII digits only inside
/// [`PairingPasskey::expose_ascii`]. The owner is zeroized on every drop,
/// including when a coalescing handoff overwrites a pending view.
///
/// ```compile_fail
/// use reticulum_appliance_display_model::PairingPasskey;
///
/// fn require_copy<T: Copy>() {}
/// require_copy::<PairingPasskey>();
/// ```
///
/// ```compile_fail
/// use reticulum_appliance_display_model::PairingPasskey;
///
/// let passkey = PairingPasskey::from_number(123_456).unwrap();
/// let _ = format!("{passkey:?}");
/// ```
pub struct PairingPasskey {
    digits: [u8; 6],
}

impl PairingPasskey {
    /// Validate an exact six-byte ASCII decimal representation.
    pub fn from_ascii(ascii: &[u8]) -> Result<Self, PairingPasskeyError> {
        let digits: [u8; 6] = ascii
            .try_into()
            .map_err(|_| PairingPasskeyError::InvalidLength)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(PairingPasskeyError::NonDecimalDigit);
        }
        Ok(Self { digits })
    }

    /// Format a numeric passkey as six digits, retaining leading zeroes.
    pub fn from_number(number: u32) -> Result<Self, PairingPasskeyError> {
        if number > 999_999 {
            return Err(PairingPasskeyError::OutOfRange);
        }
        let mut remainder = number;
        let mut digits = [b'0'; 6];
        let mut index = digits.len();
        while index > 0 {
            index -= 1;
            digits[index] += (remainder % 10) as u8;
            remainder /= 10;
        }
        Ok(Self { digits })
    }

    /// Borrow the exact six ASCII digits for the duration of one callback.
    ///
    /// The callback must not retain or log a copy. A hardware renderer should
    /// consume the borrowed digits directly while composing its private
    /// framebuffer.
    pub fn expose_ascii(&self, inspect: impl FnOnce(&[u8; 6])) {
        inspect(&self.digits);
    }

    fn clear(&mut self) {
        self.digits.zeroize();
    }
}

impl Drop for PairingPasskey {
    fn drop(&mut self) {
        self.clear();
    }
}

/// A relative pairing-window duration must be nonzero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingWindowSecondsError;

/// Nonzero relative pairing-window duration for presentation.
///
/// The producer remains the timeout authority. This bounded scalar only lets
/// the display render the window associated with a complete pairing view; the
/// display state never expires a secret without an explicit terminal command.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PairingWindowSeconds(NonZeroU16);

impl PairingWindowSeconds {
    /// Validate a nonzero duration of at most [`MAX_PAIRING_WINDOW_SECONDS`].
    pub const fn new(seconds: u16) -> Result<Self, PairingWindowSecondsError> {
        match NonZeroU16::new(seconds) {
            Some(seconds) => Ok(Self(seconds)),
            None => Err(PairingWindowSecondsError),
        }
    }

    /// Return the relative duration in whole seconds.
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// Whether one boot-composed appliance capability is present in the image.
///
/// This is deliberately not a live health or task-spawn state. A `Configured`
/// capability records that its prerequisites were present in the composed boot
/// graph; its actor may not have spawned yet and may later fault. Live actor
/// telemetry belongs to a future display coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayCompositionState {
    /// The capability's prerequisites were present in the composed boot graph.
    Configured,
    /// The capability was unavailable when the boot graph was composed.
    Unavailable,
}

/// Local app setup represented by credential authority, pairing admission,
/// and an explicitly recoverable BLE transport.
///
/// A Bluetooth bond alone never selects `Paired`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplaySetupState {
    /// No active application credential exists and setup is required. This
    /// includes a valid empty authority and pre-authority media for which the
    /// resident pairing policy can initialize or recover credentials.
    PairingRequired,
    /// Application credentials remain configured, but the BLE bearer has no
    /// durable bond and is advertising only for explicit phone recovery.
    BluetoothRecoveryRequired,
    /// The publishable authority contains at least one active application
    /// credential and no boot-composed BLE recovery is outstanding.
    Paired,
    /// No active authority or resident pairing path was available.
    Unavailable,
}

impl DisplaySetupState {
    /// Compose setup from application credential authority and resident pairing
    /// admission.
    ///
    /// A valid empty authority is always setup-required. Without an authority,
    /// the pairing policy distinguishes a fresh or recoverable appliance from
    /// a genuinely unavailable local API.
    pub const fn from_application_state(
        active_credential_count: Option<usize>,
        pairing_policy_available: bool,
    ) -> Self {
        match active_credential_count {
            Some(0) => Self::PairingRequired,
            Some(_) => Self::Paired,
            None if pairing_policy_available => Self::PairingRequired,
            None => Self::Unavailable,
        }
    }

    /// Refine configured application state with the boot BLE bond projection.
    ///
    /// This never turns a fresh application setup into a transport-only
    /// recovery and never hides an unavailable credential authority.
    pub const fn with_ble_bond(self, ble_available: bool, durable_bond_present: bool) -> Self {
        if matches!(self, Self::Paired) && ble_available && !durable_bond_present {
            Self::BluetoothRecoveryRequired
        } else {
            self
        }
    }
}

/// Complete non-secret snapshot for the appliance Home view.
///
/// The physical suffix is the exact six-character suffix used by the
/// board-specific discovery card. Capability fields describe boot
/// composition, not task-spawn completion, live actor connectivity, or
/// health. The uncollected-message count is live mailbox presentation state;
/// it contains no sender or message content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayHomeSnapshot {
    label: DisplayLabel,
    device_suffix: DisplayLabel,
    setup: DisplaySetupState,
    lora: DisplayCompositionState,
    ble: DisplayCompositionState,
    lxmf: DisplayCompositionState,
    nomad: DisplayCompositionState,
    uncollected_messages: u32,
}

impl DisplayHomeSnapshot {
    /// Construct one complete Home snapshot with an empty mailbox indicator.
    pub const fn new(
        label: DisplayLabel,
        device_suffix: DisplayLabel,
        setup: DisplaySetupState,
        lora: DisplayCompositionState,
        ble: DisplayCompositionState,
        lxmf: DisplayCompositionState,
        nomad: DisplayCompositionState,
    ) -> Self {
        Self {
            label,
            device_suffix,
            setup,
            lora,
            ble,
            lxmf,
            nomad,
            uncollected_messages: 0,
        }
    }

    /// Public appliance product label.
    pub const fn label(self) -> DisplayLabel {
        self.label
    }

    /// Exact board suffix shown by discovery clients.
    pub const fn device_suffix(self) -> DisplayLabel {
        self.device_suffix
    }

    /// Application-level setup state.
    pub const fn setup(self) -> DisplaySetupState {
        self.setup
    }

    /// Boot-composed LoRa capability.
    pub const fn lora(self) -> DisplayCompositionState {
        self.lora
    }

    /// Boot-composed BLE device-API capability.
    pub const fn ble(self) -> DisplayCompositionState {
        self.ble
    }

    /// Boot-composed LXMF capability.
    pub const fn lxmf(self) -> DisplayCompositionState {
        self.lxmf
    }

    /// Boot-composed Nomad capability.
    pub const fn nomad(self) -> DisplayCompositionState {
        self.nomad
    }

    /// Messages durably retained by the appliance but not yet acknowledged as
    /// collected by its app.
    pub const fn uncollected_messages(self) -> u32 {
        self.uncollected_messages
    }

    /// Return the same composition snapshot with an updated authoritative
    /// application setup state.
    pub const fn with_setup(self, setup: DisplaySetupState) -> Self {
        Self { setup, ..self }
    }

    /// Return the same snapshot with updated live mailbox presentation state.
    ///
    /// Renderers may cap the displayed value, but the semantic snapshot keeps
    /// the complete count so changes above that visual cap still deduplicate
    /// correctly.
    pub const fn with_uncollected_messages(self, uncollected_messages: u32) -> Self {
        Self {
            uncollected_messages,
            ..self
        }
    }
}

/// Non-secret semantic view kind suitable for diagnostics and assertions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayViewKind {
    /// No retained presentation; the panel should be explicitly cleared.
    Blank,
    /// The appliance is starting.
    Booting,
    /// The appliance boot-composition Home snapshot is visible.
    Home,
    /// A pairing passkey is currently visible.
    Pairing,
    /// Pairing failed.
    PairingFailed,
    /// The pairing window expired.
    PairingTimedOut,
}

/// Complete semantic presentation owned by the display state machine.
///
/// This type deliberately has no `Debug` or cloning implementation because
/// one variant owns a pairing secret.
pub enum DisplayView {
    /// Clear the retained e-paper presentation.
    Blank,
    /// Show appliance startup progress.
    Booting {
        /// Public appliance label.
        label: DisplayLabel,
    },
    /// Show the non-secret boot-composition Home snapshot.
    Home {
        /// Complete snapshot rendered without claiming live actor telemetry.
        snapshot: DisplayHomeSnapshot,
    },
    /// Show a temporary pairing passkey.
    Pairing {
        /// Public appliance label.
        label: DisplayLabel,
        /// Private six-digit passkey owner.
        passkey: PairingPasskey,
        /// Bounded relative pairing window rendered with the passkey.
        expires_after_seconds: PairingWindowSeconds,
    },
    /// Show failed pairing without retaining the passkey.
    PairingFailed {
        /// Public appliance label.
        label: DisplayLabel,
    },
    /// Show pairing timeout without retaining the passkey.
    PairingTimedOut {
        /// Public appliance label.
        label: DisplayLabel,
    },
}

impl DisplayView {
    /// Return the non-secret semantic view kind.
    pub const fn kind(&self) -> DisplayViewKind {
        match self {
            Self::Blank => DisplayViewKind::Blank,
            Self::Booting { .. } => DisplayViewKind::Booting,
            Self::Home { .. } => DisplayViewKind::Home,
            Self::Pairing { .. } => DisplayViewKind::Pairing,
            Self::PairingFailed { .. } => DisplayViewKind::PairingFailed,
            Self::PairingTimedOut { .. } => DisplayViewKind::PairingTimedOut,
        }
    }

    /// Borrow the public appliance label when the view has one.
    pub const fn label(&self) -> Option<&DisplayLabel> {
        match self {
            Self::Blank => None,
            Self::Booting { label }
            | Self::Pairing { label, .. }
            | Self::PairingFailed { label }
            | Self::PairingTimedOut { label } => Some(label),
            Self::Home { snapshot } => Some(&snapshot.label),
        }
    }

    /// Return the complete Home snapshot when that view is active.
    pub const fn home(&self) -> Option<DisplayHomeSnapshot> {
        match self {
            Self::Home { snapshot } => Some(*snapshot),
            _ => None,
        }
    }

    /// Borrow a pairing passkey only for the duration of one callback.
    pub fn expose_pairing_passkey(&self, inspect: impl FnOnce(Option<&[u8; 6]>)) {
        match self {
            Self::Pairing { passkey, .. } => passkey.expose_ascii(|digits| inspect(Some(digits))),
            _ => inspect(None),
        }
    }

    /// Return the relative pairing-window duration for a pairing view.
    pub const fn pairing_window(&self) -> Option<PairingWindowSeconds> {
        match self {
            Self::Pairing {
                expires_after_seconds,
                ..
            } => Some(*expires_after_seconds),
            _ => None,
        }
    }

    const fn owns_pairing_secret(&self) -> bool {
        matches!(self, Self::Pairing { .. })
    }
}

/// Terminal reason that must remove a visible pairing passkey.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingSecretClearReason {
    /// The bounded pairing window expired.
    TimedOut,
    /// Pairing failed or was refused.
    Failed,
    /// The appliance is preparing to reboot.
    Reboot,
}

/// Complete desired-state command published to a coalescing display actor.
///
/// Each command contains enough public context to render independently. This
/// makes it safe for a latest-value handoff to discard older commands.
pub enum DisplayCommand {
    /// Replace the current presentation with startup progress.
    ShowBooting {
        /// Public appliance label.
        label: DisplayLabel,
    },
    /// Replace the current presentation with the boot-composition Home view.
    ShowHome {
        /// Complete non-secret Home snapshot.
        snapshot: DisplayHomeSnapshot,
    },
    /// Install or replace the temporary pairing passkey.
    ShowPairing {
        /// Public appliance label.
        label: DisplayLabel,
        /// Sole private passkey owner transferred to the display state.
        passkey: PairingPasskey,
        /// Bounded relative pairing window rendered with the passkey.
        expires_after_seconds: PairingWindowSeconds,
    },
    /// Clear any pairing passkey and show the reason-specific terminal state.
    ClearPairingSecret {
        /// Public appliance label retained for non-reboot terminal views.
        label: DisplayLabel,
        /// Reason the secret must leave both semantic state and panel content.
        reason: PairingSecretClearReason,
    },
}

impl DisplayCommand {
    /// Return the complete non-secret view kind this command requests.
    pub const fn requested_view(&self) -> DisplayViewKind {
        match self {
            Self::ShowBooting { .. } => DisplayViewKind::Booting,
            Self::ShowHome { .. } => DisplayViewKind::Home,
            Self::ShowPairing { .. } => DisplayViewKind::Pairing,
            Self::ClearPairingSecret {
                reason: PairingSecretClearReason::TimedOut,
                ..
            } => DisplayViewKind::PairingTimedOut,
            Self::ClearPairingSecret {
                reason: PairingSecretClearReason::Failed,
                ..
            } => DisplayViewKind::PairingFailed,
            Self::ClearPairingSecret {
                reason: PairingSecretClearReason::Reboot,
                ..
            } => DisplayViewKind::Blank,
        }
    }
}

/// Observable secret ownership change caused by one state transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingSecretDisposition {
    /// No passkey owner entered or left semantic display state.
    Unchanged,
    /// A passkey entered a previously non-secret view.
    Installed,
    /// A prior passkey was zeroized and replaced by a new owner.
    Replaced,
    /// A passkey was zeroized for the named terminal reason.
    Cleared(PairingSecretClearReason),
    /// A non-pairing view replaced and zeroized the prior passkey.
    ClearedByViewReplacement,
}

/// Non-secret report for one applied display command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "display transitions report whether a secret owner changed"]
pub struct DisplayTransition {
    previous: DisplayViewKind,
    current: DisplayViewKind,
    secret: PairingSecretDisposition,
}

impl DisplayTransition {
    /// View kind before the command.
    pub const fn previous(self) -> DisplayViewKind {
        self.previous
    }

    /// View kind after the command.
    pub const fn current(self) -> DisplayViewKind {
        self.current
    }

    /// Pairing-secret ownership change performed by the command.
    pub const fn secret_disposition(self) -> PairingSecretDisposition {
        self.secret
    }
}

/// Sole semantic display state with explicit pairing-secret lifecycle.
///
/// A boot always begins blank, so a retained e-paper passkey cannot be treated
/// as current state before the hardware actor performs an explicit refresh.
pub struct DisplayState {
    view: DisplayView,
}

impl DisplayState {
    /// Construct boot state with no retained passkey or label.
    pub const fn new() -> Self {
        Self {
            view: DisplayView::Blank,
        }
    }

    /// Borrow the complete current semantic view.
    pub const fn view(&self) -> &DisplayView {
        &self.view
    }

    /// Report whether semantic state currently owns a pairing passkey.
    pub const fn owns_pairing_secret(&self) -> bool {
        self.view.owns_pairing_secret()
    }

    /// Apply one complete desired-state command.
    ///
    /// Assigning a replacement view drops and zeroizes any old passkey before
    /// this transition returns.
    pub fn apply(&mut self, command: DisplayCommand) -> DisplayTransition {
        let previous = self.view.kind();
        let previously_secret = self.view.owns_pairing_secret();
        let (view, secret) = match command {
            DisplayCommand::ShowBooting { label } => (
                DisplayView::Booting { label },
                if previously_secret {
                    PairingSecretDisposition::ClearedByViewReplacement
                } else {
                    PairingSecretDisposition::Unchanged
                },
            ),
            DisplayCommand::ShowHome { snapshot } => (
                DisplayView::Home { snapshot },
                if previously_secret {
                    PairingSecretDisposition::ClearedByViewReplacement
                } else {
                    PairingSecretDisposition::Unchanged
                },
            ),
            DisplayCommand::ShowPairing {
                label,
                passkey,
                expires_after_seconds,
            } => (
                DisplayView::Pairing {
                    label,
                    passkey,
                    expires_after_seconds,
                },
                if previously_secret {
                    PairingSecretDisposition::Replaced
                } else {
                    PairingSecretDisposition::Installed
                },
            ),
            DisplayCommand::ClearPairingSecret { label, reason } => {
                let view = match reason {
                    PairingSecretClearReason::TimedOut => DisplayView::PairingTimedOut { label },
                    PairingSecretClearReason::Failed => DisplayView::PairingFailed { label },
                    PairingSecretClearReason::Reboot => DisplayView::Blank,
                };
                (
                    view,
                    if previously_secret {
                        PairingSecretDisposition::Cleared(reason)
                    } else {
                        PairingSecretDisposition::Unchanged
                    },
                )
            }
        };
        self.view = view;
        DisplayTransition {
            previous,
            current: self.view.kind(),
            secret,
        }
    }
}

impl Default for DisplayState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
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
        let maximum =
            PairingWindowSeconds::new(u16::MAX).expect("the complete u16 range is bounded");
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
}
