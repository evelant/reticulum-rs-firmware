use super::*;

#[derive(Clone, Copy)]
pub(crate) struct DispatchMeta {
    pub(crate) deadline_us: u64,
    pub(crate) grace_deadline_us: u64,
    pub(crate) frame_count: u8,
    pub(crate) aggregate_airtime_us: u64,
    pub(crate) permit_resource: TxPermitResourceId,
    pub(crate) airtime_profile: LoRaProfile,
    pub(crate) data_packet: Option<DataPacketDispatchObservation>,
    pub(crate) ordinary_packet: Option<OrdinaryPacketDispatchObservation>,
}

impl DispatchMeta {
    #[allow(
        clippy::too_many_arguments,
        reason = "constructor retains the complete scalar scheduling and one-of-family trace facts"
    )]
    pub(crate) fn try_new(
        deadline_ms: u64,
        grace_us: u64,
        frame_count: u8,
        aggregate_airtime_us: u64,
        permit_resource: TxPermitResourceId,
        airtime_profile: LoRaProfile,
        data_packet: Option<DataPacketDispatchObservation>,
        ordinary_packet: Option<OrdinaryPacketDispatchObservation>,
    ) -> Option<Self> {
        let deadline_us = deadline_ms.checked_mul(1_000)?;
        Some(Self {
            deadline_us,
            grace_deadline_us: deadline_us.saturating_add(grace_us),
            frame_count,
            aggregate_airtime_us,
            permit_resource,
            airtime_profile,
            data_packet,
            ordinary_packet,
        })
    }

    pub(crate) fn data_report(
        self,
        outcome: DispatchOutcome,
        progress: Option<PacketTxProgress>,
    ) -> DispatchReport {
        DispatchReport::data(
            outcome,
            self.frame_count,
            progress,
            self.data_packet
                .expect("DATA dispatch metadata must retain packet evidence"),
        )
    }

    pub(crate) fn ordinary_report(
        self,
        outcome: DispatchOutcome,
        progress: Option<PacketTxProgress>,
    ) -> DispatchReport {
        DispatchReport::ordinary(
            outcome,
            self.frame_count,
            progress,
            self.ordinary_packet
                .expect("ordinary dispatch metadata must retain packet evidence"),
        )
    }
}

pub(crate) enum DataRetainedControl {
    None,
    UnsentRequest(TxPermitRequest),
    Reply(TxPermitReply),
}

impl DataRetainedControl {
    pub(crate) fn kind(&self) -> Option<DispatcherFaultResidueKind> {
        match self {
            Self::None => None,
            Self::UnsentRequest(request) => {
                let _ = request;
                Some(DispatcherFaultResidueKind::DataPermitRequest)
            }
            Self::Reply(reply) => {
                let _ = reply;
                Some(DispatcherFaultResidueKind::DataPermitReply)
            }
        }
    }
}

