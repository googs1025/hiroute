use hiroute_domain::MaterializationState;
use hiroute_integrations::{
    ComputeControlPlaneError, RegisteredComputeDiscoveryFactV1, TrustedReleaseCatalog,
};

const MANIFEST: &[u8] =
    include_bytes!("../../../assets/release-facts/current/bundle/manifest.json");
const REGISTRY: &[u8] =
    include_bytes!("../../../assets/release-facts/current/bundle/connector-registry.json");
const MODEL_DATA: &[u8] =
    include_bytes!("../../../assets/release-facts/current/bundle/model-data.json");

fn current_catalog() -> TrustedReleaseCatalog {
    TrustedReleaseCatalog::load_bundled_release_facts(MANIFEST, MANIFEST, REGISTRY, MODEL_DATA)
        .unwrap()
}

fn discovery(base_url: &str, model: &str) -> RegisteredComputeDiscoveryFactV1 {
    RegisteredComputeDiscoveryFactV1 {
        agent_id: "agent.claude-code".into(),
        scanner_id: "scanner.claude-code.v1".into(),
        scanner_version: "1".into(),
        discovered_source_ref: "claude/settings/user/fixture".into(),
        configuration_revision: 7,
        connection_option_id: "zhipu.coding-plan.cn.v1".into(),
        endpoint_profile_id: "endpoint.zhipu.coding-plan.cn.v1".into(),
        endpoint_profile_revision: 1,
        registered_base_url: base_url.into(),
        observed_model_id: model.into(),
        model_configuration_id: "model.zhipu.glm-5.3".into(),
        protected_credential_available: true,
    }
}

fn empty_projection_expectation() -> hiroute_domain::ComputeProjectionExpectationV1 {
    hiroute_domain::ComputeProjectionExpectationV1 {
        source_revision: 0,
        source_digest: None,
        binding_revision: 0,
        binding_digest: None,
        inventory_revision: 0,
        inventory_digest: None,
    }
}

#[test]
fn pre_gateway_compute_projects_only_exact_client_bound_endpoint_model_and_pool_identity() {
    let catalog = current_catalog();
    let discovered = discovery("https://open.bigmodel.cn/api/anthropic", "glm-5.3");
    let candidate = catalog
        .authorize_compute_discovery(discovered.clone())
        .unwrap();
    let ids = catalog.compute_projection_ids(&candidate).unwrap();
    let prepared = catalog
        .prepare_compute_projection(candidate, empty_projection_expectation(), true)
        .unwrap();
    prepared.validate().unwrap();
    assert_eq!(
        prepared.desired.source.state,
        MaterializationState::NeedsCredential
    );
    assert_eq!(
        prepared
            .desired
            .credential_pool_identity
            .as_ref()
            .unwrap()
            .binding_id,
        prepared.desired.binding.binding_id
    );
    let encoded = serde_json::to_string(&prepared).unwrap();
    assert!(!encoded.contains("token"));
    assert!(!encoded.contains("secret"));

    let mut rescanned = discovered;
    rescanned.configuration_revision += 1;
    let rescanned = catalog.authorize_compute_discovery(rescanned).unwrap();
    assert_eq!(catalog.compute_projection_ids(&rescanned).unwrap(), ids);
    let rescanned = catalog
        .prepare_compute_projection(rescanned, empty_projection_expectation(), true)
        .unwrap();
    assert_ne!(
        rescanned.desired.scanner.evidence_digest,
        prepared.desired.scanner.evidence_digest
    );

    assert!(matches!(
        catalog.authorize_compute_discovery(discovery(
            "https://open.bigmodel.cn.evil.test/api/anthropic",
            "glm-5.3",
        )),
        Err(ComputeControlPlaneError::DiscoveryMismatch)
    ));
    assert!(matches!(
        catalog.authorize_compute_discovery(discovery(
            "https://open.bigmodel.cn/api/anthropic",
            "unknown-model",
        )),
        Err(ComputeControlPlaneError::DiscoveryMismatch)
    ));
}

#[test]
fn pre_gateway_compute_projection_requires_explicit_materialization() {
    let catalog = current_catalog();
    let candidate = catalog
        .authorize_compute_discovery(discovery(
            "https://open.bigmodel.cn/api/anthropic",
            "glm-5.3",
        ))
        .unwrap();
    assert!(matches!(
        catalog.prepare_compute_projection(candidate, empty_projection_expectation(), false),
        Err(ComputeControlPlaneError::ExplicitMaterializationRequired)
    ));
}
