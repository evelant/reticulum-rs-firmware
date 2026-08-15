//! Latest-value handoff from appliance state owners to one slow display actor.
//!
//! E-paper refreshes are intentionally much slower than ordinary node state
//! changes. This handoff therefore stores only the newest complete
//! [`DisplayRequest`]. Publishing never waits for a refresh and overwriting a
//! queued pairing view drops and zeroizes its passkey owner. A separate
//! latest-value acknowledgement path lets the publisher distinguish a desired
//! semantic view from one that the actor physically rendered.

use embassy_sync::{blocking_mutex::raw::RawMutex, signal::Signal};
use reticulum_appliance_display_model::{DisplayCommand, DisplayHomeSnapshot, DisplayViewKind};

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

/// Latest non-secret live fields folded into the complete Home presentation.
///
/// This is deliberately separate from [`DisplayRequest`]. Pairing and recovery
/// remain the only producer of protected presentation commands, while the node
/// actor may replace this small telemetry snapshot without waiting for an
/// e-paper refresh.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DisplayHomeTelemetry {
    uncollected_messages: u32,
}

impl DisplayHomeTelemetry {
    /// Construct the current live Home projection.
    pub const fn new(uncollected_messages: u32) -> Self {
        Self {
            uncollected_messages,
        }
    }

    /// Messages durably retained by the appliance but not yet acknowledged as
    /// collected by the app.
    pub const fn uncollected_messages(self) -> u32 {
        self.uncollected_messages
    }
}

/// Overlay live telemetry only after a producer has supplied an observation.
///
/// `None` deliberately preserves the durable count already carried by the
/// boot-composed Home snapshot. It is not equivalent to an observed zero.
pub const fn overlay_observed_home_telemetry(
    snapshot: DisplayHomeSnapshot,
    telemetry: Option<DisplayHomeTelemetry>,
) -> DisplayHomeSnapshot {
    match telemetry {
        Some(telemetry) => snapshot.with_uncollected_messages(telemetry.uncollected_messages()),
        None => snapshot,
    }
}

/// Latest-value producer for non-secret Home telemetry.
#[must_use = "dropping the telemetry publisher abandons live Home updates"]
pub struct DisplayTelemetryPublisher<M>
where
    M: RawMutex + 'static,
{
    latest: &'static Signal<M, DisplayHomeTelemetry>,
}

impl<M> DisplayTelemetryPublisher<M>
where
    M: RawMutex + 'static,
{
    /// Replace any unconsumed telemetry with the newest complete snapshot.
    pub fn publish_latest(&mut self, telemetry: DisplayHomeTelemetry) {
        self.latest.signal(telemetry);
    }
}

/// Latest-value consumer owned by the display actor.
#[must_use = "dropping the telemetry receiver abandons live Home updates"]
pub struct DisplayTelemetryReceiver<M>
where
    M: RawMutex + 'static,
{
    latest: &'static Signal<M, DisplayHomeTelemetry>,
}

impl<M> DisplayTelemetryReceiver<M>
where
    M: RawMutex + 'static,
{
    /// Take the newest telemetry immediately, when one is pending.
    pub fn try_take_latest(&mut self) -> Option<DisplayHomeTelemetry> {
        self.latest.try_take()
    }

    /// Wait for the next telemetry replacement.
    pub async fn next(&mut self) -> DisplayHomeTelemetry {
        self.latest.wait().await
    }
}

/// Static storage for the node-to-display live Home projection.
pub struct DisplayTelemetryHandoff<M>
where
    M: RawMutex,
{
    latest: Signal<M, DisplayHomeTelemetry>,
}

impl<M> DisplayTelemetryHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Construct an empty latest-value telemetry store.
    pub const fn new() -> Self {
        Self {
            latest: Signal::new(),
        }
    }

    /// Split storage into its sole publisher and receiver capabilities.
    pub fn split(&'static mut self) -> (DisplayTelemetryPublisher<M>, DisplayTelemetryReceiver<M>) {
        (
            DisplayTelemetryPublisher {
                latest: &self.latest,
            },
            DisplayTelemetryReceiver {
                latest: &self.latest,
            },
        )
    }
}

impl<M> Default for DisplayTelemetryHandoff<M>
where
    M: RawMutex + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Opaque, strictly increasing identifier assigned to one published request.
///
/// Identifiers are unique for the lifetime of one handoff. The publisher
/// rejects new requests after assigning `u64::MAX` instead of wrapping to a
/// stale identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DisplayRequestId(u64);

impl DisplayRequestId {
    /// Return the nonzero sequence carried by this opaque identifier.
    pub const fn sequence(self) -> u64 {
        self.0
    }
}

