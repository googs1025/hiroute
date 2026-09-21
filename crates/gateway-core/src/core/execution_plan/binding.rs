use super::*;

/// The request-owned view. None of these segment Arcs points back to the
/// publication root, so a long stream cannot pin unrelated publication data.
#[derive(Debug)]
pub struct RequestExecutionBinding {
    plan_revision: PlanRevision,
    ingress_plan: Option<CompiledIngressPlanHandle>,
    attempt_plan_index: Option<AttemptPlanIndexHandle>,
    route_attempt_plans: Option<HashMap<ResolvedTargetBindingId, Arc<CompiledAttemptPlan>>>,
    candidate_bindings: Option<Arc<[ResolvedTargetBindingId]>>,
    logical_request_plan: Option<CompiledLogicalRequestPlanHandle>,
    accepted_response_plan: Option<CompiledAcceptedResponsePlanHandle>,
    overall_request_timeout: Option<Duration>,
    max_attempts: Option<u32>,
    config_catalog: Option<ConfigCellsHandle>,
    config_handles: Option<HashMap<ConfigCellId, ConfigCellHandle>>,
    request_configs: Option<RequestConfigSnapshot>,
}

impl RequestExecutionBinding {
    pub(crate) fn new(
        plan_revision: PlanRevision,
        ingress_plan: CompiledIngressPlanHandle,
        attempt_plan_index: AttemptPlanIndexHandle,
        config_handles: ConfigCellsHandle,
    ) -> Result<Self, PlanError> {
        Ok(Self {
            plan_revision,
            ingress_plan: Some(ingress_plan),
            attempt_plan_index: Some(attempt_plan_index),
            route_attempt_plans: None,
            candidate_bindings: None,
            logical_request_plan: None,
            accepted_response_plan: None,
            overall_request_timeout: None,
            max_attempts: None,
            config_catalog: Some(config_handles),
            config_handles: None,
            request_configs: None,
        })
    }

    pub fn plan_revision(&self) -> PlanRevision {
        self.plan_revision
    }

    pub fn ingress_plan(&self) -> Result<&CompiledIngressPlan, PlanError> {
        self.ingress_plan
            .as_deref()
            .ok_or(PlanError::ExecutionSegmentReleased("logical ingress"))
    }

    pub fn release_logical_plan(&mut self) {
        self.ingress_plan.take();
    }

    /// Narrow the publication segments to one matched route and atomically
    /// acquire its complete RequestPinned candidate/accepted closure.
    pub fn bind_route(&mut self, route: &CompiledRoute) -> Result<(), PlanError> {
        let request = &route.request_plan;
        if route.binding.plan_revision() != self.plan_revision
            || request.candidate_bindings.is_empty()
            || !request.candidate_bindings.contains(&route.binding)
            || request.overall_request_timeout.is_zero()
            || request.max_attempts == 0
            || request.logical_request.chunk_capacity == 0
        {
            return Err(PlanError::InvalidRouteCandidateClosure);
        }
        self.bind_candidate_closure(
            &request.candidate_bindings,
            Some(Arc::clone(&request.logical_request)),
            Arc::clone(&request.accepted_response),
            request.overall_request_timeout,
            request.max_attempts,
        )
    }

    /// A route miss has no attempt closure, but local response filters still
    /// require the accepted plan's RequestPinned cells.
    pub fn bind_local_response(&mut self) -> Result<(), PlanError> {
        let local = self
            .ingress_plan
            .as_deref()
            .ok_or(PlanError::ExecutionSegmentReleased("logical ingress"))?
            .local_response_plan
            .clone();
        self.bind_candidate_closure(
            &[],
            None,
            Arc::clone(&local.accepted_response),
            local.overall_request_timeout,
            0,
        )
    }

