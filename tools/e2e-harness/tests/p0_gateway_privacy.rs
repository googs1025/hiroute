mod runtime_support;

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use runtime_support::{NativeProvider, ObservationFaults, ProviderReply, RuntimeFixture, request};
use serde_json::Value;

const ACCEPTED: &[u8] = br#"{"id":"privacy-ok","model":"native-private","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"private-response-22008"}]}],"usage":{"input_tokens":2,"output_tokens":3,"total_tokens":5}}"#;

#[test]
fn lifecycle_facts_and_default_otel_are_zero_content_and_rejected_auth_has_no_content() {
    hiroute_e2e::p0_execution_receipt!(
        "privacy.secret_scan",
        [
            "privacy.otel_zero_content",
            "privacy.rejected_auth_zero_content"
        ]
    );
    let provider = NativeProvider::start(vec![ProviderReply::Complete {
        status: 200,
        error_kind: None,
        body: ACCEPTED,
    }]);
    let fixture =
        RuntimeFixture::launch_with_observation(&[&provider], 1, ObservationFaults::healthy());
    let rejected_secret = "rejected-auth-secret-22008";
    let rejected = request(
        fixture.address,
        "POST",
        "/v1/responses",
        &[("Authorization", "Bearer wrong-secret-token-22008")],
        format!(r#"{{"model":"runtime-model","input":"{rejected_secret}"}}"#).as_bytes(),
    );
    assert_eq!(rejected.status, 401);

    let accepted_secret = "accepted-request-secret-22008";
    assert_eq!(
        fixture
            .request_body(
                format!(
                    r#"{{"model":"runtime-model","input":"{accepted_secret}","stream":false}}"#
                )
                .as_bytes(),
            )
            .status,
        200
    );

    let root = fixture.observation_root.as_deref().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let (lifecycle, facts, content, otel) = loop {
        let read = |name: &str| fs::read_to_string(root.join(name)).unwrap_or_default();
        let values = (
            read("lifecycle.jsonl"),
            read("execution-fact.jsonl"),
            read("conversation-content.jsonl"),
            read("otel.jsonl"),
        );
        let contains = |document: &str, pointer: &str, expected: &str| {
            document.lines().any(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|value| value.pointer(pointer).cloned())
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .as_deref()
                    == Some(expected)
            })
        };
        let response_content_finished = values.2.lines().any(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .is_some_and(|value| {
                    value.pointer("/direction").and_then(Value::as_str)
                        == Some("response_delivered")
                        && value.pointer("/phase").and_then(Value::as_str) == Some("finish")
                })
        });
        if contains(&values.1, "/fact/kind", "request_finished")
            && response_content_finished
            && !values.0.is_empty()
            && !values.3.is_empty()
        {
            break values;
        }
        assert!(
            Instant::now() < deadline,
            "observation files did not become visible"
        );
        thread::sleep(Duration::from_millis(10));
    };

    for zero_content in [&lifecycle, &facts, &otel] {
        assert!(!zero_content.contains(accepted_secret));
        assert!(!zero_content.contains("private-response-22008"));
        assert!(!zero_content.contains(rejected_secret));
        assert!(!zero_content.contains("wrong-secret-token-22008"));
        assert!(!zero_content.contains("provider-secret"));
        assert!(!zero_content.contains("authorization"));
        assert!(!zero_content.contains("canonical_bytes_base64"));
    }
    let decoded = decoded_content(&content);
    assert!(decoded.contains(accepted_secret));
    assert!(decoded.contains("private-response-22008"));
    for forbidden in [
        rejected_secret,
        "wrong-secret-token-22008",
        "provider-secret",
    ] {
        assert!(!content.contains(forbidden));
        assert!(!decoded.contains(forbidden));
    }
}

fn decoded_content(records: &str) -> String {
    let mut decoded = Vec::new();
    for encoded in records.lines().filter_map(|line| {
        serde_json::from_str::<Value>(line)
            .ok()?
            .get("canonical_bytes_base64")?
            .as_str()
            .map(str::to_owned)
    }) {
        decoded.extend(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
        );
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
