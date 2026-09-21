use crate::fixture::*;

#[tokio::test]
async fn logical_pass_through_is_incremental_and_content_length_is_preflighted()
-> Result<(), TestError> {
    let plan = PlanRevision(90);
    let mut plans = lifecycle_body_plans();
    plans.logical_request = BodyPlan::PassThrough { max_chunk_bytes: 4 };
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 20)
            .route(
                "gateway.test",
                "/stream-request",
                1,
                plain_target("127.0.0.1:9".parse()?, 27),
            )?
            .body_plans(1, plans)?,
    )?;
    let selection = TestSelection::new([]);
    let provider = PassthroughProvider {
        drop_logical_body: true,
        ..PassthroughProvider::default()
    };
    let consumed = Arc::clone(&provider.logical_body_frames);
    let gateway = lifecycle_with_provider(
        publications,
        selection.clone(),
        provider,
        TrackingFilters::default(),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/stream-request",
        Bytes::new(),
        HttpProtocol::Http1,
    )
    .with_body_chunks([
        Bytes::from_static(b"ab"),
        Bytes::from_static(b"cd"),
        Bytes::from_static(b"ef"),
    ]);

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(
        session
            .response_head
            .as_ref()
            .expect("local response")
            .status,
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(consumed.load(Ordering::Relaxed), 4, "three frames + EOS");
    assert_eq!(selection.selected(), 0);

    let plan = PlanRevision(91);
    let mut plans = lifecycle_body_plans();
    plans.logical_request = BodyPlan::BufferedTransform { max_body_bytes: 4 };
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 21)
            .route(
                "gateway.test",
                "/preflight",
                1,
                plain_target("127.0.0.1:9".parse()?, 28),
            )?
            .body_plans(1, plans)?,
    )?;
    let selection = TestSelection::new([]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let mut oversized = RecordingSession::new(
        "gateway.test",
        "/preflight",
        Bytes::from_static(b"12345"),
        HttpProtocol::Http1,
    );
    let read_count = Arc::clone(&oversized.request_body_reads);

    assert_eq!(gateway.process(&mut oversized).await?, SessionReuse::Close);
    assert_eq!(
        oversized
            .response_head
            .as_ref()
            .expect("preflight response")
            .status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(read_count.load(Ordering::Relaxed), 0);
    assert_eq!(selection.selected(), 0);
    Ok(())
}

#[tokio::test]
async fn logical_buffered_transform_holds_head_through_body_filters_and_reframes_at_eos()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (request_tx, request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let head = read_h1_head(&mut socket).await?;
        let body_len = content_length(&head);
        let mut body = vec![0_u8; body_len];
        socket.read_exact(&mut body).await?;
        let _ = request_tx.send((head, body));
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok")
            .await?;
        socket.flush().await?;
        Ok::<(), TestError>(())
    });
    let plan = PlanRevision(95);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let mut plans = lifecycle_body_plans();
    plans.logical_request = BodyPlan::BufferedTransform { max_body_bytes: 8 };
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 25)
            .route(
                "gateway.test",
                "/logical-buffered",
                1,
                plain_target(address, 32),
            )?
            .body_plans(1, plans)?
            .logical_filters(
                1,
                Arc::from([CompiledFilterDescriptor::new("logical-body-header", 8)?
                    .with_capabilities(
                        FilterCapabilities::observe_only().with_header_mutation_during_body(),
                    )]),
            )?,
    )?;
    let filters = TrackingFilters {
        rewrite_logical_header_on_body: true,
        ..TrackingFilters::default()
    };
    let gateway = lifecycle(publications, TestSelection::new([binding]), filters)?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/logical-buffered",
        Bytes::new(),
        HttpProtocol::Http1,
    )
    .with_body_chunks([Bytes::from_static(b"ab"), Bytes::from_static(b"cd")]);

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    let (upstream_head, upstream_body) = request_rx.await?;
    let upstream_head = String::from_utf8_lossy(&upstream_head).to_ascii_lowercase();
    assert!(upstream_head.contains("x-logical-body: filtered"));
    assert!(upstream_head.contains("content-length: 4"));
    assert_eq!(upstream_body, b"abcd");
    server.await??;
    Ok(())
}

