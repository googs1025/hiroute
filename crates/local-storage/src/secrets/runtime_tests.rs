use crate::test_tempdir as tempdir;
use hiroute_domain::{
    CanonicalDigest, ConnectorRuntimeKind, CredentialRefV1, GatewayAuthenticationSemanticsV1,
    GatewayOperationalTargetV1, HEADER_SECRET_LEASE_REQUEST_SCHEMA_V1, HeaderSecretLeaseRequestV1,
    NATIVE_CREDENTIAL_LEASE_REQUEST_SCHEMA_V1, NativeCredentialAuthorityV1,
    NativeCredentialCapabilityErrorV1, NativeCredentialLeaseRequestV1, NativeCredentialLeaseV1,
    OperationId, PortErrorCode, PortResult, ProtectedSecret, SecretMutationV1, SecretStorePort,
    SensitiveAuthorizationTargetV1, UpstreamProtocol,
};

use super::{LocalNativeCredentialAuthority, LocalSecretStore};

fn seeded_authority() -> (tempfile::TempDir, LocalNativeCredentialAuthority) {
    let directory = tempdir().unwrap();
    let root = directory.path().join("data");
    let store = LocalSecretStore::open(
        &crate::test_storage_authority(),
        root.join("secrets.db"),
        root.join("master-key"),
        root.join("backups"),
    )
    .unwrap();
    let reference = CredentialRefV1::new(
        "credential/claude-primary",
        "source/native-claude",
        "hirouted",
        "provider-auth",
        ["connection-option/claude.custom.v1".into()],
        0,
    )
    .unwrap();
    let secret = ProtectedSecret::new(b"native-secret-sentinel".to_vec()).unwrap();
    let mutation = SecretMutationV1::upsert(
        reference,
        0,
        "claude-primary",
        Some(store.fingerprint(&secret).unwrap()),
    )
    .unwrap();
    let effect = store
        .apply_secret(
            &OperationId::parse("op_0123456789abcdef0123456789abcdef").unwrap(),
            &mutation,
            Some(&secret),
        )
        .unwrap();
    store.activate_secret(&effect).unwrap();
    (directory, LocalNativeCredentialAuthority::new(store))
}

fn request() -> NativeCredentialLeaseRequestV1 {
    let target = GatewayOperationalTargetV1::RegisteredHttps {
        uri: "https://api.anthropic.com/v1/messages".into(),
    };
    NativeCredentialLeaseRequestV1 {
        schema_version: NATIVE_CREDENTIAL_LEASE_REQUEST_SCHEMA_V1.into(),
        stable_binding_id: "binding/claude-primary".into(),
        credential_id: "credential/claude-primary".into(),
        credential_destination_ref: "connection-option/claude.custom.v1".into(),
        excluded_key_ids: Vec::new(),
        connector_runtime: ConnectorRuntimeKind::BuiltinNative,
        connector_id: "builtin-anthropic".into(),
        upstream_protocol: UpstreamProtocol::Messages,
        upstream_model_id: "claude-sonnet".into(),
        native_transport_model: "claude-sonnet".into(),
        logical_endpoint: target.uri().into(),
        operational_target_digest: CanonicalDigest::of(&target).unwrap(),
        operational_target: target,
        request_path: "/v1/messages".into(),
        runtime_epoch: None,
        target_epoch: None,
        protocol_profile_digest: CanonicalDigest::of_bytes(b"claude-profile"),
        authentication: GatewayAuthenticationSemanticsV1::Bearer,
    }
}

fn classifier_request() -> HeaderSecretLeaseRequestV1 {
    HeaderSecretLeaseRequestV1 {
        schema_version: HEADER_SECRET_LEASE_REQUEST_SCHEMA_V1.into(),
        secret_id: "classifier/main".into(),
        header_name: "authorization".into(),
    }
}

fn seeded_classifier_authority() -> (tempfile::TempDir, LocalNativeCredentialAuthority) {
    let directory = tempdir().unwrap();
    let root = directory.path().join("data");
    let store = LocalSecretStore::open(
        &crate::test_storage_authority(),
        root.join("secrets.db"),
        root.join("master-key"),
        root.join("backups"),
    )
    .unwrap();
    let reference = CredentialRefV1::new(
        "classifier/main",
        "personal/default",
        "hirouted",
        "http-header",
        std::iter::empty(),
        0,
    )
    .unwrap();
    let secret = ProtectedSecret::new(b"classifier-secret-sentinel".to_vec()).unwrap();
    let mutation = SecretMutationV1::upsert(
        reference,
        0,
        "classifier-main",
        Some(store.fingerprint(&secret).unwrap()),
    )
    .unwrap();
    let effect = store
        .apply_secret(
            &OperationId::parse("op_1123456789abcdef0123456789abcdef").unwrap(),
            &mutation,
            Some(&secret),
        )
        .unwrap();
    store.activate_secret(&effect).unwrap();
    (directory, LocalNativeCredentialAuthority::new(store))
}

