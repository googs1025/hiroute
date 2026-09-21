use std::collections::BTreeSet;

use crate::test_tempdir as tempdir;
use hiroute_domain::{
    AuthenticationKind, BillingClass, CapabilityFactsV1, ChangeSpecV1, CompensationOutcome,
    ComputeSourceControlPort, ComputeSourceV1, ConnectionOptionV1, ConnectionOrigin,
    ConnectorDescriptorV1, ConnectorRegistryBundleV1, ConnectorRuntimeKind, ControlRepositoryPort,
    CredentialPoolControlPort, CredentialPoolIdentityV1, CredentialPoolMutationKind,
    CredentialPoolMutationV1, CredentialRefV1, EndpointProfileV1, InventoryStrategyKind,
    MaterializationState, ModelDataBundleV1, ModelDefinitionV1, ObservedModelV1, OperationId,
    ProtocolEndpointV1, SourceBindingV1, SourceIdentityV1, SourceOrigin, TransactionPlanV1,
    UpstreamProtocol, WorkspaceId,
};

use super::*;

fn registry() -> ConnectorRegistryBundleV1 {
    ConnectorRegistryBundleV1 {
        schema: hiroute_domain::CONNECTOR_REGISTRY_SCHEMA_V1.into(),
        registry_version: "registry-v2".into(),
        product_release: "release-v2".into(),
        connectors: vec![ConnectorDescriptorV1 {
            connector_id: "connector.one".into(),
            revision: 1,
            runtime_kind: ConnectorRuntimeKind::BuiltinNative,
            implementation_ref: "builtin/one".into(),
            implementation_revision: 1,
            accepted_origins: BTreeSet::from([ConnectionOrigin::NativeApi]),
            authentication: AuthenticationKind::ProviderApiKey,
            required_secret_slots: vec!["provider_api_key".into()],
            endpoint_profile_refs: vec!["endpoint.one".into()],
            catalog_adapter_ref: "catalog.one".into(),
            catalog_adapter_revision: 1,
            error_classifier_ref: "errors.one".into(),
            error_classifier_revision: 1,
            usage_decoder_ref: "usage.one".into(),
            usage_decoder_revision: 1,
            cache_policy_ref: "cache.one".into(),
            cache_policy_revision: 1,
        }],
        endpoint_profiles: vec![EndpointProfileV1 {
            endpoint_profile_id: "endpoint.one".into(),
            revision: 1,
            connector_id: "connector.one".into(),
            connector_revision: 1,
            provider_platform_id: "provider.one".into(),
            service_offering_id: "payg".into(),
            entitlement_id: "payg".into(),
            usage_scope: "account".into(),
            region_id: "test".into(),
            logical_endpoint_group: "one".into(),
            protocol_endpoints: vec![ProtocolEndpointV1 {
                protocol_endpoint_id: "endpoint.one.chat".into(),
                protocol: UpstreamProtocol::ChatCompletions,
                base_url: "https://one.release-fixture.invalid".into(),
                request_path: "/v1/chat/completions".into(),
                adapter_ref: "adapter.one".into(),
                adapter_revision: 1,
                stable_preference: 0,
                inventory_path: Some("/v1/models".into()),
                authentication_semantics: None,
                required_headers: Vec::new(),
            }],
            inventory_strategy: InventoryStrategyKind::RemoteModels,
            inventory_protocol_endpoint_id: Some("endpoint.one.chat".into()),
            verification_evidence: CanonicalDigest::of_bytes(b"endpoint-one").to_string(),
            last_verified_at: 1,
        }],
        connection_options: vec![ConnectionOptionV1 {
            connection_option_id: "provider.payg.test.v1".into(),
            display_name: "Provider One".into(),
            origin: ConnectionOrigin::NativeApi,
            connector_id: "connector.one".into(),
            connector_revision: 1,
            endpoint_profile_id: "endpoint.one".into(),
            endpoint_profile_revision: 1,
            billing_class: BillingClass::Paid,
            free_offer_ref: None,
            direct_verification_evidence: None,
        }],
    }
}

