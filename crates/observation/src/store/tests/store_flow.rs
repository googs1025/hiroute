use std::sync::Arc;

use hiroute_domain::{
    ExecutionFactV1, ObservationNackDetailV1, ObservationQueryError, ObservationQueryPort,
};
use tempfile::tempdir;

use crate::{
    FactChannel, LocalObservationStore, LocalObservationWriter, OfferOutcome, WriterCycleOutcome,
};

use super::support::*;

#[test]
fn receipt_value_and_checkpoint_are_frozen_and_restart_safe() {
    let temporary = tempdir().unwrap();
    let fixture = Fixture::new("receipt");
    let store = open_store(temporary.path());
    let writer = writer(&store);
    let channel = fact_channel(&fixture, 512 * 1024);
    let facts = request_facts(&fixture, "attempt-1", finished("attempt-1"), 100);
    for fact in facts.clone() {
        assert!(matches!(
            offer_fact(&writer, &channel, fact),
            WriterCycleOutcome::Ack(_)
        ));
    }
    let receipt = store
        .get_receipt(
            &fixture.workspace,
            &hiroute_domain::ReceiptId::parse(fixture.request.as_str()).unwrap(),
        )
        .unwrap();
    assert_eq!(receipt.trust.route.plan_revision(), Some(17));
    assert_eq!(receipt.trust.gateway_publication_revision, "41");
    assert_eq!(receipt.trust.grant_generation, 3);
    assert_eq!(receipt.ordered_facts.len(), 10);
    assert!(receipt.ordered_facts.iter().any(|event| matches!(
        &event.fact,
        ExecutionFactV1::AttemptStarted { model_configuration_id, .. }
            if model_configuration_id == "model-a"
    )));
    let value = store
        .get_value(
            &fixture.workspace,
            &value_query("plan/codex-daily", "USD", Some(fixture.session.clone())),
        )
        .unwrap();
    assert_eq!(value.baseline_api_equivalent_cost_micros, Some(1_200));
    assert_eq!(value.chosen_api_equivalent_cost_micros, Some(900));
    assert_eq!(value.actual_incremental_cost_micros, Some(300));
    assert_eq!(value.routing_savings_micros, Some(300));
    assert_eq!(value.entitlement_savings_micros, Some(600));
    assert_eq!(value.estimated_total_savings_micros, Some(900));

    drop(channel);
    drop(writer);
    drop(store);
    let reopened = Arc::new(LocalObservationStore::open(temporary.path(), authority()).unwrap());
    let writer = LocalObservationWriter::new(reopened.clone());
    let replay_channel = FactChannel::new(fixture.fact_stream.clone(), 64 * 1024);
    assert_eq!(
        replay_channel.offer(facts[9].clone()),
        OfferOutcome::Accepted
    );
    let WriterCycleOutcome::Ack(replay_ack) = writer.consume_fact(&replay_channel) else {
        panic!("replayed event must be acknowledged")
    };
    assert_eq!(replay_ack.highest_contiguous_sequence, 10);
    assert_eq!(replay_ack.highest_accounted_sequence, 10);
    let persisted_facts: i64 = reopened
        .connection
        .lock()
        .query_row("SELECT COUNT(*) FROM execution_fact_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(persisted_facts, 10, "dedup must not append a second fact");
    assert_eq!(
        reopened
            .get_receipt(
                &fixture.workspace,
                &hiroute_domain::ReceiptId::parse(fixture.request.as_str()).unwrap(),
            )
            .unwrap(),
        receipt
    );
}

#[test]
fn fact_log_tamper_and_unknown_contract_version_fail_closed() {
    let temporary = tempdir().unwrap();
    let fixture = Fixture::new("integrity");
    let store = open_store(temporary.path());
    let writer = writer(&store);
    let channel = fact_channel(&fixture, 512 * 1024);
    let facts = request_facts(
        &fixture,
        "attempt-integrity",
        finished("attempt-integrity"),
        1_000,
    );

    let mut unknown = facts[0].clone();
    unknown.schema_version = "hiroute.observation.execution-fact-envelope/v3".to_owned();
    assert_eq!(channel.offer(unknown), OfferOutcome::Accepted);
    let WriterCycleOutcome::Nack(nack) = writer.consume_fact(&channel) else {
        panic!("an unknown fact version must be rejected")
    };
    assert!(matches!(
        nack.detail,
        ObservationNackDetailV1::UnsupportedSchema { .. }
    ));

    for envelope in facts.iter().cloned() {
        assert!(matches!(
            offer_fact(&writer, &channel, envelope),
            WriterCycleOutcome::Ack(_)
        ));
    }
    let receipt_id = hiroute_domain::ReceiptId::parse(fixture.request.as_str()).unwrap();
    assert!(store.get_receipt(&fixture.workspace, &receipt_id).is_ok());
    {
        let connection = store.connection.lock();
        let body: String = connection
            .query_row(
                "SELECT body_json FROM observation_sensitive_payloads_v2 WHERE id=(SELECT 'fact:'||envelope_digest FROM execution_fact_events WHERE sequence=1)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let tampered = body.replace(
            "\"authority_id\":\"authority-local\"",
            "\"authority_id\":\"authority-tampered\"",
        );
        assert_ne!(tampered, body);
        connection
            .execute(
                "UPDATE observation_sensitive_payloads_v2 SET body_json=?1 WHERE id=(SELECT 'fact:'||envelope_digest FROM execution_fact_events WHERE sequence=1)",
                [tampered],
            )
            .unwrap();
    }
    assert_eq!(
        store
            .get_receipt(&fixture.workspace, &receipt_id)
            .unwrap_err(),
        ObservationQueryError::Corrupt
    );
    assert_eq!(
        offer_fact(&writer, &channel, facts[0].clone()),
        WriterCycleOutcome::StoreFailed(crate::writer::ObservationStoreError::Corrupt),
        "dedup must validate the authoritative raw fact instead of trusting its ACK ledger"
    );
}
