#![cfg(unix)]

mod worker_product_support;

#[test]
#[ignore = "requires explicitly selected Codex and Claude ACP installations"]
fn real_worker_read_observes_both_harnesses_during_exec_and_continue() {
    worker_product_support::run_real_worker_scenario(
        "HIROUTE_PRODUCT_PROGRESS_ROUNDTRIP",
        "A-real-worker-read-roundtrip",
    );
}
