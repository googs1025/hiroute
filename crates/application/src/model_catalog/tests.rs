use super::*;
use hiroute_application_api::ModelRatingQueryItemV1;
use hiroute_domain::*;
fn snapshot(version: &str) -> RatingSnapshotV2 {
    let mut s = RatingSnapshotV2 {
        schema: RATING_SNAPSHOT_SCHEMA_V2.into(),
        version: version.into(),
        scale_version: "test-scale".into(),
        model_catalog_digest: CanonicalDigest::of_bytes(b"test"),
        models: vec![ModelNativeReasoningV1 {
            native_render_convention: None,
            model_configuration_id: "model".into(),
            capability: NativeReasoningCapabilityV1::Fixed {
                profile: "standard".into(),
            },
        }],
        records: vec![],
        digest: CanonicalDigest::of_bytes(b"pending"),
    };
    s.digest = s.computed_digest().unwrap();
    s
}
fn query() -> ResolveModelRatingsV1 {
    ResolveModelRatingsV1 {
        snapshot: RatingSnapshotSelectionV1::Latest,
        items: vec![ModelRatingQueryItemV1 {
            query_id: "one".into(),
            model_configuration_id: "model".into(),
            exact_native_reasoning: ExactNativeReasoningV1::Fixed {
                profile: "standard".into(),
                render_mode: ReasoningRenderModeV1::NoControlParameter,
            },
        }],
    }
}
#[test]
fn ratings_batch_version_is_explicit_and_missing_version_never_falls_back() {
    let service = ModelRatings::default();
    service.install(snapshot("v1")).unwrap();
    let old = service.resolve(&query()).unwrap();
    service.install(snapshot("v2")).unwrap();
    assert_eq!(old.snapshot_ref.version, "v1");
    assert_eq!(
        service.resolve(&query()).unwrap().snapshot_ref.version,
        "v2"
    );
    let mut q = query();
    q.snapshot = RatingSnapshotSelectionV1::Version {
        version: "v1".into(),
    };
    assert_eq!(
        service.resolve(&q),
        Err(RatingQueryError::SnapshotUnavailable)
    );
    q = query();
    q.items.push(q.items[0].clone());
    assert_eq!(service.resolve(&q), Err(RatingQueryError::InvalidArguments));
    let mut altered = snapshot("v2");
    altered.scale_version = "other".into();
    altered.digest = altered.computed_digest().unwrap();
    assert!(service.install(altered).is_err());
}
#[test]
fn ratings_batch_update_does_not_mix_snapshot_or_reorder_queries() {
    let service = Arc::new(ModelRatings::default());
    service.install(snapshot("v0")).unwrap();
    let writer = service.clone();
    let t = std::thread::spawn(move || {
        for i in 1..40 {
            writer.install(snapshot(&format!("v{i}"))).unwrap();
        }
    });
    let mut q = query();
    q.items = (0..MAX_RATING_QUERY_ITEMS)
        .map(|i| ModelRatingQueryItemV1 {
            query_id: format!("q{i}"),
            ..q.items[0].clone()
        })
        .collect();
    for _ in 0..20 {
        let r = service.resolve(&q).unwrap();
        assert_eq!(r.items.len(), MAX_RATING_QUERY_ITEMS);
        assert_eq!(
            r.snapshot_ref.digest,
            snapshot(&r.snapshot_ref.version).digest
        );
        for (i, item) in r.items.iter().enumerate() {
            assert_eq!(item.query_id, format!("q{i}"));
        }
    }
    t.join().unwrap();
}
