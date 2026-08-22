//! Allocation-free semantic display state for Reticulum appliances.
//!
//! This crate owns presentation meaning, not pixels or hardware. Producers
//! publish complete desired views so a slow e-paper actor may coalesce updates
//! without replaying intermediate commands.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::{fmt, str};

/// Maximum encoded UTF-8 bytes retained for one appliance display label.
pub const DISPLAY_LABEL_CAPACITY: usize = 32;
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

/// Local management enrollment state shown on the appliance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplaySetupState {
    /// No requester identity is authorized; physical-presence enrollment is required.
    EnrollmentRequired,
    /// At least one requester identity is durably authorized.
    Enrolled,
    /// The management authorization store was unavailable.
    Unavailable,
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
    appliance_label: Option<DisplayLabel>,
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
            appliance_label: None,
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

    /// Optional product-owned appliance label.
    pub const fn appliance_label(self) -> Option<DisplayLabel> {
        self.appliance_label
    }

    /// Return the same snapshot with an updated optional appliance label.
    pub const fn with_appliance_label(self, appliance_label: Option<DisplayLabel>) -> Self {
        Self {
            appliance_label,
            ..self
        }
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
}

/// Complete semantic presentation owned by the display state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
}

impl DisplayView {
    /// Return the non-secret semantic view kind.
    pub const fn kind(&self) -> DisplayViewKind {
        match self {
            Self::Blank => DisplayViewKind::Blank,
            Self::Booting { .. } => DisplayViewKind::Booting,
            Self::Home { .. } => DisplayViewKind::Home,
        }
    }

    /// Borrow the public appliance label when the view has one.
    pub const fn label(&self) -> Option<&DisplayLabel> {
        match self {
            Self::Blank => None,
            Self::Booting { label } => Some(label),
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
}

/// Complete desired-state command published to a coalescing display actor.
///
/// Each command contains enough public context to render independently. This
/// makes it safe for a latest-value handoff to discard older commands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
}

impl DisplayCommand {
    /// Return the complete non-secret view kind this command requests.
    pub const fn requested_view(&self) -> DisplayViewKind {
        match self {
            Self::ShowBooting { .. } => DisplayViewKind::Booting,
            Self::ShowHome { .. } => DisplayViewKind::Home,
        }
    }
}

/// Report for one applied display command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "display transitions report the previous and current view"]
pub struct DisplayTransition {
    previous: DisplayViewKind,
    current: DisplayViewKind,
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
}

/// Sole semantic display state.
pub struct DisplayState {
    view: DisplayView,
}

impl DisplayState {
    /// Construct boot state with no retained presentation.
    pub const fn new() -> Self {
        Self {
            view: DisplayView::Blank,
        }
    }

    /// Borrow the complete current semantic view.
    pub const fn view(&self) -> &DisplayView {
        &self.view
    }

    /// Apply one complete desired-state command.
    pub fn apply(&mut self, command: DisplayCommand) -> DisplayTransition {
        let previous = self.view.kind();
        self.view = match command {
            DisplayCommand::ShowBooting { label } => DisplayView::Booting { label },
            DisplayCommand::ShowHome { snapshot } => DisplayView::Home { snapshot },
        };
        DisplayTransition {
            previous,
            current: self.view.kind(),
        }
    }
}

impl Default for DisplayState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
