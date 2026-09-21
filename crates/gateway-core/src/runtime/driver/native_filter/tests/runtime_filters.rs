use super::*;

#[test]
fn runtime_filter_retention_is_charged_until_returned_backing_drops() {
    let tree = BudgetTree::new(4096, 4096).expect("budget tree");
    let budget = tree.stream(4096).expect("stream budget");
    let mut retention = RuntimeBodyRetention::new(1024, 2, &budget).expect("retention");
    let metadata_live = budget.snapshot().expect("metadata snapshot").live;

    let id = retention
        .retain(Bytes::from_static(b"body"))
        .expect("retain body");
    let retained_live = budget.snapshot().expect("retained snapshot").live;
    assert!(retained_live > metadata_live);

    let returned = retention.take(id).expect("take retained body");
    assert_eq!(
        budget.snapshot().expect("returned snapshot").live,
        retained_live,
        "the returned Bytes owner still pins the retained backing charge",
    );
    drop(returned);
    assert_eq!(
        budget.snapshot().expect("released snapshot").live,
        metadata_live,
    );
    drop(retention);
    assert_eq!(budget.snapshot().expect("final snapshot").live, 0);
}

#[tokio::test]
async fn production_no_buffer_releases_only_current_root_not_promoted_merge_lineage() {
    let tree = BudgetTree::new(1024 * 1024, 1024 * 1024).expect("budget tree");
    let budget = tree.stream(1024 * 1024).expect("stream budget");
    let continuations = Arc::new(ProductionLineageContinuations::default());
    let mut filters = production_lineage_filters(Arc::clone(&continuations));
    let descriptors = production_lineage_descriptors();
    let mut head = GatewayRequestHead {
        method: Method::POST,
        path_and_query: Arc::from("/lineage"),
        authority: Some(Arc::from("gateway.test")),
        headers: HeaderMap::new(),
        protocol: HttpProtocol::Http1,
    };
    let headers = filters
        .begin_logical_request(
            &descriptors,
            production_filter_context(ScopeKind::LogicalRequest, &budget),
            &mut head,
        )
        .await
        .expect("begin lineage filters");
    assert_eq!(headers.pause, Some(FilterPause::Buffer));

    for (bytes, end_stream) in [(b"A".as_slice(), false), (b"B".as_slice(), true)] {
        let body = ChargedBytes::copy_from_opaque(&budget, MemoryRole::RawRequest, bytes)
            .expect("lineage input");
        let buffered = filters
            .filter_logical_request_body(
                &mut head,
                LogicalRequestBodyFrame {
                    bytes: Some(body),
                    end_stream,
                    queue_metadata: BodyMetadataOwner::default(),
                },
                FilterConfigSnapshot::default(),
            )
            .await
            .expect("buffer lineage input");
        assert_eq!(buffered.pause, Some(FilterPause::Buffer));
        assert!(buffered.frames.is_empty());
    }

    resume_lineage(&continuations.header, "buffer header");
    let nested_pause = filters
        .try_logical_resume(&mut head)
        .await
        .expect("drain buffered lineage")
        .expect("signalled buffer continuation");
    assert_eq!(nested_pause.pause, Some(FilterPause::Buffer));
    assert!(nested_pause.frames.is_empty());
    assert_eq!(
        budget.snapshot().expect("nested pause budget").role_live[MemoryRole::RawRequest as usize],
        1,
        "NoBuffer consumes only B/current replacement; forwarded A remains owned by runtime",
    );

    resume_lineage(&continuations.data, "NoBuffer data");
    let resumed = filters
        .try_logical_resume(&mut head)
        .await
        .expect("resume lineage NoBuffer")
        .expect("signalled NoBuffer continuation");
    assert_eq!(resumed.pause, None);
    let mut frames = resumed.frames.into_units();
    let forwarded_a = frames.pop_front().expect("forwarded A");
    assert_eq!(
        forwarded_a
            .bytes
            .as_ref()
            .expect("A backing")
            .bytes()
            .as_ref(),
        b"A",
    );
    assert!(!forwarded_a.end_stream);
    let eos = frames.pop_front().expect("typed EOS");
    assert!(frames.is_empty());
    assert!(eos.bytes.is_none());
    assert!(eos.end_stream);
    drop(forwarded_a);
    drop(eos);
    assert_eq!(
        budget
            .snapshot()
            .expect("resolved lineage budget")
            .role_live[MemoryRole::RawRequest as usize],
        0,
    );
    filters.finalize();
    assert_eq!(budget.snapshot().expect("final lineage budget").live, 0);
}

