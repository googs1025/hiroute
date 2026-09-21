//! Production OS launcher. Task, ACP, profile rendering and permission policy stay in 20.
use super::{platform::*, profile::CandidateWorkerProfile};
use async_trait::async_trait;
use hiroute_domain::delegation::DelegationErrorV1 as Error;
use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    process::Command,
    time::{Instant, sleep, timeout_at},
};

mod materials;
mod process;
use materials::OwnedRoot;
use process::Process;

type Records = HashMap<String, Arc<Mutex<Record>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaterialCleanup {
    Pending,
    Complete,
    Failed,
}

struct Record {
    identity: WorkerProcessIdentity,
    process: Process,
    materials: Option<OwnedRoot>,
    cleanup: MaterialCleanup,
    scope_stopped: bool,
}

/// Contains current OS objects only, not task/run business state or durable recovery data.
#[derive(Clone, Default)]
pub struct LocalWorkerPlatform {
    records: Arc<Mutex<Records>>,
    launching: Arc<Mutex<HashSet<String>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn opaque() -> Result<String, Error> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|_| Error::CapabilityUnavailable)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl LocalWorkerPlatform {
    /// Local facts for a launch whose caller disappeared after ownership transfer.
    pub fn identity_for_launch(&self, nonce: &str) -> Option<WorkerProcessIdentity> {
        lock(&self.records)
            .get(nonce)
            .map(|r| lock(r).identity.clone())
    }

    fn record(&self, identity: &WorkerProcessIdentity) -> Option<Arc<Mutex<Record>>> {
        lock(&self.records)
            .get(&identity.launch_nonce)
            .filter(|record| lock(record).identity == *identity)
            .cloned()
    }

    pub fn material_cleanup(&self, identity: &WorkerProcessIdentity) -> Option<MaterialCleanup> {
        self.record(identity).map(|r| lock(&r).cleanup)
    }

    /// Explicit caller release of completed local facts. Active objects never lose ownership.
    pub fn release(&self, identity: &WorkerProcessIdentity) -> Result<(), Error> {
        let mut records = lock(&self.records);
        let record = records.get(&identity.launch_nonce).ok_or(Error::Conflict)?;
        {
            let record = lock(record);
            if record.identity != *identity
                || !record.scope_stopped
                || record.cleanup != MaterialCleanup::Complete
            {
                return Err(Error::Busy);
            }
        }
        records.remove(&identity.launch_nonce);
        Ok(())
    }

    async fn cleanup(record: Arc<Mutex<Record>>, deadline: Instant) {
        let materials = lock(&record).materials.take();
        if let Some(mut materials) = materials {
            // A timed-out caller does not cancel ownership of filesystem cleanup.
            let cleanup_record = record.clone();
            let job = tokio::task::spawn_blocking(move || {
                let result = materials.cleanup();
                let mut record = lock(&cleanup_record);
                record.cleanup = if result.is_ok() {
                    MaterialCleanup::Complete
                } else {
                    MaterialCleanup::Failed
                };
                if result.is_err() {
                    record.materials = Some(materials);
                }
            });
            let _ = timeout_at(deadline, job).await;
        }
    }
}

struct PreparationReservation<'a> {
    platform: &'a LocalWorkerPlatform,
    nonce: String,
}
impl Drop for PreparationReservation<'_> {
    fn drop(&mut self) {
        lock(&self.platform.launching).remove(&self.nonce);
    }
}

#[async_trait]
impl WorkerPlatformPort for LocalWorkerPlatform {
    fn capabilities(
        &self,
        profile: &CandidateWorkerProfile,
    ) -> Result<WorkerPlatformCapabilities, Error> {
        materials::executable(&profile.executable)?;
        if !profile.cwd.is_absolute()
            || !profile.cwd.is_dir()
            || !profile.private_root.is_absolute()
        {
            return Err(Error::InvalidArguments);
        }
        Ok(WorkerPlatformCapabilities {
            can_start: true,
            can_stop: true,
        })
    }

