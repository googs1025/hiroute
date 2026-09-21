use serde_json::Value;
use std::{path::PathBuf, process::Command};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn validate(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hiroute-e2e"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn generic_case_validation_selects_one_declared_partition() {
    let root = root();
    let scenario = root.join("e2e/scenarios/core-routing.json");
    let profile = root.join("e2e/profiles/local-process.json");
    let output = validate(&[
        "validate",
        "--scenario",
        scenario.to_str().unwrap(),
        "--profile",
        profile.to_str().unwrap(),
        "--case",
        "responses-complex-continuation",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["selected_case"], "responses-complex-continuation");
    assert_eq!(report["steps"], 2);
    assert_eq!(
        report["case_shards"],
        serde_json::json!([{
            "id": "responses-complex-continuation",
            "steps": [
                "responses-complex-to-codex",
                "responses-tool-continuation-affinity-hit"
            ]
        }])
    );

    let unknown = validate(&[
        "validate",
        "--scenario",
        scenario.to_str().unwrap(),
        "--profile",
        profile.to_str().unwrap(),
        "--case",
        "not-a-case",
    ]);
    assert!(!unknown.status.success());
}

#[test]
fn sealed_production_scenario_rejects_case_selection() {
    let root = root();
    let schema = root.join("e2e/schema");
    let scenario = root.join("e2e/scenarios/p0-gateway.json");
    let profile = root.join("e2e/profiles/gateway-isolated.json");
    let output = validate(&[
        "validate",
        "--schema",
        schema.to_str().unwrap(),
        "--scenario",
        scenario.to_str().unwrap(),
        "--profile",
        profile.to_str().unwrap(),
        "--case",
        "normal_hirouted_listener_exact_evidence",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--case"));
}
