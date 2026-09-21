use crate::fixture::*;

#[tokio::test]
async fn precommit_plan_enforces_cumulative_limit_across_transport_chunks() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats,
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::from([
            TransportPrecommitEvent::Body(Bytes::from_static(b"abc")),
            TransportPrecommitEvent::Body(Bytes::from_static(b"def")),
        ]),
        protocol: HttpProtocol::Http1,
    };
    let book = RequestLeaseBook::new();
    let mut body_plans = plans();
    body_plans.attempt_response_precommit = BodyPlan::StreamingReplay {
        max_chunk_bytes: 4,
        max_replay_bytes: 5,
    };
    let mut exchange = exchange_with_plans(transport, &book, b"", body_plans);

    exchange.drive_writer_once().await.unwrap();
    assert!(matches!(
        exchange.next_precommit_event(),
        Some(PrecommitEvent::Body(bytes)) if bytes.bytes().as_ref() == b"abc"
    ));
    assert_eq!(
        exchange.drive_writer_once().await.unwrap_err(),
        AttemptError::Body(BodyError::BodyLimitExceeded)
    );
}

#[tokio::test]
async fn deadline_blocked_accept_replacement_never_publishes_old_accept_and_converges() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats: Arc::clone(&stats),
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::new(),
        protocol: HttpProtocol::Http2,
    };
    let book = RequestLeaseBook::new();
    let mut exchange = exchange(transport, &book, b"still-writing");
    exchange.drive_writer_once().await.unwrap();
    exchange
        .submit_disposition_candidate(Disposition::Accept)
        .unwrap();
    assert!(matches!(
        exchange.wait_writer_gate(Instant::now()).await.unwrap(),
        WriterGate::AcceptBlocked {
            reason: AcceptBlockedReason::Deadline
        }
    ));
    let blocked = exchange.snapshot();
    assert_eq!(blocked.downstream_header_fence, CommitFence::Clear);
    assert_eq!(blocked.downstream_semantic_fence, CommitFence::Clear);
    assert_eq!(blocked.published, None);
    assert_eq!(
        stats.lock().unwrap().resets,
        0,
        "blocked Accept preserves response"
    );

    exchange
        .replace_accept_blocked_candidate(Disposition::Continue)
        .unwrap();
    let permit = match exchange
        .wait_writer_gate(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap()
    {
        WriterGate::ReadyToPublishNonAccept { permit, quiescence } => {
            assert_eq!(
                quiescence.close_mode(),
                hiroute_gateway_core::runtime::attempt::RequestCloseMode::CancelReset
            );
            permit
        }
        _ => panic!("expected non-Accept permit"),
    };
    let published = exchange
        .publish_disposition(Disposition::Continue, permit)
        .unwrap();
    assert_eq!(published.disposition, Disposition::Continue);
    exchange.finish_or_abort().await.unwrap();
    assert_eq!(book.outstanding(), 0);
    assert_eq!(stats.lock().unwrap().resets, 1);
    let final_state = exchange.snapshot();
    assert_eq!(final_state.writer_state, WriterState::QuiescedCancelReset);
    assert!(final_state.finalized);
}

#[tokio::test]
async fn early_accept_gate_boundedly_drives_writer_to_normal_eos() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats: Arc::clone(&stats),
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::new(),
        protocol: HttpProtocol::Http2,
    };
    let book = RequestLeaseBook::new();
    let mut exchange = exchange(transport, &book, b"early-response-prompt");
    exchange.drive_writer_once().await.unwrap();
    assert_eq!(exchange.snapshot().writer_state, WriterState::HeaderWritten);
    exchange
        .submit_disposition_candidate(Disposition::Accept)
        .unwrap();

    let permit = match exchange
        .wait_writer_gate(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap()
    {
        WriterGate::ReadyToPublishAccept { permit, quiescence } => {
            assert_eq!(
                quiescence.close_mode(),
                hiroute_gateway_core::runtime::attempt::RequestCloseMode::NormalEos
            );
            permit
        }
        other => panic!("early Accept should wait for normal EOS: {other:?}"),
    };
    assert_eq!(
        exchange.snapshot().writer_state,
        WriterState::QuiescedNormalEos
    );
    assert_eq!(
        stats.lock().unwrap().body_bytes,
        b"early-response-prompt".len()
    );
    assert_eq!(stats.lock().unwrap().finishes, 1);
    assert_eq!(book.outstanding(), 0);
    exchange
        .publish_disposition(Disposition::Accept, permit)
        .unwrap();
}

