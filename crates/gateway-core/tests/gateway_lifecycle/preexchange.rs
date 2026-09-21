use crate::fixture::*;

#[tokio::test]
async fn materialized_attempt_filter_failure_finalizes_before_fallback_selection()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"filter-fallback-ok", false).await
    });

    let plan = PlanRevision(7509);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 509)
            .route(
                "gateway.test",
                "/materialized-filter-failure",
                1,
                plain_target(first_address, 509),
            )?
            .route(
                "gateway.test",
                "/materialized-filter-fallback",
                2,
                plain_target(second_address, 510),
            )?
            .route_candidates(1, [1, 2])?
            .attempt_request_filters(
                1,
                Arc::from([compiled_filter("attempt-request-head-failure")]),
            )?,
    )?;
    let completion_trace = Arc::new(Mutex::new(Vec::new()));
    let selection = TestSelection::new([first_binding, second_binding])
        .with_preexchange_completion_trace(Arc::clone(&completion_trace));
    let provider = PassthroughProvider {
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..PassthroughProvider::default()
    };
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let finalized_without_readiness = Arc::clone(&provider.finalized_without_readiness);
    let filters = TrackingFilters {
        fail_attempt_request_head: true,
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..TrackingFilters::default()
    };
    let gateway =
        lifecycle_with_provider(publications, selection.clone(), provider, filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/materialized-filter-failure",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"filter-fallback-ok");
    assert_eq!(selection.selected(), 2);
    assert_eq!(
        selection.published(),
        [Disposition::Continue, Disposition::Accept]
    );
    assert_eq!(
        selection.failures(),
        [AttemptFailureClass::AttemptRequestFilter]
    );
    assert_eq!(
        selection.trace(),
        [
            SelectionTrace::Select,
            SelectionTrace::Complete,
            SelectionTrace::Select,
            SelectionTrace::Complete,
        ]
    );
    {
        let trace = completion_trace
            .lock()
            .expect("pre-exchange completion trace");
        assert!(trace.len() >= 5);
        assert_eq!(
            &trace[..5],
            [
                PreexchangeCompletionStep::Select,
                PreexchangeCompletionStep::AttemptFilterCleanup,
                PreexchangeCompletionStep::ProviderFinalize,
                PreexchangeCompletionStep::ObserveCompleted,
                PreexchangeCompletionStep::Select,
            ]
        );
    }
    let completed = selection.completed();
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].cleanup, AttemptCleanupOutcome::Completed);
    assert_eq!(
        completed[0].termination_reason,
        AttemptTerminationReason::PreexchangeFailure
    );
    assert_eq!(completed[0].stream, AttemptStreamOutcome::NotStarted);
    assert!(
        completed[0]
            .provider
            .as_ref()
            .and_then(|facts| facts.ended_at)
            .is_some()
    );
    assert_eq!(filters.attempt_request_head.load(Ordering::Relaxed), 1);
    assert_eq!(filters.attempt_cleanup.load(Ordering::Relaxed), 2);
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 2);
    assert_eq!(finalized_without_readiness.load(Ordering::Relaxed), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), first_listener.accept())
            .await
            .is_err(),
        "AttemptRequest filter failure must happen before connect"
    );
    assert!(!second_server.await??);
    Ok(())
}

