use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StreamId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ScopeId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScopeKind {
    LogicalRequest,
    RouteAttempt,
    AcceptedResponse,
}

#[derive(Debug, Default)]
struct ScopeState {
    next_id: AtomicU64,
    logical_started: AtomicBool,
    logical_finalized: AtomicUsize,
    active_attempt: AtomicBool,
    attempt_started: AtomicUsize,
    attempt_finalized: AtomicUsize,
    accepted_started: AtomicBool,
    accepted_finalized: AtomicUsize,
}

/// Enforces `LogicalRequest : RouteAttempt : AcceptedResponse = 1 : N : 0..1`.
#[derive(Clone, Debug, Default)]
pub struct ScopeSupervisor {
    state: Arc<ScopeState>,
}

impl ScopeSupervisor {
    pub fn begin_logical(&self) -> Result<ScopeGuard, ScopeError> {
        if self.state.logical_started.swap(true, Ordering::AcqRel) {
            return Err(ScopeError::LogicalAlreadyStarted);
        }
        Ok(self.new_guard(ScopeKind::LogicalRequest))
    }

    pub fn begin_attempt(&self) -> Result<ScopeGuard, ScopeError> {
        if !self.state.logical_started.load(Ordering::Acquire) {
            return Err(ScopeError::LogicalNotStarted);
        }
        if self.state.accepted_started.load(Ordering::Acquire) {
            return Err(ScopeError::AcceptedAlreadyStarted);
        }
        if self.state.active_attempt.swap(true, Ordering::AcqRel) {
            return Err(ScopeError::AttemptStillActive);
        }
        self.state.attempt_started.fetch_add(1, Ordering::Relaxed);
        Ok(self.new_guard(ScopeKind::RouteAttempt))
    }

    pub fn begin_accepted(&self) -> Result<ScopeGuard, ScopeError> {
        if self.state.active_attempt.load(Ordering::Acquire) {
            return Err(ScopeError::AttemptStillActive);
        }
        if self.state.accepted_started.swap(true, Ordering::AcqRel) {
            return Err(ScopeError::AcceptedAlreadyStarted);
        }
        Ok(self.new_guard(ScopeKind::AcceptedResponse))
    }

    fn new_guard(&self, kind: ScopeKind) -> ScopeGuard {
        ScopeGuard {
            id: ScopeId(self.state.next_id.fetch_add(1, Ordering::Relaxed) + 1),
            kind,
            state: Arc::clone(&self.state),
            finalized: false,
        }
    }

    pub fn counts(&self) -> ScopeCounts {
        ScopeCounts {
            logical_finalized: self.state.logical_finalized.load(Ordering::Acquire),
            attempts_started: self.state.attempt_started.load(Ordering::Acquire),
            attempts_finalized: self.state.attempt_finalized.load(Ordering::Acquire),
            accepted_started: usize::from(self.state.accepted_started.load(Ordering::Acquire)),
            accepted_finalized: self.state.accepted_finalized.load(Ordering::Acquire),
        }
    }
}

#[derive(Debug)]
pub struct ScopeGuard {
    id: ScopeId,
    kind: ScopeKind,
    state: Arc<ScopeState>,
    finalized: bool,
}

impl ScopeGuard {
    pub fn id(&self) -> ScopeId {
        self.id
    }

    pub fn kind(&self) -> ScopeKind {
        self.kind
    }

    pub fn finalize(mut self) {
        self.finalize_once();
    }

    fn finalize_once(&mut self) {
        if self.finalized {
            return;
        }
        match self.kind {
            ScopeKind::LogicalRequest => {
                self.state.logical_finalized.fetch_add(1, Ordering::AcqRel);
            }
            ScopeKind::RouteAttempt => {
                self.state.attempt_finalized.fetch_add(1, Ordering::AcqRel);
                self.state.active_attempt.store(false, Ordering::Release);
            }
            ScopeKind::AcceptedResponse => {
                self.state.accepted_finalized.fetch_add(1, Ordering::AcqRel);
            }
        }
        self.finalized = true;
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        self.finalize_once();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopeCounts {
    pub logical_finalized: usize,
    pub attempts_started: usize,
    pub attempts_finalized: usize,
    pub accepted_started: usize,
    pub accepted_finalized: usize,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ScopeError {
    #[error("logical request scope already started")]
    LogicalAlreadyStarted,
    #[error("logical request scope has not started")]
    LogicalNotStarted,
    #[error("the previous route-attempt scope is still active")]
    AttemptStillActive,
    #[error("accepted-response scope already started")]
    AcceptedAlreadyStarted,
}
