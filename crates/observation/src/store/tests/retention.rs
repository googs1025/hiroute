use hiroute_domain::{
    CanonicalDigest, ContentCompleteness, ContentMode, DeletionDataClass, ObservationNackDetailV1,
    ObservationPrincipalV1, ObservationQueryError, ObservationQueryPort, SEVEN_DAYS_MILLIS,
    SessionDeletionSpecV1,
};
use tempfile::tempdir;

use crate::WriterCycleOutcome;

use super::support::*;

#[test]
fn seven_day_retention_tombstones_details_and_preserves_only_zero_content_value_rollup() {
    let temporary = tempdir().unwrap();
    let fixture = Fixture::new("retention");
    let store = open_store(temporary.path());
    let writer = writer(&store);
    let facts = fact_channel(&fixture, 512 * 1024);
    for envelope in request_facts(
        &fixture,
        "attempt-retention",
        finished("attempt-retention"),
        100,
    ) {
        assert!(matches!(
            offer_fact(&writer, &facts, envelope),
            WriterCycleOutcome::Ack(_)
        ));
    }
    install_content(&store, &fixture, b"retained-until-expiry", 200);

    assert_eq!(store.run_retention(SEVEN_DAYS_MILLIS + 10_000).unwrap(), 1);
    let detail = store
        .get_session(&fixture.workspace, &fixture.session, ContentMode::Messages)
        .unwrap();
    assert_eq!(
        detail.summary.content_completeness,
        ContentCompleteness::Expired
    );
    assert_eq!(detail.summary.turn_count, 0);
    assert_eq!(detail.summary.request_count, 0);
    assert!(detail.turns.is_empty());
    assert_eq!(
        detail.summary.tombstone_reason,
        Some(hiroute_domain::TombstoneReason::RetentionExpired)
    );
    assert_eq!(
        store
            .get_receipt(
                &fixture.workspace,
                &hiroute_domain::ReceiptId::parse(fixture.request.as_str()).unwrap(),
            )
            .unwrap_err(),
        ObservationQueryError::NotFound
    );
    let session_value = store
        .get_value(
            &fixture.workspace,
            &value_query("plan/codex-daily", "USD", Some(fixture.session.clone())),
        )
        .unwrap();
    assert!(session_value.entries.is_empty());
    assert!(session_value.daily_aggregates.is_empty());
    assert_eq!(session_value.estimated_total_savings_micros, None);
    let value = store
        .get_value(
            &fixture.workspace,
            &value_query("plan/codex-daily", "USD", None),
        )
        .unwrap();
    assert!(value.entries.is_empty());
    assert_eq!(value.daily_aggregates.len(), 1);
    assert_eq!(value.daily_aggregates[0].day_number, 0);
    assert_eq!(value.input_tokens, 100);
    assert_eq!(value.cache_read_tokens, 40);
    assert_eq!(value.routing_savings_micros, Some(300));
    assert_eq!(value.entitlement_savings_micros, Some(600));
    assert_eq!(value.estimated_total_savings_micros, Some(900));
    assert!(!value.detail_available);
    assert_eq!(
        store.get_status(&fixture.workspace).unwrap().content_bytes,
        0
    );
    assert_eq!(store.run_retention(SEVEN_DAYS_MILLIS + 20_000).unwrap(), 0);
}

#[test]
fn explicit_delete_requires_authority_exact_revision_and_digest_then_gcs_content() {
    let temporary = tempdir().unwrap();
    let fixture = Fixture::new("delete");
    let store = open_store(temporary.path());
    let writer = writer(&store);
    install_content(&store, &fixture, b"delete-me", 50_000);
    let content = content_channel(&fixture, 128 * 1024);
    for envelope in fixture.open_content_events(4, b"unfinished", 50_003) {
        assert!(matches!(
            offer_content(&writer, &content, envelope),
            WriterCycleOutcome::Ack(_)
        ));
    }
    let open_staging = store.staging_directory(
        &fixture.workspace,
        &hiroute_domain::ContentId::parse("content-open").expect("fixture content ID is valid"),
    );
    assert!(open_staging.exists());
    let spec = SessionDeletionSpecV1 {
        workspace_id: fixture.workspace.clone(),
        session_id: fixture.session.clone(),
        data_class: DeletionDataClass::ContentOnly,
        delete_rollups: false,
    };
    let facts_only = ObservationPrincipalV1::facts_only(fixture.workspace.clone());
    assert_eq!(
        store
            .preview_session_deletion(&facts_only, &spec)
            .unwrap_err(),
        ObservationQueryError::Unauthorized
    );
    let principal = ObservationPrincipalV1::local_user(fixture.workspace.clone());
    let preview = store.preview_session_deletion(&principal, &spec).unwrap();
    assert_eq!(preview.content_instances, 2);
    let wrong = CanonicalDigest::parse(format!("sha256:{}", "00".repeat(32))).unwrap();
    assert_eq!(
        store
            .apply_session_deletion(&principal, &spec, preview.store_revision, &wrong, 60_000,)
            .unwrap_err(),
        ObservationQueryError::StalePreview
    );
    assert_eq!(
        store.get_status(&fixture.workspace).unwrap().content_bytes,
        9
    );
    let outcome = store
        .apply_session_deletion(
            &principal,
            &spec,
            preview.store_revision,
            &preview.change_digest,
            60_000,
        )
        .unwrap();
    assert_eq!(
        outcome.tombstone_reason,
        Some(hiroute_domain::TombstoneReason::UserDeleted)
    );
    assert_eq!(outcome.garbage_collected_blobs, 1);
    assert!(!open_staging.exists());
    let detail = store
        .get_session(&fixture.workspace, &fixture.session, ContentMode::Messages)
        .unwrap();
    assert_eq!(
        detail.summary.content_completeness,
        ContentCompleteness::Deleted
    );
    assert!(detail.turns[0].messages.is_empty());
    assert_eq!(
        store.get_status(&fixture.workspace).unwrap().content_bytes,
        0
    );
    let rejected = offer_content(&writer, &content, fixture.content_begin(6, 60_001));
    assert!(matches!(
        rejected,
        WriterCycleOutcome::Nack(ref nack)
            if matches!(nack.detail, ObservationNackDetailV1::ImmutableProjectionConflict { .. })
    ));
}

