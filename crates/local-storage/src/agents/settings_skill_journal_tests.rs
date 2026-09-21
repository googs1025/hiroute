//! Real protected file staging driven only by the original serialized settings effect.
use super::*;

fn settings_control(restore: bool) -> (ChangeSpecV1, AgentConnectionControlIntentV1) {
    let spec = ChangeSpecV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        command_id: AgentConnectionTransactionKindV1::Settings
            .command_id()
            .into(),
        resource_id: Some("context/one".into()),
        desired_state: json!({
            "schema_version":{"major":2,"minor":0}, "context_id":"context/one",
            "collaboration": if restore { json!({"intent":"restore","restore_point_ref":"restore/one"}) }
                else { json!({"intent":"configure","settings":{"trigger_mode":"explicit"}}) },
        }),
    };
    let control = AgentConnectionControlIntentV1::from_settings_planner(
        AgentConnectionTransactionSubjectV1::from_registered_profile(
            "agent.codex",
            "default",
            "codex.profile.v1",
        )
        .unwrap(),
        &spec,
        true,
        &json!({"revision":1}),
    )
    .unwrap();
    (spec, control)
}

fn restore_intent(bytes: &[u8]) -> ExternalEffectIntentV1 {
    let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    ExternalEffectIntentV1::from_registered_adapter(
        value["effect_id"].as_str().unwrap(),
        serde_json::from_value(value["kind"].clone()).unwrap(),
        value["target"].as_str().unwrap(),
        serde_json::from_value(value["before_fingerprint"].clone()).unwrap(),
        value["desired"].clone(),
        value["desired_mode"].as_u64().unwrap() as u32,
        value["sensitive"].as_bool().unwrap(),
    )
    .unwrap()
}

#[test]
fn original_settings_journal_recovers_skill_stage_activate_and_remove_without_bundle_lookup() {
    let root = crate::test_tempdir().unwrap();
    let path = root.path().join("skills/hiroute/SKILL.md");
    let template =
        CollaborationSkillTemplate::bundled("original", "# Original confirmed content\n").unwrap();
    let planned = plan_skill_install("root/shared", "context/one", &template, None, None).unwrap();
    let (spec, control) = settings_control(false);
    let intent = settings_skill_file_intent(
        &control,
        "context/one",
        None,
        &planned,
        Some(&template),
        None,
    )
    .unwrap();
    let plan =
        TransactionPlanV1::from_agent_connection_planner(spec, control, vec![intent.clone()])
            .unwrap();
    assert_eq!(plan.external().len(), 1);
    let encoded = serde_json::to_vec(&intent).unwrap();
    drop(template);
    drop(planned);
    drop(plan);
    let operation = OperationId::parse(format!("op_{:032x}", 0xa14001)).unwrap();
    let intent = restore_intent(&encoded);
    {
        let artifacts = store(root.path(), intent.target(), &path);
        stage_settings_skill_file(&artifacts, &operation, &intent).unwrap();
        assert!(!path.exists(), "staging is not activation");
    }
    let artifacts = store(root.path(), intent.target(), &path);
    let staged = stage_settings_skill_file(&artifacts, &operation, &intent).unwrap();
    artifacts.activate_artifact(&staged).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"# Original confirmed content\n");
    let (expected_revision, current) = settings_skill_file_record(&operation, &intent).unwrap();
    assert_eq!(expected_revision, 0);
    assert_eq!(
        current.file_effect.as_ref().unwrap().operation_id,
        operation
    );

    let removed =
        plan_skill_remove("context/one", &current, Some(&current.content_digest)).unwrap();
    let (spec, control) = settings_control(true);
    let removal = settings_skill_file_intent(
        &control,
        "context/one",
        Some(&current),
        &removed,
        None,
        artifacts
            .current_external_fingerprint(intent.target())
            .unwrap(),
    )
    .unwrap();
    TransactionPlanV1::from_agent_connection_planner(spec, control, vec![removal.clone()]).unwrap();
    let encoded = serde_json::to_vec(&removal).unwrap();
    drop(artifacts);
    drop(removed);
    let removal = restore_intent(&encoded);
    let remove_operation = OperationId::parse(format!("op_{:032x}", 0xa14002)).unwrap();
    let artifacts = store(root.path(), removal.target(), &path);
    let staged = stage_settings_skill_file(&artifacts, &remove_operation, &removal).unwrap();
    assert!(path.exists());
    // The real settings Operation must persist revocation/reference CAS before this activation.
    // This component test exercises the file effect, and does not claim authorization coverage.
    artifacts.activate_artifact(&staged).unwrap();
    let (expected_revision, next) =
        settings_skill_file_record(&remove_operation, &removal).unwrap();
    assert_eq!(expected_revision, current.revision);
    assert!(next.contexts.is_empty());
    assert!(
        finish_collaboration_skill_removal(&artifacts, &remove_operation, &removal, &next).unwrap()
    );
    assert!(!root.path().join("skills").exists());
}

