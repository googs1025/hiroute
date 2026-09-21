use hiroute_domain::{
    CanonicalDigest, ComputeContractError, ConnectorRegistryBundleV1, RatingV1,
    ReleaseModelDataBundleV2,
};

fn current() -> (ConnectorRegistryBundleV1, ReleaseModelDataBundleV2) {
    (
        serde_json::from_slice(include_bytes!(
            "../../../../assets/release-facts/current/bundle/connector-registry.json"
        ))
        .unwrap(),
        serde_json::from_slice(include_bytes!(
            "../../../../assets/release-facts/current/bundle/model-data.json"
        ))
        .unwrap(),
    )
}

#[test]
fn current_release_model_data_has_one_configuration_scoped_rating_snapshot() {
    let (registry, value) = current();
    value.validate_against(&registry).unwrap();
    assert!(value.data.ratings.is_empty());
    assert_eq!(
        value.data.ratings_slice_version,
        value.rating_snapshot.version
    );
    assert_eq!(
        value.rating_snapshot.model_catalog_digest,
        CanonicalDigest::of(&value.data.models).unwrap()
    );
    assert_eq!(value.rating_snapshot.models.len(), value.data.models.len());
}

#[test]
fn current_release_model_data_rejects_mixed_model_level_ratings() {
    let (registry, mut value) = current();
    value.data.ratings.push(RatingV1 {
        model_configuration_id: value.data.models[0].model_configuration_id.clone(),
        overall_score_tenths: 35,
        rating_count: 1,
    });
    assert_eq!(
        value.validate_against(&registry).unwrap_err(),
        ComputeContractError::MixedReleaseSlice
    );
}

#[test]
fn current_release_model_data_rejects_rating_identity_and_digest_tampering() {
    let (registry, mut value) = current();
    value.rating_snapshot.model_catalog_digest = CanonicalDigest::of_bytes(b"wrong-models");
    value.rating_snapshot.digest = value.rating_snapshot.computed_digest().unwrap();
    assert_eq!(
        value.validate_against(&registry).unwrap_err(),
        ComputeContractError::CrossReference
    );

    let (registry, mut value) = current();
    value.rating_snapshot.digest = CanonicalDigest::of_bytes(b"wrong-snapshot");
    assert_eq!(
        value.validate_against(&registry).unwrap_err(),
        ComputeContractError::InvalidEvidence
    );
}