#[tokio::test]
async fn logical_streaming_mode_rejects_body_stage_header_mutation_after_provider_commit()
-> Result<(), TestError> {
    let plan = PlanRevision(96);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let mut plans = lifecycle_body_plans();
    plans.logical_request = BodyPlan::PassThrough { max_chunk_bytes: 8 };
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 26)
            .route(
                "gateway.test",
                "/logical-streaming-fence",
                1,
                plain_target("127.0.0.1:9".parse()?, 33),
            )?
            .body_plans(1, plans)?
            .logical_filters(
                1,
                Arc::from([CompiledFilterDescriptor::new(
                    "logical-misbehaving-body-header",
                    8,
                )?]),
            )?,
    )?;
    let selection = TestSelection::new([binding]);
    let filters = TrackingFilters {
        rewrite_logical_header_on_body: true,
        ..TrackingFilters::default()
    };
    let gateway = lifecycle(publications, selection.clone(), filters)?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/logical-streaming-fence",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    let error = gateway.process(&mut session).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("logical body filter mutated request headers after provider commit")
    );
    assert_eq!(selection.selected(), 0);
    assert!(session.response_head.is_none());
    Ok(())
}

#[tokio::test]
async fn buffered_accepted_response_holds_head_until_eos_and_rebuilds_exact_length()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"buffered", false).await
    });
    let plan = PlanRevision(92);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let mut plans = lifecycle_body_plans();
    plans.accepted_response = BodyPlan::BufferedTransform { max_body_bytes: 64 };
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 22)
            .route("gateway.test", "/buffered", 1, plain_target(address, 29))?
            .body_plans(1, plans)?
            .route_accepted_filters(
                1,
                Arc::from([CompiledFilterDescriptor::new("accepted-body-header", 8)?
                    .with_capabilities(
                        FilterCapabilities::observe_only().with_header_mutation_during_body(),
                    )]),
            )?,
    )?;
    let selection = TestSelection::new([binding]);
    let filters = TrackingFilters {
        rewrite_content_length_on_body: true,
        ..TrackingFilters::default()
    };
    let gateway = lifecycle(publications, selection.clone(), filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/buffered",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    let response = session.response_head.as_ref().expect("buffered head");
    assert_eq!(response.headers[CONTENT_LENGTH], "8");
    assert!(
        !response
            .headers
            .contains_key(http::header::TRANSFER_ENCODING)
    );
    assert_eq!(session.response_body, b"buffered");
    assert!(session.response_eos);
    assert_eq!(filters.accepted_body.load(Ordering::Relaxed), 2);
    assert_eq!(selection.published(), [Disposition::Accept]);
    assert!(!server.await??);
    Ok(())
}

#[tokio::test]
async fn core_reconciles_provider_attempt_framing_before_upstream_head_write()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (head_tx, head_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let head = read_h1_head(&mut socket).await?;
        let body_len = content_length(&head);
        let _ = head_tx.send(head);
        let mut body = vec![0_u8; body_len];
        socket.read_exact(&mut body).await?;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok")
            .await?;
        socket.flush().await?;
        Ok::<(), TestError>(())
    });
    let plan = PlanRevision(93);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(BootstrapPublicationBuilder::new(plan.0, 23).route(
        "gateway.test",
        "/upstream-framing",
        1,
        plain_target(address, 30),
    )?)?;
    let provider = PassthroughProvider {
        invalid_attempt_framing: true,
        ..PassthroughProvider::default()
    };
    let gateway = lifecycle_with_provider(
        publications,
        TestSelection::new([binding]),
        provider,
        TrackingFilters::default(),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/upstream-framing",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    let upstream_head_bytes = head_rx.await?;
    let upstream_head = String::from_utf8_lossy(&upstream_head_bytes);
    assert!(
        upstream_head
            .to_ascii_lowercase()
            .contains("content-length: 6")
    );
    assert!(
        !upstream_head
            .to_ascii_lowercase()
            .contains("transfer-encoding")
    );
    server.await??;
    Ok(())
}

#[tokio::test]
async fn accepted_cumulative_body_limit_fails_before_buffered_head_commit() -> Result<(), TestError>
{
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"too-long", false).await
    });
    let plan = PlanRevision(94);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let mut plans = lifecycle_body_plans();
    plans.accepted_response = BodyPlan::BufferedTransform { max_body_bytes: 4 };
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 24)
            .route(
                "gateway.test",
                "/accepted-limit",
                1,
                plain_target(address, 31),
            )?
            .body_plans(1, plans)?,
    )?;
    let selection = TestSelection::new([binding]);
    let provider = PassthroughProvider::default();
    let terminal_releases = Arc::clone(&provider.terminal_request_releases);
    let gateway = lifecycle_with_provider(
        publications,
        selection.clone(),
        provider,
        TrackingFilters::default(),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/accepted-limit",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    let error = gateway.process(&mut session).await.unwrap_err();
    assert!(error.to_string().contains("body limit exceeded"));
    assert!(session.response_head.is_none());
    assert_eq!(selection.published(), [Disposition::Accept]);
    assert_eq!(terminal_releases.load(Ordering::Relaxed), 1);
    let _ = server.await?;
    Ok(())
}
