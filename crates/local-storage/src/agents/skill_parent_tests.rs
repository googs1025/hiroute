//! Shared Skill record → upgraded effect → restart → removal → directory cleanup/reinstall.
use super::*;

#[test]
fn skill_directory_ownership_survives_upgrade_and_reinstall_without_original_intent() {
    let root = crate::test_tempdir().unwrap();
    let path = root.path().join("skills/hiroute/SKILL.md");
    let prototype = intent(
        AgentConnectionTransactionKindV1::Apply,
        AgentConnectionEffectRoleV1::RoutingSkill,
        None,
    );
    let mut record = None;
    let templates = [
        CollaborationSkillTemplate::bundled("first", "# First\n").unwrap(),
        CollaborationSkillTemplate::bundled("upgrade", "# Upgraded\n").unwrap(),
    ];
    for cycle in 0..2 {
        for (step, template) in templates.iter().enumerate() {
            let artifacts = store(root.path(), prototype.target(), &path);
            let observed = fs::read(&path)
                .ok()
                .map(|bytes| CanonicalDigest::of_bytes(&bytes));
            let mut planned = plan_skill_install(
                "root/shared",
                "context/one",
                template,
                record.as_ref(),
                observed.as_ref(),
            )
            .unwrap();
            let install = intent(
                AgentConnectionTransactionKindV1::Apply,
                AgentConnectionEffectRoleV1::RoutingSkill,
                artifacts
                    .current_external_fingerprint(prototype.target())
                    .unwrap(),
            );
            let operation =
                OperationId::parse(format!("op_{:032x}", 0x100 + cycle * 10 + step)).unwrap();
            let effect = stage_collaboration_skill(
                &artifacts,
                &operation,
                &install,
                record.as_ref(),
                &mut planned,
                Some(template),
            )
            .unwrap()
            .unwrap();
            artifacts.activate_artifact(&effect).unwrap();
            assert!(planned.next.file_effect.is_some());
            // The durable record contains only the new effect ref; callers need no original intent.
            let encoded = serde_json::to_vec(&planned.next).unwrap();
            assert!(!String::from_utf8_lossy(&encoded).contains(root.path().to_str().unwrap()));
            record = Some(serde_json::from_slice(&encoded).unwrap());
        }
        let artifacts = store(root.path(), prototype.target(), &path);
        let previous = record.as_ref().unwrap();
        let mut planned =
            plan_skill_remove("context/one", previous, Some(&templates[1].digest)).unwrap();
        let removal = intent(
            AgentConnectionTransactionKindV1::Restore,
            AgentConnectionEffectRoleV1::RoutingSkill,
            artifacts
                .current_external_fingerprint(prototype.target())
                .unwrap(),
        );
        let operation = OperationId::parse(format!("op_{:032x}", 0x105 + cycle * 10)).unwrap();
        let effect = stage_collaboration_skill(
            &artifacts,
            &operation,
            &removal,
            Some(previous),
            &mut planned,
            None,
        )
        .unwrap()
        .unwrap();
        assert!(
            finish_collaboration_skill_removal(&artifacts, &operation, &removal, &planned.next)
                .is_err()
        );
        artifacts.activate_artifact(&effect).unwrap();
        drop(artifacts);
        let reopened = store(root.path(), prototype.target(), &path);
        assert!(
            finish_collaboration_skill_removal(&reopened, &operation, &removal, &planned.next)
                .unwrap()
        );
        assert!(!root.path().join("skills").exists());
        assert!(
            finish_collaboration_skill_removal(&reopened, &operation, &removal, &planned.next)
                .unwrap()
        );
        record = Some(planned.next);
    }
}

#[test]
fn skill_cleanup_keeps_user_content_and_rejects_a_live_shared_reference() {
    let root = crate::test_tempdir().unwrap();
    let path = root.path().join("skills/hiroute/SKILL.md");
    let install = intent(
        AgentConnectionTransactionKindV1::Apply,
        AgentConnectionEffectRoleV1::RoutingSkill,
        None,
    );
    let artifacts = store(root.path(), install.target(), &path);
    let template = CollaborationSkillTemplate::bundled("first", "# Skill\n").unwrap();
    let mut planned =
        plan_skill_install("root/shared", "context/one", &template, None, None).unwrap();
    let operation = OperationId::parse("op_aaaaaaaa11111111aaaaaaaa11111111").unwrap();
    let effect = stage_collaboration_skill(
        &artifacts,
        &operation,
        &install,
        None,
        &mut planned,
        Some(&template),
    )
    .unwrap()
    .unwrap();
    artifacts.activate_artifact(&effect).unwrap();
    let mut removed =
        plan_skill_remove("context/one", &planned.next, Some(&template.digest)).unwrap();
    let removal = intent(
        AgentConnectionTransactionKindV1::Restore,
        AgentConnectionEffectRoleV1::RoutingSkill,
        artifacts
            .current_external_fingerprint(install.target())
            .unwrap(),
    );
    let remove_operation = OperationId::parse("op_bbbbbbbb22222222bbbbbbbb22222222").unwrap();
    let effect = stage_collaboration_skill(
        &artifacts,
        &remove_operation,
        &removal,
        Some(&planned.next),
        &mut removed,
        None,
    )
    .unwrap()
    .unwrap();
    artifacts.activate_artifact(&effect).unwrap();
    let mut live = removed.next.clone();
    live.contexts.insert("context/two".into());
    assert!(
        finish_collaboration_skill_removal(&artifacts, &remove_operation, &removal, &live).is_err()
    );
    let user_file = path.parent().unwrap().join("user.txt");
    fs::write(&user_file, b"user content").unwrap();
    assert!(
        !finish_collaboration_skill_removal(&artifacts, &remove_operation, &removal, &removed.next)
            .unwrap()
    );
    assert_eq!(fs::read(&user_file).unwrap(), b"user content");
    assert!(removed.next.contexts.is_empty());
}
