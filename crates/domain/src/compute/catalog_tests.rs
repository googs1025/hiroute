use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::{CanonicalDigest, GatewayAuthenticationSemanticsV1};
use serde_json::json;

fn digest(label: &str) -> CanonicalDigest {
    CanonicalDigest::of_bytes(label.as_bytes())
}

fn registry() -> ConnectorRegistryBundleV1 {
    ConnectorRegistryBundleV1 {
        schema: CONNECTOR_REGISTRY_SCHEMA_V1.to_owned(),
        registry_version: "registry-fixture-v1".to_owned(),
        product_release: "release-fixture-v1".to_owned(),
        connectors: vec![ConnectorDescriptorV1 {
            connector_id: "connector.fixture".to_owned(),
            revision: 1,
            runtime_kind: ConnectorRuntimeKind::BuiltinNative,
            implementation_ref: "builtin/fixture".to_owned(),
            implementation_revision: 1,
            accepted_origins: BTreeSet::from([
                ConnectionOrigin::NativeApi,
                ConnectionOrigin::FreeCatalog,
            ]),
            authentication: AuthenticationKind::ProviderApiKey,
            required_secret_slots: vec!["provider_api_key".into()],
            endpoint_profile_refs: vec!["endpoint.fixture".to_owned()],
            catalog_adapter_ref: "catalog.fixture".into(),
            catalog_adapter_revision: 1,
            error_classifier_ref: "errors.fixture".into(),
            error_classifier_revision: 1,
            usage_decoder_ref: "usage.fixture".into(),
            usage_decoder_revision: 1,
            cache_policy_ref: "cache.fixture".into(),
            cache_policy_revision: 1,
        }],
        endpoint_profiles: vec![EndpointProfileV1 {
            endpoint_profile_id: "endpoint.fixture".to_owned(),
            revision: 1,
            connector_id: "connector.fixture".into(),
            connector_revision: 1,
            provider_platform_id: "provider.fixture".to_owned(),
            service_offering_id: "offering.fixture".to_owned(),
            entitlement_id: "payg".to_owned(),
            usage_scope: "account".to_owned(),
            region_id: "test".to_owned(),
            logical_endpoint_group: "fixture".to_owned(),
            protocol_endpoints: vec![ProtocolEndpointV1 {
                protocol_endpoint_id: "protocol.fixture.chat".to_owned(),
                protocol: UpstreamProtocol::ChatCompletions,
                base_url: "https://api.release-fixture.invalid".to_owned(),
                request_path: "/v1/chat/completions".to_owned(),
                adapter_ref: "adapter.fixture".to_owned(),
                adapter_revision: 1,
                stable_preference: 0,
                inventory_path: Some("/v1/models".to_owned()),
                authentication_semantics: None,
                required_headers: Vec::new(),
            }],
            inventory_strategy: InventoryStrategyKind::RemoteModels,
            inventory_protocol_endpoint_id: Some("protocol.fixture.chat".into()),
            verification_evidence: digest("endpoint-evidence").to_string(),
            last_verified_at: 1,
        }],
        connection_options: vec![ConnectionOptionV1 {
            connection_option_id: "fixture.payg.test.v1".to_owned(),
            display_name: "Fixture Pay-as-you-go".into(),
            origin: ConnectionOrigin::NativeApi,
            connector_id: "connector.fixture".to_owned(),
            connector_revision: 1,
            endpoint_profile_id: "endpoint.fixture".to_owned(),
            endpoint_profile_revision: 1,
            billing_class: BillingClass::Paid,
            free_offer_ref: None,
            direct_verification_evidence: None,
        }],
    }
}

