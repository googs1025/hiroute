use crate::fixture::*;

#[test]
fn bootstrap_matches_normalized_host_and_preserves_query() {
    let envelope = envelope(1, 1);
    let uri: Uri = "/v1/chat?secret=kept-on-wire".parse().unwrap();
    let matched = match_bootstrap_request(
        &envelope.ingress_plan_handle,
        &uri,
        Some("Example.COM:8080"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        matched.original_path_and_query.as_ref(),
        "/v1/chat?secret=kept-on-wire"
    );

    let miss: Uri = "/other".parse().unwrap();
    let counter = NetworkUseCounter::default();
    assert!(
        match_bootstrap_request(&envelope.ingress_plan_handle, &miss, Some("example.com"))
            .unwrap()
            .is_none()
    );
    assert_eq!(counter.connections(), 0, "local 404 must not connect");
}

#[test]
fn bootstrap_matcher_uses_segment_boundaries_and_longest_prefix() {
    let address = "127.0.0.1:18080".parse().unwrap();
    let envelope = BootstrapPublicationBuilder::new(10, 10)
        .route("example.com", "/v1", 1, plain_target(address, 1))
        .unwrap()
        .route("example.com", "/v1/chat", 2, plain_target(address, 2))
        .unwrap()
        .route("example.com", "/v10", 3, plain_target(address, 3))
        .unwrap()
        .build()
        .unwrap();

    let longest: Uri = "/v1/chat/completions".parse().unwrap();
    let matched =
        match_bootstrap_request(&envelope.ingress_plan_handle, &longest, Some("example.com"))
            .unwrap()
            .unwrap();
    assert_eq!(matched.binding.local_id(), 2);

    let distinct_segment: Uri = "/v10/models".parse().unwrap();
    let matched = match_bootstrap_request(
        &envelope.ingress_plan_handle,
        &distinct_segment,
        Some("example.com"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(matched.binding.local_id(), 3);

    let false_prefix: Uri = "/v100".parse().unwrap();
    assert!(
        match_bootstrap_request(
            &envelope.ingress_plan_handle,
            &false_prefix,
            Some("example.com"),
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn bootstrap_listener_and_tls_contract_fail_closed() {
    let remote = BootstrapListenerConfig {
        bind: "0.0.0.0:8080".parse().unwrap(),
        allow_non_loopback: false,
        downstream_tls: None,
    };
    assert!(remote.validate().is_err());

    let tls = BootstrapListenerConfig {
        downstream_tls: Some(Arc::from(&b"certificate"[..])),
        ..BootstrapListenerConfig::default()
    };
    assert!(tls.validate().is_err());

    tls_target(address(443), "api.example.com", "api.example.com", 19)
        .validate()
        .unwrap();
}
