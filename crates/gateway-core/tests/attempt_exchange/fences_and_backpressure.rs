use crate::fixture::*;

struct ResponseDuringEosTransport {
    body_written: bool,
    post_body_response_polls: usize,
    response_emitted: bool,
    finish_polls: Arc<AtomicUsize>,
}

#[async_trait]
impl AttemptTransport for ResponseDuringEosTransport {
    async fn connect(
        &mut self,
        _target: &hiroute_gateway_core::core::execution_plan::TransportTarget,
        _address: SocketAddr,
    ) -> Result<(), AttemptError> {
        Ok(())
    }

    async fn write_request_head(
        &mut self,
        _head: &PreparedRequestHead,
    ) -> Result<(), AttemptError> {
        Ok(())
    }

    async fn write_request_body(
        &mut self,
        _body: Bytes,
        _end_stream: bool,
    ) -> Result<(), AttemptError> {
        unreachable!("duplex transport uses poll_write_request_body")
    }

    async fn finish_request_body(&mut self) -> Result<(), AttemptError> {
        unreachable!("duplex transport uses poll_finish_request_body")
    }

    fn supports_duplex_request_body(&self) -> bool {
        true
    }

    fn poll_write_request_body(
        &mut self,
        _context: &mut Context<'_>,
        body: &mut Bytes,
        _end_stream: bool,
    ) -> Poll<Result<(), AttemptError>> {
        body.clear();
        self.body_written = true;
        Poll::Ready(Ok(()))
    }

