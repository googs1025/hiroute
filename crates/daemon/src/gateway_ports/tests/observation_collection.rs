use std::sync::Arc;

use hiroute_gateway::server::core_runtime::observation::{
    EXECUTION_FACT_PORT_DIGEST, EXECUTION_FACT_SCHEMA,
};
use hiroute_observation::{DigestAuthority, LocalObservationStore, writer::IngestOutcome};
use rusqlite::Connection;
use serde_json::{Value, json};

use super::super::GatewayExecutionFactSink;

const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn current_gateway_payloads_collect_priced_and_unpriced_attempts_in_one_product_table() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        LocalObservationStore::open(directory.path(), DigestAuthority::new([31; 32])).unwrap(),
    );
    let sink = GatewayExecutionFactSink::new(store);

    for (sequence, request, pricing) in [
        (1, "request-priced", Some(pricing())),
        (2, "request-unpriced", None),
    ] {
        let payload = execution_payload(sequence, request, pricing);
        let outcome = sink
            .project_and_ingest_payload_for_test(&serde_json::to_vec(&payload).unwrap())
            .unwrap();
        assert!(matches!(outcome, IngestOutcome::Ack(_)));
    }

    let connection = Connection::open(directory.path().join("activity.db")).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT request_id, pricing_json IS NOT NULL
             FROM valuation_attempt_inputs_v2 ORDER BY request_id",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("request-priced".to_owned(), true),
            ("request-unpriced".to_owned(), false),
        ]
    );
}

fn execution_payload(sequence: u64, request_id: &str, pricing: Option<Value>) -> Value {
    json!({
        "schema_version": EXECUTION_FACT_SCHEMA,
        "schema_digest": EXECUTION_FACT_PORT_DIGEST,
        "channel": "execution_fact",
        "producer": {
            "component": "gateway-execution",
            "revision": "hiroute-gateway-observation/1",
            "producer_id": "producer-main",
            "producer_epoch": "epoch-main",
            "stream_id": "stream-main"
        },
        "sequence": sequence,
        "event_id": format!("execution-event-{sequence}"),
        "correlation": {
            "workspace_id": "personal/default",
            "conversation_id": "conversation-main",
            "session_scope": "conversation",
            "correlation_provenance": "agent_supplied",
            "turn_id": "turn-main",
            "request_id": request_id
        },
        "attempt_id": format!("attempt-{sequence}"),
        "authority_id": "authority-main",
        "authority_epoch": 3,
        "served_model_id": "hiroute/coding",
        "selector_source": "trusted_model_alias",
        "agent_plan_id": "plan/coding",
        "route": {"kind": "plan", "revision": 7, "semantic_digest": DIGEST},
        "plan_display_name": "Coding route",
        "gateway_publication_revision": "11",
        "gateway_publication_digest": DIGEST,
        "grant_id": "grant-main",
        "grant_generation": 2,
        "ingress_protocol": "responses",
        "occurred_at_unix_nanos": sequence * 1_000_000,
        "pricing": pricing,
        "fact": {
            "kind": "attempt_started",
            "ordinal": 1,
            "candidate_id": format!("candidate-{sequence}"),
            "stable_binding_id": format!("binding-{sequence}"),
            "profile_digest": DIGEST,
            "credential_ref": format!("credential-{sequence}"),
            "key_id": format!("key-{sequence}"),
            "provider_name": "provider-main",
            "request_model": "native-model-main",
            "upstream_protocol": "responses",
            "model_configuration_id": "model-config-main",
            "adapter_revision": "protocol-adapter/v1",
            "start_reason": "initial_candidate",
            "previous_attempt_id": null
        },
        "loss_watermark": null,
        "completeness_delta": null
    })
}

fn pricing() -> Value {
    json!({
        "schema_version": "hiroute.observation.execution-pricing/v1",
        "request_generation": null,
        "captured_at_ms": 0,
        "attempt_execution_at_ms": 1,
        "quote": null,
        "reference_quote": null,
        "unknown_reason": "snapshot_unavailable"
    })
}
