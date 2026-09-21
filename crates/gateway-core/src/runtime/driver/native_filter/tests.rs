use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct ProductionLineageContinuations {
    header: Mutex<Option<crate::core::filter::FilterContinuation>>,
    data: Mutex<Option<crate::core::filter::FilterContinuation>>,
}

struct ProductionLineageFactory {
    continuations: Arc<ProductionLineageContinuations>,
}

impl NativeFilterFactory for ProductionLineageFactory {
    fn create(
        &self,
        descriptor: &CompiledFilterDescriptor,
        _: &FilterInvocationContext,
        _: &FilterExecutorServices,
    ) -> Result<Box<dyn crate::core::filter::NativeFilter>, FilterError> {
        Ok(Box::new(ProductionLineageFilter {
            id: descriptor.id.clone(),
            continuations: Arc::clone(&self.continuations),
            promoted: None,
            data_calls: 0,
        }))
    }
}

struct ProductionLineageFilter {
    id: Arc<str>,
    continuations: Arc<ProductionLineageContinuations>,
    promoted: Option<crate::core::filter::PromotedBody>,
    data_calls: usize,
}

#[async_trait::async_trait]
impl crate::core::filter::NativeFilter for ProductionLineageFilter {
    fn name(&self) -> &str {
        &self.id
    }

    fn may_drop_body(&self) -> bool {
        matches!(
            self.id.as_ref(),
            "lineage-no-buffer" | "attempt-no-buffer-second"
        )
    }

    fn capabilities(&self) -> crate::core::filter::FilterCapabilities {
        match self.id.as_ref() {
            "lineage-buffer-merge" => {
                crate::core::filter::FilterCapabilities::observe_only().with_body_expansion()
            }
            "attempt-buffer-replace" => {
                crate::core::filter::FilterCapabilities::observe_only().with_body_mutation()
            }
            "lineage-no-buffer" | "attempt-no-buffer-second" => {
                crate::core::filter::FilterCapabilities::observe_only().with_body_drop()
            }
            _ => crate::core::filter::FilterCapabilities::observe_only(),
        }
    }

    async fn on_headers(
        &mut self,
        input: crate::core::filter::HeaderInput,
    ) -> Result<crate::core::filter::HeadersAction, FilterError> {
        if matches!(
            self.id.as_ref(),
            "lineage-buffer-merge" | "attempt-buffer-replace"
        ) {
            *self
                .continuations
                .header
                .lock()
                .expect("lineage header continuation") = Some(input.continuation);
            Ok(
                crate::core::filter::HeadersAction::StopAllIterationAndBuffer(
                    crate::core::filter::HeaderPatch::default(),
                ),
            )
        } else {
            Ok(crate::core::filter::HeadersAction::Continue(
                crate::core::filter::HeaderPatch::default(),
            ))
        }
    }

    async fn on_data(
        &mut self,
        input: crate::core::filter::DataInput<'_>,
    ) -> Result<crate::core::filter::DataAction, FilterError> {
        self.data_calls += 1;
        if self.id.as_ref() == "attempt-buffer-replace" {
            if self.data_calls == 1 {
                let mut emitter = input.body_emitter()?;
                emitter.emit_copy(input.bytes())?;
                return Ok(crate::core::filter::DataAction::Emit {
                    output: crate::core::filter::FilterBodyEmission::Replace(emitter.finish()),
                    patch: crate::core::filter::HeaderPatch::default(),
                });
            }
            return Ok(crate::core::filter::DataAction::Emit {
                output: crate::core::filter::FilterBodyEmission::Forward,
                patch: crate::core::filter::HeaderPatch::default(),
            });
        }
        if self.id.as_ref() == "lineage-buffer-merge" {
            if self.data_calls == 1 {
                self.promoted = Some(input.promote()?);
                return Ok(crate::core::filter::DataAction::Emit {
                    output: crate::core::filter::FilterBodyEmission::Forward,
                    patch: crate::core::filter::HeaderPatch::default(),
                });
            }
            let promoted = self.promoted.take().expect("promoted A source");
            let mut emitter = input.body_emitter()?;
            emitter.emit_copy_from_promoted(&[&promoted], b"merged")?;
            return Ok(crate::core::filter::DataAction::Emit {
                output: crate::core::filter::FilterBodyEmission::Replace(emitter.finish()),
                patch: crate::core::filter::HeaderPatch::default(),
            });
        }
        if input.bytes() == b"A" {
            return Ok(crate::core::filter::DataAction::Emit {
                output: crate::core::filter::FilterBodyEmission::Forward,
                patch: crate::core::filter::HeaderPatch::default(),
            });
        }
        if self.id.as_ref() == "attempt-no-buffer-second" && self.data_calls != 2 {
            return Ok(crate::core::filter::DataAction::Emit {
                output: crate::core::filter::FilterBodyEmission::Forward,
                patch: crate::core::filter::HeaderPatch::default(),
            });
        }
        *self
            .continuations
            .data
            .lock()
            .expect("lineage data continuation") = Some(input.continuation);
        Ok(crate::core::filter::DataAction::StopIteration {
            retention: crate::core::filter::RetentionMode::NoBuffer,
            patch: crate::core::filter::HeaderPatch::default(),
        })
    }

