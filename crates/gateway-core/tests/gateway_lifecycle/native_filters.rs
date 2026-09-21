use crate::fixture::*;

#[tokio::test]
async fn production_filter_sidecall_is_admitted_before_build_and_watermarks_request_body()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"sidecall-ok", false).await
    });

    let plan = PlanRevision(88);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 18)
            .route(
                "gateway.test",
                "/filter-sidecall",
                1,
                plain_target(address, 18),
            )?
            .logical_filters(1, Arc::from([compiled_filter("logical-sidecall")]))?,
    )?;
    let facts = Arc::new(NativeFilterFacts::default());
    let gateway = GatewayCoreLifecycle::new(
        publications,
        TestSelection::new([binding]),
        PassthroughProvider::default(),
        native_filter_manager(Arc::clone(&facts), &["logical-sidecall"])?,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/filter-sidecall",
        Bytes::from_static(b"prompt-body"),
        HttpProtocol::Http1,
    );
    let request_body_reads = Arc::clone(&session.request_body_reads);
    let mut process = Box::pin(gateway.process(&mut session));

    tokio::select! {
        _ = facts.executor_entered.notified() => {}
        result = &mut process => panic!("request completed before sidecall release: {result:?}"),
    }
    assert_eq!(facts.executor_builder_calls.load(Ordering::Relaxed), 1);
    assert_eq!(facts.executor_payload_bytes.load(Ordering::Relaxed), 32);
    assert_eq!(
        request_body_reads.load(Ordering::Relaxed),
        0,
        "an awaited native sidecall must apply StopAll/Watermark semantics to the source read owner",
    );
    facts.executor_release.notify_one();

    assert_eq!(process.await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"sidecall-ok");
    assert!(
        facts
            .executor_services
            .lock()
            .expect("native executor services")
            .iter()
            .all(|services| services.active_children() == 0),
        "request finalization must not leave an admitted child active",
    );
    server.await??;
    Ok(())
}

#[tokio::test]
async fn production_compute_and_blocking_filters_run_on_distinct_fixed_backends()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"offloaded", false).await
    });
    let plan = PlanRevision(111);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 111)
            .route(
                "gateway.test",
                "/fixed-filter-backends",
                1,
                plain_target(address, 111),
            )?
            .logical_filters(
                1,
                Arc::from([
                    compiled_filter("logical-compute"),
                    compiled_filter("logical-blocking"),
                ]),
            )?,
    )?;
    let facts = Arc::new(NativeFilterFacts::default());
    let gateway = GatewayCoreLifecycle::new(
        publications,
        TestSelection::new([binding]),
        PassthroughProvider::default(),
        native_filter_manager(Arc::clone(&facts), &["logical-compute", "logical-blocking"])?,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/fixed-filter-backends",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"offloaded");
    {
        let threads = facts
            .offloaded_threads
            .lock()
            .expect("offloaded filter threads");
        assert_eq!(threads.len(), 2);
        for (kind, gateway_thread, worker_thread) in threads.iter() {
            assert_ne!(worker_thread, gateway_thread);
            assert!(worker_thread.contains(&format!("hiroute-{kind:?}")));
        }
        assert_ne!(threads[0].2, threads[1].2);
    }
    server.await??;
    Ok(())
}