fn model_data() -> ModelDataBundleV1 {
    serde_json::from_value(serde_json::json!({
        "schema": hiroute_domain::MODEL_DATA_SCHEMA_V1,
        "bundle_version": "models-v2",
        "product_release": "release-v2",
        "connector_registry_version": "registry-v2",
        "models_slice_version": "models-v2",
        "capability_slice_version": "cap-v2",
        "ratings_slice_version": "rating-v2",
        "prices_slice_version": "price-v2",
        "free_offers_slice_version": "free-v2",
        "models": [ModelDefinitionV1 {
            model_configuration_id: "model.one".into(),
            revision: 1,
            display_name: "One".into(),
            publisher_id: "publisher.one".into(),
            capabilities: CapabilityFactsV1 {
                tool: true,
                vision: false,
                streaming: true,
                context_tokens: 100,
                max_output_tokens: 10,
            },
        }],
        "model_endpoint_capabilities": [{
            "capability_id": "cap.model.one",
            "revision": 1,
            "model_configuration_id": "model.one",
            "connector_id": "connector.one",
            "connector_revision": 1,
            "endpoint_profile_id": "endpoint.one",
            "endpoint_profile_revision": 1,
            "protocol_endpoint_id": "endpoint.one.chat",
            "upstream_protocol": "chat_completions",
            "upstream_model_id": "upstream-one",
            "required_adapter_ref": "adapter.one",
            "required_adapter_revision": 1,
            "evidence_digest": CanonicalDigest::of_bytes(b"cap-one"),
        }],
        "ratings": [{
            "model_configuration_id": "model.one",
            "overall_score_tenths": 40,
            "rating_count": 1,
        }],
        "offers": [{
            "offer_id": "offer.one",
            "revision": 1,
            "endpoint_profile_id": "endpoint.one",
            "endpoint_profile_revision": 1,
            "service_offering_id": "payg",
            "entitlement_id": "payg",
            "usage_scope": "account",
            "region_id": "test",
            "model_configuration_ids": ["model.one"],
            "billing_class": "paid",
            "evidence_digest": CanonicalDigest::of_bytes(b"offer-one"),
        }],
        "free_offers": [],
        "price_rates": [{
            "price_rate_id": "rate.model.one",
            "revision": 1,
            "offer_ref": "offer.one",
            "model_configuration_id": "model.one",
            "currency": "USD",
            "input_micros_per_million": 100,
            "output_micros_per_million": 200,
        }],
    }))
    .unwrap()
}

fn compute_source(revision: u64) -> ComputeSourceV1 {
    let identity = SourceIdentityV1 {
        identity_revision: revision,
        provider_platform_id: "provider.one".into(),
        service_offering_id: "payg".into(),
        entitlement_id: "payg".into(),
        usage_scope: "account".into(),
        endpoint_profile_id: "endpoint.one".into(),
        endpoint_profile_revision: 1,
        region_id: "test".into(),
        account_subject_ref: "account.one".into(),
        evidence_refs: vec![CanonicalDigest::of_bytes(
            format!("account-evidence-{revision}").as_bytes(),
        )],
    };
    ComputeSourceV1 {
        schema: hiroute_domain::COMPUTE_STATE_SCHEMA_V1.into(),
        source_id: "source.one".into(),
        revision,
        connection_option_id: "provider.payg.test.v1".into(),
        connector_id: "connector.one".into(),
        connector_revision: 1,
        origin: SourceOrigin::NativeApi,
        identity_digest: identity.digest().unwrap(),
        identity,
        billing_class: BillingClass::Paid,
        state: MaterializationState::NeedsCredential,
    }
}