#[tokio::test]
async fn materialized_attempt_filter_failure_terminate_finalizes_exactly_once()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let plan = PlanRevision(7510);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 510)
            .route(
                "gateway.test",
                "/materialized-filter-terminate",
                1,
                plain_target(address, 511),
            )?
            .attempt_request_filters(
                1,
                Arc::from([compiled_filter("attempt-request-head-failure")]),
            )?,
    )?;
    let completion_trace = Arc::new(Mutex::new(Vec::new()));
    let selection = TestSelection::new([binding])
        .with_preexchange_completion_trace(Arc::clone(&completion_trace));
    selection.force_disposition(Disposition::Terminate);
    let provider = PassthroughProvider {
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..PassthroughProvider::default()
    };
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let finalized_without_readiness = Arc::clone(&provider.finalized_without_readiness);
    let filters = TrackingFilters {
        fail_attempt_request_head: true,
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..TrackingFilters::default()
    };
    let gateway =
        lifecycle_with_provider(publications, selection.clone(), provider, filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/materialized-filter-terminate",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(
        session.response_head.as_ref().map(|head| head.status),
        Some(StatusCode::BAD_GATEWAY)
    );
    assert_eq!(session.response_body, b"upstream attempt failed");
    assert!(session.response_eos);
    assert_eq!(selection.published(), [Disposition::Terminate]);
    assert_eq!(
        completion_trace
            .lock()
            .expect("pre-exchange completion trace")
            .as_slice(),
        [
            PreexchangeCompletionStep::Select,
            PreexchangeCompletionStep::AttemptFilterCleanup,
            PreexchangeCompletionStep::ProviderFinalize,
            PreexchangeCompletionStep::ObserveCompleted,
        ]
    );
    assert_eq!(filters.attempt_cleanup.load(Ordering::Relaxed), 1);
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 1);
    assert_eq!(finalized_without_readiness.load(Ordering::Relaxed), 1);
    let completed = selection.completed();
    assert_eq!(completed.len(), 1);
    assert_eq!(
        completed[0].termination_reason,
        AttemptTerminationReason::PreexchangeFailure
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "terminated AttemptRequest filter failure must happen before connect"
    );
    Ok(())
}

#[tokio::test]
async fn exchange_construction_deadline_completes_materialized_attempt_before_fallback()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"constructor-fallback-ok", false).await
    });

    let plan = PlanRevision(7511);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 511)
            .route(
                "gateway.test",
                "/constructor-deadline",
                1,
                plain_target(first_address, 512),
            )?
            .route(
                "gateway.test",
                "/constructor-deadline-fallback",
                2,
                plain_target(second_address, 513),
            )?
            .route_candidates(1, [1, 2])?
            .attempt_request_filters(
                1,
                Arc::from([compiled_filter("attempt-request-constructor-deadline")]),
            )?,
    )?;
    let completion_trace = Arc::new(Mutex::new(Vec::new()));
    let selection = TestSelection::new([first_binding, second_binding])
        .with_attempt_budgets([Duration::from_secs(1), Duration::from_secs(3)])
        .with_preexchange_completion_trace(Arc::clone(&completion_trace));
    let provider = PassthroughProvider {
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..PassthroughProvider::default()
    };
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let finalized_without_readiness = Arc::clone(&provider.finalized_without_readiness);
    let filters = TrackingFilters {
        attempt_request_eos_blocking_delay: Some(Duration::from_millis(1_200)),
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..TrackingFilters::default()
    };
    let gateway =
        lifecycle_with_provider(publications, selection.clone(), provider, filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/constructor-deadline",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"constructor-fallback-ok");
    assert_eq!(selection.selected(), 2);
    assert_eq!(
        selection.published(),
        [Disposition::Continue, Disposition::Accept]
    );
    assert_eq!(selection.failures(), [AttemptFailureClass::AttemptDeadline]);
    assert_eq!(
        selection.trace(),
        [
            SelectionTrace::Select,
            SelectionTrace::Complete,
            SelectionTrace::Select,
            SelectionTrace::Complete,
        ]
    );
    assert_eq!(
        &completion_trace
            .lock()
            .expect("pre-exchange completion trace")[..5],
        [
            PreexchangeCompletionStep::Select,
            PreexchangeCompletionStep::AttemptFilterCleanup,
            PreexchangeCompletionStep::ProviderFinalize,
            PreexchangeCompletionStep::ObserveCompleted,
            PreexchangeCompletionStep::Select,
        ]
    );
    let completed = selection.completed();
    assert_eq!(completed.len(), 2);
    assert_eq!(
        completed[0].transport.timeout,
        Some(AttemptTimeoutKind::AttemptDeadline)
    );
    assert_eq!(completed[0].stream, AttemptStreamOutcome::NotStarted);
    assert_eq!(filters.attempt_cleanup.load(Ordering::Relaxed), 2);
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 2);
    assert_eq!(finalized_without_readiness.load(Ordering::Relaxed), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), first_listener.accept())
            .await
            .is_err(),
        "an expired constructor must not connect its candidate"
    );
    assert!(!second_server.await??);
    Ok(())
}

