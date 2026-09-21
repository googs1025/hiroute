use crate::fixture::*;

#[tokio::test]
async fn stale_fallback_generation_is_rejected_before_materialization_or_connect()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let plan = PlanRevision(7501);
    let first = ResolvedTargetBindingId::new(plan, 1);
    let second = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 5)
            .route(
                "gateway.test",
                "/stale-fallback-generation",
                1,
                plain_target(first_listener.local_addr()?, 51),
            )?
            .route(
                "gateway.test",
                "/unused-stale-binding",
                2,
                plain_target(second_listener.local_addr()?, 52),
            )?
            .route_candidates(1, [1, 2])?
            .body_plans(1, attempt_request_local_reply_body_plans())?
            .attempt_request_filters(1, Arc::from([compiled_filter("attempt-request-reply")]))?,
    )?;
    let selection = TestSelection::new([first, second])
        .with_generation_overrides([None, Some(AttemptGeneration(1))]);
    let filters = TrackingFilters::default();
    *filters
        .attempt_request_reply
        .lock()
        .expect("attempt request reply") = Some(LocalReply {
        status: StatusCode::SERVICE_UNAVAILABLE,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"retry with stale generation"),
        provenance: SemanticProvenance::NonSemantic,
    });
    let provider = PassthroughProvider {
        local_reply_classification: Some(LocalReplyClassification::Retryable),
        ..PassthroughProvider::default()
    };
    let materialized_attempts = Arc::clone(&provider.materialized_attempts);
    let gateway = lifecycle_with_provider(publications, selection.clone(), provider, filters)?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/stale-fallback-generation",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(
        gateway.process(&mut session).await.unwrap_err(),
        TransportError::Io(Arc::from(
            GatewayExecutionError::InvalidSelection.to_string()
        )),
    );
    assert_eq!(selection.selected(), 2);
    assert_eq!(selection.published(), [Disposition::Continue]);
    assert_eq!(selection.completed().len(), 1);
    assert_eq!(
        selection.trace(),
        [
            SelectionTrace::Select,
            SelectionTrace::Complete,
            SelectionTrace::Select,
        ],
    );
    assert_eq!(materialized_attempts.load(Ordering::Relaxed), 1);
    assert!(session.response_head.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), first_listener.accept())
            .await
            .is_err(),
        "the local Continue attempt must not connect",
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), second_listener.accept())
            .await
            .is_err(),
        "the stale fallback must be rejected before connect",
    );
    Ok(())
}

#[tokio::test]
async fn early_continue_resets_first_pingora_attempt_before_explicit_second_selection()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let (reset_tx, reset_rx) = oneshot::channel();
    let first_server = tokio::spawn(async move {
        let (socket, _) = first_listener.accept().await?;
        let reset = serve_h1_once(socket, StatusCode::TOO_MANY_REQUESTS, b"retry", true).await?;
        let _ = reset_tx.send(reset);
        Ok::<(), TestError>(())
    });

    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"second-ok", false).await
    });

    let plan = PlanRevision(75);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 5)
            .route("gateway.test", "/retry", 1, plain_target(first_address, 5))?
            .route(
                "gateway.test",
                "/fallback-only",
                2,
                plain_target(second_address, 6),
            )?
            .route_candidates(1, [1, 2])?,
    )?;
    let selection = TestSelection::new([first_binding, second_binding]);
    let provider = PassthroughProvider::default();
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let gateway = lifecycle_with_provider(
        publications,
        selection.clone(),
        provider,
        TrackingFilters::default(),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/retry",
        Bytes::from(vec![b'x'; 32 * 1024]),
        HttpProtocol::Http1,
    );
    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"second-ok");
    assert_eq!(selection.selected(), 2);
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
        "Continue cleanup and completion must precede the next selection"
    );
    let completed = selection.completed();
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].cleanup, AttemptCleanupOutcome::Completed);
    assert_eq!(
        completed[0].termination_reason,
        AttemptTerminationReason::FallbackComplete
    );
    let rate_limit = completed[0]
        .provider
        .as_ref()
        .expect("classified 429 facts");
    assert_eq!(rate_limit.http_status, Some(StatusCode::TOO_MANY_REQUESTS));
    assert_eq!(rate_limit.retry_after, Some(Duration::from_secs(2)));
    assert!(rate_limit.reset_at.is_some());
    assert_eq!(
        rate_limit
            .provider_code
            .as_ref()
            .map(ObservationLabel::as_str),
        Some("rate_limit_exceeded")
    );
    assert_eq!(
        rate_limit
            .provider_request_id
            .as_ref()
            .map(ObservationLabel::as_str),
        Some("provider-request-test")
    );
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 2);
    assert!(
        reset_rx.await?,
        "first H1 attempt should observe cancel/close"
    );
    first_server.await??;
    assert!(!second_server.await??);
    Ok(())
}

