use super::*;

#[derive(Clone, Copy)]
struct RegistrySlot {
    last_generation: u64,
    descriptor: Option<InterfaceDescriptor>,
}

impl RegistrySlot {
    const fn vacant() -> Self {
        Self {
            last_generation: 0,
            descriptor: None,
        }
    }
}

/// Failure to register one interface in a fixed queue slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceRegistrationError {
    /// Queue index is outside the registry's fixed capacity.
    QueueOutsideRegistry(InterfaceQueueId),
    /// The requested queue already has an active registration.
    QueueOccupied(InterfaceDescriptor),
    /// Another queue already owns the stable Reticulum interface identity.
    DuplicateInterface {
        /// Interface identity already registered elsewhere.
        interface: PacketInterfaceId,
        /// Authoritative existing queue.
        queue: InterfaceQueueId,
    },
    /// The queue-local generation counter cannot advance safely.
    GenerationExhausted(InterfaceQueueId),
}

/// Failure to validate or mutate one generation-safe interface lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceLeaseError {
    /// Queue index is outside the registry's fixed capacity.
    QueueOutsideRegistry(InterfaceQueueId),
    /// Queue has no active interface registration.
    QueueVacant(InterfaceQueueId),
    /// Queue is active, but no longer under the supplied interface generation.
    Stale {
        /// Lease rejected by the registry.
        supplied: InterfaceLease,
        /// Current authoritative descriptor.
        current: InterfaceDescriptor,
    },
    /// Reconfiguration cannot allocate another generation.
    GenerationExhausted(InterfaceQueueId),
    /// Another queue already owns the replacement stable interface identity.
    DuplicateInterface {
        /// Interface identity already registered elsewhere.
        interface: PacketInterfaceId,
        /// Authoritative existing queue.
        queue: InterfaceQueueId,
    },
}

/// Failure to derive node-core's compact eligible-interface snapshot from the
/// authoritative registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EligibleInterfaceSetError {
    /// A registered identity cannot fit node-core's current 0-through-63
    /// compact route profile.
    InterfaceOutsideProfile(InterfaceDescriptor),
}

/// Failure to derive the exact egress set for an ingress-derived announce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnounceEgressSetError {
    /// The supplied ingress interface is not registered.
    SourceUnavailable(PacketInterfaceId),
    /// A registered identity cannot fit the compact route profile.
    InterfaceOutsideProfile(InterfaceDescriptor),
}

/// Failure to derive recursive path-search egress for one ingress interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecursivePathSearchEgressSetError {
    /// The supplied ingress interface is not registered.
    SourceUnavailable(PacketInterfaceId),
    /// A registered identity cannot fit the compact route profile.
    InterfaceOutsideProfile(InterfaceDescriptor),
}

/// Fixed-capacity authoritative packet-interface registry.
///
/// Lookup is a bounded linear scan. The intended embedded profile has only a
/// handful of simultaneously enabled interfaces, so this keeps storage and
/// failure behavior explicit without allocating a map.
pub struct InterfaceRegistry<const SLOTS: usize> {
    slots: [RegistrySlot; SLOTS],
}

impl<const SLOTS: usize> InterfaceRegistry<SLOTS> {
    /// Construct an empty registry.
    pub const fn new() -> Self {
        const {
            assert!(SLOTS > 0, "interface registry must have at least one slot");
            assert!(
                SLOTS <= (u16::MAX as usize) + 1,
                "interface registry slots must fit InterfaceQueueId"
            );
        }
        Self {
            slots: [const { RegistrySlot::vacant() }; SLOTS],
        }
    }

    /// Fixed number of actor queue slots.
    pub const fn capacity(&self) -> usize {
        SLOTS
    }

