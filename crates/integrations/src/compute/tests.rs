use hiroute_domain::{
    AuthenticationKind, BillingClass, ComputeSourceV1, CredentialPoolV1, CredentialRefV1,
    MaterializationState, OperationId, PoolCredentialV1, SourceIdentityV1, SourceOrigin,
};

use super::*;

const MANIFEST: &[u8] =
    include_bytes!("../../../../assets/release-facts/current/bundle/manifest.json");
const REGISTRY: &[u8] =
    include_bytes!("../../../../assets/release-facts/current/bundle/connector-registry.json");
const MODEL_DATA: &[u8] =
    include_bytes!("../../../../assets/release-facts/current/bundle/model-data.json");

fn current_catalog() -> TrustedReleaseCatalog {
    TrustedReleaseCatalog::load_bundled_release_facts(MANIFEST, MANIFEST, REGISTRY, MODEL_DATA)
        .unwrap()
}

#[test]
fn current_catalog_resolves_exact_destination_inventory_and_price_scope() {
    let catalog = current_catalog();
    let resolved = catalog
        .resolve_connection_option("deepseek.official.global.v1")
        .unwrap();
    assert_eq!(resolved.endpoint_profile.provider_platform_id, "deepseek");
    let inventory_endpoint = resolved
        .endpoint_profile
        .protocol_endpoints
        .iter()
        .find(|endpoint| {
            resolved
                .endpoint_profile
                .inventory_protocol_endpoint_id
                .as_deref()
                == Some(endpoint.protocol_endpoint_id.as_str())
        })
        .unwrap();
    assert!(inventory_endpoint.authorize_exact_destination("https://api.deepseek.com/responses"));
    assert_eq!(
        inventory_endpoint.inventory_destination().as_deref(),
        Some("https://api.deepseek.com/models")
    );
    assert_ne!(
        inventory_endpoint.inventory_destination().as_deref(),
        Some("https://api.deepseek.com.evil.test/models")
    );
    let inventory = catalog
        .reconcile_observed_inventory(
            "endpoint.deepseek.official.global.v1",
            [ObservedModelV1 {
                upstream_model_id: "deepseek-v4-pro".into(),
                metadata: Default::default(),
            }],
        )
        .unwrap();
    assert_eq!(inventory.len(), 1);
    // The current catalog knows the publisher identity but keeps this Provider binding
    // conditional, so runtime qualification uses the provider-scoped fallback without
    // inventing a canonical executable configuration or cross-provider price.
    assert_eq!(inventory[0].model_configuration_id, None);
    assert!(catalog.runtime_fallback_allows_observed_text("deepseek-v4-pro"));
}

#[test]
fn model_data_with_endpoint_field_is_rejected_by_schema() {
    let mut value: serde_json::Value = serde_json::from_slice(MODEL_DATA).unwrap();
    value["data"]["models"][0]["endpoint_url"] = serde_json::json!("https://evil.test");
    assert!(serde_json::from_value::<hiroute_domain::ReleaseModelDataBundleV2>(value).is_err());
}

#[test]
fn current_catalog_contains_distinct_registered_provider_options() {
    let catalog = current_catalog();
    for option_id in [
        "bailian.payg.cn.v1",
        "openai.platform.global.v1",
        "anthropic.platform.global.v1",
        "google.gemini-api-paid.global.v1",
        "deepseek.official.global.v1",
    ] {
        catalog.resolve_connection_option(option_id).unwrap();
    }
}

#[test]
fn cpa_supervisor_returns_only_connector_owned_opaque_reference() {
    struct FakeCpa;

    impl CpaSupervisorPort for FakeCpa {
        fn materialize_account(
            &self,
            connector_id: &str,
            _endpoint_profile_id: &str,
        ) -> Result<CpaAccountMaterializationV1, CpaSupervisorError> {
            Ok(CpaAccountMaterializationV1 {
                connector_id: connector_id.to_owned(),
                connection_option_id: "codex.subscription.global.v1".into(),
                endpoint_profile_id: "endpoint.cpa.codex".into(),
                source_id: "cpa-account".into(),
                account_subject: "account.fixture".into(),
                credential_ref: CredentialRefV1::new(
                    "credential/cpa-account",
                    "source/cpa-account",
                    format!("connector/{connector_id}"),
                    "provider-auth",
                    ["connection-option/codex.subscription.global.v1".into()],
                    1,
                )
                .unwrap(),
                observed_model_ids: ["gpt-5.5".into()].into_iter().collect(),
            })
        }
    }

    let account = FakeCpa
        .materialize_account("connector.cpa.codex", "endpoint.cpa.codex")
        .unwrap();
    assert_eq!(
        account.credential_ref.subject(),
        "connector/connector.cpa.codex"
    );
    assert_eq!(account.credential_ref.purpose(), "provider-auth");
}