/// The handoff assigned every representable display request identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayRequestIdExhausted;

/// Exact request transferred from the publisher to the display actor.
///
/// This owner deliberately implements neither `Clone` nor `Debug` because its
/// command may own a pairing passkey.
#[must_use = "a display request may own a pairing passkey and must be rendered or dropped"]
pub struct DisplayRequest {
    request_id: DisplayRequestId,
    command: DisplayCommand,
}

impl DisplayRequest {
    const fn new(request_id: DisplayRequestId, command: DisplayCommand) -> Self {
        Self {
            request_id,
            command,
        }
    }

    /// Opaque identifier that a completion must echo.
    pub const fn request_id(&self) -> DisplayRequestId {
        self.request_id
    }

    /// Non-secret semantic view requested by the owned command.
    pub const fn requested_view(&self) -> DisplayViewKind {
        self.command.requested_view()
    }

    /// Consume the request into its identifier and exact command owner.
    pub fn into_parts(self) -> (DisplayRequestId, DisplayCommand) {
        (self.request_id, self.command)
    }
}

/// Physical outcome reported by the display actor for one request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayRenderOutcome {
    /// The actor completed the requested physical panel refresh.
    Rendered,
    /// Initialization, rendering, transfer, refresh, or shutdown faulted.
    Faulted,
}

/// Non-secret physical completion for one display request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "display completion determines whether a requested view became physically visible"]
pub struct DisplayCompletion {
    request_id: DisplayRequestId,
    view: DisplayViewKind,
    outcome: DisplayRenderOutcome,
}

impl DisplayCompletion {
    /// Construct the actor's non-secret physical completion.
    pub const fn new(
        request_id: DisplayRequestId,
        view: DisplayViewKind,
        outcome: DisplayRenderOutcome,
    ) -> Self {
        Self {
            request_id,
            view,
            outcome,
        }
    }

    /// Identifier of the exact request the actor processed.
    pub const fn request_id(self) -> DisplayRequestId {
        self.request_id
    }

    /// Non-secret semantic view the actor attempted to render.
    pub const fn view(self) -> DisplayViewKind {
        self.view
    }

    /// Whether the physical operation completed or faulted.
    pub const fn outcome(self) -> DisplayRenderOutcome {
        self.outcome
    }
}

/// Why an expected physical display completion was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayCompletionGateError {
    /// The actor reported a later request, so the expected completion can no
    /// longer arrive through the latest-value acknowledgement path.
    Superseded {
        /// Request whose physical completion was required.
        expected: DisplayRequestId,
        /// Later request observed from the actor.
        observed: DisplayRequestId,
    },
    /// The actor echoed the expected request identifier with the wrong
    /// non-secret semantic view.
    ViewMismatch {
        /// View associated with the published request.
        expected: DisplayViewKind,
        /// View reported by the actor.
        observed: DisplayViewKind,
    },
    /// The actor processed the exact request and reported a physical fault.
    Faulted {
        /// Exact request that faulted.
        request_id: DisplayRequestId,
        /// Non-secret view whose physical render faulted.
        view: DisplayViewKind,
    },
}

/// Physical boot-clear result reported before fresh pairing may be admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayBootClearOutcome {
    /// The actor completed a physical blank refresh, removing retained content.
    Ready,
    /// The actor could not prove that retained panel content was cleared.
    Faulted,
}

/// Unique producer for complete semantic display states.
#[must_use = "dropping the display publisher abandons the sole update capability"]
pub struct DisplayPublisher<M>
where
    M: RawMutex + 'static,
{
    latest: &'static Signal<M, DisplayRequest>,
    completion: &'static Signal<M, DisplayCompletion>,
    boot_clear: &'static Signal<M, DisplayBootClearOutcome>,
    next_request_id: Option<u64>,
}

