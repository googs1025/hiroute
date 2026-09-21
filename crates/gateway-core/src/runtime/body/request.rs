use super::memory::lock;
use super::*;

#[derive(Debug)]
pub struct RawRequestBodyStore {
    plan: BodyPlan,
    chunks: Vec<ChargedBytes>,
    visible_bytes: usize,
}

impl RawRequestBodyStore {
    pub fn new(plan: BodyPlan) -> Result<Self, BodyError> {
        plan.validate()?;
        if matches!(plan, BodyPlan::PassThrough { .. }) {
            return Err(BodyError::PassThroughCannotStore);
        }
        Ok(Self {
            plan,
            chunks: Vec::new(),
            visible_bytes: 0,
        })
    }

    pub fn push(&mut self, chunk: ChargedBytes) -> Result<(), BodyError> {
        if chunk.role() != MemoryRole::RawRequest {
            return Err(BodyError::WrongMemoryRole);
        }
        let next = self
            .visible_bytes
            .checked_add(chunk.bytes().len())
            .ok_or(BodyError::BodyLimitExceeded)?;
        let limit = match self.plan {
            BodyPlan::StreamingReplay {
                max_replay_bytes, ..
            } => max_replay_bytes,
            BodyPlan::BufferedTransform { max_body_bytes } => max_body_bytes,
            BodyPlan::SseFramedStreaming {
                max_pending_bytes, ..
            } => max_pending_bytes,
            BodyPlan::PassThrough { .. } => return Err(BodyError::PassThroughCannotStore),
        };
        if next > limit {
            return Err(BodyError::BodyLimitExceeded);
        }
        self.visible_bytes = next;
        self.chunks.push(chunk);
        Ok(())
    }

    /// Consuming ownership transfer; no raw clone remains after this call.
    pub fn into_model_backing(self) -> Result<ModelBodyBacking, BodyError> {
        let chunks = self
            .chunks
            .into_iter()
            .map(|chunk| chunk.transfer_role(MemoryRole::ModelIrBacking))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ModelBodyBacking {
            chunks,
            visible_bytes: self.visible_bytes,
        })
    }
}

#[derive(Debug)]
pub struct ModelBodyBacking {
    chunks: Vec<ChargedBytes>,
    visible_bytes: usize,
}

impl ModelBodyBacking {
    pub fn visible_bytes(&self) -> usize {
        self.visible_bytes
    }

    pub fn chunks(&self) -> impl Iterator<Item = &Bytes> {
        self.chunks.iter().map(ChargedBytes::bytes)
    }
}

#[derive(Debug)]
struct LeaseState {
    outstanding: usize,
    terminal: bool,
    proof_issued: bool,
}

#[derive(Clone, Debug)]
pub struct RequestLeaseBook {
    state: Arc<Mutex<LeaseState>>,
}

impl Default for RequestLeaseBook {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestLeaseBook {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(LeaseState {
                outstanding: 0,
                terminal: false,
                proof_issued: false,
            })),
        }
    }

    pub fn acquire(&self) -> Result<RequestBodyLease, BodyError> {
        let mut state = lock(&self.state);
        if state.terminal || state.proof_issued {
            return Err(BodyError::LeaseAfterTerminal);
        }
        state.outstanding = state
            .outstanding
            .checked_add(1)
            .ok_or(BodyError::LeaseOverflow)?;
        Ok(RequestBodyLease {
            state: Arc::clone(&self.state),
            released: false,
        })
    }

    pub fn lease_zero_for_continue(&self) -> Result<LeaseZeroProof, BodyError> {
        let state = lock(&self.state);
        if state.outstanding != 0 {
            return Err(BodyError::OutstandingRequestLease(state.outstanding));
        }
        if state.terminal {
            return Err(BodyError::AlreadyTerminal);
        }
        Ok(LeaseZeroProof { _private: () })
    }

    pub fn mark_terminal_and_release_ready(&self) -> Result<IrReleaseReady, BodyError> {
        let mut state = lock(&self.state);
        if state.outstanding != 0 {
            return Err(BodyError::OutstandingRequestLease(state.outstanding));
        }
        if state.proof_issued {
            return Err(BodyError::ReleaseProofAlreadyIssued);
        }
        state.terminal = true;
        state.proof_issued = true;
        Ok(IrReleaseReady { _private: () })
    }

    pub fn outstanding(&self) -> usize {
        lock(&self.state).outstanding
    }
}

#[derive(Debug)]
pub struct RequestBodyLease {
    state: Arc<Mutex<LeaseState>>,
    released: bool,
}

impl RequestBodyLease {
    pub fn release(mut self) {
        self.release_once();
    }

    fn release_once(&mut self) {
        if self.released {
            return;
        }
        let mut state = lock(&self.state);
        state.outstanding -= 1;
        self.released = true;
    }
}

impl Drop for RequestBodyLease {
    fn drop(&mut self) {
        self.release_once();
    }
}

#[derive(Debug)]
pub struct LeaseZeroProof {
    _private: (),
}

#[derive(Debug)]
pub struct IrReleaseReady {
    _private: (),
}
