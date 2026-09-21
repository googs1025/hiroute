use crate::fixture::*;

#[tokio::test]
async fn callback_panic_fails_closed_and_all_filters_still_finalize_once() {
    struct PanicFilter {
        finalized: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl NativeFilter for PanicFilter {
        fn name(&self) -> &str {
            "panic"
        }

        async fn on_headers(&mut self, _: HeaderInput) -> Result<HeadersAction, FilterError> {
            panic!("private callback failure")
        }

        async fn on_data(&mut self, _: DataInput<'_>) -> Result<DataAction, FilterError> {
            Ok(DataAction::Continue(HeaderPatch::default()))
        }

        async fn on_trailers(&mut self, _: TrailersInput) -> Result<TrailersAction, FilterError> {
            Ok(TrailersAction::Continue(HeaderPatch::default()))
        }

        fn on_finalize(&mut self) {
            *self.finalized.lock().unwrap() += 1;
        }
    }

    let panic_finalized = Arc::new(Mutex::new(0));
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (following, following_finalized) = ScriptFilter::new(
        "following",
        [ScriptAction::Headers(HeadersAction::Continue(
            HeaderPatch::default(),
        ))],
        trace,
    );
    let mut manager = machine(vec![
        Box::new(PanicFilter {
            finalized: Arc::clone(&panic_finalized),
        }),
        Box::new(following),
    ]);
    assert_eq!(
        manager
            .on_headers(HeaderMap::new(), false)
            .await
            .unwrap_err(),
        FilterError::CallbackPanic
    );
    assert!(manager.callback_panicked());
    assert_eq!(
        manager
            .on_data(Bytes::from_static(b"must-not-run"), false)
            .await
            .unwrap_err(),
        FilterError::MachineTerminal
    );
    manager.finalize();
    manager.finalize();
    assert_eq!(*panic_finalized.lock().unwrap(), 1);
    assert_eq!(*following_finalized.lock().unwrap(), 1);
}

#[tokio::test]
async fn callback_error_is_terminal_and_hanging_callback_obeys_deadline_and_cancel() {
    struct FailingFilter;

    #[async_trait]
    impl NativeFilter for FailingFilter {
        fn name(&self) -> &str {
            "failing"
        }

        async fn on_headers(&mut self, _: HeaderInput) -> Result<HeadersAction, FilterError> {
            Err(FilterError::Callback(Arc::from("expected failure")))
        }

        async fn on_data(&mut self, _: DataInput<'_>) -> Result<DataAction, FilterError> {
            Ok(DataAction::Continue(HeaderPatch::default()))
        }

        async fn on_trailers(&mut self, _: TrailersInput) -> Result<TrailersAction, FilterError> {
            Ok(TrailersAction::Continue(HeaderPatch::default()))
        }

        fn on_finalize(&mut self) {}
    }

    struct HangingFilter;

    #[async_trait]
    impl NativeFilter for HangingFilter {
        fn name(&self) -> &str {
            "hanging"
        }

        async fn on_headers(&mut self, _: HeaderInput) -> Result<HeadersAction, FilterError> {
            std::future::pending().await
        }

        async fn on_data(&mut self, _: DataInput<'_>) -> Result<DataAction, FilterError> {
            std::future::pending().await
        }

        async fn on_trailers(&mut self, _: TrailersInput) -> Result<TrailersAction, FilterError> {
            std::future::pending().await
        }

        fn on_finalize(&mut self) {}
    }

    let mut failing = machine(vec![Box::new(FailingFilter)]);
    assert!(matches!(
        failing.on_headers(HeaderMap::new(), false).await,
        Err(FilterError::Callback(_))
    ));
    assert_eq!(
        failing
            .on_data(Bytes::from_static(b"after-error"), false)
            .await
            .unwrap_err(),
        FilterError::MachineTerminal
    );

    let mut deadline = DirectionMachine::decoder_with_context(
        StreamId(2),
        ScopeId(2),
        ScopeKind::LogicalRequest,
        vec![Box::new(HangingFilter)],
        2,
        Box::new(BoundedBodyRetention::new(16)),
        Box::new(FakeFramingLedger::default()),
        FilterCallbackContext::with_timeout(Duration::from_millis(20)),
    )
    .unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            deadline.on_headers(HeaderMap::new(), false)
        )
        .await
        .expect("callback deadline must be bounded")
        .unwrap_err(),
        FilterError::CallbackDeadline
    );
    assert_eq!(
        deadline.on_data(Bytes::new(), true).await.unwrap_err(),
        FilterError::MachineTerminal
    );

    let cancellation = CancellationToken::new();
    let mut cancelled = DirectionMachine::decoder_with_context(
        StreamId(3),
        ScopeId(3),
        ScopeKind::LogicalRequest,
        vec![Box::new(HangingFilter)],
        2,
        Box::new(BoundedBodyRetention::new(16)),
        Box::new(FakeFramingLedger::default()),
        FilterCallbackContext::new(
            Instant::now() + Duration::from_secs(1),
            cancellation.clone(),
        ),
    )
    .unwrap();
    cancellation.cancel();
    assert_eq!(
        cancelled
            .on_headers(HeaderMap::new(), false)
            .await
            .unwrap_err(),
        FilterError::CallbackCancelled
    );
}