#[tokio::test]
async fn production_stop_all_buffer_services_continuation_while_watermark_stops_source_reads()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;

    for (case, expect_source_progress) in [("logical-buffer", true), ("logical-watermark", false)] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await?;
            serve_h1_once(socket, StatusCode::OK, b"pause-ok", false).await
        });
        let plan = PlanRevision(if expect_source_progress { 96 } else { 97 });
        let binding = ResolvedTargetBindingId::new(plan, 1);
        let mut plans = lifecycle_body_plans();
        plans.logical_request = BodyPlan::StreamingReplay {
            max_chunk_bytes: 16,
            max_replay_bytes: 64,
        };
        let publications = install_publication(
            BootstrapPublicationBuilder::new(plan.0, plan.0)
                .route(
                    "gateway.test",
                    "/logical-pause",
                    1,
                    plain_target(address, plan.0),
                )?
                .body_plans(1, plans)?
                .logical_filters(1, Arc::from([compiled_filter(case)]))?,
        )?;
        let facts = Arc::new(NativeFilterFacts::default());
        let gateway = GatewayCoreLifecycle::new(
            publications,
            TestSelection::new([binding]),
            PassthroughProvider::default(),
            native_filter_manager(Arc::clone(&facts), &[case])?,
            PingoraConnectorAdapter::new(),
            bounded_test_limits(Duration::from_secs(5)),
        )?;
        let mut session = RecordingSession::new(
            "gateway.test",
            "/logical-pause",
            Bytes::new(),
            HttpProtocol::Http1,
        )
        .with_body_chunks([
            Bytes::from_static(b"bounded-"),
            Bytes::from_static(b"prompt"),
        ]);
        let request_body_reads = Arc::clone(&session.request_body_reads);
        let mut process = Box::pin(gateway.process(&mut session));

        tokio::select! {
            _ = facts.watermark_entered.notified() => {}
            result = &mut process => panic!("{case} completed before explicit resume: {result:?}"),
        }
        if expect_source_progress {
            tokio::time::timeout(Duration::from_secs(1), async {
                while request_body_reads.load(Ordering::Relaxed) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| "StopAll Buffer did not service its bounded source")?;
        } else {
            assert!(
                tokio::time::timeout(Duration::from_millis(25), &mut process)
                    .await
                    .is_err(),
                "StopAll Watermark must remain paused without completing"
            );
            assert_eq!(
                request_body_reads.load(Ordering::Relaxed),
                0,
                "StopAll Watermark must never poll the request-body source"
            );
        }
        facts
            .watermark_continuation
            .lock()
            .expect("pause continuation")
            .take()
            .expect("filter retained explicit continuation")
            .resume(ResumeAction::Continue(HeaderPatch::default()))
            .expect("request owner still holds continuation receiver");

        assert_eq!(process.await?, SessionReuse::Reusable);
        assert_eq!(session.response_body, b"pause-ok");
        assert!(request_body_reads.load(Ordering::Relaxed) >= 2);
        server.await??;
    }
    Ok(())
}

