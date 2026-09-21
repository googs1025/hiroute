use super::*;

fn classifier_secret_change() -> hiroute_domain::ChangeSpecV1 {
    hiroute_domain::ChangeSpecV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        command_id: "routing.classifier.secret.apply".into(),
        resource_id: Some("personal/default".into()),
        desired_state: json!({
            "secret_id": "classifier/main",
            "input_slot": "candidate/native/classifier-input",
            "expected_generation": 0,
        }),
    }
}

#[test]
fn classifier_header_secret_uses_the_existing_protected_transaction_path() {
    let ports = MemoryPorts::with_input(b"Bearer protected-value");
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let preview = coordinator
        .preview(
            &WorkspaceId::default(),
            PreviewRequestV1::new(classifier_secret_change()),
        )
        .unwrap();
    assert_eq!(
        preview
            .effects
            .iter()
            .map(|effect| (effect.channel.as_str(), effect.target.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("control_database", "personal/default"),
            ("secret_store", "classifier/main"),
        ]
    );
    let request = ApplyRequestV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        spec: preview.normalized_spec,
        accept_digest: preview.change_digest,
        expected_revisions: preview.expected_revisions,
        idempotency_key: "classifier-secret-create".into(),
        apply_capability: Some("classifier-secret-capability".into()),
    };
    ports.grant_for(
        "classifier-secret-capability",
        &request.accept_digest,
        &request.expected_revisions,
        "ApplyClassifierHeaderSecret",
    );
    let accepted = coordinator
        .accept(&WorkspaceId::default(), &principal(), request)
        .unwrap();
    let operation = coordinator.run_accepted(accepted).unwrap();
    assert_eq!(operation.state, OperationState::Succeeded);
    let state = ports.state.borrow();
    assert!(
        state.protected_reads >= 2,
        "preview and apply must re-read the protected slot"
    );
    assert!(
        state
            .activation_log
            .iter()
            .any(|effect| effect == "secret:classifier/main")
    );
}

#[test]
fn classifier_header_secret_rejects_effect_shaping_fields_before_protected_input() {
    let ports = MemoryPorts::with_input(b"must-not-be-read");
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let mut spec = classifier_secret_change();
    spec.desired_state["endpoint"] = json!("https://attacker.invalid");
    assert!(
        coordinator
            .preview(&WorkspaceId::default(), PreviewRequestV1::new(spec))
            .is_err()
    );
    assert_eq!(ports.state.borrow().protected_reads, 0);
}
