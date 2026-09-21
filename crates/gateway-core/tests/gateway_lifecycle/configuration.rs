use crate::fixture::*;

#[tokio::test]
async fn runtime_attempt_context_uses_one_group_snapshot_across_mid_callback_publish()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"config-snapshot", false).await
    });
    let descriptors = [
        ConfigCellDescriptor {
            id: ConfigCellId(101),
            compatibility_hash: [1; 32],
            atomicity_group: AtomicityGroupId(101),
            binding_policy: ConfigBindingPolicy::AttemptPinned,
        },
        ConfigCellDescriptor {
            id: ConfigCellId(102),
            compatibility_hash: [2; 32],
            atomicity_group: AtomicityGroupId(101),
            binding_policy: ConfigBindingPolicy::AttemptPinned,
        },
    ];
    let bundle = |generation| {
        Arc::new(ConfigBundle::new(
            AtomicityGroupId(101),
            HashMap::from([
                (
                    ConfigCellId(101),
                    ImmutableConfig {
                        generation: ConfigGeneration(generation),
                        compatibility_hash: [1; 32],
                        bytes: Arc::from([1, generation as u8]),
                    },
                ),
                (
                    ConfigCellId(102),
                    ImmutableConfig {
                        generation: ConfigGeneration(generation),
                        compatibility_hash: [2; 32],
                        bytes: Arc::from([2, generation as u8]),
                    },
                ),
            ]),
        ))
    };
    let group = ConfigCellGroup::new(descriptors, bundle(1))?;
    let publisher = group.handle(ConfigCellId(101)).expect("publisher handle");
    let plan = PlanRevision(98);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let builder = BootstrapPublicationBuilder::new(plan.0, 28)
        .route(
            "gateway.test",
            "/config-snapshot",
            1,
            plain_target(address, 35),
        )?
        .config_cells(Arc::new(group.handles()));
    let publications = install_publication(
        builder
            .attempt_config_cells(1, Arc::from([ConfigCellId(101), ConfigCellId(102)]))?
            .accepted_config_cells(Arc::from([ConfigCellId(101), ConfigCellId(102)])),
    )?;
    let provider = PassthroughProvider {
        config_publish_between_reads: Some((publisher, bundle(2))),
        ..PassthroughProvider::default()
    };
    let observed = Arc::clone(&provider.observed_pinned_config_generations);
    let accepted_observed = Arc::clone(&provider.observed_accepted_config_generations);
    let sink = Arc::new(RecordingFailingSink::default());
    let telemetry = Arc::new(Telemetry::new(sink.clone()));
    let gateway = lifecycle_with_provider(
        publications,
        TestSelection::new([binding]),
        provider,
        TrackingFilters::default(),
    )?
    .with_telemetry(telemetry.clone());
    let mut session = RecordingSession::new(
        "gateway.test",
        "/config-snapshot",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(
        *observed.lock().expect("pinned config facts"),
        vec![(ConfigGeneration(1), ConfigGeneration(1))],
        "publish between value lookups cannot tear one attempt snapshot"
    );
    assert_eq!(
        *accepted_observed.lock().expect("accepted config facts"),
        vec![(ConfigGeneration(1), ConfigGeneration(1))],
        "Accepted must reuse the selected attempt snapshot after generation 2 publishes"
    );
    telemetry.flush(Duration::from_secs(1))?;
    {
        let events = sink.events.lock().expect("config telemetry events");
        assert!(events.iter().any(|event| matches!(
            event.kind,
            LifecycleKind::ConfigLease(ConfigLeaseFact {
                cell_id: 101,
                generation: ConfigGeneration(1),
                acquire_scope: ConfigAcquireScope::Attempt,
                stage: ConfigLeaseStage::Acquired,
                ..
            })
        )));
        assert!(!events.iter().any(|event| matches!(
            event.kind,
            LifecycleKind::ConfigLease(ConfigLeaseFact {
                generation: ConfigGeneration(2),
                acquire_scope: ConfigAcquireScope::Attempt,
                stage: ConfigLeaseStage::Acquired,
                ..
            })
        )));
        assert!(
            events.iter().any(|event| matches!(
                event.kind,
                LifecycleKind::ConfigLease(ConfigLeaseFact {
                    cell_id: 101,
                    generation: ConfigGeneration(1),
                    acquire_scope: ConfigAcquireScope::Attempt,
                    stage: ConfigLeaseStage::Released,
                    release_latency_micros,
                }) if release_latency_micros > 0
            )),
            "attempt snapshot Drop must emit its measured, non-zero hold latency"
        );
    }
    assert_eq!(
        group
            .acquire_attempt_snapshot()?
            .value(ConfigCellId(101))
            .expect("fresh config")
            .generation,
        ConfigGeneration(2)
    );
    assert!(!server.await??);
    Ok(())
}

