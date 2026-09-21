//! One bounded maintenance loop per production store. Request producers never
//! wait for this thread; its only inputs are already committed observations.
use crate::LocalObservationStore;
use std::sync::{Arc, Weak, mpsc};
use std::time::Duration;

pub struct ObservationMaintenance {
    stop: mpsc::Sender<()>,
}

/// Optional product-owned work attached to the one maintenance lifecycle.  The hook receives no
/// observation lock or deletion authority; implementations must perform their own bounded work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservationMaintenanceHookError;

pub trait ObservationMaintenanceHook: Send + Sync {
    fn cycle(&self, now_ms: i64) -> Result<(), ObservationMaintenanceHookError>;
}

impl ObservationMaintenance {
    pub fn start(store: &Arc<LocalObservationStore>) -> std::io::Result<Self> {
        Self::start_with_hook(store, None)
    }

    pub fn start_with_hook(
        store: &Arc<LocalObservationStore>,
        hook: Option<Arc<dyn ObservationMaintenanceHook>>,
    ) -> std::io::Result<Self> {
        let lease = LocalWorkerGuard::acquire(&store.maintenance_running).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "observation maintenance already running",
            )
        })?;
        let (stop, receiver) = mpsc::channel();
        let weak = Arc::downgrade(store);
        let result = std::thread::Builder::new()
            .name("hiroute-observation-maintenance".into())
            .spawn(move || {
                let _lease = lease;
                run(weak, receiver, hook)
            });
        if let Err(error) = result {
            store
                .maintenance_errors
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Err(error);
        }
        Ok(Self { stop })
    }
}

impl Drop for ObservationMaintenance {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

fn run(
    store: Weak<LocalObservationStore>,
    stop: mpsc::Receiver<()>,
    hook: Option<Arc<dyn ObservationMaintenanceHook>>,
) {
    let mut index = store
        .upgrade()
        .and_then(|store| crate::text_index::TextIndexBuilder::new(&store).ok());
    while matches!(
        stop.recv_timeout(Duration::from_millis(250)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ) {
        let Some(store) = store.upgrade() else {
            break;
        };
        let backfill_failed = store.backfill_inline_payloads().is_err();
        let settlement_failed = store.settle_pending_valuations(16).is_err();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|time| i64::try_from(time.as_millis()).ok());
        let retention_failed = now_ms
            .map(|now| {
                store.managed_text_expire_pending(now, 16).is_err()
                    | store.managed_text_gc(now, 32).is_err()
                    | store.managed_text_progress_gc(now, 32).is_err()
                    | store.expire_request_details(now, 16).is_err()
                    | store.collect_garbage_batch(32).is_err()
            })
            .unwrap_or(true);
        let hook_failed = match (hook.as_ref(), now_ms) {
            (Some(hook), Some(now)) => hook.cycle(now).is_err(),
            (Some(_), None) => true,
            (None, _) => false,
        };
        let index_failed = match index.as_mut() {
            Some(index) => index.cycle(&store).is_err(),
            None => {
                index = crate::text_index::TextIndexBuilder::new(&store).ok();
                index.is_none()
            }
        };
        if backfill_failed || settlement_failed || retention_failed || hook_failed || index_failed {
            store
                .maintenance_errors
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Process-local ownership only: not a distributed lease or durable authority.
pub(crate) struct LocalWorkerGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl LocalWorkerGuard {
    pub(crate) fn acquire(flag: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> Option<Self> {
        flag.compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .ok()
        .map(|_| Self(flag.clone()))
    }
}
impl Drop for LocalWorkerGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}
impl LocalObservationStore {
    pub fn maintenance_status(&self) -> (bool, u64) {
        (
            self.maintenance_running
                .load(std::sync::atomic::Ordering::Acquire),
            self.maintenance_errors
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }
}
