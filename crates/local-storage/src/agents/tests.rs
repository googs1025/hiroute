use std::collections::BTreeMap;

use hiroute_domain::{AgentConfigChangeV1, AgentConfigDocumentV1};
use serde_json::json;

use super::*;

fn current() -> AgentConfigDocumentV1 {
    AgentConfigDocumentV1 {
        fields: BTreeMap::from([
            ("model_provider".to_owned(), json!("user-provider")),
            ("user.theme".to_owned(), json!("dark")),
        ]),
    }
}

#[test]
fn agents_apply_and_restore_touch_only_owned_fields() {
    let before = current();
    let change = AgentConfigChangeV1::preview(
        &before,
        BTreeMap::from([
            ("model_provider".to_owned(), Some(json!("hiroute"))),
            ("model_catalog_json".to_owned(), Some(json!("{}"))),
        ]),
    )
    .unwrap();
    let applied = apply_agent_config_change("codex-responses-v1", &before, &change).unwrap();
    assert_eq!(applied.document.fields["user.theme"], "dark");
    assert_eq!(applied.document.fields["model_provider"], "hiroute");

    let mut concurrent = applied.document;
    concurrent
        .fields
        .insert("user.theme".to_owned(), json!("light"));
    concurrent
        .fields
        .insert("user.other".to_owned(), json!(true));
    let RestoreAgentConfigOutcomeV1::Restored(restored) =
        restore_agent_config(&concurrent, &applied.restore_point).unwrap()
    else {
        panic!("known restore schema must execute");
    };
    assert_eq!(restored.fields["model_provider"], "user-provider");
    assert!(!restored.fields.contains_key("model_catalog_json"));
    assert_eq!(restored.fields["user.theme"], "light");
    assert_eq!(restored.fields["user.other"], true);
}

#[test]
fn agents_restore_refuses_concurrent_owned_field_without_partial_write() {
    let before = current();
    let change = AgentConfigChangeV1::preview(
        &before,
        BTreeMap::from([
            ("model_provider".to_owned(), Some(json!("hiroute"))),
            ("model".to_owned(), Some(json!("hiroute/0011223344556677"))),
        ]),
    )
    .unwrap();
    let applied = apply_agent_config_change("codex-responses-v1", &before, &change).unwrap();
    let mut concurrent = applied.document.clone();
    concurrent
        .fields
        .insert("model".to_owned(), json!("user-concurrent"));
    assert_eq!(
        restore_agent_config(&concurrent, &applied.restore_point).unwrap_err(),
        AgentConfigMutationError::OwnedFieldConflict {
            path: "model".to_owned()
        }
    );
    assert_eq!(concurrent.fields["model_provider"], "hiroute");
}

#[test]
fn agents_apply_refuses_stale_owned_field_but_allows_unrelated_concurrency() {
    let before = current();
    let change = AgentConfigChangeV1::preview(
        &before,
        BTreeMap::from([("model_provider".to_owned(), Some(json!("hiroute")))]),
    )
    .unwrap();
    let mut unrelated = before.clone();
    unrelated
        .fields
        .insert("user.theme".to_owned(), json!("light"));
    let applied = apply_agent_config_change("codex-responses-v1", &unrelated, &change).unwrap();
    assert_eq!(applied.document.fields["user.theme"], "light");

    let mut stale = before;
    stale
        .fields
        .insert("model_provider".to_owned(), json!("concurrent"));
    assert!(matches!(
        apply_agent_config_change("codex-responses-v1", &stale, &change),
        Err(AgentConfigMutationError::OwnedFieldConflict { .. })
    ));
}

#[test]
fn agents_restore_point_round_trips_canonically_after_restart() {
    let before = current();
    let change = AgentConfigChangeV1::preview(
        &before,
        BTreeMap::from([("model_provider".to_owned(), Some(json!("hiroute")))]),
    )
    .unwrap();
    let applied = apply_agent_config_change("codex-responses-v1", &before, &change).unwrap();
    let bytes = encode_agent_config_restore_point(&applied.restore_point).unwrap();
    assert_eq!(
        decode_agent_config_restore_point(&bytes).unwrap(),
        applied.restore_point
    );
}

#[test]
fn agents_unknown_restore_schema_is_report_only() {
    let before = current();
    let change = AgentConfigChangeV1::preview(
        &before,
        BTreeMap::from([("model_provider".to_owned(), Some(json!("hiroute")))]),
    )
    .unwrap();
    let mut point = apply_agent_config_change("codex-responses-v1", &before, &change)
        .unwrap()
        .restore_point;
    point.schema = "hiroute.agent-config-restore/v2".to_owned();
    assert_eq!(
        restore_agent_config(&before, &point).unwrap(),
        RestoreAgentConfigOutcomeV1::ReportOnlyUnknownVersion
    );
}
