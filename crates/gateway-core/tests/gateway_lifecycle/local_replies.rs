use crate::fixture::*;

#[tokio::test]
async fn local_replies_enter_accepted_scope_and_reconcile_no_body_framing() -> Result<(), TestError>
{
    let plan = PlanRevision(81);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 11)
            .route(
                "gateway.test",
                "/local",
                1,
                plain_target("127.0.0.1:9".parse()?, 18),
            )?
            .logical_filters(
                1,
                Arc::from([CompiledFilterDescriptor::new("logical-reply", 1)?
                    .with_capabilities(FilterCapabilities::observe_only().with_local_reply())]),
            )?
            .route_accepted_filters(
                1,
                Arc::from([CompiledFilterDescriptor::new("accepted-track", 1)?]),
            )?,
    )?;
    let selection = TestSelection::new([binding]);
    let filters = TrackingFilters::default();
    *filters.logical_reply.lock().expect("logical reply") = Some(LocalReply {
        status: StatusCode::NO_CONTENT,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"must-not-be-emitted"),
        provenance: SemanticProvenance::NonSemantic,
    });
    let gateway = lifecycle(publications, selection.clone(), filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/local",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Close);
    let response = session.response_head.as_ref().expect("local response head");
    assert_eq!(response.status, StatusCode::NO_CONTENT);
    assert!(!response.headers.contains_key(CONTENT_LENGTH));
    assert!(
        !response
            .headers
            .contains_key(http::header::TRANSFER_ENCODING)
    );
    assert!(session.response_body.is_empty());
    assert!(session.response_eos);
    assert_eq!(selection.selected(), 0);
    assert_eq!(filters.accepted_head.load(Ordering::Relaxed), 1);
    assert_eq!(filters.accepted_body.load(Ordering::Relaxed), 1);
    assert_eq!(filters.finalized.load(Ordering::Relaxed), 1);

    let publications = install_publication(BootstrapPublicationBuilder::new(82, 12).route(
        "other.test",
        "/",
        1,
        plain_target("127.0.0.1:9".parse()?, 19),
    )?)?;
    let no_route_selection = TestSelection::new([]);
    let no_route_filters = TrackingFilters::default();
    let gateway = lifecycle(
        publications,
        no_route_selection.clone(),
        no_route_filters.clone(),
    )?;
    let mut head_request = RecordingSession::new(
        "gateway.test",
        "/missing",
        Bytes::new(),
        HttpProtocol::Http1,
    )
    .with_method(Method::HEAD);

    assert_eq!(
        gateway.process(&mut head_request).await?,
        SessionReuse::Close
    );
    let response = head_request
        .response_head
        .as_ref()
        .expect("HEAD local response");
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert!(!response.headers.contains_key(CONTENT_LENGTH));
    assert!(
        !response
            .headers
            .contains_key(http::header::TRANSFER_ENCODING)
    );
    assert!(head_request.response_body.is_empty());
    assert!(head_request.response_eos);
    assert_eq!(no_route_selection.selected(), 0);
    assert_eq!(no_route_filters.accepted_head.load(Ordering::Relaxed), 0);
    assert_eq!(no_route_filters.finalized.load(Ordering::Relaxed), 1);
    Ok(())
}

