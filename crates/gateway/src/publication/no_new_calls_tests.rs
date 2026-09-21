//! Exercise the production installer and authority, including persisted restoration.
use super::tests::{TestDirectory, publish, snapshot};
use super::*;
use crate::server::dispatch::GatewayRequestAuthority;
use crate::server::request_plan::IngressProtocol;
use std::sync::Arc;

fn deny(revision: u64) -> GatewayPublicationSnapshotV3 {
    let projection = hiroute_domain::GatewayPublicationSnapshotProjectionV3::no_new_calls(
        "personal/default".into(),
        "authority".into(),
        1,
        revision,
        "renderer-1".into(),
    )
    .unwrap();
    let value: GatewayPublicationSnapshotV3 =
        serde_json::from_value(serde_json::to_value(projection).unwrap()).unwrap();
    value.validate().unwrap();
    value
}

#[test]
fn no_new_calls_installs_identified_state_and_restores_without_reviving_aliases() {
    let directory = TestDirectory::new();
    let path = directory.path().join("publication.json");
    let installer = Arc::new(GatewayPublicationInstaller::open(&path).unwrap());
    publish(&installer, snapshot(1, "renderer-1"));
    let old = installer.active().unwrap();
    let denied = deny(2);
    let digest = denied.payload_digest.clone();
    publish(&installer, denied);
    let current = installer.active().unwrap();
    assert_eq!(current.publication_revision(), 2);
    assert_eq!(current.payload_digest(), digest);
    assert!(current.authenticate_bearer("Bearer token-alpha").is_none());
    assert!(current.agent_plan_revision("alpha").is_none());
    assert!(
        GatewayRequestAuthority::new(installer.clone())
            .authorize_bytes(
                IngressProtocol::Responses,
                Some("Bearer token-alpha"),
                br#"{"model":"alpha","input":"hello"}"#,
                std::time::Instant::now(),
            )
            .is_err()
    );
    // An existing pinned configuration remains an immutable value; it does not authorize new calls.
    assert_eq!(old.agent_plan_revision("alpha"), Some(10));
    drop(installer);
    let restored = GatewayPublicationInstaller::open(&path).unwrap();
    assert_eq!(restored.active().unwrap().payload_digest(), digest);
    assert!(
        restored
            .active()
            .unwrap()
            .authenticate_bearer("Bearer token-alpha")
            .is_none()
    );
    publish(&restored, snapshot(3, "renderer-1"));
    assert_eq!(
        restored.active().unwrap().agent_plan_revision("alpha"),
        Some(10)
    );
}

#[test]
fn empty_allow_nonempty_deny_and_missing_admission_are_rejected() {
    let mut empty_allow = snapshot(1, "renderer-1");
    empty_allow.aliases.clear();
    empty_allow.grants.clear();
    empty_allow.payload_digest = empty_allow.canonical_digest().unwrap();
    assert!(empty_allow.validate().is_err());
    let mut inconsistent = deny(2);
    inconsistent.aliases = snapshot(1, "renderer-1").aliases;
    inconsistent.payload_digest = inconsistent.canonical_digest().unwrap();
    assert!(inconsistent.validate().is_err());
    let mut missing_state = serde_json::to_value(deny(2)).unwrap();
    missing_state.as_object_mut().unwrap().remove("admission");
    assert!(serde_json::from_value::<GatewayPublicationSnapshotV3>(missing_state).is_err());
}
