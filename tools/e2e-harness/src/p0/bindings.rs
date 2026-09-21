use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::canonical::{canonical_json_bytes, sha256_hex};

const CHALLENGE: &str = "${RUN_CHALLENGE}";
const CHALLENGE_JSON: &str = "${RUN_CHALLENGE_JSON}";
const CHALLENGE_SHA256: &str = "${RUN_CHALLENGE_SHA256}";
const CHALLENGE_JSON_SHA256: &str = "${RUN_CHALLENGE_JSON_SHA256}";
const REQUEST_ID: &str = "${REQUEST_ID}";
const CANONICAL_LEDGER_ROOT_DIGEST: &str = "${CANONICAL_LEDGER_ROOT_DIGEST}";
const OTEL_ROOT_DIGEST: &str = "${OTEL_ROOT_DIGEST}";
const FACT_PRODUCER_ID: &str = "${FACT_PRODUCER_ID}";
const FACT_PRODUCER_EPOCH: &str = "${FACT_PRODUCER_EPOCH}";
const FACT_EVENT_ID: &str = "${FACT_EVENT_ID}";
const FACT_STREAM_ID: &str = "${FACT_STREAM_ID}";
const CONTENT_PRODUCER_ID: &str = "${CONTENT_PRODUCER_ID}";
const CONTENT_PRODUCER_EPOCH: &str = "${CONTENT_PRODUCER_EPOCH}";
const CONTENT_STREAM_ID: &str = "${CONTENT_STREAM_ID}";
const CONTENT_FRAME_ID: &str = "${CONTENT_FRAME_ID}";
const CONTENT_REQUEST_ID: &str = "${CONTENT_REQUEST_ID}";
const CONTENT_RESPONSE_ID: &str = "${CONTENT_RESPONSE_ID}";

#[derive(Clone, Debug)]
pub(crate) struct RuntimeBindings {
    binding_nonce: String,
    producer_nonce: String,
    challenge: String,
    request_ids: BTreeMap<String, String>,
}

impl RuntimeBindings {
    pub fn from_nonces<'a>(
        binding_nonce: String,
        producer_nonce: String,
        case_ids: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let challenge = bound_id("challenge", &binding_nonce, "run");
        let request_ids = case_ids
            .into_iter()
            .map(|case_id| {
                (
                    case_id.to_owned(),
                    bound_id("request", &binding_nonce, case_id),
                )
            })
            .collect();
        Self {
            binding_nonce,
            producer_nonce,
            challenge,
            request_ids,
        }
    }

    pub fn validation<'a>(case_ids: impl IntoIterator<Item = &'a str>) -> Self {
        Self::from_nonces(
            "validation-binding-nonce-00000000000000000000000000000000".into(),
            "validation-producer-nonce-0000000000000000000000000000000".into(),
            case_ids,
        )
    }

    pub fn binding_nonce(&self) -> &str {
        &self.binding_nonce
    }

    pub fn producer_nonce(&self) -> &str {
        &self.producer_nonce
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    pub fn request_id(&self, case_id: &str) -> Option<&str> {
        self.request_ids.get(case_id).map(String::as_str)
    }

    pub fn case_for_request(&self, request_id: &str) -> Option<&str> {
        self.request_ids
            .iter()
            .find_map(|(case_id, candidate)| (candidate == request_id).then_some(case_id.as_str()))
    }

    pub fn fact_stream_id(&self, case_id: &str) -> Option<String> {
        self.request_id(case_id)
            .map(|request_id| scoped_id("fact-stream", request_id))
    }

    pub fn content_stream_id(&self, case_id: &str) -> Option<String> {
        self.request_id(case_id)
            .map(|request_id| scoped_id("content-stream", request_id))
    }

    pub fn materialize(&self, case_id: &str, value: &Value) -> Value {
        materialize_value(
            value,
            &self.producer_nonce,
            &self.challenge,
            self.request_id(case_id),
        )
    }

    pub fn materialize_global(&self, value: &Value) -> Value {
        materialize_value(value, &self.producer_nonce, &self.challenge, None)
    }
}

