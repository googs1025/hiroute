use std::sync::atomic::Ordering;

use hiroute_application_api::{
    ModelConnectionAuthenticationStatusV1, ModelConnectionReachabilityV1,
};
use hiroute_domain::{
    CanonicalDigest, FreeAccess, GatewayAuthenticationSemanticsV1, ProtectedSecret,
    UpstreamProtocol,
};
use hiroute_integrations::{
    ModelConnectionProbeCancellationV1, NativeConnectionProvenanceInputV1,
    NativeModelConnectionCredentialV1, NativeModelConnectionServiceV1,
    ReqwestModelDirectoryTransportV1,
};

use super::{
    draft,
    support::{ControlledServer, CountingTransport, TestOnlyComputeCandidatePort},
};

#[test]
fn custom_none_authentication_keeps_unknown_qualification_non_free() {
    let server = ControlledServer::start(vec![(
        200,
        Vec::new(),
        r#"{"data":[{"id":"manual-model"}]}"#.into(),
    )]);
    let service = NativeModelConnectionServiceV1::new(
        TestOnlyComputeCandidatePort::new(),
        ReqwestModelDirectoryTransportV1,
    );
    let mut custom = draft(
        server.base_url.clone(),
        GatewayAuthenticationSemanticsV1::None,
    );
    custom.qualification.free_access = None;
    custom.qualification.evidence_ref = None;
    let result = service
        .check(
            custom,
            NativeModelConnectionCredentialV1::NotRequired,
            &ModelConnectionProbeCancellationV1::default(),
        )
        .unwrap();

    assert_eq!(
        result.authentication,
        ModelConnectionAuthenticationStatusV1::NotRequired
    );
    let request = &server.finish()[0].to_ascii_lowercase();
    assert!(!request.contains("authorization:"));
    assert!(!request.contains("x-api-key:"));

    let transport = CountingTransport::default();
    let calls = transport.calls();
    let invalid_service =
        NativeModelConnectionServiceV1::new(TestOnlyComputeCandidatePort::new(), transport);
    let mut unproved_free = draft(
        "http://127.0.0.1:9/v1".into(),
        GatewayAuthenticationSemanticsV1::None,
    );
    unproved_free.qualification = super::NativeConnectionQualificationV1 {
        free_access: Some(FreeAccess::Direct),
        evidence_ref: None,
    };
    assert!(
        invalid_service
            .check(
                unproved_free,
                NativeModelConnectionCredentialV1::NotRequired,
                &ModelConnectionProbeCancellationV1::default(),
            )
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn configured_messages_profile_adds_only_its_required_version_header() {
    let server = ControlledServer::start(vec![(
        200,
        Vec::new(),
        r#"{"data":[{"id":"manual-model"}]}"#.into(),
    )]);
    let secret = ProtectedSecret::new(b"messages-key".to_vec()).unwrap();
    let service = NativeModelConnectionServiceV1::new(
        TestOnlyComputeCandidatePort::new(),
        ReqwestModelDirectoryTransportV1,
    );
    let mut configured = draft(
        server.base_url.clone(),
        GatewayAuthenticationSemanticsV1::ApiKeyHeader {
            header: "x-api-key".into(),
        },
    );
    configured.protocol = UpstreamProtocol::Messages;
    configured.protocol_profile_id = "adapter.anthropic-messages.v1".into();
    configured.protocol_header_semantics.required_headers =
        vec![("anthropic-version".into(), "2023-06-01".into())];
    configured.provenance = NativeConnectionProvenanceInputV1::Registered {
        connection_option_id: "builtin.messages.test".into(),
        registry_version: "test-registry-v1".into(),
        catalog_digest: CanonicalDigest::of_bytes(b"test-catalog"),
    };
    let result = service
        .check(
            configured,
            NativeModelConnectionCredentialV1::Protected {
                descriptor: hiroute_application::compute_management::ProtectedInputSourceDescriptorV1::ManualInput,
                input_slot: "input/native/messages".into(),
                secret: &secret,
            },
            &ModelConnectionProbeCancellationV1::default(),
        )
        .unwrap();
    assert_eq!(
        result.reachability,
        ModelConnectionReachabilityV1::Reachable
    );

    let request = &server.finish()[0].to_ascii_lowercase();
    assert!(request.contains("anthropic-version: 2023-06-01\r\n"));
    assert!(request.contains("x-api-key: messages-key\r\n"));
    assert!(!request.contains("authorization:"));
}

#[test]
fn custom_messages_profile_does_not_gain_anthropic_headers_and_unsafe_profile_headers_fail_closed()
{
    let server = ControlledServer::start(vec![(
        200,
        Vec::new(),
        r#"{"data":[{"id":"manual-model"}]}"#.into(),
    )]);
    let service = NativeModelConnectionServiceV1::new(
        TestOnlyComputeCandidatePort::new(),
        ReqwestModelDirectoryTransportV1,
    );
    let mut custom = draft(
        server.base_url.clone(),
        GatewayAuthenticationSemanticsV1::None,
    );
    custom.protocol = UpstreamProtocol::Messages;
    let result = service
        .check(
            custom,
            NativeModelConnectionCredentialV1::NotRequired,
            &ModelConnectionProbeCancellationV1::default(),
        )
        .unwrap();
    assert_eq!(
        result.reachability,
        ModelConnectionReachabilityV1::Reachable
    );
    assert!(
        !server.finish()[0]
            .to_ascii_lowercase()
            .contains("anthropic-version:")
    );

    let transport = CountingTransport::default();
    let calls = transport.calls();
    let unsafe_service =
        NativeModelConnectionServiceV1::new(TestOnlyComputeCandidatePort::new(), transport);
    let mut unsafe_draft = draft(
        "http://127.0.0.1:9/v1".into(),
        GatewayAuthenticationSemanticsV1::Bearer,
    );
    unsafe_draft.protocol = UpstreamProtocol::Messages;
    unsafe_draft.protocol_header_semantics.required_headers =
        vec![("host".into(), "attacker.example".into())];
    let secret = ProtectedSecret::new(b"never-sent".to_vec()).unwrap();
    assert!(
        unsafe_service
            .check(
                unsafe_draft,
                NativeModelConnectionCredentialV1::Protected {
                    descriptor: hiroute_application::compute_management::ProtectedInputSourceDescriptorV1::ManualInput,
                    input_slot: "input/native/unsafe".into(),
                    secret: &secret,
                },
                &ModelConnectionProbeCancellationV1::default(),
            )
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
