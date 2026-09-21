use hiroute_domain::{
    GatewayPublicationRevision, PortError, PublicationRecordV1, PublicationRepositoryPort,
    WorkspaceId,
};
use thiserror::Error;

use super::{PublicationTargetError, PublicationTargetPort};

pub struct PublicationActivator<'a, S, T> {
    store: &'a S,
    target: &'a T,
}

impl<'a, S, T> PublicationActivator<'a, S, T>
where
    S: PublicationRepositoryPort,
    T: PublicationTargetPort,
{
    pub fn new(store: &'a S, target: &'a T) -> Self {
        Self { store, target }
    }

    pub fn publish(
        &self,
        record: &PublicationRecordV1,
        expected_active_revision: Option<GatewayPublicationRevision>,
    ) -> Result<PublicationActivationOutcome, PublicationActivationError> {
        let publication = record
            .verify()
            .map_err(|_| PublicationActivationError::VerificationFailed)?;
        if let Some(active_record) = self.store.active_publication(&record.workspace_id)?
            && active_record != *record
        {
            let active = active_record
                .verify()
                .map_err(|_| PublicationActivationError::VerificationFailed)?;
            publication
                .validate_transition_from(&active)
                .map_err(|_| PublicationActivationError::InvalidTransition)?;
        }
        let prepared = self
            .store
            .prepare_publication(record, expected_active_revision)?;
        self.target.activate_verified(record)?;
        self.store.mark_publication_active(
            &record.workspace_id,
            record.publication_revision,
            &record.digest,
        )?;
        Ok(match prepared {
            hiroute_domain::PreparePublicationOutcome::Created => {
                PublicationActivationOutcome::Activated
            }
            hiroute_domain::PreparePublicationOutcome::ExistingSame => {
                PublicationActivationOutcome::AlreadyPreparedActivated
            }
        })
    }

    pub fn recover(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<PublicationRecoveryOutcome, PublicationActivationError> {
        if let Some(prepared) = self.store.prepared_publication(workspace)? {
            if let Ok(publication) = prepared.verify() {
                if let Some(active_record) = self.store.active_publication(workspace)? {
                    let Ok(active) = active_record.verify() else {
                        return self.restore_known_good(workspace, true);
                    };
                    if publication.validate_transition_from(&active).is_err() {
                        return self.restore_known_good(workspace, true);
                    }
                }
                self.target.activate_verified(&prepared)?;
                self.store.mark_publication_active(
                    workspace,
                    prepared.publication_revision,
                    &prepared.digest,
                )?;
                return Ok(PublicationRecoveryOutcome::CompletedPrepared);
            }
            return self.restore_known_good(workspace, true);
        }
        self.restore_known_good(workspace, false)
    }

    fn restore_known_good(
        &self,
        workspace: &WorkspaceId,
        corrupt_prepared: bool,
    ) -> Result<PublicationRecoveryOutcome, PublicationActivationError> {
        if let Some(active) = self.store.active_publication(workspace)?
            && active.verify().is_ok()
        {
            self.target.activate_verified(&active)?;
            return Ok(if corrupt_prepared {
                PublicationRecoveryOutcome::RejectedPreparedKeptActive
            } else {
                PublicationRecoveryOutcome::RestoredActive
            });
        }
        if let Some(lkg) = self.store.last_known_good_publication(workspace)?
            && lkg.verify().is_ok()
        {
            self.target.activate_verified(&lkg)?;
            return Ok(PublicationRecoveryOutcome::RestoredLastKnownGood);
        }
        if corrupt_prepared {
            Err(PublicationActivationError::NoKnownGoodPublication)
        } else {
            Ok(PublicationRecoveryOutcome::Empty)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationActivationOutcome {
    Activated,
    AlreadyPreparedActivated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationRecoveryOutcome {
    Empty,
    CompletedPrepared,
    RejectedPreparedKeptActive,
    RestoredActive,
    RestoredLastKnownGood,
}

#[derive(Debug, Error)]
pub enum PublicationActivationError {
    #[error("prepared publication failed closed verification")]
    VerificationFailed,
    #[error("no valid active or last-known-good publication remains")]
    NoKnownGoodPublication,
    #[error("publication or immutable AgentPlan revision transition is invalid")]
    InvalidTransition,
    #[error(transparent)]
    Store(#[from] PortError),
    #[error(transparent)]
    Target(#[from] PublicationTargetError),
}