#[test]
fn endpoint_authority_is_exact_not_url_equivalence() {
    let endpoint = &registry().endpoint_profiles[0].protocol_endpoints[0];
    assert!(
        endpoint
            .authorize_exact_destination("https://api.release-fixture.invalid/v1/chat/completions")
    );
    for malicious in [
        "https://API.release-fixture.invalid/v1/chat/completions",
        "https://api.release-fixture.invalid./v1/chat/completions",
        "https://api.release-fixture.invalid:443/v1/chat/completions",
        "https://api.release-fixture.invalid/v1/chat/completions?next=evil",
        "https://api.release-fixture.invalid.evil.test/v1/chat/completions",
        "https://xn--api-release-fixture-qf0c.invalid/v1/chat/completions",
    ] {
        assert!(!endpoint.authorize_exact_destination(malicious));
    }

    for malicious_base in [
        "http://api.release-fixture.invalid",
        "https://API.release-fixture.invalid",
        "https://api.release-fixture.invalid.",
        "https://api.release-fixture.invalid:443",
        "https://api.release-fixture.invalid/path",
        "https://api.release-fixture.invalid?next=evil",
        "https://api.release-fixture.invalid#fragment",
        "https://api.release-fixture.invalid@evil.test",
    ] {
        let mut invalid = registry();
        invalid.endpoint_profiles[0].protocol_endpoints[0].base_url = malicious_base.into();
        assert_eq!(
            invalid.validate(),
            Err(ComputeContractError::InvalidEndpoint),
            "accepted malicious base {malicious_base}"
        );
    }
    for malicious_path in [
        "//evil.test/v1",
        "/v1/../admin",
        "/v1/chat?next=evil",
        "/v1/chat#fragment",
        "/v1/%2e%2e/admin",
        "/v1\\chat",
    ] {
        let mut invalid = registry();
        invalid.endpoint_profiles[0].protocol_endpoints[0].request_path = malicious_path.into();
        assert_eq!(
            invalid.validate(),
            Err(ComputeContractError::InvalidEndpoint),
            "accepted malicious path {malicious_path}"
        );
    }
}

#[test]
fn catalog_endpoint_authentication_and_static_headers_fail_closed() {
    let mut valid = registry();
    let endpoint = &mut valid.endpoint_profiles[0].protocol_endpoints[0];
    endpoint.authentication_semantics = Some(GatewayAuthenticationSemanticsV1::Bearer);
    endpoint.required_headers = vec![("anthropic-version".into(), "2023-06-01".into())];
    valid.validate().unwrap();

    let mut duplicate = valid.clone();
    duplicate.endpoint_profiles[0].protocol_endpoints[0]
        .required_headers
        .push(("anthropic-version".into(), "2023-06-01".into()));
    assert_eq!(
        duplicate.validate(),
        Err(ComputeContractError::InvalidEndpoint)
    );

    let mut secret_static_header = valid.clone();
    secret_static_header.endpoint_profiles[0].protocol_endpoints[0].required_headers =
        vec![("x-api-key".into(), "not-a-secret-slot".into())];
    assert_eq!(
        secret_static_header.validate(),
        Err(ComputeContractError::InvalidEndpoint)
    );

    let mut conflicting_authentication_header = valid.clone();
    let endpoint =
        &mut conflicting_authentication_header.endpoint_profiles[0].protocol_endpoints[0];
    endpoint.authentication_semantics = Some(GatewayAuthenticationSemanticsV1::ApiKeyHeader {
        header: "anthropic-version".into(),
    });
    assert_eq!(
        conflicting_authentication_header.validate(),
        Err(ComputeContractError::InvalidEndpoint)
    );

    let mut unsafe_authentication_header = valid.clone();
    unsafe_authentication_header.endpoint_profiles[0].protocol_endpoints[0]
        .authentication_semantics = Some(GatewayAuthenticationSemanticsV1::ApiKeyHeader {
        header: "cookie".into(),
    });
    assert_eq!(
        unsafe_authentication_header.validate(),
        Err(ComputeContractError::InvalidEndpoint)
    );

    let mut mismatched_connector_authentication = valid;
    mismatched_connector_authentication.endpoint_profiles[0].protocol_endpoints[0]
        .authentication_semantics = Some(GatewayAuthenticationSemanticsV1::None);
    assert_eq!(
        mismatched_connector_authentication.validate(),
        Err(ComputeContractError::InvalidRegistry)
    );
}

#[test]
fn unknown_and_unknown_billing_fail_closed() {
    let registry = registry();
    assert_eq!(
        registry.resolve_option("https://evil.test"),
        Err(ComputeContractError::InvalidIdentifier)
    );
    let mut unknown = registry.clone();
    unknown.connection_options[0].billing_class = BillingClass::Unknown;
    assert_eq!(
        unknown.validate(),
        Err(ComputeContractError::CrossReference)
    );

    let identity = SourceIdentityV1 {
        identity_revision: 1,
        provider_platform_id: "provider.fixture".into(),
        service_offering_id: "offering.fixture".into(),
        entitlement_id: "payg".into(),
        usage_scope: "account".into(),
        endpoint_profile_id: "endpoint.fixture".into(),
        endpoint_profile_revision: 1,
        region_id: "test".into(),
        account_subject_ref: "account.opaque".into(),
        evidence_refs: vec![digest("account-evidence")],
    };
    let source = ComputeSourceV1 {
        schema: COMPUTE_STATE_SCHEMA_V1.into(),
        source_id: "source.fixture".into(),
        revision: 1,
        connection_option_id: "fixture.payg.test.v1".into(),
        connector_id: "connector.fixture".into(),
        connector_revision: 1,
        origin: SourceOrigin::NativeApi,
        identity_digest: identity.digest().unwrap(),
        identity,
        billing_class: BillingClass::Paid,
        state: MaterializationState::NeedsCredential,
    };
    assert_eq!(
        source.validate(&registry, false),
        Err(ComputeContractError::ExplicitMaterializationRequired)
    );
    source.validate(&registry, true).unwrap();
}

