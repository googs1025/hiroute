use crate::fixture::*;

#[tokio::test]
async fn connect_request_write_first_byte_and_idle_timeouts_are_distinct() {
    let mut connect_target = plain_target("127.0.0.1:8080".parse().unwrap(), 201);
    connect_target.connect_timeout = Duration::from_millis(15);
    let mut connect = exchange_with_transport(
        TimedTransport {
            connect_pending: true,
            write_pending: false,
            head_written: false,
            events: VecDeque::new(),
            event_timer: None,
        },
        connect_target,
        short_timeouts(),
        Instant::now() + Duration::from_secs(1),
        1,
    );
    assert_eq!(
        connect.connect().await.unwrap_err(),
        AttemptError::ConnectTimeout
    );
    assert_eq!(
        connect.transport_facts().timeout,
        Some(AttemptTimeoutKind::Connect)
    );

    let mut request_write = exchange_with_transport(
        TimedTransport {
            connect_pending: false,
            write_pending: true,
            head_written: false,
            events: VecDeque::new(),
            event_timer: None,
        },
        plain_target("127.0.0.1:8080".parse().unwrap(), 202),
        short_timeouts(),
        Instant::now() + Duration::from_secs(1),
        1,
    );
    assert_eq!(
        request_write.drive_writer_once().await.unwrap_err(),
        AttemptError::RequestWriteTimeout
    );
    assert_eq!(
        request_write.transport_facts().timeout,
        Some(AttemptTimeoutKind::RequestWrite)
    );

    let mut first_byte = exchange_with_transport(
        timed_transport([]),
        plain_target("127.0.0.1:8080".parse().unwrap(), 203),
        short_timeouts(),
        Instant::now() + Duration::from_secs(1),
        1,
    );
    first_byte.drive_writer_once().await.unwrap();
    first_byte.drive_writer_once().await.unwrap();
    assert_eq!(
        first_byte
            .wait_precommit_event(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err(),
        AttemptError::FirstByteTimeout
    );
    assert_eq!(
        first_byte.transport_facts().timeout,
        Some(AttemptTimeoutKind::FirstByte)
    );

    let mut stream_idle = exchange_with_transport(
        timed_transport([
            (
                Duration::ZERO,
                TransportPrecommitEvent::ResponseHead {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                },
            ),
            (
                Duration::from_millis(60),
                TransportPrecommitEvent::Body(Bytes::from_static(b"late")),
            ),
        ]),
        plain_target("127.0.0.1:8080".parse().unwrap(), 204),
        short_timeouts(),
        Instant::now() + Duration::from_secs(1),
        1,
    );
    stream_idle.drive_writer_once().await.unwrap();
    assert!(matches!(
        stream_idle
            .wait_precommit_event(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap(),
        Some(PrecommitEvent::ResponseHead(_))
    ));
    stream_idle.drive_writer_once().await.unwrap();
    assert_eq!(
        stream_idle
            .wait_precommit_event(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err(),
        AttemptError::StreamIdleTimeout
    );
    assert_eq!(
        stream_idle.transport_facts().timeout,
        Some(AttemptTimeoutKind::StreamIdle)
    );
}

#[tokio::test]
async fn upstream_heartbeat_receipts_reset_idle_and_ttfb_is_transport_measured() {
    let mut exchange = exchange_with_transport(
        timed_transport([
            (
                Duration::from_millis(12),
                TransportPrecommitEvent::ResponseHead {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                },
            ),
            (
                Duration::from_millis(10),
                TransportPrecommitEvent::Body(Bytes::from_static(b": heartbeat\n\n")),
            ),
            (
                Duration::from_millis(10),
                TransportPrecommitEvent::Body(Bytes::from_static(b": heartbeat\n\n")),
            ),
            (
                Duration::from_millis(10),
                TransportPrecommitEvent::Body(Bytes::from_static(b"data: token\n\n")),
            ),
            (
                Duration::from_millis(10),
                TransportPrecommitEvent::EndStream,
            ),
        ]),
        plain_target("127.0.0.1:8080".parse().unwrap(), 205),
        short_timeouts(),
        Instant::now() + Duration::from_secs(1),
        1,
    );
    exchange.drive_writer_once().await.unwrap();
    exchange.drive_writer_once().await.unwrap();
    let head = exchange
        .wait_precommit_event(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    assert!(matches!(head, Some(PrecommitEvent::ResponseHead(_))));
    let ttfb = exchange
        .transport_facts()
        .upstream_ttfb
        .expect("response-head receipt records TTFB before downstream writes");
    assert!(ttfb >= Duration::from_millis(8), "TTFB was {ttfb:?}");

    let mut bodies = 0;
    loop {
        match exchange
            .wait_precommit_event(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap()
        {
            Some(PrecommitEvent::Body(_)) => bodies += 1,
            Some(PrecommitEvent::EndStream) => break,
            other => panic!("unexpected heartbeat event: {other:?}"),
        }
    }
    assert_eq!(bodies, 3);
    assert_eq!(exchange.transport_facts().timeout, None);
}

#[tokio::test]
async fn local_mailbox_backpressure_longer_than_idle_does_not_consume_idle_budget() {
    let reported_suppression = Arc::new(Mutex::new(None));
    let mut exchange = exchange_with_transport(
        SuppressionRaceTransport {
            head_written: false,
            stage: 0,
            head_received_at: None,
            suppression_started: None,
            suppression_completed: Duration::ZERO,
            reported_suppression: Arc::clone(&reported_suppression),
            upstream_gap: None,
        },
        plain_target("127.0.0.1:8080".parse().unwrap(), 206),
        short_timeouts(),
        Instant::now() + Duration::from_secs(1),
        1,
    );
    exchange.drive_writer_once().await.unwrap();
    tokio::time::sleep(Duration::from_millis(65)).await;
    assert!(matches!(
        exchange
            .wait_precommit_event(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap(),
        Some(PrecommitEvent::ResponseHead(_))
    ));
    let next = exchange
        .wait_precommit_event(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
    assert!(matches!(next, Some(PrecommitEvent::Body(_))));
    let expected = reported_suppression
        .lock()
        .unwrap()
        .expect("transport completed the mailbox suppression interval");
    assert_eq!(
        exchange.transport_facts().local_read_suppressed,
        expected,
        "receipt and cumulative suppression represent one interval and must be counted once"
    );
    assert_eq!(exchange.transport_facts().timeout, None);
}

#[tokio::test]
async fn absolute_attempt_deadline_clamps_longer_phase_timeouts() {
    let long = AttemptTimeouts {
        request_write: Duration::from_secs(1),
        first_byte: Duration::from_secs(1),
        stream_idle: Duration::from_secs(1),
    };
    let mut exchange = exchange_with_transport(
        timed_transport([]),
        plain_target("127.0.0.1:8080".parse().unwrap(), 207),
        long,
        Instant::now() + Duration::from_millis(20),
        1,
    );
    exchange.drive_writer_once().await.unwrap();
    exchange.drive_writer_once().await.unwrap();
    assert_eq!(
        exchange
            .wait_precommit_event(Instant::now() + Duration::from_secs(2))
            .await
            .unwrap_err(),
        AttemptError::DeadlineExceeded
    );
    assert_eq!(
        exchange.transport_facts().timeout,
        Some(AttemptTimeoutKind::AttemptDeadline)
    );
}

#[tokio::test]
async fn extreme_phase_durations_fail_closed_at_the_attempt_deadline_without_panicking() {
    let extreme = AttemptTimeouts {
        request_write: Duration::MAX,
        first_byte: Duration::MAX,
        stream_idle: Duration::MAX,
    };

    let mut connect_target = plain_target("127.0.0.1:8080".parse().unwrap(), 208);
    connect_target.connect_timeout = Duration::MAX;
    connect_target = connect_target.with_derived_connection_fingerprint();
    let mut connect = exchange_with_transport(
        TimedTransport {
            connect_pending: true,
            write_pending: false,
            head_written: false,
            events: VecDeque::new(),
            event_timer: None,
        },
        connect_target,
        extreme,
        Instant::now() + Duration::from_millis(15),
        1,
    );
    assert_eq!(
        connect.connect().await.unwrap_err(),
        AttemptError::DeadlineExceeded
    );

    let mut request_write = exchange_with_transport(
        TimedTransport {
            connect_pending: false,
            write_pending: true,
            head_written: false,
            events: VecDeque::new(),
            event_timer: None,
        },
        plain_target("127.0.0.1:8080".parse().unwrap(), 209),
        extreme,
        Instant::now() + Duration::from_millis(15),
        1,
    );
    assert_eq!(
        request_write.drive_writer_once().await.unwrap_err(),
        AttemptError::DeadlineExceeded
    );

    let mut first_byte = exchange_with_transport(
        timed_transport([]),
        plain_target("127.0.0.1:8080".parse().unwrap(), 210),
        extreme,
        Instant::now() + Duration::from_millis(15),
        1,
    );
    first_byte.drive_writer_once().await.unwrap();
    first_byte.drive_writer_once().await.unwrap();
    assert_eq!(
        first_byte
            .wait_precommit_event(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err(),
        AttemptError::DeadlineExceeded
    );

    let mut idle = exchange_with_transport(
        timed_transport([(
            Duration::ZERO,
            TransportPrecommitEvent::ResponseHead {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
            },
        )]),
        plain_target("127.0.0.1:8080".parse().unwrap(), 211),
        extreme,
        Instant::now() + Duration::from_millis(20),
        1,
    );
    idle.drive_writer_once().await.unwrap();
    assert!(matches!(
        idle.wait_precommit_event(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap(),
        Some(PrecommitEvent::ResponseHead(_))
    ));
    assert_eq!(
        idle.wait_precommit_event(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err(),
        AttemptError::DeadlineExceeded
    );
}
