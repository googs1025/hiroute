use crate::fixture::*;

#[test]
fn request_binding_does_not_keep_publication_root_alive() {
    let installer = PublicationInstaller::new();
    install(&installer, envelope(1, 1));
    let old_root = installer.active().unwrap();
    let weak_root = Arc::downgrade(&old_root);
    let long_request = installer.bind_request().unwrap();
    drop(old_root);

    install(&installer, envelope(2, 2));
    assert!(weak_root.upgrade().is_none());
    assert_eq!(long_request.plan_revision(), PlanRevision(1));
    assert_eq!(
        installer.bind_request().unwrap().plan_revision(),
        PlanRevision(2)
    );
}

#[test]
fn matched_route_drops_unrelated_attempt_index_before_long_stream() {
    let old = BootstrapPublicationBuilder::new(10, 10)
        .route(
            "example.test",
            "/primary",
            1,
            plain_target("127.0.0.1:18081".parse().unwrap(), 1),
        )
        .unwrap()
        .route(
            "example.test",
            "/unused",
            2,
            plain_target("127.0.0.1:18082".parse().unwrap(), 2),
        )
        .unwrap()
        .build()
        .unwrap();
    let unused = old
        .attempt_plan_index_handle
        .resolve(ResolvedTargetBindingId::new(PlanRevision(10), 2))
        .unwrap();
    let weak_unused = Arc::downgrade(&unused);
    drop(unused);

    let installer = PublicationInstaller::new();
    install(&installer, old);
    let mut request = installer.bind_request().unwrap();
    let route = request.ingress_plan().unwrap().routes[0].clone();
    request.bind_route(&route).unwrap();
    let _request_configs = request.take_request_configs().unwrap();
    drop(request.take_logical_request().unwrap());
    let primary = request
        .resolve_attempt(ResolvedTargetBindingId::new(PlanRevision(10), 1))
        .unwrap();
    install(&installer, envelope(11, 11));
    assert!(
        weak_unused.upgrade().is_none(),
        "matched route must not retain an unrelated route attempt"
    );
    drop(primary);
    let accepted = request.take_accepted_response().unwrap();
    assert!(matches!(
        accepted.plan().body_plan,
        BodyPlan::PassThrough { .. }
    ));
    assert!(weak_unused.upgrade().is_none());
}

#[test]
fn long_sse_route_a_does_not_pin_unrelated_route_b_old_request_bundle() {
    let make_group = |id: ConfigCellId, group: AtomicityGroupId| {
        let descriptor = ConfigCellDescriptor {
            id,
            compatibility_hash: [id.0 as u8; 32],
            atomicity_group: group,
            binding_policy: ConfigBindingPolicy::RequestPinned,
        };
        let bundle = Arc::new(ConfigBundle::new(
            group,
            std::collections::HashMap::from([(
                id,
                ImmutableConfig {
                    generation: ConfigGeneration(1),
                    compatibility_hash: descriptor.compatibility_hash,
                    bytes: Arc::from([id.0 as u8]),
                },
            )]),
        ));
        let weak = Arc::downgrade(&bundle);
        let group = ConfigCellGroup::new([descriptor], bundle).unwrap();
        (group, weak)
    };
    let (group_a, weak_a) = make_group(ConfigCellId(101), AtomicityGroupId(101));
    let (group_b, weak_b) = make_group(ConfigCellId(202), AtomicityGroupId(202));
    let mut handles = group_a.handles();
    handles.extend(group_b.handles());

    let publication = BootstrapPublicationBuilder::new(30, 30)
        .route(
            "example.test",
            "/stream-a",
            1,
            plain_target(address(18091), 31),
        )
        .unwrap()
        .route(
            "example.test",
            "/unrelated-b",
            2,
            plain_target(address(18092), 32),
        )
        .unwrap()
        .config_cells(Arc::new(handles))
        .attempt_config_cells(1, Arc::from([ConfigCellId(101)]))
        .unwrap()
        .attempt_config_cells(2, Arc::from([ConfigCellId(202)]))
        .unwrap()
        .build()
        .unwrap();
    let installer = PublicationInstaller::new();
    install(&installer, publication);

    let mut request = installer.bind_request().unwrap();
    let route_a = request.ingress_plan().unwrap().routes[0].clone();
    request.bind_route(&route_a).unwrap();
    let request_configs = request.take_request_configs().unwrap();
    assert_eq!(
        request_configs.value(ConfigCellId(101)).unwrap().generation,
        ConfigGeneration(1)
    );
    assert!(request_configs.value(ConfigCellId(202)).is_none());

    let replacement = |id: ConfigCellId, group: AtomicityGroupId| {
        Arc::new(ConfigBundle::new(
            group,
            std::collections::HashMap::from([(
                id,
                ImmutableConfig {
                    generation: ConfigGeneration(2),
                    compatibility_hash: [id.0 as u8; 32],
                    bytes: Arc::from([id.0 as u8, 2]),
                },
            )]),
        ))
    };
    group_b
        .publish(replacement(ConfigCellId(202), AtomicityGroupId(202)))
        .unwrap();
    assert!(
        weak_b.upgrade().is_none(),
        "a long route-A request must not pin route B's retired bundle"
    );

    group_a
        .publish(replacement(ConfigCellId(101), AtomicityGroupId(101)))
        .unwrap();
    assert!(
        weak_a.upgrade().is_some(),
        "the matched route's RequestPinned generation must remain stable"
    );
    drop(request_configs);
    assert!(weak_a.upgrade().is_none());
}

#[test]
fn cross_revision_binding_fails_before_network_use() {
    let installer = PublicationInstaller::new();
    install(&installer, envelope(1, 1));
    let request = installer.bind_request().unwrap();
    let network = NetworkUseCounter::default();
    let injected = ResolvedTargetBindingId::new(PlanRevision(2), 7);
    assert_eq!(
        request.resolve_attempt(injected).unwrap_err(),
        PlanError::CrossRevisionBinding {
            expected: PlanRevision(1),
            actual: PlanRevision(2)
        }
    );
    assert_eq!(network.connections(), 0);
}
