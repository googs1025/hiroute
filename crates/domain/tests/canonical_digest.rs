//! Frozen default-graph JSON digest goldens; run with and without serde_json/preserve_order.
use hiroute_domain::CanonicalDigest;
use serde::Serialize;
use serde_json::json;

#[derive(Serialize)]
struct Inner {
    z: u64,
    a: u64,
}
#[derive(Serialize)]
struct Outer {
    z: Vec<Inner>,
    a: u64,
}

#[test]
fn canonical_digest_nested_struct_and_object_share_legacy_golden() {
    let typed = Outer {
        z: vec![Inner { z: 3, a: 1 }],
        a: 2,
    };
    let object = json!({"z": [{"z": 3, "a": 1}], "a": 2});
    let reverse = json!({"a": 2, "z": [{"a": 1, "z": 3}]});
    const LEGACY_DEFAULT: &str =
        "sha256:4e7c6c4111ef328c3ae29a0401b303ead682d55d03e3010a4d883bd05b8c031e";
    for digest in [
        CanonicalDigest::of(&typed).unwrap(),
        CanonicalDigest::of(&object).unwrap(),
        CanonicalDigest::of(&reverse).unwrap(),
    ] {
        assert_eq!(digest.as_str(), LEGACY_DEFAULT);
    }
    assert_ne!(
        CanonicalDigest::of(&json!([1, 2])).unwrap(),
        CanonicalDigest::of(&json!([2, 1])).unwrap()
    );
}

#[test]
fn canonical_digest_raw_bytes_are_not_json_normalized() {
    let raw = br#"{"z":[{"z":3,"a":1}],"a":2}"#;
    assert_eq!(
        CanonicalDigest::of_bytes(raw).as_str(),
        "sha256:e7bca2bb08ac83902bd9d9db9e00e78d162befdb6f2f63d423b2e31bcae26214"
    );
    assert_ne!(
        CanonicalDigest::of_bytes(raw),
        CanonicalDigest::of(&serde_json::from_slice::<serde_json::Value>(raw).unwrap()).unwrap()
    );
}