#[tokio::test]
async fn early_accept_gate_observes_cancellation_without_starting_upstream() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats: Arc::clone(&stats),
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::new(),
        protocol: HttpProtocol::Http1,
    };
    let book = RequestLeaseBook::new();
    let mut exchange = exchange(transport, &book, b"cancelled");
    exchange
        .submit_disposition_candidate(Disposition::Accept)
        .unwrap();
    exchange.cancellation_token().cancel();
    assert!(matches!(
        exchange
            .wait_writer_gate(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap(),
        WriterGate::AcceptBlocked {
            reason: AcceptBlockedReason::Cancelled
        }
    ));
    assert!(stats.lock().unwrap().connects.is_empty());
    assert_eq!(
        exchange.snapshot().upstream_request_fence,
        CommitFence::Clear
    );
}

#[tokio::test]
async fn destructive_cleanup_has_a_bounded_join_and_releases_core_owned_body() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats: Arc::clone(&stats),
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: true,
        events: VecDeque::new(),
        protocol: HttpProtocol::Http1,
    };
    let book = RequestLeaseBook::new();
    let mut exchange = exchange(transport, &book, b"bounded-cleanup");
    exchange.drive_writer_once().await.unwrap();
    let started = Instant::now();

    assert_eq!(
        exchange
            .finish_or_abort_bounded(Duration::from_millis(20))
            .await
            .unwrap_err(),
        AttemptError::CleanupTimeout
    );
    assert!(started.elapsed() < Duration::from_millis(200));
    assert_eq!(stats.lock().unwrap().resets, 1);
    assert_eq!(book.outstanding(), 0);
    assert!(exchange.snapshot().finalized);
}

#[tokio::test]
async fn normal_eos_is_required_for_accept_and_no_reset_occurs() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats: Arc::clone(&stats),
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::new(),
        protocol: HttpProtocol::Http1,
    };
    let book = RequestLeaseBook::new();
    let mut exchange = exchange(transport, &book, b"a");
    for _ in 0..3 {
        exchange.drive_writer_once().await.unwrap();
    }
    assert_eq!(
        exchange.snapshot().writer_state,
        WriterState::QuiescedNormalEos
    );
    exchange
        .submit_disposition_candidate(Disposition::Accept)
        .unwrap();
    let permit = match exchange
        .wait_writer_gate(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap()
    {
        WriterGate::ReadyToPublishAccept { permit, .. } => permit,
        _ => panic!("expected Accept permit"),
    };
    exchange
        .publish_disposition(Disposition::Accept, permit)
        .unwrap();
    assert_eq!(stats.lock().unwrap().resets, 0);
    assert_eq!(book.outstanding(), 0);
}

#[tokio::test]
async fn non_accept_after_request_eos_resets_response_side_before_publication() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats: Arc::clone(&stats),
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::new(),
        protocol: HttpProtocol::Http2,
    };
    let book = RequestLeaseBook::new();
    let mut exchange = exchange(transport, &book, b"a");
    for _ in 0..3 {
        exchange.drive_writer_once().await.unwrap();
    }
    assert_eq!(
        exchange.snapshot().writer_state,
        WriterState::QuiescedNormalEos
    );
    exchange
        .submit_disposition_candidate(Disposition::Continue)
        .unwrap();
    let permit = match exchange
        .wait_writer_gate(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap()
    {
        WriterGate::ReadyToPublishNonAccept { permit, quiescence } => {
            assert_eq!(
                quiescence.close_mode(),
                hiroute_gateway_core::runtime::attempt::RequestCloseMode::CancelReset
            );
            permit
        }
        _ => panic!("expected non-Accept permit"),
    };
    exchange
        .publish_disposition(Disposition::Continue, permit)
        .unwrap();
    assert_eq!(stats.lock().unwrap().resets, 1);
    assert_eq!(
        exchange.snapshot().writer_state,
        WriterState::QuiescedCancelReset
    );
}
