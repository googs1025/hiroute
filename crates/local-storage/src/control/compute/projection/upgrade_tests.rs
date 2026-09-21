use std::os::unix::fs::PermissionsExt;

use crate::test_tempdir as tempdir;
use hiroute_domain::{
    AuthenticationKind, CanonicalDigest, ComputeControlProjectionV1, ComputeInventorySnapshotV1,
    ComputeProjectionExpectationV1, ComputeSourceControlPort, ComputeSourceV1,
    ControlRepositoryPort, CredentialPoolV1, CredentialRefV1, EffectReconciliation, OperationId,
    PoolCredentialV1, PortErrorCode, SourceBindingV1, WorkspaceId,
};
use rusqlite::{Connection, params};

use super::tests::{prepared, projection_mutation, registry, v7_operation_json};
use super::*;
use crate::control::ControlStore;

#[test]
fn migrated_projection_matches_exact_v7_source_and_binding_cas_digests() {
    let (encoded, _, _, _, _) = v7_operation_json();
    let operation: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    let raw: ComputeControlProjectionV1 = serde_json::from_value(
        operation["plan"]["control"]["compute_source_mutation"]["desired_projection"].clone(),
    )
    .unwrap();
    let expected = ComputeProjectionExpectationV1 {
        source_revision: raw.source.revision,
        source_digest: Some(CanonicalDigest::of(&raw.source).unwrap()),
        binding_revision: raw.binding.revision,
        binding_digest: Some(CanonicalDigest::of(&raw.binding).unwrap()),
        inventory_revision: raw.inventory.inventory_revision,
        inventory_digest: Some(CanonicalDigest::of(&raw.inventory).unwrap()),
    };
    let normalized = normalize_projection(&raw).unwrap();
    let migrated = ProjectionEffectStateV1 {
        schema: EFFECT_STATE_SCHEMA.into(),
        source_id: normalized.source.source_id.clone(),
        binding_id: normalized.binding.binding_id.clone(),
        endpoint_profile_id: normalized.inventory.endpoint_profile_id.clone(),
        source: Some(normalized.source),
        binding: Some(normalized.binding),
        other_bindings: Vec::new(),
        credential_pools: Vec::new(),
        inventory: Some(normalized.inventory),
    };
    assert!(migrated.matches_expectation(&expected).unwrap());
}

#[test]
fn real_v7_staged_and_activated_effects_reconcile_after_v8_upgrade() {
    for activated in [false, true] {
        let directory = tempdir().unwrap();
        let root = directory
            .path()
            .join(if activated { "activated" } else { "staged" });
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let database = root.join("control.db");
        let connection = Connection::open(&database).unwrap();
        crate::migrations::initialize_control_v7_fixture(&connection).unwrap();
        let (encoded, operation_id, workspace, request_digest, accepted_digest) =
            v7_operation_json();
        let operation: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        seed_operation(&connection, &operation, &encoded);
        let control = operation["plan"]["control"].clone();
        let projection: ComputeControlProjectionV1 = serde_json::from_value(
            control["compute_source_mutation"]["desired_projection"].clone(),
        )
        .unwrap();
        let before = empty_state(&projection);
        let after = populated_state(&projection);
        let staged = serde_json::json!({
            "schema": "hiroute.control-desired/v1",
            "value": control,
        });
        let after_digest = CanonicalDigest::of(&control).unwrap();
        connection
            .execute(
                "INSERT INTO control_effects(
                    operation_id,workspace_id,before_exists,before_json,before_revision,
                    before_digest,before_owner_operation_id,after_revision,after_digest,
                    compensated,staged_json,activated,compute_source_id,
                    compute_source_expected_revision,compute_source_before_json,
                    compute_source_after_json
                 ) VALUES (?1,?2,0,NULL,0,NULL,NULL,1,?3,0,?4,?5,?6,0,?7,?8)",
                params![
                    operation_id.as_str(),
                    workspace.as_str(),
                    after_digest.as_str(),
                    serde_json::to_string(&staged).unwrap(),
                    activated,
                    projection.source.source_id,
                    serde_json::to_string(&before).unwrap(),
                    serde_json::to_string(&after).unwrap(),
                ],
            )
            .unwrap();
        if activated {
            materialize_v7_projection(&connection, &projection);
            connection
                .execute(
                    "INSERT INTO workspace_state(
                        workspace_id,desired_json,target_revision,desired_digest,
                        owner_operation_id,updated_at
                     ) VALUES (?1,?2,1,?3,?4,1)",
                    params![
                        workspace.as_str(),
                        serde_json::to_string(&staged).unwrap(),
                        after_digest.as_str(),
                        operation_id.as_str(),
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO workspace_revision_heads(workspace_id,revision)
                     VALUES (?1,1)",
                    params![workspace.as_str()],
                )
                .unwrap();
        }
        drop(connection);
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();

        let store = ControlStore::open(
            &crate::test_storage_authority(),
            &database,
            root.join("backups"),
        )
        .unwrap();
        let recovered = store.recoverable_operations().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].request_digest, request_digest);
        assert_eq!(recovered[0].accepted_digest, accepted_digest);
        let observed = store.observe_control(&operation_id, &workspace).unwrap();
        let effect = match (activated, observed) {
            (false, EffectReconciliation::Staged(effect)) => {
                let applied = store.activate_control(&effect).unwrap();
                assert_eq!(store.activate_control(&effect).unwrap(), applied);
                effect
            }
            (true, EffectReconciliation::Applied(effect)) => effect,
            (_, other) => panic!("unexpected upgraded effect state: {other:?}"),
        };
        assert!(matches!(
            store.observe_control(&operation_id, &workspace).unwrap(),
            EffectReconciliation::Applied(current) if current == effect
        ));
        let rows = store.compute_projection_rows().unwrap();
        assert_eq!(rows.len(), 1);
        rows[0].0.validate_shape().unwrap();
        assert_eq!(
            rows[0].0.identity_digest,
            projection.source.identity.digest().unwrap()
        );
        if activated {
            assert_eq!(
                store.compensate_control(&effect).unwrap(),
                hiroute_domain::CompensationOutcome::Compensated
            );
            assert!(matches!(
                store.observe_control(&operation_id, &workspace).unwrap(),
                EffectReconciliation::Missing
            ));
            assert!(store.compute_projection_rows().unwrap().is_empty());
        }
    }
}

