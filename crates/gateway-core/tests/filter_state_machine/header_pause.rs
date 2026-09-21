use crate::fixture::*;

#[tokio::test]
async fn header_pause_body_continue_resumes_headers_before_same_frame() {
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
            ScriptAction::Data(DataAction::Continue(HeaderPatch::default().insert(
                "x-body-stage".parse().unwrap(),
                HeaderValue::from_static("yes"),
            ))),
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
    let token = match manager.on_headers(HeaderMap::new(), false).await.unwrap() {
        MachineOutcome::Paused(token) => token,
        _ => panic!("expected pause"),
    };
    manager
        .on_data(Bytes::from_static(b"body"), false)
        .await
        .unwrap();
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["f0:H", "f1:H", "f0:D", "f1:D", "f2:H", "f2:D"]
    );
    assert_eq!(manager.held_headers()["x-body-stage"], "yes");
    assert_eq!(
        manager
            .resume(token, ResumeAction::Continue(HeaderPatch::default()))
            .await
            .unwrap_err(),
        FilterError::StaleContinuation
    );
}

#[tokio::test]
async fn consecutive_header_stop_iterations_are_driven_by_the_same_eos_frame() {
    for (name, action, drops_body, mutates_body) in [
        (
            "continue",
            DataAction::Continue(HeaderPatch::default()),
            false,
            false,
        ),
        (
            "forward",
            DataAction::Emit {
                output: FilterBodyEmission::Forward,
                patch: HeaderPatch::default(),
            },
            false,
            false,
        ),
        (
            "drop",
            DataAction::Emit {
                output: FilterBodyEmission::Drop,
                patch: HeaderPatch::default(),
            },
            true,
            false,
        ),
        (
            "replace-empty",
            DataAction::Emit {
                output: FilterBodyEmission::Replace(
                    hiroute_gateway_core::core::filter::FilterBodyOutput::empty(),
                ),
                patch: HeaderPatch::default(),
            },
            false,
            true,
        ),
    ] {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (first, _) = ScriptFilter::new(
            "first",
            [
                ScriptAction::Headers(HeadersAction::StopIteration(HeaderPatch::default())),
                ScriptAction::Data(action),
            ],
            Arc::clone(&trace),
        );
        let first = if drops_body {
            first.dropping()
        } else if mutates_body {
            first.mutating()
        } else {
            first
        };
        let (second, _) = ScriptFilter::new(
            "second",
            [
                ScriptAction::Headers(HeadersAction::StopIteration(HeaderPatch::default())),
                ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
            ],
            Arc::clone(&trace),
        );
        let (last, _) = ScriptFilter::new(
            "last",
            [
                ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
                ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
            ],
            Arc::clone(&trace),
        );
        let mut manager = machine(vec![Box::new(first), Box::new(second), Box::new(last)]);
        let stale_first_token = match manager.on_headers(HeaderMap::new(), false).await.unwrap() {
            MachineOutcome::Paused(token) => token,
            other => panic!("{name}: expected first header pause, got {other:?}"),
        };

        assert!(matches!(
            manager
                .on_data(Bytes::from_static(b"only-frame"), true)
                .await
                .unwrap(),
            MachineOutcome::Complete
        ));
        assert_eq!(
            trace.lock().unwrap().as_slice(),
            [
                "first:H", "first:D", "second:H", "second:D", "last:H", "last:D",
            ],
            "{name}: the same terminal frame must drive every consecutive header stop"
        );
        assert_eq!(
            manager
                .resume(
                    stale_first_token,
                    ResumeAction::Continue(HeaderPatch::default()),
                )
                .await
                .unwrap_err(),
            FilterError::StaleContinuation,
            "{name}: body-driven resume must invalidate the original token"
        );
    }
}