    fn bind_candidate_closure(
        &mut self,
        candidates: &[ResolvedTargetBindingId],
        logical: Option<CompiledLogicalRequestPlanHandle>,
        accepted: CompiledAcceptedResponsePlanHandle,
        overall_request_timeout: Duration,
        max_attempts: u32,
    ) -> Result<(), PlanError> {
        if self.request_configs.is_some() || self.route_attempt_plans.is_some() {
            return Err(PlanError::RouteAlreadyBound);
        }
        let index = self
            .attempt_plan_index
            .as_deref()
            .ok_or(PlanError::ExecutionSegmentReleased("attempt index"))?;
        let mut plans = HashMap::with_capacity(candidates.len());
        let mut config_ids = std::collections::HashSet::new();
        for id in candidates {
            if id.plan_revision() != self.plan_revision || plans.contains_key(id) {
                return Err(PlanError::InvalidRouteCandidateClosure);
            }
            let plan = index.resolve(*id)?;
            config_ids.extend(plan.config_cell_ids.iter().copied());
            plans.insert(*id, plan);
        }
        if let Some(logical) = logical.as_deref() {
            config_ids.extend(logical.config_cell_ids.iter().copied());
        }
        config_ids.extend(accepted.config_cell_ids.iter().copied());
        let catalog = self
            .config_catalog
            .as_deref()
            .ok_or(PlanError::ExecutionSegmentReleased("config catalog"))?;
        let handles: HashMap<_, _> = config_ids
            .into_iter()
            .map(|id| {
                catalog
                    .get(&id)
                    .cloned()
                    .map(|handle| (id, handle))
                    .ok_or(PlanError::MissingConfigCell(id))
            })
            .collect::<Result<_, _>>()?;
        let request_handles: Vec<_> = handles.values().cloned().collect();
        let request_configs = RequestConfigSnapshot::acquire(&request_handles)?;

        self.route_attempt_plans = Some(plans);
        self.candidate_bindings = Some(candidates.into());
        self.logical_request_plan = logical;
        self.accepted_response_plan = Some(accepted);
        self.overall_request_timeout = Some(overall_request_timeout);
        self.max_attempts = Some(max_attempts);
        self.config_handles = Some(handles);
        self.request_configs = Some(request_configs);
        // No request-owned Arc now reaches an unrelated route plan or cell.
        self.ingress_plan.take();
        self.attempt_plan_index.take();
        self.config_catalog.take();
        Ok(())
    }

    pub fn take_request_configs(&mut self) -> Result<RequestConfigSnapshot, PlanError> {
        self.request_configs
            .take()
            .ok_or(PlanError::ExecutionSegmentReleased(
                "request config snapshot",
            ))
    }

    pub fn overall_request_timeout(&self) -> Result<Duration, PlanError> {
        self.overall_request_timeout
            .ok_or(PlanError::ExecutionSegmentReleased("request timeout"))
    }

    pub fn max_attempts(&self) -> Result<u32, PlanError> {
        self.max_attempts
            .ok_or(PlanError::ExecutionSegmentReleased("attempt budget"))
    }

    pub fn candidate_bindings(&self) -> Result<&[ResolvedTargetBindingId], PlanError> {
        self.candidate_bindings
            .as_deref()
            .ok_or(PlanError::ExecutionSegmentReleased("candidate bindings"))
    }

    pub fn take_logical_request(&mut self) -> Result<LogicalRequestExecutionBinding, PlanError> {
        let plan = self
            .logical_request_plan
            .take()
            .ok_or(PlanError::ExecutionSegmentReleased("logical request"))?;
        let config_handles = bind_actual_config_cells(
            self.config_handles
                .as_ref()
                .ok_or(PlanError::ExecutionSegmentReleased("route config closure"))?,
            &plan.config_cell_ids,
        )?;
        self.ingress_plan.take();
        Ok(LogicalRequestExecutionBinding {
            plan,
            config_handles,
        })
    }

    pub fn resolve_attempt(
        &self,
        id: ResolvedTargetBindingId,
    ) -> Result<AttemptExecutionBinding, PlanError> {
        if id.plan_revision() != self.plan_revision {
            return Err(PlanError::CrossRevisionBinding {
                expected: self.plan_revision,
                actual: id.plan_revision(),
            });
        }
        let plan = self
            .route_attempt_plans
            .as_ref()
            .ok_or(PlanError::ExecutionSegmentReleased("route attempt closure"))?
            .get(&id)
            .cloned()
            .ok_or(PlanError::BindingOutsideRouteClosure(id))?;
        let config_handles = bind_actual_config_cells(
            self.config_handles
                .as_ref()
                .ok_or(PlanError::ExecutionSegmentReleased("route config closure"))?,
            &plan.config_cell_ids,
        )?;
        Ok(AttemptExecutionBinding {
            plan,
            config_handles,
        })
    }