#[test]
fn identical_existing_skill_is_referenced_without_rewrite_or_delete() {
    use hiroute_domain::{
        BeginOperationOutcome, IdempotencyScopeV1, OperationState, OperationV1,
        ProtectedApplyCapability, RevisionSetV1, WorkspaceId,
    };
    use std::os::unix::fs::MetadataExt;

    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("SKILL.md");
    let content = b"# Existing identical skill\n";
    fs::write(&path, content).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let original_metadata = fs::metadata(&path).unwrap();
    let original_inode = original_metadata.ino();
    let original_mode = original_metadata.mode() & 0o777;
    let template =
        CollaborationSkillTemplate::bundled("original", std::str::from_utf8(content).unwrap())
            .unwrap();
    let planned = plan_skill_install(
        "root/shared",
        "context/one",
        &template,
        None,
        Some(&template.digest),
    )
    .unwrap();
    assert_eq!(planned.file_action, SkillFileAction::Keep);
    let spec = ChangeSpecV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        command_id: AgentConnectionTransactionKindV1::Settings
            .command_id()
            .into(),
        resource_id: Some("context/one".into()),
        desired_state: json!({
            "schema_version":{"major":2,"minor":0}, "context_id":"context/one",
            "collaboration":{"intent":"configure","settings":{"trigger_mode":"explicit"}},
        }),
    };
    let subject = AgentConnectionTransactionSubjectV1::from_registered_profile(
        "agent.codex",
        "default",
        "codex.profile.v1",
    )
    .unwrap();
    let target = AgentConnectionEffectRoleV1::RoutingSkill
        .target_for(&subject)
        .unwrap();
    let artifacts = store(root.path(), &target, &path);
    let fingerprint = artifacts
        .current_external_fingerprint(&target)
        .unwrap()
        .unwrap();
    let mut forged = planned.clone();
    forged.next.file_ownership = CollaborationSkillFileOwnership::Managed;
    assert!(
        settings_skill_reference_change(
            "context/one",
            &target,
            None,
            &forged,
            Some(fingerprint.clone()),
            SkillReferenceAction::Add,
        )
        .is_err(),
        "an identical user file must not be reclassified as managed"
    );
    let reference = settings_skill_reference_change(
        "context/one",
        &target,
        None,
        &planned,
        Some(fingerprint),
        SkillReferenceAction::Add,
    )
    .unwrap();
    let control = AgentConnectionControlIntentV1::from_settings_planner(
        subject,
        &spec,
        false,
        &json!({"mutations":{
            "skill_record_change":true,
            "skill_reference_change":reference,
            "collaboration":{"worker_channel":"local_trust_v1","trigger_mode":"explicit"},
        }}),
    )
    .unwrap();
    let transaction =
        TransactionPlanV1::from_agent_connection_planner(spec, control, vec![]).unwrap();
    assert!(transaction.external().is_empty());
    let workspace = WorkspaceId::default();
    let scope = IdempotencyScopeV1::new(
        "interactive-user",
        "ApplyAgentSettingsChange",
        "borrowed-skill-reference",
    )
    .unwrap();
    let request_digest = CanonicalDigest::of_bytes(b"borrowed-skill-reference");
    let mut operation = OperationV1::new(
        OperationId::derive(&workspace, &scope, &request_digest),
        workspace.clone(),
        scope,
        request_digest,
        CanonicalDigest::of_bytes(b"confirmed-borrowed-skill-reference"),
        RevisionSetV1 {
            target: 0,
            dependencies: Default::default(),
        },
        transaction,
    )
    .unwrap();
    let control = ControlStore::open(
        &crate::test_storage_authority(),
        root.path().join("control.db"),
        root.path().join("backups"),
    )
    .unwrap();
    let capability = "borrowed-skill-reference-capability";
    control
        .grant_apply_capability(capability, &operation, i64::MAX)
        .unwrap();
    let authorization = control
        .verify_apply_authorization(
            &ProtectedApplyCapability::new(capability.into()).unwrap(),
            &workspace,
            &operation.idempotency.principal,
            &operation.idempotency.operation_kind,
            &operation.accepted_digest,
            &operation.expected_revisions,
        )
        .unwrap();
    assert_eq!(
        control.begin_operation(&operation, &authorization).unwrap(),
        BeginOperationOutcome::Created
    );
    operation.state = OperationState::Succeeded;
    let current =
        persist_settings_skill_reference_change(&control, &artifacts, &operation, &target)
            .unwrap()
            .unwrap();
    assert_eq!(
        current.file_ownership,
        CollaborationSkillFileOwnership::BorrowedIdentical
    );
    assert!(current.file_effect.is_none());
    assert_eq!(fs::read(&path).unwrap(), content);
    assert_eq!(fs::metadata(&path).unwrap().ino(), original_inode);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, original_mode);

    let removed = plan_skill_remove("context/one", &current, Some(&template.digest)).unwrap();
    assert_eq!(removed.file_action, SkillFileAction::Keep);
    assert!(removed.next.contexts.is_empty());
}

