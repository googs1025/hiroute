use hiroute_domain::{
    ContentMode, DeletionDataClass, ObservationPrincipalV1, ObservationQueryError,
    ObservationQueryPort, ReceiptId, SEVEN_DAYS_MILLIS, SessionDeletionSpecV1, TombstoneReason,
};
use rusqlite::params;
use tempfile::tempdir;

use super::support::*;

#[test]
fn delete_rollups_true_is_legal_for_live_content_and_facts_scopes_without_cross_deletion() {
    for (case_name, data_class) in [
        ("live-content", DeletionDataClass::ContentOnly),
        ("live-facts", DeletionDataClass::FactsAndContent),
    ] {
        let temporary = tempdir().unwrap();
        let fixture = Fixture::new(case_name);
        let receipt_id = format!("receipt-{case_name}");
        let attempt_id = format!("attempt-{case_name}");
        let store = open_store(temporary.path());
        install_value_request(
            &store,
            &fixture,
            &receipt_id,
            &attempt_id,
            finished(&attempt_id),
            100,
        );
        install_content(&store, &fixture, b"live-delete-content", 200);
        let spec = SessionDeletionSpecV1 {
            workspace_id: fixture.workspace.clone(),
            session_id: fixture.session.clone(),
            data_class: data_class.clone(),
            delete_rollups: true,
        };
        let principal = ObservationPrincipalV1::local_user(fixture.workspace.clone());
        let preview = store.preview_session_deletion(&principal, &spec).unwrap();
        assert_eq!(preview.content_instances, 1);
        let outcome = store
            .apply_session_deletion(
                &principal,
                &spec,
                preview.store_revision,
                &preview.change_digest,
                1_000,
            )
            .unwrap();
        assert_eq!(outcome.tombstone_reason, None);
        assert_eq!(
            count_for_session(&store, "observation_tombstones", &fixture),
            0
        );
        assert_eq!(
            store.get_status(&fixture.workspace).unwrap().content_bytes,
            0
        );

        let value = store
            .get_value(
                &fixture.workspace,
                &value_query("plan/codex-daily", "USD", Some(fixture.session.clone())),
            )
            .unwrap();
        if data_class == DeletionDataClass::ContentOnly {
            assert_eq!(value.entries.len(), 1);
            // Raw V1 receipts may carry literal keyword evidence. Content-only
            // deletion retains the numeric ledger, but closes this raw surface.
            assert_eq!(
                store
                    .get_receipt(
                        &fixture.workspace,
                        &ReceiptId::parse(fixture.request.as_str()).unwrap()
                    )
                    .unwrap_err(),
                ObservationQueryError::NotFound
            );
            let detail = store
                .get_session(&fixture.workspace, &fixture.session, ContentMode::Messages)
                .unwrap();
            assert!(detail.turns[0].messages.is_empty());
        } else {
            assert!(value.entries.is_empty());
            assert_eq!(
                store
                    .get_receipt(
                        &fixture.workspace,
                        &ReceiptId::parse(fixture.request.as_str()).unwrap(),
                    )
                    .unwrap_err(),
                ObservationQueryError::NotFound
            );
            assert_eq!(
                store
                    .get_session(&fixture.workspace, &fixture.session, ContentMode::None)
                    .unwrap_err(),
                ObservationQueryError::NotFound
            );
        }
    }
}

#[test]
fn explicit_delete_rollup_matrix_preserves_false_and_erases_true_without_replacement() {
    for (case_name, data_class, delete_rollups) in [
        ("content-keep", DeletionDataClass::ContentOnly, false),
        ("content-erase", DeletionDataClass::ContentOnly, true),
        ("facts-keep", DeletionDataClass::FactsAndContent, false),
        ("facts-erase", DeletionDataClass::FactsAndContent, true),
    ] {
        let temporary = tempdir().unwrap();
        let fixture = Fixture::new(case_name);
        let store = open_store(temporary.path());
        install_value_request(
            &store,
            &fixture,
            &format!("receipt-{case_name}"),
            &format!("attempt-{case_name}"),
            finished(&format!("attempt-{case_name}")),
            100,
        );
        install_content(&store, &fixture, b"matrix-content", 200);
        assert_eq!(store.run_retention(SEVEN_DAYS_MILLIS + 10_000).unwrap(), 1);

        let before = store
            .get_value(
                &fixture.workspace,
                &value_query("plan/codex-daily", "USD", None),
            )
            .unwrap();
        assert_eq!(before.daily_aggregates.len(), 1, "{case_name}");
        assert_eq!(
            count_for_session(&store, "observation_tombstones", &fixture),
            1
        );
        assert_eq!(
            count_for_session(&store, "session_value_rollup_contributions", &fixture),
            1
        );

        let spec = SessionDeletionSpecV1 {
            workspace_id: fixture.workspace.clone(),
            session_id: fixture.session.clone(),
            data_class: data_class.clone(),
            delete_rollups,
        };
        let principal = ObservationPrincipalV1::local_user(fixture.workspace.clone());
        let preview = store.preview_session_deletion(&principal, &spec).unwrap();
        assert_eq!(preview.content_instances, 0, "{case_name}");
        assert_eq!(preview.rollup_contributions, 1, "{case_name}");
        assert_eq!(preview.tombstones, 1, "{case_name}");
        let outcome = store
            .apply_session_deletion(
                &principal,
                &spec,
                preview.store_revision,
                &preview.change_digest,
                SEVEN_DAYS_MILLIS + 20_000,
            )
            .unwrap();

        let after = store
            .get_value(
                &fixture.workspace,
                &value_query("plan/codex-daily", "USD", None),
            )
            .unwrap();
        if delete_rollups {
            assert!(after.daily_aggregates.is_empty(), "{case_name}");
            assert_eq!(outcome.tombstone_reason, None, "{case_name}");
            assert_eq!(
                count_for_session(&store, "session_value_rollup_contributions", &fixture),
                0,
                "{case_name}"
            );
            assert_eq!(
                count_for_session(&store, "observation_tombstones", &fixture),
                0,
                "{case_name}"
            );
        } else {
            assert_eq!(after.daily_aggregates.len(), 1, "{case_name}");
            assert_eq!(
                outcome.tombstone_reason,
                Some(TombstoneReason::UserDeleted),
                "{case_name}"
            );
            assert_eq!(
                count_for_session(&store, "session_value_rollup_contributions", &fixture),
                1,
                "{case_name}"
            );
            assert_eq!(
                count_for_session(&store, "observation_tombstones", &fixture),
                2,
                "{case_name}"
            );
        }
        let expected_session_rows =
            u64::from(!(delete_rollups && data_class == DeletionDataClass::FactsAndContent));
        assert_eq!(
            count_for_session(&store, "sessions", &fixture),
            expected_session_rows,
            "{case_name}"
        );
    }
}

