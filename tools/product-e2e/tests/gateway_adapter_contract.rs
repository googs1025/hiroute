use std::collections::BTreeSet;

use hiroute_domain::{
    GATEWAY_OPERATIONAL_TARGET_SCHEMA_V1, NATIVE_CREDENTIAL_LEASE_REQUEST_SCHEMA_V1,
};
use hiroute_gateway::server::core_runtime::observation::{
    CONVERSATION_CONTENT_PORT_SCHEMA, CREDENTIAL_PORT_SCHEMA, EXECUTION_FACT_PORT_SCHEMA,
    LIFECYCLE_FACT_PORT_SCHEMA, RUNTIME_PUBLICATION_PORT_SCHEMA, RUNTIME_STATE_PORT_SCHEMA,
};
use hiroute_product_e2e::gateway_adapters::{COMPOSITION_BLOCKER, GatewayAdapterContractV1};
use serde::Deserialize;

const SCENARIO: &str =
    include_str!("../../../e2e/product/scenarios/gateway-adapters/exact-adapters.v1.json");
const FIXTURE: &str =
    include_str!("../../../e2e/product/fixtures/gateway-adapters/exact-fields.v1.json");
const GOLDEN: &str =
    include_str!("../../../e2e/product/golden/gateway-adapters/exact-boundary.v1.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactFields {
    schema: String,
    publication_candidate: BTreeSet<String>,
    credential_request: BTreeSet<String>,
    probe_outcomes: BTreeSet<String>,
    content_events: BTreeSet<String>,
    rich_ack: BTreeSet<String>,
    rich_nack: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactBoundary {
    scenario_id: String,
    adapter_contract: String,
    real_hirouted_smoke: String,
    composition_owner: String,
    runtime_publication_schema: String,
    credential_schema: String,
    runtime_state_schema: String,
    lifecycle_schema: String,
    execution_schema: String,
    content_schema: String,
    native_credential_schema: String,
    target_schema: String,
    adapter_time_join: bool,
    acknowledgement_cache: bool,
    second_content_queue: bool,
}

#[test]
fn gateway_adapter_contract_freezes_six_ports_without_claiming_composition() {
    let contract: GatewayAdapterContractV1 = serde_json::from_str(SCENARIO).unwrap();
    contract.validate().unwrap();
    assert_eq!(contract.composition.blocker, COMPOSITION_BLOCKER);

    let golden: ExactBoundary = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(golden.scenario_id, "process-25028-gateway-exact-adapters");
    assert_eq!(golden.adapter_contract, "green");
    assert_eq!(golden.real_hirouted_smoke, "expected_red");
    assert_eq!(golden.composition_owner, "PROCESS-25009");
    assert_eq!(
        golden.runtime_publication_schema,
        RUNTIME_PUBLICATION_PORT_SCHEMA
    );
    assert_eq!(golden.credential_schema, CREDENTIAL_PORT_SCHEMA);
    assert_eq!(golden.runtime_state_schema, RUNTIME_STATE_PORT_SCHEMA);
    assert_eq!(golden.lifecycle_schema, LIFECYCLE_FACT_PORT_SCHEMA);
    assert_eq!(golden.execution_schema, EXECUTION_FACT_PORT_SCHEMA);
    assert_eq!(golden.content_schema, CONVERSATION_CONTENT_PORT_SCHEMA);
    assert_eq!(
        golden.native_credential_schema,
        NATIVE_CREDENTIAL_LEASE_REQUEST_SCHEMA_V1
    );
    assert_eq!(golden.target_schema, GATEWAY_OPERATIONAL_TARGET_SCHEMA_V1);
    assert!(!golden.adapter_time_join);
    assert!(!golden.acknowledgement_cache);
    assert!(!golden.second_content_queue);
}

#[test]
fn gateway_adapter_fixture_lists_every_p0_identity_and_rich_feedback_coordinate() {
    let fields: ExactFields = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(fields.schema, "hiroute.gateway-exact-adapter-fields/v1");
    for required in [
        "credential_destination_ref",
        "upstream_model_id",
        "native_transport_model",
        "operational_target",
        "operational_target_digest",
        "protocol_profiles",
        "protocol_profile_digest",
    ] {
        assert!(fields.publication_candidate.contains(required));
    }
    for required in [
        "authentication",
        "logical_endpoint",
        "request_path",
        "runtime_epoch",
        "target_epoch",
        "excluded_key_ids",
    ] {
        assert!(fields.credential_request.contains(required));
    }
    assert_eq!(
        fields.probe_outcomes,
        BTreeSet::from(["acquired".into(), "busy".into(), "conflict".into()])
    );
    assert_eq!(
        fields.content_events,
        BTreeSet::from([
            "abort".into(),
            "append".into(),
            "begin".into(),
            "finish".into(),
        ])
    );
    assert!(fields.rich_ack.contains("acknowledged_blobs"));
    assert!(fields.rich_ack.contains("delta_parent_transcript_root"));
    assert!(fields.rich_nack.contains("missing_sequence_ranges"));
    assert!(fields.rich_nack.contains("missing_blob"));
    assert!(fields.rich_nack.contains("unknown_transcript_root"));
    assert!(fields.rich_nack.contains("content_state_conflict"));
}