#[test]
fn skill_journal_rejects_cross_context_reference_edits_and_user_file_drift() {
    let root = crate::test_tempdir().unwrap();
    let path = root.path().join("SKILL.md");
    let template = CollaborationSkillTemplate::bundled("original", "# Confirmed\n").unwrap();
    let mut planned =
        plan_skill_install("root/shared", "context/one", &template, None, None).unwrap();
    let (_, control) = settings_control(false);
    planned.next.contexts.insert("context/unconfirmed".into());
    assert!(
        settings_skill_file_intent(
            &control,
            "context/one",
            None,
            &planned,
            Some(&template),
            None
        )
        .is_err()
    );
    planned.next.contexts.remove("context/unconfirmed");
    let intent = settings_skill_file_intent(
        &control,
        "context/one",
        None,
        &planned,
        Some(&template),
        None,
    )
    .unwrap();
    fs::write(&path, b"# User content\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let artifacts = store(root.path(), intent.target(), &path);
    let operation = OperationId::parse(format!("op_{:032x}", 0xa14003)).unwrap();
    assert!(stage_settings_skill_file(&artifacts, &operation, &intent).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"# User content\n");
}

#[test]
fn admitted_settings_operation_persists_exact_skill_reference_after_stage_and_reopens() {
    use hiroute_domain::{
        BeginOperationOutcome, IdempotencyScopeV1, OperationState, OperationV1,
        ProtectedApplyCapability, RevisionSetV1, WorkspaceId,
    };
    let root = crate::test_tempdir().unwrap();
    let database = root.path().join("data/control.db");
    let backups = root.path().join("backups");
    let path = root.path().join("SKILL.md");
    let template =
        CollaborationSkillTemplate::bundled("original", "# Durable reference\n").unwrap();
    let planned = plan_skill_install("root/shared", "context/one", &template, None, None).unwrap();
    let (spec, settings) = settings_control(false);
    let intent = settings_skill_file_intent(
        &settings,
        "context/one",
        None,
        &planned,
        Some(&template),
        None,
    )
    .unwrap();
    let plan =
        TransactionPlanV1::from_agent_connection_planner(spec, settings, vec![intent.clone()])
            .unwrap();
    let workspace = WorkspaceId::default();
    let scope = IdempotencyScopeV1::new(
        "interactive-user",
        "ApplyAgentSettingsChange",
        "skill-reference-original",
    )
    .unwrap();
    let request_digest = CanonicalDigest::of_bytes(b"skill-reference-original");
    let mut operation = OperationV1::new(
        OperationId::derive(&workspace, &scope, &request_digest),
        workspace.clone(),
        scope,
        request_digest,
        CanonicalDigest::of_bytes(b"confirmed-skill-reference"),
        RevisionSetV1 {
            target: 0,
            dependencies: Default::default(),
        },
        plan,
    )
    .unwrap();
    let control =
        ControlStore::open(&crate::test_storage_authority(), &database, &backups).unwrap();
    let capability = "skill-reference-test-capability";
    control
        .grant_apply_capability(capability, &operation, i64::MAX)
        .unwrap();
    let authorization = control
        .verify_apply_authorization(
            &ProtectedApplyCapability::new(capability.into()).unwrap(),
            &workspace,
            &operation.idempotency.principal,
            &operation.idempotency.operation_kind,
            &operation.accepted_digest,
            &operation.expected_revisions,
        )
        .unwrap();
    assert_eq!(
        control.begin_operation(&operation, &authorization).unwrap(),
        BeginOperationOutcome::Created
    );
    let native = store(root.path(), intent.target(), &path);
    operation.state = OperationState::Activating;
    control.save_operation(&mut operation).unwrap();
    assert!(persist_settings_skill_file_record(&control, &native, &operation, &intent).is_err());
    assert!(
        control
            .skill_installation(&workspace, "root/shared")
            .unwrap()
            .is_none()
    );
    let staged = stage_settings_skill_file(&native, &operation.operation_id, &intent).unwrap();
    let record =
        persist_settings_skill_file_record(&control, &native, &operation, &intent).unwrap();
    assert_eq!(
        record.file_effect.as_ref().unwrap().operation_id,
        operation.operation_id
    );
    assert!(!path.exists());
    drop(native);
    drop(control);

    let control =
        ControlStore::open(&crate::test_storage_authority(), &database, &backups).unwrap();
    let original = control
        .load_operation(&operation.operation_id)
        .unwrap()
        .unwrap();
    let intent = &original.plan.external()[0];
    let native = store(root.path(), intent.target(), &path);
    assert_eq!(
        persist_settings_skill_file_record(&control, &native, &original, intent).unwrap(),
        record
    );
    native.activate_artifact(&staged).unwrap();
    assert_eq!(
        persist_settings_skill_file_record(&control, &native, &original, intent).unwrap(),
        record
    );
    assert_eq!(fs::read(&path).unwrap(), b"# Durable reference\n");

    let mut foreign = original.clone();
    foreign.operation_id = OperationId::parse(format!("op_{:032x}", 0xa14004)).unwrap();
    assert!(persist_settings_skill_file_record(&control, &native, &foreign, intent).is_err());
    fs::write(&path, b"# User edit after activation\n").unwrap();
    assert!(persist_settings_skill_file_record(&control, &native, &original, intent).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"# User edit after activation\n");
    assert_eq!(
        control
            .skill_installation(&workspace, "root/shared")
            .unwrap(),
        Some(record)
    );
}
