use super::*;
use hiroute_domain::WorkspaceId;
use hiroute_observation::DigestAuthority;

fn scope() -> ManagedTextScope {
    ManagedTextScope {
        workspace_id: WorkspaceId::default(),
        task_id: "task".into(),
        run_id: "original-run".into(),
    }
}

fn writer(store: Arc<LocalObservationStore>) -> RunBodyWriter {
    RunBodyWriter::create(
        store,
        scope(),
        ManagedTextPurpose::NativeRecovery,
        "recovery".into(),
        10,
        10,
    )
    .unwrap()
}

#[test]
fn output_larger_than_a_chunk_uses_managed_store_without_resetting_original_retention() {
    let temp = tempfile::tempdir().unwrap();
    let store =
        Arc::new(LocalObservationStore::open(temp.path(), DigestAuthority::new([4; 32])).unwrap());
    let mut writer = writer(store.clone());
    let content = "x".repeat(CHUNK_BYTES + 13);
    writer.append(content.as_bytes(), 20).unwrap();
    let reference = writer.finish(30).unwrap();
    assert_eq!(
        reference.original_retention_deadline_ms,
        10 + hiroute_observation::managed_text::RETENTION_MS
    );
    let mut checks = 0;
    let actual = read_required_body(&store, &scope(), &reference, 40, PAGE_BYTES, |_| {
        checks += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(actual, content.as_bytes());
    assert!(checks >= 2);
    let mut other_scope = scope();
    other_scope.run_id = "new-run".into();
    assert_eq!(
        read_required_body(&store, &other_scope, &reference, 40, PAGE_BYTES, |_| Ok(()))
            .unwrap_err(),
        DelegationErrorV1::ResumeUnavailable
    );
}

#[test]
fn old_complete_reference_cannot_bypass_deletion_or_current_content_authority() {
    let temp = tempfile::tempdir().unwrap();
    let store =
        Arc::new(LocalObservationStore::open(temp.path(), DigestAuthority::new([4; 32])).unwrap());
    let mut writer = writer(store.clone());
    writer.append(b"native recovery", 20).unwrap();
    let reference = writer.finish(30).unwrap();
    assert_eq!(
        read_required_body(&store, &scope(), &reference, 40, PAGE_BYTES, |_| Err(
            DelegationErrorV1::PermissionDenied
        ))
        .unwrap_err(),
        DelegationErrorV1::PermissionDenied
    );
    let preview = store.managed_text_delete_preview(&scope(), 50, 50).unwrap();
    store.managed_text_delete_apply(&preview).unwrap();
    assert_eq!(reference.state, ManagedTextState::Complete);
    assert_eq!(
        read_required_body(&store, &scope(), &reference, 60, PAGE_BYTES, |_| Ok(())).unwrap_err(),
        DelegationErrorV1::ResumeUnavailable
    );
}

#[test]
fn deleted_stream_cannot_be_completed_or_recreated_by_a_late_notification() {
    let temp = tempfile::tempdir().unwrap();
    let store =
        Arc::new(LocalObservationStore::open(temp.path(), DigestAuthority::new([4; 32])).unwrap());
    let mut writer = writer(store.clone());
    writer.append(b"first", 20).unwrap();
    let preview = store.managed_text_delete_preview(&scope(), 30, 30).unwrap();
    store.managed_text_delete_apply(&preview).unwrap();
    assert_eq!(
        writer.append(b"late", 40),
        Err(DelegationErrorV1::ContentUnavailable)
    );
    assert_eq!(
        writer.finish(40),
        Err(DelegationErrorV1::ContentUnavailable)
    );
}
