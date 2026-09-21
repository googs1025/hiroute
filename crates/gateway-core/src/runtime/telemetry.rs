use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::core::execution_plan::{
    AuthorityId, ConfigGeneration, ConfigRevision, PlanRevision, StableTargetKey,
};
use crate::core::publication::InstallerPhase;
use crate::runtime::attempt::{
    AttemptGeneration, AttemptId, AttemptSnapshot, CommitFence, Disposition, RequestCloseMode,
    RequestId,
};
use crate::runtime::body::{
    BodyDirection, BodyPlan, BudgetMemorySnapshot, MemoryRole, StreamBudgetSnapshot,
};
use crate::runtime::executor::{ExecutorKind, ExecutorSnapshot, ResumeTiming};
use crate::runtime::scope::{ScopeId, ScopeKind};

mod event;
mod observers;
mod projector;
mod runtime;

pub use event::*;
pub use observers::*;
pub use projector::*;
pub use runtime::*;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ObservationError {
    #[error("observation sink is full or unavailable")]
    Unavailable,
    #[error("observation channel capacity must be non-zero")]
    InvalidCapacity,
}

fn nanos_since(epoch: Instant, value: Instant) -> u64 {
    value.checked_duration_since(epoch).map_or(0, |duration| {
        duration.as_nanos().min(u128::from(u64::MAX)) as u64
    })
}

fn saturating_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
