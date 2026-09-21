use crate::fixture::*;

#[tokio::test]
async fn real_h1_request_emits_runtime_transitions_and_sink_failure_is_fail_open()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"observed", false).await
    });

    let sink = Arc::new(RecordingFailingSink::default());
    let telemetry = Arc::new(Telemetry::new(sink.clone()));
    let plan = PlanRevision(76);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let installer = Arc::new(PublicationInstaller::new().with_telemetry(telemetry.clone()));
    let cancel = CancellationToken::new();
    let envelope = BootstrapPublicationBuilder::new(plan.0, 6)
        .route("gateway.test", "/observed", 1, plain_target(address, 7))?
        .build()?;
    let prepared =
        match installer.prepare(envelope, &cancel, Instant::now() + Duration::from_secs(2))? {
            PrepareOutcome::Prepared(prepared) => prepared,
            PrepareOutcome::Duplicate(_) => return Err("unexpected duplicate publication".into()),
        };
    installer.publish(prepared, &cancel, Instant::now() + Duration::from_secs(2))?;

    let gateway = lifecycle(
        installer,
        TestSelection::new([binding]),
        TrackingFilters::default(),
    )?
    .with_telemetry(telemetry.clone());
    let mut session = RecordingSession::new(
        "gateway.test",
        "/observed?prompt=TELEMETRY_CANARY_SECRET",
        Bytes::from_static(b"private prompt bytes"),
        HttpProtocol::Http1,
    );
    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"observed");
    assert!(session.response_eos);
    assert!(!server.await??);
    telemetry.flush(Duration::from_secs(1))?;
    assert!(
        telemetry.sink_failures() > 0,
        "sink failures must be counted without changing Accept"
    );

    let events = sink.events.lock().expect("telemetry events");
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Publication(PublicationFact {
            stage: PublicationStage::Published,
            ..
        })
    )));
    for direction in [
        BodyDirection::LogicalRequest,
        BodyDirection::AttemptRequest,
        BodyDirection::AttemptResponsePrecommit,
        BodyDirection::AcceptedResponse,
    ] {
        assert!(events.iter().any(|event| matches!(
            event.kind,
            LifecycleKind::Body(BodyFact {
                direction: event_direction,
                ..
            }) if event_direction == direction
        )));
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, LifecycleKind::Response(ResponseFact { .. })))
    );
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Cleanup(CleanupFact {
            kind: CleanupKind::AcceptedRelease,
            timed_out: false,
            ..
        })
    )));
    for stage in [
        DispositionStage::Candidate,
        DispositionStage::GateReady,
        DispositionStage::Published,
    ] {
        assert!(events.iter().any(|event| matches!(
            event.kind,
            LifecycleKind::Disposition(DispositionFact {
                stage: event_stage,
                disposition: Disposition::Accept,
                ..
            }) if event_stage == stage
        )));
    }
    for fence in [
        FenceKind::UpstreamAttemptRequest,
        FenceKind::DownstreamFinalHeaders,
        FenceKind::DownstreamSemanticOutput,
    ] {
        assert!(events.iter().any(|event| matches!(
            event.kind,
            LifecycleKind::Commit(ref fact) if fact.fence == fence
        )));
    }
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Scope(ScopeFact {
            phase: ScopePhase::Paused,
            ..
        })
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Release(ReleaseFact {
            point: ReleasePoint::ModelIrReleased,
            ..
        })
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Memory(MemoryFact {
            role: MemoryRole::RawRequest,
            live_bytes,
            ..
        }) if live_bytes > 0
    )));
    let rendered = format!("{events:?}");
    assert!(!rendered.contains("TELEMETRY_CANARY_SECRET"));
    assert!(!rendered.contains("private prompt bytes"));
    Ok(())
}

