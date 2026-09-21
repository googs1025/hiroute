use super::*;
use hiroute_application::publication::admission::SharedAdmissionGate;
use hiroute_domain::WorkspaceId;

fn build_verifier(pair: &RunCredentialPair) -> Arc<RunCredentialVerifier> {
    let safety = Arc::new(
        RunSafetyProjection::new(Arc::new(SharedAdmissionGate::new()), "epoch".into()).unwrap(),
    );
    safety.finish_startup_recovery();
    Arc::new(
        RunCredentialVerifier::new(
            RunCredentialRecord {
                task_id: "task".into(),
                run_id: "run".into(),
                lease_id: "lease".into(),
                safety: RunSafetyBinding {
                    workspace: WorkspaceId::default(),
                    daemon_epoch: "epoch".into(),
                    permit_id: "permit".into(),
                    permit_generation: 1,
                    expires_at_ms: 100,
                },
                model_alias: "reviewer-strong".into(),
                protocol: AgentIngressProtocolV1::Responses,
                fingerprints: pair.fingerprints(),
            },
            safety,
        )
        .unwrap(),
    )
}

#[test]
fn model_and_query_tokens_have_disjoint_audiences_and_cannot_name_another_run() {
    let pair = RunCredentialPair::generate().unwrap();
    let verifier = build_verifier(&pair);
    assert_ne!(pair.model.expose(), pair.self_query.expose());
    assert!(
        verifier
            .authenticate(pair.self_query.expose(), RunCredentialAudience::Model, 1)
            .is_err()
    );
    assert!(
        verifier
            .authenticate(pair.model.expose(), RunCredentialAudience::SelfQuery, 1)
            .is_err()
    );
    let model = verifier
        .authenticate(pair.model.expose(), RunCredentialAudience::Model, 1)
        .unwrap();
    assert!(
        model
            .check_model("reviewer-strong", AgentIngressProtocolV1::Responses, 1)
            .is_ok()
    );
    assert!(
        model
            .check_model("other-model", AgentIngressProtocolV1::Responses, 1)
            .is_err()
    );
    assert!(
        model
            .check_model("reviewer-strong", AgentIngressProtocolV1::Messages, 1)
            .is_err()
    );
    assert!(model.check_self_query("task", "run", 1).is_err());
    let query = verifier
        .authenticate(
            pair.self_query.expose(),
            RunCredentialAudience::SelfQuery,
            1,
        )
        .unwrap();
    assert!(query.check_self_query("task", "run", 1).is_ok());
    assert!(query.check_self_query("task", "another-run", 1).is_err());
    assert!(
        query
            .check_model("reviewer-strong", AgentIngressProtocolV1::Responses, 1)
            .is_err()
    );
}

#[test]
fn completed_run_revokes_cached_credentials_and_next_run_cannot_reuse_the_old_token() {
    let old = RunCredentialPair::generate().unwrap();
    let verifier = build_verifier(&old);
    let cached = verifier
        .authenticate(old.model.expose(), RunCredentialAudience::Model, 1)
        .unwrap();
    verifier.revoke();
    assert!(
        cached
            .check_model("reviewer-strong", AgentIngressProtocolV1::Responses, 2)
            .is_err()
    );
    let next = RunCredentialPair::generate().unwrap();
    let next_verifier = build_verifier(&next);
    assert!(
        next_verifier
            .authenticate(old.model.expose(), RunCredentialAudience::Model, 2)
            .is_err()
    );
    assert!(
        next_verifier
            .authenticate(next.model.expose(), RunCredentialAudience::Model, 2)
            .is_ok()
    );
    assert!(
        next_verifier
            .authenticate(next.model.expose(), RunCredentialAudience::Model, 100)
            .is_err()
    );
}
