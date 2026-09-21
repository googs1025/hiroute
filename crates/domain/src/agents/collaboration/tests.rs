use super::*;
fn credential(seed: u8) -> AgentCollaborationCredential {
    AgentCollaborationCredential::from_csprng_entropy([seed; 32])
}
fn grant(material: &AgentCollaborationCredential) -> AgentCollaborationGrant {
    AgentCollaborationGrant::issue(
        WorkspaceId::default(),
        "agent-context/one".into(),
        "collaboration-grant/one".into(),
        1,
        BTreeSet::new(),
        material,
    )
    .unwrap()
}
#[test]
fn collaboration_bootstrap_needs_protected_material_for_exact_context_and_generation() {
    let material = credential(1);
    let grant = grant(&material);
    let principal = grant
        .verify_bootstrap("agent-context/one", 1, &material)
        .unwrap();
    assert!(principal.allowed_plan_ids().is_empty());
    assert!(
        grant
            .verify_bootstrap("agent-context/two", 1, &material)
            .is_err()
    );
    assert!(
        grant
            .verify_bootstrap("agent-context/one", 2, &material)
            .is_err()
    );
    assert!(
        grant
            .verify_bootstrap("agent-context/one", 1, &credential(2))
            .is_err()
    );
    let public = serde_json::to_string(&grant).unwrap();
    assert!(!public.contains(std::str::from_utf8(material.expose()).unwrap()));
}

#[test]
fn native_management_selection_still_requires_every_current_grant_selector() {
    let material = credential(3);
    let grant = grant(&material);
    let principal = grant
        .verify_management_selection(
            &WorkspaceId::default(),
            "agent-context/one",
            "collaboration-grant/one",
            1,
        )
        .unwrap();
    assert_eq!(principal.context_id(), "agent-context/one");
    assert!(
        grant
            .verify_management_selection(
                &WorkspaceId::parse("workspace/other").unwrap(),
                "agent-context/one",
                "collaboration-grant/one",
                1,
            )
            .is_err()
    );
    assert!(
        grant
            .verify_management_selection(
                &WorkspaceId::default(),
                "agent-context/two",
                "collaboration-grant/one",
                1,
            )
            .is_err()
    );
    assert!(
        grant
            .verify_management_selection(
                &WorkspaceId::default(),
                "agent-context/one",
                "collaboration-grant/two",
                1,
            )
            .is_err()
    );
    assert!(
        grant
            .verify_management_selection(
                &WorkspaceId::default(),
                "agent-context/one",
                "collaboration-grant/one",
                2,
            )
            .is_err()
    );
    let revoked = grant
        .plan_revocation(
            OperationId::parse("op_00112233445566778899aabbccddeeff").unwrap(),
            1,
        )
        .unwrap();
    assert!(
        revoked
            .after
            .verify_management_selection(
                &WorkspaceId::default(),
                "agent-context/one",
                "collaboration-grant/one",
                2,
            )
            .is_err()
    );
}
#[test]
fn collaboration_revocation_keeps_original_operation_and_cannot_verify_old_credential() {
    let material = credential(1);
    let grant = grant(&material);
    let operation = OperationId::parse("op_00112233445566778899aabbccddeeff").unwrap();
    let revoked = grant.plan_revocation(operation.clone(), 1).unwrap();
    assert_eq!(revoked.operation_id, operation);
    assert_eq!(revoked.through_generation, 1);
    assert_eq!(revoked.after.generation, 2);
    assert!(!revoked.after.enabled);
    assert!(
        revoked
            .after
            .verify_bootstrap("agent-context/one", 2, &material)
            .is_err()
    );
    let encoded = serde_json::to_string(&revoked).unwrap();
    let replay: AgentCollaborationRevocation = serde_json::from_str(&encoded).unwrap();
    replay.validate().unwrap();
    assert_eq!(revoked, replay);
}
