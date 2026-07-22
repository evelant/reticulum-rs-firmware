//! Per-boot announce ordering over a durably reserved epoch.

use reticulum_announce_clock::{AnnounceOrdinal, BootEpoch, MAX_ANNOUNCE_ORDINAL};
use reticulum_node_core::AnnounceEmissionTime;

use crate::config;

/// One local destination selected by the boot announce scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduledAnnounce {
    /// The node's primary transport destination.
    Primary,
    /// The optional local `lxmf.delivery` destination.
    LxmfDelivery,
}

/// Boot-burst and steady-state scheduling for local destinations.
///
/// A transport node immediately rebroadcasts a newly received announce. LoRa
/// is half duplex, so sending primary and service announces back-to-back makes
/// a peer receive the primary, begin its rebroadcast, and miss the service
/// announce. This scheduler therefore emits at most one local destination per
/// event and leaves a fixed quiet interval before the optional LXMF service.
/// Two identity-phased retry cycles follow before the 30-minute cadence.
pub struct BootAnnounceSchedule {
    next_seconds: u64,
    next: ScheduledAnnounce,
    lxmf_enabled: bool,
    bootstrap_retries_remaining: u8,
    phase_seconds: u64,
}

impl BootAnnounceSchedule {
    /// Start with one immediately due primary announce.
    pub fn new(now_seconds: u64, primary_destination: [u8; 16], lxmf_enabled: bool) -> Self {
        let seed = u32::from_le_bytes([
            primary_destination[0],
            primary_destination[1],
            primary_destination[2],
            primary_destination[3],
        ]);
        Self {
            next_seconds: now_seconds,
            next: ScheduledAnnounce::Primary,
            lxmf_enabled,
            bootstrap_retries_remaining: config::ANNOUNCE_BOOTSTRAP_RETRIES,
            phase_seconds: u64::from(seed) % config::ANNOUNCE_BOOTSTRAP_PHASE_SLOTS,
        }
    }

    /// Return the next destination only once its independent event is due.
    pub const fn due(&self, now_seconds: u64) -> Option<ScheduledAnnounce> {
        if now_seconds >= self.next_seconds {
            Some(self.next)
        } else {
            None
        }
    }

    /// Retain the selected destination and bootstrap budget after protocol
    /// admission rejects it, while moving the next attempt out of this poll.
    pub fn defer_attempt(&mut self, now_seconds: u64) {
        self.next_seconds = now_seconds.saturating_add(config::ANNOUNCE_ADMISSION_RETRY_SECONDS);
    }

    /// Advance after the selected destination is admitted or intentionally
    /// consumed because its optional service is disabled.
    pub fn mark_attempted(&mut self, now_seconds: u64) {
        if self.next == ScheduledAnnounce::Primary && self.lxmf_enabled {
            self.next = ScheduledAnnounce::LxmfDelivery;
            self.next_seconds =
                now_seconds.saturating_add(config::ANNOUNCE_DESTINATION_SPACING_SECONDS);
            return;
        }

        self.next = ScheduledAnnounce::Primary;
        let delay = match self.bootstrap_retries_remaining {
            remaining if remaining == config::ANNOUNCE_BOOTSTRAP_RETRIES => {
                config::ANNOUNCE_BOOTSTRAP_BASE_SECONDS.saturating_add(self.phase_seconds)
            }
            1.. => config::ANNOUNCE_BOOTSTRAP_RETRY_SPACING_SECONDS,
            0 => config::ANNOUNCE_INTERVAL_SECONDS,
        };
        self.bootstrap_retries_remaining = self.bootstrap_retries_remaining.saturating_sub(1);
        self.next_seconds = now_seconds.saturating_add(delay);
    }

    #[cfg(test)]
    const fn next_seconds(&self) -> u64 {
        self.next_seconds
    }

    #[cfg(test)]
    const fn next(&self) -> ScheduledAnnounce {
        self.next
    }
}