    async fn on_trailers(
        &mut self,
        _: crate::core::filter::TrailersInput,
    ) -> Result<crate::core::filter::TrailersAction, FilterError> {
        Ok(crate::core::filter::TrailersAction::Continue(
            crate::core::filter::HeaderPatch::default(),
        ))
    }

    fn on_finalize(&mut self) {}
}

fn production_lineage_filters(
    continuations: Arc<ProductionLineageContinuations>,
) -> NativeGatewayRequestFilters {
    let mut manager = NativeGatewayFilterManager::new(64 * 1024).expect("native manager");
    for id in ["lineage-buffer-merge", "lineage-no-buffer"] {
        manager
            .register(
                id,
                Arc::new(ProductionLineageFactory {
                    continuations: Arc::clone(&continuations),
                }),
            )
            .expect("register lineage filter");
    }
    manager.instantiate_request().expect("request filters")
}

fn production_attempt_lineage_filters(
    continuations: Arc<ProductionLineageContinuations>,
) -> NativeGatewayRequestFilters {
    let mut manager = NativeGatewayFilterManager::new(64 * 1024).expect("native manager");
    for id in ["attempt-buffer-replace", "attempt-no-buffer-second"] {
        manager
            .register(
                id,
                Arc::new(ProductionLineageFactory {
                    continuations: Arc::clone(&continuations),
                }),
            )
            .expect("register attempt lineage filter");
    }
    manager.instantiate_request().expect("request filters")
}

fn production_lineage_descriptors() -> [CompiledFilterDescriptor; 2] {
    [
        CompiledFilterDescriptor::new("lineage-buffer-merge", 4)
            .expect("buffer descriptor")
            .with_capabilities(
                crate::core::filter::FilterCapabilities::observe_only().with_body_expansion(),
            ),
        CompiledFilterDescriptor::new("lineage-no-buffer", 4)
            .expect("NoBuffer descriptor")
            .with_capabilities(
                crate::core::filter::FilterCapabilities::observe_only().with_body_drop(),
            ),
    ]
}

fn production_attempt_lineage_descriptors() -> [CompiledFilterDescriptor; 2] {
    [
        CompiledFilterDescriptor::new("attempt-buffer-replace", 4)
            .expect("attempt buffer descriptor")
            .with_capabilities(
                crate::core::filter::FilterCapabilities::observe_only().with_body_mutation(),
            ),
        CompiledFilterDescriptor::new("attempt-no-buffer-second", 4)
            .expect("attempt NoBuffer descriptor")
            .with_capabilities(
                crate::core::filter::FilterCapabilities::observe_only().with_body_drop(),
            ),
    ]
}

fn resume_lineage(slot: &Mutex<Option<crate::core::filter::FilterContinuation>>, name: &str) {
    slot.lock()
        .expect("lineage continuation slot")
        .take()
        .unwrap_or_else(|| panic!("missing {name} continuation"))
        .resume(crate::core::filter::ResumeAction::Continue(
            crate::core::filter::HeaderPatch::default(),
        ))
        .unwrap_or_else(|_| panic!("resume {name} continuation"));
}

struct ProductionNoBufferFactory {
    continuation: Arc<Mutex<Option<crate::core::filter::FilterContinuation>>>,
}

impl NativeFilterFactory for ProductionNoBufferFactory {
    fn create(
        &self,
        _: &CompiledFilterDescriptor,
        _: &FilterInvocationContext,
        _: &FilterExecutorServices,
    ) -> Result<Box<dyn crate::core::filter::NativeFilter>, FilterError> {
        Ok(Box::new(ProductionNoBufferFilter {
            continuation: Arc::clone(&self.continuation),
        }))
    }
}

struct ProductionNoBufferFilter {
    continuation: Arc<Mutex<Option<crate::core::filter::FilterContinuation>>>,
}

#[async_trait::async_trait]
impl crate::core::filter::NativeFilter for ProductionNoBufferFilter {
    fn name(&self) -> &str {
        "production-no-buffer"
    }

    fn may_drop_body(&self) -> bool {
        true
    }

    fn capabilities(&self) -> crate::core::filter::FilterCapabilities {
        crate::core::filter::FilterCapabilities::observe_only().with_body_drop()
    }

    async fn on_headers(
        &mut self,
        _: crate::core::filter::HeaderInput,
    ) -> Result<crate::core::filter::HeadersAction, FilterError> {
        Ok(crate::core::filter::HeadersAction::StopIteration(
            crate::core::filter::HeaderPatch::default(),
        ))
    }