fn seed_operation(connection: &Connection, operation: &serde_json::Value, encoded: &str) {
    connection
        .execute(
            "INSERT INTO operations(
                operation_id,workspace_id,principal,operation_kind,idempotency_key,
                request_digest,accepted_change_digest,state,generation,operation_json,
                created_at,updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,'accepted',0,?8,1,1)",
            params![
                operation["operation_id"].as_str().unwrap(),
                operation["workspace_id"].as_str().unwrap(),
                operation["idempotency"]["principal"].as_str().unwrap(),
                operation["idempotency"]["operation_kind"].as_str().unwrap(),
                operation["idempotency"]["key"].as_str().unwrap(),
                operation["request_digest"].as_str().unwrap(),
                operation["accepted_digest"].as_str().unwrap(),
                encoded,
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO writer_claim VALUES (1,?1,1)",
            params![operation["operation_id"].as_str().unwrap()],
        )
        .unwrap();
    for step in operation["steps"].as_array().unwrap() {
        connection
            .execute(
                "INSERT INTO operation_steps VALUES (?1,?2,?3,?4,?5)",
                params![
                    operation["operation_id"].as_str().unwrap(),
                    step["sequence"].as_u64().unwrap(),
                    step["kind"].as_str().unwrap(),
                    step["status"].as_str().unwrap(),
                    serde_json::to_string(&serde_json::json!({
                        "schema": "hiroute.operation-step/v1",
                        "step": step,
                    }))
                    .unwrap(),
                ],
            )
            .unwrap();
    }
}

fn empty_state(projection: &ComputeControlProjectionV1) -> serde_json::Value {
    serde_json::json!({
        "schema": EFFECT_STATE_SCHEMA,
        "source_id": projection.source.source_id,
        "binding_id": projection.binding.binding_id,
        "endpoint_profile_id": projection.inventory.endpoint_profile_id,
    })
}

fn populated_state(projection: &ComputeControlProjectionV1) -> serde_json::Value {
    serde_json::json!({
        "schema": EFFECT_STATE_SCHEMA,
        "source_id": projection.source.source_id,
        "binding_id": projection.binding.binding_id,
        "endpoint_profile_id": projection.inventory.endpoint_profile_id,
        "source": projection.source,
        "binding": projection.binding,
        "inventory": projection.inventory,
    })
}

