use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hiroute_gateway_core::runtime::attempt::{
    AttemptError, AttemptRequestBodyReader, PreparedAttemptBody,
};
use hiroute_gateway_core::runtime::body::{
    BudgetTree, ChargedBytes, ChargedBytesBuilder, MemoryRole, RequestLeaseBook, StreamBudget,
};

#[test]
fn charged_builder_accounts_retained_capacity_before_allocation() {
    let tree = BudgetTree::new(64, 64).expect("tree");
    let budget = tree.stream(64).expect("budget");
    let mut builder =
        ChargedBytesBuilder::new(&budget, MemoryRole::AttemptWire, 32).expect("reserve builder");
    assert_eq!(budget.snapshot().expect("snapshot").live, 32);
    assert_eq!(builder.capacity(), 32);
    builder
        .resize_zeroed(32)
        .expect("materialize admitted buffer");
    builder.as_mut_slice()[..4].copy_from_slice(b"body");
    builder.truncate(4);
    let charged = builder.finish();
    assert_eq!(charged.bytes().as_ref(), b"body");
    assert_eq!(charged.retained_capacity(), 32);
    drop(charged);
    assert_eq!(budget.snapshot().expect("snapshot").live, 0);
}

#[test]
fn tracked_transport_clones_keep_the_charge_until_the_last_owner() {
    let tree = BudgetTree::new(64, 64).expect("tree");
    let budget = tree.stream(64).expect("budget");
    let bytes = ChargedBytes::copy_from_opaque(
        &budget,
        MemoryRole::AttemptWire,
        b"bounded transport quantum",
    )
    .expect("charged bytes")
    .into_tracked_bytes();
    let clone = bytes.clone();
    assert_ne!(budget.snapshot().expect("snapshot").live, 0);
    drop(bytes);
    assert_ne!(budget.snapshot().expect("snapshot").live, 0);
    drop(clone);
    assert_eq!(budget.snapshot().expect("snapshot").live, 0);
}

#[test]
fn sequential_attempt_body_accepts_only_a_nonzero_bounded_quantum() {
    let tree = BudgetTree::new(256, 256).expect("tree");
    let budget = tree.stream(256).expect("budget");
    let leases = RequestLeaseBook::new();
    let released = Arc::new(AtomicBool::new(false));
    let body = PreparedAttemptBody::from_reader(
        Box::new(StubReader {
            budget: budget.clone(),
            max_chunk: 8,
            remaining: 17,
            released: Arc::clone(&released),
        }),
        leases.acquire().expect("lease"),
    )
    .expect("sequential body");
    assert_eq!(body.visible_bytes(), 17);
    assert!(!body.is_empty());
    drop(body);

    let error = PreparedAttemptBody::from_reader(
        Box::new(StubReader {
            budget,
            max_chunk: 0,
            remaining: 1,
            released,
        }),
        leases.acquire().expect("lease"),
    )
    .expect_err("zero quantum must fail");
    assert!(matches!(
        error,
        AttemptError::RequestChunkExceedsWriteQuantum
    ));
}

#[derive(Debug)]
struct StubReader {
    budget: StreamBudget,
    max_chunk: usize,
    remaining: usize,
    released: Arc<AtomicBool>,
}

impl AttemptRequestBodyReader for StubReader {
    fn visible_bytes(&self) -> usize {
        self.remaining
    }

    fn max_chunk_bytes(&self) -> usize {
        self.max_chunk
    }

    fn next_chunk(&mut self) -> Result<Option<ChargedBytes>, AttemptError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let count = self.remaining.min(self.max_chunk);
        self.remaining -= count;
        Ok(Some(ChargedBytes::copy_from_opaque(
            &self.budget,
            MemoryRole::AttemptWire,
            &vec![0_u8; count],
        )?))
    }

    fn release(&mut self) {
        self.remaining = 0;
        self.released.store(true, Ordering::Release);
    }
}
