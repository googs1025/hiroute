use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hiroute_domain::{
    ComputeRuntimeHealthV1, ComputeRuntimeStateStoreV1, PortErrorCode, RuntimeClockSampleV1,
    RuntimeProbeAcquireOutcomeV1, RuntimeProbeLeaseRequestV1, RuntimeStateIdentityV1,
    RuntimeStateV1,
};
use hiroute_gateway::ports::{
    CasOutcome, ExecutionScope, ProbeLeaseOutcome, RuntimeHealth, RuntimeStateEntry,
    RuntimeStateKey,
};
use hiroute_gateway::server::composition::{PortError, RuntimeStateStore};

/// Mechanical bridge from Gateway monotonic deadlines to the Product-owned durable state store.
/// Only the clock anchor is process-local; generations, cooldowns and leases remain durable facts.
pub struct GatewayRuntimeStateStore<S> {
    store: Mutex<S>,
    clock: ClockBridge,
}

impl<S> GatewayRuntimeStateStore<S> {
    pub fn new(store: S) -> Result<Self, PortError> {
        Ok(Self {
            store: Mutex::new(store),
            clock: ClockBridge::now()?,
        })
    }
}

#[async_trait]
impl<S> RuntimeStateStore for GatewayRuntimeStateStore<S>
where
    S: ComputeRuntimeStateStoreV1 + Send,
{
    async fn read_exact(
        &self,
        key: &RuntimeStateKey,
        scope: &ExecutionScope,
    ) -> Result<RuntimeStateEntry, PortError> {
        scope.ensure_active().map_err(|_| PortError::Rejected)?;
        let identity = product_identity(key)?;
        let stored = self
            .store
            .lock()
            .map_err(|_| PortError::Unavailable("ComputeRuntimeStateStoreV1"))?
            .runtime_state(&identity)
            .map_err(map_store_error)?;
        scope.ensure_active().map_err(|_| PortError::Rejected)?;
        stored
            .as_ref()
            .map(|state| gateway_state(state, &self.clock))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    async fn compare_and_swap_exact(
        &self,
        key: &RuntimeStateKey,
        expected_generation: u64,
        next: RuntimeStateEntry,
        scope: &ExecutionScope,
    ) -> Result<CasOutcome, PortError> {
        scope.ensure_active().map_err(|_| PortError::Rejected)?;
        if expected_generation.checked_add(1) != Some(next.generation) {
            return Ok(CasOutcome::Conflict);
        }
        let identity = product_identity(key)?;
        let sampled_at = self.clock.sample(Instant::now())?;
        let next = product_state(identity.clone(), &next, sampled_at, &self.clock)?;
        let store = self
            .store
            .lock()
            .map_err(|_| PortError::Unavailable("ComputeRuntimeStateStoreV1"))?;
        let current = store.runtime_state(&identity).map_err(map_store_error)?;
        let result = if let Some(current) = current {
            if current.generation() != expected_generation {
                return Ok(CasOutcome::Conflict);
            }
            if let Some(lease) = current.probe_lease() {
                store.complete_runtime_probe(lease, &next)
            } else {
                store.compare_and_set_runtime_state(expected_generation, &next)
            }
        } else {
            store.compare_and_set_runtime_state(expected_generation, &next)
        };
        match result {
            Ok(()) => {
                scope.ensure_active().map_err(|_| PortError::Rejected)?;
                Ok(CasOutcome::Applied {
                    generation: next.generation(),
                })
            }
            Err(error) if error.code == PortErrorCode::Conflict => Ok(CasOutcome::Conflict),
            Err(error) => Err(map_store_error(error)),
        }
    }

    async fn acquire_probe_lease_exact(
        &self,
        key: &RuntimeStateKey,
        expected_generation: u64,
        now: Instant,
        lease_duration: Duration,
        scope: &ExecutionScope,
    ) -> Result<ProbeLeaseOutcome, PortError> {
        scope.ensure_active().map_err(|_| PortError::Rejected)?;
        let identity = product_identity(key)?;
        let duration_millis = duration_millis_ceil(lease_duration)?;
        let request = RuntimeProbeLeaseRequestV1::new(self.clock.sample(now)?, duration_millis)
            .map_err(|_| PortError::Rejected)?;
        let outcome = self
            .store
            .lock()
            .map_err(|_| PortError::Unavailable("ComputeRuntimeStateStoreV1"))?
            .acquire_runtime_probe(&identity, expected_generation, &request)
            .map_err(map_store_error)?;
        scope.ensure_active().map_err(|_| PortError::Rejected)?;
        Ok(match outcome {
            RuntimeProbeAcquireOutcomeV1::Acquired(state) => ProbeLeaseOutcome::Acquired {
                generation: state.generation(),
            },
            RuntimeProbeAcquireOutcomeV1::Busy => ProbeLeaseOutcome::Busy,
            RuntimeProbeAcquireOutcomeV1::Conflict => ProbeLeaseOutcome::Conflict,
        })
    }
}

fn product_identity(key: &RuntimeStateKey) -> Result<RuntimeStateIdentityV1, PortError> {
    match key {
        RuntimeStateKey::Binding { stable_binding_id } => {
            RuntimeStateIdentityV1::binding(stable_binding_id.to_string())
        }
        RuntimeStateKey::Credential {
            stable_binding_id,
            credential_ref,
            key_id,
            credential_generation,
        } => RuntimeStateIdentityV1::credential(
            stable_binding_id.to_string(),
            credential_ref.to_string(),
            key_id.to_string(),
            *credential_generation,
        ),
    }
    .map_err(|_| PortError::Rejected)
}

fn product_state(
    identity: RuntimeStateIdentityV1,
    state: &RuntimeStateEntry,
    sampled_at: RuntimeClockSampleV1,
    clock: &ClockBridge,
) -> Result<RuntimeStateV1, PortError> {
    let health = match state.health {
        RuntimeHealth::Active => ComputeRuntimeHealthV1::Ready,
        RuntimeHealth::Disabled => ComputeRuntimeHealthV1::Disabled,
        RuntimeHealth::CoolingDown { until } => ComputeRuntimeHealthV1::CoolingDown {
            until_unix_millis: clock.instant_to_unix_millis(until)?,
        },
    };
    let lease = state
        .probe_lease_until
        .map(|until| clock.instant_to_unix_millis(until))
        .transpose()?;
    RuntimeStateV1::from_exact_parts_with_backoff(
        identity,
        state.generation,
        health,
        lease,
        state.transient_backoff_step,
        sampled_at,
    )
    .map_err(|_| PortError::Rejected)
}

fn gateway_state(
    state: &RuntimeStateV1,
    clock: &ClockBridge,
) -> Result<RuntimeStateEntry, PortError> {
    state.validate().map_err(|_| PortError::Rejected)?;
    let health = match state.health() {
        ComputeRuntimeHealthV1::Ready => RuntimeHealth::Active,
        ComputeRuntimeHealthV1::Disabled => RuntimeHealth::Disabled,
        ComputeRuntimeHealthV1::CoolingDown { until_unix_millis } => RuntimeHealth::CoolingDown {
            until: clock.unix_millis_to_instant(until_unix_millis)?,
        },
    };
    let probe_lease_until = state
        .probe_lease()
        .map(|lease| clock.unix_millis_to_instant(lease.expires_at_unix_millis()))
        .transpose()?;
    Ok(RuntimeStateEntry {
        generation: state.generation(),
        health,
        probe_lease_until,
        transient_backoff_step: state.transient_backoff_step(),
    })
}

struct ClockBridge {
    monotonic_anchor: Instant,
    unix_millis_anchor: i64,
}

impl ClockBridge {
    fn now() -> Result<Self, PortError> {
        let unix_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| PortError::Rejected)?
            .as_millis();
        Ok(Self {
            monotonic_anchor: Instant::now(),
            unix_millis_anchor: i64::try_from(unix_millis).map_err(|_| PortError::Rejected)?,
        })
    }

    fn sample(&self, instant: Instant) -> Result<RuntimeClockSampleV1, PortError> {
        RuntimeClockSampleV1::from_unix_millis(self.instant_to_unix_millis(instant)?)
            .map_err(|_| PortError::Rejected)
    }

    fn instant_to_unix_millis(&self, instant: Instant) -> Result<i64, PortError> {
        if instant >= self.monotonic_anchor {
            self.unix_millis_anchor
                .checked_add(duration_i64_millis_ceil(
                    instant.duration_since(self.monotonic_anchor),
                )?)
                .ok_or(PortError::Rejected)
        } else {
            self.unix_millis_anchor
                .checked_sub(duration_i64_millis_ceil(
                    self.monotonic_anchor.duration_since(instant),
                )?)
                .ok_or(PortError::Rejected)
        }
    }

    fn unix_millis_to_instant(&self, unix_millis: i64) -> Result<Instant, PortError> {
        if unix_millis <= self.unix_millis_anchor {
            return Ok(self.monotonic_anchor);
        }
        self.monotonic_anchor
            .checked_add(Duration::from_millis(
                u64::try_from(unix_millis - self.unix_millis_anchor)
                    .map_err(|_| PortError::Rejected)?,
            ))
            .ok_or(PortError::Rejected)
    }
}