fn materialize_v7_projection(connection: &Connection, projection: &ComputeControlProjectionV1) {
    connection
        .execute(
            "INSERT INTO compute_sources(
                source_id,revision,identity_digest,source_json,updated_at
             ) VALUES (?1,?2,?3,?4,1)",
            params![
                projection.source.source_id,
                projection.source.revision,
                projection.source.identity_digest.as_str(),
                serde_json::to_string(&projection.source).unwrap(),
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_bindings(
                binding_id,revision,source_id,binding_json,updated_at
             ) VALUES (?1,?2,?3,?4,1)",
            params![
                projection.binding.binding_id,
                projection.binding.revision,
                projection.binding.source_id,
                serde_json::to_string(&projection.binding).unwrap(),
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_inventory_snapshots(
                source_id,endpoint_profile_id,inventory_revision,inventory_digest,
                observed_models_json,captured_at
             ) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                projection.inventory.source_id,
                projection.inventory.endpoint_profile_id,
                projection.inventory.inventory_revision,
                projection.inventory.inventory_digest.as_str(),
                serde_json::to_string(&projection.inventory.observed_models).unwrap(),
                projection.inventory.captured_at,
            ],
        )
        .unwrap();
}

struct LegacyGraphFixture {
    prepared: hiroute_domain::PreparedComputeProjectionV1,
    legacy_source_id: String,
    legacy_pool_id: String,
    source: ComputeSourceV1,
    binding: SourceBindingV1,
    inventory: ComputeInventorySnapshotV1,
    pool: CredentialPoolV1,
}

fn legacy_graph(agent_id: &str, credential_id: &str) -> LegacyGraphFixture {
    let registry = registry();
    let mut prepared = prepared(&registry);
    prepared.desired.source.identity.account_subject_ref = format!("account/agent/{agent_id}");
    prepared.desired.source.identity_digest = prepared.desired.source.identity.digest().unwrap();
    let canonical_suffix = prepared
        .desired
        .source
        .identity_digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap()
        .get(..24)
        .unwrap();
    prepared.desired.source.source_id = format!("source/agent-{canonical_suffix}");
    prepared.desired.binding.binding_id = derived_binding_id(
        &prepared.desired.source.source_id,
        &prepared.desired.binding,
    )
    .unwrap();
    prepared.desired.binding.source_id = prepared.desired.source.source_id.clone();
    prepared.desired.binding.source_identity_digest =
        prepared.desired.source.identity_digest.clone();
    let canonical_pool_id = prepared
        .desired
        .binding
        .binding_id
        .replacen("binding/", "pool/", 1);
    prepared.desired.binding.credential_pool_id = Some(canonical_pool_id.clone());
    prepared.desired.inventory.source_id = prepared.desired.source.source_id.clone();
    let pool_identity = prepared.desired.credential_pool_identity.as_mut().unwrap();
    pool_identity.pool_id = canonical_pool_id;
    pool_identity.binding_id = prepared.desired.binding.binding_id.clone();
    pool_identity.binding_digest = CanonicalDigest::of(&prepared.desired.binding).unwrap();
    pool_identity.source_id = prepared.desired.source.source_id.clone();
    pool_identity.source_identity_digest = prepared.desired.source.identity_digest.clone();
    prepared.validate().unwrap();

    let legacy_source_id = legacy_v7_source_id(&prepared.desired).unwrap();
    let suffix = legacy_source_id.strip_prefix("source/agent-").unwrap();
    let legacy_binding_id = format!("binding/agent-{suffix}");
    let legacy_pool_id = format!("pool/agent-{suffix}");
    let mut source = prepared.desired.source.clone();
    source.source_id = legacy_source_id.clone();
    source.revision = 7;
    source.identity.identity_revision = 7;
    source.identity.account_subject_ref = format!("account/agent-{suffix}");
    source.identity_digest = source.identity.digest().unwrap();
    let mut binding = prepared.desired.binding.clone();
    binding.binding_id = legacy_binding_id.clone();
    binding.revision = 4;
    binding.source_id = legacy_source_id.clone();
    binding.source_revision = source.revision;
    binding.source_identity_digest = source.identity_digest.clone();
    binding.credential_pool_id = Some(legacy_pool_id.clone());
    let mut inventory = prepared.desired.inventory.clone();
    inventory.source_id = legacy_source_id.clone();
    inventory.inventory_revision = 5;
    let credential = CredentialRefV1::new(
        credential_id,
        format!("source/{legacy_source_id}"),
        "hirouted",
        "provider-auth",
        [format!("connection-option/{}", source.connection_option_id)],
        2,
    )
    .unwrap();
    let pool = CredentialPoolV1 {
        pool_id: legacy_pool_id.clone(),
        binding_id: legacy_binding_id.clone(),
        binding_revision: binding.revision,
        binding_digest: CanonicalDigest::of(&binding).unwrap(),
        source_id: legacy_source_id.clone(),
        source_revision: source.revision,
        connection_option_id: source.connection_option_id.clone(),
        source_identity_digest: source.identity_digest.clone(),
        offer_ref: binding.offer_ref.clone(),
        offer_revision: 1,
        offer_evidence_digest: binding.offer_evidence_digest.clone(),
        billing_class: binding.billing_class,
        model_configuration_id: binding.model_configuration_id.clone(),
        authentication: AuthenticationKind::ProviderApiKey,
        revision: 3,
        credentials: vec![PoolCredentialV1 {
            credential,
            fingerprint: CanonicalDigest::of_bytes(credential_id.as_bytes()),
            ordinal: 0,
            enabled: true,
        }],
    };
    pool.validate().unwrap();
    LegacyGraphFixture {
        prepared,
        legacy_source_id,
        legacy_pool_id,
        source,
        binding,
        inventory,
        pool,
    }
}

