use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DataOwnerBinding {
    prepared: PreparedPacket,
    interface: PacketInterfaceId,
}

impl DataOwnerBinding {
    pub(crate) fn from_job(job: &RoutedTxJob<'_>) -> Self {
        Self {
            prepared: job.prepared(),
            interface: job.interface(),
        }
    }

    pub(crate) fn from_completion(completion: &TxCompletion<'_>) -> Self {
        Self {
            prepared: completion.prepared(),
            interface: completion.interface(),
        }
    }
}

/// One DATA job delivered only to its selected interface actor.
#[must_use = "an interface DATA owner must be processed or explicitly retained"]
pub struct InterfaceDataJob {
    pub(crate) context: InterfaceDispatchContext,
    pub(crate) binding: DataOwnerBinding,
    pub(crate) job: RoutedTxJob<'static>,
}

impl InterfaceDataJob {
    /// Registry/configuration snapshot under which this owner was accepted.
    pub const fn context(&self) -> InterfaceDispatchContext {
        self.context
    }

    /// Consume the wrapper into the exact node-core job and its one-shot
    /// completion ticket.
    pub fn into_parts(self) -> (DataCompletionTicket, RoutedTxJob<'static>) {
        (
            DataCompletionTicket {
                context: self.context,
                binding: self.binding,
            },
            self.job,
        )
    }
}

/// One-shot capability that binds a DATA completion to the exact dispatched
/// owner and interface lease.
#[must_use = "a DATA completion ticket must return with its exact owner"]
pub struct DataCompletionTicket {
    context: InterfaceDispatchContext,
    binding: DataOwnerBinding,
}

impl DataCompletionTicket {
    /// Registry/configuration snapshot bound to this ticket.
    pub const fn context(&self) -> InterfaceDispatchContext {
        self.context
    }

    /// Bind the exact owning node-core completion for return to the router.
    // The unchanged non-Copy owner must remain inline on mismatch; heap boxing
    // is not available on this portable ownership boundary.
    #[allow(clippy::result_large_err)]
    pub fn complete(
        self,
        completion: TxCompletion<'static>,
    ) -> Result<InterfaceTxCompletion, DataCompletionMismatch> {
        if DataOwnerBinding::from_completion(&completion) != self.binding {
            return Err(DataCompletionMismatch {
                ticket: self,
                completion,
            });
        }
        Ok(InterfaceTxCompletion {
            context: self.context,
            owner: InterfaceCompletionOwner::Data(completion),
        })
    }
}

/// Crossed DATA completion retaining both exact values unchanged.
#[must_use = "a crossed DATA completion and its ticket remain uniquely owned"]
pub struct DataCompletionMismatch {
    ticket: DataCompletionTicket,
    completion: TxCompletion<'static>,
}

impl fmt::Debug for DataCompletionMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DataCompletionMismatch")
            .field("expected", &self.ticket.binding)
            .field(
                "supplied",
                &DataOwnerBinding::from_completion(&self.completion),
            )
            .finish()
    }
}

impl DataCompletionMismatch {
    /// Recover the unchanged ticket and owning completion for fail-closed
    /// handling.
    pub fn into_parts(self) -> (DataCompletionTicket, TxCompletion<'static>) {
        (self.ticket, self.completion)
    }
}

/// One ordinary protocol-action job delivered only to its selected interface
/// actor.
#[must_use = "an ordinary interface owner must be processed or explicitly retained"]
pub struct InterfaceOrdinaryJob {
    pub(crate) context: InterfaceDispatchContext,
    pub(crate) binding: OrdinaryPreparedPacket,
    pub(crate) job: OrdinaryTxJob<'static>,
}

impl InterfaceOrdinaryJob {
    /// Registry/configuration snapshot under which this owner was accepted.
    pub const fn context(&self) -> InterfaceDispatchContext {
        self.context
    }

    /// Consume the wrapper into the exact ordinary job and its one-shot
    /// completion ticket.
    pub fn into_parts(self) -> (OrdinaryCompletionTicket, OrdinaryTxJob<'static>) {
        (
            OrdinaryCompletionTicket {
                context: self.context,
                binding: self.binding,
            },
            self.job,
        )
    }
}

