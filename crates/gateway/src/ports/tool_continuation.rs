//! Bounded, process-local authority for accepted Tool-call continuation.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::server::core_runtime::model_ir::ToolIdMapEntryV1;

pub const DEFAULT_TOOL_CONTINUATION_CAPACITY: usize = 16_384;
pub const DEFAULT_TOOL_CONTINUATION_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_TOOL_CONTINUATION_CAPACITY: usize = 1_000_000;
const MAX_TOOL_CONTINUATION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Exact logical authority scope. Deliberately excludes ingress protocol so a
/// continuation may be losslessly projected through another supported client
/// protocol, while Workspace epoch, Grant generation, and AgentPlan semantic
/// identity remain hard boundaries.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ToolContinuationScopeV1 {
    pub authority_id: String,
    pub authority_epoch: u64,
    pub grant_id: String,
    pub grant_generation: u64,
    pub served_model_id: String,
    pub route: hiroute_domain::ModelRequestRouteV2,
}

impl ToolContinuationScopeV1 {
    pub fn is_complete(&self) -> bool {
        self.authority_epoch > 0
            && self.grant_generation > 0
            && self.route.validate().is_ok()
            && [
                self.authority_id.as_str(),
                self.grant_id.as_str(),
                self.served_model_id.as_str(),
            ]
            .into_iter()
            .all(|value| !value.trim().is_empty())
    }
}

/// Request-owned issuance identity. It is never serialized or accepted from
/// the client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolContinuationIssuanceV1 {
    sequence: u64,
    scope: ToolContinuationScopeV1,
}