#[test]
fn native_authority_resolves_opaque_id_and_applies_only_sensitive_bearer() {
    let (_directory, authority) = seeded_authority();
    let lease = authority
        .lease_native_credential(&request())
        .unwrap()
        .unwrap();
    assert_eq!(lease.credential_id(), "credential/claude-primary");
    assert_eq!(lease.key_id(), "credential/claude-primary");
    assert_eq!(lease.generation(), 1);
    let mut target = Capture::default();
    lease.apply_authorization(&mut target).unwrap();
    assert_eq!(
        target.values,
        vec![b"Bearer native-secret-sentinel".to_vec()]
    );
}

#[test]
fn native_authority_applies_the_exact_named_api_key_header() {
    let (_directory, authority) = seeded_authority();
    let mut header_request = request();
    header_request.authentication = GatewayAuthenticationSemanticsV1::ApiKeyHeader {
        header: "x-provider-key".into(),
    };
    let lease = authority
        .lease_native_credential(&header_request)
        .unwrap()
        .unwrap();
    let mut target = Capture::default();
    lease.apply_authorization(&mut target).unwrap();
    assert!(target.values.is_empty());
    assert_eq!(
        target.headers,
        vec![(
            "x-provider-key".to_owned(),
            b"native-secret-sentinel".to_vec()
        )]
    );
}

#[test]
fn native_authority_rejects_wrong_facts_before_resolution_and_honors_exclusion() {
    let (_directory, authority) = seeded_authority();

    let mut wrong_digest = request();
    wrong_digest.operational_target_digest = CanonicalDigest::of_bytes(b"wrong");
    assert_eq!(
        lease_error_code(authority.lease_native_credential(&wrong_digest)),
        PortErrorCode::InvalidData
    );

    let mut wrong_destination = request();
    wrong_destination.credential_destination_ref = "connection-option/other".into();
    assert_eq!(
        lease_error_code(authority.lease_native_credential(&wrong_destination)),
        PortErrorCode::PermissionDenied
    );

    let mut unknown = request();
    unknown.credential_id = "credential/unknown".into();
    assert_eq!(
        lease_error_code(authority.lease_native_credential(&unknown)),
        PortErrorCode::NotFound
    );

    let mut excluded = request();
    excluded.excluded_key_ids = vec!["credential/claude-primary".into()];
    assert!(
        authority
            .lease_native_credential(&excluded)
            .unwrap()
            .is_none()
    );
}

#[test]
fn header_secret_authority_applies_the_configured_header_without_endpoint_binding() {
    let (_directory, authority) = seeded_classifier_authority();
    let lease = authority
        .lease_header_secret(&classifier_request())
        .unwrap()
        .unwrap();
    assert_eq!(lease.credential_id(), "classifier/main");
    let mut target = Capture::default();
    lease.apply_authorization(&mut target).unwrap();
    assert!(target.values.is_empty());
    assert_eq!(
        target.headers,
        vec![(
            "authorization".into(),
            b"classifier-secret-sentinel".to_vec()
        )]
    );

    let mut forbidden = classifier_request();
    forbidden.header_name = "host".into();
    assert_eq!(
        lease_error_code(authority.lease_header_secret(&forbidden)),
        PortErrorCode::InvalidData
    );

    let (_directory, provider_authority) = seeded_authority();
    let mut provider_secret = classifier_request();
    provider_secret.secret_id = "credential/claude-primary".into();
    assert_eq!(
        lease_error_code(provider_authority.lease_header_secret(&provider_secret)),
        PortErrorCode::PermissionDenied
    );
}

#[derive(Default)]
struct Capture {
    values: Vec<Vec<u8>>,
    headers: Vec<(String, Vec<u8>)>,
}

impl SensitiveAuthorizationTargetV1 for Capture {
    fn set_sensitive_authorization(
        &mut self,
        value: &[u8],
    ) -> Result<(), NativeCredentialCapabilityErrorV1> {
        self.values.push(value.to_vec());
        Ok(())
    }

    fn set_sensitive_header(
        &mut self,
        name: &str,
        value: &[u8],
    ) -> Result<(), NativeCredentialCapabilityErrorV1> {
        self.headers.push((name.to_owned(), value.to_vec()));
        Ok(())
    }
}

#[test]
fn frozen_native_request_rejects_duplicate_exclusions_and_non_bearer_auth() {
    let mut duplicate = request();
    duplicate.excluded_key_ids = vec!["key/a".into(), "key/a".into()];
    assert!(duplicate.validate().is_err());

    let mut auth = request();
    auth.authentication = GatewayAuthenticationSemanticsV1::None;
    assert!(auth.validate().is_err());
}

fn lease_error_code(result: PortResult<Option<NativeCredentialLeaseV1>>) -> PortErrorCode {
    match result {
        Err(error) => error.code,
        Ok(_) => panic!("expected credential authority rejection"),
    }
}
