use crate::fixture::*;

#[derive(Clone, Debug, Default)]
struct ConfirmPrecommitFailureProvider(PassthroughProvider);

#[async_trait]
impl ProviderRuntimePort for ConfirmPrecommitFailureProvider {
    type LogicalRequest = PassthroughLogicalRequest;
    type RouteRequestContext = PassthroughRouteContext;
    type AttemptState = PassthroughAttemptState;
    type Readiness = PassthroughReadiness;
    type DecodedSseEvent = PassthroughDecodedSse;

    async fn begin_request(
        &self,
        head: GatewayRequestHead,
        context: LogicalRequestContext<'_>,
    ) -> Result<Self::LogicalRequest, Arc<str>> {
        self.0.begin_request(head, context).await
    }

    async fn consume_request_body(
        &self,
        logical: &mut Self::LogicalRequest,
        frame: LogicalRequestBodyFrame,
    ) -> Result<(), Arc<str>> {
        self.0.consume_request_body(logical, frame).await
    }

    fn finalize_route_request_context(
        &self,
        logical: &mut Self::LogicalRequest,
    ) -> Result<Self::RouteRequestContext, Arc<str>> {
        self.0.finalize_route_request_context(logical)
    }

    async fn materialize_attempt(
        &self,
        logical: &mut Self::LogicalRequest,
        context: AttemptMaterializationContext<'_>,
    ) -> Result<(PreparedAttemptHttpRequest, Self::AttemptState), Arc<str>> {
        self.0.materialize_attempt(logical, context).await
    }

    fn classify_materialization_failure(&self, error: &Arc<str>) -> AttemptMaterializationFailure {
        self.0.classify_materialization_failure(error)
    }

    fn classify_precommit(
        &self,
        state: &mut Self::AttemptState,
        event: PrecommitEvent,
        pinned_configs: &hiroute_gateway_core::runtime::driver::PinnedConfigContext<'_>,
        event_configs: &hiroute_gateway_core::core::execution_plan::ConfigEventSnapshot,
    ) -> Result<PrecommitClassification<Self::Readiness, Self::DecodedSseEvent>, Arc<str>> {
        self.0
            .classify_precommit(state, event, pinned_configs, event_configs)
    }

    async fn confirm_precommit(
        &self,
        _state: &mut Self::AttemptState,
        _facts: &ProviderClassificationFacts,
        _deadline: Instant,
        _cancellation: &CancellationToken,
    ) -> Result<(), Arc<str>> {
        Err(Arc::from("synthetic post-exchange authority failure"))
    }

    fn normalize_attempt_local_reply(
        &self,
        state: &mut Self::AttemptState,
        reply: LocalReply,
        upstream_side_effects: UpstreamSideEffectSnapshot,
    ) -> Result<NormalizedAttemptLocalReply<Self::Readiness>, Arc<str>> {
        self.0
            .normalize_attempt_local_reply(state, reply, upstream_side_effects)
    }

    fn finalize_attempt_facts(
        &self,
        state: &mut Self::AttemptState,
        readiness: Option<&mut Self::Readiness>,
        published_facts: Option<&ProviderClassificationFacts>,
        completion: &hiroute_gateway_core::runtime::driver::ProviderAttemptCompletion,
    ) -> Result<ProviderClassificationFacts, Arc<str>> {
        self.0
            .finalize_attempt_facts(state, readiness, published_facts, completion)
    }

    fn accepted_response_head(
        &self,
        readiness: &mut Self::Readiness,
        published: &PublishedDisposition,
        accepted: &hiroute_gateway_core::core::execution_plan::AcceptedResponseExecutionBinding,
        configs: &hiroute_gateway_core::runtime::driver::PinnedConfigContext<'_>,
    ) -> Result<GatewayResponseHead, Arc<str>> {
        self.0
            .accepted_response_head(readiness, published, accepted, configs)
    }

