use std::collections::BTreeSet;

use super::{CompiledPlanError, MaterializedModelGroupV1, MaterializedOrderingV1};

pub(super) fn validate_ordering_facts(
    group: &MaterializedModelGroupV1,
) -> Result<(), CompiledPlanError> {
    let candidate_ids = group
        .candidates
        .iter()
        .map(|candidate| candidate.binding_id.as_str())
        .collect::<BTreeSet<_>>();
    let fact_ids = group
        .pinned_ratings
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if !fact_ids.is_subset(&candidate_ids)
        || (!matches!(
            group.ordering_evidence,
            MaterializedOrderingV1::ExplicitOrder
        ) && fact_ids != candidate_ids)
    {
        return Err(CompiledPlanError::InvalidOrderingFacts);
    }
    for (binding_id, rating) in &group.pinned_ratings {
        let candidate = group
            .candidates
            .iter()
            .find(|candidate| candidate.binding_id == *binding_id)
            .ok_or(CompiledPlanError::InvalidOrderingFacts)?;
        if rating.model_configuration_id != candidate.model_configuration_id
            || !(5..=50).contains(&rating.overall_score_tenths)
        {
            return Err(CompiledPlanError::InvalidOrderingFacts);
        }
    }
    validate_materialized_order(group)
}

fn validate_materialized_order(group: &MaterializedModelGroupV1) -> Result<(), CompiledPlanError> {
    if matches!(
        &group.ordering_evidence,
        MaterializedOrderingV1::ExplicitOrder
    ) {
        return Ok(());
    }
    if let MaterializedOrderingV1::CheapestWithRatingGuard {
        quality_anchor_binding_id,
        maximum_score_gap_tenths,
    } = &group.ordering_evidence
    {
        let anchor_score = group
            .pinned_ratings
            .get(quality_anchor_binding_id)
            .map(|rating| rating.overall_score_tenths)
            .ok_or(CompiledPlanError::InvalidOrderingFacts)?;
        if group.pinned_ratings.values().any(|rating| {
            anchor_score.saturating_sub(rating.overall_score_tenths) > *maximum_score_gap_tenths
        }) {
            return Err(CompiledPlanError::InvalidOrderingFacts);
        }
        // The compiler already froze the cost-derived order. Price slices remain Turn-owned
        // follow-latest facts and therefore are intentionally absent from runtime publication.
        return Ok(());
    }
    for pair in group.candidates.windows(2) {
        let left = &pair[0];
        let right = &pair[1];
        let left_rating = &group.pinned_ratings[&left.binding_id];
        let right_rating = &group.pinned_ratings[&right.binding_id];
        let order = match &group.ordering_evidence {
            MaterializedOrderingV1::QualityFirst => right_rating
                .overall_score_tenths
                .cmp(&left_rating.overall_score_tenths)
                .then_with(|| {
                    left.model_configuration_id
                        .cmp(&right.model_configuration_id)
                })
                .then_with(|| left.binding_id.cmp(&right.binding_id)),
            MaterializedOrderingV1::CheapestWithRatingGuard { .. } => {
                unreachable!("returned after validating rating guard")
            }
            MaterializedOrderingV1::FreeScoreDescending => right_rating
                .overall_score_tenths
                .cmp(&left_rating.overall_score_tenths)
                .then_with(|| {
                    left.model_configuration_id
                        .cmp(&right.model_configuration_id)
                })
                .then_with(|| left.binding_id.cmp(&right.binding_id)),
            MaterializedOrderingV1::ExplicitOrder => unreachable!("returned above"),
        };
        if order == std::cmp::Ordering::Greater {
            return Err(CompiledPlanError::InvalidOrderingFacts);
        }
    }
    Ok(())
}
