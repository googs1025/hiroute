//! Real native Session -> protected Application -> daemon/store publication path.
use super::*;

fn editor_input(
    action: &str,
    plan: &AgentPlanStatusV2,
    editor: &hiroute_domain::PlanEditorStateV2,
    draft_revision: Option<u64>,
) -> crate::session::EditorInput {
    serde_json::from_value(serde_json::json!({
        "action": action,
        "plan_id": plan.agent_plan_id,
        "draft_id": "draft/native-publication",
        "expected_head_revision": plan.head.head_revision,
        "expected_draft_revision": draft_revision,
        "editor": editor,
        "language": "en"
    }))
    .unwrap()
}

#[tokio::test]
async fn edited_draft_publish_uses_submitted_content_and_consumes_the_exact_draft() {
    let root = prepare();
    let mut session = open(root.path());
    let original = plan(&mut session).await;
    let mut editor = original
        .desired
        .editor(Some(original.model_alias.as_str().into()))
        .unwrap();
    editor.purpose = "saved draft before the final edit".into();
    let draft = session
        .preview_editor(editor_input("save_draft", &original, &editor, None))
        .await
        .unwrap();
    assert!(!draft.requires_confirmation());
    session
        .finish_native_confirmation(draft, true)
        .await
        .unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let draft_revision = snapshot
        .catalog
        .drafts
        .iter()
        .find(|draft| draft.draft_id == "draft/native-publication")
        .unwrap()
        .revision;

    editor.purpose = "content edited immediately before ordinary Publish".into();
    let publication = session
        .preview_editor(editor_input(
            "publish",
            &original,
            &editor,
            Some(draft_revision),
        ))
        .await
        .unwrap();
    assert!(
        !publication.requires_confirmation(),
        "ordinary Publish has no hidden confirmation dialog"
    );
    let outcome = session
        .finish_native_confirmation(publication, true)
        .await
        .unwrap();
    assert_eq!(outcome.operation.unwrap().state, "succeeded");
    let current = plan(&mut session).await;
    assert_eq!(current.desired.purpose.as_str(), editor.purpose);
    assert_eq!(current.desired.strategy, original.desired.strategy);
    assert_eq!(current.model_alias, original.model_alias);
    assert!(
        session
            .snapshot()
            .await
            .unwrap()
            .catalog
            .drafts
            .iter()
            .all(|draft| draft.draft_id != "draft/native-publication")
    );
    drop(session);
    let mut restarted = open(root.path());
    assert_eq!(
        plan(&mut restarted).await.desired.purpose.as_str(),
        editor.purpose
    );
    assert!(
        restarted
            .snapshot()
            .await
            .unwrap()
            .catalog
            .drafts
            .iter()
            .all(|draft| draft.draft_id != "draft/native-publication")
    );
}
