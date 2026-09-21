use super::*;

#[derive(Debug)]
pub(crate) struct SelectionState {
    pub(crate) bindings: VecDeque<ResolvedTargetBindingId>,
    pub(crate) selected: usize,
    pub(crate) published: Vec<Disposition>,
    pub(crate) blocked: usize,
    pub(crate) blocked_replacement: Disposition,
    pub(crate) forced_disposition: Option<Disposition>,
    pub(crate) attempt_budget: Option<Duration>,
    pub(crate) attempt_budgets: VecDeque<Duration>,
    pub(crate) generation_overrides: VecDeque<Option<AttemptGeneration>>,
    pub(crate) failures: Vec<AttemptFailureClass>,
    pub(crate) completed_route_decisions: Vec<RouteDecisionId>,
    pub(crate) completed: Vec<CompletedAttemptObservation>,
    pub(crate) selection_history: Vec<Vec<CompletedAttemptObservation>>,
    pub(crate) trace: Vec<SelectionTrace>,
    pub(crate) fail_observation_sink: bool,
    pub(crate) routing_snapshots: VecDeque<RealtimeRoutingFacts>,
    pub(crate) preexchange_completion_trace: Option<Arc<Mutex<Vec<PreexchangeCompletionStep>>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SelectionTrace {
    Select,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreexchangeCompletionStep {
    Select,
    AttemptFilterCleanup,
    ProviderFinalize,
    ObserveCompleted,
}

#[derive(Clone, Debug)]
pub(crate) struct TestSelection {
    pub(crate) state: Arc<Mutex<SelectionState>>,
}

impl TestSelection {
    pub(crate) fn new(bindings: impl IntoIterator<Item = ResolvedTargetBindingId>) -> Self {
        Self {
            state: Arc::new(Mutex::new(SelectionState {
                bindings: bindings.into_iter().collect(),
                selected: 0,
                published: Vec::new(),
                blocked: 0,
                blocked_replacement: Disposition::Terminate,
                forced_disposition: None,
                attempt_budget: None,
                attempt_budgets: VecDeque::new(),
                generation_overrides: VecDeque::new(),
                failures: Vec::new(),
                completed_route_decisions: Vec::new(),
                completed: Vec::new(),
                selection_history: Vec::new(),
                trace: Vec::new(),
                fail_observation_sink: false,
                routing_snapshots: VecDeque::new(),
                preexchange_completion_trace: None,
            })),
        }
    }

    pub(crate) fn with_attempt_budget(self, attempt_budget: Duration) -> Self {
        self.state.lock().expect("selection state").attempt_budget = Some(attempt_budget);
        self
    }

    pub(crate) fn with_attempt_budgets(
        self,
        attempt_budgets: impl IntoIterator<Item = Duration>,
    ) -> Self {
        self.state.lock().expect("selection state").attempt_budgets =
            attempt_budgets.into_iter().collect();
        self
    }

    pub(crate) fn with_generation_overrides(
        self,
        overrides: impl IntoIterator<Item = Option<AttemptGeneration>>,
    ) -> Self {
        self.state
            .lock()
            .expect("selection state")
            .generation_overrides = overrides.into_iter().collect();
        self
    }

    pub(crate) fn force_disposition(&self, disposition: Disposition) {
        self.state
            .lock()
            .expect("selection state")
            .forced_disposition = Some(disposition);
    }

    pub(crate) fn with_blocked_replacement(self, disposition: Disposition) -> Self {
        self.state
            .lock()
            .expect("selection state")
            .blocked_replacement = disposition;
        self
    }

    pub(crate) fn selected(&self) -> usize {
        self.state.lock().expect("selection state").selected
    }

    pub(crate) fn published(&self) -> Vec<Disposition> {
        self.state
            .lock()
            .expect("selection state")
            .published
            .clone()
    }

    pub(crate) fn failures(&self) -> Vec<AttemptFailureClass> {
        self.state.lock().expect("selection state").failures.clone()
    }

    pub(crate) fn blocked(&self) -> usize {
        self.state.lock().expect("selection state").blocked
    }

    pub(crate) fn completed_route_decisions(&self) -> Vec<RouteDecisionId> {
        self.state
            .lock()
            .expect("selection state")
            .completed_route_decisions
            .clone()
    }

    pub(crate) fn completed(&self) -> Vec<CompletedAttemptObservation> {
        self.state
            .lock()
            .expect("selection state")
            .completed
            .clone()
    }

    pub(crate) fn trace(&self) -> Vec<SelectionTrace> {
        self.state.lock().expect("selection state").trace.clone()
    }

    pub(crate) fn selection_history(&self) -> Vec<Vec<CompletedAttemptObservation>> {
        self.state
            .lock()
            .expect("selection state")
            .selection_history
            .clone()
    }

    pub(crate) fn fail_observation_sink(&self) {
        self.state
            .lock()
            .expect("selection state")
            .fail_observation_sink = true;
    }

    pub(crate) fn with_routing_snapshots(
        self,
        snapshots: impl IntoIterator<Item = RealtimeRoutingFacts>,
    ) -> Self {
        self.state
            .lock()
            .expect("selection state")
            .routing_snapshots = snapshots.into_iter().collect();
        self
    }

    pub(crate) fn with_preexchange_completion_trace(
        self,
        trace: Arc<Mutex<Vec<PreexchangeCompletionStep>>>,
    ) -> Self {
        self.state
            .lock()
            .expect("selection state")
            .preexchange_completion_trace = Some(trace);
        self
    }
}

pub(crate) fn candidate_health_snapshot(
    snapshot_id: u64,
    plan: PlanRevision,
    binding: ResolvedTargetBindingId,
    stable_target: &str,
    basis_points: u16,
    observed_at: Instant,
) -> RealtimeRoutingFacts {
    RealtimeRoutingFacts {
        snapshot_id: Some(RoutingFactsSnapshotId(snapshot_id)),
        facts: Arc::from([FreshRoutingFact {
            source: ObservationLabel::new("health-snapshot").unwrap(),
            subject: FactSubject {
                plan_revision: plan,
                binding,
                stable_target: ObservationLabel::new(stable_target).unwrap(),
                provider: None,
                model: None,
                entitlement: None,
                credential: None,
            },
            scope: FactScope::Candidate,
            confidence: FactConfidence::Measured,
            observed_at,
            valid_until: observed_at + Duration::from_secs(30),
            state: RoutingFactState::Known(RealtimeRoutingFact::HealthScore { basis_points }),
        }]),
    }
}

pub(crate) struct TestDecisionSession {
    pub(crate) state: Arc<Mutex<SelectionState>>,
    pub(crate) bindings: VecDeque<ResolvedTargetBindingId>,
    pub(crate) next_attempt_id: u64,
    pub(crate) route_decision_id: RouteDecisionId,
    pub(crate) overall_deadline: Instant,
    pub(crate) attempt_budget: Option<Duration>,
    pub(crate) attempt_budgets: VecDeque<Duration>,
    pub(crate) generation_overrides: VecDeque<Option<AttemptGeneration>>,
}

impl DecisionSessionPort for TestDecisionSession {
    fn route_decision_id(&self) -> RouteDecisionId {
        self.route_decision_id
    }

    fn snapshot_realtime_facts(&mut self, _now: Instant) -> Result<RealtimeRoutingFacts, Arc<str>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| Arc::from("poisoned selector"))?
            .routing_snapshots
            .pop_front()
            .unwrap_or_default())
    }

    fn select_next(
        &mut self,
        request: SelectionRequest<'_>,
    ) -> Result<Option<SelectedGatewayAttempt>, Arc<str>> {
        let Some(binding) = self.bindings.pop_front() else {
            return Ok(None);
        };
        let issued_at = Instant::now();
        let remaining = self
            .overall_deadline
            .saturating_duration_since(issued_at)
            .min(request.remaining_total);
        let allocated = self
            .attempt_budgets
            .pop_front()
            .or(self.attempt_budget)
            .unwrap_or(remaining)
            .min(remaining);
        let deadline = issued_at
            .checked_add(allocated)
            .unwrap_or(self.overall_deadline)
            .min(self.overall_deadline);
        let generation = self
            .generation_overrides
            .pop_front()
            .flatten()
            .unwrap_or(request.generation);
        let selected = SelectedGatewayAttempt {
            request_id: request.request_id,
            attempt_id: hiroute_gateway_core::runtime::attempt::AttemptId(self.next_attempt_id),
            generation,
            binding,
            credential_ref: CredentialRef::new(format!(
                "bootstrap-credential-{}",
                binding.local_id()
            ))
            .map_err(|error| Arc::from(error.to_string()))?,
            route_decision_id: self.route_decision_id,
            budget: AttemptBudgetGrant {
                issued_at,
                allocated,
                deadline,
            },
        };
        self.next_attempt_id = self.next_attempt_id.wrapping_add(1);
        let mut state = self
            .state
            .lock()
            .map_err(|_| Arc::from("poisoned selector"))?;
        state.selected += 1;
        state
            .selection_history
            .push(request.previous_attempts.to_vec());
        state.trace.push(SelectionTrace::Select);
        if let Some(trace) = state.preexchange_completion_trace.as_ref() {
            trace
                .lock()
                .map_err(|_| Arc::from("poisoned pre-exchange completion trace"))?
                .push(PreexchangeCompletionStep::Select);
        }
        Ok(Some(selected))
    }

    fn decide(
        &mut self,
        _selected: &SelectedGatewayAttempt,
        facts: &ProviderClassificationFacts,
        _transport: &hiroute_gateway_core::runtime::attempt::AttemptTransportFacts,
    ) -> Result<Disposition, Arc<str>> {
        let forced = self
            .state
            .lock()
            .map_err(|_| Arc::from("poisoned selector"))?
            .forced_disposition;
        let policy_disposition = match facts.model_event.as_ref().map(ObservationLabel::as_str) {
            Some("local-reply-acceptable") | Some("semantic-response") => Disposition::Accept,
            Some("local-reply-retryable") => Disposition::Continue,
            Some("local-reply-terminal") => Disposition::Terminate,
            _ if facts.retryability == RetryabilityFact::Retryable => Disposition::Continue,
            _ => Disposition::Accept,
        };
        Ok(forced.unwrap_or(policy_disposition))
    }

    fn decide_failure(
        &mut self,
        _selected: &SelectedGatewayAttempt,
        failure: &hiroute_gateway_core::runtime::driver::AttemptFailureFacts,
    ) -> Result<Disposition, Arc<str>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Arc::from("poisoned selector"))?;
        state.failures.push(failure.class);
        Ok(state.forced_disposition.unwrap_or({
            if self.bindings.is_empty() {
                Disposition::Terminate
            } else {
                Disposition::Continue
            }
        }))
    }

    fn replace_blocked_accept(
        &mut self,
        _selected: &SelectedGatewayAttempt,
        _reason: AcceptBlockedReason,
    ) -> Result<Disposition, Arc<str>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Arc::from("poisoned selector"))?;
        state.blocked += 1;
        Ok(state.blocked_replacement)
    }

    fn observe_published(&mut self, disposition: &PublishedDisposition) -> Result<(), Arc<str>> {
        if let Ok(mut state) = self.state.lock() {
            state.published.push(disposition.disposition);
            if state.fail_observation_sink {
                return Err(Arc::from("synthetic publication ledger failure"));
            }
        }
        Ok(())
    }

    fn observe_completed(
        &mut self,
        observation: &hiroute_gateway_core::runtime::driver::CompletedAttemptObservation,
    ) -> Result<(), Arc<str>> {
        if let Ok(mut state) = self.state.lock() {
            state
                .completed_route_decisions
                .push(observation.route_decision_id);
            state.completed.push(observation.clone());
            state.trace.push(SelectionTrace::Complete);
            if let Some(trace) = state.preexchange_completion_trace.as_ref() {
                trace
                    .lock()
                    .map_err(|_| Arc::from("poisoned pre-exchange completion trace"))?
                    .push(PreexchangeCompletionStep::ObserveCompleted);
            }
            if state.fail_observation_sink {
                return Err(Arc::from("synthetic completion ledger failure"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PassthroughRouteContext {
    pub(crate) method: Method,
    pub(crate) path_and_query: Arc<str>,
    pub(crate) body_bytes: usize,
}

impl SelectionPublicationPort<PassthroughRouteContext> for TestSelection {
    type Session = TestDecisionSession;

    fn begin_session(
        &self,
        request: DecisionSessionRequest<PassthroughRouteContext>,
    ) -> Result<Self::Session, Arc<str>> {
        let _ = (
            &request.route_context.method,
            &request.route_context.path_and_query,
            request.route_context.body_bytes,
        );
        let state = self
            .state
            .lock()
            .map_err(|_| Arc::from("poisoned selector"))?;
        let bindings = state.bindings.clone();
        let attempt_budget = state.attempt_budget;
        let attempt_budgets = state.attempt_budgets.clone();
        let generation_overrides = state.generation_overrides.clone();
        drop(state);
        Ok(TestDecisionSession {
            state: Arc::clone(&self.state),
            bindings,
            next_attempt_id: 1,
            route_decision_id: RouteDecisionId(request.request_id.0),
            overall_deadline: request.overall_deadline,
            attempt_budget,
            attempt_budgets,
            generation_overrides,
        })
    }
}
