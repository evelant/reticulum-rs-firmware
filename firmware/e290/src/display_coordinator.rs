//! Semantic coordination in front of the slow E290 e-paper actor.
//!
//! The coordinator owns the complete Home snapshot. Producers update narrow
//! fields instead of publishing competing full frames, so mailbox activity
//! cannot erase BLE recovery state and ordinary Home changes cannot replace a
//! pairing passkey. The owner of this value must sit at the sole downstream
//! rendering boundary so no producer can bypass its priority rules.

use reticulum_appliance_display_model::{
    DisplayCommand, DisplayHomeSnapshot, DisplayLabel, DisplaySetupState, PairingPasskey,
    PairingSecretClearReason, PairingWindowSeconds,
};

/// Minimum interval between successive non-terminal mailbox-count refreshes.
///
/// The first new message and the transition back to zero bypass this interval.
/// Intermediate nonzero changes are coalesced because a full e-paper refresh
/// is expensive and the exact count is still retained in semantic state.
pub const MESSAGE_BURST_COALESCE_MILLIS: u64 = 5_000;

/// Result of applying one semantic update to the display coordinator.
///
/// A deferred update carries a timer only when polling at that instant can
/// produce a Home render. `None` means a higher-priority view must explicitly
/// return to Home before the retained update may be published.
pub enum DisplayCoordinatorOutput {
    /// Publish this complete desired state to the physical display handoff.
    Publish(DisplayCommand),
    /// The latest Home state is retained but should not yet be refreshed.
    Deferred {
        /// Monotonic instant at which [`E290DisplayCoordinator::poll`] should
        /// next be called, or `None` while a higher-priority view is active.
        refresh_at_ms: Option<u64>,
    },
    /// The update did not change the visible or retained presentation.
    Unchanged,
}

impl DisplayCoordinatorOutput {
    /// Consume the result and return its complete display command, if any.
    pub fn into_command(self) -> Option<DisplayCommand> {
        match self {
            Self::Publish(command) => Some(command),
            Self::Deferred { .. } | Self::Unchanged => None,
        }
    }