    fn encode_accepted_event(
        &self,
        readiness: &mut Self::Readiness,
        event: ProviderAcceptedEvent<Self::DecodedSseEvent>,
        published: &PublishedDisposition,
        accepted: &hiroute_gateway_core::core::execution_plan::AcceptedResponseExecutionBinding,
        pinned_configs: &hiroute_gateway_core::runtime::driver::PinnedConfigContext<'_>,
        event_configs: &hiroute_gateway_core::core::execution_plan::ConfigEventSnapshot,
    ) -> Result<Option<AcceptedBodyFrame>, Arc<str>> {
        self.0.encode_accepted_event(
            readiness,
            event,
            published,
            accepted,
            pinned_configs,
            event_configs,
        )
    }

    fn release_terminal_request(
        &self,
        logical: Self::LogicalRequest,
        published: &PublishedDisposition,
    ) -> Result<(), Arc<str>> {
        self.0.release_terminal_request(logical, published)
    }
}

#[tokio::test]
async fn postexchange_precommit_authority_failure_records_one_nonzero_completion()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"classified", false).await
    });
    let plan = PlanRevision(7109);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(BootstrapPublicationBuilder::new(plan.0, 109).route(
        "gateway.test",
        "/postexchange-authority-failure",
        1,
        plain_target(address, 110),
    )?)?;
    let selection = TestSelection::new([binding]);
    let provider = ConfirmPrecommitFailureProvider::default();
    let finalized_attempts = Arc::clone(&provider.0.finalized_attempts);
    let gateway = GatewayCoreLifecycle::new(
        publications,
        selection.clone(),
        provider,
        TrackingFilters::default(),
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/postexchange-authority-failure",
        Bytes::new(),
        HttpProtocol::Http1,
    );

    let error = gateway.process(&mut session).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("synthetic post-exchange authority failure")
    );
    assert!(selection.published().is_empty());
    let completed = selection.completed();
    assert_eq!(completed.len(), 1);
    assert_ne!(completed[0].attempt_id.0, 0);
    assert_eq!(completed[0].binding, binding);
    assert_eq!(completed[0].disposition, Disposition::Terminate);
    assert_eq!(completed[0].cleanup, AttemptCleanupOutcome::Completed);
    assert_eq!(
        completed[0].termination_reason,
        AttemptTerminationReason::AttemptFailure
    );
    assert_eq!(
        completed[0].provider.as_ref().unwrap().http_status,
        Some(StatusCode::OK)
    );
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 1);
    assert!(!server.await??);
    Ok(())
}

#[tokio::test]
async fn local_reply_precommit_authority_failure_without_semantic_exchange_records_no_completion()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let plan = PlanRevision(7110);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 110)
            .route(
                "gateway.test",
                "/local-reply-authority-failure",
                1,
                plain_target(address, 111),
            )?
            .body_plans(1, attempt_request_local_reply_body_plans())?
            .attempt_request_filters(1, Arc::from([compiled_filter("attempt-request-reply")]))?,
    )?;
    let selection = TestSelection::new([binding]);
    let filters = TrackingFilters::default();
    *filters
        .attempt_request_reply
        .lock()
        .expect("attempt request reply") = Some(LocalReply {
        status: StatusCode::SERVICE_UNAVAILABLE,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"local authority failure"),
        provenance: SemanticProvenance::NonSemantic,
    });
    let provider = ConfirmPrecommitFailureProvider::default();
    let materialized_attempts = Arc::clone(&provider.0.materialized_attempts);
    let finalized_attempts = Arc::clone(&provider.0.finalized_attempts);
    let normalized_effects = Arc::clone(&provider.0.normalized_upstream_effects);
    let gateway = GatewayCoreLifecycle::new(
        publications,
        selection.clone(),
        provider,
        filters,
        PingoraConnectorAdapter::new(),
        bounded_test_limits(Duration::from_secs(5)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/local-reply-authority-failure",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    let error = gateway.process(&mut session).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("synthetic post-exchange authority failure")
    );
    assert_eq!(materialized_attempts.load(Ordering::Relaxed), 1);
    {
        let effects = normalized_effects.lock().expect("normalized effects");
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].semantic_upstream_calls, 0);
        assert_eq!(effects[0].connection_sub_attempts, 0);
        assert_eq!(effects[0].request_fence, CommitFence::Clear);
    }
    assert!(selection.published().is_empty());
    assert!(selection.completed().is_empty());
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "attempt-request local reply must not connect upstream"
    );
    Ok(())
}