#[tokio::test]
async fn continue_waits_for_active_attempt_filter_child_before_reselection() -> Result<(), TestError>
{
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let first_server = tokio::spawn(async move {
        let (socket, _) = first_listener.accept().await?;
        serve_h1_once(socket, StatusCode::TOO_MANY_REQUESTS, b"retry", false).await
    });
    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"after-child-join", false).await
    });

    let plan = PlanRevision(8101);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 811)
            .route(
                "gateway.test",
                "/attempt-child-join",
                1,
                plain_target(first_address, 812),
            )?
            .route(
                "gateway.test",
                "/attempt-child-join-fallback",
                2,
                plain_target(second_address, 813),
            )?
            .route_candidates(1, [1, 2])?
            .attempt_response_filters(
                1,
                Arc::from([compiled_filter("attempt-background-child")]),
            )?,
    )?;
    let facts = Arc::new(NativeFilterFacts::default());
    let child_entered = facts.attempt_cleanup_child_entered.notified();
    tokio::pin!(child_entered);
    let selection = TestSelection::new([first_binding, second_binding]);
    let gateway = GatewayCoreLifecycle::new(
        publications,
        selection.clone(),
        PassthroughProvider::default(),
        native_filter_manager(Arc::clone(&facts), &["attempt-background-child"])?,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/attempt-child-join",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );
    let process = tokio::spawn(async move {
        let result = gateway.process(&mut session).await;
        (result, session)
    });

    tokio::time::timeout(Duration::from_secs(2), &mut child_entered).await?;
    tokio::time::timeout(Duration::from_millis(150), async {
        while selection.published() != [Disposition::Continue] {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(facts.attempt_cleanup_child_running.load(Ordering::Acquire));
    assert_eq!(selection.selected(), 1);
    assert!(selection.completed().is_empty());
    assert_eq!(selection.trace(), [SelectionTrace::Select]);
    *facts
        .attempt_cleanup_child_release
        .lock()
        .expect("attempt child release") = true;
    facts.attempt_cleanup_child_release_cv.notify_all();

    let (result, session) = process.await?;
    assert_eq!(result?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"after-child-join");
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
        facts.attempt_cleanup_child_finished.load(Ordering::Relaxed),
        1
    );
    assert!(
        facts
            .executor_services
            .lock()
            .expect("attempt child executor services")
            .iter()
            .all(|services| services.active_children() == 0)
    );
    {
        let finalized = facts.finalized.lock().expect("attempt child finalization");
        assert_eq!(finalized.len(), 1);
        assert!(finalized.values().all(|count| *count == 1));
    }
    assert!(!first_server.await??);
    assert!(!second_server.await??);
    Ok(())
}

#[tokio::test]
async fn compiled_native_filters_are_request_and_attempt_local_in_production_lifecycle()
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
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"filtered", false).await
    });

    let plan = PlanRevision(81);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 11)
            .route(
                "gateway.test",
                "/native",
                1,
                plain_target(first_address, 12),
            )?
            .route(
                "gateway.test",
                "/fallback-native",
                2,
                plain_target(second_address, 13),
            )?
            .route_candidates(1, [1, 2])?
            .logical_filters(
                1,
                Arc::from([
                    compiled_filter("logical-a"),
                    compiled_filter("logical-b"),
                    compiled_filter("logical-c"),
                ]),
            )?
            .attempt_request_filters(1, Arc::from([compiled_filter("attempt-request")]))?
            .attempt_request_filters(2, Arc::from([compiled_filter("attempt-request")]))?
            .attempt_response_filters(1, Arc::from([compiled_filter("attempt")]))?
            .attempt_response_filters(2, Arc::from([compiled_filter("attempt")]))?
            .route_accepted_filters(1, Arc::from([compiled_filter("accepted")]))?,
    )?;
    let facts = Arc::new(NativeFilterFacts::default());
    let filters = native_filter_manager(
        Arc::clone(&facts),
        &[
            "logical-a",
            "logical-b",
            "logical-c",
            "attempt-request",
            "attempt",
            "accepted",
        ],
    )?;
    let selection = TestSelection::new([first_binding, second_binding]);
    let gateway = GatewayCoreLifecycle::new(
        publications,
        selection.clone(),
        PassthroughProvider::default(),
        filters,
        PingoraConnectorAdapter::new(),
        GatewayCoreLifecycleLimits {
            max_request_body_bytes: 64 * 1024,
            write_quantum: 4,
            bootstrap_hard_cap: Some(Duration::from_secs(5)),
            ..GatewayCoreLifecycleLimits::default()
        },
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/native",
        Bytes::from_static(b"nested-filter-body"),
        HttpProtocol::Http1,
    );
    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"filtered");
    assert_eq!(
        selection.published(),
        [Disposition::Continue, Disposition::Accept]
    );
    let _ = first_server.await??;
    assert!(!second_server.await??);

    let instances = facts.instances.lock().expect("native filter instances");
    assert_eq!(instances.len(), 8);
    let attempt_request_contexts = instances
        .iter()
        .filter(|(_, id, _)| id.as_ref() == "attempt-request")
        .map(|(_, _, context)| context.clone())
        .collect::<Vec<_>>();
    assert_eq!(attempt_request_contexts.len(), 2);
    assert_eq!(attempt_request_contexts[0].attempt_generation, Some(1));
    assert_eq!(attempt_request_contexts[1].attempt_generation, Some(2));
    let attempt_contexts = instances
        .iter()
        .filter(|(_, id, _)| id.as_ref() == "attempt")
        .map(|(_, _, context)| context.clone())
        .collect::<Vec<_>>();
    assert_eq!(attempt_contexts.len(), 2);
    assert_eq!(attempt_contexts[0].attempt_id, Some(1));
    assert_eq!(attempt_contexts[0].attempt_generation, Some(1));
    assert_eq!(attempt_contexts[1].attempt_id, Some(2));
    assert_eq!(attempt_contexts[1].attempt_generation, Some(2));
    assert!(attempt_contexts.iter().all(|context| context.scope_kind
        == hiroute_gateway_core::runtime::scope::ScopeKind::RouteAttempt));
    drop(instances);
    let trace = facts.trace.lock().expect("native filter trace");
    assert_eq!(
        &trace[..6],
        [
            "logical-a:H:LogicalRequest",
            "logical-b:H:LogicalRequest",
            "logical-a:D:LogicalRequest",
            "logical-b:D:LogicalRequest",
            "logical-c:H:LogicalRequest",
            "logical-c:D:LogicalRequest",
        ],
        "production owner must unwind nested data/header pauses before later-filter data"
    );
    drop(trace);
    let finalized = facts.finalized.lock().expect("native filter finalization");
    assert_eq!(finalized.len(), 8);
    assert!(finalized.values().all(|count| *count == 1));
    Ok(())
}