#[test]
fn deleting_one_archived_session_recomputes_a_shared_plan_day_currency_bucket() {
    let temporary = tempdir().unwrap();
    let first = Fixture::new("shared-first");
    let second = Fixture::new("shared-second");
    let store = open_store(temporary.path());
    install_value_request(
        &store,
        &first,
        "receipt-shared-first",
        "attempt-shared-first",
        finished_with_value(
            "attempt-shared-first",
            "plan/shared",
            "USD",
            Some(1_200),
            Some(900),
            Some(300),
        ),
        100,
    );
    install_value_request(
        &store,
        &second,
        "receipt-shared-second",
        "attempt-shared-second",
        finished_with_value(
            "attempt-shared-second",
            "plan/shared",
            "USD",
            Some(2_000),
            Some(1_500),
            Some(500),
        ),
        200,
    );
    assert_eq!(store.run_retention(SEVEN_DAYS_MILLIS + 10_000).unwrap(), 2);
    let combined = store
        .get_value(&first.workspace, &value_query("plan/shared", "USD", None))
        .unwrap();
    assert_eq!(combined.daily_aggregates.len(), 1);
    assert_eq!(combined.baseline_api_equivalent_cost_micros, Some(3_200));
    assert_eq!(combined.estimated_total_savings_micros, Some(2_400));

    let spec = SessionDeletionSpecV1 {
        workspace_id: first.workspace.clone(),
        session_id: first.session.clone(),
        data_class: DeletionDataClass::FactsAndContent,
        delete_rollups: true,
    };
    let principal = ObservationPrincipalV1::local_user(first.workspace.clone());
    let preview = store.preview_session_deletion(&principal, &spec).unwrap();
    store
        .apply_session_deletion(
            &principal,
            &spec,
            preview.store_revision,
            &preview.change_digest,
            SEVEN_DAYS_MILLIS + 20_000,
        )
        .unwrap();
    let remaining = store
        .get_value(&second.workspace, &value_query("plan/shared", "USD", None))
        .unwrap();
    assert_eq!(remaining.daily_aggregates.len(), 1);
    assert_eq!(remaining.baseline_api_equivalent_cost_micros, Some(2_000));
    assert_eq!(remaining.chosen_api_equivalent_cost_micros, Some(1_500));
    assert_eq!(remaining.actual_incremental_cost_micros, Some(500));
    assert_eq!(remaining.estimated_total_savings_micros, Some(1_500));
    assert_eq!(
        count_for_session(&store, "observation_tombstones", &first),
        0
    );
    assert_eq!(
        count_for_session(&store, "observation_tombstones", &second),
        1
    );
}

fn count_for_session(store: &crate::LocalObservationStore, table: &str, fixture: &Fixture) -> u64 {
    let sql = match table {
        "sessions" => "SELECT COUNT(*) FROM sessions WHERE workspace_id=?1 AND session_id=?2",
        "observation_tombstones" => {
            "SELECT COUNT(*) FROM observation_tombstones WHERE workspace_id=?1 AND session_id=?2"
        }
        "session_value_rollup_contributions" => {
            "SELECT COUNT(*) FROM session_value_rollup_contributions
             WHERE workspace_id=?1 AND session_id=?2"
        }
        _ => unreachable!("bounded test table"),
    };
    let connection = store.connection.lock();
    let count: i64 = connection
        .query_row(
            sql,
            params![fixture.workspace.as_str(), fixture.session.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    count.try_into().unwrap()
}
