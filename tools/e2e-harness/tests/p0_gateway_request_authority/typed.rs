use super::*;

#[test]
fn typed_authority_pins_both_revisions_alias_budget_and_atomic_cutover() {
    let directory = tempfile::tempdir().unwrap();
    let lkg = directory.path().join("gateway-publication-lkg.json");
    let installer = Arc::new(GatewayPublicationInstaller::open(&lkg).unwrap());
    publish(&installer, snapshot(1, 11));
    let authority = GatewayRequestAuthority::new(Arc::clone(&installer));
    let now = Instant::now();

    let in_flight = authority
        .begin_at(
            IngressProtocol::Responses,
            Some("Bearer test-agent-token"),
            now,
        )
        .unwrap();

    // Cut over while this request is between grant authentication and its
    // bounded alias selector. That request must finish on the pinned old root.
    publish(&installer, snapshot(2, 12));
    let old_request = in_flight.authorize_alias("plan-fast", now).unwrap();
    assert_eq!(old_request.publication_revision(), 1);
    assert_eq!(old_request.agent_plan_revision(), Some(11));
    assert_eq!(old_request.max_attempts(), 1);
    assert_eq!(old_request.deadline(), now + Duration::from_millis(1_500));
    assert_eq!(old_request.candidates()[0].binding_local_id, 1);
    assert_eq!(
        old_request.candidates()[0].endpoint.as_ref(),
        "https://provider-1.invalid/v1/responses"
    );

    let new_request = authority
        .authorize_bytes(
            IngressProtocol::Responses,
            Some("Bearer test-agent-token"),
            br#"{"model":"plan-fast"}"#,
            now,
        )
        .unwrap();
    assert_eq!(new_request.publication_revision(), 2);
    assert_eq!(new_request.agent_plan_revision(), Some(12));
    assert_eq!(old_request.agent_plan_revision(), Some(11));

    let deep = authority
        .authorize_bytes(
            IngressProtocol::Responses,
            Some("Bearer test-agent-token"),
            br#"{"model":"plan-deep"}"#,
            now,
        )
        .unwrap();
    assert_eq!(deep.max_attempts(), 4);
    assert_eq!(deep.deadline(), now + Duration::from_secs(30));
}