#[tokio::test]
async fn concrete_lifecycle_h2_accepts_response_before_request_eos() -> Result<(), TestError> {
    const REQUEST_BYTES: usize = 128 * 1024;
    const PEER_WINDOW_BYTES: u32 = 16 * 1024;
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        let mut builder = server::Builder::new();
        builder.initial_window_size(PEER_WINDOW_BYTES);
        let mut connection = builder.handshake(socket).await?;
        let (request, mut respond) = connection
            .accept()
            .await
            .ok_or("H2 client closed before request")??;
        let stream = async move {
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_LENGTH, "7")
                .body(())?;
            let mut send = respond.send_response(response, false)?;
            // The final response head is deliberately committed before the
            // server reads any request DATA. Two DATA frames then exceed the
            // exchange's one-event precommit capacity. Only response reads
            // are allowed to stop: the request writer must still cross the
            // peer's 16 KiB window and reach normal EOS.
            send.send_data(Bytes::from_static(b"a"), false)?;
            send.send_data(Bytes::from_static(b"b"), false)?;
            let mut body = request.into_body();
            let mut request_bytes = 0;
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                request_bytes += chunk.len();
                body.flow_control().release_capacity(chunk.len())?;
            }
            send.send_data(Bytes::from_static(b"h2-ok"), true)?;
            Ok::<usize, TestError>(request_bytes)
        };
        tokio::pin!(stream);
        let request_bytes = loop {
            tokio::select! {
                result = &mut stream => break result?,
                next = connection.accept() => {
                    if let Some(next) = next {
                        next?;
                        return Err::<(), TestError>("unexpected second H2 request".into());
                    }
                }
            }
        };
        if request_bytes != REQUEST_BYTES {
            return Err::<(), TestError>(
                format!("H2 request ended at {request_bytes} bytes before normal EOS").into(),
            );
        }
        connection.graceful_shutdown();
        if let Some(next) = connection.accept().await {
            next?;
            return Err::<(), TestError>("unexpected second H2 request".into());
        }
        Ok::<(), TestError>(())
    });

    let plan = PlanRevision(73);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let mut target = plain_target(address, 3);
    target.alpn = Arc::new([Arc::from("h2")]);
    target = target.with_derived_connection_fingerprint();
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 3)
            .route("gateway.test", "/h2", 1, target)?
            .precommit_event_capacity(1, 1)?,
    )?;
    let selection = TestSelection::new([binding]);
    let gateway = lifecycle_with_provider_and_limits(
        publications,
        selection.clone(),
        PassthroughProvider::default(),
        TrackingFilters::default(),
        GatewayCoreLifecycleLimits {
            max_request_body_bytes: 256 * 1024,
            write_quantum: 16 * 1024,
            bootstrap_hard_cap: Some(Duration::from_secs(5)),
            ..GatewayCoreLifecycleLimits::default()
        },
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/h2",
        Bytes::from(vec![b'p'; REQUEST_BYTES]),
        HttpProtocol::Http2,
    );
    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"abh2-ok");
    assert_eq!(selection.published(), [Disposition::Accept]);
    tokio::time::timeout(Duration::from_secs(2), server_task).await???;
    Ok(())
}