    /// Consumes the attempt index and full config catalog at the terminal
    /// boundary. The returned owner contains only accepted-response config
    /// cells, so a long SSE cannot pin unrelated attempt plans or cells.
    pub fn take_accepted_response(
        &mut self,
    ) -> Result<AcceptedResponseExecutionBinding, PlanError> {
        let plan = self
            .accepted_response_plan
            .take()
            .ok_or(PlanError::ExecutionSegmentReleased("accepted response"))?;
        let config_handles = bind_actual_config_cells(
            self.config_handles
                .as_ref()
                .ok_or(PlanError::ExecutionSegmentReleased("route config closure"))?,
            &plan.config_cell_ids,
        )?;
        self.route_attempt_plans.take();
        self.candidate_bindings.take();
        self.logical_request_plan.take();
        self.config_handles.take();
        self.ingress_plan.take();
        Ok(AcceptedResponseExecutionBinding {
            plan,
            config_handles,
        })
    }
}

#[derive(Debug)]
pub struct LogicalRequestExecutionBinding {
    plan: CompiledLogicalRequestPlanHandle,
    config_handles: Vec<ConfigCellHandle>,
}

impl LogicalRequestExecutionBinding {
    pub fn plan(&self) -> &CompiledLogicalRequestPlan {
        &self.plan
    }

    pub fn acquire_phase_configs(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(&self.config_handles, ConfigBindingPolicy::PhasePinned)
    }

    pub fn acquire_event_configs(&self) -> Result<ConfigEventSnapshot, PlanError> {
        Ok(ConfigEventSnapshot {
            snapshot: ConfigScopeSnapshot::acquire(
                &self.config_handles,
                ConfigBindingPolicy::EventLive,
            )?,
            _not_send: PhantomData,
        })
    }
}

#[derive(Debug)]
pub struct AttemptExecutionBinding {
    plan: Arc<CompiledAttemptPlan>,
    config_handles: Vec<ConfigCellHandle>,
}

impl AttemptExecutionBinding {
    pub fn plan(&self) -> &CompiledAttemptPlan {
        &self.plan
    }

    pub(crate) fn plan_handle(&self) -> Arc<CompiledAttemptPlan> {
        Arc::clone(&self.plan)
    }

    pub fn acquire_connection_configs(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(&self.config_handles, ConfigBindingPolicy::ConnectionPinned)
    }

    pub fn acquire_attempt_configs(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(&self.config_handles, ConfigBindingPolicy::AttemptPinned)
    }

    pub fn acquire_phase_configs(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(&self.config_handles, ConfigBindingPolicy::PhasePinned)
    }

    pub fn acquire_event_configs(&self) -> Result<ConfigEventSnapshot, PlanError> {
        Ok(ConfigEventSnapshot {
            snapshot: ConfigScopeSnapshot::acquire(
                &self.config_handles,
                ConfigBindingPolicy::EventLive,
            )?,
            _not_send: PhantomData,
        })
    }
}

#[derive(Debug)]
pub struct AcceptedResponseExecutionBinding {
    plan: CompiledAcceptedResponsePlanHandle,
    config_handles: Vec<ConfigCellHandle>,
}

impl AcceptedResponseExecutionBinding {
    pub fn plan(&self) -> &CompiledAcceptedResponsePlan {
        &self.plan
    }

    pub fn acquire_connection_configs(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(&self.config_handles, ConfigBindingPolicy::ConnectionPinned)
    }

    pub fn acquire_attempt_configs(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(&self.config_handles, ConfigBindingPolicy::AttemptPinned)
    }

    pub fn acquire_phase_configs(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(&self.config_handles, ConfigBindingPolicy::PhasePinned)
    }

    pub fn acquire_event_configs(&self) -> Result<ConfigEventSnapshot, PlanError> {
        Ok(ConfigEventSnapshot {
            snapshot: ConfigScopeSnapshot::acquire(
                &self.config_handles,
                ConfigBindingPolicy::EventLive,
            )?,
            _not_send: PhantomData,
        })
    }
}

fn bind_actual_config_cells(
    catalog: &HashMap<ConfigCellId, ConfigCellHandle>,
    ids: &[ConfigCellId],
) -> Result<Vec<ConfigCellHandle>, PlanError> {
    ids.iter()
        .map(|id| {
            catalog
                .get(id)
                .cloned()
                .ok_or(PlanError::MissingConfigCell(*id))
        })
        .collect()
}
