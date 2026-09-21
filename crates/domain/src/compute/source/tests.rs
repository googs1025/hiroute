use super::*;

#[test]
fn source_identity_digest_is_stable_across_evidence_and_revision_refresh() {
    let identity = SourceIdentityV1 {
        identity_revision: 1,
        provider_platform_id: "provider.fixture".into(),
        service_offering_id: "offering.fixture".into(),
        entitlement_id: "entitlement.fixture".into(),
        usage_scope: "account".into(),
        endpoint_profile_id: "endpoint.fixture".into(),
        endpoint_profile_revision: 1,
        region_id: "test".into(),
        account_subject_ref: "account.fixture".into(),
        evidence_refs: vec![CanonicalDigest::of_bytes(b"scanner-v1")],
    };
    let mut refreshed = identity.clone();
    refreshed.identity_revision = 2;
    refreshed.endpoint_profile_revision = 2;
    refreshed.evidence_refs = vec![CanonicalDigest::of_bytes(b"scanner-v2")];

    assert_eq!(identity.digest().unwrap(), refreshed.digest().unwrap());

    refreshed.entitlement_id = "entitlement.changed".into();
    assert_ne!(identity.digest().unwrap(), refreshed.digest().unwrap());
}
