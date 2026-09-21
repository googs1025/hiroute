use crate::fixture::*;

#[tokio::test]
async fn encoder_runs_in_reverse_and_finalize_is_exactly_once() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (f0, c0) = ScriptFilter::new(
        "first",
        [ScriptAction::Headers(HeadersAction::Continue(
            HeaderPatch::default(),
        ))],
        Arc::clone(&trace),
    );
    let (f1, c1) = ScriptFilter::new(
        "second",
        [ScriptAction::Headers(HeadersAction::Continue(
            HeaderPatch::default(),
        ))],
        Arc::clone(&trace),
    );
    let mut manager = DirectionMachine::encoder(
        StreamId(1),
        ScopeId(1),
        ScopeKind::AcceptedResponse,
        vec![Box::new(f0), Box::new(f1)],
        2,
        Box::new(BoundedBodyRetention::new(16)),
        Box::new(FakeFramingLedger::default()),
    )
    .unwrap();
    manager.on_headers(HeaderMap::new(), true).await.unwrap();
    manager.finalize();
    manager.finalize();
    assert_eq!(trace.lock().unwrap()[..2], ["second:H", "first:H"]);
    assert_eq!(*c0.lock().unwrap(), 1);
    assert_eq!(*c1.lock().unwrap(), 1);
}

#[test]
fn scopes_and_local_reply_preserve_owner_boundaries() {
    let supervisor = ScopeSupervisor::default();
    let logical = supervisor.begin_logical().unwrap();
    let first = supervisor.begin_attempt().unwrap();
    assert_eq!(
        supervisor.begin_attempt().unwrap_err(),
        ScopeError::AttemptStillActive
    );
    drop(first);
    drop(supervisor.begin_attempt().unwrap());
    drop(supervisor.begin_accepted().unwrap());
    assert_eq!(
        supervisor.begin_accepted().unwrap_err(),
        ScopeError::AcceptedAlreadyStarted
    );
    drop(logical);
    assert_eq!(supervisor.counts().logical_finalized, 1);
    assert_eq!(supervisor.counts().attempts_started, 2);
    assert_eq!(supervisor.counts().attempts_finalized, 2);
    assert_eq!(supervisor.counts().accepted_finalized, 1);

    let reply = LocalReply {
        status: StatusCode::FORBIDDEN,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"denied"),
        provenance: hiroute_gateway_core::runtime::sse::SemanticProvenance::NonSemantic,
    };
    assert!(matches!(
        route_local_reply(
            ScopeKind::LogicalRequest,
            reply.clone(),
            UpstreamSemanticUse::default()
        ),
        RoutedLocalReply::TerminalLocalResponse(_)
    ));
    assert!(matches!(
        route_local_reply(
            ScopeKind::RouteAttempt,
            reply,
            UpstreamSemanticUse {
                connections: 1,
                ..UpstreamSemanticUse::default()
            }
        ),
        RoutedLocalReply::AttemptResponseCandidate {
            upstream_side_effects: false,
            ..
        }
    ));
}

#[tokio::test]
async fn trailers_are_queued_behind_paused_data() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (f0, _) = ScriptFilter::new(
        "f0",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::StopIteration {
                retention: hiroute_gateway_core::core::filter::RetentionMode::Buffer,
                patch: HeaderPatch::default(),
            }),
            ScriptAction::Trailers(TrailersAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let mut manager = machine(vec![Box::new(f0)]);
    manager.on_headers(HeaderMap::new(), false).await.unwrap();
    let token = match manager
        .on_data(Bytes::from_static(b"a"), false)
        .await
        .unwrap()
    {
        MachineOutcome::Paused(token) => token,
        _ => panic!("expected data pause"),
    };
    manager.on_trailers(HeaderMap::new()).await.unwrap();
    manager
        .resume(token, ResumeAction::Continue(HeaderPatch::default()))
        .await
        .unwrap();
    assert_eq!(trace.lock().unwrap().as_slice(), ["f0:H", "f0:D", "f0:T"]);
}