#[test]
fn inventory_is_sorted_deduplicated_and_non_authoritative() {
    let data: ModelDataBundleV1 = serde_json::from_value(json!({
        "schema": MODEL_DATA_SCHEMA_V1,
        "bundle_version": "bundle-v1", "product_release": "release-fixture-v1",
        "connector_registry_version": "registry-fixture-v1",
        "models_slice_version": "models-v1", "capability_slice_version": "cap-v1",
        "ratings_slice_version": "rating-v1", "prices_slice_version": "price-v1",
        "free_offers_slice_version": "free-v1",
        "models": [{"model_configuration_id":"model.known","revision":1,"display_name":"Known","publisher_id":"publisher.fixture","capabilities":{"tool":true,"vision":false,"streaming":true,"context_tokens":100,"max_output_tokens":10}}],
        "model_endpoint_capabilities": [{"capability_id":"cap.known","revision":1,"model_configuration_id":"model.known","connector_id":"connector.fixture","connector_revision":1,"endpoint_profile_id":"endpoint.fixture","endpoint_profile_revision":1,"protocol_endpoint_id":"protocol.fixture.chat","upstream_protocol":"chat_completions","upstream_model_id":"known","required_adapter_ref":"adapter.fixture","required_adapter_revision":1,"evidence_digest":digest("cap")}],
        "ratings": [], "offers": [], "free_offers": [], "price_rates": []
    })).unwrap();
    let observed = vec![
        ObservedModelV1 {
            upstream_model_id: "unknown".into(),
            metadata: BTreeMap::new(),
        },
        ObservedModelV1 {
            upstream_model_id: "known".into(),
            metadata: BTreeMap::from([("z".into(), "1".into())]),
        },
        ObservedModelV1 {
            upstream_model_id: "known".into(),
            metadata: BTreeMap::from([("a".into(), "2".into())]),
        },
    ];
    let result = reconcile_inventory("endpoint.fixture", observed, &data).unwrap();
    assert_eq!(result[0].upstream_model_id, "known");
    assert_eq!(result[0].disposition, InventoryDisposition::CatalogMatched);
    assert_eq!(result[1].disposition, InventoryDisposition::InventoryOnly);
    assert_eq!(
        reconcile_inventory(
            "endpoint.fixture",
            [
                ObservedModelV1 {
                    upstream_model_id: "known".into(),
                    metadata: BTreeMap::from([("lifecycle".into(), "active".into())]),
                },
                ObservedModelV1 {
                    upstream_model_id: "known".into(),
                    metadata: BTreeMap::from([("lifecycle".into(), "retired".into())]),
                },
            ],
            &data,
        ),
        Err(ComputeContractError::ConflictingInventory)
    );
    for forbidden_field in [
        "endpoint_url",
        "protocol",
        "authentication",
        "billing_class",
        "capabilities",
    ] {
        let mut value = json!({"upstream_model_id":"known","metadata":{}});
        value[forbidden_field] = json!("forged");
        assert!(serde_json::from_value::<ObservedModelV1>(value).is_err());
    }
    for reserved_metadata_key in [
        "endpoint_url",
        "protocol",
        "authentication",
        "billing_class",
        "capabilities",
    ] {
        let forged = ObservedModelV1 {
            upstream_model_id: "known".into(),
            metadata: BTreeMap::from([(reserved_metadata_key.into(), "forged".into())]),
        };
        assert_eq!(
            forged.validate(),
            Err(ComputeContractError::InvalidInventory)
        );
    }

    let mut ambiguous = data;
    ambiguous.models.push(ModelDefinitionV1 {
        model_configuration_id: "model.other".into(),
        revision: 1,
        display_name: "Other".into(),
        publisher_id: "publisher.fixture".into(),
        capabilities: CapabilityFactsV1 {
            tool: false,
            vision: false,
            streaming: true,
            context_tokens: 100,
            max_output_tokens: 10,
        },
    });
    let mut second = ambiguous.model_endpoint_capabilities[0].clone();
    second.capability_id = "cap.other".into();
    second.model_configuration_id = "model.other".into();
    ambiguous.model_endpoint_capabilities.push(second);
    assert_eq!(
        ambiguous.validate_against(&registry()),
        Err(ComputeContractError::InvalidModelData)
    );
}

