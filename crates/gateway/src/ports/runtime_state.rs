use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use thiserror::Error;

use super::ExecutionScope;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RuntimeStateKey {
    Binding {
        stable_binding_id: Arc<str>,
    },
    Credential {
        stable_binding_id: Arc<str>,
        credential_ref: Arc<str>,
        key_id: Arc<str>,
        credential_generation: u64,
    },
}

impl RuntimeStateKey {
    pub fn binding(stable_binding_id: impl Into<Arc<str>>) -> Self {
        Self::Binding {
            stable_binding_id: stable_binding_id.into(),
        }
    }

    pub fn credential(
        stable_binding_id: impl Into<Arc<str>>,
        credential_ref: impl Into<Arc<str>>,
        key_id: impl Into<Arc<str>>,
        credential_generation: u64,
    ) -> Self {
        Self::Credential {
            stable_binding_id: stable_binding_id.into(),
            credential_ref: credential_ref.into(),
            key_id: key_id.into(),
            credential_generation,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeHealth {
    Active,
    Disabled,
    CoolingDown { until: Instant },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeStateEntry {
    pub generation: u64,
    pub health: RuntimeHealth,
    pub probe_lease_until: Option<Instant>,
    /// Saturating Binding-only transient failure step. Credential state and
    /// non-cooling Binding state always persist zero.
    pub transient_backoff_step: u8,
}

impl Default for RuntimeStateEntry {
    fn default() -> Self {
        Self {
            generation: 0,
            health: RuntimeHealth::Active,
            probe_lease_until: None,
            transient_backoff_step: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CasOutcome {
    Applied { generation: u64 },
    Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeLeaseOutcome {
    Acquired { generation: u64 },
    Busy,
    Conflict,
}

#[async_trait]
pub trait RuntimeStateStore: Send + Sync {
    async fn read(
        &self,
        key: &RuntimeStateKey,
        scope: &ExecutionScope,
    ) -> Result<RuntimeStateEntry, RuntimeStateError>;

    /// `next.generation` must equal `expected_generation + 1`. An adapter
    /// atomically compares the stored generation and preserves this exact next
    /// value; it returns `Applied` with that generation or makes no write.
    async fn compare_and_swap(
        &self,
        key: &RuntimeStateKey,
        expected_generation: u64,
        next: RuntimeStateEntry,
        scope: &ExecutionScope,
    ) -> Result<CasOutcome, RuntimeStateError>;

    async fn acquire_probe_lease(
        &self,
        key: &RuntimeStateKey,
        expected_generation: u64,
        now: Instant,
        lease_duration: Duration,
        scope: &ExecutionScope,
    ) -> Result<ProbeLeaseOutcome, RuntimeStateError>;
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum RuntimeStateError {
    #[error("runtime state authority is unavailable")]
    Unavailable,
    #[error("runtime state operation was rejected")]
    Rejected,
}

#[derive(Clone, Default)]
pub struct InMemoryRuntimeStateStore {
    entries: Arc<Mutex<BTreeMap<RuntimeStateKey, RuntimeStateEntry>>>,
}

impl InMemoryRuntimeStateStore {
    pub fn entry(&self, key: &RuntimeStateKey) -> RuntimeStateEntry {
        self.entries.lock().get(key).cloned().unwrap_or_default()
    }

    pub fn insert(&self, key: RuntimeStateKey, entry: RuntimeStateEntry) {
        self.entries.lock().insert(key, entry);
    }
}

#[async_trait]
impl RuntimeStateStore for InMemoryRuntimeStateStore {
    async fn read(
        &self,
        key: &RuntimeStateKey,
        _scope: &ExecutionScope,
    ) -> Result<RuntimeStateEntry, RuntimeStateError> {
        Ok(self.entry(key))
    }

    async fn compare_and_swap(
        &self,
        key: &RuntimeStateKey,
        expected_generation: u64,
        next: RuntimeStateEntry,
        _scope: &ExecutionScope,
    ) -> Result<CasOutcome, RuntimeStateError> {
        let mut entries = self.entries.lock();
        let current = entries.get(key).cloned().unwrap_or_default();
        let required_generation = expected_generation
            .checked_add(1)
            .ok_or(RuntimeStateError::Rejected)?;
        if current.generation != expected_generation || next.generation != required_generation {
            return Ok(CasOutcome::Conflict);
        }
        entries.insert(key.clone(), next);
        Ok(CasOutcome::Applied {
            generation: required_generation,
        })
    }

    async fn acquire_probe_lease(
        &self,
        key: &RuntimeStateKey,
        expected_generation: u64,
        now: Instant,
        lease_duration: Duration,
        _scope: &ExecutionScope,
    ) -> Result<ProbeLeaseOutcome, RuntimeStateError> {
        let mut entries = self.entries.lock();
        let current = entries.get(key).cloned().unwrap_or_default();
        if current.generation != expected_generation {
            return Ok(ProbeLeaseOutcome::Conflict);
        }
        if current.probe_lease_until.is_some_and(|until| until > now) {
            return Ok(ProbeLeaseOutcome::Busy);
        }
        let mut next = current;
        next.generation = next
            .generation
            .checked_add(1)
            .ok_or(RuntimeStateError::Rejected)?;
        let generation = next.generation;
        next.probe_lease_until = Some(
            now.checked_add(lease_duration)
                .ok_or(RuntimeStateError::Rejected)?,
        );
        entries.insert(key.clone(), next);
        Ok(ProbeLeaseOutcome::Acquired { generation })
    }
}
