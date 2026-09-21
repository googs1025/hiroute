use super::*;
use std::any::Any;

#[derive(Debug)]
pub struct DecodedSseToken<D> {
    pub sequence: u64,
    pub decoded: D,
}

/// One classification result. SSE inputs must return exactly one decoded
/// token even when they do not select a terminal disposition; non-SSE inputs
/// must not return one. Core validates this at the ownership boundary.
#[derive(Debug)]
pub struct PrecommitClassification<R, D> {
    pub classified: Option<ClassifiedAttemptResult<R>>,
    pub decoded_sse: Option<DecodedSseToken<D>>,
}

impl<R, D> PrecommitClassification<R, D> {
    pub fn pending() -> Self {
        Self {
            classified: None,
            decoded_sse: None,
        }
    }

    pub fn classified(classified: ClassifiedAttemptResult<R>) -> Self {
        Self {
            classified: Some(classified),
            decoded_sse: None,
        }
    }

    pub fn decoded(
        decoded_sse: DecodedSseToken<D>,
        classified: Option<ClassifiedAttemptResult<R>>,
    ) -> Self {
        Self {
            classified,
            decoded_sse: Some(decoded_sse),
        }
    }
}

/// Accepted-response encoder input. Prefix events already classified before
/// Accept enter as decoded provider IR; only the unclassified transport tail
/// enters as raw protocol events.
#[derive(Debug)]
pub enum ProviderAcceptedEvent<D> {
    Raw(PrecommitEvent),
    DecodedSse {
        sequence: u64,
        decoded: D,
        provenance: SemanticProvenance,
    },
    /// One typed terminal response unit. Attempt-local replies are normalized
    /// by #15 before this event is constructed; provider-selected Terminate
    /// candidates use the same encoder with `body: None` and may source a
    /// bounded body from their readiness owner.
    Terminal {
        body: Option<ChargedBytes>,
        provenance: SemanticProvenance,
        upstream_side_effects: UpstreamSideEffectSnapshot,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UpstreamSideEffectSnapshot {
    pub semantic_upstream_calls: usize,
    pub connection_sub_attempts: usize,
    pub request_fence: CommitFence,
    pub reset_count: usize,
}

impl From<crate::runtime::attempt::AttemptSnapshot> for UpstreamSideEffectSnapshot {
    fn from(snapshot: crate::runtime::attempt::AttemptSnapshot) -> Self {
        Self {
            semantic_upstream_calls: snapshot.semantic_upstream_calls,
            connection_sub_attempts: snapshot.connection_sub_attempts,
            request_fence: snapshot.upstream_request_fence,
            reset_count: snapshot.reset_count,
        }
    }
}

#[derive(Debug)]
pub struct NormalizedAttemptLocalReply<R> {
    pub classified: ClassifiedAttemptResult<R>,
    pub reply: LocalReply,
    /// Echo of the core-owned snapshot passed to normalization. Core validates
    /// it before publication so normalization cannot erase upstream effects.
    pub upstream_side_effects: UpstreamSideEffectSnapshot,
}

pub(super) enum AttemptDecision<R> {
    Provider(Box<ClassifiedAttemptResult<R>>),
    FilterLocalReply(LocalReply),
}

pub struct LogicalRequestContext<'a> {
    pub budget: &'a StreamBudget,
    pub plan: &'a BodyPlan,
    pub hard_total_limit: usize,
    pub chunk_capacity: usize,
    /// Present only for an authority-bound request. Providers may retain this
    /// immutable, non-secret closure with the logical request owner.
    pub frozen_candidates: Option<Arc<[DecisionCandidateAuthority]>>,
    /// Optional request-owned provider seed supplied by a bound authority.
    /// Core never interprets it; the provider may downcast it without using a
    /// global registry or runtime-specific session identifier.
    pub provider_context: Option<Arc<dyn Any + Send + Sync>>,
}

pub struct AttemptMaterializationContext<'a> {
    pub(super) binding: &'a AttemptExecutionBinding,
    pub(super) selected: &'a SelectedGatewayAttempt,
    pub(super) configs: PinnedConfigContext<'a>,
    pub budget: &'a StreamBudget,
    pub leases: &'a RequestLeaseBook,
    pub write_quantum: usize,
    pub cancellation: &'a CancellationToken,
}

