use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::ports::{
    CasOutcome, CredentialLeaseRequest, ExecutionScope, HeaderSecretLeaseRequest,
    ProbeLeaseOutcome, RuntimeStateEntry, RuntimeStateKey,
};
use crate::server::composition::{
    CredentialLease, CredentialResolver, PortError, RuntimeStateStore,
};

use super::active_request;

/// Observation decorator for the exact credential port. It invokes the
/// authority first, returns that exact result unchanged and only then offers a
/// zero-secret fact to the request's independent producer.
pub struct ObservedCredentialResolver {
    inner: Arc<dyn CredentialResolver>,
}

impl ObservedCredentialResolver {
    pub fn new(inner: Arc<dyn CredentialResolver>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl CredentialResolver for ObservedCredentialResolver {
    fn acquire(&self, credential_ref: &str) -> Result<CredentialLease, PortError> {
        self.inner.acquire(credential_ref)
    }

    async fn lease_exact(
        &self,
        request: CredentialLeaseRequest<'_>,
        scope: &ExecutionScope,
    ) -> Result<Option<crate::ports::CredentialLease>, PortError> {
        let stable_binding_id = request.stable_binding_id.to_owned();
        let credential_ref = request.credential_ref.to_owned();
        let excluded_key_count = request.excluded_key_ids.len();
        let result = self.inner.lease_exact(request, scope).await;
        if let Some(observation) = active_request() {
            match &result {
                Ok(Some(lease)) => observation.credential_lease(
                    &stable_binding_id,
                    &credential_ref,
                    excluded_key_count,
                    Some((lease.key_id(), lease.generation())),
                    "leased",
                ),
                Ok(None) => observation.credential_lease(
                    &stable_binding_id,
                    &credential_ref,
                    excluded_key_count,
                    None,
                    "exhausted",
                ),
                Err(_) => observation.credential_lease(
                    &stable_binding_id,
                    &credential_ref,
                    excluded_key_count,
                    None,
                    "authority_error",
                ),
            }
        }
        result
    }

    async fn lease_header_secret(
        &self,
        request: HeaderSecretLeaseRequest<'_>,
        scope: &ExecutionScope,
    ) -> Result<Option<crate::ports::CredentialLease>, PortError> {
        // Classifier leases are intentionally not reported as business
        // candidate credential attempts.
        self.inner.lease_header_secret(request, scope).await
    }
}

/// Observation decorator for exact runtime-state operations. Runtime state is
/// always allowed to complete before any best-effort fact is offered, so a
/// sink fault cannot change CAS, cooldown, probe or cancellation semantics.
pub struct ObservedRuntimeStateStore {
    inner: Arc<dyn RuntimeStateStore>,
}

impl ObservedRuntimeStateStore {
    pub fn new(inner: Arc<dyn RuntimeStateStore>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl RuntimeStateStore for ObservedRuntimeStateStore {
    async fn read_exact(
        &self,
        key: &RuntimeStateKey,
        scope: &ExecutionScope,
    ) -> Result<RuntimeStateEntry, PortError> {
        let result = self.inner.read_exact(key, scope).await;
        if let Some(observation) = active_request() {
            match &result {
                Ok(entry) => observation.runtime_state_read(key, Some(entry), "ok"),
                Err(_) => observation.runtime_state_read(key, None, "authority_error"),
            }
        }
        result
    }

    async fn compare_and_swap_exact(
        &self,
        key: &RuntimeStateKey,
        expected_generation: u64,
        next: RuntimeStateEntry,
        scope: &ExecutionScope,
    ) -> Result<CasOutcome, PortError> {
        let observed_next = next.clone();
        let result = self
            .inner
            .compare_and_swap_exact(key, expected_generation, next, scope)
            .await;
        if let Some(observation) = active_request() {
            let outcome = match &result {
                Ok(CasOutcome::Applied { .. }) => "applied",
                Ok(CasOutcome::Conflict) => "conflict",
                Err(_) => "authority_error",
            };
            observation.runtime_state_cas(
                key,
                expected_generation,
                &observed_next,
                result.as_ref().ok().copied(),
                outcome,
            );
        }
        result
    }

    async fn acquire_probe_lease_exact(
        &self,
        key: &RuntimeStateKey,
        expected_generation: u64,
        now: Instant,
        lease_duration: Duration,
        scope: &ExecutionScope,
    ) -> Result<ProbeLeaseOutcome, PortError> {
        let result = self
            .inner
            .acquire_probe_lease_exact(key, expected_generation, now, lease_duration, scope)
            .await;
        if let Some(observation) = active_request() {
            let outcome = match &result {
                Ok(ProbeLeaseOutcome::Acquired { .. }) => "acquired",
                Ok(ProbeLeaseOutcome::Busy) => "busy",
                Ok(ProbeLeaseOutcome::Conflict) => "conflict",
                Err(_) => "authority_error",
            };
            observation.runtime_probe(
                key,
                expected_generation,
                lease_duration,
                result.as_ref().ok().copied(),
                outcome,
            );
        }
        result
    }
}
