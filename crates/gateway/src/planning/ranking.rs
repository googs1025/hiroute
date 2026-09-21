use std::cmp::Ordering;
use std::collections::BTreeSet;

use super::eligibility::EligibleProjection;
use super::{
    CacheCostFactV1, CostClassV1, ExclusionReasonCodeV1, GroupPolicyV1, MaterializedModelGroupV1,
    PlannerCandidateFactsV1, RankingReasonCodeV1,
};

#[derive(Clone, Debug)]
pub(crate) struct EligibleForRanking<'a> {
    pub candidate: &'a PlannerCandidateFactsV1,
    pub projection: EligibleProjection,
    pub declared_order: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct RankedCandidateProjection<'a> {
    pub candidate: &'a PlannerCandidateFactsV1,
    pub projection: EligibleProjection,
    pub declared_order: u32,
    pub reasons: Vec<RankingReasonCodeV1>,
}

pub(crate) struct RankedGroup<'a> {
    pub ordered: Vec<RankedCandidateProjection<'a>>,
    pub exclusions: Vec<(&'a PlannerCandidateFactsV1, u32, ExclusionReasonCodeV1)>,
}

pub(crate) fn rank_group<'a>(
    group: &MaterializedModelGroupV1,
    mut eligible: Vec<EligibleForRanking<'a>>,
    guard_anchor_score: Option<i32>,
) -> RankedGroup<'a> {
    let mut exclusions = Vec::new();
    match &group.policy {
        GroupPolicyV1::Manual => {}
        GroupPolicyV1::QualityFirst => {
            eligible.retain(|entry| {
                if entry.candidate.overall_score_tenths.is_none() {
                    exclusions.push((
                        entry.candidate,
                        entry.declared_order,
                        ExclusionReasonCodeV1::RatingUnknown,
                    ));
                    false
                } else {
                    true
                }
            });
            eligible.sort_by(quality_first_cmp);
        }
        GroupPolicyV1::CheapestWithRatingGuard {
            quality_anchor_ref,
            max_gap_tenths,
        } => {
            let _ = quality_anchor_ref;
            let Some(anchor) = guard_anchor_score else {
                exclusions.extend(eligible.drain(..).map(|entry| {
                    (
                        entry.candidate,
                        entry.declared_order,
                        ExclusionReasonCodeV1::QualityAnchorUnavailable,
                    )
                }));
                return RankedGroup {
                    ordered: Vec::new(),
                    exclusions,
                };
            };
            let floor = anchor.saturating_sub(i32::from(*max_gap_tenths));
            eligible.retain(|entry| match entry.candidate.overall_score_tenths {
                None => {
                    exclusions.push((
                        entry.candidate,
                        entry.declared_order,
                        ExclusionReasonCodeV1::RatingUnknown,
                    ));
                    false
                }
                Some(score) if score < floor => {
                    exclusions.push((
                        entry.candidate,
                        entry.declared_order,
                        ExclusionReasonCodeV1::RatingGuardExcluded,
                    ));
                    false
                }
                Some(_) => true,
            });
            eligible.sort_by(cheapest_guard_cmp);
        }
    }

    let affinity_winners = eligible
        .iter()
        .filter(|entry| {
            entry.candidate.affinity_penalty() == 0
                && !matches!(
                    entry.candidate.cache_cost,
                    CacheCostFactV1::Confirmed { .. }
                )
                && eligible.iter().any(|other| {
                    other.candidate.candidate_id != entry.candidate.candidate_id
                        && other.candidate.affinity_penalty() > entry.candidate.affinity_penalty()
                        && same_primary_key(&group.policy, entry, other)
                })
        })
        .map(|entry| entry.candidate.candidate_id.as_str())
        .collect::<BTreeSet<_>>();
    let ordered = eligible
        .into_iter()
        .map(|entry| {
            let mut reasons = vec![match &group.policy {
                GroupPolicyV1::Manual => RankingReasonCodeV1::PublishedManualOrder,
                GroupPolicyV1::QualityFirst => RankingReasonCodeV1::QualityFirst,
                GroupPolicyV1::CheapestWithRatingGuard { .. } => {
                    RankingReasonCodeV1::LowestApiEquivalentCost
                }
            }];
            match (&group.policy, entry.candidate.cache_cost) {
                (GroupPolicyV1::Manual, _) => {}
                (_, CacheCostFactV1::Confirmed { .. }) => {
                    reasons.push(RankingReasonCodeV1::ConfirmedCacheHold);
                }
                (_, CacheCostFactV1::None | CacheCostFactV1::EligibleUnconfirmed)
                    if affinity_winners.contains(entry.candidate.candidate_id.as_str()) =>
                {
                    reasons.push(RankingReasonCodeV1::UnconfirmedAffinityTieBreak);
                }
                (_, CacheCostFactV1::None | CacheCostFactV1::EligibleUnconfirmed) => {}
            }
            RankedCandidateProjection {
                candidate: entry.candidate,
                projection: entry.projection,
                declared_order: entry.declared_order,
                reasons,
            }
        })
        .collect();
    RankedGroup {
        ordered,
        exclusions,
    }
}

