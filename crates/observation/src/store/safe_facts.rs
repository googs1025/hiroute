//! Whitelisted non-text evidence. Full wire fields never enter this projection.
use crate::writer::ObservationStoreError as Error;
use hiroute_domain::{
    CanonicalDigest, ExecutionFactEnvelopeV1, ExecutionFactV1, ObservationRoutingContextV1,
    ObservationSafeFactV2, UsageProvenanceV1,
};
use rusqlite::{Transaction, params};
pub(crate) fn project(
    transaction: &Transaction<'_>,
    envelope: &ExecutionFactEnvelopeV1,
    digest: &CanonicalDigest,
) -> Result<(), Error> {
    let fact_json = serde_json::to_value(&envelope.fact).map_err(|_| Error::Corrupt)?;
    let mut fact = ObservationSafeFactV2 {
        event_id: envelope.event_id.to_string(),
        sequence: envelope.sequence,
        original_digest: digest.to_string(),
        routing_context: Some(ObservationRoutingContextV1::from_execution_trust(
            &envelope.trust,
        )),
        occurred_at_ms: Some(envelope.occurred_at_ms().map_err(|_| Error::Corrupt)?),
        event_kind: fact_json
            .get("kind")
            .and_then(|value| value.as_str())
            .ok_or(Error::Corrupt)?
            .into(),
        attempt_ordinal: None,
        native_model: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        outcome: None,
        sensitive_fields_deleted: false,
    };
    match &envelope.fact {
        ExecutionFactV1::AttemptStarted {
            ordinal,
            request_model,
            ..
        } => {
            fact.attempt_ordinal = Some(*ordinal);
            fact.native_model = Some(request_model.clone());
        }
        ExecutionFactV1::SemanticCommit { ordinal, .. } => fact.attempt_ordinal = Some(*ordinal),
        ExecutionFactV1::UsageAndCache {
            ordinal,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            input_provenance,
            output_provenance,
            cache_read_provenance,
            cache_write_provenance,
            reasoning_provenance,
            ..
        } => {
            let reported = |value: Option<u64>, provenance: &UsageProvenanceV1| {
                if *provenance == UsageProvenanceV1::Reported {
                    value
                } else {
                    None
                }
            };
            fact.attempt_ordinal = Some(*ordinal);
            fact.input_tokens = reported(*input_tokens, input_provenance);
            fact.output_tokens = reported(*output_tokens, output_provenance);
            fact.cache_read_tokens = reported(*cache_read_tokens, cache_read_provenance);
            fact.cache_write_tokens = reported(*cache_write_tokens, cache_write_provenance);
            fact.reasoning_tokens = reported(*reasoning_tokens, reasoning_provenance);
        }
        ExecutionFactV1::RequestFinished { .. } => {
            fact.outcome = fact_json
                .get("outcome")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        }
        _ => {}
    }
    let json = serde_json::to_string(&fact).map_err(|_| Error::Corrupt)?;
    transaction.execute("INSERT OR IGNORE INTO observation_safe_facts_v2(workspace_id,request_id,original_digest,body_json) VALUES(?1,?2,?3,?4)",params![envelope.correlation.workspace_id.as_str(),envelope.correlation.request_id.as_str(),digest.as_str(),json]).map_err(|_|Error::ActivityUnavailable)?;
    Ok(())
}