#[tokio::test]
async fn h2_early_continue_progresses_after_request_exhausts_peer_window_without_drain()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    const REQUEST_BYTES: usize = 512 * 1024;
    const PEER_WINDOW_BYTES: u32 = 16 * 1024;

    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let first_server = tokio::spawn(async move {
        let (socket, _) = first_listener.accept().await?;
        let mut builder = server::Builder::new();
        builder.initial_window_size(PEER_WINDOW_BYTES);
        let mut connection = builder.handshake(socket).await?;
        let (request, mut respond) = connection
            .accept()
            .await
            .ok_or("H2 client closed before request")??;
        let observe = async move {
            let response = Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header(CONTENT_LENGTH, "5")
                .body(())?;
            let mut send = respond.send_response(response, false)?;
            send.send_data(Bytes::from_static(b"retry"), true)?;

            // Deliberately never release receive capacity. The request is
            // larger than the advertised stream window, so only an
            // independently progressing response owner can observe the 429
            // and reset the attempt instead of deadlocking in request write.
            let mut body = request.into_body();
            let mut received = 0;
            let reset = loop {
                match body.data().await {
                    Some(Ok(chunk)) => received += chunk.len(),
                    Some(Err(_)) => break true,
                    None => break false,
                }
            };
            Ok::<(usize, bool), TestError>((received, reset))
        };
        tokio::pin!(observe);
        tokio::select! {
            result = &mut observe => result,
            next = connection.accept() => match next {
                Some(Ok(_)) => Err::<(usize, bool), TestError>("unexpected second H2 request".into()),
                Some(Err(error)) => Err::<(usize, bool), TestError>(error.into()),
                None => Err::<(usize, bool), TestError>("H2 connection closed before reset observation".into()),
            },
        }
    });

    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"h2-fallback-ok", false).await
    });

    let plan = PlanRevision(108);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let mut h2_target = plain_target(first_address, 43);
    h2_target.alpn = Arc::new([Arc::from("h2")]);
    h2_target = h2_target.with_derived_connection_fingerprint();
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 38)
            .route("gateway.test", "/duplex-h2", 1, h2_target)?
            .route(
                "gateway.test",
                "/duplex-h2-fallback",
                2,
                plain_target(second_address, 44),
            )?
            .route_candidates(1, [1, 2])?,
    )?;
    let selection = TestSelection::new([first_binding, second_binding]);
    let gateway = lifecycle_with_provider_and_limits(
        publications,
        selection.clone(),
        PassthroughProvider::default(),
        TrackingFilters::default(),
        GatewayCoreLifecycleLimits {
            max_request_body_bytes: 1024 * 1024,
            write_quantum: 16 * 1024,
            bootstrap_hard_cap: Some(Duration::from_secs(5)),
            ..GatewayCoreLifecycleLimits::default()
        },
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/duplex-h2",
        Bytes::from(vec![b'h'; REQUEST_BYTES]),
        HttpProtocol::Http2,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"h2-fallback-ok");
    assert_eq!(
        selection.published(),
        [Disposition::Continue, Disposition::Accept]
    );
    let (received, reset) = tokio::time::timeout(Duration::from_secs(2), first_server).await???;
    assert!(reset, "Continue must reset the blocked H2 request stream");
    assert!(
        received < REQUEST_BYTES,
        "H2 request unexpectedly reached EOS without peer window credit: {received}/{REQUEST_BYTES}"
    );
    assert!(!second_server.await??);
    Ok(())
}

#[tokio::test]
async fn real_gateway_http_app_proxies_h1_through_production_lifecycle() -> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
    let upstream_address = upstream_listener.local_addr()?;
    let upstream = tokio::spawn(async move {
        let (socket, _) = upstream_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"h1-production-ok", false).await
    });

    let plan = PlanRevision(83);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(BootstrapPublicationBuilder::new(plan.0, 13).route(
        "gateway.test",
        "/h1-production",
        1,
        plain_target(upstream_address, 15),
    )?)?;
    let selection = TestSelection::new([binding]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let port = reserve_tcp_port()?;
    let gateway_address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let gateway_task = start_pingora_service(port, GatewayHttpApp::new(gateway), shutdown_rx);
    wait_until_listening(gateway_address).await?;

    let mut client = tokio::net::TcpStream::connect(gateway_address).await?;
    client
        .write_all(
            b"POST /h1-production HTTP/1.1\r\nHost: gateway.test\r\nContent-Length: 6\r\nConnection: close\r\n\r\nprompt",
        )
        .await?;
    let mut response = Vec::new();
    client.read_to_end(&mut response).await?;
    let head_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("H1 gateway response has no head terminator")?
        + 4;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert_eq!(&response[head_end..], b"h1-production-ok");
    assert_eq!(selection.published(), [Disposition::Accept]);
    assert!(!upstream.await??);

    stop_pingora_service(shutdown_tx, gateway_task).await?;
    Ok(())
}

