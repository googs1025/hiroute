use serde_json::{Value, json};

use crate::content_ref::{ContentRef, JsonValueExt, compact_ingress_document};
use crate::server::request_plan::IngressProtocol;

use super::{ReplayManager, ReplayStore, TestRoot, budget, config, read_all};

// Without a content digest, a client can construct a literal matching the raw
// stream's locator by finding a fixed point of the two serialized lengths.
fn matching_raw_locator(
    store: &ReplayStore,
    document: impl Fn(&str) -> Value,
) -> (Value, ContentRef, String) {
    let mut marker = ContentRef::new(1, 0, 0, 0).wire_marker();
    for _ in 0..8 {
        let value = document(&marker);
        let body = serde_json::to_vec(&value).unwrap();
        let escaped_len = serde_json::to_vec(&String::from_utf8(body.clone()).unwrap())
            .unwrap()
            .len()
            - 2;
        let next = ContentRef::new(1, 0, body.len() as u64, escaped_len as u64).wire_marker();
        if next == marker {
            let mut writer = store.begin_raw().unwrap();
            writer.append(&body).unwrap();
            let raw = writer.seal().unwrap();
            assert_eq!(raw.wire_marker(), marker);
            return (value, raw, marker);
        }
        marker = next;
    }
    panic!("raw locator lengths did not converge");
}

#[test]
fn client_text_matching_the_raw_locator_remains_literal() {
    let root = TestRoot::new("literal-raw-locator");
    let manager = ReplayManager::open(config(&root.0, 64 * 1024, 4096)).unwrap();
    let store = manager.begin_request(budget()).unwrap();
    let (mut document, raw, marker) =
        matching_raw_locator(&store, |marker| json!({"model": "alias", "input": marker}));

    compact_ingress_document(IngressProtocol::Responses, &mut document, &store).unwrap();
    let content = ContentRef::from_wire_marker(document["input"].as_str().unwrap()).unwrap();
    assert_eq!(read_all(&store, &content), marker.as_bytes());
    store.release_stream(&raw).unwrap();
    assert_eq!(
        read_all(&store, &content),
        marker.as_bytes(),
        "canonical content must survive normal raw-body release"
    );
}

#[test]
fn client_json_matching_the_raw_locator_remains_literal() {
    let root = TestRoot::new("literal-json-raw-locator");
    let manager = ReplayManager::open(config(&root.0, 64 * 1024, 4096)).unwrap();
    let store = manager.begin_request(budget()).unwrap();
    let (mut document, raw, _) = matching_raw_locator(&store, |marker| {
        let reference = ContentRef::from_wire_marker(marker).unwrap();
        json!({
            "model": "alias",
            "max_tokens": 16,
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call-1",
                    "name": "example",
                    "input": reference.json_marker()
                }]
            }]
        })
    });
    let expected = document["messages"][0]["content"][0]["input"].clone();

    compact_ingress_document(IngressProtocol::Messages, &mut document, &store).unwrap();
    let content = document["messages"][0]["content"][0]["input"]
        .content_ref()
        .unwrap();
    let observed: Value = serde_json::from_slice(&read_all(&store, &content)).unwrap();
    assert_eq!(observed, expected);
    store.release_stream(&raw).unwrap();
    let observed: Value = serde_json::from_slice(&read_all(&store, &content)).unwrap();
    assert_eq!(observed, expected);
}
