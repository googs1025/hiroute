use crate::fixture::*;

#[tokio::test]
async fn filter_executor_services_are_narrow_linear_and_admit_before_builder() {
    let deadline = Instant::now() + Duration::from_secs(1);
    let budget = BudgetTree::new(64, 64).unwrap().stream(64).unwrap();
    let sidecall = BoundedExecutor::new(ExecutorKind::SidecallIo, 1, 1).unwrap();
    let services = FilterExecutorServices::new(
        deadline,
        CancellationToken::new(),
        budget,
        sidecall,
        BoundedExecutor::new(ExecutorKind::Compute, 1, 1).unwrap(),
        BoundedExecutor::new(ExecutorKind::BlockingControl, 1, 1).unwrap(),
    );

    let Err(gateway_io_error) = services.admit(ExecutorKind::GatewayIo, 1) else {
        panic!("native filters must not receive a generic gateway-I/O spawn capability");
    };
    assert_eq!(gateway_io_error, FilterError::UnsupportedExecutorKind);
    let admission = services.admit(ExecutorKind::SidecallIo, 16).unwrap();
    assert!(
        matches!(
            services.admit(ExecutorKind::SidecallIo, 16),
            Err(FilterError::Executor(_))
        ),
        "queue admission must fail before a second payload/future can be built",
    );
    let builder_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    assert_eq!(builder_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    let observed_calls = Arc::clone(&builder_calls);
    let ((payload_bytes, value), timing) = admission
        .run(move |permit| async move {
            observed_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (permit.bytes(), "completed")
        })
        .await
        .unwrap();
    assert_eq!((payload_bytes, value), (16, "completed"));
    assert_eq!(builder_calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(timing.request_resumed_at >= timing.io_completed_at);
    services
        .cancel_and_finalize(Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(services.active_children(), 0);
}

#[tokio::test]
async fn callback_body_promotion_keeps_charge_until_the_last_clone_drops() {
    struct PromoteFilter {
        retained: Arc<Mutex<Option<PromotedBody>>>,
    }

    #[async_trait]
    impl NativeFilter for PromoteFilter {
        fn name(&self) -> &str {
            "promote"
        }

        async fn on_headers(&mut self, _: HeaderInput) -> Result<HeadersAction, FilterError> {
            Ok(HeadersAction::Continue(HeaderPatch::default()))
        }

        async fn on_data(&mut self, input: DataInput<'_>) -> Result<DataAction, FilterError> {
            let promoted = input.promote()?;
            assert_eq!(promoted.bytes(), b"held");
            *self.retained.lock().unwrap() = Some(promoted.clone());
            drop(promoted);
            Ok(DataAction::Continue(HeaderPatch::default()))
        }

        async fn on_trailers(&mut self, _: TrailersInput) -> Result<TrailersAction, FilterError> {
            Ok(TrailersAction::Continue(HeaderPatch::default()))
        }

        fn on_finalize(&mut self) {}
    }

    let budget = BudgetTree::new(1024, 1024).unwrap().stream(1024).unwrap();
    let executor =
        |kind| BoundedExecutor::new(kind, 1, 1).expect("constant promotion executor limits");
    let services = FilterExecutorServices::new(
        Instant::now() + Duration::from_secs(1),
        CancellationToken::new(),
        budget.clone(),
        executor(ExecutorKind::SidecallIo),
        executor(ExecutorKind::Compute),
        executor(ExecutorKind::BlockingControl),
    );
    let retained = Arc::new(Mutex::new(None));
    let mut machine = DirectionMachine::decoder_with_context(
        StreamId(21),
        ScopeId(21),
        ScopeKind::LogicalRequest,
        vec![Box::new(PromoteFilter {
            retained: Arc::clone(&retained),
        })],
        1,
        Box::new(BoundedBodyRetention::new(128)),
        Box::new(FakeFramingLedger::default()),
        FilterCallbackContext::with_services(services),
    )
    .unwrap();
    let baseline = budget.snapshot().unwrap();
    machine.on_headers(HeaderMap::new(), false).await.unwrap();
    machine
        .on_data(Bytes::from_static(b"held"), false)
        .await
        .unwrap();
    assert_eq!(
        budget.snapshot().unwrap().role_live[MemoryRole::SemanticState as usize],
        baseline.role_live[MemoryRole::SemanticState as usize] + 4
    );
    machine.finalize();
    assert_eq!(
        budget.snapshot().unwrap().role_live[MemoryRole::SemanticState as usize],
        baseline.role_live[MemoryRole::SemanticState as usize] + 4,
        "finalizing must retain both machine queue ownership and a promoted clone"
    );
    drop(machine);
    assert_eq!(budget.snapshot().unwrap().live, 4);
    retained.lock().unwrap().take();
    assert_eq!(budget.snapshot().unwrap().live, 0);
}

#[tokio::test]
async fn replacement_emitter_rejects_before_allocating_unbudgeted_metadata() {
    struct ReplaceFilter;

    #[async_trait]
    impl NativeFilter for ReplaceFilter {
        fn name(&self) -> &str {
            "replace"
        }

        fn capabilities(&self) -> FilterCapabilities {
            FilterCapabilities::observe_only().with_body_mutation()
        }

        async fn on_headers(&mut self, _: HeaderInput) -> Result<HeadersAction, FilterError> {
            Ok(HeadersAction::Continue(HeaderPatch::default()))
        }

        async fn on_data(&mut self, input: DataInput<'_>) -> Result<DataAction, FilterError> {
            let _never_allocated = input.body_emitter()?;
            unreachable!("metadata admission must fail before the builder is returned")
        }

        async fn on_trailers(&mut self, _: TrailersInput) -> Result<TrailersAction, FilterError> {
            Ok(TrailersAction::Continue(HeaderPatch::default()))
        }

        fn on_finalize(&mut self) {}
    }

    let budget = BudgetTree::new(256, 256).unwrap().stream(256).unwrap();
    let executor =
        |kind| BoundedExecutor::new(kind, 1, 1).expect("constant emitter executor limits");
    let services = FilterExecutorServices::new(
        Instant::now() + Duration::from_secs(1),
        CancellationToken::new(),
        budget.clone(),
        executor(ExecutorKind::SidecallIo),
        executor(ExecutorKind::Compute),
        executor(ExecutorKind::BlockingControl),
    );
    let mut machine = DirectionMachine::decoder_with_context(
        StreamId(22),
        ScopeId(22),
        ScopeKind::LogicalRequest,
        vec![Box::new(ReplaceFilter)],
        1,
        Box::new(BoundedBodyRetention::new(128)),
        Box::new(FakeFramingLedger::default()),
        FilterCallbackContext::with_services(services),
    )
    .unwrap();
    let baseline = budget.snapshot().unwrap();
    machine.on_headers(HeaderMap::new(), false).await.unwrap();
    assert_eq!(
        machine
            .on_data(Bytes::from_static(b"input"), false)
            .await
            .unwrap_err(),
        FilterError::BodyOutputBudget
    );
    let snapshot = budget.snapshot().unwrap();
    assert_eq!(snapshot.live, baseline.live);
    assert_eq!(snapshot.peak, baseline.peak);
    assert_eq!(snapshot.rejected, baseline.rejected + 1);
    drop(machine);
    assert_eq!(budget.snapshot().unwrap().live, 0);
}

#[tokio::test]
async fn many_empty_replacements_keep_queue_charge_until_output_owner_drops() {
    const UNITS: usize = 257;

    struct EmptyExpansionFilter;

    #[async_trait]
    impl NativeFilter for EmptyExpansionFilter {
        fn name(&self) -> &str {
            "empty-expansion"
        }

        fn capabilities(&self) -> FilterCapabilities {
            FilterCapabilities::observe_only().with_body_expansion()
        }

        async fn on_headers(&mut self, _: HeaderInput) -> Result<HeadersAction, FilterError> {
            Ok(HeadersAction::Continue(HeaderPatch::default()))
        }

        async fn on_data(&mut self, input: DataInput<'_>) -> Result<DataAction, FilterError> {
            let mut emitter = input.body_emitter()?;
            for _ in 0..UNITS {
                emitter.emit_copy(&[])?;
            }
            let output = emitter.finish();
            Ok(DataAction::Emit {
                output: FilterBodyEmission::Replace(output),
                patch: HeaderPatch::default(),
            })
        }

        async fn on_trailers(&mut self, _: TrailersInput) -> Result<TrailersAction, FilterError> {
            Ok(TrailersAction::Continue(HeaderPatch::default()))
        }

        fn on_finalize(&mut self) {}
    }

    let budget = BudgetTree::new(1 << 20, 1 << 20)
        .unwrap()
        .stream(1 << 20)
        .unwrap();
    let executor =
        |kind| BoundedExecutor::new(kind, 1, 1).expect("constant emitter executor limits");
    let services = FilterExecutorServices::new(
        Instant::now() + Duration::from_secs(1),
        CancellationToken::new(),
        budget.clone(),
        executor(ExecutorKind::SidecallIo),
        executor(ExecutorKind::Compute),
        executor(ExecutorKind::BlockingControl),
    );
    let mut machine = DirectionMachine::decoder_with_context(
        StreamId(23),
        ScopeId(23),
        ScopeKind::LogicalRequest,
        vec![Box::new(EmptyExpansionFilter)],
        UNITS,
        Box::new(BoundedBodyRetention::new(128)),
        Box::new(FakeFramingLedger::default()),
        FilterCallbackContext::with_services(services),
    )
    .unwrap();
    let baseline = budget.snapshot().unwrap();
    machine.on_headers(HeaderMap::new(), false).await.unwrap();
    machine.on_data(Bytes::new(), false).await.unwrap();

    let retained = budget.snapshot().unwrap();
    assert!(
        retained.role_live[MemoryRole::SemanticState as usize]
            > baseline.role_live[MemoryRole::SemanticState as usize],
        "the exhausted output iterator must leave its queue charge with emitted units"
    );
    assert!(
        retained.role_peak[MemoryRole::SemanticState as usize]
            >= retained.role_live[MemoryRole::SemanticState as usize]
    );
    drop(machine);
    assert_eq!(budget.snapshot().unwrap().live, 0);
}