/// Volatile 20-bit announce ordinal under one persisted 20-bit boot epoch.
///
/// The first timestamp of epoch `n + 1` is strictly greater than every value
/// epoch `n` can emit. At the product's 30-minute cadence, a mounted LXMF node
/// consumes two values per cadence and the 1,048,576-value per-boot space spans
/// almost 30 years; exhaustion still suppresses further local announces
/// instead of wrapping.
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

    #[test]
    fn bootstrap_schedule_is_immediate_then_bounded_then_periodic() {
        let mut schedule = BootAnnounceSchedule::new(100, [0; 16], true);
        assert_eq!(schedule.due(100), Some(ScheduledAnnounce::Primary));
        schedule.mark_attempted(100);
        assert_eq!(schedule.next_seconds(), 108);
        assert_eq!(schedule.next(), ScheduledAnnounce::LxmfDelivery);
        assert_eq!(schedule.due(107), None);
        schedule.mark_attempted(108);
        assert_eq!(schedule.next_seconds(), 121);
        assert_eq!(schedule.next(), ScheduledAnnounce::Primary);
        schedule.mark_attempted(121);
        assert_eq!(schedule.next_seconds(), 129);
        schedule.mark_attempted(129);
        assert_eq!(schedule.next_seconds(), 159);
        schedule.mark_attempted(159);
        assert_eq!(schedule.next_seconds(), 167);
        schedule.mark_attempted(167);
        assert_eq!(schedule.next_seconds(), 1_967);
        schedule.mark_attempted(1_967);
        assert_eq!(schedule.next_seconds(), 1_975);
        schedule.mark_attempted(1_975);
        assert_eq!(schedule.next_seconds(), 3_775);
    }

    #[test]
    fn admission_deferral_retains_destination_and_bootstrap_budget() {
        let mut schedule = BootAnnounceSchedule::new(100, [0; 16], true);

        schedule.defer_attempt(100);
        assert_eq!(schedule.next(), ScheduledAnnounce::Primary);
        assert_eq!(schedule.due(100), None);
        assert_eq!(schedule.due(101), Some(ScheduledAnnounce::Primary));

        schedule.mark_attempted(101);
        assert_eq!(schedule.next(), ScheduledAnnounce::LxmfDelivery);
        assert_eq!(schedule.next_seconds(), 109);
        schedule.defer_attempt(109);
        assert_eq!(schedule.next(), ScheduledAnnounce::LxmfDelivery);
        assert_eq!(schedule.due(110), Some(ScheduledAnnounce::LxmfDelivery));

        schedule.mark_attempted(110);
        assert_eq!(schedule.next(), ScheduledAnnounce::Primary);
        assert_eq!(schedule.next_seconds(), 123);
    }

    #[test]
    fn primary_only_profile_does_not_spend_time_on_a_disabled_service() {
        let mut schedule = BootAnnounceSchedule::new(100, [0; 16], false);
        assert_eq!(schedule.due(100), Some(ScheduledAnnounce::Primary));
        schedule.mark_attempted(100);
        assert_eq!(schedule.next_seconds(), 113);
        assert_eq!(schedule.next(), ScheduledAnnounce::Primary);
        schedule.mark_attempted(113);
        assert_eq!(schedule.next_seconds(), 143);
        schedule.mark_attempted(143);
        assert_eq!(schedule.next_seconds(), 1_943);
    }

    #[test]
    fn known_e290_nodes_separate_explicit_and_native_retransmit_opportunities() {
        let mut a = BootAnnounceSchedule::new(
            0,
            [
                0xc9, 0x9e, 0x8f, 0xf1, 0xec, 0x86, 0x29, 0xe4, 0xe1, 0x29, 0x0e, 0x14, 0x46, 0x2a,
                0xe8, 0xaf,
            ],
            true,
        );
        let mut b = BootAnnounceSchedule::new(
            0,
            [
                0x83, 0xa0, 0x9e, 0xd8, 0x07, 0xa0, 0xa7, 0xc6, 0x31, 0x38, 0x6d, 0xea, 0xa0, 0x44,
                0x8f, 0xb9,
            ],
            true,
        );
        a.mark_attempted(0);
        b.mark_attempted(0);
        assert_eq!(a.next_seconds(), 8);
        assert_eq!(b.next_seconds(), 8);
        a.mark_attempted(8);
        b.mark_attempted(8);
        assert_eq!(a.next_seconds(), 26);
        assert_eq!(b.next_seconds(), 43);
        assert_eq!(b.next_seconds() - a.next_seconds(), 17);

        a.mark_attempted(26);
        b.mark_attempted(43);
        assert_eq!(b.next(), ScheduledAnnounce::LxmfDelivery);
        assert_eq!(a.next(), ScheduledAnnounce::LxmfDelivery);
        assert_eq!(a.next_seconds(), 34);
        assert_eq!(b.next_seconds(), 51);

        // Model the exact immediate/+5-second opportunities produced by pinned
        // Rete for both destinations after the intentionally colliding initial
        // pair. The minimum gap is the product's three-second guard.
        let mut opportunities = [
            26,
            26 + config::ANNOUNCE_NATIVE_RETRANSMIT_SECONDS,
            34,
            34 + config::ANNOUNCE_NATIVE_RETRANSMIT_SECONDS,
            43,
            43 + config::ANNOUNCE_NATIVE_RETRANSMIT_SECONDS,
            51,
            51 + config::ANNOUNCE_NATIVE_RETRANSMIT_SECONDS,
        ];
        opportunities.sort_unstable();
        assert_eq!(opportunities, [26, 31, 34, 39, 43, 48, 51, 56]);
        assert!(opportunities.windows(2).all(|pair| {
            pair[1] - pair[0] >= config::ANNOUNCE_MINIMUM_EMISSION_SEPARATION_SECONDS
        }));

        a.mark_attempted(34);
        b.mark_attempted(51);
        assert_eq!(a.next_seconds(), 64);
        assert_eq!(b.next_seconds(), 81);
        assert_eq!(b.next_seconds() - a.next_seconds(), 17);
    }

    #[test]
    fn phase_is_bounded_and_deadlines_saturate() {
        for seed in 0_u32..=1_024 {
            let mut destination = [0; 16];
            destination[..4].copy_from_slice(&seed.to_le_bytes());
            let mut schedule = BootAnnounceSchedule::new(0, destination, false);
            schedule.mark_attempted(0);
            assert!(schedule.next_seconds() >= config::ANNOUNCE_BOOTSTRAP_BASE_SECONDS);
            assert!(
                schedule.next_seconds()
                    < config::ANNOUNCE_BOOTSTRAP_BASE_SECONDS
                        + config::ANNOUNCE_BOOTSTRAP_PHASE_SLOTS
            );
        }

        let mut saturated = BootAnnounceSchedule::new(u64::MAX, [0; 16], true);
        saturated.mark_attempted(u64::MAX);
        assert_eq!(saturated.next_seconds(), u64::MAX);
        assert_eq!(
            saturated.due(u64::MAX),
            Some(ScheduledAnnounce::LxmfDelivery)
        );
    }
}