fn same_primary_key(
    policy: &GroupPolicyV1,
    left: &EligibleForRanking<'_>,
    right: &EligibleForRanking<'_>,
) -> bool {
    match policy {
        GroupPolicyV1::Manual => false,
        GroupPolicyV1::QualityFirst => {
            left.candidate.overall_score_tenths == right.candidate.overall_score_tenths
                && left.projection.effective_cost_micros == right.projection.effective_cost_micros
        }
        GroupPolicyV1::CheapestWithRatingGuard { .. } => {
            left.projection.effective_cost_micros == right.projection.effective_cost_micros
                && left.candidate.overall_score_tenths == right.candidate.overall_score_tenths
        }
    }
}

fn quality_first_cmp(left: &EligibleForRanking<'_>, right: &EligibleForRanking<'_>) -> Ordering {
    right
        .candidate
        .overall_score_tenths
        .cmp(&left.candidate.overall_score_tenths)
        .then_with(|| {
            left.projection
                .effective_cost_micros
                .is_none()
                .cmp(&right.projection.effective_cost_micros.is_none())
        })
        .then_with(|| {
            left.projection
                .effective_cost_micros
                .unwrap_or(u64::MAX)
                .cmp(&right.projection.effective_cost_micros.unwrap_or(u64::MAX))
        })
        .then_with(|| {
            left.candidate
                .affinity_penalty()
                .cmp(&right.candidate.affinity_penalty())
        })
        .then_with(|| {
            class_order(left.candidate.cost_class).cmp(&class_order(right.candidate.cost_class))
        })
        .then_with(|| {
            left.candidate
                .compute_scope_order
                .cmp(&right.candidate.compute_scope_order)
        })
        .then_with(|| left.declared_order.cmp(&right.declared_order))
        .then_with(|| {
            left.candidate
                .stable_binding_id
                .cmp(&right.candidate.stable_binding_id)
        })
}

fn cheapest_guard_cmp(left: &EligibleForRanking<'_>, right: &EligibleForRanking<'_>) -> Ordering {
    left.projection
        .effective_cost_micros
        .is_none()
        .cmp(&right.projection.effective_cost_micros.is_none())
        .then_with(|| {
            left.projection
                .effective_cost_micros
                .unwrap_or(u64::MAX)
                .cmp(&right.projection.effective_cost_micros.unwrap_or(u64::MAX))
        })
        .then_with(|| {
            right
                .candidate
                .overall_score_tenths
                .cmp(&left.candidate.overall_score_tenths)
        })
        .then_with(|| {
            left.candidate
                .affinity_penalty()
                .cmp(&right.candidate.affinity_penalty())
        })
        .then_with(|| {
            class_order(left.candidate.cost_class).cmp(&class_order(right.candidate.cost_class))
        })
        .then_with(|| {
            left.candidate
                .compute_scope_order
                .cmp(&right.candidate.compute_scope_order)
        })
        .then_with(|| left.declared_order.cmp(&right.declared_order))
        .then_with(|| {
            left.candidate
                .stable_binding_id
                .cmp(&right.candidate.stable_binding_id)
        })
}

fn class_order(class: CostClassV1) -> u8 {
    match class {
        CostClassV1::Free => 0,
        CostClassV1::Subscription => 1,
        CostClassV1::Paid => 2,
        CostClassV1::Unknown => 3,
    }
}