#[test]
fn compute_source_effect_is_atomic_with_workspace_and_exactly_compensated() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("data");
    let store = ControlStore::open(
        &crate::test_storage_authority(),
        root.join("control.db"),
        root.join("backups"),
    )
    .unwrap();
    let registry = registry();
    let source = compute_source(1);
    let spec = ChangeSpecV1 {
        schema_version: hiroute_domain::CHANGE_SPEC_SCHEMA_V1,
        command_id: "compute.connection.apply".into(),
        resource_id: Some("personal/default".into()),
        desired_state: json!({
            "connection_option_id": "provider.payg.test.v1",
            "source_id": "source.one",
            "explicit_materialization": true,
            "expected_source_revision": 0,
        }),
    };
    let plan =
        TransactionPlanV1::from_compute_source_planner(spec, None, source.clone(), &registry, true)
            .unwrap();
    let mutation = plan.compute_source().unwrap();
    let operation = OperationId::parse("op_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let workspace = WorkspaceId::default();
    let effect = store
        .apply_compute_source(&operation, &workspace, 0, &mutation)
        .unwrap();
    assert_eq!(store.compute_source("source.one").unwrap(), None);
    assert!(store.desired_state(&workspace).unwrap().is_none());
    assert!(matches!(
        store.observe_control(&operation, &workspace).unwrap(),
        EffectReconciliation::Staged(_)
    ));

    store.activate_control(&effect).unwrap();
    assert_eq!(store.compute_source("source.one").unwrap(), Some(source));
    assert_eq!(store.current_revisions(&workspace).unwrap().target, 1);
    assert!(matches!(
        store.observe_control(&operation, &workspace).unwrap(),
        EffectReconciliation::Applied(_)
    ));

    assert_eq!(
        store.compensate_control(&effect).unwrap(),
        CompensationOutcome::Compensated
    );
    assert_eq!(store.compute_source("source.one").unwrap(), None);
    assert_eq!(store.current_revisions(&workspace).unwrap().target, 2);
    assert!(store.desired_state(&workspace).unwrap().is_none());
}

#[test]
fn compute_source_cas_fails_closed() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("data");
    let store = ControlStore::open(
        &crate::test_storage_authority(),
        root.join("control.db"),
        root.join("backups"),
    )
    .unwrap();
    let identity = SourceIdentityV1 {
        identity_revision: 1,
        provider_platform_id: "provider.one".into(),
        service_offering_id: "payg".into(),
        entitlement_id: "payg".into(),
        usage_scope: "account".into(),
        endpoint_profile_id: "endpoint.one".into(),
        endpoint_profile_revision: 1,
        region_id: "test".into(),
        account_subject_ref: "account.one".into(),
        evidence_refs: vec![CanonicalDigest::of_bytes(b"account-evidence")],
    };
    let source = ComputeSourceV1 {
        schema: hiroute_domain::COMPUTE_STATE_SCHEMA_V1.into(),
        source_id: "source.one".into(),
        revision: 1,
        connection_option_id: "provider.payg.test.v1".into(),
        connector_id: "connector.one".into(),
        connector_revision: 1,
        origin: SourceOrigin::NativeApi,
        identity_digest: identity.digest().unwrap(),
        identity,
        billing_class: BillingClass::Paid,
        state: MaterializationState::NeedsCredential,
    };
    let registry = registry();
    registry.validate().unwrap();
    store
        .put_compute_source(0, &source, &registry, true)
        .unwrap();
    assert_eq!(
        store.compute_source("source.one").unwrap(),
        Some(source.clone())
    );
    assert!(
        store
            .put_compute_source(0, &source, &registry, true)
            .is_err()
    );

    let observed_models = vec![ObservedModelV1 {
        upstream_model_id: "observed-only".into(),
        metadata: Default::default(),
    }];
    let snapshot = InventorySnapshotV1 {
        source_id: source.source_id.clone(),
        endpoint_profile_id: source.identity.endpoint_profile_id.clone(),
        inventory_revision: 1,
        inventory_digest: CanonicalDigest::of(&observed_models).unwrap(),
        observed_models,
        captured_at: 1_000,
    };
    store.put_inventory_snapshot(&snapshot).unwrap();
    assert_eq!(
        store
            .inventory_snapshot(&snapshot.source_id, &snapshot.endpoint_profile_id)
            .unwrap(),
        Some(snapshot.clone())
    );
    assert!(store.put_inventory_snapshot(&snapshot).is_err());
}