#[tokio::test]
async fn finalize_explicitly_discards_retained_backing_and_unregisters_token() {
    #[derive(Debug)]
    struct SharedRetention {
        next: u64,
        frames: Arc<Mutex<Vec<(RetainedFrameId, Bytes)>>>,
    }

    impl BodyRetentionPort for SharedRetention {
        fn retain(&mut self, bytes: Bytes) -> Result<RetainedFrameId, FilterError> {
            self.next = self.next.wrapping_add(1);
            let id = RetainedFrameId(self.next);
            self.frames.lock().unwrap().push((id, bytes));
            Ok(id)
        }

        fn take(&mut self, id: RetainedFrameId) -> Result<Bytes, FilterError> {
            let mut frames = self.frames.lock().unwrap();
            let position = frames
                .iter()
                .position(|(candidate, _)| *candidate == id)
                .ok_or(FilterError::UnknownRetainedFrame)?;
            Ok(frames.remove(position).1)
        }

        fn retain_charged(&mut self, _bytes: usize) -> Result<RetainedFrameId, FilterError> {
            self.retain(Bytes::new())
        }

        fn release_charged(&mut self, id: RetainedFrameId) -> Result<(), FilterError> {
            self.take(id).map(drop)
        }

        fn set_read_paused(&mut self, _paused: bool) {}
    }

    let trace = Arc::new(Mutex::new(Vec::new()));
    let (filter, _) = ScriptFilter::new(
        "retainer",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::StopIteration {
                retention: hiroute_gateway_core::core::filter::RetentionMode::Buffer,
                patch: HeaderPatch::default(),
            }),
        ],
        trace,
    );
    let frames = Arc::new(Mutex::new(Vec::new()));
    let mut manager = DirectionMachine::decoder(
        StreamId(4),
        ScopeId(4),
        ScopeKind::LogicalRequest,
        vec![Box::new(filter)],
        2,
        Box::new(SharedRetention {
            next: 0,
            frames: Arc::clone(&frames),
        }),
        Box::new(FakeFramingLedger::default()),
    )
    .unwrap();
    manager.on_headers(HeaderMap::new(), false).await.unwrap();
    let token = match manager
        .on_data(Bytes::from_static(b"retained"), false)
        .await
        .unwrap()
    {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected retained pause: {other:?}"),
    };
    assert_eq!(frames.lock().unwrap().len(), 1);
    manager.finalize();
    assert!(frames.lock().unwrap().is_empty());
    assert_eq!(
        manager
            .resume(token, ResumeAction::Continue(HeaderPatch::default()))
            .await
            .unwrap_err(),
        FilterError::ResumeAfterFinalize
    );
}