impl AttemptMaterializationContext<'_> {
    pub fn plan(&self) -> &CompiledAttemptPlan {
        self.binding.plan()
    }

    pub fn config(&self, id: ConfigCellId) -> Option<&ImmutableConfig> {
        self.configs.value(id)
    }

    pub fn config_generations(
        &self,
    ) -> impl Iterator<Item = (ConfigCellId, crate::core::execution_plan::ConfigGeneration)> + '_
    {
        self.configs.generations()
    }

    pub fn credential_ref(&self) -> &CredentialRef {
        &self.selected.credential_ref
    }

    pub fn binding_id(&self) -> ResolvedTargetBindingId {
        self.selected.binding
    }

    pub fn route_decision_id(&self) -> RouteDecisionId {
        self.selected.route_decision_id
    }

    pub fn attempt_id(&self) -> AttemptId {
        self.selected.attempt_id
    }

    pub fn attempt_deadline(&self) -> Instant {
        self.selected.budget.deadline
    }

    pub fn cancellation_token(&self) -> &CancellationToken {
        self.cancellation
    }
}

pub struct PinnedConfigContext<'a> {
    pub(super) ids: &'a [ConfigCellId],
    pub(super) connection: &'a ConfigScopeSnapshot,
    pub(super) request: &'a RequestConfigSnapshot,
    pub(super) attempt: &'a ConfigScopeSnapshot,
    pub(super) phase: &'a ConfigScopeSnapshot,
}

impl PinnedConfigContext<'_> {
    pub fn value(&self, id: ConfigCellId) -> Option<&ImmutableConfig> {
        self.connection
            .value(id)
            .or_else(|| self.request.value(id))
            .or_else(|| self.attempt.value(id))
            .or_else(|| self.phase.value(id))
    }

    pub fn generations(
        &self,
    ) -> impl Iterator<Item = (ConfigCellId, crate::core::execution_plan::ConfigGeneration)> + '_
    {
        self.ids
            .iter()
            .copied()
            .filter_map(|id| self.value(id).map(|value| (id, value.generation)))
    }
}