#[tokio::test]
async fn concrete_lifecycle_uses_pingora_h1_and_local_404_never_selects() -> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"h1-ok", false).await
    });

    let plan = PlanRevision(71);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, ConfigRevision(1).0)
            .route("gateway.test", "/api", 1, plain_target(address, 1))?
            .attempt_response_filters(1, Arc::from([compiled_filter("attempt-track")]))?
            .route_accepted_filters(1, Arc::from([compiled_filter("accepted-track")]))?,
    )?;
    let selection = TestSelection::new([binding]);
    let filters = TrackingFilters {
        rewrite_content_length: true,
        ..TrackingFilters::default()
    };
    let gateway = lifecycle(
        Arc::clone(&publications),
        selection.clone(),
        filters.clone(),
    )?;
    let _pingora_app = GatewayHttpApp::new(gateway);

    let gateway = lifecycle(publications, selection.clone(), filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/api/chat?stream=true",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );
    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(
        session.response_head.as_ref().unwrap().status,
        StatusCode::OK
    );
    assert!(
        !session
            .response_head
            .as_ref()
            .unwrap()
            .headers
            .contains_key(CONTENT_LENGTH)
    );
    assert_eq!(
        session.response_head.as_ref().unwrap().headers[http::header::TRANSFER_ENCODING],
        "chunked"
    );
    assert_eq!(session.response_body, b"h1-ok");
    assert!(session.response_eos);
    assert_eq!(selection.published(), [Disposition::Accept]);
    assert_eq!(filters.logical.load(Ordering::Relaxed), 0);
    assert!(filters.attempt.load(Ordering::Relaxed) >= 1);
    assert_eq!(filters.accepted_head.load(Ordering::Relaxed), 1);
    assert!(!server.await??);

    let no_route_selection = TestSelection::new([binding]);
    let publications = install_publication(BootstrapPublicationBuilder::new(72, 2).route(
        "other.test",
        "/",
        1,
        plain_target("127.0.0.1:9".parse()?, 2),
    )?)?;
    let gateway = lifecycle(
        publications,
        no_route_selection.clone(),
        TrackingFilters::default(),
    )?;
    let mut no_route = RecordingSession::new(
        "gateway.test",
        "/missing",
        Bytes::new(),
        HttpProtocol::Http1,
    );
    assert_eq!(gateway.process(&mut no_route).await?, SessionReuse::Close);
    assert_eq!(
        no_route.response_head.as_ref().unwrap().status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(no_route_selection.selected(), 0);
    Ok(())
}

#[tokio::test]
async fn selection_snapshot_keeps_candidate_attribution_and_identity_through_completion()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"attributed", false).await
    });
    let plan = PlanRevision(7101);
    let first = ResolvedTargetBindingId::new(plan, 1);
    let second = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 101)
            .route("gateway.test", "/facts", 1, plain_target(address, 101))?
            .route(
                "gateway.test",
                "/facts-unused",
                2,
                plain_target("127.0.0.1:9".parse()?, 102),
            )?
            .route_candidates(1, [1, 2])?,
    )?;
    let now = Instant::now();
    let source = ObservationLabel::new("health-snapshot").unwrap();
    let snapshot = RealtimeRoutingFacts {
        snapshot_id: Some(RoutingFactsSnapshotId(7001)),
        facts: Arc::from([
            FreshRoutingFact {
                source: source.clone(),
                subject: FactSubject {
                    plan_revision: plan,
                    binding: first,
                    stable_target: ObservationLabel::new("bootstrap-1").unwrap(),
                    provider: None,
                    model: None,
                    entitlement: None,
                    credential: None,
                },
                scope: FactScope::Candidate,
                confidence: FactConfidence::Measured,
                observed_at: now,
                valid_until: now + Duration::from_secs(30),
                state: RoutingFactState::Known(RealtimeRoutingFact::HealthScore {
                    basis_points: 9_900,
                }),
            },
            FreshRoutingFact {
                source,
                subject: FactSubject {
                    plan_revision: plan,
                    binding: second,
                    stable_target: ObservationLabel::new("bootstrap-2").unwrap(),
                    provider: None,
                    model: None,
                    entitlement: None,
                    credential: None,
                },
                scope: FactScope::Candidate,
                confidence: FactConfidence::Measured,
                observed_at: now,
                valid_until: now + Duration::from_secs(30),
                state: RoutingFactState::Known(RealtimeRoutingFact::HealthScore {
                    basis_points: 100,
                }),
            },
        ]),
    };
    let selection = TestSelection::new([first, second]).with_routing_snapshots([snapshot.clone()]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let mut session =
        RecordingSession::new("gateway.test", "/facts", Bytes::new(), HttpProtocol::Http1);

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    let completed = selection.completed();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].routing_facts.snapshot_id, snapshot.snapshot_id);
    assert!(Arc::ptr_eq(
        &completed[0].routing_facts.facts,
        &snapshot.facts
    ));
    assert_eq!(completed[0].routing_facts.facts[0].subject.binding, first);
    assert_eq!(completed[0].routing_facts.facts[1].subject.binding, second);
    assert!(!server.await??);
    Ok(())
}

