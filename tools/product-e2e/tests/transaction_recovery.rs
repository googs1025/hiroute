use std::collections::BTreeSet;

use hiroute_product_e2e::verify_dependency_metadata;
use serde::Deserialize;
use serde_json::Value;

#[path = "support/validation_metadata.rs"]
mod validation_metadata;

#[path = "../src/transaction/mod.rs"]
mod transaction;

use transaction::{FORMAL_COMPOSITION_BLOCKER, TransactionRecoveryContractV1};

const SCENARIO: &str =
    include_str!("../../../e2e/product/scenarios/transaction/recovery-contract.v1.json");
const FIXTURE: &str =
    include_str!("../../../e2e/product/fixtures/transaction/protected-input.v1.json");
const GOLDEN: &str =
    include_str!("../../../e2e/product/golden/transaction/recovery-boundary.v1.json");

#[derive(Deserialize)]
struct BoundaryGolden {
    scenario_id: String,
    formal_composition: String,
    reason: String,
    composition_owner: String,
    product_dependencies: BTreeSet<String>,
}

#[test]
fn transaction_recovery_is_an_exact_black_box_contract_with_expected_red_composition() {
    let scenario: TransactionRecoveryContractV1 = serde_json::from_str(SCENARIO).unwrap();
    scenario.validate().unwrap();
    let golden: BoundaryGolden = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(golden.scenario_id, scenario.scenario_id);
    assert_eq!(golden.formal_composition, "expected_red");
    assert_eq!(golden.reason, FORMAL_COMPOSITION_BLOCKER);
    assert_eq!(
        golden.composition_owner,
        scenario.formal_composition.composition_owner
    );
    assert_eq!(
        golden.product_dependencies,
        BTreeSet::from([
            "hiroute-application-api".to_owned(),
            "hiroute-cpa-bridge".to_owned(),
            "hiroute-domain".to_owned(),
            "hiroute-gateway".to_owned(),
        ])
    );
}

#[test]
fn transaction_recovery_fixture_contains_only_a_logical_secret_slot() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(fixture["logical_slot"], "primary");
    assert_eq!(fixture["contains_secret_literal"], false);
    assert_eq!(fixture["source_locator_persisted"], false);
    let encoded = serde_json::to_vec(&fixture).unwrap();
    for forbidden in ["api_key", "authorization", "bearer", "secret_value"] {
        assert!(
            !encoded
                .windows(forbidden.len())
                .any(|window| window.eq_ignore_ascii_case(forbidden.as_bytes()))
        );
    }
}

#[test]
fn transaction_recovery_dependency_gate_does_not_compose_application_or_storage() {
    let metadata = validation_metadata::current();
    verify_dependency_metadata(&metadata).unwrap();
    let package = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == "hiroute-product-e2e")
        .unwrap();
    let product_dependencies = package["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|dependency| dependency["name"].as_str())
        .filter(|name| name.starts_with("hiroute-"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        product_dependencies,
        BTreeSet::from([
            "hiroute-application-api",
            "hiroute-cpa-bridge",
            "hiroute-domain",
            "hiroute-gateway",
        ])
    );
}
