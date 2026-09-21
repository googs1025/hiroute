use std::sync::Arc;
use std::time::Instant;

use hiroute_gateway_core::core::execution_plan::{CredentialRef, ResolvedTargetBindingId};
use hiroute_gateway_core::runtime::attempt::{
    AcceptBlockedReason, AttemptTransportFacts, Disposition, PublishedDisposition,
};
use hiroute_gateway_core::runtime::driver::{
    AttemptFailureFacts, CompletedAttemptObservation, DecisionCandidateAuthority,
    DecisionSessionPort, DecisionSessionRequest, ObservationLabel, ProviderClassificationFacts,
    RealtimeRoutingFacts, RouteDecisionId, SelectedGatewayAttempt, SelectionPublicationPort,
    SelectionRequest,
};

use crate::runtime::{ProductionDecisionSession, ProductionRouteContext, ProductionSelection};

use super::{RequestObservation, active_request};

/// Decorates the request-owned decision session at core's authoritative
/// publication/completion seam. Delegation always happens first and its exact
/// result is returned unchanged; observation remains best effort.
#[derive(Clone, Default)]
pub struct ObservedProductionSelection {
    inner: ProductionSelection,
}

pub struct ObservedProductionDecisionSession {
    inner: ProductionDecisionSession,
    observation: Option<RequestObservation>,
    candidates: Arc<[CompletionAuthority]>,
}

struct CompletionAuthority {
    binding: ResolvedTargetBindingId,
    stable_target: ObservationLabel,
    credential_refs: Arc<[CredentialRef]>,
}

impl DecisionSessionPort for ObservedProductionDecisionSession {
    fn route_decision_id(&self) -> RouteDecisionId {
        self.inner.route_decision_id()
    }

    fn snapshot_realtime_facts(&mut self, now: Instant) -> Result<RealtimeRoutingFacts, Arc<str>> {
        self.inner.snapshot_realtime_facts(now)
    }

    fn select_next(
        &mut self,
        request: SelectionRequest<'_>,
    ) -> Result<Option<SelectedGatewayAttempt>, Arc<str>> {
        let result = self.inner.select_next(request);
        if let (Some(observation), Ok(Some(selected))) = (&self.observation, &result)
            && let Some(candidate) = self
                .candidates
                .iter()
                .find(|candidate| candidate.binding == selected.binding)
        {
            observation.selected_candidate(candidate.stable_target.as_str());
        }
        result
    }

    fn exhaustion_local_reply(
        &mut self,
        now: Instant,
    ) -> Result<Option<hiroute_gateway_core::core::filter::LocalReply>, Arc<str>> {
        self.inner.exhaustion_local_reply(now)
    }

    fn decide(
        &mut self,
        selected: &SelectedGatewayAttempt,
        facts: &ProviderClassificationFacts,
        transport: &AttemptTransportFacts,
    ) -> Result<Disposition, Arc<str>> {
        self.inner.decide(selected, facts, transport)
    }

    fn decide_failure(
        &mut self,
        selected: &SelectedGatewayAttempt,
        failure: &AttemptFailureFacts,
    ) -> Result<Disposition, Arc<str>> {
        let result = self.inner.decide_failure(selected, failure);
        if selected.attempt_id.0 == 0
            && let (Some(observation), Ok(disposition)) = (&self.observation, &result)
        {
            observation.provisional_candidate_decided(failure, *disposition);
        }
        result
    }

    fn replace_blocked_accept(
        &mut self,
        selected: &SelectedGatewayAttempt,
        reason: AcceptBlockedReason,
    ) -> Result<Disposition, Arc<str>> {
        self.inner.replace_blocked_accept(selected, reason)
    }

    fn observe_published(&mut self, disposition: &PublishedDisposition) -> Result<(), Arc<str>> {
        let result = self.inner.observe_published(disposition);
        if let Some(observation) = &self.observation {
            observation.disposition_published(disposition);
        }
        result
    }

    fn observe_completed(
        &mut self,
        completed: &CompletedAttemptObservation,
    ) -> Result<(), Arc<str>> {
        let result = self.inner.observe_completed(completed);
        if let Some(observation) = &self.observation {
            let stable_binding_id = (completed.route_decision_id == self.inner.route_decision_id())
                .then(|| {
                    self.candidates.iter().find(|candidate| {
                        candidate.binding == completed.binding
                            && candidate
                                .credential_refs
                                .contains(&completed.credential_ref)
                    })
                })
                .flatten()
                .map(|candidate| candidate.stable_target.as_str());
            observation.completed_attempt(completed, stable_binding_id);
        }
        result
    }
}

impl SelectionPublicationPort<ProductionRouteContext> for ObservedProductionSelection {
    type Session = ObservedProductionDecisionSession;

    fn begin_session(
        &self,
        request: DecisionSessionRequest<ProductionRouteContext>,
    ) -> Result<Self::Session, Arc<str>> {
        self.inner
            .begin_session(request)
            .map(|inner| ObservedProductionDecisionSession {
                inner,
                observation: active_request(),
                candidates: Arc::from([]),
            })
    }

    fn begin_authorized_session(
        &self,
        request: DecisionSessionRequest<ProductionRouteContext>,
        candidates: Arc<[DecisionCandidateAuthority]>,
    ) -> Result<Self::Session, Arc<str>> {
        let observation_candidates = candidates
            .iter()
            .map(|candidate| CompletionAuthority {
                binding: candidate.binding,
                stable_target: candidate.stable_target.clone(),
                credential_refs: Arc::clone(&candidate.credential_refs),
            })
            .collect::<Vec<_>>()
            .into();
        self.inner
            .begin_authorized_session(request, candidates)
            .map(|inner| ObservedProductionDecisionSession {
                inner,
                observation: active_request(),
                candidates: observation_candidates,
            })
    }
}
