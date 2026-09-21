use super::*;

#[test]
fn fixed_same_name_concurrent_grants_keep_bindings_and_credentials_isolated() {
    let first = NativeProvider::start(vec![
        ProviderReply::Complete {
            status: 200,
            error_kind: None,
            body: RESPONSES_OK,
        };
        2
    ]);
    let second = NativeProvider::start(vec![
        ProviderReply::Complete {
            status: 200,
            error_kind: None,
            body: FALLBACK_OK,
        };
        2
    ]);
    let fixture = RuntimeFixture::launch_isolated_fixed_grants(&[&first, &second]);
    let address = fixture.address;
    for order in [[0, 1], [1, 0]] {
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let handles = order.map(|index| {
                let barrier = &barrier;
                scope.spawn(move || {
                    let token = format!("isolated-token-{index}");
                    barrier.wait();
                    let response = request(
                        address,
                        "POST",
                        "/v1/responses",
                        &[("X-HiRoute-Token", token.as_str())],
                        br#"{"model":"runtime-model","input":"same name"}"#,
                    );
                    assert_eq!(response.status, 200);
                    let document: serde_json::Value =
                        serde_json::from_slice(&response.body).unwrap();
                    assert_eq!(
                        document["id"],
                        if index == 0 {
                            "accepted"
                        } else {
                            "accepted-fallback"
                        }
                    );
                })
            });
            for handle in handles {
                handle.join().unwrap();
            }
        });
    }
    for index in 0..2 {
        let token = format!("isolated-token-{index}");
        let body = format!(
            r#"{{"model":"private-model-{}","input":"denied"}}"#,
            1 - index
        );
        let response = request(
            address,
            "POST",
            "/v1/responses",
            &[("X-HiRoute-Token", token.as_str())],
            body.as_bytes(),
        );
        assert_eq!(response.status, 404);
    }
    assert_eq!((first.calls(), second.calls()), (2, 2));
    for (index, provider) in [&first, &second].into_iter().enumerate() {
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        for bytes in requests {
            let wire = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
            assert!(wire.contains(&format!(
                "authorization: bearer provider-secret-{}",
                index + 1
            )));
            assert!(wire.contains(&format!("runtime-native-model-{}", index + 1)));
            assert!(!wire.contains(&format!("provider-secret-{}", 2 - index)));
            assert!(!wire.contains("isolated-token"));
            assert!(!wire.contains("x-hiroute-token"));
        }
    }
}

#[test]
fn fixed_failure_auth_does_not_cross_source_or_plan() {
    assert_fixed_failure_isolation(
        ProviderReply::Complete {
            status: 401,
            error_kind: None,
            body: AUTH_ERROR,
        },
        3,
        2,
        None,
        401,
    );
}

#[test]
fn fixed_failure_quota_does_not_cross_source_or_plan() {
    assert_fixed_failure_isolation(
        ProviderReply::Complete {
            status: 429,
            error_kind: None,
            body: QUOTA_ERROR,
        },
        3,
        2,
        None,
        429,
    );
}

#[test]
fn fixed_failure_overload_does_not_cross_source_or_plan() {
    assert_fixed_failure_isolation(
        ProviderReply::Complete {
            status: 429,
            error_kind: None,
            body: OVERLOAD_ERROR,
        },
        3,
        1,
        None,
        429,
    );
}

#[test]
fn fixed_failure_budget_exhaustion_does_not_cross_source_or_plan() {
    assert_fixed_failure_isolation(
        ProviderReply::Complete {
            status: 429,
            error_kind: None,
            body: QUOTA_ERROR,
        },
        1,
        1,
        None,
        502,
    );
}

#[test]
fn fixed_failure_timeout_does_not_cross_source_or_plan() {
    assert_fixed_failure_isolation(
        ProviderReply::Stall {
            duration: Duration::from_millis(500),
        },
        3,
        1,
        Some(200),
        502,
    );
}

fn assert_fixed_failure_isolation(
    reply: ProviderReply,
    attempts: u32,
    expected_calls: usize,
    timeout: Option<u64>,
    expected_status: u16,
) {
    let primary = NativeProvider::start(vec![reply.clone(), reply]);
    let other_account = NativeProvider::start(vec![ProviderReply::Complete {
        status: 200,
        error_kind: None,
        body: FALLBACK_OK,
    }]);
    let other_model = NativeProvider::start(vec![ProviderReply::Complete {
        status: 200,
        error_kind: None,
        body: FALLBACK_OK,
    }]);
    let fixture = RuntimeFixture::launch_fixed(
        &[&primary, &other_account, &other_model],
        attempts,
        &[2, 1, 1],
        timeout,
    );
    let response = fixture.request();
    assert_eq!(
        response.status,
        expected_status,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    assert_eq!(primary.calls(), expected_calls);
    assert_eq!((other_account.calls(), other_model.calls()), (0, 0));
    assert!(!String::from_utf8_lossy(&response.body).contains("private"));
    let alternate = fixture.request_body(
        format!(r#"{{"model":"{MODEL}-alternate-plan","input":"explicit alternate request"}}"#)
            .as_bytes(),
    );
    assert_eq!(
        alternate.status, 200,
        "the other plan remains independently callable"
    );
    assert_eq!((other_account.calls(), other_model.calls()), (1, 0));
}