#[test]
fn deletion_and_expiry_allow_new_requests_without_reviving_old_origins() {
    use crate::writer::{IngestOutcome, ObservationCommitPort};
    for expiry in [false, true] {
        let directory = tempdir().unwrap();
        let store = open_store(directory.path());
        let old = Fixture::new("old-origin");
        let old_facts = request_facts(&old, "attempt-old", finished("attempt-old"), 100);
        for fact in &old_facts {
            assert!(matches!(
                store.ingest_fact(fact, &[]).unwrap(),
                IngestOutcome::Ack(_)
            ));
        }
        let next_time = if expiry {
            let time = SEVEN_DAYS_MILLIS + 1000;
            assert_eq!(store.expire_request_details(time, 16).unwrap(), 1);
            time + 100
        } else {
            let principal = ObservationPrincipalV1::local_user(old.workspace.clone());
            let spec = SessionDeletionSpecV1 {
                workspace_id: old.workspace.clone(),
                session_id: old.session.clone(),
                data_class: DeletionDataClass::FactsAndContent,
                delete_rollups: false,
            };
            let preview = store.preview_session_deletion(&principal, &spec).unwrap();
            store
                .apply_session_deletion(
                    &principal,
                    &spec,
                    preview.store_revision,
                    &preview.change_digest,
                    1000,
                )
                .unwrap();
            1100
        };
        let mut new = Fixture::new("new-origin");
        new.session = old.session.clone();
        for fact in request_facts(&new, "attempt-new", finished("attempt-new"), next_time) {
            assert!(matches!(
                store.ingest_fact(&fact, &[]).unwrap(),
                IngestOutcome::Ack(_)
            ));
        }
        assert!(matches!(
            store.ingest_fact(&old_facts[0], &[]).unwrap(),
            IngestOutcome::Nack(_)
        ));
        let view = store
            .get_session(&new.workspace, &new.session, ContentMode::None)
            .unwrap();
        assert!(view.summary.tombstone_reason.is_none());
        assert_eq!(view.summary.request_count, 1);
    }
}

#[test]
fn upgrade_retains_old_deletion_watermark_and_retired_request_ids() {
    use crate::writer::{IngestOutcome, ObservationCommitPort};
    let directory = tempdir().unwrap();
    let store = open_store(directory.path());
    let old = Fixture::new("legacy-deleted");
    let facts = request_facts(&old, "attempt", finished("attempt"), 100);
    for fact in &facts {
        store.ingest_fact(fact, &[]).unwrap();
    }
    store.connection.lock().execute("INSERT INTO observation_tombstones(workspace_id,session_id,reason,delete_scope,deleted_at_ms) VALUES(?1,?2,'user_deleted','content_only',1000)", rusqlite::params![old.workspace.as_str(),old.session.as_str()]).unwrap();
    store
        .connection
        .lock()
        .execute(
            "UPDATE observation_meta SET value='2' WHERE key='observation_extension_version'",
            [],
        )
        .unwrap();
    drop(store);
    let store = open_store(directory.path());
    assert!(matches!(
        store.ingest_fact(&facts[0], &[]).unwrap(),
        IngestOutcome::Nack(_)
    ));
    let mut fresh = Fixture::new("legacy-fresh");
    fresh.session = old.session.clone();
    for fact in request_facts(&fresh, "fresh-attempt", finished("fresh-attempt"), 1100) {
        assert!(matches!(
            store.ingest_fact(&fact, &[]).unwrap(),
            IngestOutcome::Ack(_)
        ));
    }
}