    /// Register one stable interface identity in a vacant queue.
    pub fn register(
        &mut self,
        queue: InterfaceQueueId,
        interface: PacketInterfaceId,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceRegistrationError> {
        let index = queue.index();
        let Some(slot) = self.slots.get(index).copied() else {
            return Err(InterfaceRegistrationError::QueueOutsideRegistry(queue));
        };
        if let Some(descriptor) = slot.descriptor {
            return Err(InterfaceRegistrationError::QueueOccupied(descriptor));
        }
        if let Some(existing) = self.descriptor(interface) {
            return Err(InterfaceRegistrationError::DuplicateInterface {
                interface,
                queue: existing.lease.queue,
            });
        }
        let generation = slot
            .last_generation
            .checked_add(1)
            .ok_or(InterfaceRegistrationError::GenerationExhausted(queue))?;
        let descriptor = InterfaceDescriptor {
            lease: InterfaceLease {
                interface,
                queue,
                generation: InterfaceGeneration(generation),
            },
            online,
            properties,
        };
        self.slots[index] = RegistrySlot {
            last_generation: generation,
            descriptor: Some(descriptor),
        };
        Ok(descriptor)
    }

    /// Current descriptor for one stable Reticulum interface identity.
    pub fn descriptor(&self, interface: PacketInterfaceId) -> Option<InterfaceDescriptor> {
        self.slots
            .iter()
            .filter_map(|slot| slot.descriptor)
            .find(|descriptor| descriptor.lease.interface == interface)
    }

    /// Current descriptor assigned to one fixed queue slot.
    pub fn descriptor_at(&self, queue: InterfaceQueueId) -> Option<InterfaceDescriptor> {
        self.slots
            .get(queue.index())
            .and_then(|slot| slot.descriptor)
    }

    /// Derive the sole current online-interface snapshot supplied to
    /// node-core route resolution.
    ///
    /// Firmware must use this instead of maintaining a second enabled-ID
    /// list. Every registered identity is checked even when offline so a
    /// latent out-of-profile registration cannot become an unexpected route
    /// failure merely by transitioning online later.
    pub fn eligible_interfaces(&self) -> Result<InterfaceSet, EligibleInterfaceSetError> {
        let mut eligible = InterfaceSet::empty();
        for descriptor in self.slots.iter().filter_map(|slot| slot.descriptor) {
            let Some(with_interface) = eligible.with(descriptor.lease.interface) else {
                return Err(EligibleInterfaceSetError::InterfaceOutsideProfile(
                    descriptor,
                ));
            };
            if descriptor.online {
                eligible = with_interface;
            }
        }
        Ok(eligible)
    }

    /// Derive the exact currently-online egress set for an announce learned on
    /// `source`.
    ///
    /// Point-to-point topology excludes reflection to the source interface.
    /// The supported Reticulum mode matrix additionally blocks only
    /// boundary-to-internal propagation; internal-to-boundary and all Full
    /// interactions remain allowed.
    pub fn announce_egress_interfaces(
        &self,
        source: PacketInterfaceId,
    ) -> Result<InterfaceSet, AnnounceEgressSetError> {
        let Some(source_descriptor) = self.descriptor(source) else {
            return Err(AnnounceEgressSetError::SourceUnavailable(source));
        };
        let source_properties = source_descriptor.properties;
        let mut egress = InterfaceSet::empty();
        for descriptor in self.slots.iter().filter_map(|slot| slot.descriptor) {
            let interface = descriptor.lease.interface;
            let Some(with_interface) = egress.with(interface) else {
                return Err(AnnounceEgressSetError::InterfaceOutsideProfile(descriptor));
            };
            if !descriptor.online {
                continue;
            }
            if interface == source && source_properties.topology == InterfaceTopology::PointToPoint
            {
                continue;
            }
            if !descriptor
                .properties
                .announce_mode
                .accepts_from(source_properties.announce_mode)
            {
                continue;
            }
            egress = with_interface;
        }
        Ok(egress)
    }

    /// Derive recursive unknown-path search egress for `source`.
    ///
    /// Every recursive request excludes its ingress interface, including a
    /// shared medium. Disabled sources produce an empty set. Boundary sources
    /// select only Boundary and Gateway targets; Unrestricted and Gateway
    /// sources select every other online interface.
    pub fn recursive_path_search_egress_interfaces(
        &self,
        source: PacketInterfaceId,
    ) -> Result<Option<InterfaceSet>, RecursivePathSearchEgressSetError> {
        let Some(source_descriptor) = self.descriptor(source) else {
            return Err(RecursivePathSearchEgressSetError::SourceUnavailable(source));
        };
        let source_mode = source_descriptor.properties.recursive_path_search_mode;
        if source_mode == RecursivePathSearchMode::Disabled {
            return Ok(None);
        }
        let mut egress = InterfaceSet::empty();
        for descriptor in self.slots.iter().filter_map(|slot| slot.descriptor) {
            let interface = descriptor.lease.interface;
            let Some(with_interface) = egress.with(interface) else {
                return Err(RecursivePathSearchEgressSetError::InterfaceOutsideProfile(
                    descriptor,
                ));
            };
            if !descriptor.online || interface == source {
                continue;
            }
            let selected = match source_mode {
                RecursivePathSearchMode::Disabled => unreachable!("disabled source returned early"),
                RecursivePathSearchMode::Boundary => descriptor
                    .properties
                    .recursive_path_search_mode
                    .boundary_search_target(),
                RecursivePathSearchMode::Unrestricted | RecursivePathSearchMode::Gateway => true,
            };
            if selected {
                egress = with_interface;
            }
        }
        Ok(Some(egress))
    }

    /// Validate one lease against the current authoritative record.
    pub fn validate(
        &self,
        lease: InterfaceLease,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        let Some(slot) = self.slots.get(lease.queue.index()) else {
            return Err(InterfaceLeaseError::QueueOutsideRegistry(lease.queue));
        };
        let Some(current) = slot.descriptor else {
            return Err(InterfaceLeaseError::QueueVacant(lease.queue));
        };
        if current.lease != lease {
            return Err(InterfaceLeaseError::Stale {
                supplied: lease,
                current,
            });
        }
        Ok(current)
    }

    /// Change whether a current lease accepts new outbound jobs.
    ///
    /// This does not change the generation. Previously accepted owners can
    /// still complete under the same exact lease.
    pub fn set_online(
        &mut self,
        lease: InterfaceLease,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        let mut descriptor = self.validate(lease)?;
        descriptor.online = online;
        self.slots[lease.queue.index()].descriptor = Some(descriptor);
        Ok(descriptor)
    }

    /// Atomically replace logical MTU and opaque actor configuration.
    ///
    /// Reconfiguration advances the queue generation. Jobs already accepted
    /// under the former generation remain uniquely owned, but their eventual
    /// completions are reported as stale for explicit recovery instead of
    /// being silently attributed to the new configuration.
    pub fn reconfigure(
        &mut self,
        lease: InterfaceLease,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        let current = self.validate(lease)?;
        let generation = lease
            .generation
            .0
            .checked_add(1)
            .ok_or(InterfaceLeaseError::GenerationExhausted(lease.queue))?;
        let descriptor = InterfaceDescriptor {
            lease: InterfaceLease {
                generation: InterfaceGeneration(generation),
                ..current.lease
            },
            online,
            properties,
        };
        self.slots[lease.queue.index()] = RegistrySlot {
            last_generation: generation,
            descriptor: Some(descriptor),
        };
        Ok(descriptor)
    }

    /// Remove a current interface registration while preserving its consumed
    /// generation so a later registration cannot resurrect an old lease.
    pub fn unregister(
        &mut self,
        lease: InterfaceLease,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        let descriptor = self.validate(lease)?;
        self.slots[lease.queue.index()].descriptor = None;
        Ok(descriptor)
    }
}

impl<const SLOTS: usize> Default for InterfaceRegistry<SLOTS> {
    fn default() -> Self {
        Self::new()
    }
}