fn materialize_value(
    value: &Value,
    producer_nonce: &str,
    challenge: &str,
    request_id: Option<&str>,
) -> Value {
    let request_bound = |kind: &str| {
        request_id
            .map(|request_id| Value::String(scoped_id(kind, request_id)))
            .unwrap_or_else(|| Value::String(format!("${{{kind}}}")))
    };
    let request_digest = |kind: &str| {
        request_id
            .map(|request_id| Value::String(sha256_hex(format!("{kind}\0{request_id}").as_bytes())))
            .unwrap_or_else(|| Value::String(format!("${{{kind}}}")))
    };
    match value {
        Value::String(value) if value == CHALLENGE => Value::String(challenge.to_owned()),
        Value::String(value) if value == CHALLENGE_JSON => {
            let bytes = canonical_json_bytes(&json!({"nonce": challenge}));
            Value::String(String::from_utf8(bytes).expect("canonical JSON is UTF-8"))
        }
        Value::String(value) if value == CHALLENGE_SHA256 => {
            Value::String(sha256_hex(challenge.as_bytes()))
        }
        Value::String(value) if value == CHALLENGE_JSON_SHA256 => {
            let bytes = canonical_json_bytes(&json!({"nonce": challenge}));
            Value::String(sha256_hex(&bytes))
        }
        Value::String(value) if value == REQUEST_ID => request_id
            .map(|value| Value::String(value.to_owned()))
            .unwrap_or_else(|| Value::String(value.clone())),
        Value::String(value) if value == CANONICAL_LEDGER_ROOT_DIGEST => {
            request_digest("canonical_ledger-root")
        }
        Value::String(value) if value == OTEL_ROOT_DIGEST => request_digest("otel-root"),
        Value::String(value) if value == FACT_PRODUCER_ID => {
            Value::String(scoped_id("fact-producer", producer_nonce))
        }
        Value::String(value) if value == FACT_PRODUCER_EPOCH => {
            Value::String(scoped_id("fact-epoch", producer_nonce))
        }
        Value::String(value) if value == FACT_EVENT_ID => request_bound("fact-event"),
        Value::String(value) if value == FACT_STREAM_ID => request_bound("fact-stream"),
        Value::String(value) if value == CONTENT_PRODUCER_ID => {
            Value::String(scoped_id("content-producer", producer_nonce))
        }
        Value::String(value) if value == CONTENT_PRODUCER_EPOCH => {
            Value::String(scoped_id("content-epoch", producer_nonce))
        }
        Value::String(value) if value == CONTENT_STREAM_ID => request_bound("content-stream"),
        Value::String(value) if value == CONTENT_FRAME_ID => request_bound("content-frame"),
        Value::String(value) if value == CONTENT_REQUEST_ID => request_bound("content-request"),
        Value::String(value) if value == CONTENT_RESPONSE_ID => request_bound("content-response"),
        Value::String(value)
            if value.starts_with("${CONTENT_EVENT_ID_") && value.ends_with('}') =>
        {
            let ordinal = &value[19..value.len() - 1];
            request_bound(&format!("content-event-{ordinal}"))
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| materialize_value(value, producer_nonce, challenge, request_id))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        materialize_value(value, producer_nonce, challenge, request_id),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn bound_id(kind: &str, nonce: &str, scope: &str) -> String {
    let digest = sha256_hex(format!("{kind}\0{nonce}\0{scope}").as_bytes());
    format!("{kind}-{}", &digest[7..])
}

fn scoped_id(kind: &str, scope: &str) -> String {
    let digest = sha256_hex(format!("{kind}\0{scope}").as_bytes());
    format!("{kind}-{}", &digest[7..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_exact_typed_placeholders() {
        let bindings = RuntimeBindings::from_nonces(
            "unpredictable".into(),
            "producer-instance".into(),
            ["case"],
        );
        let value = bindings.materialize(
            "case",
            &json!({
                "challenge": CHALLENGE,
                "challenge_digest": CHALLENGE_SHA256,
                "challenge_json_digest": CHALLENGE_JSON_SHA256,
                "request": REQUEST_ID,
                "ledger_root": CANONICAL_LEDGER_ROOT_DIGEST,
                "otel_root": OTEL_ROOT_DIGEST,
                "event": FACT_EVENT_ID,
                "content_event": "${CONTENT_EVENT_ID_3}",
                "not_contains": "prefix-${RUN_CHALLENGE}"
            }),
        );
        assert!(
            value["challenge"]
                .as_str()
                .unwrap()
                .starts_with("challenge-")
        );
        assert_eq!(
            value["challenge_digest"],
            sha256_hex(value["challenge"].as_str().unwrap().as_bytes())
        );
        assert!(value["request"].as_str().unwrap().starts_with("request-"));
        assert!(
            value["ledger_root"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert!(value["otel_root"].as_str().unwrap().starts_with("sha256:"));
        assert_ne!(value["ledger_root"], value["otel_root"]);
        assert!(value["event"].as_str().unwrap().starts_with("fact-event-"));
        assert!(
            value["content_event"]
                .as_str()
                .unwrap()
                .starts_with("content-event-3-")
        );
        assert_eq!(value["not_contains"], "prefix-${RUN_CHALLENGE}");
    }

    #[test]
    fn independent_runs_never_reuse_producer_or_event_identity() {
        let value = json!({
            "producer": {"producer_id": FACT_PRODUCER_ID, "producer_epoch": FACT_PRODUCER_EPOCH},
            "event_id": FACT_EVENT_ID,
            "request_id": REQUEST_ID
        });
        let first =
            RuntimeBindings::from_nonces("first-binding".into(), "first-run".into(), ["case"])
                .materialize("case", &value);
        let second =
            RuntimeBindings::from_nonces("second-binding".into(), "second-run".into(), ["case"])
                .materialize("case", &value);
        assert_ne!(first, second);
    }
}