/// Consuming #15 integration boundary. Provider bytes enter gateway core as
/// opaque ownership values; only the provider port parses, materializes,
/// classifies, and encodes them.
#[async_trait]
pub trait ProviderRuntimePort: Send + Sync + 'static {
    type LogicalRequest: Send;
    type RouteRequestContext: Send;
    type AttemptState: Send;
    type Readiness: Send;
    type DecodedSseEvent: Send;

    async fn begin_request(
        &self,
        head: GatewayRequestHead,
        context: LogicalRequestContext<'_>,
    ) -> Result<Self::LogicalRequest, Arc<str>>;

    /// Incremental logical-body consumer. The provider may parse and drop a
    /// PassThrough frame immediately or retain the charged owner for replay;
    /// core no longer aggregates every mode before invoking #15.
    async fn consume_request_body(
        &self,
        logical: &mut Self::LogicalRequest,
        frame: LogicalRequestBodyFrame,
    ) -> Result<(), Arc<str>>;

    /// Freezes the provider-neutral selection context after logical body EOS.
    /// The returned owned value is moved into one linear decision session; the
    /// provider's request owner remains available only for materialization and
    /// terminal release.
    fn finalize_route_request_context(
        &self,
        logical: &mut Self::LogicalRequest,
    ) -> Result<Self::RouteRequestContext, Arc<str>>;

    async fn materialize_attempt(
        &self,
        logical: &mut Self::LogicalRequest,
        context: AttemptMaterializationContext<'_>,
    ) -> Result<(PreparedAttemptHttpRequest, Self::AttemptState), Arc<str>>;

    /// Returns an executable exact target resolved during materialization.
    /// `None` preserves the immutable compiled target (the common case for
    /// already-numeric endpoints). Implementations may not select a different
    /// binding here; core still validates and owns the Attempt binding.
    fn resolved_transport_target(
        &self,
        _state: &Self::AttemptState,
        _binding: &AttemptExecutionBinding,
    ) -> Result<Option<TransportTarget>, Arc<str>> {
        Ok(None)
    }

    /// Converts a provider-private materialization error into product-neutral
    /// facts. Raw credential, DNS, and encoder details never cross into the
    /// decision session.
    fn classify_materialization_failure(&self, error: &Arc<str>) -> AttemptMaterializationFailure;

    fn classify_precommit(
        &self,
        state: &mut Self::AttemptState,
        event: PrecommitEvent,
        pinned_configs: &PinnedConfigContext<'_>,
        event_configs: &ConfigEventSnapshot,
    ) -> Result<PrecommitClassification<Self::Readiness, Self::DecodedSseEvent>, Arc<str>>;

    /// Confirms provider-private runtime state after semantic classification
    /// but before the decision can publish any downstream commit. A failed
    /// confirmation is fail-closed and therefore cannot expose an accepted
    /// response or start another attempt behind an unconfirmed write.
    async fn confirm_precommit(
        &self,
        _state: &mut Self::AttemptState,
        _facts: &ProviderClassificationFacts,
        _deadline: Instant,
        _cancellation: &CancellationToken,
    ) -> Result<(), Arc<str>> {
        Ok(())
    }

    /// Confirms provider-private RuntimeState for a core-classified mechanical
    /// failure before its disposition is selected. The hook shares the exact
    /// attempt deadline/cancellation and may enrich only bounded provider
    /// facts. A failed confirmation is fail-closed.
    async fn confirm_attempt_failure(
        &self,
        _state: &mut Self::AttemptState,
        failure: &AttemptFailureFacts,
        _deadline: Instant,
        _cancellation: &CancellationToken,
    ) -> Result<Option<ProviderClassificationFacts>, Arc<str>> {
        Ok(failure.provider.clone())
    }

    /// The provider normalizes an attempt-scope filter reply into structured
    /// classification facts. The request-owned decision session alone chooses
    /// the resulting disposition.
    fn normalize_attempt_local_reply(
        &self,
        state: &mut Self::AttemptState,
        reply: LocalReply,
        upstream_side_effects: UpstreamSideEffectSnapshot,
    ) -> Result<NormalizedAttemptLocalReply<Self::Readiness>, Arc<str>>;

    /// Produces the final provider-owned facts at true attempt completion.
    /// `readiness` is absent only for failures before a classified response.
    /// This callback is observational: once a disposition is published, an
    /// adapter error here is ignored by core and cannot rewrite the response.
    fn finalize_attempt_facts(
        &self,
        state: &mut Self::AttemptState,
        readiness: Option<&mut Self::Readiness>,
        published_facts: Option<&ProviderClassificationFacts>,
        completion: &ProviderAttemptCompletion,
    ) -> Result<ProviderClassificationFacts, Arc<str>>;

    fn accepted_response_head(
        &self,
        readiness: &mut Self::Readiness,
        published: &PublishedDisposition,
        accepted: &AcceptedResponseExecutionBinding,
        configs: &PinnedConfigContext<'_>,
    ) -> Result<GatewayResponseHead, Arc<str>>;

    /// Transfers a classified non-SSE prefix event retained by provider
    /// readiness into the accepted owner before that owner polls the transport
    /// tail. The default covers providers that decide at response head.
    fn take_accepted_prefix(
        &self,
        _readiness: &mut Self::Readiness,
    ) -> Result<Option<ProviderAcceptedEvent<Self::DecodedSseEvent>>, Arc<str>> {
        Ok(None)
    }

    fn encode_accepted_event(
        &self,
        readiness: &mut Self::Readiness,
        event: ProviderAcceptedEvent<Self::DecodedSseEvent>,
        published: &PublishedDisposition,
        accepted: &AcceptedResponseExecutionBinding,
        pinned_configs: &PinnedConfigContext<'_>,
        event_configs: &ConfigEventSnapshot,
    ) -> Result<Option<AcceptedBodyFrame>, Arc<str>>;

    /// Drops #15-owned raw/model request backing at terminal publication.
    /// Gateway core checks lease-zero immediately afterwards; accepted SSE
    /// work therefore cannot retain Prompt through an unrelated attempt lease.
    fn release_terminal_request(
        &self,
        logical: Self::LogicalRequest,
        published: &PublishedDisposition,
    ) -> Result<(), Arc<str>>;
}

pub(super) fn finalize_preexchange_provider_facts<P: ProviderRuntimePort>(
    provider: &P,
    state: Option<&mut P::AttemptState>,
    published_facts: Option<&ProviderClassificationFacts>,
    completion: &ProviderAttemptCompletion,
) -> Option<ProviderClassificationFacts> {
    let Some(state) = state else {
        return published_facts.cloned();
    };
    provider
        .finalize_attempt_facts(state, None, published_facts, completion)
        .ok()
        .filter(|facts| validate_provider_facts(facts).is_ok())
        .or_else(|| published_facts.cloned())
}

pub(super) fn validate_provider_facts(
    facts: &ProviderClassificationFacts,
) -> Result<(), GatewayExecutionError> {
    let Some(usage) = facts.usage else {
        return Ok(());
    };
    for dimension in [
        usage.input,
        usage.output,
        usage.billable,
        usage.cache_read,
        usage.cache_write,
        usage.reasoning,
    ] {
        match (dimension.units, dimension.provenance) {
            (None, UsageProvenance::Unknown)
            | (Some(_), UsageProvenance::Reported | UsageProvenance::Estimated) => {}
            _ => return Err(GatewayExecutionError::InvalidProviderFacts),
        }
    }
    Ok(())
}
