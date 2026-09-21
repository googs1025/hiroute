use super::*;

#[derive(Clone, Debug)]
pub(super) struct FilterExecutorPool {
    sidecall: BoundedExecutor,
    compute: BoundedExecutor,
    blocking: BoundedExecutor,
}

impl FilterExecutorPool {
    pub(super) fn new(limits: &GatewayCoreLifecycleLimits) -> Result<Self, ExecutorError> {
        Ok(Self {
            sidecall: BoundedExecutor::new(
                ExecutorKind::SidecallIo,
                limits.filter_sidecall_concurrency,
                limits.filter_sidecall_queue,
            )?,
            compute: BoundedExecutor::new(
                ExecutorKind::Compute,
                limits.filter_compute_concurrency,
                limits.filter_compute_queue,
            )?,
            blocking: BoundedExecutor::new(
                ExecutorKind::BlockingControl,
                limits.filter_blocking_concurrency,
                limits.filter_blocking_queue,
            )?,
        })
    }

    pub(super) fn services(
        &self,
        deadline: Instant,
        cancellation: CancellationToken,
        budget: StreamBudget,
        telemetry: Option<RequestTelemetry>,
    ) -> FilterExecutorServices {
        let with_telemetry = |executor: &BoundedExecutor| {
            telemetry.as_ref().map_or_else(
                || executor.clone(),
                |value| executor.clone().with_telemetry(value.clone()),
            )
        };
        FilterExecutorServices::new(
            deadline,
            cancellation,
            budget,
            with_telemetry(&self.sidecall),
            with_telemetry(&self.compute),
            with_telemetry(&self.blocking),
        )
    }
}

pub(super) async fn await_attempt_operation<T, F>(
    session: &mut dyn GatewaySession,
    cancellation: &CancellationToken,
    deadline: Instant,
    telemetry: Option<&RequestTelemetry>,
    operation: F,
) -> Result<T, GatewayExecutionError>
where
    F: Future<Output = Result<T, AttemptError>>,
{
    await_request_operation(session, cancellation, deadline, telemetry, async {
        let result = operation.await;
        match &result {
            Err(AttemptError::Cancelled) => observe_runtime_error(telemetry, ErrorClass::Cancelled),
            Err(AttemptError::DeadlineExceeded) => {
                observe_runtime_error(telemetry, ErrorClass::Deadline)
            }
            _ => {}
        }
        result.map_err(GatewayExecutionError::Attempt)
    })
    .await
}

pub(super) async fn await_request_operation<T, F>(
    session: &mut dyn GatewaySession,
    cancellation: &CancellationToken,
    deadline: Instant,
    telemetry: Option<&RequestTelemetry>,
    operation: F,
) -> Result<T, GatewayExecutionError>
where
    F: Future<Output = Result<T, GatewayExecutionError>>,
{
    if cancellation.is_cancelled() {
        observe_runtime_error(telemetry, ErrorClass::Cancelled);
        return Err(GatewayExecutionError::Cancelled);
    }
    if Instant::now() >= deadline {
        observe_runtime_error(telemetry, ErrorClass::Deadline);
        return Err(GatewayExecutionError::Deadline);
    }
    let deadline_sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
    tokio::pin!(deadline_sleep);
    let operation = AssertUnwindSafe(operation).catch_unwind();
    tokio::pin!(operation);
    tokio::select! {
        biased;
        result = &mut operation => match result {
            Ok(result) => result,
            Err(_) => {
                cancellation.cancel();
                observe_runtime_error(telemetry, ErrorClass::CallbackPanic);
                Err(GatewayExecutionError::OperationPanic)
            }
        },
        _ = cancellation.cancelled() => {
            observe_runtime_error(telemetry, ErrorClass::Cancelled);
            Err(GatewayExecutionError::Cancelled)
        },
        _ = session.wait_for_disconnect() => {
            cancellation.cancel();
            observe_runtime_error(telemetry, ErrorClass::Cancelled);
            Err(GatewayExecutionError::Cancelled)
        }
        _ = &mut deadline_sleep => {
            cancellation.cancel();
            observe_runtime_error(telemetry, ErrorClass::Deadline);
            Err(GatewayExecutionError::Deadline)
        }
    }
}

