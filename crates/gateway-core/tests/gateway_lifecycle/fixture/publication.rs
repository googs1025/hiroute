use super::*;

pub(crate) fn compiled_filter(id: &str) -> CompiledFilterDescriptor {
    let descriptor = CompiledFilterDescriptor::new(id, 8).expect("compiled filter descriptor");
    match id {
        "logical-replace" => {
            descriptor.with_capabilities(FilterCapabilities::observe_only().with_body_expansion())
        }
        "logical-drop" => {
            descriptor.with_capabilities(FilterCapabilities::observe_only().with_body_drop())
        }
        "accepted-sse-expand" | "attempt-sse-expand" => descriptor.with_capabilities(
            FilterCapabilities::observe_only()
                .with_body_expansion()
                .with_semantic_provenance(),
        ),
        "accepted-sse-merge-two" | "accepted-sse-repeat-promoted" => descriptor.with_capabilities(
            FilterCapabilities::observe_only()
                .with_body_expansion()
                .with_body_drop()
                .with_semantic_provenance(),
        ),
        "attempt-sse-buffer-drop-first" => descriptor.with_capabilities(
            FilterCapabilities::observe_only()
                .with_body_drop()
                .with_semantic_provenance(),
        ),
        "accepted-stop" => descriptor.with_capabilities(
            FilterCapabilities::observe_only().with_header_mutation_during_body(),
        ),
        "attempt-request-reply" => {
            descriptor.with_capabilities(FilterCapabilities::observe_only().with_local_reply())
        }
        "logical-config" => descriptor
            .with_config_dependencies(Arc::from([
                FilterConfigDependency {
                    id: ConfigCellId(201),
                    policy: ConfigBindingPolicy::RequestPinned,
                },
                FilterConfigDependency {
                    id: ConfigCellId(202),
                    policy: ConfigBindingPolicy::PhasePinned,
                },
                FilterConfigDependency {
                    id: ConfigCellId(203),
                    policy: ConfigBindingPolicy::EventLive,
                },
            ]))
            .expect("compiled filter config dependencies"),
        _ => descriptor,
    }
}

pub(crate) fn lifecycle_body_plans() -> BootstrapBodyPlans {
    BootstrapBodyPlans {
        logical_request: BodyPlan::BufferedTransform {
            max_body_bytes: 1024 * 1024,
        },
        attempt_request: BodyPlan::StreamingReplay {
            max_chunk_bytes: 16 * 1024,
            max_replay_bytes: 1024 * 1024,
        },
        attempt_response_precommit: BodyPlan::PassThrough {
            max_chunk_bytes: 64 * 1024,
        },
        accepted_response: BodyPlan::PassThrough {
            max_chunk_bytes: 64 * 1024,
        },
    }
}

pub(crate) fn attempt_request_local_reply_body_plans() -> BootstrapBodyPlans {
    let mut plans = lifecycle_body_plans();
    plans.attempt_request = BodyPlan::BufferedTransform {
        max_body_bytes: 1024 * 1024,
    };
    plans
}

pub(crate) fn native_filter_manager(
    facts: Arc<NativeFilterFacts>,
    ids: &[&str],
) -> Result<NativeGatewayFilterManager, TestError> {
    let mut manager = NativeGatewayFilterManager::new(64 * 1024)?;
    let factory: Arc<dyn NativeFilterFactory> = Arc::new(LifecycleNativeFactory { facts });
    for id in ids {
        manager.register(Arc::<str>::from(*id), Arc::clone(&factory))?;
    }
    Ok(manager)
}

pub(crate) fn install_publication(
    builder: BootstrapPublicationBuilder,
) -> Result<Arc<PublicationInstaller>, TestError> {
    let installer = Arc::new(PublicationInstaller::new());
    let cancel = CancellationToken::new();
    let prepared = match installer.prepare(
        builder.build()?,
        &cancel,
        Instant::now() + Duration::from_secs(2),
    )? {
        PrepareOutcome::Prepared(prepared) => prepared,
        PrepareOutcome::Duplicate(_) => return Err("unexpected duplicate publication".into()),
    };
    installer.publish(prepared, &cancel, Instant::now() + Duration::from_secs(2))?;
    Ok(installer)
}

pub(crate) fn lifecycle(
    publications: Arc<PublicationInstaller>,
    selection: TestSelection,
    filters: TrackingFilters,
) -> Result<
    GatewayCoreLifecycle<
        TestSelection,
        PassthroughProvider,
        TrackingFilters,
        PingoraConnectorAdapter,
    >,
    TestError,
> {
    lifecycle_with_provider(
        publications,
        selection,
        PassthroughProvider::default(),
        filters,
    )
}

pub(crate) fn lifecycle_with_provider(
    publications: Arc<PublicationInstaller>,
    selection: TestSelection,
    provider: PassthroughProvider,
    filters: TrackingFilters,
) -> Result<
    GatewayCoreLifecycle<
        TestSelection,
        PassthroughProvider,
        TrackingFilters,
        PingoraConnectorAdapter,
    >,
    TestError,
> {
    lifecycle_with_provider_and_limits(
        publications,
        selection,
        provider,
        filters,
        GatewayCoreLifecycleLimits {
            max_request_body_bytes: 64 * 1024,
            write_quantum: 4,
            bootstrap_hard_cap: Some(Duration::from_secs(5)),
            ..GatewayCoreLifecycleLimits::default()
        },
    )
}

pub(crate) fn lifecycle_with_provider_and_limits(
    publications: Arc<PublicationInstaller>,
    selection: TestSelection,
    provider: PassthroughProvider,
    filters: TrackingFilters,
    limits: GatewayCoreLifecycleLimits,
) -> Result<
    GatewayCoreLifecycle<
        TestSelection,
        PassthroughProvider,
        TrackingFilters,
        PingoraConnectorAdapter,
    >,
    TestError,
> {
    Ok(GatewayCoreLifecycle::new(
        publications,
        selection,
        provider,
        filters,
        PingoraConnectorAdapter::new(),
        limits,
    )?)
}

pub(crate) fn bounded_test_limits(bootstrap_hard_cap: Duration) -> GatewayCoreLifecycleLimits {
    GatewayCoreLifecycleLimits {
        max_request_body_bytes: 64 * 1024,
        write_quantum: 4,
        bootstrap_hard_cap: Some(bootstrap_hard_cap),
        cleanup_timeout: Duration::from_millis(500),
        ..GatewayCoreLifecycleLimits::default()
    }
}