#[tokio::test]
async fn unchanged_snapshot_identity_can_be_reused_but_cannot_equivocate() -> Result<(), TestError>
{
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let first_server = tokio::spawn(async move {
        let (socket, _) = first_listener.accept().await?;
        serve_h1_once(socket, StatusCode::TOO_MANY_REQUESTS, b"retry", false).await
    });
    let second_listener = TcpListener::bind("127.0.0.1:0").await?;
    let second_address = second_listener.local_addr()?;
    let second_server = tokio::spawn(async move {
        let (socket, _) = second_listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"same-snapshot-ok", false).await
    });
    let plan = PlanRevision(7103);
    let first = ResolvedTargetBindingId::new(plan, 1);
    let second = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 103)
            .route(
                "gateway.test",
                "/snapshot-reuse",
                1,
                plain_target(first_address, 104),
            )?
            .route(
                "gateway.test",
                "/snapshot-reuse-fallback",
                2,
                plain_target(second_address, 105),
            )?
            .route_candidates(1, [1, 2])?,
    )?;
    let observed_at = Instant::now();
    let snapshot = candidate_health_snapshot(8001, plan, first, "bootstrap-1", 9_000, observed_at);
    let repeated = RealtimeRoutingFacts {
        snapshot_id: snapshot.snapshot_id,
        facts: Arc::from(snapshot.facts.iter().cloned().collect::<Vec<_>>()),
    };
    assert!(!Arc::ptr_eq(&snapshot.facts, &repeated.facts));
    let selection = TestSelection::new([first, second])
        .with_routing_snapshots([snapshot.clone(), repeated.clone()]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/snapshot-reuse",
        Bytes::new(),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"same-snapshot-ok");
    assert_eq!(selection.selected(), 2);
    let completed = selection.completed();
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].routing_facts, snapshot);
    assert_eq!(completed[1].routing_facts, repeated);
    assert!(!first_server.await??);
    assert!(!second_server.await??);

    let first_listener = TcpListener::bind("127.0.0.1:0").await?;
    let first_address = first_listener.local_addr()?;
    let first_server = tokio::spawn(async move {
        let (socket, _) = first_listener.accept().await?;
        serve_h1_once(socket, StatusCode::TOO_MANY_REQUESTS, b"retry", false).await
    });
    let plan = PlanRevision(7104);
    let first = ResolvedTargetBindingId::new(plan, 1);
    let second = ResolvedTargetBindingId::new(plan, 2);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 104)
            .route(
                "gateway.test",
                "/snapshot-equivocation",
                1,
                plain_target(first_address, 106),
            )?
            .route(
                "gateway.test",
                "/snapshot-equivocation-fallback",
                2,
                plain_target("127.0.0.1:9".parse()?, 107),
            )?
            .route_candidates(1, [1, 2])?,
    )?;
    let observed_at = Instant::now();
    let original = candidate_health_snapshot(8002, plan, first, "bootstrap-1", 9_000, observed_at);
    let changed = candidate_health_snapshot(8002, plan, first, "bootstrap-1", 1_000, observed_at);
    let selection = TestSelection::new([first, second]).with_routing_snapshots([original, changed]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/snapshot-equivocation",
        Bytes::new(),
        HttpProtocol::Http1,
    );
    let error = gateway.process(&mut session).await.unwrap_err();
    assert!(error.to_string().contains("realtime routing facts"));
    assert_eq!(selection.selected(), 1);
    assert_eq!(selection.completed().len(), 1);
    assert!(!first_server.await??);
    Ok(())
}