#[tokio::test]
async fn accepted_h1_duplex_owner_restores_keepalive_session_to_pingora_pool()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_keepalive_pair(socket).await
    });

    let plan = PlanRevision(109);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(BootstrapPublicationBuilder::new(plan.0, 39).route(
        "gateway.test",
        "/h1-reuse",
        1,
        plain_target(address, 45),
    )?)?;
    let selection = TestSelection::new([binding, binding]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;

    let mut first = RecordingSession::new(
        "gateway.test",
        "/h1-reuse",
        Bytes::from_static(b"request-one"),
        HttpProtocol::Http1,
    );
    assert_eq!(gateway.process(&mut first).await?, SessionReuse::Reusable);
    assert_eq!(first.response_body, b"first");

    let mut second = RecordingSession::new(
        "gateway.test",
        "/h1-reuse",
        Bytes::from_static(b"request-two"),
        HttpProtocol::Http1,
    );
    assert_eq!(gateway.process(&mut second).await?, SessionReuse::Reusable);
    assert_eq!(second.response_body, b"second");
    assert_eq!(
        selection.published(),
        [Disposition::Accept, Disposition::Accept]
    );
    tokio::time::timeout(Duration::from_secs(2), server).await???;
    Ok(())
}

#[tokio::test]
async fn real_gateway_h1_delayed_header_filter_does_not_steal_request_body() -> Result<(), TestError>
{
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
    let upstream_address = upstream_listener.local_addr()?;
    let upstream = tokio::spawn(async move {
        let (socket, _) = upstream_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"delayed-filter-ok", false).await
    });

    let plan = PlanRevision(85);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 15)
            .route(
                "gateway.test",
                "/h1-delayed-filter",
                1,
                plain_target(upstream_address, 17),
            )?
            .logical_filters(1, Arc::from([compiled_filter("logical-delay")]))?,
    )?;
    let selection = TestSelection::new([binding]);
    let facts = Arc::new(NativeFilterFacts::default());
    let gateway = GatewayCoreLifecycle::new(
        publications,
        selection.clone(),
        PassthroughProvider::default(),
        native_filter_manager(facts, &["logical-delay"])?,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let port = reserve_tcp_port()?;
    let gateway_address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let gateway_task = start_pingora_service(port, GatewayHttpApp::new(gateway), shutdown_rx);
    wait_until_listening(gateway_address).await?;

    let mut client = tokio::net::TcpStream::connect(gateway_address).await?;
    client
        .write_all(
            b"POST /h1-delayed-filter HTTP/1.1\r\nHost: gateway.test\r\nContent-Length: 11\r\nConnection: close\r\n\r\nprompt-body",
        )
        .await?;
    let mut response = Vec::new();
    client.read_to_end(&mut response).await?;
    let head_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("H1 delayed-filter response has no head terminator")?
        + 4;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert_eq!(&response[head_end..], b"delayed-filter-ok");
    assert_eq!(selection.published(), [Disposition::Accept]);
    assert!(!upstream.await??);

    stop_pingora_service(shutdown_tx, gateway_task).await?;
    Ok(())
}

