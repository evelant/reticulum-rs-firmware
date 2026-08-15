//! Per-boot announce ordering over a durably reserved epoch.

use reticulum_announce_clock::{AnnounceOrdinal, BootEpoch, MAX_ANNOUNCE_ORDINAL};
use reticulum_node_core::AnnounceEmissionTime;
use reticulum_radio_interface::LoRaProfile;

use crate::config;

/// One local destination selected by the boot announce scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduledAnnounce {
    /// The node's primary transport destination.
    Primary,
    /// The optional local `lxmf.delivery` destination.
    LxmfDelivery,
    /// The local `nomadnetwork.node` destination.
    NomadNode,
}

/// Profile-derived quiet intervals for local service announce cycles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootAnnounceTiming {
    airtime_slot_seconds: u64,
    destination_spacing_seconds: u64,
    first_retry_base_seconds: u64,
    retry_spacing_seconds: u64,
}

impl BootAnnounceTiming {
    /// Derive bootstrap timing from the exact largest local announce airtime.
    pub fn for_profile(profile: LoRaProfile) -> Self {
        let airtime_us = profile
            .rnode_packet_airtime(config::ANNOUNCE_BOOTSTRAP_MAXIMUM_PACKET_BYTES)
            .expect("the largest bootstrap announce fits the base RNS MTU")
            .aggregate_time_on_air_us();
        let airtime_slot_seconds = airtime_us.saturating_add(999_999) / 1_000_000;
        let airtime_slot_seconds = airtime_slot_seconds.max(1);
        let profile_extension = airtime_slot_seconds.saturating_sub(1);
        let destination_spacing_seconds = config::ANNOUNCE_NATIVE_RETRANSMIT_SECONDS
            .saturating_add(config::ANNOUNCE_MINIMUM_EMISSION_SEPARATION_SECONDS)
            .saturating_add(profile_extension);
        let first_retry_base_seconds =
            destination_spacing_seconds.saturating_add(config::ANNOUNCE_NATIVE_RETRANSMIT_SECONDS);
        // At the baseline one-second slot this preserves the established
        // 38-second quiet interval: four destination spacings, one native
        // retransmission window, and one complete announce airtime slot.
        let retry_spacing_seconds = destination_spacing_seconds
            .saturating_mul(4)
            .saturating_add(config::ANNOUNCE_NATIVE_RETRANSMIT_SECONDS)
            .saturating_add(airtime_slot_seconds);
        Self {
            airtime_slot_seconds,
            destination_spacing_seconds,
            first_retry_base_seconds,
            retry_spacing_seconds,
        }
    }

    /// Whole-second ceiling of the largest local announce's actual airtime.
    pub const fn airtime_slot_seconds(self) -> u64 {
        self.airtime_slot_seconds
    }

    /// Quiet interval between distinct local destinations.
    pub const fn destination_spacing_seconds(self) -> u64 {
        self.destination_spacing_seconds
    }

    /// Earliest interval before the first post-boot retry cycle.
    pub const fn first_retry_base_seconds(self) -> u64 {
        self.first_retry_base_seconds
    }

    /// Quiet interval before the second post-boot retry cycle.
    pub const fn retry_spacing_seconds(self) -> u64 {
        self.retry_spacing_seconds
    }
}

/// Coalescing result for one authenticated manual announce request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualAnnounceRequestDisposition {
    /// A fresh three-destination cycle was queued.
    Queued,
    /// A manual cycle was already pending and remains the sole queued cycle.
    AlreadyPending,
}

/// Volatile, spacing-aware manual service announce cycle.
///
/// A button press queues Primary, optional LXMF Delivery, and NomadNet as
/// independent events. Repeated presses coalesce until the complete cycle is
/// consumed. This keeps the manual path inside the same fair announce lane as
/// periodic traffic and avoids back-to-back LoRa service announcements.
pub struct ManualAnnounceSchedule {
    next: Option<ScheduledAnnounce>,
    next_seconds: u64,
    lxmf_enabled: bool,
    timing: BootAnnounceTiming,
}

