use super::*;
fn template() -> CollaborationSkillTemplate {
    CollaborationSkillTemplate::bundled(
        "test-collaboration/1",
        "---\nname: hiroute-collaboration\n---\nUse the trusted task CLI.\n",
    )
    .unwrap()
}
#[test]
fn agent_skill_shared_root_keeps_file_until_final_reference() {
    let template = template();
    let one =
        plan_skill_install("skill-root/private", "context/one", &template, None, None).unwrap();
    assert_eq!(one.file_action, SkillFileAction::Install);
    let two = plan_skill_install(
        "skill-root/private",
        "context/two",
        &template,
        Some(&one.next),
        Some(&template.digest),
    )
    .unwrap();
    assert_eq!(two.file_action, SkillFileAction::Keep);
    assert_eq!(two.next.contexts.len(), 2);
    let removed = plan_skill_remove("context/one", &two.next, Some(&template.digest)).unwrap();
    assert_eq!(removed.file_action, SkillFileAction::Keep);
    assert_eq!(
        removed.next.contexts,
        BTreeSet::from(["context/two".into()])
    );
    let last = plan_skill_remove("context/two", &removed.next, Some(&template.digest)).unwrap();
    assert_eq!(last.file_action, SkillFileAction::Remove);
    assert!(last.next.contexts.is_empty());
    let replay = plan_skill_remove("context/two", &last.next, None).unwrap();
    assert_eq!(replay.file_action, SkillFileAction::Keep);
    assert_eq!(replay.next, last.next);
}
#[test]
fn agent_skill_user_file_never_becomes_managed_by_name() {
    assert_eq!(
        plan_skill_install(
            "skill-root/private",
            "context/one",
            &template(),
            None,
            Some(&CanonicalDigest::of_bytes(b"user skill"))
        )
        .unwrap_err(),
        SkillPlanningError::UnmanagedFile
    );
}

#[test]
fn identical_unmanaged_skill_is_borrowed_and_never_removed() {
    let template = template();
    let installed = plan_skill_install(
        "skill-root/private",
        "context/one",
        &template,
        None,
        Some(&template.digest),
    )
    .unwrap();
    assert_eq!(installed.file_action, SkillFileAction::Keep);
    assert_eq!(
        installed.next.file_ownership,
        CollaborationSkillFileOwnership::BorrowedIdentical
    );

    let removed =
        plan_skill_remove("context/one", &installed.next, Some(&template.digest)).unwrap();
    assert!(removed.next.contexts.is_empty());
    assert_eq!(removed.file_action, SkillFileAction::Keep);
}

#[test]
fn borrowed_identical_skill_releases_reference_after_user_file_drift() {
    let template = template();
    let borrowed = plan_skill_install(
        "skill-root/private",
        "context/one",
        &template,
        None,
        Some(&template.digest),
    )
    .unwrap();
    let changed = CanonicalDigest::of_bytes(b"user changed borrowed skill");

    for observed in [Some(&changed), None] {
        let removed = plan_skill_remove("context/one", &borrowed.next, observed).unwrap();
        assert!(removed.next.contexts.is_empty());
        assert_eq!(removed.file_action, SkillFileAction::Keep);
    }
}

#[test]
fn borrowed_identical_skill_blocks_bundle_content_replacement() {
    let original = template();
    let borrowed = plan_skill_install(
        "skill-root/private",
        "context/one",
        &original,
        None,
        Some(&original.digest),
    )
    .unwrap();
    let replacement = CollaborationSkillTemplate::bundled(
        "test-collaboration/2",
        "---\nname: hiroute-collaboration\n---\nUse a newer trusted task CLI.\n",
    )
    .unwrap();
    assert_eq!(
        plan_skill_install(
            "skill-root/private",
            "context/one",
            &replacement,
            Some(&borrowed.next),
            Some(&original.digest),
        )
        .unwrap_err(),
        SkillPlanningError::FileChanged,
        "a bundle update must not turn a borrowed user file into an owned rewrite"
    );
}

#[test]
fn agent_skill_file_conflict_does_not_restore_removed_context_reference() {
    let template = template();
    let initial =
        plan_skill_install("skill-root/private", "context/one", &template, None, None).unwrap();
    let changed = CanonicalDigest::of_bytes(b"user changed installed skill");
    let removed = plan_skill_remove("context/one", &initial.next, Some(&changed)).unwrap();
    assert!(removed.next.contexts.is_empty());
    assert_eq!(removed.file_action, SkillFileAction::Conflict);
    // Existing Operation recovery keeps the zero-reference ownership record until cleanup.
    let retry = plan_skill_remove("context/one", &removed.next, Some(&changed)).unwrap();
    assert_eq!(retry.file_action, SkillFileAction::Conflict);
    assert_eq!(retry.next, removed.next);
}
