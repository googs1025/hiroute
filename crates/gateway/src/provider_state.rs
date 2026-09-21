//! Process-local provenance for native ciphertext actually accepted downstream.
//! The store retains only a digest and exact owner; it never rewrites ciphertext.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ports::ToolContinuationScopeV1;
use crate::server::core_runtime::model_ir::{ExactProviderPathV1, ModelIrError};

const CAPACITY: usize = 4096;
const IDLE_TTL: Duration = Duration::from_secs(3600);
const MAX_PATTERN: usize = 256 * 1024;
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
type Key = (ToolContinuationScopeV1, [u8; 32]);

#[derive(Default)]
pub(crate) struct ProviderStateStore(Mutex<BTreeMap<Key, Entry>>);

struct Entry {
    owner: Option<ExactProviderPathV1>,
    expires: Instant,
}

impl ProviderStateStore {
    fn accept(&self, scope: &ToolContinuationScopeV1, pending: &Pending, now: Instant) {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, entry| entry.expires > now);
        let key = (scope.clone(), pending.digest);
        if let Some(entry) = entries.get_mut(&key) {
            if entry.owner.as_ref() != Some(&pending.owner) {
                entry.owner = None;
            }
            return;
        }
        if entries.len() >= CAPACITY
            && let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires)
                .map(|(key, _)| key.clone())
        {
            entries.remove(&oldest);
        }
        entries.insert(
            key,
            Entry {
                owner: Some(pending.owner.clone()),
                expires: now + IDLE_TTL,
            },
        );
    }

    pub(crate) fn resolve(
        &self,
        scope: &ToolContinuationScopeV1,
        document: &Value,
        now: Instant,
    ) -> Result<Option<ExactProviderPathV1>, ModelIrError> {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, entry| entry.expires > now);
        let mut owner = None;
        let mut resolved_keys = Vec::new();
        if let Some(input) = document.get("input").and_then(Value::as_array) {
            for item in input.iter().filter(|item| item["type"] == "reasoning") {
                let state = match item.get("encrypted_content") {
                    None | Some(Value::Null) => continue,
                    Some(Value::String(state)) if state.is_empty() => continue,
                    Some(Value::String(state)) => state,
                    Some(_) => return Err(ModelIrError::InvalidField("encrypted_content")),
                };
                let key = (scope.clone(), digest(state));
                let entry = entries
                    .get(&key)
                    .ok_or(ModelIrError::ProviderStateOwnershipRequired)?;
                let next = entry
                    .owner
                    .as_ref()
                    .ok_or(ModelIrError::ProviderStateNotPortable)?;
                if owner.as_ref().is_some_and(|current| current != next) {
                    return Err(ModelIrError::ProviderStateNotPortable);
                }
                owner = Some(next.clone());
                resolved_keys.push(key);
            }
        }
        // Renew only after the entire replay passes ownership validation.
        for key in resolved_keys {
            entries
                .get_mut(&key)
                .expect("resolved entry remains locked")
                .expires = now + IDLE_TTL;
        }
        Ok(owner)
    }
}

fn digest(state: &str) -> [u8; 32] {
    Sha256::digest(state.as_bytes()).into()
}