fn seed_legacy_graph(store: &ControlStore, graph: &LegacyGraphFixture) {
    store.with_connection(|connection| {
        connection
            .execute(
                "INSERT INTO compute_sources VALUES (?1,?2,?3,?4,1)",
                params![
                    graph.source.source_id,
                    graph.source.revision,
                    graph.source.identity_digest.as_str(),
                    serde_json::to_string(&graph.source).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_bindings VALUES (?1,?2,?3,?4,1,1)",
                params![
                    graph.binding.binding_id,
                    graph.binding.revision,
                    graph.binding.source_id,
                    serde_json::to_string(&graph.binding).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_inventory_snapshots VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    graph.inventory.source_id,
                    graph.inventory.endpoint_profile_id,
                    graph.inventory.inventory_revision,
                    graph.inventory.inventory_digest.as_str(),
                    serde_json::to_string(&graph.inventory.observed_models).unwrap(),
                    graph.inventory.captured_at,
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO credential_pools(
                    pool_id,binding_id,binding_revision,binding_digest,source_id,
                    source_revision,connection_option_id,offer_ref,offer_revision,
                    offer_evidence_digest,billing_class,model_configuration_id,revision,
                    homogeneous_identity_digest,authentication_kind,pool_json,active,updated_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'paid',?11,?12,?13,
                           'provider_api_key',?14,1,1)",
                params![
                    graph.pool.pool_id,
                    graph.pool.binding_id,
                    graph.pool.binding_revision,
                    graph.pool.binding_digest.as_str(),
                    graph.pool.source_id,
                    graph.pool.source_revision,
                    graph.pool.connection_option_id,
                    graph.pool.offer_ref,
                    graph.pool.offer_revision,
                    graph.pool.offer_evidence_digest.as_str(),
                    graph.pool.model_configuration_id,
                    graph.pool.revision,
                    CanonicalDigest::of(&graph.pool.identity())
                        .unwrap()
                        .as_str(),
                    serde_json::to_string(&graph.pool).unwrap(),
                ],
            )
            .unwrap();
    });
}

fn expectation(graph: &LegacyGraphFixture) -> ComputeProjectionExpectationV1 {
    ComputeProjectionExpectationV1 {
        source_revision: graph.source.revision,
        source_digest: Some(CanonicalDigest::of(&graph.source).unwrap()),
        binding_revision: graph.binding.revision,
        binding_digest: Some(CanonicalDigest::of(&graph.binding).unwrap()),
        inventory_revision: graph.inventory.inventory_revision,
        inventory_digest: Some(CanonicalDigest::of(&graph.inventory).unwrap()),
    }
}

fn advance(
    mut prepared: hiroute_domain::PreparedComputeProjectionV1,
    expected: ComputeProjectionExpectationV1,
) -> hiroute_domain::PreparedComputeProjectionV1 {
    prepared.expected = expected;
    prepared.desired.source.revision = prepared.expected.source_revision + 1;
    prepared.desired.source.identity.identity_revision = prepared.desired.source.revision;
    prepared.desired.binding.revision = prepared.expected.binding_revision + 1;
    prepared.desired.binding.source_revision = prepared.desired.source.revision;
    prepared.desired.inventory.inventory_revision = prepared.expected.inventory_revision + 1;
    let pool = prepared.desired.credential_pool_identity.as_mut().unwrap();
    pool.binding_revision = prepared.desired.binding.revision;
    pool.binding_digest = CanonicalDigest::of(&prepared.desired.binding).unwrap();
    pool.source_revision = prepared.desired.source.revision;
    prepared.validate().unwrap();
    prepared
}

#[test]
fn different_agent_lineage_cannot_alias_or_transfer_a_legacy_pool() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("different-agent");
    let store = ControlStore::open(
        &crate::test_storage_authority(),
        root.join("control.db"),
        root.join("backups"),
    )
    .unwrap();
    let owner = legacy_graph("agent.owner", "credential.owner");
    let foreign = legacy_graph("agent.foreign", "credential.foreign");
    seed_legacy_graph(&store, &owner);
    let source_id = &foreign.prepared.desired.source.source_id;
    let binding_id = &foreign.prepared.desired.binding.binding_id;
    assert_eq!(
        store
            .compute_projection_expectation_with_legacy_lineage(
                source_id,
                binding_id,
                &foreign.prepared.desired.inventory.endpoint_profile_id,
                &foreign.legacy_source_id,
            )
            .unwrap_err()
            .code,
        PortErrorCode::Conflict,
    );
    let desired = advance(foreign.prepared, expectation(&owner));
    let mutation = projection_mutation(&desired, None, &registry());
    let operation = OperationId::parse("op_51515151515151515151515151515151").unwrap();
    assert_eq!(
        store
            .apply_compute_source(&operation, &WorkspaceId::default(), 0, &mutation)
            .unwrap_err()
            .code,
        PortErrorCode::Conflict,
    );
    assert_eq!(
        store.credential_pool(&owner.legacy_pool_id).unwrap(),
        Some(owner.pool)
    );
    store.with_connection(|connection| {
        for table in [
            "compute_source_identity_aliases",
            "control_effects",
            "operations",
        ] {
            let count: u64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "failed foreign transition wrote {table}");
        }
    });
}

