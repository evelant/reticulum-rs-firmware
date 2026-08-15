//! Coarse public initialization results for the pre-authentication bearer.
//!
//! Internal media, policy, and backend diagnostics must never cross the local
//! bootstrap wire boundary. This module centralizes that lossy mapping so the
//! bearer owner cannot accidentally disclose a product-storage detail.

use reticulum_device_api_pairing_control::{InitializationStatus, InitializeResult};
use reticulum_device_api_pairing_policy::RequestRefusal;

use crate::credential_runtime::{
    CredentialInitializationStatus, InitializationDriveOutcome, InitializationRequestRefusal,
};

/// Collapse the resident credential owner into the stable public status set.
pub const fn public_initialization_status(
    status: CredentialInitializationStatus,
) -> InitializationStatus {
    match status {
        CredentialInitializationStatus::Unavailable => InitializationStatus::Unavailable,
        CredentialInitializationStatus::Eligible { .. } => {
            InitializationStatus::InitializationRequired
        }
        CredentialInitializationStatus::InFlight { .. } => InitializationStatus::InFlight,
        CredentialInitializationStatus::Blocked { .. }
        | CredentialInitializationStatus::PolicyFault { .. } => InitializationStatus::Blocked,
        CredentialInitializationStatus::Completed => InitializationStatus::Completed,
    }
}

/// Collapse one initialization-admission refusal into its public result.
pub const fn public_initialization_refusal(
    refusal: InitializationRequestRefusal,
) -> InitializeResult {
    match refusal {
        InitializationRequestRefusal::PairingUnavailable => InitializeResult::Unavailable,
        InitializationRequestRefusal::Policy(refused) => public_policy_refusal(refused.reason()),
    }
}

const fn public_policy_refusal(refusal: RequestRefusal) -> InitializeResult {
    match refusal {
        RequestRefusal::NotConnected
        | RequestRefusal::WrongConnection
        | RequestRefusal::WindowNotOpen
        | RequestRefusal::TimedOut => InitializeResult::PhysicalPresenceRequired,
        RequestRefusal::OperationInFlight => InitializeResult::Retrying,
        RequestRefusal::InitializationNotEligible
        | RequestRefusal::PendingExists
        | RequestRefusal::PendingMissing
        | RequestRefusal::PendingMismatch => InitializeResult::Refused,
        RequestRefusal::ClockRegression | RequestRefusal::OperationIdExhausted => {
            InitializeResult::Blocked
        }
    }
}

/// Collapse one physical initialization drive into its public result.
pub fn public_initialization_drive<E>(outcome: InitializationDriveOutcome<E>) -> InitializeResult {
    match outcome {
        InitializationDriveOutcome::Completed => InitializeResult::Completed,
        InitializationDriveOutcome::Retry(_) => InitializeResult::Retrying,
        InitializationDriveOutcome::Blocked(_) => InitializeResult::Blocked,
        InitializationDriveOutcome::NotInFlight(status) => {
            match public_initialization_status(status) {
                InitializationStatus::Unavailable => InitializeResult::Unavailable,
                InitializationStatus::InitializationRequired => {
                    InitializeResult::PhysicalPresenceRequired
                }
                InitializationStatus::InFlight => InitializeResult::Retrying,
                InitializationStatus::Completed => InitializeResult::Completed,
                InitializationStatus::Blocked => InitializeResult::Blocked,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use reticulum_device_api_pairing_policy::RequestRefusal;

    use super::*;
    use crate::credential_runtime::{InitializationRequestRefusal, InitializationRetry};

    #[test]
    fn every_resident_status_maps_without_internal_details() {
        use reticulum_device_api_pairing_policy::InitializableMedia;

        let eligible = CredentialInitializationStatus::Eligible {
            media: InitializableMedia::ExactlyErased,
        };
        let in_flight = CredentialInitializationStatus::InFlight {
            media: InitializableMedia::RecoverableInterrupted,
            physical_io_attempted: true,
        };

        assert_eq!(
            public_initialization_status(CredentialInitializationStatus::Unavailable),
            InitializationStatus::Unavailable
        );
        assert_eq!(
            public_initialization_status(eligible),
            InitializationStatus::InitializationRequired
        );
        assert_eq!(
            public_initialization_status(in_flight),
            InitializationStatus::InFlight
        );
        assert_eq!(
            public_initialization_status(CredentialInitializationStatus::Completed),
            InitializationStatus::Completed
        );
    }

    #[test]
    fn every_policy_refusal_class_maps_to_one_coarse_result() {
        for (reason, expected) in [
            (
                RequestRefusal::NotConnected,
                InitializeResult::PhysicalPresenceRequired,
            ),
            (
                RequestRefusal::WrongConnection,
                InitializeResult::PhysicalPresenceRequired,
            ),
            (
                RequestRefusal::WindowNotOpen,
                InitializeResult::PhysicalPresenceRequired,
            ),
            (
                RequestRefusal::TimedOut,
                InitializeResult::PhysicalPresenceRequired,
            ),
            (
                RequestRefusal::OperationInFlight,
                InitializeResult::Retrying,
            ),
            (
                RequestRefusal::InitializationNotEligible,
                InitializeResult::Refused,
            ),
            (RequestRefusal::PendingExists, InitializeResult::Refused),
            (RequestRefusal::PendingMissing, InitializeResult::Refused),
            (RequestRefusal::PendingMismatch, InitializeResult::Refused),
            (RequestRefusal::ClockRegression, InitializeResult::Blocked),
            (
                RequestRefusal::OperationIdExhausted,
                InitializeResult::Blocked,
            ),
        ] {
            assert_eq!(public_policy_refusal(reason), expected);
        }
        assert_eq!(
            public_initialization_refusal(InitializationRequestRefusal::PairingUnavailable),
            InitializeResult::Unavailable
        );
    }

    #[test]
    fn physical_drive_retry_does_not_leak_its_cause() {
        let retry = InitializationDriveOutcome::<u8>::Retry(InitializationRetry::Backend(7));
        assert_eq!(
            public_initialization_drive(retry),
            InitializeResult::Retrying
        );
        assert_eq!(
            public_initialization_drive::<u8>(InitializationDriveOutcome::Completed),
            InitializeResult::Completed
        );
        assert_eq!(
            public_initialization_drive::<u8>(InitializationDriveOutcome::NotInFlight(
                CredentialInitializationStatus::Unavailable
            )),
            InitializeResult::Unavailable
        );
    }
}