#[tokio::test]
async fn production_native_body_emitter_drops_or_replaces_charged_logical_units()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;

    for (case, expected) in [
        (
            "logical-replace",
            vec![
                Bytes::from_static(b"filtered-"),
                Bytes::from_static(b"body"),
            ],
        ),
        ("logical-drop", Vec::new()),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await?;
            serve_h1_once(socket, StatusCode::OK, b"emitter-ok", false).await
        });
        let revision = if case == "logical-replace" { 85 } else { 86 };
        let plan = PlanRevision(revision);
        let binding = ResolvedTargetBindingId::new(plan, 1);
        let publications = install_publication(
            BootstrapPublicationBuilder::new(plan.0, revision)
                .route(
                    "gateway.test",
                    "/emitter",
                    1,
                    plain_target(address, revision),
                )?
                .logical_filters(1, Arc::from([compiled_filter(case)]))?,
        )?;
        let provider = PassthroughProvider::default();
        let observed = Arc::clone(&provider.observed_logical_body);
        let filters = native_filter_manager(Arc::new(NativeFilterFacts::default()), &[case])?;
        let gateway = GatewayCoreLifecycle::new(
            publications,
            TestSelection::new([binding]),
            provider,
            filters,
            PingoraConnectorAdapter::new(),
            bounded_test_limits(Duration::from_secs(5)),
        )?;
        let mut session = RecordingSession::new(
            "gateway.test",
            "/emitter",
            Bytes::from_static(b"original"),
            HttpProtocol::Http1,
        );

        assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
        assert_eq!(session.response_body, b"emitter-ok");
        {
            let observed = observed.lock().expect("logical emitter observations");
            let observed_bytes: Vec<_> = observed.iter().map(|(bytes, _)| bytes.clone()).collect();
            assert_eq!(
                observed_bytes, expected,
                "{case} must own the emitted units"
            );
            assert!(
                observed
                    .iter()
                    .all(|(_, role)| *role == MemoryRole::RawRequest),
                "every replacement must be charged before the #15 handoff"
            );
            assert!(
                observed
                    .iter()
                    .all(|(bytes, _)| bytes.as_ref() != b"original"),
                "drop/replace must not leak the original charged frame"
            );
        }
        server.await??;
    }
    Ok(())
}

#[tokio::test]
async fn production_native_factory_cannot_widen_compiled_capabilities() -> Result<(), TestError> {
    let plan = PlanRevision(87);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 87)
            .route(
                "gateway.test",
                "/capability-escalation",
                1,
                plain_target("127.0.0.1:9".parse()?, 87),
            )?
            .logical_filters(
                1,
                Arc::from([CompiledFilterDescriptor::new("logical-replace", 8)
                    .expect("observe-only descriptor")]),
            )?,
    )?;
    let filters =
        native_filter_manager(Arc::new(NativeFilterFacts::default()), &["logical-replace"])?;
    let gateway = GatewayCoreLifecycle::new(
        publications,
        TestSelection::new([binding]),
        PassthroughProvider::default(),
        filters,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/capability-escalation",
        Bytes::from_static(b"original"),
        HttpProtocol::Http1,
    );

    let error = gateway.process(&mut session).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("runtime capabilities exceed its compiled descriptor")
    );
    assert!(session.response_head.is_none());
    Ok(())
}

#[tokio::test]
async fn accepted_header_stop_iteration_holds_head_until_body_driven_resume()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"body-resume", false).await
    });

    let plan = PlanRevision(83);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let mut plans = lifecycle_body_plans();
    plans.accepted_response = BodyPlan::BufferedTransform {
        max_body_bytes: 64 * 1024,
    };
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 13)
            .route(
                "gateway.test",
                "/accepted-stop",
                1,
                plain_target(address, 15),
            )?
            .body_plans(1, plans)?
            .route_accepted_filters(1, Arc::from([compiled_filter("accepted-stop")]))?,
    )?;
    let facts = Arc::new(NativeFilterFacts::default());
    let filters = native_filter_manager(Arc::clone(&facts), &["accepted-stop"])?;
    let gateway = GatewayCoreLifecycle::new(
        publications,
        TestSelection::new([binding]),
        PassthroughProvider::default(),
        filters,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/accepted-stop",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"body-resume");
    assert_eq!(
        session
            .response_head
            .as_ref()
            .expect("accepted response head")
            .headers["x-accepted-body"],
        "filtered-before-commit"
    );
    server.await??;
    Ok(())
}

