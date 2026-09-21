//! Trusted Tool continuation issuance and downstream acceptance bridge.

use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::ports::{
    ToolContinuationAuthority, ToolContinuationIssuanceV1, ToolContinuationScopeV1,
};
use crate::server::core_runtime::model_ir::{
    ExactProviderPathV1, ModelIrError, ToolIdMapEntryV1, ToolKindV1,
};

use super::ProtocolAdapterError;

const HMAC_BLOCK_BYTES: usize = 64;
const TOOL_ID_PREFIX: &str = "hiroute_tool_v1_";
const TOOL_ID_DIGEST_BYTES: usize = 32;
const TOOL_ID_DIGEST_HEX_BYTES: usize = TOOL_ID_DIGEST_BYTES * 2;
const TOOL_ID_BYTES: usize = TOOL_ID_PREFIX.len() + TOOL_ID_DIGEST_HEX_BYTES;
const TOOL_ID_DOMAIN: &[u8] = b"hiroute/tool-continuation-logical-id/v2";

static PROCESS_TOOL_ID_KEY: OnceLock<[u8; 32]> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct ActiveToolContinuation {
    authority: Arc<dyn ToolContinuationAuthority>,
    issuance: ToolContinuationIssuanceV1,
    provider_states: Option<crate::provider_state::ActiveProviderStates>,
    accepted_count: Arc<AtomicUsize>,
}

impl ActiveToolContinuation {
    pub(crate) fn new(
        authority: Arc<dyn ToolContinuationAuthority>,
        issuance: ToolContinuationIssuanceV1,
    ) -> Self {
        Self {
            authority,
            issuance,
            provider_states: None,
            accepted_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(crate) fn with_provider_states(
        mut self,
        states: crate::provider_state::ActiveProviderStates,
    ) -> Self {
        self.provider_states = Some(states);
        self
    }

    pub(crate) fn scanner(&self) -> AcceptedToolContinuationScanner {
        AcceptedToolContinuationScanner {
            authority: Arc::clone(&self.authority),
            issuance: self.issuance.clone(),
            carry: Vec::with_capacity(TOOL_ID_BYTES.saturating_sub(1)),
            provider_states: self.provider_states.as_ref().map(|states| states.scanner()),
            accepted_count: Arc::clone(&self.accepted_count),
        }
    }

    pub(crate) fn accepted_count(&self) -> usize {
        self.accepted_count.load(Ordering::Acquire)
    }

    /// Returns a request-bound, projection-only capability for off-path
    /// canonical decoding. It can reproduce this issuance's opaque logical
    /// IDs, but carries no authority port and therefore cannot record,
    /// activate, resolve, or abort continuation mappings.
    pub(crate) fn logical_id_projection(
        &self,
    ) -> Result<ToolLogicalIdProjection, ProtocolAdapterError> {
        let key = PROCESS_TOOL_ID_KEY
            .get()
            .copied()
            .ok_or(ModelIrError::ToolContinuationUnavailable)?;
        Ok(ToolLogicalIdProjection {
            key,
            scope: self.issuance.scope().clone(),
        })
    }

    pub(crate) fn abort_pending(&self) {
        self.authority.abort_pending(&self.issuance);
    }
}

/// Non-escalating capability used only to reconstruct the same request-bound
/// logical Tool identity in an asynchronous canonical observation decoder.
/// The process key remains private and is redacted from Debug output.
#[derive(Clone)]
pub(crate) struct ToolLogicalIdProjection {
    key: [u8; 32],
    scope: ToolContinuationScopeV1,
}

impl ToolLogicalIdProjection {
    // Keep every identity component explicit at this authority boundary; an
    // omitted kind/namespace/name would permit cross-tool ID aliasing.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn project(
        &self,
        response_id: &str,
        index: u32,
        native_id: &str,
        kind: ToolKindV1,
        namespace: Option<&str>,
        name: &str,
        owner: &ExactProviderPathV1,
    ) -> Result<String, ProtocolAdapterError> {
        let physical_binding = serde_json::to_vec(owner)
            .map_err(|error| ProtocolAdapterError::Serialization(error.to_string()))?;
        let mut hmac = ToolIdHmac::new(&self.key, TOOL_ID_DOMAIN);
        hmac.update(self.scope.authority_id.as_bytes());
        hmac.update(&self.scope.authority_epoch.to_be_bytes());
        hmac.update(self.scope.grant_id.as_bytes());
        hmac.update(&self.scope.grant_generation.to_be_bytes());
        hmac.update(self.scope.served_model_id.as_bytes());
        let route = serde_json::to_vec(&self.scope.route)
            .map_err(|error| ProtocolAdapterError::Serialization(error.to_string()))?;
        hmac.update(&route);
        hmac.update(response_id.as_bytes());
        hmac.update(&index.to_be_bytes());
        hmac.update(native_id.as_bytes());
        hmac.update(match kind {
            ToolKindV1::Function => b"function",
            ToolKindV1::Custom => b"custom",
        });
        match namespace {
            Some(namespace) => {
                hmac.update(b"namespace-some");
                hmac.update(namespace.as_bytes());
            }
            None => hmac.update(b"namespace-none"),
        }
        hmac.update(name.as_bytes());
        hmac.update(&physical_binding);
        Ok(format!("{TOOL_ID_PREFIX}{:x}", hmac.finish()))
    }

    #[cfg(test)]
    pub(crate) fn for_test(key: [u8; 32], scope: ToolContinuationScopeV1) -> Self {
        Self { key, scope }
    }
}

impl fmt::Debug for ToolLogicalIdProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolLogicalIdProjection")
            .field("scope", &self.scope)
            .field("key", &"[REDACTED]")
            .finish()
    }
}

