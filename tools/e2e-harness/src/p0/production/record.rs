use std::collections::BTreeSet;

use hiroute_gateway::server::core_runtime::observation::{
    ConversationContentEnvelopeV1, ExecutionFactEnvelopeV1, LifecycleFactEnvelopeV1,
    OtelGenAiRecordV1,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::types::{
    ChannelEvidence, LEGACY_EXECUTION_SCHEMA, LEGACY_PRICED_EXECUTION_SCHEMA,
    LEGACY_PRICED_EXECUTION_SCHEMA_DIGEST, ProductionError,
};

pub(super) fn validate_channel(
    channel: &ChannelEvidence,
    expected_name: &str,
    schema: &str,
    schema_digest: &str,
    request_id: &str,
) -> Result<(), ProductionError> {
    if channel.name != expected_name
        || channel.records.is_empty()
        || channel.first_sequence != 1
        || channel.last_sequence != channel.first_sequence + channel.records.len() as u64 - 1
        || channel.record_count != channel.records.len()
        || !valid_sha256(&channel.file_sha256)
        || channel.records_digest != ChannelEvidence::digest_records(&channel.records)
    {
        return Err(collector("channel summary is inconsistent"));
    }
    let mut event_ids = BTreeSet::new();
    for (offset, record) in channel.records.iter().enumerate() {
        validate_record_shape(expected_name, record)?;
        if !valid_record_schema(expected_name, record, schema, schema_digest)
            || (expected_name != "otel"
                && record.get("channel").and_then(Value::as_str) != Some(expected_name))
            || (expected_name == "otel"
                && record
                    .get("channel")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value != expected_name))
            || record.get("sequence").and_then(Value::as_u64)
                != Some(channel.first_sequence + offset as u64)
            || record
                .pointer("/producer/producer_id")
                .and_then(Value::as_str)
                != Some(channel.producer_id.as_str())
            || record
                .pointer("/producer/producer_epoch")
                .and_then(Value::as_str)
                != Some(channel.producer_epoch.as_str())
            || record
                .pointer("/producer/stream_id")
                .and_then(Value::as_str)
                != Some(channel.stream_id.as_str())
            || record
                .pointer("/correlation/request_id")
                .and_then(Value::as_str)
                != Some(request_id)
            || ["workspace_id", "conversation_id", "turn_id"]
                .iter()
                .any(|field| {
                    record
                        .pointer(&format!("/correlation/{field}"))
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                })
            || !record.get("loss_watermark").is_some_and(Value::is_null)
        {
            return Err(collector(
                "channel record identity, ordering, or gap proof failed",
            ));
        }
        let event_id = record
            .get("event_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| collector("channel event identity is missing"))?;
        if !event_ids.insert(event_id) {
            return Err(collector("channel event identity is duplicated"));
        }
    }
    Ok(())
}

fn valid_record_schema(
    channel: &str,
    record: &Value,
    base_schema: &str,
    base_digest: &str,
) -> bool {
    let schema = record.get("schema_version").and_then(Value::as_str);
    let digest = record.get("schema_digest").and_then(Value::as_str);
    if channel != "execution_fact" {
        return schema == Some(base_schema) && digest == Some(base_digest);
    }

    let priced_attempt = record
        .get("pricing")
        .is_some_and(|pricing| !pricing.is_null())
        && record.pointer("/fact/kind").and_then(Value::as_str) == Some("attempt_started");
    if base_schema == LEGACY_EXECUTION_SCHEMA {
        if priced_attempt {
            schema == Some(LEGACY_PRICED_EXECUTION_SCHEMA)
                && digest == Some(LEGACY_PRICED_EXECUTION_SCHEMA_DIGEST)
        } else {
            record.get("pricing").is_none()
                && schema == Some(base_schema)
                && digest == Some(base_digest)
        }
    } else {
        schema == Some(base_schema) && digest == Some(base_digest)
    }
}

pub(super) fn validate_record_shape(channel: &str, record: &Value) -> Result<(), ProductionError> {
    match channel {
        "lifecycle" => decode::<LifecycleFactEnvelopeV1>(channel, record),
        "execution_fact" => decode::<ExecutionFactEnvelopeV1>(channel, record),
        "conversation_content" => decode::<ConversationContentEnvelopeV1>(channel, record),
        "otel" => decode::<OtelGenAiRecordV1>(channel, record),
        _ => Err(ProductionError::Collector(format!(
            "unknown production observation channel {channel}"
        ))),
    }
}

fn decode<T: DeserializeOwned>(channel: &str, record: &Value) -> Result<(), ProductionError> {
    serde_json::from_value::<T>(record.clone())
        .map(|_| ())
        .map_err(|error| {
            ProductionError::Collector(format!(
                "{channel} record violates its exact product schema: {error}"
            ))
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn collector(message: &'static str) -> ProductionError {
    ProductionError::Collector(message.into())
}
