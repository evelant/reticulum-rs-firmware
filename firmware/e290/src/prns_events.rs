//! Owned application events copied from PRNS's synchronous callback.
//!
//! PRNS publishes a borrowed delivery event and then continues its ordinary
//! proof behavior. Product services cannot retain that borrow across storage
//! work, so this module copies only application payloads the product owns into
//! a separately bounded allocation budget. Exhaustion is observable product
//! pressure; it never changes PRNS proof timing or deduplication.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use allocator_api2::alloc::{Allocator, Global};
use allocator_api2::vec::Vec;
use personal_rns::identity::OpenedBy;
use personal_rns::interfaces::InterfaceId;
use personal_rns::routing::delivery::{Delivery, SingleDelivery};
use personal_rns::routing::links::LinkId;
use personal_rns::routing::links::resources::ResourceHash;
use personal_rns::runtime::{Message, PrnsEvent};
use personal_rns::units::InstantMillis;
use personal_rns::wire::{DestinationHash, WireContext};
use reticulum_lxmf_ingress::CarrierIngress;
use reticulum_lxmf_model::{InboundInterfaceId, InboundTransportObservation};

use crate::prns_requests::PrnsApplicationState;

/// Shared byte budget for application-owned copies of borrowed PRNS payloads.
pub struct ApplicationPayloadBudget {
    capacity: usize,
    in_use: AtomicUsize,
    pressure_events: AtomicU32,
}

impl ApplicationPayloadBudget {
    /// Construct a fixed aggregate payload budget.
    pub const fn new(capacity: usize) -> Self {
        Self {
            capacity,
            in_use: AtomicUsize::new(0),
            pressure_events: AtomicU32::new(0),
        }
    }

    fn try_claim(&self, bytes: usize) -> bool {
        let mut current = self.in_use.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                self.note_pressure();
                return false;
            };
            if next > self.capacity {
                self.note_pressure();
                return false;
            }
            match self.in_use.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn release(&self, bytes: usize) {
        let prior = self.in_use.fetch_sub(bytes, Ordering::AcqRel);
        debug_assert!(prior >= bytes);
    }

    fn note_pressure(&self) {
        self.pressure_events.fetch_add(1, Ordering::Relaxed);
    }

    /// Maximum number of simultaneously owned payload bytes.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Payload bytes currently owned by queued or active product events.
    pub fn in_use(&self) -> usize {
        self.in_use.load(Ordering::Acquire)
    }

    /// Saturating count of rejected payload copies.
    pub fn pressure_events(&self) -> u32 {
        self.pressure_events.load(Ordering::Relaxed)
    }
}

/// Why a borrowed PRNS payload could not become an owned application event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationEventCopyError {
    /// The aggregate product payload budget had no room.
    PayloadBudgetFull,
    /// The selected allocator could not reserve the claimed bytes.
    AllocationFailed,
}

/// Allocator-backed payload whose drop returns its exact aggregate budget.
pub struct OwnedApplicationPayload<A: Allocator = Global> {
    bytes: Vec<u8, A>,
    budget: &'static ApplicationPayloadBudget,
    claimed: usize,
}

impl<A: Allocator + Default> OwnedApplicationPayload<A> {
    fn try_copy(
        bytes: &[u8],
        budget: &'static ApplicationPayloadBudget,
    ) -> Result<Self, ApplicationEventCopyError> {
        if !budget.try_claim(bytes.len()) {
            return Err(ApplicationEventCopyError::PayloadBudgetFull);
        }
        let mut owned = Vec::new_in(A::default());
        if owned.try_reserve_exact(bytes.len()).is_err() {
            budget.release(bytes.len());
            budget.note_pressure();
            return Err(ApplicationEventCopyError::AllocationFailed);
        }
        owned.extend_from_slice(bytes);
        Ok(Self {
            bytes: owned,
            budget,
            claimed: bytes.len(),
        })
    }
}

impl<A: Allocator> OwnedApplicationPayload<A> {
    /// Exact bytes copied from the synchronous PRNS delivery callback.
    pub fn as_slice(&self) -> &[u8] {
        self.bytes.as_slice()
    }
}

impl<A: Allocator> Drop for OwnedApplicationPayload<A> {
    fn drop(&mut self) {
        self.budget.release(self.claimed);
    }
}

/// One opportunistic LXMF delivery owned beyond PRNS's callback lifetime.
pub struct OwnedLxmfSingleDelivery<A: Allocator = Global> {
    destination: DestinationHash,
    context: WireContext,
    opened_by: OpenedBy,
    arrived_at: InstantMillis,
    source_interface: InterfaceId,
    plaintext: OwnedApplicationPayload<A>,
}

