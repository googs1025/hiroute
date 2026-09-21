//! Small local launcher consumed by 20 and implemented by 18. No task or ACP policy here.
use super::profile::CandidateWorkerProfile;
use async_trait::async_trait;
use hiroute_domain::delegation::DelegationErrorV1;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};

pub type WorkerStdout = Pin<Box<dyn AsyncRead + Send>>;
pub type WorkerStdin = Pin<Box<dyn AsyncWrite + Send>>;

/// A persisted locator never authorizes killing a PID: the backend must own or revalidate
/// the actual process object. Worker input cannot grant ownership of a handle.
pub use hiroute_domain::delegation::DelegationProcessBindingV1 as WorkerProcessIdentity;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkerPlatformCapabilities {
    pub can_start: bool,
    pub can_stop: bool,
}

pub struct WorkerLaunchRequest {
    /// Correlates this launch; does not require a child nonce handshake.
    pub launch_nonce: String,
    pub profile: CandidateWorkerProfile,
    /// Deadline while the daemon manages execution; no crash-survival guarantee.
    pub deadline_unix_ms: u64,
}

/// Spawn succeeded and correct resources/stdio were acquired. ACP initialize belongs to 20.
/// The launcher normally retains the child in the daemon; no helper is required.
pub struct ReadyWorker {
    pub identity: WorkerProcessIdentity,
    pub stdin: WorkerStdin,
    pub stdout: WorkerStdout,
}

pub use hiroute_domain::delegation::{
    RunProcessObservationV1 as WorkerObservation, RunStopEvidenceV1 as WorkerStopEvidence,
    RunStopScopeV1 as WorkerStopScope,
};

#[async_trait]
pub trait WorkerPlatformPort: Send + Sync {
    /// Basic selected-install start/stop availability. Native permissions stay in 20.
    fn capabilities(
        &self,
        profile: &CandidateWorkerProfile,
    ) -> Result<WorkerPlatformCapabilities, DelegationErrorV1>;
    /// Fixed argv/env/cwd and requested private materials. On spawn failure/cancellation,
    /// finitely clean acquired resources; no recovery DB or extra ready protocol.
    async fn launch(&self, request: WorkerLaunchRequest) -> Result<ReadyWorker, DelegationErrorV1>;
    async fn observe(
        &self,
        identity: &WorkerProcessIdentity,
    ) -> Result<WorkerObservation, DelegationErrorV1>;
    /// After token denial/cancel intent and outside the admission gate; only held/verified
    /// objects, never stale PIDs. Returns real scope/residual information within the bound.
    async fn terminate(
        &self,
        identity: &WorkerProcessIdentity,
        max_wait_ms: u64,
    ) -> Result<WorkerStopEvidence, DelegationErrorV1>;
    /// Discard local held-object bookkeeping only after the lifecycle has recorded successful
    /// stop evidence. Backends without retained in-memory objects may use the default no-op.
    fn release(&self, _: &WorkerProcessIdentity) -> Result<(), DelegationErrorV1> {
        Ok(())
    }
}
