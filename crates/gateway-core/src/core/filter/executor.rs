use super::*;

#[derive(Clone, Debug)]
pub struct FilterCallbackContext {
    pub(super) scope: ChildScope,
    pub(super) services: FilterExecutorServices,
}

impl FilterCallbackContext {
    pub fn new(deadline: Instant, cancellation: CancellationToken) -> Self {
        let budget = BudgetTree::new(1024 * 1024, 1024 * 1024)
            .expect("constant standalone filter budget")
            .stream(1024 * 1024)
            .expect("constant standalone filter stream");
        let executor =
            |kind| BoundedExecutor::new(kind, 1, 8).expect("constant standalone executor limits");
        let services = FilterExecutorServices::new(
            deadline,
            cancellation,
            budget,
            executor(ExecutorKind::SidecallIo),
            executor(ExecutorKind::Compute),
            executor(ExecutorKind::BlockingControl),
        );
        Self::with_services(services)
    }

    pub fn with_services(services: FilterExecutorServices) -> Self {
        Self {
            scope: services.scope.clone(),
            services,
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self::new(Instant::now() + timeout, CancellationToken::new())
    }

    pub fn services(&self) -> &FilterExecutorServices {
        &self.services
    }
}

/// Request/scope-owned executor seam exposed to native filter factories and
/// callbacks. Only the three non-worker callback classes are admitted here;
/// gateway I/O stays owned by the lifecycle driver.
#[derive(Clone, Debug)]
pub struct FilterExecutorServices {
    scope: ChildScope,
    pub(super) budget: StreamBudget,
    sidecall: BoundedExecutor,
    compute: BoundedExecutor,
    blocking: BoundedExecutor,
}

impl FilterExecutorServices {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        deadline: Instant,
        cancellation: CancellationToken,
        budget: StreamBudget,
        sidecall: BoundedExecutor,
        compute: BoundedExecutor,
        blocking: BoundedExecutor,
    ) -> Self {
        Self {
            scope: ChildScope::with_cancellation(deadline, cancellation),
            budget,
            sidecall,
            compute,
            blocking,
        }
    }

    /// Returns a linear admission before any payload or future is built. The
    /// payload reservation is held through I/O completion and resume.
    pub fn admit(
        &self,
        kind: ExecutorKind,
        estimated_payload_bytes: usize,
    ) -> Result<FilterJobAdmission, FilterError> {
        let executor = match kind {
            ExecutorKind::SidecallIo => self.sidecall.clone(),
            ExecutorKind::Compute => self.compute.clone(),
            ExecutorKind::BlockingControl => self.blocking.clone(),
            ExecutorKind::GatewayIo => return Err(FilterError::UnsupportedExecutorKind),
        };
        let admission = executor
            .try_admit(estimated_payload_bytes)
            .map_err(filter_executor_error)?;
        let payload_reservation = self
            .budget
            .reserve(MemoryRole::SemanticState, estimated_payload_bytes)
            .map_err(|_| FilterError::ExecutorPayloadBudget)?;
        Ok(FilterJobAdmission {
            scope: self.scope.clone(),
            executor,
            admission,
            payload: FilterPayloadPermit {
                bytes: estimated_payload_bytes,
                _reservation: payload_reservation,
            },
        })
    }

    pub async fn cancel_and_finalize(&self, join_timeout: Duration) -> Result<(), FilterError> {
        self.scope
            .cancel_and_finalize(join_timeout)
            .await
            .map_err(filter_executor_error)
    }

    pub fn active_children(&self) -> usize {
        self.scope.active_children()
    }

    pub(super) fn promote_body(
        &self,
        bytes: &[u8],
        sources: FilterBodySourceSet,
    ) -> Result<PromotedBody, FilterError> {
        let charged =
            ChargedBytes::copy_from_opaque(&self.budget, MemoryRole::SemanticState, bytes)
                .map_err(|_| FilterError::BodyOutputBudget)?;
        Ok(PromotedBody {
            bytes: charged.into_drop_tracked_bytes(),
            sources,
        })
    }

    pub(super) fn body_emitter(
        &self,
        role: MemoryRole,
        max_units: usize,
        inherited_sources: FilterBodySourceSet,
    ) -> Result<FilterBodyEmitter, FilterError> {
        let metadata_bytes = max_units
            .checked_mul(std::mem::size_of::<FilterBodyOutputData>())
            .ok_or(FilterError::BodyOutputBudget)?;
        let metadata_reservation = self
            .budget
            .reserve(MemoryRole::SemanticState, metadata_bytes)
            .map_err(|_| FilterError::BodyOutputBudget)?;
        Ok(FilterBodyEmitter {
            budget: self.budget.clone(),
            role,
            max_units,
            units: Vec::with_capacity(max_units),
            metadata_reservation: Some(Arc::new(metadata_reservation)),
            inherited_sources,
        })
    }
}

/// Linear payload permission. Its reservation exists before the builder
/// closure is invoked and remains live until the admitted future completes.
#[derive(Debug)]
pub struct FilterPayloadPermit {
    bytes: usize,
    _reservation: Reservation,
}

impl FilterPayloadPermit {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

pub struct FilterJobAdmission {
    scope: ChildScope,
    executor: BoundedExecutor,
    admission: QueuedAdmission,
    payload: FilterPayloadPermit,
}

impl FilterJobAdmission {
    /// The builder is called only after both queue admission and an executor
    /// concurrency permit have been obtained.
    pub async fn run<F, Fut, T>(self, build: F) -> Result<(T, ResumeTiming), FilterError>
    where
        F: FnOnce(FilterPayloadPermit) -> Fut + Send,
        Fut: Future<Output = T> + Send,
        T: Send,
    {
        let Self {
            scope,
            executor,
            admission,
            payload,
        } = self;
        if executor.kind() != ExecutorKind::SidecallIo {
            return Err(filter_executor_error(ExecutorError::WrongExecutionMode));
        }
        let completion = scope
            .run(&executor, admission, async move { build(payload).await })
            .await
            .map_err(filter_executor_error)?;
        scope
            .resume(&executor, completion)
            .map_err(filter_executor_error)
    }

    /// Runs CPU or blocking/control work on that class's fixed worker pool.
    /// The owning payload and builder cross the thread boundary only after
    /// queue/concurrency admission; the gateway Tokio task never polls them.
    pub async fn run_offloaded<F, T>(self, build: F) -> Result<(T, ResumeTiming), FilterError>
    where
        F: FnOnce(FilterPayloadPermit) -> T + Send + 'static,
        T: Send + 'static,
    {
        let Self {
            scope,
            executor,
            admission,
            payload,
        } = self;
        if !matches!(
            executor.kind(),
            ExecutorKind::Compute | ExecutorKind::BlockingControl
        ) {
            return Err(filter_executor_error(ExecutorError::WrongExecutionMode));
        }
        let completion = scope
            .run_offloaded(&executor, admission, move || build(payload))
            .await
            .map_err(filter_executor_error)?;
        scope
            .resume(&executor, completion)
            .map_err(filter_executor_error)
    }
}

pub(super) fn filter_executor_error(error: ExecutorError) -> FilterError {
    match error {
        ExecutorError::DeadlineExceeded => FilterError::CallbackDeadline,
        ExecutorError::Cancelled | ExecutorError::ResultDropped => FilterError::CallbackCancelled,
        error => FilterError::Executor(error.to_string().into()),
    }
}