/// One complete ordinary PRNS Resource owned beyond the synchronous callback.
///
/// The product only arms Resources on an OTA session's identified management
/// Link. This copy preserves the PRNS-verified hash and exact opaque metadata;
/// it does not add another Resource receiver or redefine protocol settlement.
pub struct OwnedOtaResource<A: Allocator = Global> {
    link_id: LinkId,
    hash: ResourceHash,
    metadata: Option<OwnedApplicationPayload<A>>,
    data: OwnedApplicationPayload<A>,
}

impl<A: Allocator> OwnedOtaResource<A> {
    /// PRNS Link on which the application Resource completed.
    pub const fn link_id(&self) -> LinkId {
        self.link_id
    }

    /// Resource hash already verified by PRNS assembly.
    pub const fn hash(&self) -> ResourceHash {
        self.hash
    }

    /// Exact opaque Resource metadata, when supplied by the sender.
    pub fn metadata(&self) -> Option<&[u8]> {
        self.metadata
            .as_ref()
            .map(OwnedApplicationPayload::as_slice)
    }

    /// Complete verified Resource application data.
    pub fn data(&self) -> &[u8] {
        self.data.as_slice()
    }
}

impl<A: Allocator> OwnedLxmfSingleDelivery<A> {
    /// Protocol destination that admitted the delivery.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Reticulum wire context supplied with the delivery.
    pub const fn context(&self) -> WireContext {
        self.context
    }

    /// Identity key or retained ratchet that opened the packet.
    pub const fn opened_by(&self) -> OpenedBy {
        self.opened_by
    }

    /// PRNS monotonic arrival timestamp.
    pub const fn arrived_at(&self) -> InstantMillis {
        self.arrived_at
    }

    /// PRNS interface on which the packet arrived.
    pub const fn source_interface(&self) -> InterfaceId {
        self.source_interface
    }

    /// Preserve PRNS's complete interface identity as durable product evidence.
    ///
    /// This event does not carry physical signal data, so the observation is
    /// intentionally interface-only rather than synthesizing RSSI or SNR.
    pub fn ingress_observation(&self) -> InboundTransportObservation {
        InboundTransportObservation::new(
            InboundInterfaceId::new(*self.source_interface.as_bytes()),
            None,
        )
    }

    /// Exact decrypted LXMF bytes copied before PRNS continued proof handling.
    pub fn plaintext(&self) -> &[u8] {
        self.plaintext.as_slice()
    }

    /// Borrow this delivery as an ordinary transport-neutral LXMF carrier.
    ///
    /// The PRNS callback already established the local destination binding;
    /// LXMF validation neither needs nor receives a PRNS or legacy-node event.
    pub fn carrier(&self) -> CarrierIngress<'_> {
        CarrierIngress::Opportunistic {
            implied_destination: self.destination.as_bytes(),
            payload: self.plaintext(),
        }
    }
}

/// Copy an opportunistic delivery only when it targets this boot's LXMF app.
pub fn copy_lxmf_single_delivery<A>(
    event: &PrnsEvent<'_>,
    state: &PrnsApplicationState,
    budget: &'static ApplicationPayloadBudget,
) -> Result<Option<OwnedLxmfSingleDelivery<A>>, ApplicationEventCopyError>
where
    A: Allocator + Default,
{
    let PrnsEvent::Message(Message::Delivered(Delivery::Single(SingleDelivery {
        destination,
        context,
        plaintext,
        opened_by,
        arrived_at,
        source_interface,
    }))) = event
    else {
        return Ok(None);
    };
    if Some(*destination) != state.lxmf() {
        return Ok(None);
    }
    Ok(Some(OwnedLxmfSingleDelivery {
        destination: *destination,
        context: *context,
        opened_by: *opened_by,
        arrived_at: *arrived_at,
        source_interface: *source_interface,
        plaintext: OwnedApplicationPayload::try_copy(plaintext, budget)?,
    }))
}

/// Copy one complete PRNS Resource into the product's bounded payload budget.
///
/// All recipe destinations start with `AcceptNone`; product policy opens a
/// specific session Link before such an event can exist. The session owner
/// still validates the Link, metadata, ordering, and manifest before writing.
pub fn copy_ota_resource<A>(
    event: &PrnsEvent<'_>,
    budget: &'static ApplicationPayloadBudget,
) -> Result<Option<OwnedOtaResource<A>>, ApplicationEventCopyError>
where
    A: Allocator + Default,
{
    let PrnsEvent::Message(Message::Resource {
        link_id,
        hash,
        metadata,
        data,
    }) = event
    else {
        return Ok(None);
    };
    let metadata = metadata
        .map(|metadata| OwnedApplicationPayload::try_copy(metadata, budget))
        .transpose()?;
    let data = OwnedApplicationPayload::try_copy(data, budget)?;
    Ok(Some(OwnedOtaResource {
        link_id: *link_id,
        hash: *hash,
        metadata,
        data,
    }))
}

