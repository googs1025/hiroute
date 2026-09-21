use serde::{Deserialize, Serialize};

pub use crate::operation::ComputeRuntimeStateStoreV1;

use super::common::*;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAvailability {
    Ready,
    Cooling,
    Disabled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeReason {
    None,
    CredentialQuota429,
    BindingOverload,
    PermanentInsufficient,
    AuthenticationRejected,
}

/// Legacy subject-only projection retained for decoding old product artifacts.
///
/// It is not accepted by [`ComputeRuntimeStateStoreV1`]; schema-v6 storage quarantines its rows
/// read-only because they cannot prove an exact Binding revision or Credential key generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAvailabilityStateV1 {
    pub subject_id: String,
    pub availability: RuntimeAvailability,
    pub reason: RuntimeReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<i64>,
    pub failure_window: u8,
    pub generation: u64,
}

impl RuntimeAvailabilityStateV1 {
    pub fn ready(subject_id: impl Into<String>) -> Result<Self, ComputeContractError> {
        let subject_id = subject_id.into();
        validate_identifier(&subject_id)?;
        Ok(Self {
            subject_id,
            availability: RuntimeAvailability::Ready,
            reason: RuntimeReason::None,
            cooldown_until: None,
            failure_window: 0,
            generation: 0,
        })
    }

    pub fn credential_quota(
        &self,
        expected_generation: u64,
        now: i64,
        trusted_reset: Option<i64>,
    ) -> Result<Self, ComputeContractError> {
        self.cooldown(
            expected_generation,
            now,
            trusted_reset,
            RuntimeReason::CredentialQuota429,
        )
    }

    pub fn binding_overload(
        &self,
        expected_generation: u64,
        now: i64,
        trusted_reset: Option<i64>,
    ) -> Result<Self, ComputeContractError> {
        self.cooldown(
            expected_generation,
            now,
            trusted_reset,
            RuntimeReason::BindingOverload,
        )
    }