#[test]
fn embedded_catalog_dependency_revisions_survive_restart_without_catalog_history() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("data");
    let database = root.join("control.db");
    let backups = root.join("backups");
    let workspace = WorkspaceId::default();
    let store = ControlStore::open(&crate::test_storage_authority(), &database, &backups).unwrap();
    store
        .activate_embedded_release_catalog(&workspace, 2)
        .unwrap();
    drop(store);

    let store = ControlStore::open(&crate::test_storage_authority(), &database, &backups).unwrap();
    let revisions = store.current_revisions(&workspace).unwrap();
    assert_eq!(revisions.dependencies.get("release.registry"), Some(&2));
    assert_eq!(revisions.dependencies.get("release.model_data"), Some(&2));

    assert!(
        store
            .activate_embedded_release_catalog(&workspace, 1)
            .is_err()
    );
    store
        .activate_embedded_release_catalog(&workspace, 3)
        .unwrap();
    assert!(
        store
            .activate_embedded_release_catalog(&workspace, 2)
            .is_err(),
        "a lower catalog sequence must not become the publication dependency"
    );
    let revisions = store.current_revisions(&workspace).unwrap();
    assert_eq!(revisions.dependencies.get("release.registry"), Some(&3));
    assert_eq!(revisions.dependencies.get("release.model_data"), Some(&3));
    store.with_connection(|connection| {
        let history_tables: u64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='table' AND name='release_fact_installations'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history_tables, 0);
    });
}

