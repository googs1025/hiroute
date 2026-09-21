use crate::fixture::*;

#[test]
fn caller_driven_driver_never_reruns_logical_scope_for_fallback() {
    let scopes = ScopeSupervisor::default();
    let mut driver = LogicalRequestDriver::new(scopes.clone()).unwrap();
    driver.bind_publication().unwrap();
    driver.finish_logical_filters().unwrap();
    driver.begin_attempt().unwrap();
    driver.disposition_pending().unwrap();
    driver.continue_after_attempt().unwrap();
    driver.begin_attempt().unwrap();
    driver.disposition_pending().unwrap();
    driver.select_final_response().unwrap();
    driver.begin_accepted_response().unwrap();
    assert_eq!(driver.state(), RequestState::AcceptedResponseActive);
    driver.complete();
    let counts = scopes.counts();
    assert_eq!(counts.logical_finalized, 1);
    assert_eq!(counts.attempts_started, 2);
    assert_eq!(counts.attempts_finalized, 2);
    assert_eq!(counts.accepted_started, 1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn arbitrary_fallback_chain_finalizes_every_scope_exactly_once(attempts in 1_usize..32) {
        let scopes = ScopeSupervisor::default();
        let mut driver = LogicalRequestDriver::new(scopes.clone()).unwrap();
        driver.bind_publication().unwrap();
        driver.finish_logical_filters().unwrap();
        for index in 0..attempts {
            driver.begin_attempt().unwrap();
            driver.disposition_pending().unwrap();
            if index + 1 == attempts {
                driver.select_final_response().unwrap();
                driver.begin_accepted_response().unwrap();
            } else {
                driver.continue_after_attempt().unwrap();
            }
        }
        driver.complete();
        let counts = scopes.counts();
        prop_assert_eq!(counts.logical_finalized, 1);
        prop_assert_eq!(counts.attempts_started, attempts);
        prop_assert_eq!(counts.attempts_finalized, attempts);
        prop_assert_eq!(counts.accepted_started, 1);
        prop_assert_eq!(counts.accepted_finalized, 1);
    }
}
