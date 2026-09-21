use super::*;

/// Generic O(1) publication slot for composition layers that must swap their
/// aggregate root at one linearization point without pinning in-flight users.
#[derive(Debug)]
pub struct AtomicPublicationSlot<T> {
    active: ArcSwapOption<T>,
}

impl<T> AtomicPublicationSlot<T> {
    pub fn empty() -> Self {
        Self {
            active: ArcSwapOption::empty(),
        }
    }

    pub fn load(&self) -> Option<Arc<T>> {
        self.active.load_full()
    }

    pub fn store(&self, publication: Arc<T>) {
        self.active.store(Some(publication));
    }

    pub fn clear(&self) {
        self.active.store(None);
    }
}

impl PublicationInstaller {
    /// Prepare when the composition root has no caller-owned cancellation.
    pub fn prepare_uncancelled(
        &self,
        candidate: CompiledGatewayPublicationEnvelope,
        deadline: Instant,
    ) -> Result<PrepareOutcome, InstallError> {
        self.prepare(candidate, &CancellationToken::new(), deadline)
    }

    /// Publish when the composition root has no caller-owned cancellation.
    pub fn publish_uncancelled(
        &self,
        prepared: PreparedPublication,
        deadline: Instant,
    ) -> Result<Arc<ActivePublication>, InstallError> {
        self.publish(prepared, &CancellationToken::new(), deadline)
    }

    /// Relinquish an unpublished ticket without leaving the installer in its
    /// prepared phase. Only the current ticket can clear that phase.
    pub fn abandon_prepared(&self, prepared: PreparedPublication) -> Result<(), InstallError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.prepared_generation != Some(prepared.generation) {
            return Err(InstallError::StalePreparedPublication);
        }
        state.phase = InstallerPhase::Cancelled;
        state.prepared_generation = None;
        Ok(())
    }
}
