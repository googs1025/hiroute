use crate::fixture::*;

#[tokio::test]
async fn one_exchange_has_one_semantic_call_and_response_biased_h1_writer() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats: Arc::clone(&stats),
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::from([TransportPrecommitEvent::ResponseHead {
            status: StatusCode::TOO_MANY_REQUESTS,
            headers: HeaderMap::new(),
        }]),
        protocol: HttpProtocol::Http1,
    };
    let book = RequestLeaseBook::new();
    let mut exchange = exchange(transport, &book, b"abcdefgh");
    exchange.drive_writer_once().await.unwrap();
    assert!(matches!(
        exchange.next_precommit_event(),
        Some(PrecommitEvent::ResponseHead(head))
            if head.status() == StatusCode::TOO_MANY_REQUESTS
    ));
    assert_eq!(
        stats.lock().unwrap().heads,
        0,
        "response is polled before next write quantum"
    );
    assert_eq!(
        exchange.snapshot().semantic_upstream_calls,
        0,
        "a response-biased precommit poll is not a semantic request call"
    );

    exchange.drive_writer_once().await.unwrap();
    exchange.drive_writer_once().await.unwrap();
    assert_eq!(stats.lock().unwrap().body_bytes, 4);
    assert_eq!(exchange.snapshot().semantic_upstream_calls, 1);
}

#[tokio::test]
async fn attempt_wire_backing_stays_charged_until_normal_eos_or_reset_completion() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats,
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::new(),
        protocol: HttpProtocol::Http1,
    };
    let book = RequestLeaseBook::new();
    let budgets = BudgetTree::new(1024 * 1024, 1024 * 1024).unwrap();
    let budget = budgets.stream(512 * 1024).unwrap();
    let prepared = request(&book, &budget, b"abcdefgh");
    let mut exchange = AttemptExchange::new(
        RequestId(1),
        AttemptId(2),
        AttemptGeneration(3),
        PlanRevision(4),
        plain_target("127.0.0.1:8080".parse().unwrap(), 9),
        transport,
        prepared,
        plans(),
        timeouts(),
        Instant::now() + Duration::from_secs(5),
        16,
        budget.clone(),
        4,
    )
    .unwrap();

    exchange.drive_writer_once().await.unwrap();
    exchange.drive_writer_once().await.unwrap();
    assert_eq!(
        budget.snapshot().unwrap().role_live[MemoryRole::TransportInflight as usize],
        4,
        "the charged owner outlives the transport write future"
    );
    exchange.drive_writer_once().await.unwrap();
    assert_eq!(
        budget.snapshot().unwrap().role_live[MemoryRole::TransportInflight as usize],
        8
    );
    exchange.drive_writer_once().await.unwrap();
    let normal = budget.snapshot().unwrap();
    assert_eq!(normal.role_live[MemoryRole::TransportInflight as usize], 0);
    assert_eq!(normal.role_live[MemoryRole::AttemptWire as usize], 0);
    assert_eq!(book.outstanding(), 0);

    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats,
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::new(),
        protocol: HttpProtocol::Http1,
    };
    let book = RequestLeaseBook::new();
    let reset_budget = budgets.stream(512 * 1024).unwrap();
    let prepared = request(&book, &reset_budget, b"abcd");
    let mut exchange = AttemptExchange::new(
        RequestId(5),
        AttemptId(6),
        AttemptGeneration(7),
        PlanRevision(4),
        plain_target("127.0.0.1:8080".parse().unwrap(), 9),
        transport,
        prepared,
        plans(),
        timeouts(),
        Instant::now() + Duration::from_secs(5),
        16,
        reset_budget.clone(),
        4,
    )
    .unwrap();
    exchange.drive_writer_once().await.unwrap();
    exchange.drive_writer_once().await.unwrap();
    assert_eq!(
        reset_budget.snapshot().unwrap().role_live[MemoryRole::TransportInflight as usize],
        4
    );
    exchange
        .submit_disposition_candidate(Disposition::Continue)
        .unwrap();
    assert!(matches!(
        exchange
            .wait_writer_gate(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap(),
        WriterGate::ReadyToPublishNonAccept { .. }
    ));
    let reset = reset_budget.snapshot().unwrap();
    assert_eq!(reset.role_live[MemoryRole::TransportInflight as usize], 0);
    assert_eq!(reset.role_live[MemoryRole::AttemptWire as usize], 0);
    assert_eq!(book.outstanding(), 0);
}

#[test]
fn exchange_rejects_provider_attempt_queue_that_violates_compiled_plan() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats,
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::new(),
        protocol: HttpProtocol::Http1,
    };
    let book = RequestLeaseBook::new();
    let budgets = BudgetTree::new(1024 * 1024, 1024 * 1024).unwrap();
    let budget = budgets.stream(512 * 1024).unwrap();
    let permissive = BodyPlan::PassThrough {
        max_chunk_bytes: 64,
    };
    let prepared = request_for_plan(&book, &budget, b"eightbyt", &permissive);
    let mut body_plans = plans();
    body_plans.attempt_request = BodyPlan::StreamingReplay {
        max_chunk_bytes: 4,
        max_replay_bytes: 4,
    };

    let result = AttemptExchange::new(
        RequestId(1),
        AttemptId(2),
        AttemptGeneration(3),
        PlanRevision(4),
        plain_target("127.0.0.1:8080".parse().unwrap(), 9),
        transport,
        prepared,
        body_plans,
        timeouts(),
        Instant::now() + Duration::from_secs(5),
        16,
        budget,
        4,
    );
    assert!(matches!(
        result,
        Err(AttemptError::Body(BodyError::BodyLimitExceeded))
    ));
    assert_eq!(book.outstanding(), 0);
}