    fn cooldown(
        &self,
        expected_generation: u64,
        now: i64,
        trusted_reset: Option<i64>,
        reason: RuntimeReason,
    ) -> Result<Self, ComputeContractError> {
        self.cas(expected_generation)?;
        self.ensure_attemptable()?;
        let failure_window = self.failure_window.saturating_add(1);
        let disabled = failure_window > 3;
        let seconds = match failure_window {
            1 => 60,
            2 => 300,
            _ => 1_800,
        };
        let cooldown_until = if disabled {
            None
        } else {
            Some(match trusted_reset {
                Some(reset) => reset.max(now),
                None => now
                    .checked_add(seconds)
                    .ok_or(ComputeContractError::InvalidRuntimeState)?,
            })
        };
        let state = Self {
            subject_id: self.subject_id.clone(),
            availability: if disabled {
                RuntimeAvailability::Disabled
            } else {
                RuntimeAvailability::Cooling
            },
            reason,
            cooldown_until,
            failure_window,
            generation: self.next_generation()?,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn permanent_insufficient(
        &self,
        expected_generation: u64,
    ) -> Result<Self, ComputeContractError> {
        self.cas(expected_generation)?;
        self.ensure_attemptable()?;
        let state = Self {
            subject_id: self.subject_id.clone(),
            availability: RuntimeAvailability::Disabled,
            reason: RuntimeReason::PermanentInsufficient,
            cooldown_until: None,
            failure_window: self.failure_window.saturating_add(1),
            generation: self.next_generation()?,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn authentication_rejected(
        &self,
        expected_generation: u64,
    ) -> Result<Self, ComputeContractError> {
        self.cas(expected_generation)?;
        self.ensure_attemptable()?;
        let state = Self {
            subject_id: self.subject_id.clone(),
            availability: RuntimeAvailability::Disabled,
            reason: RuntimeReason::AuthenticationRejected,
            cooldown_until: None,
            failure_window: self.failure_window.saturating_add(1),
            generation: self.next_generation()?,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn successful(&self, expected_generation: u64) -> Result<Self, ComputeContractError> {
        self.cas(expected_generation)?;
        if self.availability == RuntimeAvailability::Disabled {
            return Err(ComputeContractError::InvalidRuntimeState);
        }
        let state = Self {
            subject_id: self.subject_id.clone(),
            availability: RuntimeAvailability::Ready,
            reason: RuntimeReason::None,
            cooldown_until: None,
            failure_window: 0,
            generation: self.next_generation()?,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), ComputeContractError> {
        validate_identifier(&self.subject_id)?;
        let valid = match (self.availability, self.reason) {
            (RuntimeAvailability::Ready, RuntimeReason::None) => {
                self.cooldown_until.is_none() && self.failure_window == 0
            }
            (
                RuntimeAvailability::Cooling,
                RuntimeReason::CredentialQuota429 | RuntimeReason::BindingOverload,
            ) => self.cooldown_until.is_some() && (1..=3).contains(&self.failure_window),
            (
                RuntimeAvailability::Disabled,
                RuntimeReason::CredentialQuota429 | RuntimeReason::BindingOverload,
            ) => self.cooldown_until.is_none() && self.failure_window >= 4,
            (
                RuntimeAvailability::Disabled,
                RuntimeReason::PermanentInsufficient | RuntimeReason::AuthenticationRejected,
            ) => self.cooldown_until.is_none() && self.failure_window > 0,
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(ComputeContractError::InvalidRuntimeState)
        }
    }

    fn cas(&self, expected_generation: u64) -> Result<(), ComputeContractError> {
        if self.generation == expected_generation {
            Ok(())
        } else {
            Err(ComputeContractError::GenerationConflict)
        }
    }

    fn next_generation(&self) -> Result<u64, ComputeContractError> {
        self.generation
            .checked_add(1)
            .ok_or(ComputeContractError::GenerationConflict)
    }

    fn ensure_attemptable(&self) -> Result<(), ComputeContractError> {
        if self.availability == RuntimeAvailability::Disabled {
            Err(ComputeContractError::InvalidRuntimeState)
        } else {
            Ok(())
        }
    }
}

pub const COMPUTE_RUNTIME_IDENTITY_SCHEMA_V1: &str = "hiroute.compute-runtime-identity/v1";
pub const COMPUTE_RUNTIME_STATE_SCHEMA_V1: &str = "hiroute.compute-runtime-state/v1";
pub const COMPUTE_PROBE_LEASE_SCHEMA_V1: &str = "hiroute.compute-probe-lease/v1";
pub const MAX_RUNTIME_GENERATION: u64 = i64::MAX as u64;

/// The frozen Gateway runtime-state key, represented without a publication lookup or sidecar.
///
/// `key_id` is an opaque identifier. This contract stores it byte-for-byte as UTF-8 and never
/// parses or hashes it into a different identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStateIdentityV1 {
    schema: String,
    stable_binding_id: String,
    subject: RuntimeStateSubjectV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeStateSubjectV1 {
    Binding,
    Credential {
        credential_ref: String,
        key_id: String,
        credential_generation: u64,
    },
}

impl RuntimeStateIdentityV1 {
    pub fn binding(stable_binding_id: impl Into<String>) -> Result<Self, ComputeContractError> {
        let identity = Self {
            schema: COMPUTE_RUNTIME_IDENTITY_SCHEMA_V1.to_owned(),
            stable_binding_id: stable_binding_id.into(),
            subject: RuntimeStateSubjectV1::Binding,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn credential(
        stable_binding_id: impl Into<String>,
        credential_ref: impl Into<String>,
        key_id: impl Into<String>,
        credential_generation: u64,
    ) -> Result<Self, ComputeContractError> {
        let identity = Self {
            schema: COMPUTE_RUNTIME_IDENTITY_SCHEMA_V1.to_owned(),
            stable_binding_id: stable_binding_id.into(),
            subject: RuntimeStateSubjectV1::Credential {
                credential_ref: credential_ref.into(),
                key_id: key_id.into(),
                credential_generation,
            },
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<(), ComputeContractError> {
        if self.schema != COMPUTE_RUNTIME_IDENTITY_SCHEMA_V1
            || invalid_exact_component(&self.stable_binding_id)
        {
            return Err(ComputeContractError::InvalidRuntimeState);
        }
        if let RuntimeStateSubjectV1::Credential {
            credential_ref,
            key_id,
            credential_generation,
        } = &self.subject
            && (invalid_exact_component(credential_ref)
                || invalid_exact_component(key_id)
                || *credential_generation == 0)
        {
            return Err(ComputeContractError::InvalidRuntimeState);
        }
        Ok(())
    }

    /// Stable storage identity containing the exact versioned key fields without hashing them.
    ///
    /// SQLite stores the same canonical JSON separately and compares it on every read, so an
    /// identity-key mismatch fails closed rather than aliasing two runtime subjects.
    pub fn canonical_key(&self) -> Result<String, ComputeContractError> {
        self.validate()?;
        let encoded =
            serde_json::to_string(self).map_err(|_| ComputeContractError::InvalidRuntimeState)?;
        Ok(format!("compute-runtime/v1/{encoded}"))
    }

    pub fn stable_binding_id(&self) -> &str {
        &self.stable_binding_id
    }

    pub fn subject(&self) -> &RuntimeStateSubjectV1 {
        &self.subject
    }

    pub const fn is_credential(&self) -> bool {
        matches!(self.subject, RuntimeStateSubjectV1::Credential { .. })
    }
}

fn invalid_exact_component(value: &str) -> bool {
    value.trim().is_empty()
}

/// A persisted wall-clock sample in Unix epoch milliseconds.
///
/// Expiry is deterministic: a deadline is expired exactly when `sample >= deadline`. No
/// process-local monotonic clock value is serialized. Each exact state rejects samples older than
/// its persisted `updated_at_unix_millis`, so a clock rollback delays work instead of creating a
/// second probe owner.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RuntimeClockSampleV1(i64);

impl RuntimeClockSampleV1 {
    pub fn from_unix_millis(value: i64) -> Result<Self, ComputeContractError> {
        if value < 0 {
            Err(ComputeContractError::InvalidRuntimeState)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn unix_millis(self) -> i64 {
        self.0
    }

    pub const fn has_reached(self, deadline_unix_millis: i64) -> bool {
        self.0 >= deadline_unix_millis
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProbeLeaseRequestV1 {
    schema: String,
    sampled_at_unix_millis: i64,
    lease_duration_millis: u64,
}

impl RuntimeProbeLeaseRequestV1 {
    pub fn new(
        sampled_at: RuntimeClockSampleV1,
        lease_duration_millis: u64,
    ) -> Result<Self, ComputeContractError> {
        let request = Self {
            schema: COMPUTE_PROBE_LEASE_SCHEMA_V1.to_owned(),
            sampled_at_unix_millis: sampled_at.unix_millis(),
            lease_duration_millis,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), ComputeContractError> {
        if self.schema != COMPUTE_PROBE_LEASE_SCHEMA_V1
            || self.sampled_at_unix_millis < 0
            || self.lease_duration_millis == 0
            || self.expires_at_unix_millis().is_none()
        {
            Err(ComputeContractError::InvalidRuntimeState)
        } else {
            Ok(())
        }
    }

    pub fn sampled_at(&self) -> RuntimeClockSampleV1 {
        // `new` and adapter validation reject negative values.
        RuntimeClockSampleV1(self.sampled_at_unix_millis)
    }

    pub fn expires_at_unix_millis(&self) -> Option<i64> {
        let duration = i64::try_from(self.lease_duration_millis).ok()?;
        self.sampled_at_unix_millis.checked_add(duration)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProbeLeaseV1 {
    schema: String,
    identity_key: String,
    expires_at_unix_millis: i64,
    fence_generation: u64,
}

/// Frozen transactional result of one probe acquisition. `Busy` is an ineligible or already
/// leased state at the supplied clock sample; `Conflict` means the caller's exact generation or
/// clock fence no longer names the durable state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "outcome", content = "state", rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "the frozen port API is Acquired(RuntimeStateV1), and boxing would change that API"
)]
pub enum RuntimeProbeAcquireOutcomeV1 {
    Acquired(RuntimeStateV1),
    Busy,
    Conflict,
}

impl RuntimeProbeLeaseV1 {
    pub fn validate(&self) -> Result<(), ComputeContractError> {
        if self.schema != COMPUTE_PROBE_LEASE_SCHEMA_V1
            || !self.identity_key.starts_with("compute-runtime/v1/")
            || self.expires_at_unix_millis < 0
            || self.fence_generation == 0
            || self.fence_generation > MAX_RUNTIME_GENERATION
        {
            Err(ComputeContractError::InvalidRuntimeState)
        } else {
            Ok(())
        }
    }

    pub fn identity_key(&self) -> &str {
        &self.identity_key
    }

    pub const fn expires_at_unix_millis(&self) -> i64 {
        self.expires_at_unix_millis
    }

    pub const fn fence_generation(&self) -> u64 {
        self.fence_generation
    }

    pub const fn is_expired_at(&self, sample: RuntimeClockSampleV1) -> bool {
        sample.has_reached(self.expires_at_unix_millis)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputeRuntimeHealthV1 {
    Ready,
    CoolingDown { until_unix_millis: i64 },
    Disabled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStateV1 {
    schema: String,
    identity: RuntimeStateIdentityV1,
    generation: u64,
    health: ComputeRuntimeHealthV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    probe_lease: Option<RuntimeProbeLeaseV1>,
    transient_backoff_step: u8,
    updated_at_unix_millis: i64,
}

impl RuntimeStateV1 {
    pub fn ready(
        identity: RuntimeStateIdentityV1,
        generation: u64,
        sampled_at: RuntimeClockSampleV1,
    ) -> Result<Self, ComputeContractError> {
        Self::from_exact_parts(
            identity,
            generation,
            ComputeRuntimeHealthV1::Ready,
            None,
            sampled_at,
        )
    }

    pub fn cooling_down(
        identity: RuntimeStateIdentityV1,
        generation: u64,
        until_unix_millis: i64,
        probe_lease_until_unix_millis: Option<i64>,
        sampled_at: RuntimeClockSampleV1,
    ) -> Result<Self, ComputeContractError> {
        Self::from_exact_parts(
            identity,
            generation,
            ComputeRuntimeHealthV1::CoolingDown { until_unix_millis },
            probe_lease_until_unix_millis,
            sampled_at,
        )
    }

    pub fn disabled(
        identity: RuntimeStateIdentityV1,
        generation: u64,
        sampled_at: RuntimeClockSampleV1,
    ) -> Result<Self, ComputeContractError> {
        Self::from_exact_parts(
            identity,
            generation,
            ComputeRuntimeHealthV1::Disabled,
            None,
            sampled_at,
        )
    }

    /// Builds the durable counterpart of one frozen Gateway `RuntimeStateEntry`.
    ///
    /// The caller converts each monotonic `Instant` deadline to Unix milliseconds from one
    /// explicit clock sample. Only the bounded Binding transient-backoff step
    /// is retained in addition to health and lease state.
    pub fn from_exact_parts(
        identity: RuntimeStateIdentityV1,
        generation: u64,
        health: ComputeRuntimeHealthV1,
        probe_lease_until_unix_millis: Option<i64>,
        sampled_at: RuntimeClockSampleV1,
    ) -> Result<Self, ComputeContractError> {
        Self::from_exact_parts_with_backoff(
            identity,
            generation,
            health,
            probe_lease_until_unix_millis,
            0,
            sampled_at,
        )
    }

    pub fn from_exact_parts_with_backoff(
        identity: RuntimeStateIdentityV1,
        generation: u64,
        health: ComputeRuntimeHealthV1,
        probe_lease_until_unix_millis: Option<i64>,
        transient_backoff_step: u8,
        sampled_at: RuntimeClockSampleV1,
    ) -> Result<Self, ComputeContractError> {
        let probe_lease = match probe_lease_until_unix_millis {
            Some(expires_at_unix_millis) => Some(RuntimeProbeLeaseV1 {
                schema: COMPUTE_PROBE_LEASE_SCHEMA_V1.to_owned(),
                identity_key: identity.canonical_key()?,
                expires_at_unix_millis,
                fence_generation: generation,
            }),
            None => None,
        };
        let state = Self {
            schema: COMPUTE_RUNTIME_STATE_SCHEMA_V1.to_owned(),
            identity,
            generation,
            health,
            probe_lease,
            transient_backoff_step,
            updated_at_unix_millis: sampled_at.unix_millis(),
        };
        state.validate()?;
        Ok(state)
    }

    pub fn acquire_probe(
        &self,
        expected_generation: u64,
        request: &RuntimeProbeLeaseRequestV1,
    ) -> Result<Self, ComputeContractError> {
        self.validate()?;
        self.cas(expected_generation)?;
        request.validate()?;
        let sampled_at = request.sampled_at();
        self.ensure_clock(sampled_at)?;
        let ComputeRuntimeHealthV1::CoolingDown { until_unix_millis } = self.health else {
            return Err(ComputeContractError::InvalidRuntimeState);
        };
        if !sampled_at.has_reached(until_unix_millis)
            || self
                .probe_lease
                .as_ref()
                .is_some_and(|lease| !lease.is_expired_at(sampled_at))
        {
            return Err(ComputeContractError::InvalidRuntimeState);
        }
        let generation = self.next_generation()?;
        let lease = RuntimeProbeLeaseV1 {
            schema: COMPUTE_PROBE_LEASE_SCHEMA_V1.to_owned(),
            identity_key: self.identity.canonical_key()?,
            expires_at_unix_millis: request
                .expires_at_unix_millis()
                .ok_or(ComputeContractError::InvalidRuntimeState)?,
            fence_generation: generation,
        };
        let state = Self {
            schema: self.schema.clone(),
            identity: self.identity.clone(),
            generation,
            health: self.health,
            probe_lease: Some(lease),
            transient_backoff_step: self.transient_backoff_step,
            updated_at_unix_millis: sampled_at.unix_millis(),
        };
        state.validate()?;
        Ok(state)
    }

    pub fn cancel_probe(
        &self,
        lease: &RuntimeProbeLeaseV1,
        sampled_at: RuntimeClockSampleV1,
    ) -> Result<Self, ComputeContractError> {
        self.validate_lease_fence(lease)?;
        self.ensure_clock(sampled_at)?;
        let state = Self {
            schema: self.schema.clone(),
            identity: self.identity.clone(),
            generation: self.next_generation()?,
            health: self.health,
            probe_lease: None,
            transient_backoff_step: self.transient_backoff_step,
            updated_at_unix_millis: sampled_at.unix_millis(),
        };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), ComputeContractError> {
        self.identity.validate()?;
        if self.schema != COMPUTE_RUNTIME_STATE_SCHEMA_V1
            || self.generation > MAX_RUNTIME_GENERATION
            || self.transient_backoff_step > 5
            || self.updated_at_unix_millis < 0
        {
            return Err(ComputeContractError::InvalidRuntimeState);
        }
        if let Some(lease) = &self.probe_lease {
            lease.validate()?;
            if lease.fence_generation != self.generation
                || lease.identity_key != self.identity.canonical_key()?
            {
                return Err(ComputeContractError::InvalidRuntimeState);
            }
        }
        if let ComputeRuntimeHealthV1::CoolingDown { until_unix_millis } = self.health
            && until_unix_millis < 0
        {
            return Err(ComputeContractError::InvalidRuntimeState);
        }
        if self.generation == 0
            && (self.health != ComputeRuntimeHealthV1::Ready
                || self.probe_lease.is_some()
                || self.transient_backoff_step != 0)
        {
            return Err(ComputeContractError::InvalidRuntimeState);
        }
        if self.transient_backoff_step != 0
            && (self.identity.is_credential()
                || !matches!(self.health, ComputeRuntimeHealthV1::CoolingDown { .. }))
        {
            return Err(ComputeContractError::InvalidRuntimeState);
        }
        Ok(())
    }

    pub fn identity(&self) -> &RuntimeStateIdentityV1 {
        &self.identity
    }

    pub const fn health(&self) -> ComputeRuntimeHealthV1 {
        self.health
    }

    pub const fn cooldown_until_unix_millis(&self) -> Option<i64> {
        match self.health {
            ComputeRuntimeHealthV1::CoolingDown { until_unix_millis } => Some(until_unix_millis),
            ComputeRuntimeHealthV1::Ready | ComputeRuntimeHealthV1::Disabled => None,
        }
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn transient_backoff_step(&self) -> u8 {
        self.transient_backoff_step
    }

    pub fn probe_lease(&self) -> Option<&RuntimeProbeLeaseV1> {
        self.probe_lease.as_ref()
    }

    pub const fn updated_at_unix_millis(&self) -> i64 {
        self.updated_at_unix_millis
    }

    pub fn validate_direct_successor(&self, next: &Self) -> Result<(), ComputeContractError> {
        self.validate()?;
        next.validate()?;
        if self.identity != next.identity
            || self.probe_lease.is_some()
            || next.probe_lease.is_some()
            || self.next_generation()? != next.generation
            || next.updated_at_unix_millis < self.updated_at_unix_millis
        {
            return Err(ComputeContractError::InvalidRuntimeState);
        }
        Ok(())
    }

    pub fn validate_probe_successor(
        &self,
        lease: &RuntimeProbeLeaseV1,
        next: &Self,
    ) -> Result<(), ComputeContractError> {
        self.validate()?;
        next.validate()?;
        self.validate_lease_fence(lease)?;
        if self.identity != next.identity
            || next.probe_lease.is_some()
            || lease.fence_generation.checked_add(1) != Some(next.generation)
            || next.updated_at_unix_millis < self.updated_at_unix_millis
        {
            return Err(ComputeContractError::InvalidRuntimeState);
        }
        Ok(())
    }

    fn validate_lease_fence(
        &self,
        lease: &RuntimeProbeLeaseV1,
    ) -> Result<(), ComputeContractError> {
        self.validate()?;
        lease.validate()?;
        if self.probe_lease.as_ref() != Some(lease) || lease.fence_generation != self.generation {
            Err(ComputeContractError::InvalidRuntimeState)
        } else {
            Ok(())
        }
    }

    fn ensure_clock(&self, sampled_at: RuntimeClockSampleV1) -> Result<(), ComputeContractError> {
        if sampled_at.unix_millis() < self.updated_at_unix_millis {
            Err(ComputeContractError::InvalidRuntimeState)
        } else {
            Ok(())
        }
    }

    fn cas(&self, expected_generation: u64) -> Result<(), ComputeContractError> {
        if self.generation == expected_generation {
            Ok(())
        } else {
            Err(ComputeContractError::GenerationConflict)
        }
    }

    fn next_generation(&self) -> Result<u64, ComputeContractError> {
        if self.generation >= MAX_RUNTIME_GENERATION {
            Err(ComputeContractError::GenerationConflict)
        } else {
            Ok(self.generation + 1)
        }
    }
}

#[cfg(test)]
mod exact_contract_tests {
    use super::*;

    fn clock(value: i64) -> RuntimeClockSampleV1 {
        RuntimeClockSampleV1::from_unix_millis(value).unwrap()
    }

    fn credential_identity(generation: u64, key_id: &str) -> RuntimeStateIdentityV1 {
        RuntimeStateIdentityV1::credential(
            "binding/primary",
            "credential/ref-a",
            key_id,
            generation,
        )
        .unwrap()
    }

    #[test]
    fn frozen_g0_identity_roundtrips_opaque_key_without_extra_fields() {
        let opaque_key = "provider key @ slot?#/🔥";
        let first = credential_identity(1, opaque_key);
        let same = credential_identity(1, opaque_key);
        let rotated = credential_identity(2, opaque_key);
        let other_key = credential_identity(1, "not-a-digest:either");
        let revised_binding = RuntimeStateIdentityV1::credential(
            "binding/primary",
            "credential/ref-b",
            opaque_key,
            1,
        )
        .unwrap();
        let other_binding = RuntimeStateIdentityV1::credential(
            "binding/secondary",
            "credential/ref-a",
            opaque_key,
            1,
        )
        .unwrap();
        let first_key = first.canonical_key().unwrap();
        assert_eq!(first_key, same.canonical_key().unwrap());
        assert!(first_key.contains(opaque_key));
        let encoded = serde_json::to_value(&first).unwrap();
        assert_eq!(encoded["stable_binding_id"], "binding/primary");
        assert_eq!(encoded["subject"]["credential_ref"], "credential/ref-a");
        assert_eq!(encoded["subject"]["key_id"], opaque_key);
        assert_eq!(encoded["subject"]["credential_generation"], 1);
        assert!(encoded.get("binding_revision").is_none());
        assert!(encoded.get("binding_digest").is_none());
        assert!(encoded["subject"].get("key_fingerprint").is_none());
        let decoded: RuntimeStateIdentityV1 = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, first);
        let binding = RuntimeStateIdentityV1::binding("binding/primary").unwrap();
        for distinct in [rotated, other_key, revised_binding, other_binding, binding] {
            assert_ne!(first_key, distinct.canonical_key().unwrap());
        }
    }

    #[test]
    fn repeated_cooling_down_and_explicit_disabled_are_gateway_owned() {
        let identity = credential_identity(1, "opaque:key-A");
        let mut current = RuntimeStateV1::ready(identity.clone(), 0, clock(100)).unwrap();
        for generation in 1..=5 {
            let next = RuntimeStateV1::cooling_down(
                identity.clone(),
                generation,
                100 + i64::try_from(generation).unwrap(),
                None,
                clock(100 + i64::try_from(generation).unwrap()),
            )
            .unwrap();
            current.validate_direct_successor(&next).unwrap();
            current = next;
            assert!(matches!(
                current.health(),
                ComputeRuntimeHealthV1::CoolingDown { .. }
            ));
        }
        let disabled = RuntimeStateV1::disabled(identity, 6, clock(106)).unwrap();
        current.validate_direct_successor(&disabled).unwrap();
        assert_eq!(disabled.health(), ComputeRuntimeHealthV1::Disabled);
    }

    #[test]
    fn binding_backoff_is_required_roundtrips_and_survives_probe_lease() {
        let identity = RuntimeStateIdentityV1::binding("binding/backoff").unwrap();
        let cooling = RuntimeStateV1::from_exact_parts_with_backoff(
            identity,
            1,
            ComputeRuntimeHealthV1::CoolingDown {
                until_unix_millis: 2_000,
            },
            None,
            3,
            clock(1_000),
        )
        .unwrap();
        let encoded = serde_json::to_value(&cooling).unwrap();
        assert_eq!(encoded["transient_backoff_step"], 3);
        assert_eq!(
            serde_json::from_value::<RuntimeStateV1>(encoded.clone())
                .unwrap()
                .transient_backoff_step(),
            3
        );
        let mut missing = encoded;
        missing
            .as_object_mut()
            .unwrap()
            .remove("transient_backoff_step");
        assert!(serde_json::from_value::<RuntimeStateV1>(missing).is_err());

        let leased = cooling
            .acquire_probe(
                1,
                &RuntimeProbeLeaseRequestV1::new(clock(2_000), 1_000).unwrap(),
            )
            .unwrap();
        assert_eq!(leased.transient_backoff_step(), 3);
        let cancelled = leased
            .cancel_probe(leased.probe_lease().unwrap(), clock(2_100))
            .unwrap();
        assert_eq!(cancelled.transient_backoff_step(), 3);
    }

    #[test]
    fn credential_runtime_state_rejects_binding_backoff_steps() {
        assert_eq!(
            RuntimeStateV1::from_exact_parts_with_backoff(
                credential_identity(1, "opaque:key-A"),
                1,
                ComputeRuntimeHealthV1::CoolingDown {
                    until_unix_millis: 2_000,
                },
                None,
                1,
                clock(1_000),
            ),
            Err(ComputeContractError::InvalidRuntimeState)
        );
    }

    #[test]
    fn durable_deadline_and_probe_fence_are_deterministic() {
        let identity = credential_identity(1, "opaque:key-A");
        let cooling =
            RuntimeStateV1::cooling_down(identity.clone(), 1, 2_000, None, clock(1_000)).unwrap();
        let request = RuntimeProbeLeaseRequestV1::new(clock(2_000), 1_000).unwrap();
        let leased = cooling.acquire_probe(1, &request).unwrap();
        let lease = leased.probe_lease().unwrap();
        assert_eq!(lease.fence_generation(), 2);
        assert!(!lease.is_expired_at(clock(2_999)));
        assert!(lease.is_expired_at(clock(3_000)));
        let next =
            RuntimeStateV1::cooling_down(identity.clone(), 3, 4_000, None, clock(2_999)).unwrap();
        leased.validate_probe_successor(lease, &next).unwrap();
        let completed_at_expiry = RuntimeStateV1::ready(identity.clone(), 3, clock(3_000)).unwrap();
        leased
            .validate_probe_successor(lease, &completed_at_expiry)
            .unwrap();
        let replacement = leased
            .acquire_probe(
                2,
                &RuntimeProbeLeaseRequestV1::new(clock(3_000), 1_000).unwrap(),
            )
            .unwrap();
        assert_eq!(
            replacement.validate_probe_successor(lease, &completed_at_expiry),
            Err(ComputeContractError::InvalidRuntimeState)
        );
        let rollback_guard =
            RuntimeStateV1::cooling_down(identity, 1, 500, None, clock(1_000)).unwrap();
        assert_eq!(
            rollback_guard.acquire_probe(
                1,
                &RuntimeProbeLeaseRequestV1::new(clock(999), 1_000).unwrap(),
            ),
            Err(ComputeContractError::InvalidRuntimeState)
        );
    }

    #[test]
    fn runtime_generation_conflict_and_overflow_fail_closed() {
        let identity = credential_identity(1, "opaque:key-A");
        let initial = RuntimeStateV1::ready(identity.clone(), 0, clock(1)).unwrap();
        let next = RuntimeStateV1::ready(identity.clone(), 1, clock(2)).unwrap();
        assert_eq!(
            initial.validate_direct_successor(
                &RuntimeStateV1::ready(identity.clone(), 2, clock(2)).unwrap(),
            ),
            Err(ComputeContractError::InvalidRuntimeState)
        );
        initial.validate_direct_successor(&next).unwrap();
        let exhausted =
            RuntimeStateV1::cooling_down(identity, MAX_RUNTIME_GENERATION, 2, None, clock(2))
                .unwrap();
        assert_eq!(
            exhausted.acquire_probe(
                MAX_RUNTIME_GENERATION,
                &RuntimeProbeLeaseRequestV1::new(clock(2), 1).unwrap(),
            ),
            Err(ComputeContractError::GenerationConflict)
        );
    }
}
