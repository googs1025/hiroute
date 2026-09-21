use crate::fixture::*;

#[test]
fn publication_fences_duplicate_conflict_gap_and_last_good() {
    let installer = PublicationInstaller::new();
    install(&installer, envelope(1, 1));
    let active = installer.active().unwrap();
    assert_eq!(active.config_revision, ConfigRevision(1));

    let duplicate = installer
        .prepare(
            envelope(1, 1),
            &CancellationToken::new(),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    assert!(matches!(duplicate, PrepareOutcome::Duplicate(_)));

    let mut conflict = envelope(1, 1);
    conflict.payload_digest[0] ^= 0xff;
    assert_eq!(
        installer
            .prepare(
                conflict,
                &CancellationToken::new(),
                Instant::now() + Duration::from_secs(1)
            )
            .unwrap_err(),
        InstallError::RevisionDigestConflict
    );
    assert_eq!(
        installer.active().unwrap().config_revision,
        ConfigRevision(1)
    );

    let gap = BootstrapPublicationBuilder::new(3, 3)
        .full_snapshot(false)
        .route("example.com", "/", 1, plain_target(address(8082), 12))
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        installer
            .prepare(
                gap,
                &CancellationToken::new(),
                Instant::now() + Duration::from_secs(1)
            )
            .unwrap_err(),
        InstallError::ResyncRequired {
            active: ConfigRevision(1),
            candidate: ConfigRevision(3)
        }
    );
    assert_eq!(installer.active().unwrap().plan_revision, PlanRevision(1));
}

#[test]
fn unticketed_invalid_prepare_cannot_revoke_an_existing_prepared_candidate() {
    let installer = PublicationInstaller::new();
    let cancellation = CancellationToken::new();
    let prepared = match installer
        .prepare(
            envelope(1, 1),
            &cancellation,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap()
    {
        PrepareOutcome::Prepared(prepared) => prepared,
        PrepareOutcome::Duplicate(_) => panic!("first candidate cannot be duplicate"),
    };

    let mut invalid = envelope(2, 2);
    let ingress = Arc::make_mut(&mut invalid.ingress_plan_handle);
    let mut routes = ingress.routes.to_vec();
    routes[0].binding = ResolvedTargetBindingId::new(PlanRevision(999), 7);
    ingress.routes = routes.into();
    assert_eq!(
        prepare_error(&installer, invalid),
        InstallError::MixedPlanRevision
    );
    assert_eq!(
        installer.phase(),
        hiroute_gateway_core::core::publication::InstallerPhase::Prepared,
        "validation without an installer ticket must not rewrite another operation's phase",
    );
    assert_eq!(
        installer
            .publish(
                prepared,
                &cancellation,
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap()
            .config_revision,
        ConfigRevision(1),
    );
}

#[test]
fn cancelled_prepared_ticket_never_reaches_the_arc_swap_boundary() {
    let installer = PublicationInstaller::new();
    install(&installer, envelope(1, 1));
    let cancellation = CancellationToken::new();
    let prepared = match installer
        .prepare(
            envelope(2, 2),
            &cancellation,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap()
    {
        PrepareOutcome::Prepared(prepared) => prepared,
        PrepareOutcome::Duplicate(_) => panic!("revision two cannot be duplicate"),
    };
    cancellation.cancel();

    assert_eq!(
        installer
            .publish(
                prepared,
                &cancellation,
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_err(),
        InstallError::Cancelled,
    );
    assert_eq!(
        installer.active().unwrap().config_revision,
        ConfigRevision(1),
        "the last cancellation observation precedes the O(1) root swap",
    );
    assert_eq!(
        installer.phase(),
        hiroute_gateway_core::core::publication::InstallerPhase::Cancelled,
    );
}

#[test]
fn stale_publish_ticket_cannot_rewrite_the_newer_prepared_phase() {
    let installer = PublicationInstaller::new();
    let cancellation = CancellationToken::new();
    let first = match installer
        .prepare(
            envelope(1, 1),
            &cancellation,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap()
    {
        PrepareOutcome::Prepared(prepared) => prepared,
        PrepareOutcome::Duplicate(_) => panic!("first candidate cannot be duplicate"),
    };
    let second = match installer
        .prepare(
            envelope(2, 2),
            &cancellation,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap()
    {
        PrepareOutcome::Prepared(prepared) => prepared,
        PrepareOutcome::Duplicate(_) => panic!("second candidate cannot be duplicate"),
    };

    assert_eq!(
        installer
            .publish(
                first,
                &cancellation,
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_err(),
        InstallError::StalePreparedPublication,
    );
    assert_eq!(
        installer.phase(),
        hiroute_gateway_core::core::publication::InstallerPhase::Prepared,
    );
    assert_eq!(
        installer
            .publish(
                second,
                &cancellation,
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap()
            .config_revision,
        ConfigRevision(2),
    );
}

#[test]
fn explicit_rollback_authorization_survives_prepare_publish_boundary() {
    let installer = PublicationInstaller::new();
    install(&installer, envelope(3, 3));
    let mut rollback = envelope(2, 2);
    rollback.rollback_authorized = true;
    install(&installer, rollback);
    assert_eq!(
        installer.active().unwrap().config_revision,
        ConfigRevision(2)
    );
}

#[test]
fn large_publication_validation_is_cancel_responsive_and_does_not_hold_installer_lock() {
    let mut candidate = envelope(20, 20);
    let template = candidate.ingress_plan_handle.routes[0].clone();
    let routes = (0..200_000_u32)
        .map(|index| {
            let mut route = template.clone();
            route.path_prefix = format!("/v1/{index:06}").into();
            route
        })
        .collect::<Vec<_>>();
    candidate.ingress_plan_handle = Arc::new(CompiledIngressPlan {
        plan_revision: candidate.plan_revision,
        routes: routes.into(),
        local_response_plan: Arc::clone(&candidate.ingress_plan_handle.local_response_plan),
    });

    let installer = Arc::new(PublicationInstaller::new());
    let cancellation = CancellationToken::new();
    let worker_installer = Arc::clone(&installer);
    let worker_cancellation = cancellation.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
    let worker = std::thread::spawn(move || {
        entered_tx.send(()).unwrap();
        worker_installer.prepare(
            candidate,
            &worker_cancellation,
            Instant::now() + Duration::from_secs(10),
        )
    });

    entered_rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let phase_started = Instant::now();
    let phase = installer.phase();
    let phase_latency = phase_started.elapsed();
    cancellation.cancel();
    assert_eq!(worker.join().unwrap().unwrap_err(), InstallError::Cancelled);
    assert_eq!(
        phase,
        hiroute_gateway_core::core::publication::InstallerPhase::Idle,
        "pure shape validation must not claim or retain the installer mutex"
    );
    assert!(
        phase_latency < Duration::from_millis(50),
        "phase observation was blocked for {phase_latency:?}"
    );
}
