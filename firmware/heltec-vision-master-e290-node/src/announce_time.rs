//! Per-boot announce ordering over a durably reserved epoch.

use reticulum_announce_clock::{AnnounceOrdinal, BootEpoch, MAX_ANNOUNCE_ORDINAL};
use reticulum_node_core::AnnounceEmissionTime;

/// Volatile 20-bit announce ordinal under one persisted 20-bit boot epoch.
///
/// The first timestamp of epoch `n + 1` is strictly greater than every value
/// epoch `n` can emit. At the product's 30-minute cadence, the 1,048,576-value
/// per-boot space spans almost 60 years; exhaustion still suppresses
/// further local announces instead of wrapping.
pub struct BootAnnounceClock {
    epoch: BootEpoch,
    next_ordinal: Option<u32>,
}

impl BootAnnounceClock {
    /// Bind a durably committed 20-bit epoch to a fresh boot ordinal.
    pub const fn new(epoch: BootEpoch) -> Self {
        Self {
            epoch,
            next_ordinal: Some(0),
        }
    }

    /// Durably reserved boot epoch.
    pub const fn epoch(&self) -> BootEpoch {
        self.epoch
    }

    /// Next timestamp to offer to local announce construction, if available.
    pub const fn next_emission(&self) -> Option<AnnounceEmissionTime> {
        match self.next_ordinal {
            Some(ordinal) => match AnnounceOrdinal::new(ordinal) {
                Some(ordinal) => {
                    let value = self.epoch.timestamp(ordinal).get();
                    match AnnounceEmissionTime::new(value) {
                        Ok(value) => Some(value),
                        Err(_) => None,
                    }
                }
                None => None,
            },
            None => None,
        }
    }

    /// Consume the current value only after RNS accepted the corresponding
    /// signed announce into its owned queue.
    pub fn mark_queued(&mut self) {
        self.next_ordinal = self
            .next_ordinal
            .and_then(|value| value.checked_add(1))
            .filter(|value| *value <= MAX_ANNOUNCE_ORDINAL);
    }

    /// Whether every ordinal in this boot epoch has been consumed.
    pub const fn is_exhausted(&self) -> bool {
        self.next_ordinal.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_announce_clock::{MAX_ANNOUNCE_EMISSION_TIMESTAMP, MAX_BOOT_EPOCH};

    fn epoch(value: u32) -> BootEpoch {
        BootEpoch::new(value).expect("test epoch must fit")
    }

    #[test]
    fn next_boot_starts_after_every_prior_boot_ordinal() {
        let prior = BootAnnounceClock::new(epoch(41));
        let next = BootAnnounceClock::new(epoch(42));
        let prior_max = prior
            .epoch()
            .timestamp(AnnounceOrdinal::new(MAX_ANNOUNCE_ORDINAL).unwrap())
            .get();
        assert!(next.next_emission().unwrap().get() > prior_max);
    }

    #[test]
    fn ordinal_advances_only_when_marked_queued() {
        let mut clock = BootAnnounceClock::new(epoch(1));
        assert_eq!(clock.next_emission().unwrap().get(), 1 << 20);
        assert_eq!(clock.next_emission().unwrap().get(), 1 << 20);
        clock.mark_queued();
        assert_eq!(clock.next_emission().unwrap().get(), (1 << 20) + 1);
    }

    #[test]
    fn maximum_epoch_and_ordinal_end_at_wire_maximum_without_wrap() {
        let mut clock = BootAnnounceClock::new(epoch(MAX_BOOT_EPOCH));
        clock.next_ordinal = Some(MAX_ANNOUNCE_ORDINAL);
        assert_eq!(
            clock.next_emission().unwrap().get(),
            MAX_ANNOUNCE_EMISSION_TIMESTAMP
        );
        assert_eq!(MAX_ANNOUNCE_EMISSION_TIMESTAMP, AnnounceEmissionTime::MAX);
        clock.mark_queued();
        assert!(clock.is_exhausted());
        assert!(clock.next_emission().is_none());
    }
}
