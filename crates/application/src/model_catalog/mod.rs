//! All consumers resolve one immutable snapshot for the entire batch.
mod control;
use arc_swap::ArcSwapOption;
pub use control::ModelCatalogPort;
pub(crate) use control::dispatch;
use hiroute_application_api::{
    MAX_RATING_QUERY_ITEMS, ModelRatingResultItemV1, RatingSnapshotSelectionV1,
    ResolveModelRatingsResultV1, ResolveModelRatingsV1,
};
use hiroute_domain::{ComputeContractError, RatingSnapshotV2};
use std::{collections::BTreeSet, sync::Arc};
use thiserror::Error;

#[derive(Default)]
pub struct ModelRatings {
    current: ArcSwapOption<RatingSnapshotV2>,
}
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum RatingQueryError {
    #[error("rating snapshot unavailable")]
    SnapshotUnavailable,
    #[error("invalid rating query arguments")]
    InvalidArguments,
}
impl ModelRatings {
    /// Called only with client-bound data after the current bundle and cross-references validate.
    pub fn install(&self, snapshot: RatingSnapshotV2) -> Result<(), ComputeContractError> {
        snapshot.validate()?;
        let old = self.current.load_full();
        if old
            .as_ref()
            .is_some_and(|s| s.version == snapshot.version && s.digest != snapshot.digest)
        {
            return Err(ComputeContractError::MixedReleaseSlice);
        }
        let previous = self
            .current
            .compare_and_swap(&old, Some(Arc::new(snapshot)));
        let unchanged = match (&*previous, &old) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if unchanged {
            Ok(())
        } else {
            Err(ComputeContractError::GenerationConflict)
        }
    }
    pub fn resolve(
        &self,
        query: &ResolveModelRatingsV1,
    ) -> Result<ResolveModelRatingsResultV1, RatingQueryError> {
        if query.items.is_empty() || query.items.len() > MAX_RATING_QUERY_ITEMS {
            return Err(RatingQueryError::InvalidArguments);
        }
        let mut ids = BTreeSet::new();
        for item in &query.items {
            if item.query_id.is_empty()
                || item.query_id.len() > 128
                || item.query_id.chars().any(char::is_control)
                || !ids.insert(&item.query_id)
            {
                return Err(RatingQueryError::InvalidArguments);
            }
        }
        let snapshot = self
            .current
            .load_full()
            .ok_or(RatingQueryError::SnapshotUnavailable)?;
        if let RatingSnapshotSelectionV1::Version { version } = &query.snapshot
            && *version != snapshot.version
        {
            return Err(RatingQueryError::SnapshotUnavailable);
        }
        let items = query
            .items
            .iter()
            .map(|item| {
                Ok(ModelRatingResultItemV1 {
                    query_id: item.query_id.clone(),
                    rating: snapshot
                        .resolve(&item.model_configuration_id, &item.exact_native_reasoning)
                        .map_err(|_| RatingQueryError::InvalidArguments)?,
                })
            })
            .collect::<Result<Vec<_>, RatingQueryError>>()?;
        Ok(ResolveModelRatingsResultV1 {
            snapshot_ref: snapshot.snapshot_ref(),
            items,
        })
    }
}

#[cfg(test)]
mod tests;