#[tokio::test]
async fn real_gateway_http_app_routes_h2_authority_to_early_h2_upstream() -> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
    let upstream_address = upstream_listener.local_addr()?;
    let upstream = tokio::spawn(async move {
        let (socket, _) = upstream_listener.accept().await?;
        let mut connection = server::handshake(socket).await?;
        let (request, mut respond) = connection
            .accept()
            .await
            .ok_or("H2 gateway upstream closed before request")??;
        let exchange = async move {
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_LENGTH, "12")
                .body(())?;
            let mut send = respond.send_response(response, false)?;
            // Commit the H2 upstream response before consuming request DATA.
            // The gateway must still drive its attempt writer to normal EOS
            // before publishing Accept and forwarding this response.
            let mut body = request.into_body();
            let mut request_bytes = 0_usize;
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                request_bytes += chunk.len();
                body.flow_control().release_capacity(chunk.len())?;
            }
            send.send_data(Bytes::from_static(b"authority-ok"), true)?;
            Ok::<usize, TestError>(request_bytes)
        };
        tokio::pin!(exchange);
        let request_bytes = loop {
            tokio::select! {
                result = &mut exchange => break result?,
                next = connection.accept() => {
                    if let Some(next) = next {
                        next?;
                        return Err::<usize, TestError>("unexpected second H2 upstream request".into());
                    }
                }
            }
        };
        connection.graceful_shutdown();
        if let Some(next) = connection.accept().await {
            next?;
        }
        Ok::<usize, TestError>(request_bytes)
    });

    let plan = PlanRevision(80);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let mut target = plain_target(upstream_address, 11);
    target.alpn = Arc::new([Arc::from("h2")]);
    target = target.with_derived_connection_fingerprint();
    let publications = install_publication(BootstrapPublicationBuilder::new(plan.0, 10).route(
        "gateway.test",
        "/h2-authority",
        1,
        target,
    )?)?;
    let selection = TestSelection::new([binding]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let port = reserve_tcp_port()?;
    let gateway_address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let gateway_task = start_pingora_service(port, GatewayHttpApp::new(gateway), shutdown_rx);
    wait_until_listening(gateway_address).await?;

    let socket = tokio::net::TcpStream::connect(gateway_address).await?;
    let (mut sender, connection) = client::handshake(socket).await?;
    let client_driver = tokio::spawn(connection);
    poll_fn(|context| sender.poll_ready(context)).await?;
    let request = http::Request::builder()
        .method(Method::POST)
        .uri("http://gateway.test/h2-authority")
        .header(CONTENT_LENGTH, "6")
        .body(())?;
    assert!(
        !request.headers().contains_key(HOST),
        "the H2 client must exercise :authority without a Host fallback"
    );
    let (response, mut request_body) = sender.send_request(request, false)?;
    request_body.send_data(Bytes::from_static(b"prompt"), true)?;
    let response = response.await?;
    assert_eq!(response.status(), StatusCode::OK);
    let mut response_body = response.into_body();
    let mut received = Vec::new();
    while let Some(chunk) = response_body.data().await {
        let chunk = chunk?;
        received.extend_from_slice(&chunk);
        response_body.flow_control().release_capacity(chunk.len())?;
    }
    assert_eq!(received, b"authority-ok");
    assert_eq!(selection.published(), [Disposition::Accept]);
    assert_eq!(
        upstream.await??,
        6,
        "upstream request must reach normal EOS"
    );

    drop(sender);
    client_driver.abort();
    let _ = client_driver.await;
    stop_pingora_service(shutdown_tx, gateway_task).await?;
    Ok(())
}

#[tokio::test]
async fn gateway_http_app_shutdown_cancels_then_bounds_request_join() -> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let port = reserve_tcp_port()?;
    let address = format!("127.0.0.1:{port}").parse()?;
    let facts = Arc::new(ShutdownLifecycleFacts::default());
    let app = GatewayHttpApp::new(ShutdownHangingLifecycle {
        facts: Arc::clone(&facts),
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let gateway_task = start_pingora_service(port, app, shutdown_rx);
    wait_until_listening(address).await?;
    let mut client = tokio::net::TcpStream::connect(address).await?;
    client
        .write_all(b"GET /shutdown HTTP/1.1\r\nHost: gateway.test\r\nContent-Length: 0\r\n\r\n")
        .await?;
    tokio::time::timeout(Duration::from_secs(1), facts.entered.notified()).await?;

    let started = Instant::now();
    shutdown_tx.send(true)?;
    tokio::time::timeout(Duration::from_secs(4), facts.dropped.notified()).await?;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(facts.cancellation_seen.load(Ordering::Relaxed), 1);
    assert_eq!(facts.process_dropped.load(Ordering::Relaxed), 1);
    tokio::time::timeout(Duration::from_secs(4), gateway_task).await??;
    Ok(())
}

#[tokio::test]
async fn concrete_lifecycle_uses_pingora_tls_with_plan_ca_and_sni() -> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let certified = generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let certificate: CertificateDer<'static> = certified.cert.der().clone();
    let private_key =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
    let mut tls_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], private_key)?;
    tls_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(tls_config));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        let tls = acceptor.accept(socket).await?;
        serve_h1_once(tls, StatusCode::OK, b"tls-ok", false).await
    });

    let plan = PlanRevision(74);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let mut target = plain_target(address, 4);
    target.scheme = TransportScheme::Https;
    target.authority = Arc::from("localhost");
    target.sni = Some(Arc::from("localhost"));
    target.ca = CaPolicy::Pem(Arc::from(certified.cert.pem().into_bytes()));
    target = target.with_derived_connection_fingerprint();
    let publications = install_publication(BootstrapPublicationBuilder::new(plan.0, 4).route(
        "gateway.test",
        "/tls",
        1,
        target,
    )?)?;
    let selection = TestSelection::new([binding]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/tls",
        Bytes::from_static(b"secret prompt"),
        HttpProtocol::Http1,
    );
    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"tls-ok");
    assert_eq!(selection.published(), [Disposition::Accept]);
    assert!(!server_task.await??);
    Ok(())
}