#[tokio::test]
async fn attempt_response_metadata_survives_runtime_root_release_until_emission() {
    let tree = BudgetTree::new(1024 * 1024, 1024 * 1024).expect("budget tree");
    let budget = tree.stream(1024 * 1024).expect("stream budget");
    let continuations = Arc::new(ProductionLineageContinuations::default());
    let mut filters = production_attempt_lineage_filters(Arc::clone(&continuations));
    let descriptors = production_attempt_lineage_descriptors();
    filters
        .begin_attempt(
            &[],
            &descriptors,
            production_filter_context(ScopeKind::RouteAttempt, &budget),
        )
        .expect("begin attempt response filters");
    let header_outcome = filters
        .attempt_response
        .as_mut()
        .expect("attempt response machine")
        .on_headers(HeaderMap::new(), false)
        .await
        .expect("attempt response headers");
    assert!(matches!(header_outcome, MachineOutcome::Paused(_)));

    for byte in *b"AB" {
        let event = PrecommitEvent::Body(
            ChargedBytes::copy_from_opaque(
                &budget,
                MemoryRole::ResponsePrefix,
                std::slice::from_ref(&byte),
            )
            .expect("attempt response input"),
        );
        let buffered = filters
            .filter_attempt_response_event(event, FilterConfigSnapshot::default())
            .await
            .expect("buffer attempt response input");
        assert_eq!(buffered.pause, Some(FilterPause::Buffer));
        assert!(buffered.frames.is_empty());
    }

    resume_lineage(&continuations.header, "attempt buffer header");
    let nested_pause = filters
        .try_attempt_response_resume()
        .await
        .expect("drain attempt response buffer")
        .expect("signalled attempt header continuation");
    assert_eq!(nested_pause.pause, Some(FilterPause::Buffer));
    assert!(nested_pause.frames.is_empty());
    assert_eq!(
        budget.snapshot().expect("attempt nested pause").role_live
            [MemoryRole::ResponsePrefix as usize],
        1,
        "the replacement A remains charged while both original runtime roots are released",
    );

    resume_lineage(&continuations.data, "attempt NoBuffer data");
    let resumed = filters
        .try_attempt_response_resume()
        .await
        .expect("resume attempt NoBuffer")
        .expect("signalled attempt data continuation");
    assert_eq!(resumed.pause, None);
    let mut frames = resumed.frames.into_units();
    let emitted = frames.pop_front().expect("replacement A");
    assert!(frames.is_empty());
    match emitted {
        PrecommitEvent::Body(bytes) => assert_eq!(bytes.bytes().as_ref(), b"A"),
        other => panic!("expected body A, got {other:?}"),
    }

    let eos = filters
        .filter_attempt_response_event(PrecommitEvent::EndStream, FilterConfigSnapshot::default())
        .await
        .expect("filter attempt EOS");
    let mut eos_frames = eos.frames.into_units();
    assert!(matches!(
        eos_frames.pop_front(),
        Some(PrecommitEvent::EndStream)
    ));
    assert!(eos_frames.is_empty());
    filters.finish_attempt();
    assert_eq!(budget.snapshot().expect("attempt lineage released").live, 0);
}