/// Waits for work owned by one selected candidate before an `AttemptExchange`
/// exists. An attempt-local deadline is recoverable by the request-owned
/// decision session, while an equal (or earlier) overall deadline remains a
/// terminal request cancellation.
pub(super) async fn await_preexchange_operation<T, F>(
    session: &mut dyn GatewaySession,
    cancellation: &CancellationToken,
    overall_deadline: Instant,
    attempt_deadline: Instant,
    telemetry: Option<&RequestTelemetry>,
    operation: F,
) -> Result<T, GatewayExecutionError>
where
    F: Future<Output = Result<T, GatewayExecutionError>>,
{
    if cancellation.is_cancelled() {
        observe_runtime_error(telemetry, ErrorClass::Cancelled);
        return Err(GatewayExecutionError::Cancelled);
    }
    let now = Instant::now();
    if now >= overall_deadline {
        cancellation.cancel();
        observe_runtime_error(telemetry, ErrorClass::Deadline);
        return Err(GatewayExecutionError::Deadline);
    }
    if now >= attempt_deadline {
        observe_runtime_error(telemetry, ErrorClass::Deadline);
        return Err(GatewayExecutionError::PreexchangeAttemptDeadline);
    }

    let effective_deadline = attempt_deadline.min(overall_deadline);
    let deadline_sleep =
        tokio::time::sleep_until(tokio::time::Instant::from_std(effective_deadline));
    tokio::pin!(deadline_sleep);
    let operation = AssertUnwindSafe(operation).catch_unwind();
    tokio::pin!(operation);
    tokio::select! {
        biased;
        result = &mut operation => match result {
            Ok(result) => result,
            Err(_) => {
                cancellation.cancel();
                observe_runtime_error(telemetry, ErrorClass::CallbackPanic);
                Err(GatewayExecutionError::OperationPanic)
            }
        },
        _ = cancellation.cancelled() => {
            observe_runtime_error(telemetry, ErrorClass::Cancelled);
            Err(GatewayExecutionError::Cancelled)
        },
        _ = session.wait_for_disconnect() => {
            cancellation.cancel();
            observe_runtime_error(telemetry, ErrorClass::Cancelled);
            Err(GatewayExecutionError::Cancelled)
        }
        _ = &mut deadline_sleep => {
            observe_runtime_error(telemetry, ErrorClass::Deadline);
            if attempt_deadline < overall_deadline {
                Err(GatewayExecutionError::PreexchangeAttemptDeadline)
            } else {
                cancellation.cancel();
                Err(GatewayExecutionError::Deadline)
            }
        }
    }
}

