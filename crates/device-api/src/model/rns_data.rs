//! Outbound RNS DATA submission model.

use super::*;

/// Target-safe outbound RNS DATA submission operation.
#[cfg(feature = "rns-data")]
pub const OP_SUBMIT_RNS_DATA: u16 = 0xf001;
/// Acceptance result for an outbound RNS DATA submission.
///
/// The response contains only the device-assigned identifier used with
/// `submission.status`; it never contains prepared packet bytes.
#[cfg(feature = "rns-data")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmissionAccepted {
    /// Device-assigned submission identifier.
    ///
    /// Acceptance means the device reserved the bounded capacity needed to own
    /// the submission. It does not guarantee delivery; a later status may
    /// report [`SubmissionFailure::DeliveryTimeout`].
    pub id: SubmissionId,
}