#[tokio::test]
async fn attempt_filter_reply_is_provider_normalized_before_selection() -> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"must-not-forward", false).await
    });

    let plan = PlanRevision(83);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 13)
            .route(
                "gateway.test",
                "/attempt-reply",
                1,
                plain_target(address, 20),
            )?
            .attempt_response_filters(1, Arc::from([compiled_filter("attempt-reply")]))?,
    )?;
    let selection = TestSelection::new([binding]);
    let filters = TrackingFilters::default();
    *filters.attempt_reply.lock().expect("attempt reply") = Some(LocalReply {
        status: StatusCode::IM_A_TEAPOT,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"normalized local reply"),
        provenance: SemanticProvenance::ProducesSemantic,
    });
    let provider = PassthroughProvider::default();
    let normalization_count = Arc::clone(&provider.local_reply_normalizations);
    let terminal_head_encodes = Arc::clone(&provider.terminal_head_encodes);
    let terminal_body_encodes = Arc::clone(&provider.terminal_body_encodes);
    let normalized_effects = Arc::clone(&provider.normalized_upstream_effects);
    let encoded_effects = Arc::clone(&provider.encoded_terminal_effects);
    let gateway =
        lifecycle_with_provider(publications, selection.clone(), provider, filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/attempt-reply",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(normalization_count.load(Ordering::Relaxed), 1);
    assert_eq!(terminal_head_encodes.load(Ordering::Relaxed), 1);
    assert_eq!(terminal_body_encodes.load(Ordering::Relaxed), 1);
    assert_eq!(selection.published(), [Disposition::Terminate]);
    assert_eq!(
        session.response_head.as_ref().expect("local head").status,
        StatusCode::IM_A_TEAPOT
    );
    assert_eq!(session.response_body, b"normalized local reply");
    assert!(session.response_eos);
    assert_eq!(filters.accepted_head.load(Ordering::Relaxed), 0);
    assert_eq!(filters.finalized.load(Ordering::Relaxed), 1);
    {
        let normalized_effects = normalized_effects.lock().expect("normalized effects");
        assert_eq!(normalized_effects.len(), 1);
        assert_eq!(normalized_effects[0].semantic_upstream_calls, 1);
        assert_eq!(
            normalized_effects[0].request_fence,
            CommitFence::WriteConfirmed
        );
    }
    {
        let encoded_effects = encoded_effects.lock().expect("encoded effects");
        assert_eq!(encoded_effects.len(), 1);
        assert_eq!(encoded_effects[0].semantic_upstream_calls, 1);
        assert_eq!(encoded_effects[0].reset_count, 1);
    }
    assert!(!server.await??);
    Ok(())
}

#[tokio::test]
async fn attempt_request_body_local_accept_uses_typed_readiness_without_upstream_call()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let plan = PlanRevision(8301);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 13)
            .route(
                "gateway.test",
                "/attempt-request-accept",
                1,
                plain_target(address, 20),
            )?
            .body_plans(1, attempt_request_local_reply_body_plans())?
            .attempt_request_filters(1, Arc::from([compiled_filter("attempt-request-reply")]))?,
    )?;
    let selection = TestSelection::new([binding]);
    let filters = TrackingFilters::default();
    *filters
        .attempt_request_reply
        .lock()
        .expect("attempt request reply") = Some(LocalReply {
        status: StatusCode::IM_A_TEAPOT,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"synthetic accept"),
        provenance: SemanticProvenance::ProducesSemantic,
    });
    let provider = PassthroughProvider {
        local_reply_classification: Some(LocalReplyClassification::Acceptable),
        ..PassthroughProvider::default()
    };
    let normalized_effects = Arc::clone(&provider.normalized_upstream_effects);
    let gateway =
        lifecycle_with_provider(publications, selection.clone(), provider, filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/attempt-request-accept",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(selection.published(), [Disposition::Accept]);
    assert_eq!(
        session.response_head.as_ref().expect("local head").status,
        StatusCode::IM_A_TEAPOT
    );
    assert_eq!(session.response_body, b"synthetic accept");
    assert!(session.response_eos);
    assert_eq!(filters.attempt_request_head.load(Ordering::Relaxed), 1);
    assert!(filters.attempt_request_body.load(Ordering::Relaxed) >= 1);
    {
        let effects = normalized_effects.lock().expect("normalized effects");
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].semantic_upstream_calls, 0);
        assert_eq!(effects[0].connection_sub_attempts, 0);
        assert_eq!(effects[0].request_fence, CommitFence::Clear);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "synthetic Accept must not connect upstream"
    );
    Ok(())
}

