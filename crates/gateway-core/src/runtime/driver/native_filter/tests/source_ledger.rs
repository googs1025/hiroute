use super::*;

#[test]
fn accepted_source_ledger_tracks_only_live_promotions() {
    let tree = BudgetTree::new(4096, 4096).expect("budget tree");
    let budget = tree.stream(4096).expect("stream budget");
    let descriptor = CompiledFilterDescriptor::new("accepted", 2).expect("descriptor");
    let mut ledger =
        AcceptedFilterSourceLedger::new(std::slice::from_ref(&descriptor), &budget, None)
            .expect("accepted source ledger");
    let frame = AcceptedBodyFrame {
        output: None,
        end_stream: false,
        sse_sources: SseTransformSources::default(),
        queue_metadata: BodyMetadataOwner::default(),
    };
    let source = ledger.insert(7, &frame).expect("source insertion");
    let promoted = source.clone();

    drop(source);
    ledger.reclaim_dead();
    assert_eq!(ledger.len(), 1, "a promoted input keeps its metadata live");

    drop(promoted);
    ledger.reclaim_dead();
    assert!(
        ledger.len() == 0,
        "dropping the final promoted owner reclaims its metadata"
    );
    assert!(
        budget.snapshot().expect("ledger snapshot").live > 0,
        "the fixed-capacity registry remains conservatively charged"
    );
    drop(ledger);
    assert_eq!(budget.snapshot().expect("released ledger").live, 0);
}

#[test]
fn accepted_source_ledger_enforces_cumulative_output_across_batches() {
    let tree = BudgetTree::new(16 * 1024, 16 * 1024).expect("budget tree");
    let budget = tree.stream(16 * 1024).expect("stream budget");
    let descriptor = CompiledFilterDescriptor::new("accepted-expand", 8)
        .expect("descriptor")
        .with_capabilities(
            crate::core::filter::FilterCapabilities::observe_only().with_body_expansion(),
        );
    let plan = BodyPlan::SseFramedStreaming {
        max_event_bytes: 32,
        max_pending_bytes: 32,
        max_output_event_bytes: 32,
        expansion_ratio_numerator: 1,
        expansion_ratio_denominator: 1,
        expansion_slack_bytes: 0,
    };
    let mut ledger =
        AcceptedFilterSourceLedger::new(std::slice::from_ref(&descriptor), &budget, Some(&plan))
            .expect("accepted source ledger");
    let frame = AcceptedBodyFrame {
        output: None,
        end_stream: false,
        sse_sources: SseTransformSources::one(SseTransformSource {
            sequence: 1,
            source_bytes: 8,
            provenance: SemanticProvenance::ProducesSemantic,
        }),
        queue_metadata: BodyMetadataOwner::default(),
    };
    let source = ledger.insert(1, &frame).expect("source insertion");

    for _ in 0..2 {
        ledger.begin_batch();
        ledger
            .stage_output(std::slice::from_ref(&source), 4)
            .expect("stage within cumulative limit");
        ledger
            .commit_batch()
            .expect("commit within cumulative limit");
    }
    ledger.begin_batch();
    ledger
        .stage_output(std::slice::from_ref(&source), 4)
        .expect("staging is atomic and bounded");
    assert!(
        ledger
            .commit_batch()
            .unwrap_err()
            .contains("SSE output exceeds"),
        "a later callback must not receive a fresh expansion allowance"
    );
}

#[test]
fn accepted_source_ledger_precharges_and_caps_tiny_live_sources() {
    let tree = BudgetTree::new(4096, 4096).expect("budget tree");
    let budget = tree.stream(4096).expect("stream budget");
    let descriptor = CompiledFilterDescriptor::new("accepted-retain", 1).expect("descriptor");
    let mut ledger =
        AcceptedFilterSourceLedger::new(std::slice::from_ref(&descriptor), &budget, None)
            .expect("accepted source ledger");
    let reserved_live = budget.snapshot().expect("reserved ledger").live;
    let frame = AcceptedBodyFrame {
        output: None,
        end_stream: false,
        sse_sources: SseTransformSources::one(SseTransformSource {
            sequence: 1,
            source_bytes: 1,
            provenance: SemanticProvenance::NonSemantic,
        }),
        queue_metadata: BodyMetadataOwner::default(),
    };
    let first = ledger.insert(1, &frame).expect("first source");
    let second = ledger.insert(2, &frame).expect("second source");
    assert_eq!(
        budget.snapshot().expect("source snapshot").live,
        reserved_live,
        "all source-token and registry metadata was reserved before allocation"
    );
    assert!(
        ledger.insert(3, &frame).unwrap_err().contains("hard limit"),
        "the compiled live-source cap is enforced before another Arc allocation"
    );
    drop((first, second));
    ledger.reclaim_dead();
    assert_eq!(ledger.len(), 0);
}