#[tokio::test]
async fn credential_context_is_binding_local_across_scopes_and_credential_scope_requires_it()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"credential-facts-ok", false).await
    });
    let plan = PlanRevision(7105);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let default_credential = CredentialRef::new("bootstrap-credential-1")?;
    let alternate_credential = CredentialRef::new("credential-alternate")?;
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 105)
            .route(
                "gateway.test",
                "/credential-facts",
                1,
                plain_target(address, 108),
            )?
            .credential_refs(
                1,
                [default_credential.clone(), alternate_credential.clone()],
            )?,
    )?;
    let observed_at = Instant::now();
    let credential_fact = |credential: CredentialRef, seconds| FreshRoutingFact {
        source: ObservationLabel::new("credential-cooldown").unwrap(),
        subject: FactSubject {
            plan_revision: plan,
            binding,
            stable_target: ObservationLabel::new("bootstrap-1").unwrap(),
            provider: None,
            model: None,
            entitlement: None,
            credential: Some(credential),
        },
        scope: FactScope::Credential,
        confidence: FactConfidence::Reported,
        observed_at,
        valid_until: observed_at + Duration::from_secs(30),
        state: RoutingFactState::Known(RealtimeRoutingFact::CooldownUntil {
            until: observed_at + Duration::from_secs(seconds),
        }),
    };
    let mut candidate_credential = credential_fact(alternate_credential.clone(), 2);
    candidate_credential.scope = FactScope::Candidate;
    let mut provider_credential = credential_fact(default_credential.clone(), 3);
    provider_credential.scope = FactScope::Provider;
    provider_credential.subject.provider = Some(ObservationLabel::new("provider-a").unwrap());
    let snapshot = RealtimeRoutingFacts {
        snapshot_id: Some(RoutingFactsSnapshotId(8003)),
        facts: Arc::from([
            credential_fact(default_credential.clone(), 1),
            candidate_credential,
            provider_credential,
        ]),
    };
    let selection = TestSelection::new([binding]).with_routing_snapshots([snapshot]);
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/credential-facts",
        Bytes::new(),
        HttpProtocol::Http1,
    );
    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    let completed = selection.completed();
    assert_eq!(completed.len(), 1);
    assert_eq!(
        completed[0].routing_facts.facts[0]
            .subject
            .credential
            .as_ref(),
        Some(&default_credential)
    );
    assert_eq!(
        completed[0].routing_facts.facts[1]
            .subject
            .credential
            .as_ref(),
        Some(&alternate_credential)
    );
    assert_eq!(
        completed[0].routing_facts.facts[1].scope,
        FactScope::Candidate
    );
    assert_eq!(
        completed[0].routing_facts.facts[2].scope,
        FactScope::Provider
    );
    assert_eq!(
        completed[0].routing_facts.facts[2]
            .subject
            .credential
            .as_ref(),
        Some(&default_credential)
    );
    assert!(!server.await??);

    for (case, scope, include_provider, include_crossed_credential) in [
        (0_u64, FactScope::Candidate, false, true),
        (1, FactScope::Provider, true, true),
        (2, FactScope::Credential, false, false),
    ] {
        let plan = PlanRevision(7106 + case);
        let first = ResolvedTargetBindingId::new(plan, 1);
        let second = ResolvedTargetBindingId::new(plan, 2);
        let path = format!("/credential-invalid-{case}");
        let fallback_path = format!("/credential-invalid-fallback-{case}");
        let publications = install_publication(
            BootstrapPublicationBuilder::new(plan.0, 106 + case)
                .route(
                    "gateway.test",
                    &path,
                    1,
                    plain_target("127.0.0.1:9".parse()?, 109 + case * 2),
                )?
                .route(
                    "gateway.test",
                    &fallback_path,
                    2,
                    plain_target("127.0.0.1:9".parse()?, 110 + case * 2),
                )?
                .route_candidates(1, [1, 2])?,
        )?;
        let observed_at = Instant::now();
        let invalid = RealtimeRoutingFacts {
            snapshot_id: Some(RoutingFactsSnapshotId(8004 + case)),
            facts: Arc::from([FreshRoutingFact {
                source: ObservationLabel::new("credential-cooldown").unwrap(),
                subject: FactSubject {
                    plan_revision: plan,
                    binding: first,
                    stable_target: ObservationLabel::new("bootstrap-1").unwrap(),
                    provider: include_provider
                        .then(|| ObservationLabel::new("provider-a").unwrap()),
                    model: None,
                    entitlement: None,
                    credential: include_crossed_credential
                        .then(|| CredentialRef::new("bootstrap-credential-2").unwrap()),
                },
                scope,
                confidence: FactConfidence::Reported,
                observed_at,
                valid_until: observed_at + Duration::from_secs(30),
                state: RoutingFactState::Known(RealtimeRoutingFact::CooldownUntil {
                    until: observed_at + Duration::from_secs(1),
                }),
            }]),
        };
        let selection = TestSelection::new([first, second]).with_routing_snapshots([invalid]);
        let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
        let mut session =
            RecordingSession::new("gateway.test", &path, Bytes::new(), HttpProtocol::Http1);
        let error = gateway.process(&mut session).await.unwrap_err();
        assert!(error.to_string().contains("realtime routing facts"));
        assert_eq!(selection.selected(), 0, "invalid case {case}");
        assert!(selection.published().is_empty(), "invalid case {case}");
    }
    Ok(())
}