tokio::task_local! {
    static ACTIVE_TOOL_CONTINUATION: ActiveToolContinuation;
}

pub(crate) async fn with_active_tool_continuation<F>(
    continuation: ActiveToolContinuation,
    future: F,
) -> F::Output
where
    F: Future,
{
    ACTIVE_TOOL_CONTINUATION.scope(continuation, future).await
}

/// Installs one non-exportable process-boot secret. A restart therefore loses
/// both the authority entries and the ability to reproduce old logical IDs.
pub(crate) fn install_process_tool_id_codec() -> Result<(), ()> {
    if PROCESS_TOOL_ID_KEY.get().is_some() {
        return Ok(());
    }
    let mut key = [0_u8; 32];
    getrandom::fill(&mut key).map_err(|_| ())?;
    if key.iter().all(|byte| *byte == 0) {
        return Err(());
    }
    let _ = PROCESS_TOOL_ID_KEY.set(key);
    Ok(())
}

pub(super) fn issue_logical_tool_id(
    response_id: &str,
    index: u32,
    native_id: &str,
    kind: ToolKindV1,
    namespace: Option<&str>,
    name: &str,
    owner: &ExactProviderPathV1,
) -> Result<String, ProtocolAdapterError> {
    let active = ACTIVE_TOOL_CONTINUATION.try_with(Clone::clone).ok();
    let logical_id = match (PROCESS_TOOL_ID_KEY.get(), active.as_ref()) {
        (Some(_), Some(active)) => active.logical_id_projection()?.project(
            response_id,
            index,
            native_id,
            kind,
            namespace,
            name,
            owner,
        )?,
        // Direct adapter golden tests intentionally do not install production
        // process authority and retain their stable historical IDs.
        (None, None) => format!("hiroute_tool_{index}"),
        // A production runtime without both pieces of trusted request context
        // must not fall back to a client-guessable continuation identity.
        (Some(_), None) | (None, Some(_)) => {
            return Err(ModelIrError::ToolContinuationUnavailable.into());
        }
    };
    if let Some(active) = active {
        active
            .authority
            .record_pending(
                &active.issuance,
                ToolIdMapEntryV1 {
                    logical_id: logical_id.clone(),
                    native_id: native_id.into(),
                    kind,
                    name: name.into(),
                    namespace: namespace.map(str::to_owned),
                    owner: owner.clone(),
                },
                Instant::now(),
            )
            .map_err(|_| ModelIrError::ToolContinuationUnavailable)?;
    }
    Ok(logical_id)
}