struct Pending {
    digest: [u8; 32],
    owner: ExactProviderPathV1,
    pattern: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct ActiveProviderStates {
    store: Arc<ProviderStateStore>,
    scope: ToolContinuationScopeV1,
    pending: Arc<Mutex<Vec<Pending>>>,
}

impl ActiveProviderStates {
    pub(crate) fn new(store: Arc<ProviderStateStore>, scope: ToolContinuationScopeV1) -> Self {
        Self {
            store,
            scope,
            pending: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn record(
        &self,
        state: &Value,
        owner: &ExactProviderPathV1,
    ) -> Result<(), ModelIrError> {
        let state = state
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or(ModelIrError::InvalidField("encrypted_content"))?;
        // This exact key/value fragment is written by the native Responses renderer.
        // Quotes inside user text are escaped and cannot activate this fragment.
        let pattern = format!(
            "\"encrypted_content\":{}",
            serde_json::to_string(state)
                .map_err(|_| ModelIrError::InvalidField("encrypted_content"))?
        )
        .into_bytes();
        if pattern.len() > MAX_PATTERN {
            return Err(ModelIrError::BufferLimit(MAX_PATTERN));
        }
        let digest = digest(state);
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending
            .iter()
            .any(|p| p.digest == digest && &p.owner == owner)
        {
            return Ok(());
        }
        if pending.len() >= 128
            || pending.iter().map(|p| p.pattern.len()).sum::<usize>() + pattern.len()
                > MAX_PENDING_BYTES
        {
            return Err(ModelIrError::BufferLimit(MAX_PENDING_BYTES));
        }
        pending.push(Pending {
            digest,
            owner: owner.clone(),
            pattern,
        });
        Ok(())
    }

    pub(crate) fn scanner(&self) -> AcceptedProviderStateScanner {
        AcceptedProviderStateScanner {
            active: self.clone(),
            carry: Vec::new(),
        }
    }
}

pub(crate) struct AcceptedProviderStateScanner {
    active: ActiveProviderStates,
    carry: Vec<u8>,
}

impl AcceptedProviderStateScanner {
    pub(crate) fn accept_bytes(&mut self, bytes: &[u8], now: Instant) {
        let mut pending = self
            .active
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let keep = pending
            .iter()
            .map(|p| p.pattern.len().saturating_sub(1))
            .max()
            .unwrap_or(0);
        let mut boundary = self.carry.clone();
        boundary.extend_from_slice(&bytes[..bytes.len().min(keep)]);
        pending.retain(|p| {
            let found = bytes.windows(p.pattern.len()).any(|w| w == p.pattern)
                || boundary.windows(p.pattern.len()).any(|w| w == p.pattern);
            if found {
                self.active.store.accept(&self.active.scope, p, now);
            }
            !found
        });
        // Keep only enough bytes to recognize an accepted, split key/value unit.
        if bytes.len() >= keep {
            self.carry = bytes[bytes.len() - keep..].to_vec();
        } else {
            let start = boundary.len().saturating_sub(keep);
            self.carry = boundary[start..].to_vec();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::core_runtime::profiles::{CandidateProtocolProfile, fixed_reasoning};
    use crate::server::request_plan::IngressProtocol;
    use serde_json::json;

    fn scope() -> ToolContinuationScopeV1 {
        ToolContinuationScopeV1 {
            authority_id: "a".into(),
            authority_epoch: 1,
            grant_id: "g".into(),
            grant_generation: 1,
            served_model_id: "route".into(),
            route: hiroute_domain::ModelRequestRouteV2::Plan {
                revision: 1,
                semantic_digest: hiroute_domain::CanonicalDigest::of_bytes(b"route"),
            },
        }
    }

    fn owner(model: &str) -> ExactProviderPathV1 {
        CandidateProtocolProfile::exact_portable_path(
            IngressProtocol::Responses,
            IngressProtocol::Responses,
            model,
            fixed_reasoning("fixed"),
        )
        .exact_provider_path()
        .unwrap()
    }

    fn request(state: &str) -> Value {
        json!({"input":[{"type":"reasoning","encrypted_content":state}]})
    }

    #[test]
    fn only_accepted_ciphertext_resolves_and_scope_expiry_are_enforced() {
        let store = Arc::new(ProviderStateStore::default());
        let active = ActiveProviderStates::new(store.clone(), scope());
        let now = Instant::now();
        let state = "fixture-\\\"ciphertext";
        active.record(&json!(state), &owner("luna")).unwrap();
        assert!(store.resolve(&scope(), &request(state), now).is_err());
        let mut scanner = active.scanner();
        // A user text containing an escaped key/value must not activate anything.
        let fake = json!({"text":format!("\"encrypted_content\":{}", json!(state))}).to_string();
        scanner.accept_bytes(fake.as_bytes(), now);
        assert!(store.resolve(&scope(), &request(state), now).is_err());
        let wire = json!({"item":{"encrypted_content":state}}).to_string();
        for byte in wire.as_bytes().chunks(1) {
            scanner.accept_bytes(byte, now);
        }
        assert_eq!(
            store.resolve(&scope(), &request(state), now).unwrap(),
            Some(owner("luna"))
        );
        let mut foreign = scope();
        foreign.grant_generation += 1;
        assert!(store.resolve(&foreign, &request(state), now).is_err());
        assert!(store.resolve(&scope(), &request("altered"), now).is_err());
        assert!(
            store
                .resolve(&scope(), &request(state), now + IDLE_TTL)
                .is_err()
        );
    }

    #[test]
    fn mixed_owners_and_conflicting_producers_do_not_gain_authority() {
        let store = Arc::new(ProviderStateStore::default());
        let now = Instant::now();
        for (value, model) in [("one", "luna"), ("two", "terra")] {
            let active = ActiveProviderStates::new(store.clone(), scope());
            active.record(&json!(value), &owner(model)).unwrap();
            active.scanner().accept_bytes(
                json!({"encrypted_content":value}).to_string().as_bytes(),
                now,
            );
        }
        let mixed = json!({"input":[{"type":"reasoning","encrypted_content":"one"},{"type":"reasoning","encrypted_content":"two"}]});
        assert_eq!(
            store.resolve(&scope(), &mixed, now),
            Err(ModelIrError::ProviderStateNotPortable)
        );
        let active = ActiveProviderStates::new(store.clone(), scope());
        active.record(&json!("one"), &owner("terra")).unwrap();
        active
            .scanner()
            .accept_bytes(br#"{"encrypted_content":"one"}"#, now);
        assert_eq!(
            store.resolve(&scope(), &request("one"), now),
            Err(ModelIrError::ProviderStateNotPortable)
        );
        assert!(
            ActiveProviderStates::new(store, scope())
                .record(&json!("x".repeat(MAX_PATTERN)), &owner("luna"))
                .is_err()
        );
    }

    #[test]
    fn active_replay_renews_idle_deadline_but_eventually_expires() {
        let store = Arc::new(ProviderStateStore::default());
        let active = ActiveProviderStates::new(store.clone(), scope());
        let now = Instant::now();
        active.record(&json!("state"), &owner("luna")).unwrap();
        active
            .scanner()
            .accept_bytes(br#"{"encrypted_content":"state"}"#, now);
        for minutes in [30, 60, 90, 120] {
            assert_eq!(
                store
                    .resolve(
                        &scope(),
                        &request("state"),
                        now + Duration::from_secs(minutes * 60)
                    )
                    .unwrap(),
                Some(owner("luna")),
            );
        }
        assert_eq!(
            store.resolve(
                &scope(),
                &request("state"),
                now + Duration::from_secs(180 * 60)
            ),
            Err(ModelIrError::ProviderStateOwnershipRequired),
        );
    }

    #[test]
    fn invalid_replay_never_renews_a_valid_prefix() {
        for invalid in [
            json!({"type":"reasoning","encrypted_content":"unknown"}),
            json!({"type":"reasoning","encrypted_content":"terra-state"}),
            json!({"type":"reasoning","encrypted_content":42}),
        ] {
            let store = Arc::new(ProviderStateStore::default());
            let now = Instant::now();
            for (state, model) in [("state", "luna"), ("terra-state", "terra")] {
                let active = ActiveProviderStates::new(store.clone(), scope());
                active.record(&json!(state), &owner(model)).unwrap();
                active.scanner().accept_bytes(
                    json!({"encrypted_content":state}).to_string().as_bytes(),
                    now,
                );
            }
            let replay =
                json!({"input":[{"type":"reasoning","encrypted_content":"state"}, invalid]});
            assert!(
                store
                    .resolve(&scope(), &replay, now + Duration::from_secs(1800))
                    .is_err()
            );
            assert_eq!(
                store.resolve(&scope(), &request("state"), now + IDLE_TTL),
                Err(ModelIrError::ProviderStateOwnershipRequired)
            );
        }
    }

    #[test]
    fn summary_only_reasoning_does_not_require_provider_state_authority() {
        let store = ProviderStateStore::default();
        let now = Instant::now();
        for item in [
            json!({"type":"reasoning","summary":[]}),
            json!({"type":"reasoning","summary":[],"encrypted_content":null}),
            json!({"type":"reasoning","summary":[],"encrypted_content":""}),
        ] {
            let document = json!({"input":[item]});
            assert_eq!(store.resolve(&scope(), &document, now).unwrap(), None);
        }
        assert_eq!(
            store.resolve(
                &scope(),
                &json!({"input":[{"type":"reasoning","encrypted_content":7}]}),
                now
            ),
            Err(ModelIrError::InvalidField("encrypted_content"))
        );
    }
}