#[tokio::test]
async fn segmented_h1_response_progresses_while_large_request_write_is_blocked()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    const REQUEST_BYTES: usize = 4 * 1024 * 1024;

    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let first_server = tokio::spawn(async move {
        let (socket, _) = first_listener.accept().await?;
        serve_segmented_h1_early_then_observe_reset(socket).await
    });

    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"duplex-h1-ok", false).await
    });

    let plan = PlanRevision(107);
    let first_binding = ResolvedTargetBindingId::new(plan, 1);
    let second_binding = ResolvedTargetBindingId::new(plan, 2);
    let large_body_plans = BootstrapBodyPlans {
        logical_request: BodyPlan::BufferedTransform {
            max_body_bytes: REQUEST_BYTES,
        },
        attempt_request: BodyPlan::StreamingReplay {
            max_chunk_bytes: 64 * 1024,
            max_replay_bytes: REQUEST_BYTES,
        },
        attempt_response_precommit: BodyPlan::PassThrough {
            max_chunk_bytes: 64 * 1024,
        },
        accepted_response: BodyPlan::PassThrough {
            max_chunk_bytes: 64 * 1024,
        },
    };
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 37)
            .route(
                "gateway.test",
                "/duplex-h1",
                1,
                plain_target(first_address, 41),
            )?
            .route(
                "gateway.test",
                "/duplex-h1-fallback",
                2,
                plain_target(second_address, 42),
            )?
            .route_candidates(1, [1, 2])?
            .body_plans(1, large_body_plans.clone())?
            .body_plans(2, large_body_plans)?,
    )?;
    let selection = TestSelection::new([first_binding, second_binding]);
    let gateway = lifecycle_with_provider_and_limits(
        publications,
        selection.clone(),
        PassthroughProvider::default(),
        TrackingFilters::default(),
        GatewayCoreLifecycleLimits {
            max_request_body_bytes: REQUEST_BYTES,
            write_quantum: 64 * 1024,
            bootstrap_hard_cap: Some(Duration::from_secs(8)),
            stream_memory_bytes: 32 * 1024 * 1024,
            ..GatewayCoreLifecycleLimits::default()
        },
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/duplex-h1",
        Bytes::from(vec![b'x'; REQUEST_BYTES]),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"duplex-h1-ok");
    assert_eq!(
        selection.published(),
        [Disposition::Continue, Disposition::Accept]
    );
    let (received, expected) =
        tokio::time::timeout(Duration::from_secs(3), first_server).await???;
    assert_eq!(expected, REQUEST_BYTES);
    assert!(
        received < expected,
        "early Continue must reset before the blocked H1 writer sends the full request: {received}/{expected}"
    );
    assert!(!second_server.await??);
    Ok(())
}