#[tokio::test]
async fn classified_readiness_survives_writer_gate_deadline_until_fallback_completion()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    // Bound the accepted socket's receive window explicitly. Relying on the
    // host TCP autotuning limit lets the full request fit on some kernels and
    // turns this writer-gate scenario into an accepted streaming response.
    let first_socket = tokio::net::TcpSocket::new_v4()?;
    first_socket.set_recv_buffer_size(16 * 1024)?;
    first_socket.bind("127.0.0.1:0".parse()?)?;
    let first_listener = first_socket.listen(1)?;
    let first_address = first_listener.local_addr()?;
    let first_server = tokio::spawn(async move {
        let (socket, _) = first_listener.accept().await?;
        serve_h1_early_sse_without_reading_body(socket, Duration::from_secs(3)).await
    });
    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"data: fallback-ok\n\n", false).await
    });

    let plan = PlanRevision(7502);
    let first = ResolvedTargetBindingId::new(plan, 1);
    let second = ResolvedTargetBindingId::new(plan, 2);
    let sse_plan = BodyPlan::SseFramedStreaming {
        max_event_bytes: 64,
        max_pending_bytes: 64,
        max_output_event_bytes: 64,
        expansion_ratio_numerator: 1,
        expansion_ratio_denominator: 1,
        expansion_slack_bytes: 0,
    };
    let request_limit = 3 * 1024 * 1024;
    let body_plans = BootstrapBodyPlans {
        logical_request: BodyPlan::BufferedTransform {
            max_body_bytes: request_limit,
        },
        attempt_request: BodyPlan::StreamingReplay {
            max_chunk_bytes: 16 * 1024,
            max_replay_bytes: request_limit,
        },
        attempt_response_precommit: sse_plan.clone(),
        accepted_response: sse_plan,
    };
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 52)
            .route(
                "gateway.test",
                "/classified-gate-deadline",
                1,
                plain_target(first_address, 111),
            )?
            .route(
                "gateway.test",
                "/classified-gate-deadline-fallback",
                2,
                plain_target(second_address, 112),
            )?
            .body_plans(1, body_plans.clone())?
            .body_plans(2, body_plans)?
            .route_candidates(1, [1, 2])?,
    )?;
    let selection = TestSelection::new([first, second])
        .with_attempt_budget(Duration::from_millis(1_500))
        .with_blocked_replacement(Disposition::Continue);
    let provisional_usage = UsageFact {
        input: UsageDimension::reported(31),
        billable: UsageDimension::reported(31),
        ..UsageFact::default()
    };
    let provider = PassthroughProvider {
        accept_on_first_semantic_sse: true,
        classification_usage: Some(provisional_usage),
        ..PassthroughProvider::default()
    };
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let gateway = lifecycle_with_provider_and_limits(
        publications,
        selection.clone(),
        provider,
        TrackingFilters::default(),
        GatewayCoreLifecycleLimits {
            max_request_body_bytes: request_limit,
            stream_memory_bytes: 32 * 1024 * 1024,
            bootstrap_hard_cap: Some(Duration::from_secs(12)),
            ..GatewayCoreLifecycleLimits::default()
        },
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/classified-gate-deadline",
        Bytes::from(vec![b'p'; request_limit]),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    let observed_selected = selection.selected();
    let observed_published = selection.published();
    let observed_failures = selection.failures();
    assert_eq!(
        session.response_body, b"data: fallback-ok\n\n",
        "selected={observed_selected}, published={observed_published:?}, failures={observed_failures:?}"
    );
    assert_eq!(selection.selected(), 2);
    assert_eq!(
        selection.trace(),
        [
            SelectionTrace::Select,
            SelectionTrace::Complete,
            SelectionTrace::Select,
            SelectionTrace::Complete,
        ]
    );
    assert_eq!(selection.blocked(), 1);
    assert!(selection.failures().is_empty());
    let completed = selection.completed();
    assert_eq!(completed.len(), 2);
    let first_facts = completed[0]
        .provider
        .as_ref()
        .expect("classified facts survive gate failure");
    assert!(first_facts.ttft.is_some());
    assert_eq!(
        first_facts
            .usage
            .expect("provisional usage survives")
            .input
            .units,
        Some(31)
    );
    let selection_history = selection.selection_history();
    assert!(selection_history[0].is_empty());
    assert_eq!(selection_history[1].len(), 1);
    let facts_seen_before_fallback = selection_history[1][0]
        .provider
        .as_ref()
        .expect("completion facts are visible to fallback selection");
    assert!(facts_seen_before_fallback.ttft.is_some());
    assert_eq!(
        facts_seen_before_fallback
            .usage
            .expect("fallback sees classified usage")
            .input
            .units,
        Some(31)
    );
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 2);
    let (received, expected) = first_server.await??;
    assert!(
        received < expected,
        "the first request writer must still be blocked when its gate grant expires"
    );
    assert!(!second_server.await??);
    Ok(())
}

