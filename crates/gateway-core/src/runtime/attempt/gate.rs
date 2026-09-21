use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitFence {
    Clear,
    WriteStartedMayHaveCommitted,
    WriteConfirmed,
}

impl CommitFence {
    pub fn begin_write(&mut self) -> Result<(), AttemptError> {
        if *self != Self::Clear {
            return Err(AttemptError::FenceAlreadyAdvanced);
        }
        *self = Self::WriteStartedMayHaveCommitted;
        Ok(())
    }

    pub fn confirm(&mut self) -> Result<(), AttemptError> {
        if *self != Self::WriteStartedMayHaveCommitted {
            return Err(AttemptError::FenceNotStarted);
        }
        *self = Self::WriteConfirmed;
        Ok(())
    }

    pub fn is_clear(self) -> bool {
        self == Self::Clear
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriterState {
    NotStarted,
    HeaderWriteStarted,
    HeaderWritten,
    BodyWriting,
    EosWriting,
    QuiescedNormalEos,
    Cancelling,
    QuiescedCancelReset,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Disposition {
    Accept,
    Continue,
    Terminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptBlockedReason {
    WriterNotNormalEos,
    Deadline,
    Cancelled,
    ResponseNotLive,
}

#[derive(Debug)]
pub struct QuiescenceProof {
    pub(super) close_mode: RequestCloseMode,
}

impl QuiescenceProof {
    pub fn close_mode(&self) -> RequestCloseMode {
        self.close_mode
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestCloseMode {
    NormalEos,
    CancelReset,
}

#[derive(Debug)]
pub struct DispositionPublishPermit {
    pub(super) request_id: RequestId,
    pub(super) attempt_id: AttemptId,
    pub(super) generation: AttemptGeneration,
    pub(super) disposition: Disposition,
    pub(super) nonce: u64,
    pub(super) downstream_header_fence: CommitFence,
    pub(super) downstream_semantic_fence: CommitFence,
    pub(super) accepted_response_scope_created: bool,
}

#[derive(Debug)]
pub enum WriterGate {
    ReadyToPublishAccept {
        quiescence: QuiescenceProof,
        permit: DispositionPublishPermit,
    },
    ReadyToPublishNonAccept {
        quiescence: QuiescenceProof,
        permit: DispositionPublishPermit,
    },
    AcceptBlocked {
        reason: AcceptBlockedReason,
    },
}

#[derive(Debug)]
pub struct PublishedDisposition {
    pub request_id: RequestId,
    pub attempt_id: AttemptId,
    pub generation: AttemptGeneration,
    pub disposition: Disposition,
}

/// Linear technical publication boundary for a selected candidate that
/// failed before a semantic exchange could be constructed. With no request
/// writer or upstream response, only non-Accept dispositions are valid and
/// all commit fences are mechanically clear.
pub(crate) struct PreexchangeDispositionGate {
    request_id: RequestId,
    attempt_id: AttemptId,
    generation: AttemptGeneration,
}

impl PreexchangeDispositionGate {
    pub(crate) fn new(
        request_id: RequestId,
        attempt_id: AttemptId,
        generation: AttemptGeneration,
    ) -> Self {
        Self {
            request_id,
            attempt_id,
            generation,
        }
    }

    pub(crate) fn publish(
        self,
        disposition: Disposition,
    ) -> Result<PublishedDisposition, AttemptError> {
        if disposition == Disposition::Accept {
            return Err(AttemptError::PreexchangeAccept);
        }
        Ok(PublishedDisposition {
            request_id: self.request_id,
            attempt_id: self.attempt_id,
            generation: self.generation,
            disposition,
        })
    }
}