impl<M> DisplayPublisher<M>
where
    M: RawMutex + 'static,
{
    /// Publish a complete desired state, replacing any older unconsumed state.
    ///
    /// The returned identifier is strictly newer than every identifier
    /// previously returned by this publisher. Identifier exhaustion rejects
    /// the command without replacing an already pending request; dropping the
    /// rejected command still zeroizes any owned passkey.
    pub fn publish_latest(
        &mut self,
        command: DisplayCommand,
    ) -> Result<DisplayRequestId, DisplayRequestIdExhausted> {
        let sequence = self.next_request_id.ok_or(DisplayRequestIdExhausted)?;
        let request_id = DisplayRequestId(sequence);
        self.next_request_id = sequence.checked_add(1);
        self.latest.signal(DisplayRequest::new(request_id, command));
        Ok(request_id)
    }

    /// Take the newest physical request completion immediately, when pending.
    pub fn try_take_completion(&mut self) -> Option<DisplayCompletion> {
        self.completion.try_take()
    }

    /// Wait asynchronously for the newest physical request completion.
    pub async fn next_completion(&mut self) -> DisplayCompletion {
        self.completion.wait().await
    }

    /// Wait for one exact request to be physically rendered.
    ///
    /// Older completions are ignored. A later identifier is terminal because
    /// this latest-value acknowledgement path can no longer produce the
    /// expected completion. An exact identifier must also report the expected
    /// semantic view and a successful physical render.
    pub async fn wait_for_rendered_completion(
        &mut self,
        expected_request_id: DisplayRequestId,
        expected_view: DisplayViewKind,
    ) -> Result<DisplayCompletion, DisplayCompletionGateError> {
        loop {
            let completion = self.next_completion().await;
            if completion.request_id() < expected_request_id {
                continue;
            }
            if completion.request_id() > expected_request_id {
                return Err(DisplayCompletionGateError::Superseded {
                    expected: expected_request_id,
                    observed: completion.request_id(),
                });
            }
            if completion.view() != expected_view {
                return Err(DisplayCompletionGateError::ViewMismatch {
                    expected: expected_view,
                    observed: completion.view(),
                });
            }
            return match completion.outcome() {
                DisplayRenderOutcome::Rendered => Ok(completion),
                DisplayRenderOutcome::Faulted => Err(DisplayCompletionGateError::Faulted {
                    request_id: completion.request_id(),
                    view: completion.view(),
                }),
            };
        }
    }

    /// Take the boot-clear readiness or fault acknowledgement immediately.
    pub fn try_take_boot_clear(&mut self) -> Option<DisplayBootClearOutcome> {
        self.boot_clear.try_take()
    }

    /// Wait until the actor reports boot-clear readiness or failure.
    ///
    /// Fresh pairing admission must proceed only for
    /// [`DisplayBootClearOutcome::Ready`].
    pub async fn wait_for_boot_clear(&mut self) -> DisplayBootClearOutcome {
        self.boot_clear.wait().await
    }
}

/// Unique consumer owned by the future asynchronous display actor.
#[must_use = "dropping the display receiver abandons pending display state"]
pub struct DisplayReceiver<M>
where
    M: RawMutex + 'static,
{
    latest: &'static Signal<M, DisplayRequest>,
    completion: &'static Signal<M, DisplayCompletion>,
    boot_clear: &'static Signal<M, DisplayBootClearOutcome>,
}

impl<M> DisplayReceiver<M>
where
    M: RawMutex + 'static,
{
    /// Take the newest desired state immediately, when one is pending.
    pub fn try_take_latest(&mut self) -> Option<DisplayRequest> {
        self.latest.try_take()
    }

    /// Wait asynchronously for the newest desired state.
    pub async fn next(&mut self) -> DisplayRequest {
        self.latest.wait().await
    }

    /// Publish the newest non-secret physical completion without blocking.
    ///
    /// A slow publisher may observe only the latest completion. Request
    /// identifiers let it reject stale or unrelated acknowledgements.
    pub fn report_completion(&mut self, completion: DisplayCompletion) {
        self.completion.signal(completion);
    }

    /// Report whether the mandatory physical boot clear completed.
    ///
    /// The actor should call this exactly once after attempting the initial
    /// blank refresh. A fault acknowledgement keeps fresh pairing fail-closed
    /// while allowing unrelated appliance services to continue.
    pub fn report_boot_clear(&mut self, outcome: DisplayBootClearOutcome) {
        self.boot_clear.signal(outcome);
    }
}

/// Static storage for one coalescing producer-to-display relationship.
pub struct DisplayHandoff<M>
where
    M: RawMutex,
{
    latest: Signal<M, DisplayRequest>,
    completion: Signal<M, DisplayCompletion>,
    boot_clear: Signal<M, DisplayBootClearOutcome>,
}

impl<M> DisplayHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Construct an empty latest-value store.
    pub const fn new() -> Self {
        Self {
            latest: Signal::new(),
            completion: Signal::new(),
            boot_clear: Signal::new(),
        }
    }

    /// Split storage into its sole publisher and receiver capabilities.
    pub fn split(&'static mut self) -> (DisplayPublisher<M>, DisplayReceiver<M>) {
        (
            DisplayPublisher {
                latest: &self.latest,
                completion: &self.completion,
                boot_clear: &self.boot_clear,
                next_request_id: Some(1),
            },
            DisplayReceiver {
                latest: &self.latest,
                completion: &self.completion,
                boot_clear: &self.boot_clear,
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
#[path = "display_handoff_tests.rs"]
mod tests;