#[test]
fn exact_lineage_wins_locally_among_multiple_legacy_graphs_and_compensates() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("multiple-lineages");
    let database = root.join("control.db");
    let store = ControlStore::open(
        &crate::test_storage_authority(),
        &database,
        root.join("backups"),
    )
    .unwrap();
    let owner = legacy_graph("agent.owner", "credential.owner");
    let unrelated = legacy_graph("agent.unrelated", "credential.unrelated");
    seed_legacy_graph(&store, &owner);
    seed_legacy_graph(&store, &unrelated);
    let expected = store
        .compute_projection_expectation_with_legacy_lineage(
            &owner.prepared.desired.source.source_id,
            &owner.prepared.desired.binding.binding_id,
            &owner.prepared.desired.inventory.endpoint_profile_id,
            &owner.legacy_source_id,
        )
        .unwrap();
    assert_eq!(expected, expectation(&owner));
    assert_eq!(
        store
            .compute_projection_expectation_with_legacy_lineage(
                &unrelated.prepared.desired.source.source_id,
                &unrelated.prepared.desired.binding.binding_id,
                &unrelated.prepared.desired.inventory.endpoint_profile_id,
                &unrelated.legacy_source_id,
            )
            .unwrap(),
        expectation(&unrelated),
    );
    let desired = advance(owner.prepared.clone(), expected);
    let mutation = projection_mutation(&desired, None, &registry());
    let operation = OperationId::parse("op_52525252525252525252525252525252").unwrap();
    let effect = store
        .apply_compute_source(&operation, &WorkspaceId::default(), 0, &mutation)
        .unwrap();
    store.activate_control(&effect).unwrap();
    drop(store);

    let store = ControlStore::open(
        &crate::test_storage_authority(),
        &database,
        root.join("backups-reopened"),
    )
    .unwrap();
    assert_eq!(store.compute_projection_rows().unwrap().len(), 2);
    let continued = store
        .credential_pool(
            desired
                .desired
                .credential_pool_identity
                .as_ref()
                .unwrap()
                .pool_id
                .as_str(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(continued.credentials, owner.pool.credentials);
    assert_eq!(continued.source_id, owner.legacy_source_id);
    assert_eq!(
        store.credential_pool(&unrelated.legacy_pool_id).unwrap(),
        Some(unrelated.pool.clone()),
    );
    assert_eq!(
        store.compensate_control(&effect).unwrap(),
        hiroute_domain::CompensationOutcome::Compensated,
    );
    assert_eq!(
        store.credential_pool(&owner.legacy_pool_id).unwrap(),
        Some(owner.pool),
    );
    assert_eq!(
        store.credential_pool(&unrelated.legacy_pool_id).unwrap(),
        Some(unrelated.pool),
    );
}