    fn poll_finish_request_body(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<Result<(), AttemptError>> {
        self.finish_polls.fetch_add(1, Ordering::Relaxed);
        Poll::Ready(Ok(()))
    }

    fn poll_precommit(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<Result<Option<TransportPrecommitEvent>, AttemptError>> {
        if !self.body_written || self.response_emitted {
            return Poll::Pending;
        }
        self.post_body_response_polls += 1;
        if self.post_body_response_polls == 1 {
            return Poll::Pending;
        }
        self.response_emitted = true;
        Poll::Ready(Ok(Some(TransportPrecommitEvent::ResponseHead {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
        })))
    }

    async fn cancel_reset(&mut self) -> Result<(), AttemptError> {
        Ok(())
    }

    fn protocol(&self) -> HttpProtocol {
        HttpProtocol::Http1
    }
}

#[tokio::test]
async fn duplex_response_preemption_resumes_eos_writer_on_next_owner_turn() {
    let finish_polls = Arc::new(AtomicUsize::new(0));
    let transport = ResponseDuringEosTransport {
        body_written: false,
        post_body_response_polls: 0,
        response_emitted: false,
        finish_polls: Arc::clone(&finish_polls),
    };
    let leases = RequestLeaseBook::new();
    let budgets = BudgetTree::new(1024 * 1024, 1024 * 1024).unwrap();
    let budget = budgets.stream(512 * 1024).unwrap();
    let mut exchange = AttemptExchange::new(
        RequestId(88),
        AttemptId(89),
        AttemptGeneration(90),
        PlanRevision(91),
        plain_target("127.0.0.1:8080".parse().unwrap(), 12),
        transport,
        request(&leases, &budget, b"body"),
        plans(),
        timeouts(),
        Instant::now() + Duration::from_secs(5),
        1,
        budget,
        4,
    )
    .unwrap();

    exchange.drive_writer_once().await.unwrap();
    exchange.drive_writer_once().await.unwrap();
    exchange.drive_writer_once().await.unwrap();
    assert_eq!(exchange.snapshot().writer_state, WriterState::EosWriting);
    assert_eq!(finish_polls.load(Ordering::Relaxed), 0);
    assert!(matches!(
        exchange.next_precommit_event(),
        Some(PrecommitEvent::ResponseHead(head)) if head.status() == StatusCode::OK
    ));

    exchange.drive_writer_once().await.unwrap();
    assert_eq!(
        exchange.snapshot().writer_state,
        WriterState::QuiescedNormalEos
    );
    assert_eq!(finish_polls.load(Ordering::Relaxed), 1);
    assert_eq!(leases.outstanding(), 0);
}

#[tokio::test]
async fn non_accept_publication_and_downstream_writes_are_guarded_by_both_fences() {
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
    let mut exchange = exchange(transport, &book, b"body");

    assert_eq!(
        exchange.begin_downstream_header_write().unwrap_err(),
        AttemptError::DownstreamWriteBeforeDisposition
    );
    assert_eq!(
        exchange.begin_semantic_output_write().unwrap_err(),
        AttemptError::DownstreamWriteBeforeDisposition
    );
    assert_eq!(
        exchange.begin_accepted_response_scope().unwrap_err(),
        AttemptError::DownstreamWriteBeforeDisposition
    );

    exchange
        .submit_disposition_candidate(Disposition::Continue)
        .unwrap();
    let permit = match exchange
        .wait_writer_gate(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap()
    {
        WriterGate::ReadyToPublishNonAccept { permit, .. } => permit,
        other => panic!("unexpected writer gate: {other:?}"),
    };

    // The linear owner cannot advance either fence between gate and the
    // atomic publication point through the public write API.
    assert_eq!(
        exchange.begin_downstream_header_write().unwrap_err(),
        AttemptError::DownstreamWriteBeforeDisposition
    );
    exchange
        .publish_disposition(Disposition::Continue, permit)
        .unwrap();
    assert_eq!(
        exchange.begin_accepted_response_scope().unwrap_err(),
        AttemptError::ContinueCannotWriteDownstream
    );
    let snapshot = exchange.snapshot();
    assert_eq!(snapshot.downstream_header_fence, CommitFence::Clear);
    assert_eq!(snapshot.downstream_semantic_fence, CommitFence::Clear);
    assert!(!snapshot.accepted_response_scope_created);
}

#[tokio::test]
async fn charged_precommit_mailbox_is_fixed_capacity_and_releases_on_continue() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let large_backing = Bytes::from(vec![b'z'; 1024 * 1024]);
    let transport = MockTransport {
        stats: Arc::clone(&stats),
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::from([
            TransportPrecommitEvent::Body(large_backing.slice(0..1)),
            TransportPrecommitEvent::Body(Bytes::from_static(b"b")),
            TransportPrecommitEvent::Body(Bytes::from_static(b"c")),
        ]),
        protocol: HttpProtocol::Http1,
    };
    drop(large_backing);
    let leases = RequestLeaseBook::new();
    let budgets = BudgetTree::new(2 * 1024 * 1024, 2 * 1024 * 1024).unwrap();
    let budget = budgets.stream(1024 * 1024).unwrap();
    let mut exchange = AttemptExchange::new(
        RequestId(80),
        AttemptId(81),
        AttemptGeneration(82),
        PlanRevision(83),
        plain_target("127.0.0.1:8080".parse().unwrap(), 10),
        transport,
        request(&leases, &budget, b""),
        plans(),
        timeouts(),
        Instant::now() + Duration::from_secs(5),
        2,
        budget.clone(),
        4,
    )
    .unwrap();

    exchange.drive_writer_once().await.unwrap();
    exchange.drive_writer_once().await.unwrap();
    exchange.drive_writer_once().await.unwrap();
    assert_eq!(
        stats.lock().unwrap().response_polls,
        2,
        "a full precommit mailbox must backpressure response reads"
    );
    let first = exchange.next_precommit_event().unwrap();
    let PrecommitEvent::Body(first) = first else {
        panic!("expected charged response body")
    };
    assert_eq!(first.bytes(), &Bytes::from_static(b"z"));
    assert_eq!(
        first.retained_capacity(),
        1,
        "a tiny visible slice must not pin the 1 MiB transport backing"
    );
    drop(first);
    drop(exchange.next_precommit_event());

    exchange
        .submit_disposition_candidate(Disposition::Continue)
        .unwrap();
    let permit = match exchange
        .wait_writer_gate(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap()
    {
        WriterGate::ReadyToPublishNonAccept { permit, .. } => permit,
        other => panic!("unexpected gate: {other:?}"),
    };
    exchange
        .publish_disposition(Disposition::Continue, permit)
        .unwrap();
    assert_eq!(leases.outstanding(), 0);
    let snapshot = budget.snapshot().unwrap();
    assert_eq!(
        snapshot.live, 0,
        "Continue releases queue, body and mailbox roles"
    );
    assert_eq!(snapshot.role_live[MemoryRole::ResponsePrefix as usize], 0);
    assert_eq!(snapshot.role_live[MemoryRole::AttemptWire as usize], 0);
}

#[tokio::test]
async fn full_precommit_window_backpressures_reader_but_accept_still_writes_normal_eos() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats: Arc::clone(&stats),
        connect_failures: 0,
        write_head_fails: false,
        reset_hangs: false,
        events: VecDeque::from([
            TransportPrecommitEvent::ResponseHead {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
            },
            TransportPrecommitEvent::Body(Bytes::from_static(b"prefix-two")),
        ]),
        protocol: HttpProtocol::Http2,
    };
    let leases = RequestLeaseBook::new();
    let budgets = BudgetTree::new(1024 * 1024, 1024 * 1024).unwrap();
    let budget = budgets.stream(512 * 1024).unwrap();
    let prepared = request(&leases, &budget, &[b'p'; 32]);
    let mut exchange = AttemptExchange::new(
        RequestId(84),
        AttemptId(85),
        AttemptGeneration(86),
        PlanRevision(87),
        plain_target("127.0.0.1:8080".parse().unwrap(), 11),
        transport,
        prepared,
        plans(),
        timeouts(),
        Instant::now() + Duration::from_secs(5),
        1,
        budget,
        4,
    )
    .unwrap();

    exchange.drive_writer_once().await.unwrap();
    assert_eq!(stats.lock().unwrap().response_polls, 1);
    exchange
        .submit_disposition_candidate(Disposition::Accept)
        .unwrap();
    let gate = exchange
        .wait_writer_gate(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    assert!(matches!(gate, WriterGate::ReadyToPublishAccept { .. }));
    let snapshot = exchange.snapshot();
    assert_eq!(snapshot.writer_state, WriterState::QuiescedNormalEos);
    let stats = stats.lock().unwrap();
    assert_eq!(stats.body_bytes, 32);
    assert_eq!(stats.finishes, 1);
    assert_eq!(stats.resets, 0);
    assert_eq!(
        stats.response_polls, 1,
        "a full window must stop response polling while request EOS progresses"
    );
    drop(stats);
    assert!(matches!(
        exchange.next_precommit_event(),
        Some(PrecommitEvent::ResponseHead(head)) if head.status() == StatusCode::OK
    ));
    assert!(exchange.next_precommit_event().is_none());
}

#[tokio::test]
async fn failed_header_write_leaves_conservative_fence_and_forbids_reconnect() {
    let stats = Arc::new(Mutex::new(TransportStats::default()));
    let transport = MockTransport {
        stats,
        connect_failures: 0,
        write_head_fails: true,
        reset_hangs: false,
        events: VecDeque::new(),
        protocol: HttpProtocol::Http1,
    };
    let book = RequestLeaseBook::new();
    let mut exchange = exchange(transport, &book, b"body");
    assert!(exchange.drive_writer_once().await.is_err());
    assert_eq!(
        exchange.snapshot().upstream_request_fence,
        CommitFence::WriteStartedMayHaveCommitted
    );
    assert_eq!(
        exchange.connect().await.unwrap_err(),
        AttemptError::ReconnectAfterCommit
    );
}