#[tokio::test]
async fn attempt_request_body_local_terminate_publishes_without_upstream_call()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let plan = PlanRevision(8302);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 13)
            .route(
                "gateway.test",
                "/attempt-request-terminate",
                1,
                plain_target(address, 20),
            )?
            .body_plans(1, attempt_request_local_reply_body_plans())?
            .attempt_request_filters(1, Arc::from([compiled_filter("attempt-request-reply")]))?,
    )?;
    let selection = TestSelection::new([binding]);
    let filters = TrackingFilters::default();
    *filters
        .attempt_request_reply
        .lock()
        .expect("attempt request reply") = Some(LocalReply {
        status: StatusCode::FORBIDDEN,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"synthetic terminate"),
        provenance: SemanticProvenance::NonSemantic,
    });
    let provider = PassthroughProvider::default();
    let normalized_effects = Arc::clone(&provider.normalized_upstream_effects);
    let gateway =
        lifecycle_with_provider(publications, selection.clone(), provider, filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/attempt-request-terminate",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(selection.published(), [Disposition::Terminate]);
    assert_eq!(session.response_body, b"synthetic terminate");
    {
        let effects = normalized_effects.lock().expect("normalized effects");
        assert_eq!(effects[0].semantic_upstream_calls, 0);
        assert_eq!(effects[0].request_fence, CommitFence::Clear);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "synthetic Terminate must not connect upstream"
    );
    Ok(())
}

#[tokio::test]
async fn attempt_request_body_local_continue_reselects_before_any_upstream_call()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let skipped_listener = TcpListener::bind("127.0.0.1:0").await?;
    let skipped_address = skipped_listener.local_addr()?;
    let served_listener = TcpListener::bind("127.0.0.1:0").await?;
    let served_address = served_listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = served_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"second attempt", false).await
    });
    let plan = PlanRevision(8303);
    let first = ResolvedTargetBindingId::new(plan, 1);
    let second = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 13)
            .route(
                "gateway.test",
                "/attempt-request-continue",
                1,
                plain_target(skipped_address, 20),
            )?
            .route(
                "gateway.test",
                "/unused-second-binding",
                2,
                plain_target(served_address, 21),
            )?
            .route_candidates(1, [1, 2])?
            .body_plans(1, attempt_request_local_reply_body_plans())?
            .attempt_request_filters(1, Arc::from([compiled_filter("attempt-request-reply")]))?,
    )?;
    let selection = TestSelection::new([first, second]);
    let filters = TrackingFilters::default();
    *filters
        .attempt_request_reply
        .lock()
        .expect("attempt request reply") = Some(LocalReply {
        status: StatusCode::SERVICE_UNAVAILABLE,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"retry elsewhere"),
        provenance: SemanticProvenance::NonSemantic,
    });
    let provider = PassthroughProvider {
        local_reply_classification: Some(LocalReplyClassification::Retryable),
        ..PassthroughProvider::default()
    };
    let normalized_effects = Arc::clone(&provider.normalized_upstream_effects);
    let gateway =
        lifecycle_with_provider(publications, selection.clone(), provider, filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/attempt-request-continue",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(
        selection.published(),
        [Disposition::Continue, Disposition::Accept]
    );
    assert_eq!(session.response_body, b"second attempt");
    {
        let effects = normalized_effects.lock().expect("normalized effects");
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].semantic_upstream_calls, 0);
        assert_eq!(effects[0].connection_sub_attempts, 0);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), skipped_listener.accept())
            .await
            .is_err(),
        "Continue candidate must be published before any first-attempt connect"
    );
    assert!(!server.await??);
    Ok(())
}