fn duration_millis_ceil(duration: Duration) -> Result<u64, PortError> {
    u64::try_from(duration.as_millis())
        .map_err(|_| PortError::Rejected)?
        .checked_add(u64::from(
            !duration.subsec_nanos().is_multiple_of(1_000_000),
        ))
        .filter(|millis| *millis > 0)
        .ok_or(PortError::Rejected)
}

fn duration_i64_millis_ceil(duration: Duration) -> Result<i64, PortError> {
    i64::try_from(duration_millis_ceil(duration)?).map_err(|_| PortError::Rejected)
}

fn map_store_error(error: hiroute_domain::PortError) -> PortError {
    match error.code {
        PortErrorCode::Unavailable => PortError::Unavailable("ComputeRuntimeStateStoreV1"),
        _ => PortError::Rejected,
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    #[test]
    fn expired_wall_deadline_after_uptime_reset_maps_to_current_monotonic_anchor() {
        let monotonic_anchor = Instant::now();
        let clock = ClockBridge {
            monotonic_anchor,
            unix_millis_anchor: 2_000_000_000_000,
        };

        assert_eq!(
            clock.unix_millis_to_instant(1).unwrap(),
            monotonic_anchor,
            "a persisted deadline that is already past must not require subtracting pre-reboot uptime"
        );
    }

    #[test]
    fn future_wall_deadline_preserves_remaining_duration() {
        let monotonic_anchor = Instant::now();
        let clock = ClockBridge {
            monotonic_anchor,
            unix_millis_anchor: 1_000,
        };

        assert_eq!(
            clock.unix_millis_to_instant(2_250).unwrap(),
            monotonic_anchor + Duration::from_millis(1_250)
        );
    }

    #[test]
    fn reboot_expired_cooldown_and_probe_lease_share_the_current_anchor() {
        let monotonic_anchor = Instant::now();
        let clock = ClockBridge {
            monotonic_anchor,
            unix_millis_anchor: 10_000,
        };
        let identity =
            RuntimeStateIdentityV1::credential("binding-a", "credential-a", "key-a", 1).unwrap();
        let persisted = RuntimeStateV1::cooling_down(
            identity,
            7,
            9_000,
            Some(9_500),
            RuntimeClockSampleV1::from_unix_millis(8_000).unwrap(),
        )
        .unwrap();

        let restored = gateway_state(&persisted, &clock).unwrap();
        assert_eq!(
            restored.health,
            RuntimeHealth::CoolingDown {
                until: monotonic_anchor
            }
        );
        assert_eq!(restored.probe_lease_until, Some(monotonic_anchor));
    }
}