#[tokio::test]
async fn exchange_construction_deadline_terminate_cleans_and_finalizes_exactly_once()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let plan = PlanRevision(7512);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 512)
            .route(
                "gateway.test",
                "/constructor-deadline-terminate",
                1,
                plain_target(address, 514),
            )?
            .attempt_request_filters(
                1,
                Arc::from([compiled_filter("attempt-request-constructor-terminate")]),
            )?,
    )?;
    let completion_trace = Arc::new(Mutex::new(Vec::new()));
    let selection = TestSelection::new([binding])
        .with_attempt_budget(Duration::from_secs(1))
        .with_preexchange_completion_trace(Arc::clone(&completion_trace));
    selection.force_disposition(Disposition::Terminate);
    let provider = PassthroughProvider {
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..PassthroughProvider::default()
    };
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let finalized_without_readiness = Arc::clone(&provider.finalized_without_readiness);
    let filters = TrackingFilters {
        attempt_request_eos_blocking_delay: Some(Duration::from_millis(1_200)),
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..TrackingFilters::default()
    };
    let gateway =
        lifecycle_with_provider(publications, selection.clone(), provider, filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/constructor-deadline-terminate",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"upstream attempt failed");
    assert_eq!(selection.selected(), 1);
    assert_eq!(selection.published(), [Disposition::Terminate]);
    assert_eq!(selection.failures(), [AttemptFailureClass::AttemptDeadline]);
    assert_eq!(
        completion_trace
            .lock()
            .expect("pre-exchange completion trace")
            .as_slice(),
        [
            PreexchangeCompletionStep::Select,
            PreexchangeCompletionStep::AttemptFilterCleanup,
            PreexchangeCompletionStep::ProviderFinalize,
            PreexchangeCompletionStep::ObserveCompleted,
        ]
    );
    assert_eq!(selection.completed().len(), 1);
    assert_eq!(filters.attempt_cleanup.load(Ordering::Relaxed), 1);
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 1);
    assert_eq!(finalized_without_readiness.load(Ordering::Relaxed), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "terminated constructor failure must remain pre-connect"
    );
    Ok(())
}

#[tokio::test]
async fn response_prefix_budget_failure_aborts_request_after_cleanup_and_finalizer()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let plan = PlanRevision(7513);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 513)
            .route(
                "gateway.test",
                "/response-prefix-budget",
                1,
                plain_target(first_address, 515),
            )?
            .route(
                "gateway.test",
                "/response-prefix-budget-fallback",
                2,
                plain_target(second_address, 516),
            )?
            .route_candidates(1, [1, 2])?
            .attempt_request_filters(
                1,
                Arc::from([compiled_filter("attempt-request-before-budget")]),
            )?
            .precommit_event_capacity(1, 1_000_000)?,
    )?;
    let completion_trace = Arc::new(Mutex::new(Vec::new()));
    let selection = TestSelection::new([first_binding, second_binding])
        .with_preexchange_completion_trace(Arc::clone(&completion_trace));
    let provider = PassthroughProvider {
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..PassthroughProvider::default()
    };
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let finalized_without_readiness = Arc::clone(&provider.finalized_without_readiness);
    let filters = TrackingFilters {
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..TrackingFilters::default()
    };
    let gateway = lifecycle_with_provider_and_limits(
        publications,
        selection.clone(),
        provider,
        filters.clone(),
        GatewayCoreLifecycleLimits {
            max_request_body_bytes: 64 * 1024,
            write_quantum: 4,
            bootstrap_hard_cap: Some(Duration::from_secs(5)),
            ..GatewayCoreLifecycleLimits::default()
        },
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/response-prefix-budget",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    let result = gateway.process(&mut session).await;
    assert!(
        result.is_err(),
        "shared response-prefix budget exhaustion must abort the request: result={result:?}, selected={}, published={:?}, failures={:?}, body={:?}",
        selection.selected(),
        selection.published(),
        selection.failures(),
        session.response_body,
    );
    let error = result.expect_err("error was checked above");
    assert!(error.to_string().contains("body budget exceeded"));
    assert_eq!(selection.selected(), 1);
    assert!(selection.published().is_empty());
    assert!(selection.completed().is_empty());
    assert_eq!(selection.trace(), [SelectionTrace::Select]);
    assert_eq!(
        completion_trace
            .lock()
            .expect("pre-exchange completion trace")
            .as_slice(),
        [
            PreexchangeCompletionStep::Select,
            PreexchangeCompletionStep::AttemptFilterCleanup,
            PreexchangeCompletionStep::ProviderFinalize,
        ],
        "request-global failure finalizes provider state without fabricating publication"
    );
    assert_eq!(filters.attempt_cleanup.load(Ordering::Relaxed), 1);
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 1);
    assert_eq!(finalized_without_readiness.load(Ordering::Relaxed), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), first_listener.accept())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second_listener.accept())
            .await
            .is_err(),
        "request-global budget exhaustion must not select or connect a fallback"
    );
    Ok(())
}

