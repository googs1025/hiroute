use std::collections::{BTreeMap, BTreeSet};

use hiroute_cpa_bridge::STOCK_CPA_CONTRACT_VERSION;
use hiroute_domain::CredentialRefV1;
use serde::Deserialize;

#[path = "../src/cpa/mod.rs"]
mod cpa;

use cpa::{
    COMPOSITION_BLOCKER, CpaConnectorContractV1, GATEWAY_ADAPTER_BLOCKER, PACKAGING_BLOCKER,
    ScenarioState, validate_stock_config_keys,
};

const SCENARIO: &str =
    include_str!("../../../e2e/product/scenarios/cpa/managed-runtime-contract.v1.json");
const FIXTURE: &str = include_str!("../../../e2e/product/fixtures/cpa/managed-runtime.v1.json");
const GOLDEN: &str =
    include_str!("../../../e2e/product/golden/cpa/managed-runtime-boundary.v1.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeFixture {
    schema: String,
    stock_contract_version: String,
    launch_arguments: Vec<String>,
    ordinary_environment_forwarded: bool,
    transport_identity: String,
    logical_identity_source: String,
    credential_materialization: String,
    native_api_key_route: String,
    stock_binary_verification: String,
    account_protocols: BTreeMap<String, String>,
    config_keys: BTreeSet<String>,
    forbidden_material_fields: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundaryGolden {
    scenario_id: String,
    managed_connector_component: String,
    stock_binary_contract: String,
    gateway_exact_request_adapter: String,
    gateway_adapter_owner: String,
    production_hiroute_hirouted_composition: String,
    composition_owner: String,
    three_platform_artifact_packaging: String,
    packaging_owner: String,
    process_exit_reporting: String,
}

#[test]
fn cpa_connector_component_is_green_without_claiming_public_composition() {
    let contract: CpaConnectorContractV1 = serde_json::from_str(SCENARIO).unwrap();
    contract.validate().unwrap();
    assert_eq!(
        contract
            .scenario_states
            .iter()
            .find(|state| state.scenario_id == "managed-connector-component")
            .unwrap()
            .state,
        ScenarioState::Green
    );
    let gateway = contract
        .scenario_states
        .iter()
        .find(|state| state.scenario_id == "gateway-exact-request-adapter")
        .unwrap();
    assert_eq!(gateway.state, ScenarioState::ExpectedRed);
    assert_eq!(gateway.blocker.as_deref(), Some(GATEWAY_ADAPTER_BLOCKER));
    let composition = contract
        .scenario_states
        .iter()
        .find(|state| state.scenario_id == "production-hiroute-hirouted-composition")
        .unwrap();
    assert_eq!(composition.state, ScenarioState::ExpectedRed);
    assert_eq!(composition.blocker.as_deref(), Some(COMPOSITION_BLOCKER));
    let packaging = contract
        .scenario_states
        .iter()
        .find(|state| state.scenario_id == "three-platform-artifact-packaging")
        .unwrap();
    assert_eq!(packaging.state, ScenarioState::ExpectedRed);
    assert_eq!(packaging.blocker.as_deref(), Some(PACKAGING_BLOCKER));
}

#[test]
fn cpa_fixture_freezes_stock_launch_config_and_native_key_bypass() {
    let fixture: RuntimeFixture = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(fixture.schema, "hiroute.cpa-managed-runtime-fixture/v1");
    assert_eq!(fixture.stock_contract_version, STOCK_CPA_CONTRACT_VERSION);
    assert_eq!(
        fixture.launch_arguments,
        ["--config", "owner-only-config-path", "--local-model"]
    );
    assert!(!fixture.ordinary_environment_forwarded);
    assert_eq!(fixture.transport_identity, "authenticated_loopback_only");
    assert_eq!(
        fixture.logical_identity_source,
        "catalog_bound_connection_option_and_endpoint_profile"
    );
    assert_eq!(
        fixture.credential_materialization,
        "connector_owned_opaque_reference_only"
    );
    assert_eq!(fixture.native_api_key_route, "builtin_native_only");
    assert_eq!(fixture.stock_binary_verification, "environment_gated");
    assert_eq!(fixture.account_protocols["codex"], "responses");
    assert_eq!(fixture.account_protocols["claude"], "messages");
    assert!(validate_stock_config_keys(&fixture.config_keys));
    assert_eq!(fixture.forbidden_material_fields.len(), 6);
}

#[test]
fn cpa_opaque_reference_has_one_exact_option_scope_and_no_transport_field() {
    let credential = CredentialRefV1::new(
        "credential/cpa/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "source/cpa/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "connector/connector.cpa.codex",
        "provider-auth",
        ["connection-option/codex.subscription.test.v1".into()],
        1,
    )
    .unwrap();
    assert_eq!(credential.allowed_destinations().len(), 1);
    assert_eq!(credential.subject(), "connector/connector.cpa.codex");
    let encoded = serde_json::to_value(&credential).unwrap();
    assert!(encoded.get("endpoint_url").is_none());
    assert!(encoded.get("host").is_none());
    assert!(encoded.get("secret").is_none());
}

#[test]
fn cpa_golden_reports_process_exit_independently_from_scenario_state() {
    let golden: BoundaryGolden = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(golden.scenario_id, "process-25017-cpa-connector");
    assert_eq!(golden.managed_connector_component, "green");
    assert_eq!(golden.stock_binary_contract, "environment_gated");
    assert_eq!(golden.gateway_exact_request_adapter, "expected_red");
    assert_eq!(golden.gateway_adapter_owner, "PROCESS-25008");
    assert_eq!(
        golden.production_hiroute_hirouted_composition,
        "expected_red"
    );
    assert_eq!(golden.composition_owner, "PROCESS-25009");
    assert_eq!(golden.three_platform_artifact_packaging, "expected_red");
    assert_eq!(golden.packaging_owner, "PROCESS-25010");
    assert_eq!(
        golden.process_exit_reporting,
        "separate_from_scenario_state"
    );
}
