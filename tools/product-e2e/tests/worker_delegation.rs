#![cfg(unix)]

mod worker_product_support;

#[test]
#[ignore = "requires explicitly selected Codex and Claude ACP installations"]
fn real_worker_instance_lifecycle_covers_both_harnesses() {
    worker_product_support::run_real_worker_scenario(
        "HIROUTE_PRODUCT_LIFECYCLE_ROUNDTRIP",
        "A-real-worker-instance-lifecycle",
    );
}