impl ManualAnnounceSchedule {
    /// Construct an idle manual schedule.
    pub const fn new(lxmf_enabled: bool, timing: BootAnnounceTiming) -> Self {
        Self {
            next: None,
            next_seconds: 0,
            lxmf_enabled,
            timing,
        }
    }

    /// Coalesce or queue one complete manual service-announce cycle.
    pub fn request(&mut self, now_seconds: u64) -> ManualAnnounceRequestDisposition {
        if self.next.is_some() {
            return ManualAnnounceRequestDisposition::AlreadyPending;
        }
        self.next = Some(ScheduledAnnounce::Primary);
        self.next_seconds = now_seconds;
        ManualAnnounceRequestDisposition::Queued
    }

    /// Next manual destination once its quiet interval has elapsed.
    pub const fn due(&self, now_seconds: u64) -> Option<ScheduledAnnounce> {
        if now_seconds >= self.next_seconds {
            self.next
        } else {
            None
        }
    }

    /// Retain the selected destination after protocol admission pressure.
    pub fn defer_attempt(&mut self, now_seconds: u64) {
        if self.next.is_some() {
            self.next_seconds =
                now_seconds.saturating_add(config::ANNOUNCE_ADMISSION_RETRY_SECONDS);
        }
    }

    /// Advance after the selected destination is admitted or disabled.
    pub fn mark_attempted(&mut self, now_seconds: u64) {
        self.next = match self.next {
            Some(ScheduledAnnounce::Primary) if self.lxmf_enabled => {
                Some(ScheduledAnnounce::LxmfDelivery)
            }
            Some(ScheduledAnnounce::Primary | ScheduledAnnounce::LxmfDelivery) => {
                Some(ScheduledAnnounce::NomadNode)
            }
            Some(ScheduledAnnounce::NomadNode) | None => None,
        };
        if self.next.is_some() {
            self.next_seconds =
                now_seconds.saturating_add(self.timing.destination_spacing_seconds());
        }
    }

    /// Whether one manual cycle is waiting or in progress.
    pub const fn is_pending(&self) -> bool {
        self.next.is_some()
    }
}

/// Boot-burst and steady-state scheduling for local destinations.
///
/// A transport node immediately rebroadcasts a newly received announce. LoRa
/// is half duplex, so sending primary and service announces back-to-back makes
/// a peer receive the primary, begin its rebroadcast, and miss the service
/// announce. This scheduler therefore emits at most one local destination per
/// event and leaves a profile-derived quiet interval before the optional LXMF
/// service. The Nomad node follows at the same spacing, whether or not LXMF is
/// enabled. Identity-phased initial and retry cycles spread peer bootstrap
/// traffic by whole slots derived from the largest local announce's airtime.
pub struct BootAnnounceSchedule {
    next_seconds: u64,
    next: ScheduledAnnounce,
    lxmf_enabled: bool,
    bootstrap_retries_remaining: u8,
    phase_seconds: u64,
    timing: BootAnnounceTiming,
}

