//! App-facing phone-location observation boundary.

use std::fmt;

use reticulum_appliance_store as core;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{JsonSafeInteger, deserialize_json_safe_u64, serialize_json_safe_u64};

/// Platform-reported location authorization precision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum PhoneLocationAuthorizationView {
    Precise,
    Approximate,
    Unknown,
}

/// How the app obtained one phone location fix.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum PhoneLocationSourceView {
    ForegroundStream,
    LastKnown,
}

/// Explicit reason that the runtime cannot stamp an attempt with a fix.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum PhoneLocationUnavailableReasonView {
    NotObserved,
    TelemetryDisabled,
    PermissionDenied,
    ServicesDisabled,
    PlatformUnavailable,
    NoFixYet,
    ProviderError,
}

/// Latest phone-location state supplied by the app to the local runtime.
///
/// Available samples remain explicitly phone-side observations. They do not
/// claim board GNSS position or the exact RF transmission time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum PhoneLocationObservationView {
    Available {
        latitude_e6: i32,
        longitude_e6: i32,
        /// Platform-reported altitude above its geodetic reference, in millimetres.
        altitude_mm: Option<i32>,
        horizontal_accuracy_mm: Option<u32>,
        /// Platform-reported vertical accuracy radius, in millimetres.
        vertical_accuracy_mm: Option<u32>,
        #[serde(
            deserialize_with = "deserialize_json_safe_u64",
            serialize_with = "serialize_json_safe_u64"
        )]
        #[ts(as = "JsonSafeInteger")]
        captured_at_unix_ms: u64,
        authorization: PhoneLocationAuthorizationView,
        source: PhoneLocationSourceView,
        mocked: Option<bool>,
    },
    Unavailable {
        reason: PhoneLocationUnavailableReasonView,
    },
}

impl PhoneLocationObservationView {
    /// Validate coordinates and capture time against the durable core model.
    pub fn validate(self) -> Result<(), PhoneLocationObservationError> {
        self.into_core().map(|_| ())
    }

    pub(crate) fn into_core(
        self,
    ) -> Result<core::AttemptLocationStamp, PhoneLocationObservationError> {
        match self {
            Self::Available {
                latitude_e6,
                longitude_e6,
                altitude_mm,
                horizontal_accuracy_mm,
                vertical_accuracy_mm,
                captured_at_unix_ms,
                authorization,
                source,
                mocked,
            } => core::PhoneLocationSample::new(
                latitude_e6,
                longitude_e6,
                horizontal_accuracy_mm,
                captured_at_unix_ms,
                match authorization {
                    PhoneLocationAuthorizationView::Precise => {
                        core::PhoneLocationAuthorization::Precise
                    }
                    PhoneLocationAuthorizationView::Approximate => {
                        core::PhoneLocationAuthorization::Approximate
                    }
                    PhoneLocationAuthorizationView::Unknown => {
                        core::PhoneLocationAuthorization::Unknown
                    }
                },
                match source {
                    PhoneLocationSourceView::ForegroundStream => {
                        core::PhoneLocationSource::ForegroundStream
                    }
                    PhoneLocationSourceView::LastKnown => core::PhoneLocationSource::LastKnown,
                },
                mocked,
            )
            .map(|sample| sample.with_altitude(altitude_mm, vertical_accuracy_mm))
            .map(core::AttemptLocationStamp::Available)
            .ok_or(PhoneLocationObservationError::InvalidSample),
            Self::Unavailable { reason } => {
                Ok(core::AttemptLocationStamp::Unavailable(match reason {
                    PhoneLocationUnavailableReasonView::NotObserved => {
                        core::PhoneLocationUnavailableReason::NotObserved
                    }
                    PhoneLocationUnavailableReasonView::TelemetryDisabled => {
                        core::PhoneLocationUnavailableReason::TelemetryDisabled
                    }
                    PhoneLocationUnavailableReasonView::PermissionDenied => {
                        core::PhoneLocationUnavailableReason::PermissionDenied
                    }
                    PhoneLocationUnavailableReasonView::ServicesDisabled => {
                        core::PhoneLocationUnavailableReason::ServicesDisabled
                    }
                    PhoneLocationUnavailableReasonView::PlatformUnavailable => {
                        core::PhoneLocationUnavailableReason::PlatformUnavailable
                    }
                    PhoneLocationUnavailableReasonView::NoFixYet => {
                        core::PhoneLocationUnavailableReason::NoFixYet
                    }
                    PhoneLocationUnavailableReasonView::ProviderError => {
                        core::PhoneLocationUnavailableReason::ProviderError
                    }
                }))
            }
        }
    }
}