#[tokio::test]
async fn production_no_buffer_pause_releases_payload_and_resolves_typed_eos_in_all_scopes() {
    const PAYLOAD_BYTES: usize = 1024;

    // LogicalRequest decoder.
    {
        let tree = BudgetTree::new(1024 * 1024, 1024 * 1024).expect("budget tree");
        let budget = tree.stream(1024 * 1024).expect("stream budget");
        let continuation = Arc::new(Mutex::new(None));
        let mut filters = production_no_buffer_filters(Arc::clone(&continuation));
        let descriptor = production_no_buffer_descriptor();
        let mut head = GatewayRequestHead {
            method: Method::POST,
            path_and_query: Arc::from("/logical"),
            authority: Some(Arc::from("gateway.test")),
            headers: HeaderMap::new(),
            protocol: HttpProtocol::Http1,
        };
        let headers = filters
            .begin_logical_request(
                std::slice::from_ref(&descriptor),
                production_filter_context(ScopeKind::LogicalRequest, &budget),
                &mut head,
            )
            .await
            .expect("begin logical filters");
        assert_eq!(headers.pause, Some(FilterPause::HeaderIteration));
        let body =
            ChargedBytes::copy_from_opaque(&budget, MemoryRole::RawRequest, &[b'l'; PAYLOAD_BYTES])
                .expect("logical payload");
        let paused = filters
            .filter_logical_request_body(
                &mut head,
                LogicalRequestBodyFrame {
                    bytes: Some(body),
                    end_stream: true,
                    queue_metadata: BodyMetadataOwner::default(),
                },
                FilterConfigSnapshot::default(),
            )
            .await
            .expect("logical NoBuffer pause");
        assert_eq!(paused.pause, Some(FilterPause::Buffer));
        assert!(paused.frames.is_empty());
        assert_eq!(
            budget.snapshot().expect("logical paused budget").role_live
                [MemoryRole::RawRequest as usize],
            0,
            "the logical payload owner is released before the pause escapes the manager",
        );
        resume_production_no_buffer(&continuation);
        let resumed = filters
            .try_logical_resume(&mut head)
            .await
            .expect("logical resume")
            .expect("signalled logical continuation");
        let mut frames = resumed.frames.into_units();
        let eos = frames.pop_front().expect("typed logical EOS");
        assert!(frames.is_empty());
        assert!(eos.bytes.is_none());
        assert!(eos.end_stream);
        filters.finalize();
        assert_eq!(budget.snapshot().expect("logical released budget").live, 0);
    }

    // RouteAttempt request encoder.
    {
        let tree = BudgetTree::new(1024 * 1024, 1024 * 1024).expect("budget tree");
        let budget = tree.stream(1024 * 1024).expect("stream budget");
        let continuation = Arc::new(Mutex::new(None));
        let mut filters = production_no_buffer_filters(Arc::clone(&continuation));
        let descriptor = production_no_buffer_descriptor();
        filters
            .begin_attempt(
                std::slice::from_ref(&descriptor),
                &[],
                production_filter_context(ScopeKind::RouteAttempt, &budget),
            )
            .expect("begin attempt filters");
        let mut head = PreparedRequestHead {
            method: Method::POST,
            path_and_query: Arc::from("/attempt"),
            headers: HeaderMap::new(),
        };
        let headers = filters
            .filter_attempt_request_head(&mut head)
            .await
            .expect("attempt request headers");
        assert_eq!(headers.pause, Some(FilterPause::HeaderIteration));
        let body = ChargedBytes::copy_from_opaque(
            &budget,
            MemoryRole::AttemptWire,
            &[b'a'; PAYLOAD_BYTES],
        )
        .expect("attempt payload");
        let paused = filters
            .filter_attempt_request_body(
                &mut head,
                AttemptRequestBodyFrame {
                    bytes: Some(body),
                    end_stream: true,
                    queue_metadata: BodyMetadataOwner::default(),
                },
                FilterConfigSnapshot::default(),
            )
            .await
            .expect("attempt NoBuffer pause");
        assert_eq!(paused.pause, Some(FilterPause::Buffer));
        assert!(paused.frames.is_empty());
        assert_eq!(
            budget.snapshot().expect("attempt paused budget").role_live
                [MemoryRole::AttemptWire as usize],
            0,
            "the attempt payload owner is released before the pause escapes the manager",
        );
        resume_production_no_buffer(&continuation);
        let resumed = filters
            .try_attempt_request_resume(&mut head)
            .await
            .expect("attempt resume")
            .expect("signalled attempt continuation");
        let mut frames = resumed.frames.into_units();
        let eos = frames.pop_front().expect("typed attempt EOS");
        assert!(frames.is_empty());
        assert!(eos.bytes.is_none());
        assert!(eos.end_stream);
        filters.finish_attempt();
        assert_eq!(budget.snapshot().expect("attempt released budget").live, 0);
    }

    // AcceptedResponse encoder.
    {
        let tree = BudgetTree::new(1024 * 1024, 1024 * 1024).expect("budget tree");
        let budget = tree.stream(1024 * 1024).expect("stream budget");
        let continuation = Arc::new(Mutex::new(None));
        let mut filters = production_no_buffer_filters(Arc::clone(&continuation));
        let descriptor = production_no_buffer_descriptor();
        filters
            .begin_accepted_response(
                std::slice::from_ref(&descriptor),
                production_filter_context(ScopeKind::AcceptedResponse, &budget),
            )
            .expect("begin accepted filters");
        let mut head = GatewayResponseHead {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
        };
        let headers = filters
            .filter_accepted_head(&mut head)
            .await
            .expect("accepted response headers");
        assert_eq!(headers.pause, Some(FilterPause::HeaderIteration));
        let body = ChargedBytes::copy_from_opaque(
            &budget,
            MemoryRole::OutputQueue,
            &[b'r'; PAYLOAD_BYTES],
        )
        .expect("accepted payload");
        let paused = filters
            .filter_accepted_body(
                &mut head,
                AcceptedBodyFrame {
                    output: Some(EncodedOutputUnit {
                        bytes: body,
                        provenance: SemanticProvenance::ProducesSemantic,
                    }),
                    end_stream: true,
                    sse_sources: SseTransformSources::default(),
                    queue_metadata: BodyMetadataOwner::default(),
                },
                FilterConfigSnapshot::default(),
            )
            .await
            .expect("accepted NoBuffer pause");
        assert_eq!(paused.pause, Some(FilterPause::Buffer));
        assert!(paused.frames.is_empty());
        assert_eq!(
            budget.snapshot().expect("accepted paused budget").role_live
                [MemoryRole::OutputQueue as usize],
            0,
            "the accepted payload owner is released before the pause escapes the manager",
        );
        resume_production_no_buffer(&continuation);
        let resumed = filters
            .try_accepted_resume(&mut head)
            .await
            .expect("accepted resume")
            .expect("signalled accepted continuation");
        let mut frames = resumed.frames.into_units();
        let eos = frames.pop_front().expect("typed accepted EOS");
        assert!(frames.is_empty());
        assert!(eos.output.is_none());
        assert!(eos.end_stream);
        assert!(eos.sse_sources.as_slice().is_empty());
        filters.finalize();
        assert_eq!(budget.snapshot().expect("accepted released budget").live, 0);
    }
}