#[tokio::test]
async fn published_and_completion_ledger_failures_do_not_change_the_data_plane()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"ledger-independent", false).await
    });
    let plan = PlanRevision(7102);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(BootstrapPublicationBuilder::new(plan.0, 102).route(
        "gateway.test",
        "/ledger-fail-open",
        1,
        plain_target(address, 103),
    )?)?;
    let selection = TestSelection::new([binding]);
    selection.fail_observation_sink();
    let gateway = lifecycle(publications, selection.clone(), TrackingFilters::default())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/ledger-fail-open",
        Bytes::new(),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"ledger-independent");
    assert!(session.response_eos);
    assert_eq!(selection.published(), [Disposition::Accept]);
    assert_eq!(selection.completed().len(), 1);
    assert!(!server.await??);
    Ok(())
}

#[tokio::test]
async fn accepted_completion_waits_for_filter_cleanup_and_ledger_failure_stays_fail_open()
-> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"cleanup-before-ledger", false).await
    });
    let plan = PlanRevision(7108);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(
        BootstrapPublicationBuilder::new(plan.0, 108)
            .route(
                "gateway.test",
                "/accepted-cleanup-order",
                1,
                plain_target(address, 109),
            )?
            .route_accepted_filters(1, Arc::from([compiled_filter("accepted-cleanup")]))?,
    )?;
    let selection = TestSelection::new([binding]);
    selection.fail_observation_sink();
    let filters = TrackingFilters {
        accepted_cleanup_delay: Some(Duration::from_millis(150)),
        ..TrackingFilters::default()
    };
    let cleanup_entered = filters.accepted_cleanup_entered.notified();
    tokio::pin!(cleanup_entered);
    let provider = PassthroughProvider::default();
    let finalized_attempts = Arc::clone(&provider.finalized_attempts);
    let gateway =
        lifecycle_with_provider(publications, selection.clone(), provider, filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/accepted-cleanup-order",
        Bytes::new(),
        HttpProtocol::Http1,
    );
    let process = tokio::spawn(async move {
        let result = gateway.process(&mut session).await;
        (result, session)
    });

    tokio::time::timeout(Duration::from_secs(2), &mut cleanup_entered).await?;
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 0);
    assert!(selection.completed().is_empty());

    let (result, session) = process.await?;
    assert_eq!(result?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"cleanup-before-ledger");
    assert!(session.response_eos);
    assert_eq!(filters.attempt_cleanup.load(Ordering::Relaxed), 1);
    assert_eq!(filters.accepted_cleanup.load(Ordering::Relaxed), 1);
    assert_eq!(filters.finalized.load(Ordering::Relaxed), 1);
    assert_eq!(finalized_attempts.load(Ordering::Relaxed), 1);
    let completed = selection.completed();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].cleanup, AttemptCleanupOutcome::Completed);
    assert_eq!(
        completed[0].termination_reason,
        AttemptTerminationReason::AcceptedEos
    );
    assert!(!server.await??);
    Ok(())
}