impl ToolContinuationIssuanceV1 {
    pub fn scope(&self) -> &ToolContinuationScopeV1 {
        &self.scope
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ToolContinuationError {
    #[error("Tool continuation authority is unavailable")]
    Unavailable,
    #[error("Tool continuation identity conflicts with existing authority")]
    Conflict,
    #[error("Tool continuation authority capacity is exhausted")]
    Capacity,
    #[error("Tool continuation authority configuration is invalid")]
    InvalidConfiguration,
}

/// Trusted local port. A mapping starts pending while native response bytes
/// are decoded and becomes resolvable only after the corresponding logical ID
/// is accepted by the downstream transport.
pub trait ToolContinuationAuthority: Send + Sync {
    fn begin(
        &self,
        scope: ToolContinuationScopeV1,
    ) -> Result<ToolContinuationIssuanceV1, ToolContinuationError>;

    fn record_pending(
        &self,
        issuance: &ToolContinuationIssuanceV1,
        mapping: ToolIdMapEntryV1,
        now: Instant,
    ) -> Result<(), ToolContinuationError>;

    fn accept(&self, issuance: &ToolContinuationIssuanceV1, logical_id: &str, now: Instant)
    -> bool;

    fn abort_pending(&self, issuance: &ToolContinuationIssuanceV1);

    fn resolve(
        &self,
        scope: &ToolContinuationScopeV1,
        logical_ids: &[String],
        now: Instant,
    ) -> Result<Vec<ToolIdMapEntryV1>, ToolContinuationError>;
}

pub struct InMemoryToolContinuationAuthority {
    capacity: usize,
    ttl: Duration,
    next_issuance: AtomicU64,
    state: Mutex<AuthorityState>,
}

#[derive(Default)]
struct AuthorityState {
    entries: BTreeMap<String, StoredMapping>,
    insertion_order: VecDeque<(u64, String)>,
    next_entry: u64,
}

struct StoredMapping {
    scope: ToolContinuationScopeV1,
    mapping: ToolIdMapEntryV1,
    state: MappingState,
    expires_at: Option<Instant>,
    entry_sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MappingState {
    Pending(u64),
    Active,
    Conflicted,
}

impl Default for InMemoryToolContinuationAuthority {
    fn default() -> Self {
        Self::new(
            DEFAULT_TOOL_CONTINUATION_CAPACITY,
            DEFAULT_TOOL_CONTINUATION_TTL,
        )
        .expect("static Tool continuation limits are valid")
    }
}

impl InMemoryToolContinuationAuthority {
    pub fn new(capacity: usize, ttl: Duration) -> Result<Self, ToolContinuationError> {
        if capacity == 0
            || capacity > MAX_TOOL_CONTINUATION_CAPACITY
            || ttl.is_zero()
            || ttl > MAX_TOOL_CONTINUATION_TTL
        {
            return Err(ToolContinuationError::InvalidConfiguration);
        }
        Ok(Self {
            capacity,
            ttl,
            next_issuance: AtomicU64::new(1),
            state: Mutex::new(AuthorityState::default()),
        })
    }

    pub fn from_environment() -> Result<Self, ToolContinuationError> {
        let capacity = environment_usize(
            "HIROUTE_TOOL_CONTINUATION_CAPACITY",
            DEFAULT_TOOL_CONTINUATION_CAPACITY,
        )?;
        let ttl_ms = environment_u64(
            "HIROUTE_TOOL_CONTINUATION_TTL_MS",
            u64::try_from(DEFAULT_TOOL_CONTINUATION_TTL.as_millis())
                .map_err(|_| ToolContinuationError::InvalidConfiguration)?,
        )?;
        Self::new(capacity, Duration::from_millis(ttl_ms))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AuthorityState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn prune_expired(state: &mut AuthorityState, now: Instant) {
        state
            .entries
            .retain(|_, entry| entry.expires_at.is_none_or(|expires_at| now < expires_at));
        Self::compact_order(state);
    }

    fn compact_order(state: &mut AuthorityState) {
        let entries = &state.entries;
        state.insertion_order.retain(|(sequence, logical_id)| {
            entries
                .get(logical_id)
                .is_some_and(|entry| entry.entry_sequence == *sequence)
        });
    }

    fn reserve_slot(
        &self,
        state: &mut AuthorityState,
        now: Instant,
    ) -> Result<(), ToolContinuationError> {
        Self::prune_expired(state, now);
        let candidates = state.insertion_order.len();
        for _ in 0..candidates {
            if state.entries.len() < self.capacity {
                return Ok(());
            }
            let Some((sequence, logical_id)) = state.insertion_order.pop_front() else {
                break;
            };
            let removable = state.entries.get(&logical_id).is_some_and(|entry| {
                entry.entry_sequence == sequence && !matches!(entry.state, MappingState::Pending(_))
            });
            if removable {
                state.entries.remove(&logical_id);
            } else if state
                .entries
                .get(&logical_id)
                .is_some_and(|entry| entry.entry_sequence == sequence)
            {
                state.insertion_order.push_back((sequence, logical_id));
            }
        }
        (state.entries.len() < self.capacity)
            .then_some(())
            .ok_or(ToolContinuationError::Capacity)
    }

    fn mark_conflicted(&self, entry: &mut StoredMapping, now: Instant) {
        entry.state = MappingState::Conflicted;
        entry.expires_at = Some(now.checked_add(self.ttl).unwrap_or(now));
    }
}

impl ToolContinuationAuthority for InMemoryToolContinuationAuthority {
    fn begin(
        &self,
        scope: ToolContinuationScopeV1,
    ) -> Result<ToolContinuationIssuanceV1, ToolContinuationError> {
        if !scope.is_complete() {
            return Err(ToolContinuationError::Unavailable);
        }
        let sequence = self.next_issuance.fetch_add(1, Ordering::Relaxed);
        if sequence == 0 {
            return Err(ToolContinuationError::Unavailable);
        }
        Ok(ToolContinuationIssuanceV1 { sequence, scope })
    }

    fn record_pending(
        &self,
        issuance: &ToolContinuationIssuanceV1,
        mapping: ToolIdMapEntryV1,
        now: Instant,
    ) -> Result<(), ToolContinuationError> {
        if mapping.logical_id.trim().is_empty()
            || mapping.native_id.trim().is_empty()
            || !mapping.owner.is_complete()
        {
            return Err(ToolContinuationError::Unavailable);
        }
        let mut state = self.lock();
        Self::prune_expired(&mut state, now);
        if let Some(current) = state.entries.get_mut(&mapping.logical_id) {
            let exact = current.scope == issuance.scope && current.mapping == mapping;
            match current.state {
                MappingState::Pending(sequence) if exact && sequence == issuance.sequence => {
                    return Ok(());
                }
                MappingState::Active if exact => return Ok(()),
                MappingState::Pending(_) | MappingState::Active | MappingState::Conflicted => {
                    self.mark_conflicted(current, now);
                    return Err(ToolContinuationError::Conflict);
                }
            }
        }
        self.reserve_slot(&mut state, now)?;
        state.next_entry = state.next_entry.wrapping_add(1);
        if state.next_entry == 0 {
            state.next_entry = 1;
        }
        let entry_sequence = state.next_entry;
        state
            .insertion_order
            .push_back((entry_sequence, mapping.logical_id.clone()));
        state.entries.insert(
            mapping.logical_id.clone(),
            StoredMapping {
                scope: issuance.scope.clone(),
                mapping,
                state: MappingState::Pending(issuance.sequence),
                expires_at: None,
                entry_sequence,
            },
        );
        Ok(())
    }

    fn accept(
        &self,
        issuance: &ToolContinuationIssuanceV1,
        logical_id: &str,
        now: Instant,
    ) -> bool {
        let mut state = self.lock();
        Self::prune_expired(&mut state, now);
        let Some(entry) = state.entries.get_mut(logical_id) else {
            return false;
        };
        match entry.state {
            MappingState::Pending(sequence)
                if sequence == issuance.sequence && entry.scope == issuance.scope =>
            {
                entry.state = MappingState::Active;
                entry.expires_at = Some(now.checked_add(self.ttl).unwrap_or(now));
                true
            }
            MappingState::Active if entry.scope == issuance.scope => true,
            MappingState::Pending(_) | MappingState::Active | MappingState::Conflicted => false,
        }
    }

    fn abort_pending(&self, issuance: &ToolContinuationIssuanceV1) {
        let mut state = self.lock();
        state.entries.retain(|_, entry| {
            !matches!(entry.state, MappingState::Pending(sequence) if sequence == issuance.sequence)
        });
        Self::compact_order(&mut state);
    }

    fn resolve(
        &self,
        scope: &ToolContinuationScopeV1,
        logical_ids: &[String],
        now: Instant,
    ) -> Result<Vec<ToolIdMapEntryV1>, ToolContinuationError> {
        if !scope.is_complete() {
            return Err(ToolContinuationError::Unavailable);
        }
        let mut state = self.lock();
        Self::prune_expired(&mut state, now);
        logical_ids
            .iter()
            .map(|logical_id| {
                let entry = state
                    .entries
                    .get(logical_id)
                    .ok_or(ToolContinuationError::Unavailable)?;
                if entry.scope != *scope || entry.state != MappingState::Active {
                    return Err(ToolContinuationError::Unavailable);
                }
                Ok(entry.mapping.clone())
            })
            .collect()
    }
}

fn environment_usize(name: &str, default: usize) -> Result<usize, ToolContinuationError> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(ToolContinuationError::InvalidConfiguration),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(ToolContinuationError::InvalidConfiguration),
    }
}

fn environment_u64(name: &str, default: u64) -> Result<u64, ToolContinuationError> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(ToolContinuationError::InvalidConfiguration),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(ToolContinuationError::InvalidConfiguration),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::core_runtime::model_ir::ExactProviderPathV1;
    use crate::server::request_plan::IngressProtocol;

    fn scope(name: &str) -> ToolContinuationScopeV1 {
        ToolContinuationScopeV1 {
            authority_id: format!("authority-{name}"),
            authority_epoch: 1,
            grant_id: "grant".into(),
            grant_generation: 1,
            served_model_id: "agent".into(),
            route: hiroute_domain::ModelRequestRouteV2::Plan {
                revision: 7,
                semantic_digest: hiroute_domain::CanonicalDigest::of_bytes(b"plan"),
            },
        }
    }

    fn mapping(logical_id: &str, native_id: &str) -> ToolIdMapEntryV1 {
        ToolIdMapEntryV1 {
            logical_id: logical_id.into(),
            native_id: native_id.into(),
            kind: crate::server::core_runtime::model_ir::ToolKindV1::Function,
            name: "tool".into(),
            namespace: None,
            owner: ExactProviderPathV1 {
                provider_id: "provider".into(),
                endpoint_id: "endpoint".into(),
                entitlement_id: "entitlement".into(),
                connector_id: "connector".into(),
                connector_revision: "1".into(),
                capability_id: "capability".into(),
                capability_revision: "1".into(),
                model_configuration_id: "configuration".into(),
                native_model: "native".into(),
                upstream_protocol: IngressProtocol::Responses,
                adapter_revision: "1".into(),
                serializer_revision: "1".into(),
                decoder_revision: "1".into(),
            },
        }
    }

    #[test]
    fn pending_mapping_requires_downstream_acceptance_and_exact_scope() {
        let authority = InMemoryToolContinuationAuthority::new(4, Duration::from_secs(1)).unwrap();
        let now = Instant::now();
        let issuance = authority.begin(scope("a")).unwrap();
        let entry = mapping("hiroute_tool_v1_a", "provider-a");
        authority
            .record_pending(&issuance, entry.clone(), now)
            .unwrap();
        assert_eq!(
            authority.resolve(
                issuance.scope(),
                std::slice::from_ref(&entry.logical_id),
                now
            ),
            Err(ToolContinuationError::Unavailable)
        );
        assert!(authority.accept(&issuance, &entry.logical_id, now));
        assert_eq!(
            authority
                .resolve(
                    issuance.scope(),
                    std::slice::from_ref(&entry.logical_id),
                    now,
                )
                .unwrap(),
            vec![entry.clone()]
        );
        assert_eq!(
            authority.resolve(&scope("b"), &[entry.logical_id], now),
            Err(ToolContinuationError::Unavailable)
        );
    }

    #[test]
    fn expiration_abort_and_conflict_fail_closed() {
        let authority =
            InMemoryToolContinuationAuthority::new(2, Duration::from_millis(5)).unwrap();
        let now = Instant::now();
        let issuance = authority.begin(scope("a")).unwrap();
        let expired = mapping("hiroute_tool_v1_expired", "provider-a");
        authority
            .record_pending(&issuance, expired.clone(), now)
            .unwrap();
        assert!(authority.accept(&issuance, &expired.logical_id, now));
        assert_eq!(
            authority.resolve(
                issuance.scope(),
                &[expired.logical_id],
                now + Duration::from_millis(6)
            ),
            Err(ToolContinuationError::Unavailable)
        );

        let pending = mapping("hiroute_tool_v1_pending", "provider-b");
        authority
            .record_pending(&issuance, pending.clone(), now)
            .unwrap();
        authority.abort_pending(&issuance);
        assert!(!authority.accept(&issuance, &pending.logical_id, now));

        let first = authority.begin(scope("a")).unwrap();
        let second = authority.begin(scope("b")).unwrap();
        let shared = mapping("hiroute_tool_v1_conflict", "provider-c");
        authority
            .record_pending(&first, shared.clone(), now)
            .unwrap();
        assert_eq!(
            authority.record_pending(&second, shared.clone(), now),
            Err(ToolContinuationError::Conflict)
        );
        assert!(!authority.accept(&first, &shared.logical_id, now));
    }
}