#[tokio::test]
async fn exchange_construction_equal_overall_deadline_is_request_global() -> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let plan = PlanRevision(7514);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 514)
            .route(
                "gateway.test",
                "/constructor-overall-deadline",
                1,
                plain_target(first_address, 517),
            )?
            .route(
                "gateway.test",
                "/constructor-overall-deadline-fallback",
                2,
                plain_target(second_address, 518),
            )?
            .route_candidates(1, [1, 2])?
            .attempt_request_filters(
                1,
                Arc::from([compiled_filter("attempt-request-overall-deadline")]),
            )?,
    )?;
    let completion_trace = Arc::new(Mutex::new(Vec::new()));
    // With no narrower attempt grant, TestDecisionSession clamps the default
    // Attempt deadline to the same absolute instant as the bootstrap hard cap.
    let selection = TestSelection::new([first_binding, second_binding])
        .with_preexchange_completion_trace(Arc::clone(&completion_trace));
    let provider = PassthroughProvider {
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..PassthroughProvider::default()
    };
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let finalized_without_readiness = Arc::clone(&provider.finalized_without_readiness);
    let filters = TrackingFilters {
        attempt_request_eos_blocking_delay: Some(Duration::from_millis(1_200)),
        preexchange_completion_trace: Some(Arc::clone(&completion_trace)),
        ..TrackingFilters::default()
    };
    let gateway = lifecycle_with_provider_and_limits(
        publications,
        selection.clone(),
        provider,
        filters.clone(),
        GatewayCoreLifecycleLimits {
            max_request_body_bytes: 64 * 1024,
            write_quantum: 4,
            bootstrap_hard_cap: Some(Duration::from_secs(1)),
            ..GatewayCoreLifecycleLimits::default()
        },
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/constructor-overall-deadline",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    let error = gateway
        .process(&mut session)
        .await
        .expect_err("overall deadline expiry must abort the request");
    assert!(error.to_string().contains("request deadline exceeded"));
    assert_eq!(selection.selected(), 1);
    assert!(selection.failures().is_empty());
    assert!(selection.published().is_empty());
    assert!(selection.completed().is_empty());
    assert_eq!(selection.trace(), [SelectionTrace::Select]);
    assert_eq!(
        completion_trace
            .lock()
            .expect("pre-exchange completion trace")
            .as_slice(),
        [
            PreexchangeCompletionStep::Select,
            PreexchangeCompletionStep::AttemptFilterCleanup,
            PreexchangeCompletionStep::ProviderFinalize,
        ]
    );
    assert_eq!(filters.attempt_cleanup.load(Ordering::Relaxed), 1);
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 1);
    assert_eq!(finalized_without_readiness.load(Ordering::Relaxed), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), first_listener.accept())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second_listener.accept())
            .await
            .is_err(),
        "an overall deadline must not select or connect a fallback"
    );
    Ok(())
}