    async fn on_data(
        &mut self,
        input: crate::core::filter::DataInput<'_>,
    ) -> Result<crate::core::filter::DataAction, FilterError> {
        *self.continuation.lock().expect("continuation slot") = Some(input.continuation);
        Ok(crate::core::filter::DataAction::StopIteration {
            retention: crate::core::filter::RetentionMode::NoBuffer,
            patch: crate::core::filter::HeaderPatch::default(),
        })
    }

    async fn on_trailers(
        &mut self,
        _: crate::core::filter::TrailersInput,
    ) -> Result<crate::core::filter::TrailersAction, FilterError> {
        Ok(crate::core::filter::TrailersAction::Continue(
            crate::core::filter::HeaderPatch::default(),
        ))
    }

    fn on_finalize(&mut self) {}
}

fn production_no_buffer_descriptor() -> CompiledFilterDescriptor {
    CompiledFilterDescriptor::new("production-no-buffer", 2)
        .expect("descriptor")
        .with_capabilities(crate::core::filter::FilterCapabilities::observe_only().with_body_drop())
}

fn production_no_buffer_filters(
    continuation: Arc<Mutex<Option<crate::core::filter::FilterContinuation>>>,
) -> NativeGatewayRequestFilters {
    let mut manager = NativeGatewayFilterManager::new(64 * 1024).expect("native manager");
    manager
        .register(
            "production-no-buffer",
            Arc::new(ProductionNoBufferFactory { continuation }),
        )
        .expect("register no-buffer filter");
    manager.instantiate_request().expect("request filters")
}

fn production_filter_context(
    scope_kind: ScopeKind,
    budget: &StreamBudget,
) -> GatewayFilterScopeContext {
    let deadline = Instant::now() + Duration::from_secs(2);
    let cancellation = CancellationToken::new();
    let executor =
        |kind| BoundedExecutor::new(kind, 1, 2).expect("constant production filter test limits");
    let executors = FilterExecutorServices::new(
        deadline,
        cancellation.clone(),
        budget.clone(),
        executor(ExecutorKind::SidecallIo),
        executor(ExecutorKind::Compute),
        executor(ExecutorKind::BlockingControl),
    );
    GatewayFilterScopeContext {
        invocation: FilterInvocationContext {
            stream_id: StreamId(701),
            scope_id: ScopeId(701),
            scope_kind,
            plan_revision: 1,
            binding_local_id: Some(1),
            request_id: 701,
            attempt_id: (scope_kind == ScopeKind::RouteAttempt).then_some(1),
            attempt_generation: (scope_kind == ScopeKind::RouteAttempt).then_some(1),
            configs: FilterConfigSnapshot::default(),
        },
        deadline,
        cancellation,
        telemetry: None,
        budget: budget.clone(),
        executors,
        accepted_body_plan: (scope_kind == ScopeKind::AcceptedResponse).then_some(
            BodyPlan::BufferedTransform {
                max_body_bytes: 64 * 1024,
            },
        ),
    }
}

fn resume_production_no_buffer(
    continuation: &Arc<Mutex<Option<crate::core::filter::FilterContinuation>>>,
) {
    continuation
        .lock()
        .expect("continuation slot")
        .take()
        .expect("data continuation")
        .resume(crate::core::filter::ResumeAction::Continue(
            crate::core::filter::HeaderPatch::default(),
        ))
        .expect("resume accepted by machine");
}

struct ExactReplacementFilter;

#[async_trait::async_trait]
impl crate::core::filter::NativeFilter for ExactReplacementFilter {
    fn name(&self) -> &str {
        "exact-replacement"
    }

    fn capabilities(&self) -> crate::core::filter::FilterCapabilities {
        crate::core::filter::FilterCapabilities::observe_only().with_body_mutation()
    }

    async fn on_headers(
        &mut self,
        _: crate::core::filter::HeaderInput,
    ) -> Result<crate::core::filter::HeadersAction, FilterError> {
        Ok(crate::core::filter::HeadersAction::Continue(
            crate::core::filter::HeaderPatch::default(),
        ))
    }

    async fn on_data(
        &mut self,
        input: crate::core::filter::DataInput<'_>,
    ) -> Result<crate::core::filter::DataAction, FilterError> {
        let mut emitter = input.body_emitter()?;
        emitter.emit_copy(input.bytes())?;
        Ok(crate::core::filter::DataAction::Emit {
            output: crate::core::filter::FilterBodyEmission::Replace(emitter.finish()),
            patch: crate::core::filter::HeaderPatch::default(),
        })
    }

    async fn on_trailers(
        &mut self,
        _: crate::core::filter::TrailersInput,
    ) -> Result<crate::core::filter::TrailersAction, FilterError> {
        Ok(crate::core::filter::TrailersAction::Continue(
            crate::core::filter::HeaderPatch::default(),
        ))
    }

    fn on_finalize(&mut self) {}
}

mod runtime_filters;
mod source_ledger;