pub(super) fn record_provider_state(
    value: &serde_json::Value,
    owner: &ExactProviderPathV1,
) -> Result<(), ProtocolAdapterError> {
    ACTIVE_TOOL_CONTINUATION
        .try_with(|active| {
            active
                .provider_states
                .as_ref()
                .map_or(Ok(()), |states| states.record(value, owner))
        })
        .unwrap_or(Ok(()))
        .map_err(Into::into)
}

struct ToolIdHmac {
    inner: Sha256,
    outer_pad: [u8; HMAC_BLOCK_BYTES],
}

impl ToolIdHmac {
    fn new(key: &[u8; 32], domain: &[u8]) -> Self {
        let mut key_block = [0_u8; HMAC_BLOCK_BYTES];
        key_block[..key.len()].copy_from_slice(key);
        let mut inner_pad = [0x36_u8; HMAC_BLOCK_BYTES];
        let mut outer_pad = [0x5c_u8; HMAC_BLOCK_BYTES];
        for ((inner, outer), key) in inner_pad
            .iter_mut()
            .zip(outer_pad.iter_mut())
            .zip(key_block)
        {
            *inner ^= key;
            *outer ^= key;
        }
        let mut inner = Sha256::new();
        inner.update(inner_pad);
        inner.update((domain.len() as u64).to_be_bytes());
        inner.update(domain);
        Self { inner, outer_pad }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.inner.update((bytes.len() as u64).to_be_bytes());
        self.inner.update(bytes);
    }

    fn finish(self) -> impl std::fmt::LowerHex {
        let inner = self.inner.finalize();
        let mut outer = Sha256::new();
        outer.update(self.outer_pad);
        outer.update(inner);
        outer.finalize()
    }
}

/// Incremental O(n) scanner over bytes already accepted by the downstream
/// transport. It recognizes only opaque IDs minted by this process, and the
/// authority independently verifies that each ID is pending for this exact
/// request issuance.
pub(crate) struct AcceptedToolContinuationScanner {
    authority: Arc<dyn ToolContinuationAuthority>,
    issuance: ToolContinuationIssuanceV1,
    carry: Vec<u8>,
    provider_states: Option<crate::provider_state::AcceptedProviderStateScanner>,
    accepted_count: Arc<AtomicUsize>,
}

impl AcceptedToolContinuationScanner {
    pub(crate) fn accept_bytes(&mut self, bytes: &[u8], now: Instant) {
        if let Some(states) = &mut self.provider_states {
            states.accept_bytes(bytes, now);
        }
        let boundary_bytes = bytes.len().min(TOOL_ID_BYTES.saturating_sub(1));
        let mut boundary = Vec::with_capacity(self.carry.len().saturating_add(boundary_bytes));
        boundary.extend_from_slice(&self.carry);
        boundary.extend_from_slice(&bytes[..boundary_bytes]);
        self.accept_complete_ids(&boundary, now);
        self.accept_complete_ids(bytes, now);

        if bytes.len() >= TOOL_ID_BYTES.saturating_sub(1) {
            self.carry.clear();
            self.carry
                .extend_from_slice(&bytes[bytes.len() - (TOOL_ID_BYTES - 1)..]);
        } else {
            boundary.extend_from_slice(&bytes[boundary_bytes..]);
            let retained = TOOL_ID_BYTES.saturating_sub(1).min(boundary.len());
            self.carry.clear();
            self.carry
                .extend_from_slice(&boundary[boundary.len() - retained..]);
        }
    }