#[tokio::test]
async fn native_filter_configs_pin_request_and_refresh_phase_event_before_await()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"filter-config", false).await
    });
    let make_bundle = |id: ConfigCellId, generation: u64| {
        Arc::new(ConfigBundle::new(
            AtomicityGroupId(id.0),
            HashMap::from([(
                id,
                ImmutableConfig {
                    generation: ConfigGeneration(generation),
                    compatibility_hash: [id.0 as u8; 32],
                    bytes: Arc::from([id.0 as u8, generation as u8]),
                },
            )]),
        ))
    };
    let make_group = |id: ConfigCellId, policy: ConfigBindingPolicy, initial| {
        ConfigCellGroup::new(
            [ConfigCellDescriptor {
                id,
                compatibility_hash: [id.0 as u8; 32],
                atomicity_group: AtomicityGroupId(id.0),
                binding_policy: policy,
            }],
            initial,
        )
    };
    let request_initial = make_bundle(ConfigCellId(201), 1);
    let phase_initial = make_bundle(ConfigCellId(202), 1);
    let event_initial = make_bundle(ConfigCellId(203), 1);
    let phase_retired = Arc::downgrade(&phase_initial);
    let event_retired = Arc::downgrade(&event_initial);
    let request_group = make_group(
        ConfigCellId(201),
        ConfigBindingPolicy::RequestPinned,
        request_initial,
    )?;
    let phase_group = make_group(
        ConfigCellId(202),
        ConfigBindingPolicy::PhasePinned,
        phase_initial,
    )?;
    let event_group = make_group(
        ConfigCellId(203),
        ConfigBindingPolicy::EventLive,
        event_initial,
    )?;
    let updates = vec![
        (
            request_group
                .handle(ConfigCellId(201))
                .expect("request handle"),
            make_bundle(ConfigCellId(201), 2),
        ),
        (
            phase_group.handle(ConfigCellId(202)).expect("phase handle"),
            make_bundle(ConfigCellId(202), 2),
        ),
        (
            event_group.handle(ConfigCellId(203)).expect("event handle"),
            make_bundle(ConfigCellId(203), 2),
        ),
    ];
    let mut handles = request_group.handles();
    handles.extend(phase_group.handles());
    handles.extend(event_group.handles());
    let plan = PlanRevision(99);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 29)
            .route(
                "gateway.test",
                "/filter-config",
                1,
                plain_target(address, 36),
            )?
            .config_cells(Arc::new(handles))
            .attempt_config_cells(
                1,
                Arc::from([ConfigCellId(201), ConfigCellId(202), ConfigCellId(203)]),
            )?
            .logical_config_cells(
                1,
                Arc::from([ConfigCellId(201), ConfigCellId(202), ConfigCellId(203)]),
            )?
            .logical_filters(1, Arc::from([compiled_filter("logical-config")]))?,
    )?;
    let facts = Arc::new(NativeFilterFacts::default());
    *facts.config_updates.lock().expect("config updates") = updates;
    *facts
        .retired_config_bundles
        .lock()
        .expect("retired config bundles") = vec![phase_retired, event_retired];
    let gateway = GatewayCoreLifecycle::new(
        publications,
        TestSelection::new([binding]),
        PassthroughProvider::default(),
        native_filter_manager(Arc::clone(&facts), &["logical-config"])?,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/filter-config",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(
        *facts
            .config_observations
            .lock()
            .expect("config observations"),
        vec![
            (
                'H',
                ConfigGeneration(1),
                ConfigGeneration(1),
                ConfigGeneration(1),
            ),
            (
                'D',
                ConfigGeneration(1),
                ConfigGeneration(2),
                ConfigGeneration(2),
            ),
        ]
    );
    assert_eq!(
        facts
            .retired_configs_released_before_body
            .load(Ordering::Relaxed),
        2,
        "PhasePinned/EventLive bundle guards must be gone before the body-read await"
    );
    server.await??;
    Ok(())
}