impl BootAnnounceSchedule {
    /// Start with an identity-staggered primary announce.
    pub fn new(
        now_seconds: u64,
        primary_destination: [u8; 16],
        lxmf_enabled: bool,
        timing: BootAnnounceTiming,
    ) -> Self {
        let seed = u32::from_le_bytes([
            primary_destination[0],
            primary_destination[1],
            primary_destination[2],
            primary_destination[3],
        ]);
        let initial_phase_slot = u64::from(seed ^ seed.rotate_right(16))
            % config::ANNOUNCE_BOOTSTRAP_INITIAL_PHASE_SLOTS;
        let initial_phase_seconds =
            initial_phase_slot.saturating_mul(timing.airtime_slot_seconds());
        Self {
            next_seconds: now_seconds.saturating_add(initial_phase_seconds),
            next: ScheduledAnnounce::Primary,
            lxmf_enabled,
            bootstrap_retries_remaining: config::ANNOUNCE_BOOTSTRAP_RETRIES,
            phase_seconds: (u64::from(seed) % config::ANNOUNCE_BOOTSTRAP_PHASE_SLOTS)
                .saturating_mul(timing.airtime_slot_seconds()),
            timing,
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
        match self.next {
            ScheduledAnnounce::Primary => {
                self.next = if self.lxmf_enabled {
                    ScheduledAnnounce::LxmfDelivery
                } else {
                    ScheduledAnnounce::NomadNode
                };
                self.next_seconds =
                    now_seconds.saturating_add(self.timing.destination_spacing_seconds());
                return;
            }
            ScheduledAnnounce::LxmfDelivery => {
                self.next = ScheduledAnnounce::NomadNode;
                self.next_seconds =
                    now_seconds.saturating_add(self.timing.destination_spacing_seconds());
                return;
            }
            ScheduledAnnounce::NomadNode => {}
        }

        self.next = ScheduledAnnounce::Primary;
        let delay = match self.bootstrap_retries_remaining {
            remaining if remaining == config::ANNOUNCE_BOOTSTRAP_RETRIES => self
                .timing
                .first_retry_base_seconds()
                .saturating_add(self.phase_seconds),
            1.. => self.timing.retry_spacing_seconds(),
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
/// with a Nomad destination consumes three values per cadence and the
/// 1,048,576-value per-boot space spans almost 20 years; exhaustion still
/// suppresses further local announces
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
    use rand_core::{CryptoRng, RngCore};
    use reticulum_announce_clock::{MAX_ANNOUNCE_EMISSION_TIMESTAMP, MAX_BOOT_EPOCH};
    use reticulum_board_e290_radio::{
        E290_NA915_DEFAULT_PROFILE, E290Na915TxPower, E290RadioConfiguration,
    };
    use reticulum_node_core::{
        MonotonicSeconds, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId,
    };

    use crate::nomad_responder::{
        NOMAD_NODE_ANNOUNCE_APP_DATA, NOMAD_NODE_APPLICATION_NAME, NOMAD_NODE_ASPECTS,
    };

    const NODE_A: [u8; 16] = [
        0xc9, 0x9e, 0x8f, 0xf1, 0xec, 0x86, 0x29, 0xe4, 0xe1, 0x29, 0x0e, 0x14, 0x46, 0x2a, 0xe8,
        0xaf,
    ];
    const NODE_B: [u8; 16] = [
        0x83, 0xa0, 0x9e, 0xd8, 0x07, 0xa0, 0xa7, 0xc6, 0x31, 0x38, 0x6d, 0xea, 0xa0, 0x44, 0x8f,
        0xb9,
    ];

    #[derive(Default)]
    struct CounterRng(u8);

    impl RngCore for CounterRng {
        fn next_u32(&mut self) -> u32 {
            let mut bytes = [0; 4];
            self.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        }

        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            for byte in destination {
                self.0 = self.0.wrapping_add(1);
                *byte = self.0;
            }
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for CounterRng {}

    fn epoch(value: u32) -> BootEpoch {
        BootEpoch::new(value).expect("test epoch must fit")
    }

    fn default_timing() -> BootAnnounceTiming {
        BootAnnounceTiming::for_profile(E290_NA915_DEFAULT_PROFILE)
    }

    fn slow_timing() -> BootAnnounceTiming {
        let slow = E290RadioConfiguration::try_from_profile(
            914_875_000,
            125_000,
            10,
            5,
            E290Na915TxPower::Dbm22,
        )
        .expect("the field-test SF10 profile is board-supported");
        BootAnnounceTiming::for_profile(slow.profile())
    }

    #[test]
    fn reviewed_airtime_bound_covers_every_current_boot_announce() {
        let mut node = NodeCore::<4, 3, 4, 2, 0>::new(
            NodeIdentity::from_private_key(&[0x51; 64]).expect("test identity"),
            config::RNS_APPLICATION_NAME,
            &config::RNS_PRIMARY_ASPECTS,
            NodeInstanceId::new([0x51; 16]),
            NodeConfig::transport(),
        )
        .expect("test node");
        let lxmf = node
            .register_inbound_single_destination("lxmf", &["delivery"])
            .expect("LXMF destination");
        let nomad = node
            .register_inbound_single_destination(NOMAD_NODE_APPLICATION_NAME, &NOMAD_NODE_ASPECTS)
            .expect("Nomad destination");
        let mut rng = CounterRng::default();
        node.queue_announce(
            None,
            AnnounceEmissionTime::new(1).expect("primary emission"),
            &mut rng,
        )
        .expect("primary announce");
        node.queue_announce_for(
            &lxmf,
            Some(&config::LXMF_DELIVERY_ANNOUNCE_APP_DATA),
            AnnounceEmissionTime::new(2).expect("LXMF emission"),
            &mut rng,
        )
        .expect("LXMF announce");
        node.queue_announce_for(
            &nomad,
            Some(NOMAD_NODE_ANNOUNCE_APP_DATA.as_bytes()),
            AnnounceEmissionTime::new(3).expect("Nomad emission"),
            &mut rng,
        )
        .expect("Nomad announce");

        let actions = node.flush_announces(MonotonicSeconds::new(3), &mut rng);
        assert_eq!(actions.packets.len(), 3);
        let mut packet_lengths = [0; 3];
        for (length, packet) in packet_lengths.iter_mut().zip(actions.packets.iter()) {
            *length = packet.bytes().len();
            assert!(*length <= config::ANNOUNCE_BOOTSTRAP_MAXIMUM_PACKET_BYTES);
        }
        packet_lengths.sort_unstable();
        assert_eq!(packet_lengths, [167, 171, 177]);
        assert_eq!(
            packet_lengths[2],
            config::ANNOUNCE_BOOTSTRAP_MAXIMUM_PACKET_BYTES,
            "the timing bound must track the largest current boot announce exactly"
        );
    }

    #[test]
    fn manual_cycle_coalesces_and_preserves_destination_spacing() {
        let timing = default_timing();
        let mut schedule = ManualAnnounceSchedule::new(true, timing);
        assert_eq!(
            schedule.request(100),
            ManualAnnounceRequestDisposition::Queued
        );
        assert_eq!(
            schedule.request(101),
            ManualAnnounceRequestDisposition::AlreadyPending
        );
        assert_eq!(schedule.due(100), Some(ScheduledAnnounce::Primary));

        schedule.mark_attempted(100);
        assert_eq!(schedule.due(100), None);
        assert_eq!(
            schedule.due(100 + timing.destination_spacing_seconds()),
            Some(ScheduledAnnounce::LxmfDelivery)
        );
        schedule.mark_attempted(200);
        assert_eq!(
            schedule.due(200 + timing.destination_spacing_seconds()),
            Some(ScheduledAnnounce::NomadNode)
        );
        schedule.mark_attempted(300);
        assert!(!schedule.is_pending());
    }

    #[test]
    fn manual_cycle_skips_disabled_lxmf_but_not_nomad() {
        let timing = default_timing();
        let mut schedule = ManualAnnounceSchedule::new(false, timing);
        schedule.request(1);
        assert_eq!(schedule.due(1), Some(ScheduledAnnounce::Primary));
        schedule.mark_attempted(1);
        assert_eq!(
            schedule.due(1 + timing.destination_spacing_seconds()),
            Some(ScheduledAnnounce::NomadNode)
        );
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
        let timing = default_timing();
        let mut schedule = BootAnnounceSchedule::new(100, [0; 16], true, timing);
        assert_eq!(schedule.due(100), Some(ScheduledAnnounce::Primary));
        schedule.mark_attempted(100);
        assert_eq!(
            schedule.next_seconds(),
            100 + timing.destination_spacing_seconds()
        );
        assert_eq!(schedule.next(), ScheduledAnnounce::LxmfDelivery);
        let lxmf_at = schedule.next_seconds();
        assert_eq!(schedule.due(lxmf_at - 1), None);
        schedule.mark_attempted(lxmf_at);
        let nomad_at = lxmf_at + timing.destination_spacing_seconds();
        assert_eq!(schedule.next_seconds(), nomad_at);
        assert_eq!(schedule.next(), ScheduledAnnounce::NomadNode);
        schedule.mark_attempted(nomad_at);
        let first_retry = nomad_at + timing.first_retry_base_seconds();
        assert_eq!(schedule.next_seconds(), first_retry);
        assert_eq!(schedule.next(), ScheduledAnnounce::Primary);
        schedule.mark_attempted(first_retry);
        let first_retry_lxmf = first_retry + timing.destination_spacing_seconds();
        schedule.mark_attempted(first_retry_lxmf);
        let first_retry_nomad = first_retry_lxmf + timing.destination_spacing_seconds();
        schedule.mark_attempted(first_retry_nomad);
        let second_retry = first_retry_nomad + timing.retry_spacing_seconds();
        assert_eq!(schedule.next_seconds(), second_retry);
        schedule.mark_attempted(second_retry);
        let second_retry_lxmf = second_retry + timing.destination_spacing_seconds();
        schedule.mark_attempted(second_retry_lxmf);
        let second_retry_nomad = second_retry_lxmf + timing.destination_spacing_seconds();
        schedule.mark_attempted(second_retry_nomad);
        let periodic = second_retry_nomad + config::ANNOUNCE_INTERVAL_SECONDS;
        assert_eq!(schedule.next_seconds(), periodic);
    }

    #[test]
    fn admission_deferral_retains_destination_and_bootstrap_budget() {
        let timing = default_timing();
        let mut schedule = BootAnnounceSchedule::new(100, [0; 16], true, timing);

        schedule.defer_attempt(100);
        assert_eq!(schedule.next(), ScheduledAnnounce::Primary);
        assert_eq!(schedule.due(100), None);
        assert_eq!(schedule.due(101), Some(ScheduledAnnounce::Primary));

        schedule.mark_attempted(101);
        assert_eq!(schedule.next(), ScheduledAnnounce::LxmfDelivery);
        let lxmf_at = 101 + timing.destination_spacing_seconds();
        assert_eq!(schedule.next_seconds(), lxmf_at);
        schedule.defer_attempt(lxmf_at);
        assert_eq!(schedule.next(), ScheduledAnnounce::LxmfDelivery);
        assert_eq!(
            schedule.due(lxmf_at + config::ANNOUNCE_ADMISSION_RETRY_SECONDS),
            Some(ScheduledAnnounce::LxmfDelivery)
        );

        let lxmf_retry_at = lxmf_at + config::ANNOUNCE_ADMISSION_RETRY_SECONDS;
        schedule.mark_attempted(lxmf_retry_at);
        assert_eq!(schedule.next(), ScheduledAnnounce::NomadNode);
        let nomad_at = lxmf_retry_at + timing.destination_spacing_seconds();
        assert_eq!(schedule.next_seconds(), nomad_at);
        schedule.defer_attempt(nomad_at);
        assert_eq!(schedule.next(), ScheduledAnnounce::NomadNode);
        assert_eq!(
            schedule.due(nomad_at + config::ANNOUNCE_ADMISSION_RETRY_SECONDS),
            Some(ScheduledAnnounce::NomadNode)
        );

        let nomad_retry_at = nomad_at + config::ANNOUNCE_ADMISSION_RETRY_SECONDS;
        schedule.mark_attempted(nomad_retry_at);
        assert_eq!(schedule.next(), ScheduledAnnounce::Primary);
        assert_eq!(
            schedule.next_seconds(),
            nomad_retry_at + timing.first_retry_base_seconds()
        );
    }

    #[test]
    fn lxmf_disabled_profile_schedules_primary_then_nomad() {
        let timing = default_timing();
        let mut schedule = BootAnnounceSchedule::new(100, [0; 16], false, timing);
        assert_eq!(schedule.due(100), Some(ScheduledAnnounce::Primary));
        schedule.mark_attempted(100);
        assert_eq!(
            schedule.next_seconds(),
            100 + timing.destination_spacing_seconds()
        );
        assert_eq!(schedule.next(), ScheduledAnnounce::NomadNode);
        let nomad_at = schedule.next_seconds();
        schedule.mark_attempted(nomad_at);
        let retry_at = nomad_at + timing.first_retry_base_seconds();
        assert_eq!(schedule.next_seconds(), retry_at);
        assert_eq!(schedule.next(), ScheduledAnnounce::Primary);
        schedule.mark_attempted(retry_at);
        assert_eq!(
            schedule.next_seconds(),
            retry_at + timing.destination_spacing_seconds()
        );
        assert_eq!(schedule.next(), ScheduledAnnounce::NomadNode);
    }

    #[test]
    fn known_e290_nodes_have_identity_staggered_initial_and_retry_cycles() {
        let timing = default_timing();
        let mut a = BootAnnounceSchedule::new(0, NODE_A, true, timing);
        let mut b = BootAnnounceSchedule::new(0, NODE_B, true, timing);

        let a_initial = a.next_seconds();
        let b_initial = b.next_seconds();
        assert_ne!(a_initial, b_initial);
        assert_eq!(a_initial % timing.airtime_slot_seconds(), 0);
        assert_eq!(b_initial % timing.airtime_slot_seconds(), 0);
        assert!(a_initial < config::ANNOUNCE_BOOTSTRAP_INITIAL_PHASE_SLOTS);
        assert!(b_initial < config::ANNOUNCE_BOOTSTRAP_INITIAL_PHASE_SLOTS);

        for (schedule, initial) in [(&mut a, a_initial), (&mut b, b_initial)] {
            schedule.mark_attempted(initial);
            let lxmf_at = initial + timing.destination_spacing_seconds();
            schedule.mark_attempted(lxmf_at);
            let nomad_at = lxmf_at + timing.destination_spacing_seconds();
            schedule.mark_attempted(nomad_at);
        }
        assert_ne!(a.next_seconds(), b.next_seconds());
        assert_eq!(
            a.next_seconds().abs_diff(b.next_seconds()),
            30 * timing.airtime_slot_seconds()
        );
    }

    #[test]
    fn slow_profile_expands_slots_and_every_bootstrap_quiet_interval_from_airtime() {
        let fast = default_timing();
        let slow = slow_timing();
        assert!(slow.airtime_slot_seconds() > fast.airtime_slot_seconds());
        assert!(slow.destination_spacing_seconds() > fast.destination_spacing_seconds());
        assert!(slow.first_retry_base_seconds() > fast.first_retry_base_seconds());
        assert!(slow.retry_spacing_seconds() > fast.retry_spacing_seconds());

        let mut fast_schedule = BootAnnounceSchedule::new(0, NODE_A, true, fast);
        let mut slow_schedule = BootAnnounceSchedule::new(0, NODE_A, true, slow);
        assert_eq!(
            slow_schedule.next_seconds(),
            fast_schedule.next_seconds() * slow.airtime_slot_seconds()
        );
        let fast_primary = fast_schedule.next_seconds();
        let slow_primary = slow_schedule.next_seconds();
        fast_schedule.mark_attempted(fast_primary);
        slow_schedule.mark_attempted(slow_primary);
        assert_eq!(
            fast_schedule.next_seconds() - fast_primary,
            fast.destination_spacing_seconds()
        );
        assert_eq!(
            slow_schedule.next_seconds() - slow_primary,
            slow.destination_spacing_seconds()
        );
    }

    #[test]
    fn phase_is_bounded_and_deadlines_saturate() {
        let timing = default_timing();
        for seed in 0_u32..=1_024 {
            let mut destination = [0; 16];
            destination[..4].copy_from_slice(&seed.to_le_bytes());
            let mut schedule = BootAnnounceSchedule::new(0, destination, false, timing);
            let primary_at = schedule.next_seconds();
            assert!(
                primary_at
                    < config::ANNOUNCE_BOOTSTRAP_INITIAL_PHASE_SLOTS
                        * timing.airtime_slot_seconds()
            );
            schedule.mark_attempted(primary_at);
            let nomad_at = primary_at + timing.destination_spacing_seconds();
            schedule.mark_attempted(nomad_at);
            assert!(schedule.next_seconds() >= nomad_at + timing.first_retry_base_seconds());
            assert!(
                schedule.next_seconds()
                    < nomad_at
                        + timing.first_retry_base_seconds()
                        + config::ANNOUNCE_BOOTSTRAP_PHASE_SLOTS * timing.airtime_slot_seconds()
            );
        }

        let mut saturated = BootAnnounceSchedule::new(u64::MAX, [0; 16], true, timing);
        saturated.mark_attempted(u64::MAX);
        assert_eq!(saturated.next_seconds(), u64::MAX);
        assert_eq!(
            saturated.due(u64::MAX),
            Some(ScheduledAnnounce::LxmfDelivery)
        );
    }
}