#[tokio::test]
async fn accepted_header_stop_iteration_flushes_prefix_then_returns_to_streaming()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let first_chunk_flushed = Arc::new(Notify::new());
    let release_tail = Arc::new(Notify::new());
    let upstream_first_chunk = Arc::clone(&first_chunk_flushed);
    let upstream_release = Arc::clone(&release_tail);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let head = read_h1_head(&mut socket).await?;
        let mut request_body = vec![0_u8; content_length(&head)];
        socket.read_exact(&mut request_body).await?;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: keep-alive\r\n\r\n")
            .await?;
        socket.write_all(b"body-").await?;
        socket.flush().await?;
        upstream_first_chunk.notify_one();
        upstream_release.notified().await;
        socket.write_all(b"resume").await?;
        socket.flush().await?;
        Ok::<_, TestError>(())
    });

    let plan = PlanRevision(98);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 98)
            .route(
                "gateway.test",
                "/accepted-stream-stop",
                1,
                plain_target(address, 98),
            )?
            .route_accepted_filters(1, Arc::from([compiled_filter("accepted-stream-stop")]))?,
    )?;
    let facts = Arc::new(NativeFilterFacts::default());
    let gateway = GatewayCoreLifecycle::new(
        publications,
        TestSelection::new([binding]),
        PassthroughProvider::default(),
        native_filter_manager(Arc::clone(&facts), &["accepted-stream-stop"])?,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/accepted-stream-stop",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );
    let response_head_writes = Arc::clone(&session.response_head_writes);
    let observed_body_writes = Arc::clone(&session.observed_response_body_writes);
    let mut process = Box::pin(gateway.process(&mut session));

    tokio::select! {
        result = &mut process => panic!("accepted stream completed before upstream prefix gate: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(2), first_chunk_flushed.notified()) => {
            result.map_err(|_| "upstream did not flush the accepted prefix")?;
        }
    }
    tokio::select! {
        result = &mut process => panic!("accepted stream completed before tail release: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let prefix_written = observed_body_writes
                    .lock()
                    .expect("observed response body writes")
                    .iter()
                    .any(|(bytes, end_stream)| bytes.as_ref() == b"body-" && !end_stream);
                if response_head_writes.load(Ordering::Relaxed) == 1 && prefix_written {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }) => {
            result.map_err(|_| "accepted StopIteration did not commit and flush its prefix")?;
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut process)
            .await
            .is_err(),
        "the accepted owner must stream the prefix before waiting for the tail"
    );
    release_tail.notify_one();

    assert_eq!(process.await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"body-resume");
    assert_eq!(session.response_head_writes.load(Ordering::Relaxed), 1);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn accepted_stop_all_watermark_gates_transport_until_explicit_continuation()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"explicit-resume", false).await
    });

    let plan = PlanRevision(84);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 14)
            .route(
                "gateway.test",
                "/accepted-watermark",
                1,
                plain_target(address, 16),
            )?
            .route_accepted_filters(1, Arc::from([compiled_filter("accepted-watermark")]))?,
    )?;
    let facts = Arc::new(NativeFilterFacts::default());
    let filters = native_filter_manager(Arc::clone(&facts), &["accepted-watermark"])?;
    let gateway = GatewayCoreLifecycle::new(
        publications,
        TestSelection::new([binding]),
        PassthroughProvider::default(),
        filters,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/accepted-watermark",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );
    let mut process = Box::pin(gateway.process(&mut session));
    tokio::select! {
        _ = facts.watermark_entered.notified() => {}
        result = &mut process => panic!("watermark request completed before resume: {result:?}"),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut process)
            .await
            .is_err(),
        "StopAll Watermark must keep the downstream head uncommitted"
    );
    facts
        .watermark_continuation
        .lock()
        .expect("watermark continuation")
        .take()
        .expect("filter retained its explicit continuation")
        .resume(ResumeAction::Continue(HeaderPatch::default()))
        .expect("continuation receiver remains request-owned");
    assert_eq!(process.await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"explicit-resume");
    server.await??;
    Ok(())
}

#[tokio::test]
async fn production_native_filter_panic_finalizes_request_chain_exactly_once()
-> Result<(), TestError> {
    let plan = PlanRevision(82);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 12)
            .route(
                "gateway.test",
                "/panic",
                1,
                plain_target("127.0.0.1:9".parse()?, 14),
            )?
            .logical_filters(1, Arc::from([compiled_filter("panic")]))?,
    )?;
    let facts = Arc::new(NativeFilterFacts::default());
    let gateway = GatewayCoreLifecycle::new(
        publications,
        TestSelection::new([binding]),
        PassthroughProvider::default(),
        native_filter_manager(Arc::clone(&facts), &["panic"])?,
        PingoraConnectorAdapter::new(),
        GatewayCoreLifecycleLimits::default(),
    )?;
    let mut session =
        RecordingSession::new("gateway.test", "/panic", Bytes::new(), HttpProtocol::Http1);
    let error = gateway
        .process(&mut session)
        .await
        .expect_err("native callback panic must fail closed");
    assert!(error.to_string().contains("callback panicked"));
    let finalized = facts.finalized.lock().expect("native filter finalization");
    assert_eq!(finalized.len(), 1);
    assert!(finalized.values().all(|count| *count == 1));
    Ok(())
}