#[cfg(test)]
mod tests {
    use personal_rns::identity::IDENTITY_SECRET_KEY_LEN;
    use personal_rns::routing::delivery::SingleDelivery;

    use super::*;
    use crate::prns_applications::{ApplicationProfile, application_catalog};
    use crate::prns_requests::ManagementRequestChannel;

    const IDENTITY: [u8; IDENTITY_SECRET_KEY_LEN] = [0x63; IDENTITY_SECRET_KEY_LEN];
    static REQUESTS: ManagementRequestChannel = ManagementRequestChannel::new();
    static BUDGET: ApplicationPayloadBudget = ApplicationPayloadBudget::new(32);

    fn state() -> PrnsApplicationState {
        let catalog =
            application_catalog(&IDENTITY, b"", ApplicationProfile::new(true, false)).unwrap();
        PrnsApplicationState::new(catalog.management, catalog.nomad, catalog.lxmf, &REQUESTS)
    }

    fn event(destination: DestinationHash, plaintext: &[u8]) -> PrnsEvent<'_> {
        PrnsEvent::Message(Message::Delivered(Delivery::Single(SingleDelivery {
            destination,
            context: WireContext::None,
            plaintext,
            opened_by: OpenedBy::IdentityKey,
            arrived_at: InstantMillis(17),
            source_interface: InterfaceId::new([0x21; 8]),
        })))
    }

    #[test]
    fn lxmf_delivery_is_owned_and_releases_its_exact_budget() {
        let state = state();
        let payload = b"lxmf packet";
        let owned = copy_lxmf_single_delivery::<Global>(
            &event(state.lxmf().unwrap(), payload),
            &state,
            &BUDGET,
        )
        .unwrap()
        .unwrap();
        assert_eq!(owned.destination(), state.lxmf().unwrap());
        assert_eq!(owned.plaintext(), payload);
        assert_eq!(
            owned.ingress_observation(),
            InboundTransportObservation::new(InboundInterfaceId::new([0x21; 8]), None)
        );
        assert!(matches!(
            owned.carrier(),
            CarrierIngress::Opportunistic {
                implied_destination,
                payload: carrier_payload,
            } if implied_destination == state.lxmf().unwrap().as_bytes()
                && carrier_payload == payload
        ));
        assert_eq!(BUDGET.in_use(), payload.len());
        drop(owned);
        assert_eq!(BUDGET.in_use(), 0);
    }

    #[test]
    fn another_application_delivery_consumes_no_budget() {
        let state = state();
        assert!(
            copy_lxmf_single_delivery::<Global>(
                &event(state.management(), b"management"),
                &state,
                &BUDGET,
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(BUDGET.in_use(), 0);
    }

    #[test]
    fn aggregate_pressure_is_bounded_and_observable() {
        static SMALL: ApplicationPayloadBudget = ApplicationPayloadBudget::new(3);
        let state = state();
        assert_eq!(
            copy_lxmf_single_delivery::<Global>(
                &event(state.lxmf().unwrap(), b"four"),
                &state,
                &SMALL,
            )
            .err(),
            Some(ApplicationEventCopyError::PayloadBudgetFull)
        );
        assert_eq!(SMALL.in_use(), 0);
        assert_eq!(SMALL.pressure_events(), 1);
    }

    #[test]
    fn complete_resource_copy_preserves_prns_hash_metadata_and_link() {
        static RESOURCE_BUDGET: ApplicationPayloadBudget = ApplicationPayloadBudget::new(32);
        let link_id = LinkId::new([0x71; 16]);
        let hash = ResourceHash::new([0x72; 32]);
        let metadata = b"ota-meta";
        let data = b"ota-data";
        let event = PrnsEvent::Message(Message::Resource {
            link_id,
            hash,
            metadata: Some(metadata),
            data,
        });
        let owned = copy_ota_resource::<Global>(&event, &RESOURCE_BUDGET)
            .unwrap()
            .unwrap();
        assert_eq!(owned.link_id(), link_id);
        assert_eq!(owned.hash(), hash);
        assert_eq!(owned.metadata(), Some(metadata.as_slice()));
        assert_eq!(owned.data(), data);
        assert_eq!(RESOURCE_BUDGET.in_use(), metadata.len() + data.len());
        drop(owned);
        assert_eq!(RESOURCE_BUDGET.in_use(), 0);
    }
}
