use super::*;
use crate::compiler::test_fixtures::{compilation_facts, custom_desired};

fn service(facts: &AgentPlanCompilationFactsV1) -> ModelRatings {
    let unknown = RatingValueV1::Unknown {
        reason: RatingUnknownReasonV1::RatingNotCollected,
    };
    let mut snapshot = RatingSnapshotV2 {
        schema: RATING_SNAPSHOT_SCHEMA_V2.into(),
        version: "ratings/test-v1".into(),
        scale_version: "stars-five/v1".into(),
        model_catalog_digest: CanonicalDigest::of_bytes(b"catalog"),
        models: facts
            .candidates
            .iter()
            .map(|f| ModelNativeReasoningV1 {
                native_render_convention: None,
                model_configuration_id: f.model.model_configuration_id.clone(),
                capability: f.reasoning.clone(),
            })
            .collect(),
        records: vec![
            NativeConfigurationRatingV2 {
                model_configuration_id: "model.free-a".into(),
                native_configuration: NativeRatingConfigurationV1::Profile {
                    profile: "high".into(),
                },
                overall: RatingValueV1::Reference {
                    score_tenths: 49,
                    evidence_ref: "evidence/high".into(),
                    method_revision: "method/v1".into(),
                },
                coding: unknown.clone(),
                tool: unknown.clone(),
            },
            NativeConfigurationRatingV2 {
                model_configuration_id: "model.free-a".into(),
                native_configuration: NativeRatingConfigurationV1::Profile {
                    profile: "low".into(),
                },
                overall: RatingValueV1::Estimated {
                    score_tenths: 20,
                    evidence_ref: "evidence/low".into(),
                    method_revision: "method/v1".into(),
                },
                coding: unknown.clone(),
                tool: unknown.clone(),
            },
            NativeConfigurationRatingV2 {
                model_configuration_id: "model.free-b".into(),
                native_configuration: NativeRatingConfigurationV1::Fixed {
                    profile: "fixed".into(),
                },
                overall: RatingValueV1::Reference {
                    score_tenths: 30,
                    evidence_ref: "evidence/fixed".into(),
                    method_revision: "method/v1".into(),
                },
                coding: unknown.clone(),
                tool: unknown,
            },
        ],
        digest: CanonicalDigest::of_bytes(b"pending"),
    };
    snapshot.digest = snapshot.computed_digest().unwrap();
    let service = ModelRatings::default();
    service.install(snapshot).unwrap();
    service
}

#[test]
fn missing_snapshot_keeps_free_candidates_and_marks_incomplete_native_controls() {
    let mut facts = compilation_facts();
    for fact in &mut facts.candidates {
        fact.rating = None;
    }
    let result = suggest_free(
        &facts,
        &custom_desired().requirements,
        &BTreeMap::new(),
        &ModelRatings::default(),
        RatingSnapshotSelectionV1::Latest,
    )
    .unwrap();
    assert!(result.snapshot_ref.is_none());
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|c| c.selection.binding_id.as_str())
            .collect::<Vec<_>>(),
        vec!["binding/free-a", "binding/free-b"]
    );
    assert!(result.candidates.iter().all(|c| c.rating.is_none()));
    assert_eq!(
        result.unavailable["binding/free-budget"],
        SuggestionUnavailableReasonV1::NativeSelectionRequired
    );
    assert_eq!(
        result.unavailable["binding/free-toggle"],
        SuggestionUnavailableReasonV1::NativeSelectionRequired
    );
    assert_eq!(
        result.unavailable["binding/primary-a"],
        SuggestionUnavailableReasonV1::NotFree
    );
}

#[test]
fn suggestions_rank_exact_configuration_and_explicit_version_never_falls_back() {
    let facts = compilation_facts();
    let ratings = service(&facts);
    let args = &custom_desired().requirements;
    let low = suggest_free(
        &facts,
        args,
        &BTreeMap::new(),
        &ratings,
        RatingSnapshotSelectionV1::Latest,
    )
    .unwrap();
    assert_eq!(
        low.snapshot_ref.as_ref().unwrap().version,
        "ratings/test-v1"
    );
    assert_eq!(low.candidates[0].selection.binding_id, "binding/free-b");
    assert!(matches!(
        low.candidates[1].rating.as_ref().unwrap().overall,
        RatingValueV1::Estimated {
            score_tenths: 20,
            ..
        }
    ));
    let choices = BTreeMap::from([(
        "binding/free-a".into(),
        ReasoningSelectionV1::Profile {
            profile: "high".into(),
        },
    )]);
    let high = suggest_free(
        &facts,
        args,
        &choices,
        &ratings,
        RatingSnapshotSelectionV1::Latest,
    )
    .unwrap();
    assert_eq!(high.candidates[0].selection.binding_id, "binding/free-a");
    // Previous results/editor-adopted arrays remain owned values, not live sorted views.
    assert_eq!(low.candidates[0].selection.binding_id, "binding/free-b");
    assert_eq!(
        suggest_free(
            &facts,
            args,
            &choices,
            &ratings,
            RatingSnapshotSelectionV1::Version {
                version: "missing".into()
            }
        ),
        Err(AgentPlanCompilerError::RatingSnapshotUnavailable)
    );
}