#[tokio::test]
async fn empty_compiled_chains_bypass_a_non_noop_filter_manager() -> Result<(), TestError> {
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_once(socket, StatusCode::OK, b"empty-chain", false).await
    });
    let plan = PlanRevision(101);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let publications = install_publication(BootstrapPublicationBuilder::new(plan.0, 31).route(
        "gateway.test",
        "/empty-chain",
        1,
        plain_target(address, 41),
    )?)?;
    let filters = TrackingFilters::default();
    let gateway = lifecycle(publications, TestSelection::new([binding]), filters.clone())?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/empty-chain",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );

    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"empty-chain");
    assert_eq!(filters.logical.load(Ordering::Relaxed), 0);
    assert_eq!(filters.attempt_request_head.load(Ordering::Relaxed), 0);
    assert_eq!(filters.attempt_request_body.load(Ordering::Relaxed), 0);
    assert_eq!(filters.attempt.load(Ordering::Relaxed), 0);
    assert_eq!(filters.accepted_head.load(Ordering::Relaxed), 0);
    assert_eq!(filters.accepted_body.load(Ordering::Relaxed), 0);
    assert_eq!(filters.finalized.load(Ordering::Relaxed), 1);
    assert!(!server.await??);
    Ok(())
}

#[tokio::test]
async fn pingora_connect_timeout_does_not_bound_long_stream_response_gaps() -> Result<(), TestError>
{
    let _network_guard = NETWORK_TEST_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        serve_h1_after_response_gap(socket, Duration::from_millis(650), b"after-gap").await
    });

    let plan = PlanRevision(83);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let mut target = plain_target(address, 15);
    target.connect_timeout = Duration::from_millis(500);
    let publications = install_publication(BootstrapPublicationBuilder::new(plan.0, 13).route(
        "gateway.test",
        "/long-token-gap",
        1,
        target,
    )?)?;
    let selection = TestSelection::new([binding]);
    let gateway = lifecycle_with_provider_and_limits(
        publications,
        selection.clone(),
        PassthroughProvider::default(),
        TrackingFilters::default(),
        bounded_test_limits(Duration::from_secs(2)),
    )?;
    let mut session = RecordingSession::new(
        "gateway.test",
        "/long-token-gap",
        Bytes::from_static(b"prompt"),
        HttpProtocol::Http1,
    );
    assert_eq!(gateway.process(&mut session).await?, SessionReuse::Reusable);
    assert_eq!(session.response_body, b"after-gap");
    assert_eq!(selection.published(), [Disposition::Accept]);
    server.await??;
    Ok(())
}
