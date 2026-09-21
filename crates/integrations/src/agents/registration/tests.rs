use super::*;
use hiroute_domain::ReleaseModelDataBundleV2;

fn fixtures() -> (ConnectorRegistryBundleV1, ReleaseModelDataBundleV2) {
    (
        serde_json::from_slice(include_bytes!(
            "../../../../../assets/connector-registry/current/registry-seed.json"
        ))
        .unwrap(),
        serde_json::from_slice(include_bytes!(
            "../../../../../assets/release-facts/current/bundle/model-data.json"
        ))
        .unwrap(),
    )
}
#[test]
fn agent_registration_consumes_only_the_verified_current_model_payload() {
    let (registry, release) = fixtures();
    let index =
        ClaudeRegistrationIndexV1::from_verified_model_data(&registry, &release.data).unwrap();
    assert!(!index.entries.is_empty());

    let mut invalid = release.data;
    invalid.schema = "unknown".into();
    assert!(ClaudeRegistrationIndexV1::from_verified_model_data(&registry, &invalid).is_err());
}
#[test]
fn agent_registration_rejects_mixed_release_and_ambiguous_upstream_mapping() {
    let (registry, release) = fixtures();
    let mut mixed = release.data.clone();
    mixed.product_release = "another-release".into();
    assert!(ClaudeRegistrationIndexV1::from_verified_model_data(&registry, &mixed).is_err());
    let index =
        ClaudeRegistrationIndexV1::from_verified_model_data(&registry, &release.data).unwrap();
    let entry = index
        .entries
        .iter()
        .find(|entry| !entry.upstream_models.is_empty())
        .unwrap();
    let mut ambiguous = release.data;
    let duplicate = ambiguous
        .model_endpoint_capabilities
        .iter()
        .find(|capability| {
            capability.endpoint_profile_id == entry.endpoint_profile_id
                && entry
                    .upstream_models
                    .contains_key(&capability.upstream_model_id)
        })
        .unwrap()
        .clone();
    ambiguous.model_endpoint_capabilities.push(duplicate);
    assert!(ClaudeRegistrationIndexV1::from_verified_model_data(&registry, &ambiguous).is_err());
}