#[test]
fn compute_free_direct_requires_exact_verified_registry_evidence() {
    let evidence = digest("direct-request-verification").to_string();
    let mut direct_registry = registry();
    direct_registry.connection_options[0].billing_class = BillingClass::Free;
    direct_registry.connection_options[0].origin = ConnectionOrigin::FreeCatalog;
    direct_registry.connectors[0].accepted_origins =
        BTreeSet::from([ConnectionOrigin::FreeCatalog]);
    direct_registry.connectors[0].authentication = AuthenticationKind::None;
    direct_registry.connectors[0].required_secret_slots.clear();
    direct_registry.connection_options[0].free_offer_ref = Some("free.offer.one".into());
    direct_registry.connection_options[0].direct_verification_evidence = Some(evidence.clone());
    direct_registry.validate().unwrap();
    let data: ModelDataBundleV1 = serde_json::from_value(json!({
        "schema": MODEL_DATA_SCHEMA_V1,
        "bundle_version": "bundle-v1", "product_release": "release-fixture-v1",
        "connector_registry_version": "registry-fixture-v1",
        "models_slice_version": "models-v1", "capability_slice_version": "cap-v1",
        "ratings_slice_version": "rating-v1", "prices_slice_version": "price-v1",
        "free_offers_slice_version": "free-v1",
        "models": [{"model_configuration_id":"model.free","revision":1,"display_name":"Free","publisher_id":"publisher.fixture","capabilities":{"tool":true,"vision":false,"streaming":true,"context_tokens":100,"max_output_tokens":10}}],
        "model_endpoint_capabilities": [{"capability_id":"cap.free","revision":1,"model_configuration_id":"model.free","connector_id":"connector.fixture","connector_revision":1,"endpoint_profile_id":"endpoint.fixture","endpoint_profile_revision":1,"protocol_endpoint_id":"protocol.fixture.chat","upstream_protocol":"chat_completions","upstream_model_id":"free-upstream","required_adapter_ref":"adapter.fixture","required_adapter_revision":1,"evidence_digest":digest("cap-free")}],
        "ratings": [],
        "offers": [{"offer_id":"offer.free","revision":1,"endpoint_profile_id":"endpoint.fixture","endpoint_profile_revision":1,"service_offering_id":"offering.fixture","entitlement_id":"payg","usage_scope":"account","region_id":"test","model_configuration_ids":["model.free"],"billing_class":"free","evidence_digest":digest("offer-free")}],
        "free_offers": [{"free_offer_id":"free.offer.one","revision":1,"connection_option_id":"fixture.payg.test.v1","offer_ref":"offer.free","model_configuration_ids":["model.free"],"access":"direct","direct_verification_evidence":evidence,"last_verified_at":1}],
        "price_rates": []
    }))
    .unwrap();
    data.validate_against(&direct_registry).unwrap();

    let mut api_key_registry = registry();
    api_key_registry.connection_options[0].billing_class = BillingClass::Free;
    api_key_registry.connection_options[0].origin = ConnectionOrigin::FreeCatalog;
    api_key_registry.connectors[0].accepted_origins =
        BTreeSet::from([ConnectionOrigin::FreeCatalog]);
    api_key_registry.connection_options[0].free_offer_ref = Some("free.offer.one".into());
    api_key_registry.connection_options[0].direct_verification_evidence = None;
    api_key_registry.validate().unwrap();
    let mut api_key_data = data.clone();
    api_key_data.free_offers[0].access = FreeAccess::ApiKeyRequired;
    api_key_data.free_offers[0].direct_verification_evidence = None;
    api_key_data.validate_against(&api_key_registry).unwrap();
    api_key_data.free_offers[0].direct_verification_evidence =
        Some(digest("forged-direct-claim").to_string());
    assert_eq!(
        api_key_data.validate_against(&api_key_registry),
        Err(ComputeContractError::UnverifiedDirectOffer)
    );

    let mut unverified = data.clone();
    unverified.free_offers[0].direct_verification_evidence = None;
    assert_eq!(
        unverified.validate_against(&direct_registry),
        Err(ComputeContractError::UnverifiedDirectOffer)
    );
    let mut wrong_evidence = data;
    wrong_evidence.free_offers[0].direct_verification_evidence =
        Some(digest("different-evidence").to_string());
    assert_eq!(
        wrong_evidence.validate_against(&direct_registry),
        Err(ComputeContractError::CrossReference)
    );
}