#[tokio::test]
async fn accepted_body_local_reply_after_head_commit_is_not_recorded_as_accepted_eos()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"upstream body", false).await
    });

    let plan = PlanRevision(84);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 14)
            .route("gateway.test", "/late-reply", 1, plain_target(address, 21))?
            .route_accepted_filters(
                1,
                Arc::from([CompiledFilterDescriptor::new(
                    "accepted-misbehaving-local-reply",
                    8,
                )?]),
            )?,
    )?;
    let selection = TestSelection::new([binding]);
    let filters = TrackingFilters::default();
    *filters
        .accepted_body_reply
        .lock()
        .expect("accepted body reply") = Some(LocalReply {
        status: StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"too late to replace committed head"),
        provenance: SemanticProvenance::ProducesSemantic,
    });
    let gateway = lifecycle(publications, selection.clone(), filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/late-reply",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    let error = gateway.process(&mut session).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("accepted body filter returned a local reply after response head commit")
    );
    assert_eq!(
        session
            .response_head
            .as_ref()
            .expect("committed head")
            .status,
        StatusCode::OK
    );
    assert!(session.response_body.is_empty());
    assert!(!session.response_eos);
    assert_eq!(selection.published(), [Disposition::Accept]);
    assert_eq!(filters.accepted_head.load(Ordering::Relaxed), 1);
    assert_eq!(filters.accepted_body.load(Ordering::Relaxed), 1);
    let completed = selection.completed();
    assert_eq!(completed.len(), 1);
    assert_ne!(
        completed[0].termination_reason,
        AttemptTerminationReason::AcceptedEos
    );
    assert_eq!(
        completed[0].stream,
        AttemptStreamOutcome::AbortedBeforeSemanticCommit
    );
    assert_eq!(filters.finalized.load(Ordering::Relaxed), 1);
    assert!(!server.await??);
    Ok(())
}

#[tokio::test]
async fn accepted_body_resume_local_reply_after_head_commit_is_not_accepted_eos()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"upstream body", false).await
    });

    let plan = PlanRevision(119);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 119)
            .route(
                "gateway.test",
                "/late-resumed-reply",
                1,
                plain_target(address, 119),
            )?
            .body_plans(1, lifecycle_body_plans())?
            .route_accepted_filters(
                1,
                Arc::from([compiled_filter("accepted-body-resume-local-reply")]),
            )?,
    )?;
    let selection = TestSelection::new([binding]);
    let facts = Arc::new(NativeFilterFacts::default());
    let gateway = GatewayCoreLifecycle::new(
        publications,
        selection.clone(),
        PassthroughProvider::default(),
        native_filter_manager(Arc::clone(&facts), &["accepted-body-resume-local-reply"])?,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/late-resumed-reply",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );
    let response_head_writes = Arc::clone(&session.response_head_writes);
    let mut process = Box::pin(gateway.process(&mut session));

    tokio::select! {
        _ = facts.watermark_entered.notified() => {}
        result = &mut process => panic!("accepted body completed before continuation resume: {result:?}"),
    }
    assert_eq!(response_head_writes.load(Ordering::Relaxed), 1);
    facts
        .watermark_continuation
        .lock()
        .expect("accepted body continuation")
        .take()
        .expect("filter retained its accepted body continuation")
        .resume(ResumeAction::LocalReply(Box::new(LocalReply {
            status: StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
            headers: HeaderMap::new(),
            body: Bytes::from_static(b"too late to replace committed head"),
            provenance: SemanticProvenance::ProducesSemantic,
        })))
        .expect("continuation receiver remains request-owned");

    let error = process.await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("accepted body filter returned a local reply after response head commit")
    );
    assert_eq!(
        session
            .response_head
            .as_ref()
            .expect("committed head")
            .status,
        StatusCode::OK
    );
    assert_eq!(session.response_head_writes.load(Ordering::Relaxed), 1);
    assert!(session.response_body.is_empty());
    assert!(!session.response_eos);
    assert_eq!(selection.published(), [Disposition::Accept]);
    let completed = selection.completed();
    assert_eq!(completed.len(), 1);
    assert_ne!(
        completed[0].termination_reason,
        AttemptTerminationReason::AcceptedEos
    );
    assert_eq!(
        completed[0].stream,
        AttemptStreamOutcome::AbortedBeforeSemanticCommit
    );
    assert!(
        facts
            .finalized
            .lock()
            .expect("native finalization")
            .values()
            .all(|count| *count == 1)
    );
    assert!(!server.await??);
    Ok(())
}