#[tokio::test]
async fn credential_materialization_failure_is_decided_before_connect_then_falls_back()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"credential-fallback-ok", false).await
    });

    let plan = PlanRevision(7503);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 53)
            .route(
                "gateway.test",
                "/credential-fallback",
                1,
                plain_target(first_address, 55),
            )?
            .route(
                "gateway.test",
                "/credential-fallback-target",
                2,
                plain_target(second_address, 56),
            )?
            .route_candidates(1, [1, 2])?,
    )?;
    let selection = TestSelection::new([first_binding, second_binding]);
    let provider = PassthroughProvider {
        materialization_failure: Some(AttemptMaterializationFailure {
            class: AttemptMaterializationFailureClass::Credential,
            provider: ProviderClassificationFacts {
                error_class: Some(ObservationLabel::new("credential_unavailable").unwrap()),
                retryability: RetryabilityFact::Retryable,
                readiness: ObservationLabel::new("credential_not_ready").unwrap(),
                ..ProviderClassificationFacts::default()
            },
            termination_reason: ObservationLabel::new("credential_unavailable").unwrap(),
        }),
        materialization_failures_remaining: Arc::new(AtomicUsize::new(1)),
        materialization_raw_error: Some(Arc::from(
            "{\"Authorization\":\"Bearer sk-provider-private-secret\"}\nraw detail",
        )),
        ..PassthroughProvider::default()
    };
    let classified_materialization_errors = Arc::clone(&provider.classified_materialization_errors);
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let gateway = lifecycle_with_provider(
        publications,
        selection.clone(),
        provider,
        TrackingFilters::default(),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/credential-fallback",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"credential-fallback-ok");
    assert_eq!(selection.selected(), 2);
    assert_eq!(selection.failures(), [AttemptFailureClass::Credential]);
    assert_eq!(
        selection.published(),
        [Disposition::Continue, Disposition::Accept]
    );
    assert_eq!(
        selection.trace(),
        [
            SelectionTrace::Select,
            SelectionTrace::Complete,
            SelectionTrace::Select,
            SelectionTrace::Complete,
        ],
        "pre-exchange lease cleanup must finish before fallback selection"
    );
    let completed = selection.completed();
    let first_failure = completed[0]
        .failure
        .as_ref()
        .expect("pre-exchange failure facts");
    assert_eq!(
        first_failure.termination_reason.as_str(),
        "credential_unavailable"
    );
    assert!(
        !format!("{first_failure:?}").contains("sk-provider-private-secret"),
        "raw provider errors must not enter decision or completion facts"
    );
    assert_eq!(
        classified_materialization_errors
            .lock()
            .expect("classified materialization errors")
            .as_slice(),
        [Arc::<str>::from(
            "{\"Authorization\":\"Bearer sk-provider-private-secret\"}\nraw detail"
        )],
        "the adapter may inspect its private raw error before returning a safe label"
    );
    assert_eq!(
        completed[0].termination_reason,
        AttemptTerminationReason::PreexchangeFailure
    );
    assert_eq!(completed[0].stream, AttemptStreamOutcome::NotStarted);
    assert_eq!(
        finalized_attempts.load(Ordering::Relaxed),
        1,
        "a failed materialization has no AttemptState; only the successful fallback is finalized"
    );
    let completed_route_decisions = selection.completed_route_decisions();
    assert_eq!(completed_route_decisions.len(), 2);
    assert!(
        completed_route_decisions
            .iter()
            .all(|decision| *decision == completed_route_decisions[0])
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), first_listener.accept())
            .await
            .is_err(),
        "credential failure must be classified before DNS/connect"
    );
    assert!(!second_server.await??);
    Ok(())
}

#[tokio::test]
async fn preexchange_attempt_deadline_is_a_session_owned_failure_not_request_cancellation()
-> Result<(), TestError> {
    let plan = PlanRevision(7504);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(BootstrapPublicationBuilder::new(plan.0, 54).route(
        "gateway.test",
        "/materialization-attempt-deadline",
        1,
        plain_target("127.0.0.1:9".parse()?, 57),
    )?)?;
    let selection = TestSelection::new([binding]).with_attempt_budget(Duration::from_millis(25));
    let provider = PassthroughProvider {
        materialize_delay: Some(Duration::from_secs(5)),
        ..PassthroughProvider::default()
    };
    let filters = TrackingFilters::default();
    let gateway =
        lifecycle_with_provider(publications, selection.clone(), provider, filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/materialization-attempt-deadline",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );
    let cancellation = session.cancellation_handle();
    let started = Instant::now();

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        session.response_head.as_ref().map(|head| head.status),
        Some(StatusCode::BAD_GATEWAY)
    );
    assert_eq!(session.response_body, b"upstream attempt failed");
    assert_eq!(selection.selected(), 1);
    assert_eq!(selection.failures(), [AttemptFailureClass::AttemptDeadline]);
    assert_eq!(selection.published(), [Disposition::Terminate]);
    assert_eq!(selection.completed_route_decisions().len(), 1);
    assert!(!cancellation.is_cancelled());
    assert_eq!(filters.finalized.load(Ordering::Relaxed), 1);
    Ok(())
}
