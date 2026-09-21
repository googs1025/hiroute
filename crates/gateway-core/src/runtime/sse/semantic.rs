use super::*;

#[derive(Debug)]
pub struct BudgetedResponseMailbox<T> {
    capacity: usize,
    queue: VecDeque<T>,
    _metadata_reservation: Option<Reservation>,
}

impl<T> BudgetedResponseMailbox<T> {
    pub fn new(capacity: usize) -> Result<Self, SseError> {
        if capacity == 0 {
            return Err(SseError::InvalidMailboxCapacity);
        }
        Ok(Self {
            capacity,
            queue: VecDeque::with_capacity(capacity),
            _metadata_reservation: None,
        })
    }

    pub fn new_budgeted(capacity: usize, budget: &StreamBudget) -> Result<Self, SseError> {
        if capacity == 0 {
            return Err(SseError::InvalidMailboxCapacity);
        }
        let metadata_bytes = capacity
            .checked_mul(size_of::<T>())
            .ok_or(SseError::BudgetExceeded)?;
        let metadata_reservation = budget
            .reserve(MemoryRole::ResponsePrefix, metadata_bytes)
            .map_err(|_| SseError::BudgetExceeded)?;
        Ok(Self {
            capacity,
            queue: VecDeque::with_capacity(capacity),
            _metadata_reservation: Some(metadata_reservation),
        })
    }

    /// Never waits while an H1 session is owned. The caller retains the item
    /// and stops polling upstream until capacity becomes available.
    pub fn try_push(&mut self, item: T) -> Result<(), T> {
        if self.queue.len() == self.capacity {
            Err(item)
        } else {
            self.queue.push_back(item);
            Ok(())
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        self.queue.pop_front()
    }

    pub fn is_full(&self) -> bool {
        self.queue.len() == self.capacity
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticProvenance {
    NonSemantic,
    ProducesSemantic,
}

impl SemanticProvenance {
    pub fn merge(self, other: Self) -> Self {
        if self == Self::ProducesSemantic || other == Self::ProducesSemantic {
            Self::ProducesSemantic
        } else {
            Self::NonSemantic
        }
    }
}

#[derive(Debug)]
pub struct EncodedOutputUnit {
    pub bytes: ChargedBytes,
    pub provenance: SemanticProvenance,
}

impl EncodedOutputUnit {
    pub fn split(self, budget: &StreamBudget, at: usize) -> Result<(Self, Self), SseError> {
        if at > self.bytes.bytes().len() {
            return Err(SseError::InvalidSplit);
        }
        let left = ChargedBytes::copy_from_opaque(
            budget,
            MemoryRole::OutputQueue,
            &self.bytes.bytes()[..at],
        )
        .map_err(|_| SseError::BudgetExceeded)?;
        let right = ChargedBytes::copy_from_opaque(
            budget,
            MemoryRole::OutputQueue,
            &self.bytes.bytes()[at..],
        )
        .map_err(|_| SseError::BudgetExceeded)?;
        Ok((
            Self {
                bytes: left,
                provenance: self.provenance,
            },
            Self {
                bytes: right,
                provenance: self.provenance,
            },
        ))
    }

    pub fn merge(left: Self, right: Self, budget: &StreamBudget) -> Result<Self, SseError> {
        let provenance = left.provenance.merge(right.provenance);
        let capacity = left
            .bytes
            .bytes()
            .len()
            .checked_add(right.bytes.bytes().len())
            .ok_or(SseError::BudgetExceeded)?;
        let mut bytes = ChargedBytesBuilder::new(budget, MemoryRole::OutputQueue, capacity)
            .map_err(|_| SseError::BudgetExceeded)?;
        bytes
            .extend_from_slice(left.bytes.bytes())
            .map_err(|_| SseError::BudgetExceeded)?;
        bytes
            .extend_from_slice(right.bytes.bytes())
            .map_err(|_| SseError::BudgetExceeded)?;
        Ok(Self {
            bytes: bytes.finish(),
            provenance,
        })
    }

    pub fn replace_inheriting(self, replacement: ChargedBytes) -> Self {
        Self {
            bytes: replacement,
            provenance: self.provenance,
        }
    }

    pub fn replace_authorized(
        self,
        replacement: ChargedBytes,
        provenance: SemanticProvenance,
        capability: &SemanticReplacementCapability,
    ) -> Result<Self, SseError> {
        if !capability.authorized {
            return Err(SseError::SemanticReplacementNotAuthorized);
        }
        Ok(Self {
            bytes: replacement,
            provenance,
        })
    }
}

#[derive(Debug)]
pub struct SemanticReplacementCapability {
    authorized: bool,
}

impl SemanticReplacementCapability {
    pub fn from_compiled_plan(plan: &CompiledAcceptedResponsePlan) -> Self {
        Self {
            authorized: plan.semantic_replacement_authorized,
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SseError {
    #[error("SSE limits must be non-zero")]
    InvalidLimits,
    #[error("SSE framer is in failed state")]
    FramerFailed,
    #[error("SSE event exceeds max_event_bytes")]
    EventLimit,
    #[error("SSE pending data exceeds its hard limit")]
    PendingLimit,
    #[error("SSE output exceeds expansion or output limit")]
    OutputLimit,
    #[error("body budget rejected SSE allocation")]
    BudgetExceeded,
    #[error("incomplete SSE event at EOF")]
    IncompleteEventAtEof,
    #[error("visitor did not choose pass/drop/emit")]
    NoEmitterDecision,
    #[error("event emitter already made a terminal decision")]
    EmitterAlreadyDecided,
    #[error("precommit handoff capacity is exhausted")]
    HandoffCapacityExceeded,
    #[error("bounded SSE consumer must drain before framing can continue")]
    NeedDrain,
    #[error("SSE framer has charged deferred input; call resume before feed")]
    DrainRequired,
    #[error("unknown precommit sequence {0}")]
    UnknownSequence(u64),
    #[error("precommit sequence {0} was already decoded")]
    SequenceAlreadyDecoded(u64),
    #[error("precommit sequence {0} is not owned by a provider classifier")]
    SequenceNotInFlight(u64),
    #[error("precommit sequence {0} is still owned by a provider classifier")]
    SequenceStillInFlight(u64),
    #[error("accepted handoff lost the raw SSE event required for replay")]
    DecodedHandoffWithoutRaw,
    #[error("response mailbox capacity must be non-zero")]
    InvalidMailboxCapacity,
    #[error("output split point is invalid")]
    InvalidSplit,
    #[error("semantic replacement is not authorized by compiled plan")]
    SemanticReplacementNotAuthorized,
    #[error("SSE transform output lost or reordered its source sequence")]
    TransformSequenceMismatch,
}
