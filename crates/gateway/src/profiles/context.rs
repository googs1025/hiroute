use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{CriticalFact, ReasoningAccounting, ReasoningProfileCapability};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenEstimatorProfile {
    pub revision: String,
    /// Conservative ceil(bytes / bytes_per_token). A value of one is safe for
    /// arbitrary UTF-8 and is used by exact fixtures.
    pub bytes_per_token: u64,
    pub fixed_overhead_tokens: u64,
}

impl TokenEstimatorProfile {
    pub fn estimate(&self, serialized_bytes: usize) -> Result<u64, ContextProjectionError> {
        if self.revision.trim().is_empty() || self.bytes_per_token == 0 {
            return Err(ContextProjectionError::UnknownEstimator);
        }
        let bytes = u64::try_from(serialized_bytes)
            .map_err(|_| ContextProjectionError::ArithmeticOverflow)?;
        let variable = bytes
            .checked_add(self.bytes_per_token - 1)
            .ok_or(ContextProjectionError::ArithmeticOverflow)?
            / self.bytes_per_token;
        variable
            .checked_add(self.fixed_overhead_tokens)
            .ok_or(ContextProjectionError::ArithmeticOverflow)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextLimits {
    pub max_input_tokens: CriticalFact<u64>,
    pub max_output_tokens: CriticalFact<u64>,
    pub max_total_tokens: CriticalFact<Option<u64>>,
    pub estimator: CriticalFact<TokenEstimatorProfile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateContextDemand {
    pub target_serialized_bytes: usize,
    pub target_serialized_input_upper_bound: u64,
    pub effective_output_cap: u64,
    pub additional_reasoning_reservation: u64,
    pub required_total: u64,
    pub estimator_revision: String,
}

pub struct ContextProjector;

impl ContextProjector {
    pub fn project(
        serialized_request: &[u8],
        limits: &ContextLimits,
        reasoning: &ReasoningProfileCapability,
    ) -> Result<CandidateContextDemand, ContextProjectionError> {
        let serialized_bytes = u64::try_from(serialized_request.len())
            .map_err(|_| ContextProjectionError::ArithmeticOverflow)?;
        Self::project_serialized_len(serialized_bytes, limits, reasoning)
    }

    /// Projects an adapter-materialized target byte count without retaining
    /// or copying request content in the planner input. The adapter revision
    /// and complete capability profile are covered by the candidate profile
    /// digest, so this remains a deterministic candidate-specific fact.
    pub fn project_serialized_len(
        serialized_bytes: u64,
        limits: &ContextLimits,
        reasoning: &ReasoningProfileCapability,
    ) -> Result<CandidateContextDemand, ContextProjectionError> {
        let max_input = *limits
            .max_input_tokens
            .exact()
            .ok_or(ContextProjectionError::UnknownLimit("max_input"))?;
        let max_output = *limits
            .max_output_tokens
            .exact()
            .ok_or(ContextProjectionError::UnknownLimit("max_output"))?;
        let max_total = limits
            .max_total_tokens
            .exact()
            .ok_or(ContextProjectionError::UnknownLimit("max_total"))?;
        let estimator = limits
            .estimator
            .exact()
            .ok_or(ContextProjectionError::UnknownEstimator)?;
        let serialized_bytes_usize = usize::try_from(serialized_bytes)
            .map_err(|_| ContextProjectionError::ArithmeticOverflow)?;
        let input = estimator.estimate(serialized_bytes_usize)?;
        if input > max_input {
            return Err(ContextProjectionError::InputTooLarge {
                required: input,
                limit: max_input,
            });
        }
        let reasoning_reservation = match reasoning.accounting {
            ReasoningAccounting::WithinOutputCap => 0,
            ReasoningAccounting::Additive => reasoning.additional_reservation_tokens,
        };
        let required_total = input
            .checked_add(max_output)
            .and_then(|value| value.checked_add(reasoning_reservation))
            .ok_or(ContextProjectionError::ArithmeticOverflow)?;
        if let Some(max_total) = *max_total
            && required_total > max_total
        {
            return Err(ContextProjectionError::TotalTooLarge {
                required: required_total,
                limit: max_total,
            });
        }
        Ok(CandidateContextDemand {
            target_serialized_bytes: serialized_bytes_usize,
            target_serialized_input_upper_bound: input,
            effective_output_cap: max_output,
            additional_reasoning_reservation: reasoning_reservation,
            required_total,
            estimator_revision: estimator.revision.clone(),
        })
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ContextProjectionError {
    #[error("candidate context fact is unknown: {0}")]
    UnknownLimit(&'static str),
    #[error("candidate token estimator is unknown")]
    UnknownEstimator,
    #[error("target serialized input requires {required} tokens but limit is {limit}")]
    InputTooLarge { required: u64, limit: u64 },
    #[error("candidate total requires {required} tokens but limit is {limit}")]
    TotalTooLarge { required: u64, limit: u64 },
    #[error("context projection overflowed")]
    ArithmeticOverflow,
}