#[test]
fn cpa_registration_uses_client_bundled_identity_and_opaque_credential() {
    let catalog = current_catalog();
    let source_id = "cpa/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let materialization = CpaAccountMaterializationV1 {
        connector_id: "connector.cpa.codex".into(),
        connection_option_id: "codex.subscription.global.v1".into(),
        endpoint_profile_id: "endpoint.cpa.codex".into(),
        source_id: source_id.into(),
        account_subject:
            "account/cpa/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        credential_ref: CredentialRefV1::new(
            "credential/cpa/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            format!("source/{source_id}"),
            "connector/connector.cpa.codex",
            "provider-auth",
            ["connection-option/codex.subscription.global.v1".into()],
            1,
        )
        .unwrap(),
        observed_model_ids: ["gpt-5.5".into()].into_iter().collect(),
    };
    let registered = register_cpa_account(&catalog, &materialization).unwrap();
    assert_eq!(registered.source.origin, SourceOrigin::Cpa);
    assert_eq!(registered.source.identity.provider_platform_id, "openai");
    assert_eq!(registered.inventory.len(), 1);
    assert_eq!(
        registered.inventory[0].model_configuration_id.as_deref(),
        Some("model.openai.gpt-5.5")
    );

    let mut wrong_scope = materialization;
    wrong_scope.credential_ref = CredentialRefV1::new(
        "credential/cpa/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "source/other",
        "connector/connector.cpa.codex",
        "provider-auth",
        ["connection-option/codex.subscription.global.v1".into()],
        1,
    )
    .unwrap();
    assert_eq!(
        register_cpa_account(&catalog, &wrong_scope),
        Err(CpaSupervisorError::InvalidOpaqueMaterialization)
    );
}

#[test]
fn rotation_receipt_binds_candidate_source_endpoint_and_probe_operation() {
    let catalog = current_catalog();
    let option = catalog
        .registry()
        .resolve_option("deepseek.official.global.v1")
        .unwrap();
    let identity = SourceIdentityV1 {
        identity_revision: 1,
        provider_platform_id: option.endpoint_profile.provider_platform_id.clone(),
        service_offering_id: option.endpoint_profile.service_offering_id.clone(),
        entitlement_id: option.endpoint_profile.entitlement_id.clone(),
        usage_scope: option.endpoint_profile.usage_scope.clone(),
        endpoint_profile_id: option.endpoint_profile.endpoint_profile_id.clone(),
        endpoint_profile_revision: option.endpoint_profile.revision,
        region_id: option.endpoint_profile.region_id.clone(),
        account_subject_ref: "account.deepseek".into(),
        evidence_refs: vec![CanonicalDigest::of_bytes(b"account-deepseek")],
    };
    let source = ComputeSourceV1 {
        schema: hiroute_domain::COMPUTE_STATE_SCHEMA_V1.into(),
        source_id: "source/deepseek".into(),
        revision: 1,
        connection_option_id: option.option.connection_option_id.clone(),
        connector_id: option.connector.connector_id.clone(),
        connector_revision: option.connector.revision,
        origin: SourceOrigin::NativeApi,
        identity_digest: identity.digest().unwrap(),
        identity,
        billing_class: BillingClass::Paid,
        state: MaterializationState::Ready,
    };
    let destination = format!("connection-option/{}", source.connection_option_id);
    let credential = |generation| {
        CredentialRefV1::new(
            "credential/deepseek",
            "source/source/deepseek",
            "hirouted",
            "provider-auth",
            [destination.clone()],
            generation,
        )
        .unwrap()
    };
    let pool = CredentialPoolV1 {
        pool_id: "pool/deepseek".into(),
        binding_id: "binding/deepseek-model".into(),
        binding_revision: 1,
        binding_digest: CanonicalDigest::of_bytes(b"binding-deepseek-model"),
        source_id: source.source_id.clone(),
        source_revision: source.revision,
        connection_option_id: source.connection_option_id.clone(),
        source_identity_digest: source.identity_digest.clone(),
        offer_ref: "offer.deepseek.official.global".into(),
        offer_revision: 1,
        offer_evidence_digest: CanonicalDigest::of_bytes(b"offer"),
        billing_class: BillingClass::Paid,
        model_configuration_id: "model.deepseek.v4-pro-0813".into(),
        authentication: AuthenticationKind::ProviderApiKey,
        revision: 1,
        credentials: vec![PoolCredentialV1 {
            credential: credential(1),
            fingerprint: CanonicalDigest::of_bytes(b"old"),
            ordinal: 0,
            enabled: true,
        }],
    };
    let fingerprint = CanonicalDigest::of_bytes(b"candidate");
    let probe_operation = OperationId::parse("op_0123456789abcdef0123456789abcdef").unwrap();
    let receipt = VerifiedCredentialProbeV1::from_registered_probe(
        "credential/deepseek",
        2,
        fingerprint.clone(),
        &source,
        &option,
        probe_operation.clone(),
    )
    .unwrap();
    let rotated = rotate_credential_after_verified_probe(
        &pool,
        1,
        credential(2),
        fingerprint,
        &source,
        &option,
        &probe_operation,
        &receipt,
    )
    .unwrap();
    assert_eq!(rotated.credentials[0].credential.generation(), 2);
    assert_eq!(rotated.billing_class, BillingClass::Paid);
}

#[test]
fn trusted_option_queries_preserve_exact_results_without_exposing_mutable_catalog() {
    let catalog = current_catalog();
    for option in &catalog.registry().connection_options {
        let expected = catalog
            .registry()
            .resolve_option(&option.connection_option_id)
            .unwrap();
        for _ in 0..10 {
            assert_eq!(
                catalog
                    .resolve_connection_option(&option.connection_option_id)
                    .unwrap(),
                expected
            );
        }
        let mut returned = catalog
            .resolve_connection_option(&option.connection_option_id)
            .unwrap();
        returned.connector.connector_id = "tampered".into();
        assert_eq!(
            catalog
                .resolve_connection_option(&option.connection_option_id)
                .unwrap(),
            expected
        );
    }
    assert!(
        catalog
            .resolve_connection_option("https://evil.test")
            .is_err()
    );
    let mut raw = catalog.registry().clone();
    let known = raw.connection_options[0].connection_option_id.clone();
    raw.connection_options.last_mut().unwrap().connector_id = "missing-connector".into();
    assert!(
        raw.resolve_option(&known).is_err(),
        "unvalidated input must check the entire registry, including unrelated entries"
    );
}
