use crate::fixture::*;

#[test]
fn prepare_validates_route_and_config_reference_closure_before_last_good_swap() {
    let installer = PublicationInstaller::new();
    install(&installer, envelope(1, 1));

    let mut cross_revision = envelope(2, 2);
    let ingress = Arc::make_mut(&mut cross_revision.ingress_plan_handle);
    let mut routes = ingress.routes.to_vec();
    routes[0].binding = ResolvedTargetBindingId::new(PlanRevision(99), 7);
    ingress.routes = routes.into();
    assert_eq!(
        prepare_error(&installer, cross_revision),
        InstallError::MixedPlanRevision
    );

    let mut unknown_binding = envelope(2, 2);
    let ingress = Arc::make_mut(&mut unknown_binding.ingress_plan_handle);
    let mut routes = ingress.routes.to_vec();
    let unknown = ResolvedTargetBindingId::new(PlanRevision(2), 999);
    routes[0].binding = unknown;
    Arc::make_mut(&mut routes[0].request_plan).candidate_bindings = Arc::from([unknown]);
    ingress.routes = routes.into();
    assert_eq!(
        prepare_error(&installer, unknown_binding),
        InstallError::InvalidCompiledPlan(PlanError::UnknownBinding(unknown))
    );

    let missing_config = BootstrapPublicationBuilder::new(2, 2)
        .route("example.test", "/", 1, plain_target(address(8082), 2))
        .unwrap()
        .attempt_config_cells(1, Arc::from([ConfigCellId(404)]))
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        prepare_error(&installer, missing_config),
        InstallError::MissingReferencedConfig(ConfigCellId(404))
    );

    let mut invalid_route = envelope(2, 2);
    let ingress = Arc::make_mut(&mut invalid_route.ingress_plan_handle);
    let mut routes = ingress.routes.to_vec();
    routes[0].path_prefix = Arc::from("missing-leading-slash");
    ingress.routes = routes.into();
    assert_eq!(
        prepare_error(&installer, invalid_route),
        InstallError::InvalidRouteMetadata
    );

    assert_eq!(installer.active().unwrap().plan_revision, PlanRevision(1));
}

#[test]
fn prepare_validates_catalog_filter_digest_and_transport_epoch_metadata() {
    let installer = PublicationInstaller::new();
    install(&installer, envelope(1, 1));

    let descriptor = ConfigCellDescriptor {
        id: ConfigCellId(1),
        compatibility_hash: [1; 32],
        atomicity_group: AtomicityGroupId(8),
        binding_policy: ConfigBindingPolicy::RequestPinned,
    };
    let handle = ConfigCellHandle::new(
        descriptor,
        Arc::new(ConfigBundle::new(
            AtomicityGroupId(8),
            std::collections::HashMap::from([(
                ConfigCellId(1),
                ImmutableConfig {
                    generation: ConfigGeneration(1),
                    compatibility_hash: [1; 32],
                    bytes: Arc::from([1]),
                },
            )]),
        )),
    )
    .unwrap();
    let key_mismatch = BootstrapPublicationBuilder::new(2, 2)
        .route("example.test", "/", 1, plain_target(address(8082), 2))
        .unwrap()
        .config_cells(Arc::new(std::collections::HashMap::from([(
            ConfigCellId(2),
            handle,
        )])))
        .build()
        .unwrap();
    assert_eq!(
        prepare_error(&installer, key_mismatch),
        InstallError::ConfigCatalogKeyMismatch {
            key: ConfigCellId(2),
            descriptor: ConfigCellId(1),
        }
    );

    let mut invalid_filter = envelope(2, 2);
    let duplicate =
        hiroute_gateway_core::core::filter::CompiledFilterDescriptor::new("response-filter", 4)
            .unwrap();
    let ingress = Arc::make_mut(&mut invalid_filter.ingress_plan_handle);
    let mut routes = ingress.routes.to_vec();
    Arc::make_mut(&mut Arc::make_mut(&mut routes[0].request_plan).accepted_response).filters =
        Arc::from([duplicate.clone(), duplicate]);
    ingress.routes = routes.into();
    assert_eq!(
        prepare_error(&installer, invalid_filter),
        InstallError::InvalidAcceptedFilterMetadata
    );

    let mut invalid_digest = envelope(2, 2);
    invalid_digest.payload_digest = [0; 32];
    assert_eq!(
        prepare_error(&installer, invalid_digest),
        InstallError::InvalidPayloadDigest
    );

    let mut invalid_fingerprint = envelope(2, 2);
    Arc::make_mut(&mut invalid_fingerprint.connection_epoch_fingerprints)[0].digest = [0; 32];
    assert_eq!(
        prepare_error(&installer, invalid_fingerprint),
        InstallError::InvalidConnectionFingerprint
    );

    let mut mismatched_fingerprint = envelope(2, 2);
    Arc::make_mut(&mut mismatched_fingerprint.connection_epoch_fingerprints)[0].digest[0] ^= 1;
    assert_eq!(
        prepare_error(&installer, mismatched_fingerprint),
        InstallError::ConnectionFingerprintMismatch(TransportReuseClassId(11))
    );

    let inconsistent_fingerprint = BootstrapPublicationBuilder::new(2, 2)
        .route("example.test", "/one", 1, plain_target(address(8082), 9))
        .unwrap()
        .route("example.test", "/two", 2, plain_target(address(8083), 9))
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        prepare_error(&installer, inconsistent_fingerprint),
        InstallError::InconsistentConnectionFingerprint(TransportReuseClassId(9))
    );

    let mut uncovered = BootstrapPublicationBuilder::new(2, 2)
        .route("example.test", "/one", 1, plain_target(address(8082), 2))
        .unwrap()
        .route("example.test", "/two", 2, plain_target(address(8083), 3))
        .unwrap()
        .build()
        .unwrap();
    uncovered.connection_epoch_fingerprints =
        Arc::from([uncovered.connection_epoch_fingerprints[0]]);
    assert_eq!(
        prepare_error(&installer, uncovered),
        InstallError::ConnectionFingerprintCoverage {
            expected: 2,
            actual: 1,
        }
    );

    assert_eq!(installer.active().unwrap().plan_revision, PlanRevision(1));
}