#[tokio::test]
async fn logical_replacement_resolver_never_creates_a_second_output_backing() {
    const PAYLOAD_BYTES: usize = 1024;
    let tree = BudgetTree::new(64 * 1024, 64 * 1024).expect("budget tree");
    let budget = tree.stream(64 * 1024).expect("stream budget");
    let executor =
        |kind| BoundedExecutor::new(kind, 1, 2).expect("constant replacement executor limits");
    let services = FilterExecutorServices::new(
        Instant::now() + Duration::from_secs(1),
        CancellationToken::new(),
        budget.clone(),
        executor(ExecutorKind::SidecallIo),
        executor(ExecutorKind::Compute),
        executor(ExecutorKind::BlockingControl),
    );
    let mut machine = DirectionMachine::decoder_with_context(
        StreamId(91),
        ScopeId(91),
        ScopeKind::LogicalRequest,
        vec![Box::new(ExactReplacementFilter)],
        1,
        Box::new(
            RuntimeBodyRetention::new(PAYLOAD_BYTES * 2, 1, &budget).expect("runtime retention"),
        ),
        Box::new(FramingLedger::default()),
        FilterCallbackContext::with_services(services),
    )
    .expect("direction machine")
    .with_body_output_role(MemoryRole::RawRequest);
    machine
        .on_headers(HeaderMap::new(), false)
        .await
        .expect("logical headers");

    let input =
        ChargedBytes::copy_from_opaque(&budget, MemoryRole::RawRequest, &[b'x'; PAYLOAD_BYTES])
            .expect("source body");
    let input_view = input.bytes().clone();
    let source = FilterBodySourceId::new(91);
    let mut pending = VecDeque::from([PendingFilterFrame {
        source: Some(source.clone()),
        frame: LogicalRequestBodyFrame {
            bytes: Some(input),
            end_stream: true,
            queue_metadata: BodyMetadataOwner::default(),
        },
    }]);
    let outcome = machine
        .on_data_with_source_and_metadata(input_view, true, source, BodyMetadataOwner::default())
        .await
        .expect("replacement callback");
    let before_resolve = budget.snapshot().expect("pre-resolve snapshot");
    assert_eq!(
        before_resolve.role_live[MemoryRole::RawRequest as usize],
        PAYLOAD_BYTES * 2,
        "the source and one replacement are the only RawRequest payload owners",
    );

    let result = NativeGatewayRequestFilters::resolve_logical_frames(
        &mut machine,
        &mut pending,
        &budget,
        outcome,
    )
    .expect("linear replacement resolution");
    let after_resolve = budget.snapshot().expect("post-resolve snapshot");
    assert_eq!(
        after_resolve.role_live[MemoryRole::RawRequest as usize],
        PAYLOAD_BYTES,
        "resolution drops the source and transfers the existing replacement",
    );
    assert_eq!(
        after_resolve.role_peak[MemoryRole::RawRequest as usize],
        PAYLOAD_BYTES * 2,
        "resolution must not allocate a second full replacement backing",
    );
    drop(result);
    drop(machine);
    assert_eq!(budget.snapshot().expect("released replacement").live, 0);
}
