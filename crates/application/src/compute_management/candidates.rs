use std::collections::BTreeMap;
use std::sync::RwLock;

use hiroute_application_api::{
    ComputeCandidateIssueV2, ComputeCandidateRefV2, ComputeCandidateViewV2,
};
use hiroute_domain::{PortError, PortErrorCode, PortResult};

use super::{ComputeCandidateFactsV2, ComputeCandidatePort};

/// In-process registry for facts produced by trusted discovery and CPA adapters.
///
/// Candidate revisions are immutable. Each candidate reference retains only its current revision:
/// re-registering the same value is idempotent, while changing an existing revision or resolving a
/// superseded revision is rejected. Once the bounded set is full, a distinct reference is rejected
/// without evicting current revision evidence. Protected descriptors never leave this registry
/// through `get_compute_candidate`.
#[derive(Default)]
pub struct TrustedComputeCandidateRegistry {
    state: RwLock<CandidateRegistryState>,
}

#[derive(Default)]
struct CandidateRegistryState {
    entries: BTreeMap<String, ComputeCandidateFactsV2>,
}

pub(super) const MAX_TRUSTED_CANDIDATES: usize = 256;

impl TrustedComputeCandidateRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> PortResult<std::sync::RwLockReadGuard<'_, CandidateRegistryState>> {
        self.state
            .read()
            .map_err(|_| unavailable("compute.candidate.registry.read"))
    }

    fn view(facts: &ComputeCandidateFactsV2) -> PortResult<ComputeCandidateViewV2> {
        facts.public_view(
            facts.inferred_input_state(),
            Vec::<ComputeCandidateIssueV2>::new(),
        )
    }
}

impl ComputeCandidatePort for TrustedComputeCandidateRegistry {
    fn register_compute_candidate(
        &self,
        facts: ComputeCandidateFactsV2,
    ) -> PortResult<ComputeCandidateViewV2> {
        facts.validate_shape()?;
        let view = Self::view(&facts)?;
        let candidate_ref = facts.candidate.candidate_ref.clone();
        let candidate_revision = facts.candidate.candidate_revision;
        let mut state = self
            .state
            .write()
            .map_err(|_| unavailable("compute.candidate.registry.write"))?;
        if let Some(current) = state.entries.get(&candidate_ref) {
            match candidate_revision.cmp(&current.candidate.candidate_revision) {
                std::cmp::Ordering::Less => {
                    return Err(conflict("compute.candidate.registry.rollback"));
                }
                std::cmp::Ordering::Equal if current == &facts => return Ok(view),
                std::cmp::Ordering::Equal => {
                    return Err(conflict("compute.candidate.registry.immutable"));
                }
                std::cmp::Ordering::Greater => {}
            }
        } else if state.entries.len() >= MAX_TRUSTED_CANDIDATES {
            return Err(unavailable("compute.candidate.registry.capacity"));
        }
        state.entries.insert(candidate_ref, facts);
        Ok(view)
    }

    fn get_compute_candidate(
        &self,
        candidate: &ComputeCandidateRefV2,
    ) -> PortResult<ComputeCandidateViewV2> {
        let facts = self.resolve_compute_candidate(candidate)?;
        Self::view(&facts)
    }

    fn resolve_compute_candidate(
        &self,
        candidate: &ComputeCandidateRefV2,
    ) -> PortResult<ComputeCandidateFactsV2> {
        candidate
            .validate_shape()
            .map_err(|_| invalid("compute.candidate.registry.reference"))?;
        let state = self.read()?;
        let current = state
            .entries
            .get(&candidate.candidate_ref)
            .ok_or_else(|| not_found("compute.candidate.registry.missing"))?;
        if current.candidate.candidate_revision != candidate.candidate_revision {
            return Err(conflict("compute.candidate.registry.revision_changed"));
        }
        Ok(current.clone())
    }
}

fn invalid(context: &'static str) -> PortError {
    PortError::new(PortErrorCode::InvalidData, context)
}

fn conflict(context: &'static str) -> PortError {
    PortError::new(PortErrorCode::Conflict, context)
}

fn not_found(context: &'static str) -> PortError {
    PortError::new(PortErrorCode::NotFound, context)
}

fn unavailable(context: &'static str) -> PortError {
    PortError::new(PortErrorCode::Unavailable, context)
}