pub(super) async fn await_session_operation<T, F>(
    cancellation: &CancellationToken,
    deadline: Instant,
    telemetry: Option<&RequestTelemetry>,
    operation: F,
) -> Result<T, GatewayExecutionError>
where
    F: Future<Output = Result<T, GatewayExecutionError>>,
{
    if cancellation.is_cancelled() {
        observe_runtime_error(telemetry, ErrorClass::Cancelled);
        return Err(GatewayExecutionError::Cancelled);
    }
    if Instant::now() >= deadline {
        observe_runtime_error(telemetry, ErrorClass::Deadline);
        return Err(GatewayExecutionError::Deadline);
    }
    let deadline_sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
    tokio::pin!(deadline_sleep);
    let operation = AssertUnwindSafe(operation).catch_unwind();
    tokio::pin!(operation);
    tokio::select! {
        biased;
        result = &mut operation => match result {
            Ok(result) => result,
            Err(_) => {
                cancellation.cancel();
                observe_runtime_error(telemetry, ErrorClass::CallbackPanic);
                Err(GatewayExecutionError::OperationPanic)
            }
        },
        _ = cancellation.cancelled() => {
            observe_runtime_error(telemetry, ErrorClass::Cancelled);
            Err(GatewayExecutionError::Cancelled)
        }
        _ = &mut deadline_sleep => {
            cancellation.cancel();
            observe_runtime_error(telemetry, ErrorClass::Deadline);
            Err(GatewayExecutionError::Deadline)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn filter_scope_context(
    request_id: RequestId,
    plan_revision: crate::core::execution_plan::PlanRevision,
    binding: Option<ResolvedTargetBindingId>,
    attempt_id: Option<AttemptId>,
    attempt_generation: Option<AttemptGeneration>,
    scope_id: ScopeId,
    scope_kind: ScopeKind,
    deadline: Instant,
    cancellation: &CancellationToken,
    telemetry: Option<RequestTelemetry>,
    budget: &StreamBudget,
    configs: FilterConfigSnapshot,
    executor_pool: &FilterExecutorPool,
) -> GatewayFilterScopeContext {
    let executors = executor_pool.services(
        deadline,
        cancellation.clone(),
        budget.clone(),
        telemetry.clone(),
    );
    GatewayFilterScopeContext {
        invocation: FilterInvocationContext {
            stream_id: StreamId(request_id.0),
            scope_id,
            scope_kind,
            plan_revision: plan_revision.0,
            binding_local_id: binding.map(ResolvedTargetBindingId::local_id),
            request_id: request_id.0,
            attempt_id: attempt_id.map(|attempt| attempt.0),
            attempt_generation: attempt_generation.map(|generation| generation.0),
            configs,
        },
        deadline,
        cancellation: cancellation.clone(),
        telemetry,
        budget: budget.clone(),
        executors,
        accepted_body_plan: None,
    }
}

pub(super) fn materialize_filter_configs(
    descriptors: &[CompiledFilterDescriptor],
    connection: Option<&ConfigScopeSnapshot>,
    request: &RequestConfigSnapshot,
    attempt: Option<&ConfigScopeSnapshot>,
    phase: &ConfigScopeSnapshot,
    event: &ConfigEventSnapshot,
) -> Result<FilterConfigSnapshot, GatewayExecutionError> {
    let mut seen = std::collections::HashSet::new();
    let mut values = Vec::new();
    for dependency in descriptors
        .iter()
        .flat_map(CompiledFilterDescriptor::config_dependencies)
    {
        if !seen.insert(dependency.id) {
            continue;
        }
        let value = match dependency.policy {
            ConfigBindingPolicy::ConnectionPinned => {
                connection.and_then(|snapshot| snapshot.value(dependency.id))
            }
            ConfigBindingPolicy::RequestPinned => request.value(dependency.id),
            ConfigBindingPolicy::AttemptPinned => {
                attempt.and_then(|snapshot| snapshot.value(dependency.id))
            }
            ConfigBindingPolicy::PhasePinned => phase.value(dependency.id),
            ConfigBindingPolicy::EventLive => event.value(dependency.id),
        }
        .ok_or_else(|| {
            GatewayExecutionError::Filter(Arc::from(format!(
                "declared filter config {} is unavailable in {:?}",
                dependency.id, dependency.policy
            )))
        })?
        .clone();
        values.push(FilterConfigValue {
            id: dependency.id,
            policy: dependency.policy,
            generation: value.generation,
            value,
        });
    }
    values.sort_unstable_by_key(|value| value.id.0);
    Ok(FilterConfigSnapshot::new(Arc::<[FilterConfigValue]>::from(
        values,
    )))
}

pub(super) fn match_compiled_route(
    plan: &CompiledIngressPlan,
    head: &GatewayRequestHead,
) -> Option<CompiledRoute> {
    let authority = head
        .authority
        .as_deref()
        .or_else(|| head.headers.get(HOST).and_then(|value| value.to_str().ok()))?;
    let normalized_host = authority
        .parse::<http::uri::Authority>()
        .ok()?
        .host()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let path = head.path_and_query.split('?').next().unwrap_or("/");
    plan.routes
        .iter()
        .filter(|route| {
            route.normalized_host.as_ref() == normalized_host
                && path_prefix_matches(path, &route.path_prefix)
        })
        .max_by_key(|route| route.path_prefix.len())
        .cloned()
}

pub(crate) fn path_prefix_matches(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return path.starts_with('/');
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub(super) fn request_content_length(
    headers: &HeaderMap,
) -> Result<Option<usize>, GatewayExecutionError> {
    if headers.contains_key(TRANSFER_ENCODING) && headers.contains_key(CONTENT_LENGTH) {
        return Err(GatewayExecutionError::InvalidRequestFraming);
    }
    let mut declared = None;
    for value in headers.get_all(CONTENT_LENGTH) {
        let value = value
            .to_str()
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .ok_or(GatewayExecutionError::InvalidRequestFraming)?;
        if declared.is_some_and(|prior| prior != value) {
            return Err(GatewayExecutionError::InvalidRequestFraming);
        }
        declared = Some(value);
    }
    Ok(declared)
}