/// One-shot capability binding an ordinary completion to its exact dispatched
/// owner and interface lease.
#[must_use = "an ordinary completion ticket must return with its exact owner"]
pub struct OrdinaryCompletionTicket {
    context: InterfaceDispatchContext,
    binding: OrdinaryPreparedPacket,
}

impl OrdinaryCompletionTicket {
    /// Registry/configuration snapshot bound to this ticket.
    pub const fn context(&self) -> InterfaceDispatchContext {
        self.context
    }

    /// Bind the exact ordinary owning completion for return to the router.
    // The unchanged non-Copy owner must remain inline on mismatch; heap boxing
    // is not available on this portable ownership boundary.
    #[allow(clippy::result_large_err)]
    pub fn complete(
        self,
        completion: OrdinaryTxCompletion<'static>,
    ) -> Result<InterfaceTxCompletion, OrdinaryCompletionMismatch> {
        if completion.prepared() != self.binding {
            return Err(OrdinaryCompletionMismatch {
                ticket: self,
                completion,
            });
        }
        Ok(InterfaceTxCompletion {
            context: self.context,
            owner: InterfaceCompletionOwner::Ordinary(completion),
        })
    }
}

/// Crossed ordinary completion retaining both exact values unchanged.
#[must_use = "a crossed ordinary completion and its ticket remain uniquely owned"]
pub struct OrdinaryCompletionMismatch {
    ticket: OrdinaryCompletionTicket,
    completion: OrdinaryTxCompletion<'static>,
}

impl fmt::Debug for OrdinaryCompletionMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrdinaryCompletionMismatch")
            .field("expected", &self.ticket.binding)
            .field("supplied", &self.completion.prepared())
            .finish()
    }
}

impl OrdinaryCompletionMismatch {
    /// Recover the unchanged ticket and owning completion for fail-closed
    /// handling.
    pub fn into_parts(self) -> (OrdinaryCompletionTicket, OrdinaryTxCompletion<'static>) {
        (self.ticket, self.completion)
    }
}

/// Exact outbound owner accepted by one per-interface queue.
#[must_use = "an interface TX owner must remain uniquely owned"]
pub enum InterfaceTxJob {
    /// Destination-DATA owner with a receipt/attempt ledger entry.
    Data(InterfaceDataJob),
    /// Ordinary announce, proof, forwarding, Link, or Resource packet owner.
    Ordinary(InterfaceOrdinaryJob),
}

pub(crate) enum InterfaceCompletionOwner {
    Data(TxCompletion<'static>),
    Ordinary(OrdinaryTxCompletion<'static>),
}

/// Owning completion returned by one exact interface actor.
///
/// Its owner and context are private so only a matching one-shot DATA or
/// ordinary completion ticket can construct this envelope.
#[must_use = "an interface completion must return to the outbound router"]
pub struct InterfaceTxCompletion {
    context: InterfaceDispatchContext,
    owner: InterfaceCompletionOwner,
}

impl InterfaceTxCompletion {
    pub(crate) fn context(&self) -> InterfaceDispatchContext {
        self.context
    }

    /// Recover the exact node-owner completion while discarding only the
    /// already-inspected interface routing envelope.
    pub fn into_outbound(self) -> OutboundCompletion {
        match self.owner {
            InterfaceCompletionOwner::Data(completion) => OutboundCompletion::Data(completion),
            InterfaceCompletionOwner::Ordinary(completion) => {
                OutboundCompletion::Ordinary(completion)
            }
        }
    }
}

/// Exact completion ready for its node-core DATA or ordinary owner.
#[must_use = "an outbound completion must be reconciled by its node owner"]
pub enum OutboundCompletion {
    /// Destination-DATA owning completion.
    Data(TxCompletion<'static>),
    /// Ordinary protocol-action owning completion.
    Ordinary(OrdinaryTxCompletion<'static>),
}

impl fmt::Debug for OutboundCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Data(completion) => formatter
                .debug_tuple("Data")
                .field(&completion.prepared())
                .field(&completion.interface())
                .finish(),
            Self::Ordinary(completion) => formatter
                .debug_tuple("Ordinary")
                .field(&completion.prepared())
                .finish(),
        }
    }
}