#[test]
fn prepare_rejects_body_capabilities_that_the_directional_plan_cannot_hold() {
    let installer = PublicationInstaller::new();
    let plans = BootstrapBodyPlans {
        logical_request: BodyPlan::PassThrough {
            max_chunk_bytes: 4096,
        },
        attempt_request: BodyPlan::StreamingReplay {
            max_chunk_bytes: 4096,
            max_replay_bytes: 16 * 1024,
        },
        attempt_response_precommit: BodyPlan::PassThrough {
            max_chunk_bytes: 4096,
        },
        accepted_response: BodyPlan::PassThrough {
            max_chunk_bytes: 4096,
        },
    };
    let mutating = CompiledFilterDescriptor::new("mutating", 4)
        .unwrap()
        .with_capabilities(FilterCapabilities::observe_only().with_body_mutation());
    let invalid = BootstrapPublicationBuilder::new(1, 1)
        .route("example.com", "/", 1, plain_target(address(8081), 11))
        .unwrap()
        .body_plans(1, plans)
        .unwrap()
        .logical_filters(1, Arc::from([mutating]))
        .unwrap()
        .build()
        .unwrap();

    assert_eq!(
        prepare_error(&installer, invalid),
        InstallError::IncompatibleFilterBodyPlan
    );
}