    /// Return the monotonic refresh deadline for a coalesced Home update.
    pub const fn refresh_at_ms(&self) -> Option<u64> {
        match self {
            Self::Deferred { refresh_at_ms } => *refresh_at_ms,
            Self::Publish(_) | Self::Unchanged => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisiblePresentation {
    None,
    Blank,
    Booting(DisplayLabel),
    Home,
    Pairing,
    PairingFailed(DisplayLabel),
    PairingTimedOut(DisplayLabel),
}

/// Sole owner of complete semantic state for the E290 display.
///
/// Pairing, pairing terminal states, and booting are protected presentations:
/// Home-field changes remain retained but cannot replace them. Call
/// [`E290DisplayCoordinator::resume_home`] only when the protected lifecycle
/// has actually ended.
pub struct E290DisplayCoordinator {
    home: DisplayHomeSnapshot,
    visible: VisiblePresentation,
    last_requested_home: Option<DisplayHomeSnapshot>,
    last_home_request_ms: Option<u64>,
    pending_home: bool,
    refresh_at_ms: Option<u64>,
}

impl E290DisplayCoordinator {
    /// Construct a coordinator with a retained Home snapshot and no assumed
    /// physical presentation.
    pub const fn new(home: DisplayHomeSnapshot) -> Self {
        Self {
            home,
            visible: VisiblePresentation::None,
            last_requested_home: None,
            last_home_request_ms: None,
            pending_home: true,
            refresh_at_ms: None,
        }
    }

    /// Return the complete retained Home snapshot.
    pub const fn home(&self) -> DisplayHomeSnapshot {
        self.home
    }

    /// Request boot progress unless the same boot view is already desired.
    pub fn show_booting(&mut self, label: DisplayLabel) -> DisplayCoordinatorOutput {
        if self.visible == VisiblePresentation::Booting(label) {
            return DisplayCoordinatorOutput::Unchanged;
        }
        self.visible = VisiblePresentation::Booting(label);
        self.defer_home_behind_protected_view();
        DisplayCoordinatorOutput::Publish(DisplayCommand::ShowBooting { label })
    }

    /// Request a pairing passkey as a protected presentation.
    ///
    /// Passkeys are intentionally never deduplicated. Ownership of every
    /// supplied secret transfers to the returned command and then to the
    /// display state or is zeroized when that command is dropped.
    pub fn show_pairing(
        &mut self,
        label: DisplayLabel,
        passkey: PairingPasskey,
        expires_after_seconds: PairingWindowSeconds,
    ) -> DisplayCoordinatorOutput {
        self.visible = VisiblePresentation::Pairing;
        self.defer_home_behind_protected_view();
        DisplayCoordinatorOutput::Publish(DisplayCommand::ShowPairing {
            label,
            passkey,
            expires_after_seconds,
        })
    }

    /// Clear any pairing passkey and install its protected terminal view.
    pub fn clear_pairing_secret(
        &mut self,
        label: DisplayLabel,
        reason: PairingSecretClearReason,
    ) -> DisplayCoordinatorOutput {
        let next = match reason {
            PairingSecretClearReason::TimedOut => VisiblePresentation::PairingTimedOut(label),
            PairingSecretClearReason::Failed => VisiblePresentation::PairingFailed(label),
            PairingSecretClearReason::Reboot => VisiblePresentation::Blank,
        };
        if self.visible == next && self.visible != VisiblePresentation::Pairing {
            return DisplayCoordinatorOutput::Unchanged;
        }
        self.visible = next;
        self.defer_home_behind_protected_view();
        DisplayCoordinatorOutput::Publish(DisplayCommand::ClearPairingSecret { label, reason })
    }

    /// Explicitly leave a protected lifecycle and publish the latest complete
    /// Home state.
    pub fn resume_home(&mut self, now_ms: u64) -> DisplayCoordinatorOutput {
        if self.visible == VisiblePresentation::Home
            && self
                .last_requested_home
                .is_some_and(|last| same_rendered_home(last, self.home))
        {
            self.pending_home = false;
            self.refresh_at_ms = None;
            return DisplayCoordinatorOutput::Unchanged;
        }
        self.request_home(now_ms)
    }

    /// Update only the authoritative setup/recovery field.
    ///
    /// This narrow mutation prevents mailbox updates from accidentally
    /// replacing `BluetoothRecoveryRequired` with a stale full snapshot.
    pub fn set_setup(&mut self, setup: DisplaySetupState, now_ms: u64) -> DisplayCoordinatorOutput {
        if self.home.setup() == setup {
            return DisplayCoordinatorOutput::Unchanged;
        }
        self.home = self.home.with_setup(setup);
        self.home_changed_immediately(now_ms)
    }

    /// Update the count of messages not yet acknowledged as collected.
    ///
    /// `0 -> nonzero` renders immediately so the first waiting message is
    /// visible. `nonzero -> 0` also renders immediately to clear the badge.
    /// Further nonzero changes are retained and coalesced.
    pub fn set_uncollected_messages(
        &mut self,
        count: u32,
        now_ms: u64,
    ) -> DisplayCoordinatorOutput {
        let previous = self.home.uncollected_messages();
        if previous == count {
            return DisplayCoordinatorOutput::Unchanged;
        }

        self.home = self.home.with_uncollected_messages(count);
        if self
            .last_requested_home
            .is_some_and(|last| same_rendered_home(last, self.home))
        {
            self.pending_home = false;
            self.refresh_at_ms = None;
            return DisplayCoordinatorOutput::Unchanged;
        }
        self.pending_home = true;

        if self.visible != VisiblePresentation::Home {
            self.refresh_at_ms = None;
            return DisplayCoordinatorOutput::Deferred {
                refresh_at_ms: None,
            };
        }

        if previous == 0 || count == 0 {
            return self.request_home(now_ms);
        }

        let deadline = self
            .refresh_at_ms
            .unwrap_or_else(|| self.next_mailbox_refresh(now_ms));
        if now_ms >= deadline {
            self.request_home(now_ms)
        } else {
            self.refresh_at_ms = Some(deadline);
            DisplayCoordinatorOutput::Deferred {
                refresh_at_ms: Some(deadline),
            }
        }
    }

    /// Publish a coalesced Home change once its monotonic deadline arrives.
    pub fn poll(&mut self, now_ms: u64) -> DisplayCoordinatorOutput {
        if !self.pending_home || self.visible != VisiblePresentation::Home {
            return DisplayCoordinatorOutput::Unchanged;
        }
        match self.refresh_at_ms {
            Some(deadline) if now_ms >= deadline => self.request_home(now_ms),
            Some(deadline) => DisplayCoordinatorOutput::Deferred {
                refresh_at_ms: Some(deadline),
            },
            None => DisplayCoordinatorOutput::Unchanged,
        }
    }

    fn home_changed_immediately(&mut self, now_ms: u64) -> DisplayCoordinatorOutput {
        self.pending_home = true;
        if self.visible == VisiblePresentation::Home {
            self.request_home(now_ms)
        } else {
            self.refresh_at_ms = None;
            DisplayCoordinatorOutput::Deferred {
                refresh_at_ms: None,
            }
        }
    }

    fn request_home(&mut self, now_ms: u64) -> DisplayCoordinatorOutput {
        self.visible = VisiblePresentation::Home;
        self.last_requested_home = Some(self.home);
        self.last_home_request_ms = Some(now_ms);
        self.pending_home = false;
        self.refresh_at_ms = None;
        DisplayCoordinatorOutput::Publish(DisplayCommand::ShowHome {
            snapshot: self.home,
        })
    }

    fn next_mailbox_refresh(&self, now_ms: u64) -> u64 {
        self.last_home_request_ms
            .map_or(now_ms, |last| {
                last.saturating_add(MESSAGE_BURST_COALESCE_MILLIS)
            })
            .max(now_ms)
    }

    fn defer_home_behind_protected_view(&mut self) {
        self.pending_home = self
            .last_requested_home
            .is_none_or(|last| !same_rendered_home(last, self.home));
        self.refresh_at_ms = None;
    }
}

fn same_rendered_home(left: DisplayHomeSnapshot, right: DisplayHomeSnapshot) -> bool {
    normalized_home(left) == normalized_home(right)
}

fn normalized_home(snapshot: DisplayHomeSnapshot) -> DisplayHomeSnapshot {
    snapshot.with_uncollected_messages(snapshot.uncollected_messages().min(100))
}

#[cfg(test)]
mod tests {
    use reticulum_appliance_display_model::{
        DisplayCompositionState, DisplayHomeSnapshot, DisplayLabel, DisplaySetupState,
        DisplayViewKind, PairingPasskey, PairingSecretClearReason, PairingWindowSeconds,
    };

    use super::{DisplayCoordinatorOutput, E290DisplayCoordinator, MESSAGE_BURST_COALESCE_MILLIS};

    fn label() -> DisplayLabel {
        DisplayLabel::new("Reticulum E290").expect("fixture label fits")
    }

    fn home() -> DisplayHomeSnapshot {
        DisplayHomeSnapshot::new(
            label(),
            DisplayLabel::new("e13f88").expect("fixture suffix fits"),
            DisplaySetupState::Paired,
            DisplayCompositionState::Configured,
            DisplayCompositionState::Configured,
            DisplayCompositionState::Configured,
            DisplayCompositionState::Configured,
        )
    }

    fn published_view(output: DisplayCoordinatorOutput) -> Option<DisplayViewKind> {
        output
            .into_command()
            .map(|command| command.requested_view())
    }

    #[test]
    fn first_message_and_acknowledgement_render_immediately() {
        let mut coordinator = E290DisplayCoordinator::new(home());
        assert_eq!(
            published_view(coordinator.resume_home(10)),
            Some(DisplayViewKind::Home)
        );
        assert_eq!(
            published_view(coordinator.set_uncollected_messages(1, 11)),
            Some(DisplayViewKind::Home)
        );
        assert_eq!(coordinator.home().uncollected_messages(), 1);
        assert_eq!(
            published_view(coordinator.set_uncollected_messages(0, 12)),
            Some(DisplayViewKind::Home)
        );
    }

    #[test]
    fn message_bursts_keep_the_latest_count_and_one_deadline() {
        let mut coordinator = E290DisplayCoordinator::new(home());
        let _ = coordinator.resume_home(100);
        let _ = coordinator.set_uncollected_messages(1, 101);

        let deadline = 101 + MESSAGE_BURST_COALESCE_MILLIS;
        assert_eq!(
            coordinator.set_uncollected_messages(2, 102).refresh_at_ms(),
            Some(deadline)
        );
        assert_eq!(
            coordinator
                .set_uncollected_messages(9, deadline - 1)
                .refresh_at_ms(),
            Some(deadline)
        );
        assert_eq!(
            published_view(coordinator.poll(deadline)),
            Some(DisplayViewKind::Home)
        );
        assert_eq!(coordinator.home().uncollected_messages(), 9);
        assert!(matches!(
            coordinator.poll(deadline + 1),
            DisplayCoordinatorOutput::Unchanged
        ));
    }

    #[test]
    fn pairing_protects_secret_view_and_resume_uses_latest_home() {
        let mut coordinator = E290DisplayCoordinator::new(home());
        let _ = coordinator.resume_home(0);
        assert_eq!(
            published_view(coordinator.show_pairing(
                label(),
                PairingPasskey::from_number(123_456).expect("fixture passkey"),
                PairingWindowSeconds::new(30).expect("fixture window"),
            )),
            Some(DisplayViewKind::Pairing)
        );
        assert!(matches!(
            coordinator.set_uncollected_messages(1, 1),
            DisplayCoordinatorOutput::Deferred {
                refresh_at_ms: None
            }
        ));
        assert!(matches!(
            coordinator.set_setup(DisplaySetupState::BluetoothRecoveryRequired, 2),
            DisplayCoordinatorOutput::Deferred {
                refresh_at_ms: None
            }
        ));
        assert_eq!(
            published_view(coordinator.resume_home(3)),
            Some(DisplayViewKind::Home)
        );
        assert_eq!(coordinator.home().uncollected_messages(), 1);
        assert_eq!(
            coordinator.home().setup(),
            DisplaySetupState::BluetoothRecoveryRequired
        );
    }

    #[test]
    fn terminal_pairing_view_is_not_replaced_by_ordinary_home_activity() {
        let mut coordinator = E290DisplayCoordinator::new(home());
        let _ = coordinator.resume_home(0);
        assert_eq!(
            published_view(
                coordinator.clear_pairing_secret(label(), PairingSecretClearReason::Failed,)
            ),
            Some(DisplayViewKind::PairingFailed)
        );
        assert!(matches!(
            coordinator.set_uncollected_messages(1, 1),
            DisplayCoordinatorOutput::Deferred {
                refresh_at_ms: None
            }
        ));
        assert_eq!(
            published_view(coordinator.resume_home(2)),
            Some(DisplayViewKind::Home)
        );
    }

    #[test]
    fn identical_state_and_visually_capped_counts_are_deduplicated() {
        let mut coordinator = E290DisplayCoordinator::new(home());
        let _ = coordinator.resume_home(0);
        assert!(matches!(
            coordinator.resume_home(1),
            DisplayCoordinatorOutput::Unchanged
        ));
        let _ = coordinator.set_uncollected_messages(100, 2);
        assert!(matches!(
            coordinator.set_uncollected_messages(101, 3),
            DisplayCoordinatorOutput::Unchanged
        ));
        assert_eq!(coordinator.home().uncollected_messages(), 101);
        assert!(matches!(
            coordinator.set_setup(DisplaySetupState::Paired, 4),
            DisplayCoordinatorOutput::Unchanged
        ));
    }

    #[test]
    fn boot_and_terminal_commands_deduplicate_without_replacing_home_state() {
        let mut coordinator = E290DisplayCoordinator::new(home());
        assert_eq!(
            published_view(coordinator.show_booting(label())),
            Some(DisplayViewKind::Booting)
        );
        assert!(matches!(
            coordinator.show_booting(label()),
            DisplayCoordinatorOutput::Unchanged
        ));
        assert_eq!(
            published_view(
                coordinator.clear_pairing_secret(label(), PairingSecretClearReason::TimedOut,)
            ),
            Some(DisplayViewKind::PairingTimedOut)
        );
        assert!(matches!(
            coordinator.clear_pairing_secret(label(), PairingSecretClearReason::TimedOut),
            DisplayCoordinatorOutput::Unchanged
        ));
    }
}
