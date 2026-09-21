//! Strict Product ↔ Gateway frozen-DTO projections.
//!
//! These functions perform no lookup, caching, queueing, retry, or business inference. Every
//! input first decodes as the Gateway-owned type and every output validates as the Product-owned
//! type, so an unknown field or schema drift fails before a receiver or credential authority.

use hiroute_domain::{
    CONVERSATION_CONTENT_PORT_DIGEST_V2, CONVERSATION_CONTENT_SCHEMA_V2,
    ConversationContentEnvelopeV1 as ProductContentEnvelopeV2, EXECUTION_FACT_PORT_DIGEST_V2,
    EXECUTION_FACT_SCHEMA_V2, ExecutionFactEnvelopeV1 as ProductExecutionEnvelopeV1,
    LIFECYCLE_FACT_PORT_DIGEST_V2, LIFECYCLE_FACT_SCHEMA_V2,
    LifecycleFactEnvelopeV2 as ProductLifecycleEnvelopeV2, ObservationAckV2 as ProductAckV2,
    ObservationFeedback, ObservationGapHeartbeatV1 as ProductGapHeartbeatV1,
    ObservationNackV1 as ProductNackV1,
};
use hiroute_gateway::server::core_runtime::observation::{
    CONVERSATION_CONTENT_PORT_DIGEST, CONVERSATION_CONTENT_SCHEMA,
    ConversationContentEnvelopeV1 as GatewayContentEnvelopeV2, EXECUTION_FACT_PORT_DIGEST,
    EXECUTION_FACT_SCHEMA, ExecutionFactEnvelopeV1 as GatewayExecutionEnvelopeV2,
    LIFECYCLE_FACT_PORT_DIGEST, LIFECYCLE_FACT_SCHEMA,
    LifecycleFactEnvelopeV1 as GatewayLifecycleEnvelopeV2, OBSERVATION_GAP_HEARTBEAT_SCHEMA,
    ObservationAckV2 as GatewayAckV2, ObservationGapHeartbeatV1 as GatewayGapHeartbeatV1,
    ObservationNackV1 as GatewayNackV1,
};
use hiroute_gateway::server::publication::GatewayPublicationSnapshotV3;
use serde_json::{Map, Value};
use thiserror::Error;

pub fn project_publication(
    product: &hiroute_domain::GatewayPublicationSnapshotProjectionV3,
) -> Result<GatewayPublicationSnapshotV3, GatewayProjectionError> {
    let gateway: GatewayPublicationSnapshotV3 = transcode(product)?;
    gateway
        .validate()
        .map_err(|_| GatewayProjectionError::InvalidOutput)?;
    Ok(gateway)
}

pub fn project_lifecycle_payload(
    payload: &[u8],
) -> Result<ProductLifecycleEnvelopeV2, GatewayProjectionError> {
    let gateway: GatewayLifecycleEnvelopeV2 = decode(payload)?;
    if gateway.schema_version != LIFECYCLE_FACT_SCHEMA
        || gateway.schema_digest != LIFECYCLE_FACT_PORT_DIGEST
        || LIFECYCLE_FACT_SCHEMA != LIFECYCLE_FACT_SCHEMA_V2
        || LIFECYCLE_FACT_PORT_DIGEST != LIFECYCLE_FACT_PORT_DIGEST_V2
    {
        return Err(GatewayProjectionError::UnsupportedSchema);
    }
    let product: ProductLifecycleEnvelopeV2 = transcode(&gateway)?;
    product
        .validate()
        .map_err(|_| GatewayProjectionError::InvalidOutput)?;
    Ok(product)
}

pub fn project_execution_payload(
    payload: &[u8],
) -> Result<ProductExecutionEnvelopeV1, GatewayProjectionError> {
    let gateway: GatewayExecutionEnvelopeV2 = decode(payload)?;
    if gateway.schema_version != EXECUTION_FACT_SCHEMA
        || gateway.schema_digest != EXECUTION_FACT_PORT_DIGEST
    {
        return Err(GatewayProjectionError::UnsupportedSchema);
    }
    let mut value = serde_json::to_value(gateway).map_err(|_| GatewayProjectionError::Encoding)?;
    let object = value
        .as_object_mut()
        .ok_or(GatewayProjectionError::InvalidInput)?;
    object.insert(
        "schema_version".into(),
        Value::String(EXECUTION_FACT_SCHEMA_V2.into()),
    );
    object.insert(
        "schema_digest".into(),
        Value::String(EXECUTION_FACT_PORT_DIGEST_V2.into()),
    );
    let mut trust = Map::new();
    for field in [
        "authority_id",
        "authority_epoch",
        "served_model_id",
        "selector_source",
        "agent_plan_id",
        "route",
        "gateway_publication_revision",
        "gateway_publication_digest",
        "grant_id",
        "grant_generation",
        "ingress_protocol",
    ] {
        trust.insert(
            field.into(),
            object
                .remove(field)
                .ok_or(GatewayProjectionError::InvalidInput)?,
        );
    }
    if let Some(plan_display_name) = object.remove("plan_display_name") {
        trust.insert("plan_display_name".into(), plan_display_name);
    }
    object.insert("trust".into(), Value::Object(trust));
    if let Some(reasoning) = object
        .get_mut("fact")
        .and_then(Value::as_object_mut)
        .filter(|fact| fact.get("kind").and_then(Value::as_str) == Some("route_decision"))
        .and_then(|fact| fact.get_mut("requested_reasoning_value"))
        && !reasoning.is_null()
    {
        *reasoning = tagged_native_value(reasoning.take());
    }
    let producer = object
        .get_mut("producer")
        .and_then(Value::as_object_mut)
        .ok_or(GatewayProjectionError::InvalidInput)?;
    let mut stream = Map::new();
    for field in ["producer_id", "producer_epoch", "stream_id"] {
        stream.insert(
            field.into(),
            producer
                .remove(field)
                .ok_or(GatewayProjectionError::InvalidInput)?,
        );
    }
    producer.insert("stream".into(), Value::Object(stream));
    let product: ProductExecutionEnvelopeV1 =
        serde_json::from_value(value).map_err(|_| GatewayProjectionError::InvalidOutput)?;
    product
        .validate()
        .map_err(|_| GatewayProjectionError::InvalidOutput)?;
    Ok(product)
}