#[test]
fn credential_pool_effect_is_staged_then_activated_and_compensated_under_exact_cas() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("data");
    let store = ControlStore::open(
        &crate::test_storage_authority(),
        root.join("control.db"),
        root.join("backups"),
    )
    .unwrap();
    let registry = registry();
    let model_data = model_data();
    model_data.validate_against(&registry).unwrap();
    let identity = SourceIdentityV1 {
        identity_revision: 1,
        provider_platform_id: "provider.one".into(),
        service_offering_id: "payg".into(),
        entitlement_id: "payg".into(),
        usage_scope: "account".into(),
        endpoint_profile_id: "endpoint.one".into(),
        endpoint_profile_revision: 1,
        region_id: "test".into(),
        account_subject_ref: "account.one".into(),
        evidence_refs: vec![CanonicalDigest::of_bytes(b"account-evidence")],
    };
    let source = ComputeSourceV1 {
        schema: hiroute_domain::COMPUTE_STATE_SCHEMA_V1.into(),
        source_id: "source.one".into(),
        revision: 1,
        connection_option_id: "provider.payg.test.v1".into(),
        connector_id: "connector.one".into(),
        connector_revision: 1,
        origin: SourceOrigin::NativeApi,
        identity_digest: identity.digest().unwrap(),
        identity,
        billing_class: BillingClass::Paid,
        state: MaterializationState::NeedsCredential,
    };
    store
        .put_compute_source(0, &source, &registry, true)
        .unwrap();
    let credential = |id: &str, generation| {
        CredentialRefV1::new(
            id,
            "source/source.one",
            "hirouted",
            "provider-auth",
            ["connection-option/provider.payg.test.v1".to_owned()],
            generation,
        )
        .unwrap()
    };
    let binding = SourceBindingV1 {
        binding_id: "binding.one".into(),
        revision: 1,
        source_id: source.source_id.clone(),
        source_revision: source.revision,
        source_identity_digest: source.identity_digest.clone(),
        model_data_bundle_version: model_data.bundle_version.clone(),
        capability_slice_version: model_data.capability_slice_version.clone(),
        offer_ref: "offer.one".into(),
        offer_evidence_digest: CanonicalDigest::of_bytes(b"offer-one"),
        billing_class: BillingClass::Paid,
        model_configuration_id: "model.one".into(),
        upstream_model_id: "upstream-one".into(),
        capability_id: "cap.model.one".into(),
        credential_pool_id: Some("pool.one".into()),
    };
    store
        .put_source_binding(0, &binding, &registry, &model_data)
        .unwrap();
    assert!(store.credential_pool("pool.one").unwrap().is_none());
    let pool_identity =
        CredentialPoolIdentityV1::for_registered_binding(&binding, &source, &registry, &model_data)
            .unwrap();
    let pool = pool_identity
        .materialize_first(
            credential("credential.one", 1),
            CanonicalDigest::of_bytes(b"key-one"),
        )
        .unwrap();
    let first_mutation = CredentialPoolMutationV1::from_registered_planner(
        CredentialPoolMutationKind::Add,
        None,
        pool.clone(),
    )
    .unwrap();
    let dangling_directory = tempdir().unwrap();
    let dangling_root = dangling_directory.path().join("data");
    let dangling_store = ControlStore::open(
        &crate::test_storage_authority(),
        dangling_root.join("control.db"),
        dangling_root.join("backups"),
    )
    .unwrap();
    let dangling_operation = OperationId::parse("op_dddddddddddddddddddddddddddddddd").unwrap();
    assert!(
        dangling_store
            .apply_credential_pool(
                &dangling_operation,
                &WorkspaceId::default(),
                0,
                &first_mutation,
            )
            .is_err(),
        "storage admission must reject a pool whose persisted Source/Binding is missing"
    );
    let mut mismatched_pool = pool.clone();
    mismatched_pool.source_revision += 1;
    let mismatched_mutation = CredentialPoolMutationV1::from_registered_planner(
        CredentialPoolMutationKind::Add,
        None,
        mismatched_pool,
    )
    .unwrap();
    let mismatched_operation = OperationId::parse("op_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").unwrap();
    assert!(
        store
            .apply_credential_pool(
                &mismatched_operation,
                &WorkspaceId::default(),
                0,
                &mismatched_mutation,
            )
            .is_err(),
        "storage admission must reject mismatched persisted Source revisions"
    );
    store.with_connection(|connection| {
        connection
            .execute(
                "UPDATE source_bindings SET active=0 WHERE binding_id=?1",
                [&binding.binding_id],
            )
            .unwrap();
    });
    let stale_operation = OperationId::parse("op_abababababababababababababababab").unwrap();
    assert!(
        store
            .apply_credential_pool(
                &stale_operation,
                &WorkspaceId::default(),
                0,
                &first_mutation,
            )
            .is_err(),
        "storage admission must reject an inactive durable Binding"
    );
    store.with_connection(|connection| {
        connection
            .execute(
                "UPDATE source_bindings SET active=1 WHERE binding_id=?1",
                [&binding.binding_id],
            )
            .unwrap();
    });
    let first_operation = OperationId::parse("op_11111111111111111111111111111111").unwrap();
    let workspace = WorkspaceId::default();
    let first_effect = store
        .apply_credential_pool(&first_operation, &workspace, 0, &first_mutation)
        .unwrap();
    assert!(store.credential_pool("pool.one").unwrap().is_none());
    store.activate_control(&first_effect).unwrap();
    assert_eq!(
        store.credential_pool("pool.one").unwrap(),
        Some(pool.clone())
    );
    assert!(
        store
            .put_credential_pool(0, &pool, &registry, &model_data)
            .is_err()
    );
    let desired = pool
        .add(
            1,
            credential("credential.two", 1),
            CanonicalDigest::of_bytes(b"key-two"),
        )
        .unwrap();
    let mutation = CredentialPoolMutationV1::from_registered_planner(
        CredentialPoolMutationKind::Add,
        Some(&pool),
        desired.clone(),
    )
    .unwrap();
    let operation = OperationId::parse("op_0123456789abcdef0123456789abcdef").unwrap();
    let effect = store
        .apply_credential_pool(&operation, &workspace, 1, &mutation)
        .unwrap();
    assert_eq!(
        store.credential_pool("pool.one").unwrap(),
        Some(pool.clone())
    );
    store.activate_control(&effect).unwrap();
    assert_eq!(
        store.credential_pool("pool.one").unwrap(),
        Some(desired.clone())
    );

    let stale_operation = OperationId::parse("op_ffffffffffffffffffffffffffffffff").unwrap();
    assert!(
        store
            .apply_credential_pool(&stale_operation, &workspace, 2, &mutation)
            .is_err()
    );
    assert_eq!(
        store.compensate_control(&effect).unwrap(),
        CompensationOutcome::Compensated
    );
    assert_eq!(store.credential_pool("pool.one").unwrap(), Some(pool));
}
