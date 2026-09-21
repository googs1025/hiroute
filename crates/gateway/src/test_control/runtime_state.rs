use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ports::{
    CasOutcome, ExecutionScope, ProbeLeaseOutcome, RuntimeStateEntry, RuntimeStateKey,
};
use crate::server::composition::{PortError, RuntimeStateStore};

pub(super) const MAX_FAILURE_COUNT: u32 = 32;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RuntimeStateWriteOperation {
    NextWrite,
    CompareAndSwapExact,
    AcquireProbeLeaseExact,
}

#[derive(Clone, Copy, Debug)]
struct ArmedFault {
    operation: RuntimeStateWriteOperation,
    remaining: u32,
}

#[derive(Debug, Default)]
pub(super) struct RuntimeStateFaultControl {
    armed: Mutex<Option<ArmedFault>>,
}

impl RuntimeStateFaultControl {
    pub(super) fn arm(
        &self,
        operation: RuntimeStateWriteOperation,
        failure_count: u32,
    ) -> Result<(), ()> {
        if !(1..=MAX_FAILURE_COUNT).contains(&failure_count) {
            return Err(());
        }
        *self
            .armed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ArmedFault {
            operation,
            remaining: failure_count,
        });
        Ok(())
    }

    fn consume(&self, operation: RuntimeStateWriteOperation) -> bool {
        let mut armed = self
            .armed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(fault) = armed.as_mut() else {
            return false;
        };
        if fault.operation != RuntimeStateWriteOperation::NextWrite && fault.operation != operation
        {
            return false;
        }
        fault.remaining -= 1;
        if fault.remaining == 0 {
            *armed = None;
        }
        true
    }
}

pub(super) struct FaultingRuntimeStateStore {
    inner: Arc<dyn RuntimeStateStore>,
    control: Arc<RuntimeStateFaultControl>,
}

impl FaultingRuntimeStateStore {
    pub(super) fn new(
        inner: Arc<dyn RuntimeStateStore>,
        control: Arc<RuntimeStateFaultControl>,
    ) -> Self {
        Self { inner, control }
    }
}

#[async_trait]
impl RuntimeStateStore for FaultingRuntimeStateStore {
    async fn read_exact(
        &self,
        key: &RuntimeStateKey,
        scope: &ExecutionScope,
    ) -> Result<RuntimeStateEntry, PortError> {
        self.inner.read_exact(key, scope).await
    }

    async fn compare_and_swap_exact(
        &self,
        key: &RuntimeStateKey,
        expected_generation: u64,
        next: RuntimeStateEntry,
        scope: &ExecutionScope,
    ) -> Result<CasOutcome, PortError> {
        if self
            .control
            .consume(RuntimeStateWriteOperation::CompareAndSwapExact)
        {
            return Err(PortError::Unavailable("RuntimeStateStore"));
        }
        self.inner
            .compare_and_swap_exact(key, expected_generation, next, scope)
            .await
    }

    async fn acquire_probe_lease_exact(
        &self,
        key: &RuntimeStateKey,
        expected_generation: u64,
        now: Instant,
        lease_duration: Duration,
        scope: &ExecutionScope,
    ) -> Result<ProbeLeaseOutcome, PortError> {
        if self
            .control
            .consume(RuntimeStateWriteOperation::AcquireProbeLeaseExact)
        {
            return Err(PortError::Unavailable("RuntimeStateStore"));
        }
        self.inner
            .acquire_probe_lease_exact(key, expected_generation, now, lease_duration, scope)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{InMemoryRuntimeStateStore, RuntimeHealth};

    fn scope() -> ExecutionScope {
        ExecutionScope::new(
            Instant::now() + Duration::from_secs(1),
            tokio_util::sync::CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn runtime_state_fault_is_atomic_one_shot_and_never_calls_inner() {
        let inner = Arc::new(InMemoryRuntimeStateStore::default());
        let control = Arc::new(RuntimeStateFaultControl::default());
        let store = FaultingRuntimeStateStore::new(inner.clone(), control.clone());
        let key = RuntimeStateKey::binding("test-binding");
        let next = RuntimeStateEntry {
            generation: 1,
            health: RuntimeHealth::Disabled,
            probe_lease_until: None,
            transient_backoff_step: 0,
        };
        control
            .arm(RuntimeStateWriteOperation::CompareAndSwapExact, 1)
            .unwrap();

        assert!(
            store
                .compare_and_swap_exact(&key, 0, next.clone(), &scope())
                .await
                .is_err()
        );
        assert_eq!(inner.entry(&key), RuntimeStateEntry::default());
        assert_eq!(
            store
                .compare_and_swap_exact(&key, 0, next.clone(), &scope())
                .await
                .unwrap(),
            CasOutcome::Applied { generation: 1 }
        );
        assert_eq!(inner.entry(&key), next);

        let any_key = RuntimeStateKey::binding("next-write-binding");
        control
            .arm(RuntimeStateWriteOperation::NextWrite, 2)
            .unwrap();
        assert_eq!(
            store.read_exact(&any_key, &scope()).await.unwrap(),
            RuntimeStateEntry::default(),
            "reads must neither fail nor consume an armed write fault"
        );
        assert!(
            store
                .compare_and_swap_exact(&any_key, 0, next.clone(), &scope())
                .await
                .is_err()
        );
        assert!(
            store
                .acquire_probe_lease_exact(
                    &any_key,
                    0,
                    Instant::now(),
                    Duration::from_millis(10),
                    &scope(),
                )
                .await
                .is_err()
        );
        assert!(
            store
                .compare_and_swap_exact(&any_key, 0, next, &scope())
                .await
                .is_ok(),
            "exactly two writes must have consumed the armed count"
        );
    }
}