#[test]
fn prepare_accepts_declared_buffered_transform_and_requires_sse_provenance() {
    let installer = PublicationInstaller::new();
    let mut plans = BootstrapBodyPlans {
        logical_request: BodyPlan::BufferedTransform {
            max_body_bytes: 16 * 1024,
        },
        attempt_request: BodyPlan::StreamingReplay {
            max_chunk_bytes: 4096,
            max_replay_bytes: 16 * 1024,
        },
        attempt_response_precommit: BodyPlan::SseFramedStreaming {
            max_event_bytes: 4096,
            max_pending_bytes: 4096,
            max_output_event_bytes: 4096,
            expansion_ratio_numerator: 2,
            expansion_ratio_denominator: 1,
            expansion_slack_bytes: 128,
        },
        accepted_response: BodyPlan::PassThrough {
            max_chunk_bytes: 4096,
        },
    };
    let buffered = CompiledFilterDescriptor::new("buffered", 4)
        .unwrap()
        .with_capabilities(
            FilterCapabilities::observe_only()
                .with_body_mutation()
                .with_header_mutation_during_body(),
        );
    let valid = BootstrapPublicationBuilder::new(1, 1)
        .route("example.com", "/", 1, plain_target(address(8081), 11))
        .unwrap()
        .body_plans(1, plans.clone())
        .unwrap()
        .logical_filters(1, Arc::from([buffered]))
        .unwrap()
        .build()
        .unwrap();
    assert!(matches!(
        installer
            .prepare(
                valid,
                &CancellationToken::new(),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap(),
        PrepareOutcome::Prepared(_)
    ));

    let missing_provenance = CompiledFilterDescriptor::new("sse-mutating", 4)
        .unwrap()
        .with_capabilities(FilterCapabilities::observe_only().with_body_mutation());
    let invalid = BootstrapPublicationBuilder::new(2, 2)
        .route("example.com", "/", 1, plain_target(address(8081), 11))
        .unwrap()
        .body_plans(1, plans.clone())
        .unwrap()
        .attempt_response_filters(1, Arc::from([missing_provenance]))
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        prepare_error(&PublicationInstaller::new(), invalid),
        InstallError::IncompatibleFilterBodyPlan
    );

    plans.accepted_response = BodyPlan::SseFramedStreaming {
        max_event_bytes: 4096,
        max_pending_bytes: 4096,
        max_output_event_bytes: 4096,
        expansion_ratio_numerator: 2,
        expansion_ratio_denominator: 1,
        expansion_slack_bytes: 128,
    };
    let provenance_aware = CompiledFilterDescriptor::new("sse-mutating", 4)
        .unwrap()
        .with_capabilities(
            FilterCapabilities::observe_only()
                .with_body_mutation()
                .with_semantic_provenance(),
        );
    let valid_sse = BootstrapPublicationBuilder::new(3, 3)
        .route("example.com", "/", 1, plain_target(address(8081), 11))
        .unwrap()
        .body_plans(1, plans)
        .unwrap()
        .attempt_response_filters(1, Arc::from([provenance_aware]))
        .unwrap()
        .build()
        .unwrap();
    assert!(matches!(
        PublicationInstaller::new()
            .prepare(
                valid_sse,
                &CancellationToken::new(),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap(),
        PrepareOutcome::Prepared(_)
    ));
}

#[test]
fn attempt_request_filter_capabilities_are_validated_against_request_body_plan() {
    let local_reply = CompiledFilterDescriptor::new("attempt-request-local", 4)
        .unwrap()
        .with_capabilities(FilterCapabilities::observe_only().with_local_reply());
    let invalid = BootstrapPublicationBuilder::new(31, 31)
        .route("example.com", "/", 1, plain_target(address(8081), 11))
        .unwrap()
        .attempt_request_filters(1, Arc::from([local_reply.clone()]))
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        prepare_error(&PublicationInstaller::new(), invalid),
        InstallError::IncompatibleFilterBodyPlan,
        "StreamingReplay cannot hold attempt-request headers for a body-stage local reply"
    );

    let plans = BootstrapBodyPlans {
        logical_request: BodyPlan::BufferedTransform {
            max_body_bytes: 16 * 1024,
        },
        attempt_request: BodyPlan::BufferedTransform {
            max_body_bytes: 16 * 1024,
        },
        attempt_response_precommit: BodyPlan::PassThrough {
            max_chunk_bytes: 4096,
        },
        accepted_response: BodyPlan::PassThrough {
            max_chunk_bytes: 4096,
        },
    };
    let valid = BootstrapPublicationBuilder::new(32, 32)
        .route("example.com", "/", 1, plain_target(address(8081), 11))
        .unwrap()
        .body_plans(1, plans)
        .unwrap()
        .attempt_request_filters(1, Arc::from([local_reply]))
        .unwrap()
        .build()
        .unwrap();
    assert!(matches!(
        PublicationInstaller::new()
            .prepare(
                valid,
                &CancellationToken::new(),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap(),
        PrepareOutcome::Prepared(_)
    ));
}

#[test]
fn prepare_rejects_filter_config_dependency_policy_or_reachability_mismatch() {
    let id = ConfigCellId(700);
    let initial = Arc::new(ConfigBundle::new(
        AtomicityGroupId(700),
        std::collections::HashMap::from([(
            id,
            ImmutableConfig {
                generation: ConfigGeneration(1),
                compatibility_hash: [7; 32],
                bytes: Arc::from([7]),
            },
        )]),
    ));
    let group = ConfigCellGroup::new(
        [ConfigCellDescriptor {
            id,
            compatibility_hash: [7; 32],
            atomicity_group: AtomicityGroupId(700),
            binding_policy: ConfigBindingPolicy::RequestPinned,
        }],
        initial,
    )
    .unwrap();
    let filter = CompiledFilterDescriptor::new("wrong-policy", 4)
        .unwrap()
        .with_config_dependencies(Arc::from([FilterConfigDependency {
            id,
            policy: ConfigBindingPolicy::PhasePinned,
        }]))
        .unwrap();
    let invalid = BootstrapPublicationBuilder::new(1, 1)
        .route("example.com", "/", 1, plain_target(address(8081), 11))
        .unwrap()
        .config_cells(Arc::new(group.handles()))
        .attempt_config_cells(1, Arc::from([id]))
        .unwrap()
        .logical_filters(1, Arc::from([filter]))
        .unwrap()
        .build()
        .unwrap();

    assert_eq!(
        prepare_error(&PublicationInstaller::new(), invalid),
        InstallError::InvalidFilterConfigDependency(id)
    );
}