#[tokio::test]
async fn nested_data_pause_preserves_header_continuation_before_later_data() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (f0, _) = ScriptFilter::new(
        "f0",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::StopIteration {
                retention: hiroute_gateway_core::core::filter::RetentionMode::Buffer,
                patch: HeaderPatch::default(),
            }),
        ],
        Arc::clone(&trace),
    );
    let (f1, _) = ScriptFilter::new(
        "f1",
        [
            ScriptAction::Headers(HeadersAction::StopIteration(HeaderPatch::default())),
            ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let (f2, _) = ScriptFilter::new(
        "f2",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let mut manager = machine(vec![Box::new(f0), Box::new(f1), Box::new(f2)]);
    let header_token = match manager.on_headers(HeaderMap::new(), false).await.unwrap() {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected header pause: {other:?}"),
    };
    let data_token = match manager
        .on_data(Bytes::from_static(b"nested"), false)
        .await
        .unwrap()
    {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected nested data pause: {other:?}"),
    };
    assert_eq!(trace.lock().unwrap().as_slice(), ["f0:H", "f1:H", "f0:D"]);
    assert!(matches!(
        manager
            .resume(data_token, ResumeAction::Continue(HeaderPatch::default()))
            .await
            .unwrap(),
        MachineOutcome::Advanced
    ));
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["f0:H", "f1:H", "f0:D", "f1:D", "f2:H", "f2:D"],
        "later data must never run before that filter's headers"
    );
    assert_eq!(
        manager
            .resume(header_token, ResumeAction::Continue(HeaderPatch::default()))
            .await
            .unwrap_err(),
        FilterError::StaleContinuation
    );
}

#[tokio::test]
async fn header_stopped_filter_own_data_pause_resumes_later_headers_before_data() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (f0, _) = ScriptFilter::new(
        "f0",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let (f1, _) = ScriptFilter::new(
        "f1",
        [
            ScriptAction::Headers(HeadersAction::StopIteration(HeaderPatch::default())),
            ScriptAction::Data(DataAction::StopIteration {
                retention: hiroute_gateway_core::core::filter::RetentionMode::Buffer,
                patch: HeaderPatch::default(),
            }),
        ],
        Arc::clone(&trace),
    );
    let (f2, _) = ScriptFilter::new(
        "f2",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let mut manager = machine(vec![Box::new(f0), Box::new(f1), Box::new(f2)]);
    let header_token = match manager.on_headers(HeaderMap::new(), false).await.unwrap() {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected f1 header pause: {other:?}"),
    };
    let data_token = match manager
        .on_data(Bytes::from_static(b"same-filter-pause"), false)
        .await
        .unwrap()
    {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected f1 data pause: {other:?}"),
    };
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["f0:H", "f1:H", "f0:D", "f1:D"]
    );
    assert!(matches!(
        manager
            .resume(data_token, ResumeAction::Continue(HeaderPatch::default()))
            .await
            .unwrap(),
        MachineOutcome::Advanced
    ));
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["f0:H", "f1:H", "f0:D", "f1:D", "f2:H", "f2:D"]
    );
    assert_eq!(
        manager
            .resume(header_token, ResumeAction::Continue(HeaderPatch::default()))
            .await
            .unwrap_err(),
        FilterError::StaleContinuation
    );
}

#[tokio::test]
async fn header_pause_trailers_resume_propagates_complete() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (f0, _) = ScriptFilter::new(
        "f0",
        [
            ScriptAction::Headers(HeadersAction::StopAllIterationAndBuffer(
                HeaderPatch::default(),
            )),
            ScriptAction::Trailers(TrailersAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let (f1, _) = ScriptFilter::new(
        "f1",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Trailers(TrailersAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let mut manager = machine(vec![Box::new(f0), Box::new(f1)]);
    let token = match manager.on_headers(HeaderMap::new(), false).await.unwrap() {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected header pause: {other:?}"),
    };
    manager.on_trailers(HeaderMap::new()).await.unwrap();
    assert!(matches!(
        manager
            .resume(token, ResumeAction::Continue(HeaderPatch::default()))
            .await
            .unwrap(),
        MachineOutcome::Complete
    ));
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["f0:H", "f1:H", "f0:T", "f1:T"]
    );
}

#[tokio::test]
async fn header_stop_iteration_is_body_driven_by_trailers_eos() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (before, _) = ScriptFilter::new(
        "before",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Trailers(TrailersAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let (stopped, _) = ScriptFilter::new(
        "stopped",
        [
            ScriptAction::Headers(HeadersAction::StopIteration(HeaderPatch::default())),
            ScriptAction::Trailers(TrailersAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let (after, _) = ScriptFilter::new(
        "after",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Trailers(TrailersAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let mut manager = machine(vec![Box::new(before), Box::new(stopped), Box::new(after)]);
    let stale_header_token = match manager.on_headers(HeaderMap::new(), false).await.unwrap() {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected header stop: {other:?}"),
    };

    assert!(matches!(
        manager.on_trailers(HeaderMap::new()).await.unwrap(),
        MachineOutcome::Complete
    ));
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        [
            "before:H",
            "stopped:H",
            "before:T",
            "stopped:T",
            "after:H",
            "after:T",
        ]
    );
    assert_eq!(
        manager
            .resume(
                stale_header_token,
                ResumeAction::Continue(HeaderPatch::default()),
            )
            .await
            .unwrap_err(),
        FilterError::StaleContinuation,
    );
}

#[tokio::test]
async fn zero_byte_eos_and_paused_final_frame_both_propagate_complete() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (direct, _) = ScriptFilter::new(
        "direct",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let mut direct_manager = machine(vec![Box::new(direct)]);
    direct_manager
        .on_headers(HeaderMap::new(), false)
        .await
        .unwrap();
    assert!(matches!(
        direct_manager.on_data(Bytes::new(), true).await.unwrap(),
        MachineOutcome::Complete
    ));

    let paused_trace = Arc::new(Mutex::new(Vec::new()));
    let (f0, _) = ScriptFilter::new(
        "f0",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::StopIteration {
                retention: hiroute_gateway_core::core::filter::RetentionMode::Buffer,
                patch: HeaderPatch::default(),
            }),
        ],
        Arc::clone(&paused_trace),
    );
    let (f1, _) = ScriptFilter::new(
        "f1",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&paused_trace),
    );
    let mut paused_manager = machine(vec![Box::new(f0), Box::new(f1)]);
    paused_manager
        .on_headers(HeaderMap::new(), false)
        .await
        .unwrap();
    let token = match paused_manager.on_data(Bytes::new(), true).await.unwrap() {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected paused final frame: {other:?}"),
    };
    assert!(matches!(
        paused_manager
            .resume(token, ResumeAction::Continue(HeaderPatch::default()))
            .await
            .unwrap(),
        MachineOutcome::Complete
    ));
    assert_eq!(
        paused_trace.lock().unwrap().as_slice(),
        ["f0:H", "f1:H", "f0:D", "f1:D"]
    );
}
