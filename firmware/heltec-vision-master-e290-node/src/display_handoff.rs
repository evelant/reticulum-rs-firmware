//! Latest-value handoff from appliance state owners to one slow display actor.
//!
//! E-paper refreshes are intentionally much slower than ordinary node state
//! changes. This handoff therefore stores only the newest complete
//! [`DisplayCommand`]. Publishing never waits for a refresh and overwriting a
//! queued pairing view drops and zeroizes its passkey owner.

use embassy_sync::{blocking_mutex::raw::RawMutex, signal::Signal};
use reticulum_appliance_display_model::DisplayCommand;

/// Pixel width of the fitted E290 monochrome panel.
pub const E290_DISPLAY_WIDTH_PIXELS: usize = 296;
/// Pixel height of the fitted E290 monochrome panel.
pub const E290_DISPLAY_HEIGHT_PIXELS: usize = 128;
/// Bits retained for each pixel in the initial E290 actor framebuffer.
pub const E290_DISPLAY_BITS_PER_PIXEL: usize = 1;
/// Exact packed bytes required by one 296-by-128 one-bit E290 framebuffer.
pub const E290_MONOCHROME_FRAMEBUFFER_BYTES: usize =
    E290_DISPLAY_WIDTH_PIXELS * E290_DISPLAY_HEIGHT_PIXELS * E290_DISPLAY_BITS_PER_PIXEL / 8;
/// Product ceiling for the initial E290 display actor-owned framebuffer.
pub const E290_DISPLAY_FRAMEBUFFER_CEILING_BYTES: usize = 4_736;

const _: () = assert!(E290_MONOCHROME_FRAMEBUFFER_BYTES == 4_736);
const _: () = assert!(E290_MONOCHROME_FRAMEBUFFER_BYTES <= E290_DISPLAY_FRAMEBUFFER_CEILING_BYTES);

/// Unique producer for complete semantic display states.
#[must_use = "dropping the display publisher abandons the sole update capability"]
pub struct DisplayPublisher<M>
where
    M: RawMutex + 'static,
{
    latest: &'static Signal<M, DisplayCommand>,
}

impl<M> DisplayPublisher<M>
where
    M: RawMutex + 'static,
{
    /// Publish a complete desired state, replacing any older unconsumed state.
    pub fn publish_latest(&mut self, command: DisplayCommand) {
        self.latest.signal(command);
    }
}

/// Unique consumer owned by the future asynchronous display actor.
#[must_use = "dropping the display receiver abandons pending display state"]
pub struct DisplayReceiver<M>
where
    M: RawMutex + 'static,
{
    latest: &'static Signal<M, DisplayCommand>,
}

impl<M> DisplayReceiver<M>
where
    M: RawMutex + 'static,
{
    /// Take the newest desired state immediately, when one is pending.
    pub fn try_take_latest(&mut self) -> Option<DisplayCommand> {
        self.latest.try_take()
    }

    /// Wait asynchronously for the newest desired state.
    pub async fn next(&mut self) -> DisplayCommand {
        self.latest.wait().await
    }
}

/// Static storage for one coalescing producer-to-display relationship.
pub struct DisplayHandoff<M>
where
    M: RawMutex,
{
    latest: Signal<M, DisplayCommand>,
}

impl<M> DisplayHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Construct an empty latest-value store.
    pub const fn new() -> Self {
        Self {
            latest: Signal::new(),
        }
    }

    /// Split storage into its sole publisher and receiver capabilities.
    pub fn split(&'static mut self) -> (DisplayPublisher<M>, DisplayReceiver<M>) {
        (
            DisplayPublisher {
                latest: &self.latest,
            },
            DisplayReceiver {
                latest: &self.latest,
            },
        )
    }
}

impl<M> Default for DisplayHandoff<M>
where
    M: RawMutex + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use reticulum_appliance_display_model::{
        DisplayCommand, DisplayLabel, DisplayState, DisplayViewKind, PairingPasskey,
        PairingSecretClearReason, PairingWindowSeconds,
    };

    use super::{
        DisplayHandoff, DisplayPublisher, DisplayReceiver, E290_DISPLAY_FRAMEBUFFER_CEILING_BYTES,
        E290_MONOCHROME_FRAMEBUFFER_BYTES,
    };

    fn label() -> DisplayLabel {
        DisplayLabel::new("Reticulum E290").expect("test label must fit")
    }

    fn handoff() -> (
        DisplayPublisher<NoopRawMutex>,
        DisplayReceiver<NoopRawMutex>,
    ) {
        std::boxed::Box::leak(std::boxed::Box::new(DisplayHandoff::new())).split()
    }

    #[test]
    fn pressure_keeps_only_the_latest_complete_state() {
        let (mut publisher, mut receiver) = handoff();
        publisher.publish_latest(DisplayCommand::ShowBooting { label: label() });
        publisher.publish_latest(DisplayCommand::ShowReady { label: label() });
        for passkey in 0..64 {
            publisher.publish_latest(DisplayCommand::ShowPairing {
                label: label(),
                passkey: PairingPasskey::from_number(passkey).expect("six-digit passkey"),
                expires_after_seconds: PairingWindowSeconds::new(60)
                    .expect("nonzero pairing window"),
            });
        }

        let command = receiver
            .try_take_latest()
            .expect("one latest state must remain");
        assert_eq!(command.requested_view(), DisplayViewKind::Pairing);
        let mut state = DisplayState::new();
        let _ = state.apply(command);
        state
            .view()
            .expose_pairing_passkey(|digits| assert_eq!(digits, Some(b"000063")));
        assert!(receiver.try_take_latest().is_none());
    }

    #[test]
    fn terminal_clear_supersedes_a_queued_passkey() {
        let (mut publisher, mut receiver) = handoff();
        publisher.publish_latest(DisplayCommand::ShowPairing {
            label: label(),
            passkey: PairingPasskey::from_number(123_456).expect("six-digit passkey"),
            expires_after_seconds: PairingWindowSeconds::new(60).expect("nonzero pairing window"),
        });
        publisher.publish_latest(DisplayCommand::ClearPairingSecret {
            label: label(),
            reason: PairingSecretClearReason::Succeeded,
        });

        let mut state = DisplayState::new();
        let _ = state.apply(
            receiver
                .try_take_latest()
                .expect("terminal state must replace pending passkey"),
        );
        assert_eq!(state.view().kind(), DisplayViewKind::PairingSucceeded);
        assert!(!state.owns_pairing_secret());
        assert!(receiver.try_take_latest().is_none());
    }

    #[test]
    fn e290_framebuffer_exactly_meets_the_product_ceiling() {
        assert_eq!(E290_MONOCHROME_FRAMEBUFFER_BYTES, 4_736);
        assert_eq!(E290_DISPLAY_FRAMEBUFFER_CEILING_BYTES, 4_736);
    }
}