pub(crate) enum OrdinaryRetainedControl {
    None,
    CancellationMismatch(reticulum_node_core::OrdinaryPermitCancelMismatch<'static>),
    Reply(OrdinaryTxPermitReply),
}

pub(crate) enum ActiveCompletionTicket {
    None,
    Data(DataCompletionTicket),
    Ordinary(OrdinaryCompletionTicket),
}

pub(crate) enum CompletionTicketResidue {
    DataTicket(DataCompletionTicket),
    OrdinaryTicket(OrdinaryCompletionTicket),
    DataMismatch(DataCompletionMismatch),
    OrdinaryMismatch(OrdinaryCompletionMismatch),
    DataWithoutTicket(TxCompletion<'static>),
    OrdinaryWithoutTicket(OrdinaryTxCompletion<'static>),
    DataWithOrdinaryTicket {
        ticket: OrdinaryCompletionTicket,
        completion: TxCompletion<'static>,
    },
    OrdinaryWithDataTicket {
        ticket: DataCompletionTicket,
        completion: OrdinaryTxCompletion<'static>,
    },
    InterfaceCompletion(InterfaceTxCompletion),
}

impl CompletionTicketResidue {
    pub(crate) fn kind(&self) -> DispatcherFaultResidueKind {
        match self {
            Self::DataTicket(ticket) => {
                let _ = ticket;
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::OrdinaryTicket(ticket) => {
                let _ = ticket;
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::DataMismatch(mismatch) => {
                let _ = mismatch;
                DispatcherFaultResidueKind::DataCompletionTicketMismatch
            }
            Self::OrdinaryMismatch(mismatch) => {
                let _ = mismatch;
                DispatcherFaultResidueKind::OrdinaryCompletionTicketMismatch
            }
            Self::DataWithoutTicket(completion) => {
                let _ = completion;
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::OrdinaryWithoutTicket(completion) => {
                let _ = completion;
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::DataWithOrdinaryTicket { ticket, completion } => {
                let _ = (ticket, completion);
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::OrdinaryWithDataTicket { ticket, completion } => {
                let _ = (ticket, completion);
                DispatcherFaultResidueKind::CompletionTicketInvariant
            }
            Self::InterfaceCompletion(completion) => {
                let _ = completion;
                DispatcherFaultResidueKind::InterfaceCompletion
            }
        }
    }
}

impl OrdinaryRetainedControl {
    pub(crate) fn kind(&self) -> Option<DispatcherFaultResidueKind> {
        match self {
            Self::None => None,
            Self::CancellationMismatch(mismatch) => {
                let _ = mismatch;
                Some(DispatcherFaultResidueKind::OrdinaryPermitMismatch)
            }
            Self::Reply(reply) => {
                let _ = reply;
                Some(DispatcherFaultResidueKind::OrdinaryPermitReply)
            }
        }
    }
}

pub(crate) enum DataAfterReturn {
    Resume,
    Disable {
        fault: DispatcherFault,
        retained: DataRetainedControl,
    },
}

#[allow(
    clippy::large_enum_variant,
    reason = "disablement must retain the exact no-alloc ordinary packet owner for recovery"
)]
pub(crate) enum OrdinaryAfterReturn {
    Resume,
    Disable {
        fault: DispatcherFault,
        retained: OrdinaryRetainedControl,
    },
}

pub(crate) enum DataState {
    Idle,
    Job(RoutedTxJob<'static>),
    Access {
        job: RoutedTxJob<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    CadInFlight {
        job: RoutedTxJob<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitSend {
        pending: PermitPendingTx<'static>,
        request: TxPermitRequest,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitWait {
        pending: PermitPendingTx<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitReply {
        pending: PermitPendingTx<'static>,
        reply: TxPermitReply,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    TransmitReady {
        owner: AuthorizedTx<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    TxInFlight {
        owner: AuthorizedTx<'static>,
        meta: DispatchMeta,
        authorized_frame: AuthorizedFrameObservation,
    },
    Expired {
        owner: ExpiredAuthorizedTx<'static>,
        meta: DispatchMeta,
    },
    Unpermitted {
        owner: UnpermittedTx<'static>,
        meta: DispatchMeta,
    },
    AuthorizedFrameRequest {
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
        expected: AuthorizedFrameObservation,
        request: AuthorizedFrameObservation,
    },
    AuthorizedFrameAcknowledgementWait {
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
        expected: AuthorizedFrameObservation,
    },
    AuthorizedFrameAcknowledgementFault {
        fault: DispatcherFault,
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
        request: Option<AuthorizedFrameObservation>,
        residue: AuthorizedFrameAcknowledgementMismatch,
    },
    Return {
        completion: TxCompletion<'static>,
        after: DataAfterReturn,
    },
    InterfaceReturn {
        completion: InterfaceTxCompletion,
        after: DataAfterReturn,
    },
    Disabled {
        fault: DispatcherFault,
        retained: DataRetainedControl,
    },
    Transitioning,
}

pub(crate) enum OrdinaryState {
    Idle,
    Job(OrdinaryTxJob<'static>),
    Access {
        job: OrdinaryTxJob<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    CadInFlight {
        job: OrdinaryTxJob<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitSend {
        pending: OrdinaryPermitPendingTx<'static>,
        request: OrdinaryTxPermitRequest,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitWait {
        pending: OrdinaryPermitPendingTx<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    PermitReply {
        pending: OrdinaryPermitPendingTx<'static>,
        reply: OrdinaryTxPermitReply,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    TransmitReady {
        owner: OrdinaryAuthorizedTx<'static>,
        access: LogicalPacketChannelAccess,
        meta: DispatchMeta,
    },
    TxInFlight {
        owner: OrdinaryAuthorizedTx<'static>,
        meta: DispatchMeta,
    },
    Expired {
        owner: OrdinaryExpiredAuthorizedTx<'static>,
        meta: DispatchMeta,
    },
    Unpermitted {
        owner: OrdinaryUnpermittedTx<'static>,
        meta: DispatchMeta,
    },
    Return {
        completion: OrdinaryTxCompletion<'static>,
        after: OrdinaryAfterReturn,
    },
    InterfaceReturn {
        completion: InterfaceTxCompletion,
        after: OrdinaryAfterReturn,
    },
    Disabled {
        fault: DispatcherFault,
        retained: OrdinaryRetainedControl,
    },
    Transitioning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActiveFamily {
    None,
    Data,
    Ordinary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReceiveState {
    Idle,
    InFlight,
    Disabled(DispatcherFault),
}

pub(crate) struct ReceiveCancellationGuard<'a, R>
where
    R: SoleRnodeRadio,
{
    pub(crate) radio: &'a mut R,
    pub(crate) preserve_active: bool,
}

impl<'a, R> ReceiveCancellationGuard<'a, R>
where
    R: SoleRnodeRadio,
{
    pub(crate) fn new(radio: &'a mut R) -> Self {
        Self {
            radio,
            preserve_active: false,
        }
    }

    pub(crate) fn preserve_active(&mut self) {
        self.preserve_active = true;
    }
}

impl<R> Drop for ReceiveCancellationGuard<'_, R>
where
    R: SoleRnodeRadio,
{
    fn drop(&mut self) {
        if !self.preserve_active {
            self.radio.shutdown();
        }
    }
}
