// These are fully static projections of the request, provider, filter and
// transport ports. Keeping the phase owner concrete avoids trait objects and
// heap-erased futures on the request path.
#![allow(clippy::type_complexity)]

use super::*;

mod accept;
mod continue_attempt;
mod terminate;

pub(super) enum AttemptCompletionOutcome {
    Continue,
    Completed(SessionReuse),
}

pub(super) struct PublishedAttempt<'a, D, R: GatewayRequestFilterPort, A, W, X: AttemptTransport, E>
{
    pub(super) disposition: Disposition,
    pub(super) session: &'a mut dyn GatewaySession,
    pub(super) request_filters: &'a mut GatewayFilterRequestOwner<R>,
    pub(super) cancellation: &'a CancellationToken,
    pub(super) request_id: RequestId,
    pub(super) binding: &'a mut RequestExecutionBinding,
    pub(super) request_configs: &'a mut ObservedConfigSnapshot<RequestConfigSnapshot>,
    pub(super) driver: &'a mut LogicalRequestDriver,
    pub(super) final_writer: &'a mut RequestFinalWriter,
    pub(super) budget: &'a StreamBudget,
    pub(super) deadline: Instant,
    pub(super) run_filter_callbacks: bool,
    pub(super) decision_session: &'a mut D,
    pub(super) generation: &'a mut AttemptGeneration,
    pub(super) completed_attempts: &'a mut Vec<CompletedAttemptObservation>,
    pub(super) selected: SelectedGatewayAttempt,
    pub(super) routing_facts: RealtimeRoutingFacts,
    pub(super) provider_state: A,
    pub(super) readiness: Option<W>,
    pub(super) provider_facts: ProviderClassificationFacts,
    pub(super) attempt_telemetry: Option<RequestTelemetry>,
    pub(super) downstream_method: &'a Method,
    pub(super) downstream_protocol: HttpProtocol,
    pub(super) exchange: AttemptExchange<X>,
    pub(super) published: PublishedDisposition,
    pub(super) filter_local_reply: Option<LocalReply>,
    pub(super) accepted_sse_handoff: Option<(AcceptedHandoff<E>, u64)>,
    pub(super) sse_end_stream_pending: bool,
    pub(super) accepted_sse_limits: Option<SseLimits>,
    pub(super) sse_framer: Option<SseFramer>,
    pub(super) precommit_sse_mailbox: Option<BudgetedResponseMailbox<u64>>,
    pub(super) accepted_body_plan: BodyPlan,
    pub(super) precommit_event_capacity: usize,
    pub(super) connection_configs: ObservedConfigSnapshot<ConfigScopeSnapshot>,
    pub(super) attempt_configs: ObservedConfigSnapshot<ConfigScopeSnapshot>,
}

impl<S, P, F, T> GatewayCoreLifecycle<S, P, F, T>
where
    P: ProviderRuntimePort,
    S: SelectionPublicationPort<P::RouteRequestContext>,
    F: GatewayFilterManagerPort,
    T: AttemptTransportFactory,
{
    pub(super) async fn complete_published_attempt(
        &self,
        attempt: PublishedAttempt<
            '_,
            S::Session,
            F::RequestFilters,
            P::AttemptState,
            P::Readiness,
            T::Transport,
            P::DecodedSseEvent,
        >,
    ) -> Result<AttemptCompletionOutcome, GatewayExecutionError> {
        match attempt.disposition {
            Disposition::Continue => self.complete_continue(attempt).await,
            Disposition::Terminate => self.complete_terminate(attempt).await,
            Disposition::Accept => self.complete_accept(attempt).await,
        }
    }
}
