use crate::fixture::*;

#[tokio::test]
async fn dropped_or_empty_replaced_final_frame_still_reaches_later_filter_as_eos() {
    for (name, action, configure) in [
        (
            "drop",
            DataAction::Emit {
                output: FilterBodyEmission::Drop,
                patch: HeaderPatch::default(),
            },
            false,
        ),
        (
            "empty-replace",
            DataAction::Emit {
                output: FilterBodyEmission::Replace(
                    hiroute_gateway_core::core::filter::FilterBodyOutput::empty(),
                ),
                patch: HeaderPatch::default(),
            },
            true,
        ),
    ] {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (first, _) = ScriptFilter::new(
            name,
            [
                ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
                ScriptAction::Data(action),
            ],
            Arc::clone(&trace),
        );
        let first = if configure {
            first.mutating()
        } else {
            first.dropping()
        };
        let (later, _) = ScriptFilter::new(
            "eos-observer",
            [
                ScriptAction::Headers(HeadersAction::Continue(HeaderPatch::default())),
                ScriptAction::Data(DataAction::Continue(HeaderPatch::default())),
            ],
            Arc::clone(&trace),
        );
        let mut manager = machine(vec![Box::new(first), Box::new(later)]);
        manager.on_headers(HeaderMap::new(), false).await.unwrap();
        assert!(matches!(
            manager
                .on_data(Bytes::from_static(b"last"), true)
                .await
                .unwrap(),
            MachineOutcome::Complete
        ));
        assert_eq!(
            trace.lock().unwrap().as_slice(),
            [
                format!("{name}:H"),
                "eos-observer:H".to_owned(),
                format!("{name}:D"),
                "eos-observer:D:0:true".to_owned()
            ]
        );
    }
}

#[tokio::test]
async fn production_direction_machine_emits_pause_resume_and_finalize_transitions() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (filter, _) = ScriptFilter::new(
        "observed",
        [ScriptAction::Headers(HeadersAction::StopIteration(
            HeaderPatch::default(),
        ))],
        trace,
    );
    let sink = Arc::new(FilterTelemetrySink::default());
    let telemetry = Arc::new(Telemetry::new(sink.clone()));
    let request = RequestTelemetry::new(
        Arc::clone(&telemetry),
        Correlation {
            authority_id: AuthorityId::new("telemetry-test").unwrap(),
            authority_epoch: 1,
            config_revision: ConfigRevision(1),
            plan_revision: PlanRevision(1),
            config_generations: Arc::new([]),
            stable_target_key: None,
            binding_local_id: None,
            request_id: None,
            decision_id: None,
            attempt_id: None,
            attempt_generation: None,
        },
    );
    let mut manager = machine(vec![Box::new(filter)]).with_telemetry(request);
    let token = match manager.on_headers(HeaderMap::new(), false).await.unwrap() {
        MachineOutcome::Paused(token) => token,
        outcome => panic!("expected pause, got {outcome:?}"),
    };
    manager
        .resume(token, ResumeAction::Continue(HeaderPatch::default()))
        .await
        .unwrap();
    manager.finalize();
    telemetry.flush(Duration::from_secs(1)).unwrap();

    let events = sink.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Scope(ScopeFact {
            phase: ScopePhase::Paused,
            ..
        })
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Scope(ScopeFact {
            phase: ScopePhase::Headers,
            ..
        })
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        LifecycleKind::Scope(ScopeFact {
            phase: ScopePhase::Finalized,
            finalize_count: 1,
            ..
        })
    )));
}
