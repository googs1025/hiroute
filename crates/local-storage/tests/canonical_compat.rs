//! The frozen records were emitted by the pre-fix default (BTreeMap) dependency graph.
//! Never regenerate them with the implementation under test.
use hiroute_domain::*;
use hiroute_local_storage::LocalStorageSet;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
#[path = "canonical_compat/support.rs"]
mod support;

fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/canonical_compat/default-records.v1.json")
}

/// Test-only decoder for the exact scope bytes captured before the executable model grant
/// replaced the alias-only scope. Production does not accept or upgrade this incomplete shape:
/// it has no Plan identities, revisions, or route digests from which a current grant could be
/// reconstructed without inventing authority.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyAgentAccessGrantScopeV1 {
    connection_id: String,
    protocol: AgentIngressProtocolV1,
    plan_grant_digest: CanonicalDigest,
    allowed_aliases: BTreeSet<ModelAlias>,
}

impl LegacyAgentAccessGrantScopeV1 {
    fn digest(&self) -> CanonicalDigest {
        CanonicalDigest::of(self).unwrap()
    }
}

#[test]
#[ignore = "one-time capture on the pre-fix default graph; never refresh after changing the digest"]
fn capture_pre_fix_canonical_records() {
    let root = private_tempdir();
    let stores = LocalStorageSet::open_for_daemon_startup(root.path()).unwrap();
    let operation = support::begin(&stores, "canonical-legacy");
    let material = AgentCollaborationCredential::from_csprng_entropy([31; 32]);
    let grant = AgentCollaborationGrant::issue(
        WorkspaceId::default(),
        "legacy-owner".into(),
        "collaboration-grant/legacy".into(),
        1,
        BTreeSet::from([AgentPlanId::parse("legacy-plan").unwrap()]),
        &material,
    )
    .unwrap();
    stores
        .control()
        .store_collaboration_grant(&operation.operation_id, 0, &grant)
        .unwrap();
    let loaded = stores
        .control()
        .load_operation(&operation.operation_id)
        .unwrap()
        .unwrap();
    let loaded_grant = stores
        .control()
        .collaboration_grant(&WorkspaceId::default(), &grant.grant_id)
        .unwrap()
        .unwrap();
    let connection = rusqlite::Connection::open(root.path().join("live/control.db")).unwrap();
    let operation_json: String = connection
        .query_row(
            "SELECT operation_json FROM operations WHERE operation_id = ?1",
            [operation.operation_id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    let scope = LegacyAgentAccessGrantScopeV1 {
        connection_id: "agent-connection/legacy".into(),
        protocol: AgentIngressProtocolV1::Responses,
        plan_grant_digest: CanonicalDigest::of_bytes(b"legacy-plan-grant"),
        allowed_aliases: BTreeSet::from([ModelAlias::parse("hiroute/0123456789abcdef").unwrap()]),
    };
    let value = json!({
        "schema": "hiroute.pre-fix-default-canonical-records/v1",
        "operation": loaded,
        "operation_json": operation_json,
        "model_grant_scope": scope,
        "model_grant_scope_digest": scope.digest(),
        "collaboration_grant": loaded_grant,
        "expected_revisions_digest": CanonicalDigest::of(&operation.expected_revisions).unwrap(),
        "transaction_plan_digest": CanonicalDigest::of(&operation.plan).unwrap(),
    });
    std::fs::write(
        fixture_path(),
        format!("{}\n", serde_json::to_string_pretty(&value).unwrap()),
    )
    .unwrap();
}

#[test]
fn canonical_legacy_default_operation_and_grant_survive_storage_reopen() {
    let value: Value = serde_json::from_slice(&std::fs::read(fixture_path()).unwrap()).unwrap();
    let grant: AgentCollaborationGrant =
        serde_json::from_value(value["collaboration_grant"].clone()).unwrap();
    let scope: LegacyAgentAccessGrantScopeV1 =
        serde_json::from_value(value["model_grant_scope"].clone()).unwrap();
    assert_eq!(
        scope.digest().as_str(),
        value["model_grant_scope_digest"].as_str().unwrap()
    );
    let root = private_tempdir();
    let operation_id;
    {
        let stores = LocalStorageSet::open_for_daemon_startup(root.path()).unwrap();
        let operation = support::begin(&stores, "canonical-legacy");
        operation_id = operation.operation_id.clone();
        assert_eq!(
            serde_json::to_value(&operation).unwrap(),
            value["operation"]
        );
        assert_eq!(
            CanonicalDigest::of(&operation.expected_revisions)
                .unwrap()
                .as_str(),
            value["expected_revisions_digest"].as_str().unwrap()
        );
        assert_eq!(
            CanonicalDigest::of(&operation.plan).unwrap().as_str(),
            value["transaction_plan_digest"].as_str().unwrap()
        );
        // Replay the byte-for-byte old persisted journal into the identical row. This is a
        // fixture import only; production recovery must use its existing strict decoder.
        let connection = rusqlite::Connection::open(root.path().join("live/control.db")).unwrap();
        assert_eq!(
            connection
                .execute(
                    "UPDATE operations SET operation_json = ?1 WHERE operation_id = ?2",
                    rusqlite::params![
                        value["operation_json"].as_str().unwrap(),
                        operation_id.as_str()
                    ]
                )
                .unwrap(),
            1
        );
        stores
            .control()
            .store_collaboration_grant(&operation_id, 0, &grant)
            .unwrap();
    }
    let stores = LocalStorageSet::open_for_daemon_startup(root.path()).unwrap();
    let recovered = stores
        .control()
        .load_operation(&operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&recovered).unwrap(),
        value["operation"]
    );
    assert!(
        stores
            .control()
            .recoverable_operations()
            .unwrap()
            .iter()
            .any(|op| op.operation_id == operation_id)
    );
    let current = stores
        .control()
        .collaboration_grant(&grant.workspace_id, &grant.grant_id)
        .unwrap()
        .unwrap();
    current
        .verify_bootstrap(
            "legacy-owner",
            1,
            &AgentCollaborationCredential::from_csprng_entropy([31; 32]),
        )
        .unwrap();
    assert!(
        current
            .verify_bootstrap(
                "legacy-owner",
                1,
                &AgentCollaborationCredential::from_csprng_entropy([32; 32])
            )
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(current).unwrap(),
        value["collaboration_grant"]
    );
}