impl From<core::AttemptLocationStamp> for PhoneLocationObservationView {
    fn from(location: core::AttemptLocationStamp) -> Self {
        match location {
            core::AttemptLocationStamp::Available(sample) => Self::Available {
                latitude_e6: sample.latitude_e6(),
                longitude_e6: sample.longitude_e6(),
                altitude_mm: sample.altitude_mm(),
                horizontal_accuracy_mm: sample.horizontal_accuracy_mm(),
                vertical_accuracy_mm: sample.vertical_accuracy_mm(),
                captured_at_unix_ms: sample.captured_at_unix_ms(),
                authorization: match sample.authorization() {
                    core::PhoneLocationAuthorization::Precise => {
                        PhoneLocationAuthorizationView::Precise
                    }
                    core::PhoneLocationAuthorization::Approximate => {
                        PhoneLocationAuthorizationView::Approximate
                    }
                    core::PhoneLocationAuthorization::Unknown => {
                        PhoneLocationAuthorizationView::Unknown
                    }
                },
                source: match sample.source() {
                    core::PhoneLocationSource::ForegroundStream => {
                        PhoneLocationSourceView::ForegroundStream
                    }
                    core::PhoneLocationSource::LastKnown => PhoneLocationSourceView::LastKnown,
                },
                mocked: sample.mocked(),
            },
            core::AttemptLocationStamp::Unavailable(reason) => Self::Unavailable {
                reason: match reason {
                    core::PhoneLocationUnavailableReason::NotObserved => {
                        PhoneLocationUnavailableReasonView::NotObserved
                    }
                    core::PhoneLocationUnavailableReason::TelemetryDisabled => {
                        PhoneLocationUnavailableReasonView::TelemetryDisabled
                    }
                    core::PhoneLocationUnavailableReason::PermissionDenied => {
                        PhoneLocationUnavailableReasonView::PermissionDenied
                    }
                    core::PhoneLocationUnavailableReason::ServicesDisabled => {
                        PhoneLocationUnavailableReasonView::ServicesDisabled
                    }
                    core::PhoneLocationUnavailableReason::PlatformUnavailable => {
                        PhoneLocationUnavailableReasonView::PlatformUnavailable
                    }
                    core::PhoneLocationUnavailableReason::NoFixYet => {
                        PhoneLocationUnavailableReasonView::NoFixYet
                    }
                    core::PhoneLocationUnavailableReason::ProviderError => {
                        PhoneLocationUnavailableReasonView::ProviderError
                    }
                },
            },
        }
    }
}

/// Invalid app-provided phone location observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhoneLocationObservationError {
    /// A coordinate or capture time is outside the shared durable bounds.
    InvalidSample,
}

impl fmt::Display for PhoneLocationObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("phone location coordinate or capture time is outside shared bounds")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_observation_round_trips_without_losing_precision_metadata() {
        let observation = PhoneLocationObservationView::Available {
            latitude_e6: 42_357_111,
            longitude_e6: -71_061_924,
            altitude_mm: Some(17_234),
            horizontal_accuracy_mm: Some(8_250),
            vertical_accuracy_mm: Some(12_500),
            captured_at_unix_ms: 1_722_000_000_123,
            authorization: PhoneLocationAuthorizationView::Precise,
            source: PhoneLocationSourceView::ForegroundStream,
            mocked: Some(false),
        };
        let core = observation.into_core().unwrap();
        assert_eq!(PhoneLocationObservationView::from(core), observation);
    }

    #[test]
    fn out_of_world_coordinates_are_rejected() {
        let observation = PhoneLocationObservationView::Available {
            latitude_e6: 90_000_001,
            longitude_e6: 0,
            altitude_mm: None,
            horizontal_accuracy_mm: None,
            vertical_accuracy_mm: None,
            captured_at_unix_ms: 1,
            authorization: PhoneLocationAuthorizationView::Unknown,
            source: PhoneLocationSourceView::LastKnown,
            mocked: None,
        };
        assert_eq!(
            observation.validate(),
            Err(PhoneLocationObservationError::InvalidSample)
        );
    }
}