    async fn launch(&self, request: WorkerLaunchRequest) -> Result<ReadyWorker, Error> {
        if request.launch_nonce.is_empty() || request.launch_nonce.len() > 128 {
            return Err(Error::InvalidArguments);
        }
        {
            let mut launching = lock(&self.launching);
            if launching.contains(&request.launch_nonce)
                || lock(&self.records).contains_key(&request.launch_nonce)
            {
                return Err(Error::Conflict);
            }
            launching.insert(request.launch_nonce.clone());
        }
        let _reservation = PreparationReservation {
            platform: self,
            nonce: request.launch_nonce.clone(),
        };
        self.capabilities(&request.profile)?;
        if request
            .profile
            .env
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['=', '\0']) || value.contains('\0'))
        {
            return Err(Error::InvalidArguments);
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::DeadlineExceeded)?
            .as_millis();
        let remaining = u128::from(request.deadline_unix_ms)
            .checked_sub(now)
            .filter(|n| *n > 0)
            .ok_or(Error::DeadlineExceeded)?;
        let deadline = Instant::now() + Duration::from_millis(remaining.min(10_000) as u64);
        let root = request.profile.private_root.clone();
        let session_root = request.profile.session_root.clone();
        let requested_materials = request.profile.materials;
        let prepared = tokio::task::spawn_blocking(move || {
            materials::prepare(requested_materials, root, session_root)
        });
        // OwnedRoot drops on the blocking worker if this receiver is cancelled.
        let mut materials = timeout_at(deadline, prepared)
            .await
            .map_err(|_| Error::DeadlineExceeded)?
            .map_err(|_| Error::StorageUnavailable)??;
        if Instant::now() >= deadline {
            return Err(Error::DeadlineExceeded);
        }
        let identity = WorkerProcessIdentity {
            launch_nonce: request.launch_nonce,
            handle_id: opaque()?,
            creation_identity: opaque()?,
        };
        let mut command = Command::new(&request.profile.executable);
        command
            .args(&request.profile.args)
            .env_clear()
            .current_dir(&request.profile.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in &request.profile.env {
            command.env(key, value.as_str());
        }
        Process::configure(&mut command);
        // No await between spawn, ownership registration and stdio delivery.
        let child = command.spawn().map_err(|_| Error::CapabilityUnavailable)?;
        let mut process = Process::new(child).map_err(|_| Error::CapabilityUnavailable)?;
        let pipes = process.pipes();
        materials.retain();
        let record = Arc::new(Mutex::new(Record {
            identity: identity.clone(),
            process,
            materials: Some(materials),
            cleanup: MaterialCleanup::Pending,
            scope_stopped: false,
        }));
        lock(&self.records).insert(identity.launch_nonce.clone(), record);
        match pipes {
            Some((stdin, stdout)) => Ok(ReadyWorker {
                identity,
                stdin: Box::pin(stdin),
                stdout: Box::pin(stdout),
            }),
            None => {
                let _ = self.terminate(&identity, 1_000).await;
                Err(Error::CapabilityUnavailable)
            }
        }
    }

    async fn observe(&self, identity: &WorkerProcessIdentity) -> Result<WorkerObservation, Error> {
        Ok(self
            .record(identity)
            .map_or(WorkerObservation::Unknown, |r| lock(&r).process.observe()))
    }

    async fn terminate(
        &self,
        identity: &WorkerProcessIdentity,
        max_wait_ms: u64,
    ) -> Result<WorkerStopEvidence, Error> {
        let deadline = Instant::now() + Duration::from_millis(max_wait_ms.min(10_000));
        let Some(record) = self.record(identity) else {
            return Ok(WorkerStopEvidence {
                scope: WorkerStopScope::Root,
                observation: WorkerObservation::Unknown,
                scope_stopped: false,
                residual_unknown: true,
            });
        };
        loop {
            let stopped = {
                let mut record = lock(&record);
                if max_wait_ms > 0 && !record.scope_stopped && record.process.request_stop().is_ok()
                {
                    record.scope_stopped = record.process.stopped();
                }
                record.scope_stopped
            };
            if stopped {
                Self::cleanup(record.clone(), deadline).await;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            sleep(
                Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
            )
            .await;
        }
        let mut record = lock(&record);
        Ok(WorkerStopEvidence {
            scope: record.process.scope(),
            observation: record.process.observe(),
            scope_stopped: record.scope_stopped,
            // Windows root-only remains insufficient evidence for ordinary descendants.
            residual_unknown: !record.scope_stopped
                || !cfg!(unix)
                || record.cleanup != MaterialCleanup::Complete,
        })
    }

    fn release(&self, identity: &WorkerProcessIdentity) -> Result<(), Error> {
        LocalWorkerPlatform::release(self, identity)
    }
}