    fn accept_complete_ids(&self, bytes: &[u8], now: Instant) {
        let prefix = TOOL_ID_PREFIX.as_bytes();
        let mut offset: usize = 0;
        while offset.saturating_add(TOOL_ID_BYTES) <= bytes.len() {
            let Some(relative) = bytes[offset..]
                .windows(prefix.len())
                .position(|window| window == prefix)
            else {
                break;
            };
            let start = offset + relative;
            let end = start + TOOL_ID_BYTES;
            if end > bytes.len() {
                break;
            }
            let candidate = &bytes[start..end];
            if candidate[prefix.len()..].iter().all(u8::is_ascii_hexdigit)
                && let Ok(logical_id) = std::str::from_utf8(candidate)
                && self.authority.accept(&self.issuance, logical_id, now)
            {
                self.accepted_count.fetch_add(1, Ordering::AcqRel);
            }
            offset = start.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::ports::{InMemoryToolContinuationAuthority, ToolContinuationScopeV1};
    use crate::server::request_plan::IngressProtocol;

    fn scope() -> ToolContinuationScopeV1 {
        ToolContinuationScopeV1 {
            authority_id: "authority".into(),
            authority_epoch: 1,
            grant_id: "grant".into(),
            grant_generation: 1,
            served_model_id: "agent".into(),
            route: hiroute_domain::ModelRequestRouteV2::Plan {
                revision: 2,
                semantic_digest: hiroute_domain::CanonicalDigest::of_bytes(b"plan"),
            },
        }
    }

    fn owner() -> ExactProviderPathV1 {
        ExactProviderPathV1 {
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
        }
    }

    #[test]
    fn logical_ids_are_opaque_stable_and_owner_affine() {
        let key = [7_u8; 32];
        let projection = ToolLogicalIdProjection::for_test(key, scope());
        let first = projection
            .project(
                "response",
                1,
                "native",
                ToolKindV1::Function,
                None,
                "weather",
                &owner(),
            )
            .unwrap();
        let repeated = projection
            .project(
                "response",
                1,
                "native",
                ToolKindV1::Function,
                None,
                "weather",
                &owner(),
            )
            .unwrap();
        let changed = projection
            .project(
                "response",
                1,
                "native-2",
                ToolKindV1::Function,
                None,
                "weather",
                &owner(),
            )
            .unwrap();
        let namespaced = projection
            .project(
                "response",
                1,
                "native",
                ToolKindV1::Function,
                Some("group"),
                "weather",
                &owner(),
            )
            .unwrap();
        let mut other_scope = scope();
        other_scope.grant_id = "other-grant".into();
        let other_scope = ToolLogicalIdProjection::for_test(key, other_scope)
            .project(
                "response",
                1,
                "native",
                ToolKindV1::Function,
                None,
                "weather",
                &owner(),
            )
            .unwrap();
        assert_eq!(first, repeated);
        assert_ne!(first, changed);
        assert_ne!(first, namespaced);
        assert_ne!(first, other_scope);
        assert!(first.starts_with(TOOL_ID_PREFIX));
        assert_eq!(first.len(), TOOL_ID_BYTES);
    }

    #[test]
    fn accepted_scanner_activates_only_complete_fragmented_pending_ids() {
        let authority: Arc<dyn ToolContinuationAuthority> =
            Arc::new(InMemoryToolContinuationAuthority::new(4, Duration::from_secs(1)).unwrap());
        let issuance = authority.begin(scope()).unwrap();
        let logical_id = format!("{TOOL_ID_PREFIX}{}", "a".repeat(TOOL_ID_DIGEST_HEX_BYTES));
        let mapping = ToolIdMapEntryV1 {
            logical_id: logical_id.clone(),
            native_id: "provider-native".into(),
            kind: ToolKindV1::Function,
            name: "weather".into(),
            namespace: None,
            owner: owner(),
        };
        let now = Instant::now();
        authority
            .record_pending(&issuance, mapping.clone(), now)
            .unwrap();
        let active = ActiveToolContinuation::new(Arc::clone(&authority), issuance.clone());
        let mut scanner = active.scanner();
        let wire = format!("data: {{\"call_id\":\"{logical_id}\"}}\n\n");
        for chunk in wire.as_bytes().chunks(3) {
            scanner.accept_bytes(chunk, now);
        }
        assert_eq!(
            authority
                .resolve(issuance.scope(), &[logical_id], now)
                .unwrap(),
            vec![mapping]
        );
    }
}