fn tagged_native_value(value: Value) -> Value {
    match value {
        Value::Null => serde_json::json!({"kind": "null"}),
        Value::Bool(value) => serde_json::json!({"kind": "bool", "value": value}),
        Value::Number(value) => {
            if let Some(value) = value.as_u64() {
                serde_json::json!({"kind": "unsigned", "value": value})
            } else if let Some(value) = value.as_i64() {
                serde_json::json!({"kind": "signed", "value": value})
            } else {
                serde_json::json!({"kind": "decimal", "value": value})
            }
        }
        Value::String(value) => serde_json::json!({"kind": "string", "value": value}),
        Value::Array(values) => serde_json::json!({
            "kind": "array",
            "value": values.into_iter().map(tagged_native_value).collect::<Vec<_>>()
        }),
        Value::Object(values) => serde_json::json!({
            "kind": "object",
            "value": values
                .into_iter()
                .map(|(key, value)| (key, tagged_native_value(value)))
                .collect::<Map<String, Value>>()
        }),
    }
}

pub fn project_content_payload(
    payload: &[u8],
) -> Result<ProductContentEnvelopeV2, GatewayProjectionError> {
    let gateway: GatewayContentEnvelopeV2 = decode(payload)?;
    if gateway.schema_version != CONVERSATION_CONTENT_SCHEMA
        || gateway.schema_digest != CONVERSATION_CONTENT_PORT_DIGEST
        || CONVERSATION_CONTENT_SCHEMA != CONVERSATION_CONTENT_SCHEMA_V2
        || CONVERSATION_CONTENT_PORT_DIGEST != CONVERSATION_CONTENT_PORT_DIGEST_V2
    {
        return Err(GatewayProjectionError::UnsupportedSchema);
    }
    let product: ProductContentEnvelopeV2 = transcode(&gateway)?;
    product
        .validate()
        .map_err(|_| GatewayProjectionError::InvalidOutput)?;
    Ok(product)
}

pub fn project_gap_payload(
    payload: &[u8],
    expected_channel: &str,
) -> Result<ProductGapHeartbeatV1, GatewayProjectionError> {
    let gateway: GatewayGapHeartbeatV1 = decode(payload)?;
    if gateway.schema_version != OBSERVATION_GAP_HEARTBEAT_SCHEMA
        || gateway.channel != expected_channel
    {
        return Err(GatewayProjectionError::UnsupportedSchema);
    }
    let product: ProductGapHeartbeatV1 = transcode(&gateway)?;
    product
        .validate()
        .map_err(|_| GatewayProjectionError::InvalidOutput)?;
    Ok(product)
}

pub fn project_feedback(
    feedback: ObservationFeedback,
) -> Result<Result<GatewayAckV2, Box<GatewayNackV1>>, GatewayProjectionError> {
    match feedback {
        ObservationFeedback::Ack(ack) => {
            let gateway: GatewayAckV2 = transcode::<ProductAckV2, _>(&ack)?;
            gateway
                .validate()
                .map_err(|_| GatewayProjectionError::InvalidOutput)?;
            Ok(Ok(gateway))
        }
        ObservationFeedback::Nack(nack) => {
            let gateway: GatewayNackV1 = transcode::<ProductNackV1, _>(&nack)?;
            gateway
                .validate()
                .map_err(|_| GatewayProjectionError::InvalidOutput)?;
            Ok(Err(Box::new(gateway)))
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T, GatewayProjectionError> {
    serde_json::from_slice(payload).map_err(|_| GatewayProjectionError::InvalidInput)
}

fn transcode<T: serde::Serialize, U: serde::de::DeserializeOwned>(
    value: &T,
) -> Result<U, GatewayProjectionError> {
    let value = serde_json::to_value(value).map_err(|_| GatewayProjectionError::Encoding)?;
    serde_json::from_value(value).map_err(|_| GatewayProjectionError::InvalidOutput)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum GatewayProjectionError {
    #[error("Gateway payload does not match its frozen DTO")]
    InvalidInput,
    #[error("Gateway payload uses an unsupported schema or digest")]
    UnsupportedSchema,
    #[error("Gateway adapter projection could not be encoded")]
    Encoding,
    #[error("Product projection does not match its frozen DTO")]
    InvalidOutput,
}

#[cfg(test)]
mod tests;