#[tokio::test]
async fn stop_all_buffers_without_delivering_current_filter_until_resume() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (f0, _) = ScriptFilter::new(
        "f0",
        [
            ScriptAction::Headers(HeadersAction::StopAllIterationAndBuffer(
                HeaderPatch::default(),
            )),
            ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let (f1, _) = ScriptFilter::new(
        "f1",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let mut manager = machine(vec![Box::new(f0), Box::new(f1)]);
    let token = match manager.on_headers(HeaderMap::new(), false).await.unwrap() {
        MachineOutcome::Paused(token) => token,
        _ => panic!("expected pause"),
    };
    manager
        .on_data(Bytes::from_static(b"held"), false)
        .await
        .unwrap();
    assert_eq!(trace.lock().unwrap().as_slice(), ["f0:H"]);
    manager
        .resume(token, ResumeAction::Continue(HeaderPatch::default()))
        .await
        .unwrap();
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["f0:H", "f1:H", "f0:D", "f1:D"]
    );
}

#[tokio::test]
async fn no_buffer_requires_compiled_drop_capability() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let action = DataAction::StopIteration {
        retention: hiroute_gateway_core::core::filter::RetentionMode::NoBuffer,
        patch: HeaderPatch::default(),
    };
    let (f0, _) = ScriptFilter::new(
        "not-authorized",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::StopIteration {
                retention: hiroute_gateway_core::core::filter::RetentionMode::NoBuffer,
                patch: HeaderPatch::default(),
            }),
        ],
        Arc::clone(&trace),
    );
    let mut manager = machine(vec![Box::new(f0)]);
    manager.on_headers(HeaderMap::new(), false).await.unwrap();
    assert!(matches!(
        manager.on_data(Bytes::from_static(b"x"), false).await,
        Err(FilterError::BodyDropNotDeclared { .. })
    ));

    let (f1, _) = ScriptFilter::new(
        "authorized",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(action),
        ],
        trace,
    );
    let mut manager = machine(vec![Box::new(f1.dropping())]);
    manager.on_headers(HeaderMap::new(), false).await.unwrap();
    assert!(matches!(
        manager
            .on_data(Bytes::from_static(b"x"), false)
            .await
            .unwrap(),
        MachineOutcome::Paused(_)
    ));
}

#[tokio::test]
async fn header_stopped_no_buffer_pause_resumes_headers_without_forwarding_payload() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (stopped, _) = ScriptFilter::new(
        "stopped",
        [
            ScriptAction::Headers(HeadersAction::StopIteration(HeaderPatch::default())),
            ScriptAction::Data(DataAction::StopIteration {
                retention: hiroute_gateway_core::core::filter::RetentionMode::NoBuffer,
                patch: HeaderPatch::default(),
            }),
        ],
        Arc::clone(&trace),
    );
    let (later, _) = ScriptFilter::new(
        "later",
        [ScriptAction::Headers(HeadersAction::Continue(
            HeaderPatch::default(),
        ))],
        Arc::clone(&trace),
    );
    let mut manager = machine(vec![Box::new(stopped.dropping()), Box::new(later)]);
    let stale_header_token = match manager.on_headers(HeaderMap::new(), false).await.unwrap() {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected header pause: {other:?}"),
    };
    let data_token = match manager
        .on_data(Bytes::from_static(b"discarded"), false)
        .await
        .unwrap()
    {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected data pause: {other:?}"),
    };

    assert!(matches!(
        manager
            .resume(data_token, ResumeAction::Continue(HeaderPatch::default()))
            .await
            .unwrap(),
        MachineOutcome::Advanced
    ));
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["stopped:H", "stopped:D", "later:H"],
        "NoBuffer drops the payload but its continuation still advances suspended headers"
    );
    assert_eq!(
        manager
            .resume(
                stale_header_token,
                ResumeAction::Continue(HeaderPatch::default()),
            )
            .await
            .unwrap_err(),
        FilterError::StaleContinuation
    );
}

#[tokio::test]
async fn header_stopped_no_buffer_eos_pause_forwards_only_zero_byte_terminator() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (stopped, _) = ScriptFilter::new(
        "stopped",
        [
            ScriptAction::Headers(HeadersAction::StopIteration(HeaderPatch::default())),
            ScriptAction::Data(DataAction::StopIteration {
                retention: hiroute_gateway_core::core::filter::RetentionMode::NoBuffer,
                patch: HeaderPatch::default(),
            }),
        ],
        Arc::clone(&trace),
    );
    let (later, _) = ScriptFilter::new(
        "eos-observer",
        [
            ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
            ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
        ],
        Arc::clone(&trace),
    );
    let mut manager = machine(vec![Box::new(stopped.dropping()), Box::new(later)]);
    let stale_header_token = match manager.on_headers(HeaderMap::new(), false).await.unwrap() {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected header pause: {other:?}"),
    };
    let data_token = match manager
        .on_data(Bytes::from_static(b"discarded-terminal-payload"), true)
        .await
        .unwrap()
    {
        MachineOutcome::Paused(token) => token,
        other => panic!("expected data pause: {other:?}"),
    };

    assert!(matches!(
        manager
            .resume(data_token, ResumeAction::Continue(HeaderPatch::default()))
            .await
            .unwrap(),
        MachineOutcome::Complete
    ));
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        [
            "stopped:H",
            "stopped:D",
            "eos-observer:H",
            "eos-observer:D:0:true",
        ],
        "the dropped payload is not retained, while zero-byte EOS continues from the exact cursor"
    );
    assert_eq!(
        manager
            .resume(
                stale_header_token,
                ResumeAction::Continue(HeaderPatch::default()),
            )
            .await
            .unwrap_err(),
        FilterError::StaleContinuation
    );
}