#[tokio::test]
async fn published_release_failure_finalizes_staged_provider_state_without_fallback()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let first_server = tokio::spawn(async move {
        let (socket, _) = first_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"classified", false).await
    });
    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let plan = PlanRevision(7503);
    let first = ResolvedTargetBindingId::new(plan, 1);
    let second = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 53)
            .route(
                "gateway.test",
                "/release-failure",
                1,
                plain_target(first_address, 113),
            )?
            .route(
                "gateway.test",
                "/release-failure-fallback",
                2,
                plain_target(second_address, 114),
            )?
            .route_candidates(1, [1, 2])?,
    )?;
    let selection = TestSelection::new([first, second]);
    selection.force_disposition(Disposition::Terminate);
    let readiness_usage = UsageFact {
        billable: UsageDimension::reported(9),
        ..UsageFact::default()
    };
    let provider = PassthroughProvider {
        fail_terminal_request_release: true,
        readiness_completion_usage: Some(readiness_usage),
        ..PassthroughProvider::default()
    };
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let gateway = lifecycle_with_provider(
        publications,
        selection.clone(),
        provider,
        TrackingFilters::default(),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/release-failure",
        Bytes::new(),
        HttpProtocol::Http1,
    );

    let error = gateway.process(&mut session).await.unwrap_err();
    assert!(error.to_string().contains("request release failure"));
    assert_eq!(selection.selected(), 1);
    assert_eq!(selection.published(), [Disposition::Terminate]);
    let completed = selection.completed();
    assert_eq!(completed.len(), 1);
    assert_eq!(
        completed[0].termination_reason,
        AttemptTerminationReason::ProviderReleaseFailure,
        "request-owner release is a distinct mechanical phase from response encoding"
    );
    assert_eq!(
        completed[0]
            .provider
            .as_ref()
            .and_then(|facts| facts.usage)
            .map(|usage| usage.billable.units),
        Some(Some(9))
    );
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second_listener.accept())
            .await
            .is_err(),
        "a published terminal disposition cannot re-enter fallback"
    );
    assert!(!first_server.await??);
    Ok(())
}

#[tokio::test]
async fn compiled_attempt_limit_prevents_an_extra_session_selection_and_connect()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let first_server = tokio::spawn(async move {
        let (socket, _) = first_listener.accept().await?;
        serve_h1_once(socket, StatusCode::TOO_MANY_REQUESTS, b"retry", true).await
    });

    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let (second_connect_tx, second_connect_rx) = oneshot::channel();
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        let _ = second_connect_tx.send(());
        serve_h1_once(socket, StatusCode::OK, b"must-not-run", false).await
    });

    let plan = PlanRevision(7501);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 51)
            .route(
                "gateway.test",
                "/attempt-limit",
                1,
                plain_target(first_address, 51),
            )?
            .route(
                "gateway.test",
                "/unused-candidate",
                2,
                plain_target(second_address, 52),
            )?
            .route_candidates(1, [1, 2])?
            .route_max_attempts(1, 1)?,
    )?;
    let selection = TestSelection::new([first_binding, second_binding]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/attempt-limit",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(
        session.response_head.as_ref().map(|head| head.status),
        Some(StatusCode::BAD_GATEWAY)
    );
    assert_eq!(session.response_body, b"no upstream candidate");
    assert_eq!(selection.selected(), 1);
    assert_eq!(selection.published(), [Disposition::Continue]);
    let _ = first_server.await??;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), second_connect_rx)
            .await
            .is_err(),
        "core must not ask a session for or connect the max+1 attempt"
    );
    second_server.abort();
    let _ = second_server.await;
    Ok(())
}

#[tokio::test]
async fn first_byte_timeout_returns_transport_facts_to_the_session_before_fallback()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let first_server = tokio::spawn(async move {
        let (socket, _) = first_listener.accept().await?;
        serve_h1_after_response_gap(socket, Duration::from_millis(250), b"too-late").await
    });

    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"fallback-ok", false).await
    });

    let plan = PlanRevision(7502);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 52)
            .route(
                "gateway.test",
                "/first-byte-fallback",
                1,
                plain_target(first_address, 53),
            )?
            .route(
                "gateway.test",
                "/fallback-target",
                2,
                plain_target(second_address, 54),
            )?
            .route_candidates(1, [1, 2])?
            .attempt_timeouts(
                1,
                AttemptTimeouts {
                    request_write: Duration::from_secs(1),
                    first_byte: Duration::from_millis(30),
                    stream_idle: Duration::from_secs(1),
                },
            )?,
    )?;
    let selection = TestSelection::new([first_binding, second_binding]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/first-byte-fallback",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"fallback-ok");
    assert_eq!(selection.selected(), 2);
    assert_eq!(
        selection.published(),
        [Disposition::Continue, Disposition::Accept]
    );
    assert_eq!(selection.failures(), [AttemptFailureClass::FirstByte]);
    assert!(!second_server.await??);
    first_server.abort();
    let _ = first_server.await;
    Ok(())
}
