use super::*;

pub(super) struct SelectionState<'a, D> {
    pub(super) request_id: RequestId,
    pub(super) generation: AttemptGeneration,
    pub(super) route_binding: ResolvedTargetBindingId,
    pub(super) deadline: Instant,
    pub(super) max_attempts: u32,
    pub(super) candidate_bindings: &'a [ResolvedTargetBindingId],
    pub(super) binding: &'a RequestExecutionBinding,
    pub(super) decision_session: &'a mut D,
    pub(super) completed_attempts: &'a [CompletedAttemptObservation],
    pub(super) routing_snapshots: &'a mut HashMap<RoutingFactsSnapshotId, Arc<[FreshRoutingFact]>>,
}

impl<D> SelectionState<'_, D>
where
    D: DecisionSessionPort,
{
    pub(super) fn select(
        &mut self,
    ) -> Result<Option<(SelectedGatewayAttempt, RealtimeRoutingFacts)>, GatewayExecutionError> {
        let remaining_attempts = self
            .max_attempts
            .saturating_sub(self.completed_attempts.len() as u32);
        let selection_now = Instant::now();
        let remaining_total = self
            .deadline
            .checked_duration_since(selection_now)
            .ok_or(GatewayExecutionError::Deadline)?;
        if remaining_attempts == 0 {
            return Ok(None);
        }

        let routing_facts = self
            .decision_session
            .snapshot_realtime_facts(selection_now)
            .map_err(GatewayExecutionError::Selection)?;
        validate_realtime_routing_facts(
            self.binding,
            self.candidate_bindings,
            &routing_facts,
            selection_now,
        )?;
        if let Some(snapshot_id) = routing_facts.snapshot_id {
            match self.routing_snapshots.get(&snapshot_id) {
                Some(previous) if previous.as_ref() != routing_facts.facts.as_ref() => {
                    return Err(GatewayExecutionError::InvalidRoutingFacts);
                }
                Some(_) => {}
                None => {
                    self.routing_snapshots
                        .insert(snapshot_id, Arc::clone(&routing_facts.facts));
                }
            }
        }
        let selected = self
            .decision_session
            .select_next(SelectionRequest {
                request_id: self.request_id,
                generation: self.generation,
                route_binding: self.route_binding,
                remaining_total,
                remaining_attempts,
                realtime_facts: &routing_facts,
                previous_attempts: self.completed_attempts,
            })
            .map_err(GatewayExecutionError::Selection)?;
        Ok(selected.map(|selected| (selected, routing_facts)))
    }
}

pub(super) struct SelectedAttemptSetup<'a> {
    pub(super) request_id: RequestId,
    pub(super) generation: AttemptGeneration,
    pub(super) binding: &'a RequestExecutionBinding,
    pub(super) request_configs: &'a RequestConfigSnapshot,
    pub(super) driver: &'a mut LogicalRequestDriver,
    pub(super) run_filter_callbacks: bool,
    pub(super) route_decision_id: RouteDecisionId,
    pub(super) deadline: Instant,
    pub(super) request_telemetry: Option<&'a RequestTelemetry>,
}

pub(super) struct SelectedAttemptRun {
    pub(super) selected: SelectedGatewayAttempt,
    pub(super) routing_facts: RealtimeRoutingFacts,
    pub(super) attempt_binding: AttemptExecutionBinding,
    pub(super) attempt_deadline: Instant,
    pub(super) has_attempt_request_filters: bool,
    pub(super) has_attempt_response_filters: bool,
    pub(super) connection_configs: ObservedConfigSnapshot<ConfigScopeSnapshot>,
    pub(super) attempt_configs: ObservedConfigSnapshot<ConfigScopeSnapshot>,
    pub(super) materialize_phase_configs: ObservedConfigSnapshot<ConfigScopeSnapshot>,
    pub(super) attempt_telemetry: Option<RequestTelemetry>,
}

impl SelectedAttemptSetup<'_> {
    pub(super) fn bind(
        self,
        selected: SelectedGatewayAttempt,
        routing_facts: RealtimeRoutingFacts,
    ) -> Result<SelectedAttemptRun, GatewayExecutionError> {
        validate_selection(
            self.request_id,
            self.generation,
            &selected,
            self.binding.plan_revision(),
            self.route_decision_id,
            self.deadline,
        )?;
        let attempt_binding = self.binding.resolve_attempt(selected.binding)?;
        let plan = attempt_binding.plan();
        if !plan.credential_refs.contains(&selected.credential_ref) {
            return Err(GatewayExecutionError::InvalidSelection);
        }
        let attempt_deadline = selected.budget.deadline;
        let has_attempt_request_filters =
            self.run_filter_callbacks && !plan.attempt_request_filters.is_empty();
        let has_attempt_response_filters =
            self.run_filter_callbacks && !plan.attempt_response_filters.is_empty();
        let connection_configs = attempt_binding.acquire_connection_configs()?;
        let attempt_configs = attempt_binding.acquire_attempt_configs()?;
        let materialize_phase_configs = attempt_binding.acquire_phase_configs()?;
        let attempt_telemetry = self.request_telemetry.map(|telemetry| {
            let config_generations: Arc<[(u64, crate::core::execution_plan::ConfigGeneration)]> =
                PinnedConfigContext {
                    ids: &plan.config_cell_ids,
                    connection: &connection_configs,
                    request: self.request_configs,
                    attempt: &attempt_configs,
                    phase: &materialize_phase_configs,
                }
                .generations()
                .map(|(id, generation)| (id.0, generation))
                .collect::<Vec<_>>()
                .into();
            telemetry.with_attempt(
                selected.route_decision_id.0,
                plan.stable_target_key.clone(),
                selected.binding.local_id(),
                selected.attempt_id,
                selected.generation,
                config_generations,
            )
        });
        let connection_observation = attempt_telemetry.as_ref().map(|telemetry| {
            ConfigLeaseObservation::acquire(
                telemetry,
                ConfigAcquireScope::Connection,
                connection_configs.generations(),
            )
        });
        let attempt_observation = attempt_telemetry.as_ref().map(|telemetry| {
            ConfigLeaseObservation::acquire(
                telemetry,
                ConfigAcquireScope::Attempt,
                attempt_configs.generations(),
            )
        });
        let materialize_phase_observation = attempt_telemetry.as_ref().map(|telemetry| {
            ConfigLeaseObservation::acquire(
                telemetry,
                ConfigAcquireScope::Phase,
                materialize_phase_configs.generations(),
            )
        });
        let connection_configs =
            ObservedConfigSnapshot::new(connection_configs, connection_observation);
        let attempt_configs = ObservedConfigSnapshot::new(attempt_configs, attempt_observation);
        let materialize_phase_configs =
            ObservedConfigSnapshot::new(materialize_phase_configs, materialize_phase_observation);
        // AttemptId(0) is a core-recognized provisional candidate. Product
        // selection may use it while credential/DNS/protocol materialization
        // is still fallible; core promotes it only after that work succeeds.
        // Existing selection ports that allocate a real ID retain the prior
        // lifecycle behavior.
        if selected.attempt_id.0 != 0 {
            self.driver.begin_attempt()?;
        }
        Ok(SelectedAttemptRun {
            selected,
            routing_facts,
            attempt_binding,
            attempt_deadline,
            has_attempt_request_filters,
            has_attempt_response_filters,
            connection_configs,
            attempt_configs,
            materialize_phase_configs,
            attempt_telemetry,
        })
    }
}
