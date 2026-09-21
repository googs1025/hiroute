//! In-process exclusion between terminal Worker publication and operations that consume or
//! remove its continuation materials.

use std::sync::{Arc, Condvar, Mutex, PoisonError};

#[derive(Default)]
pub(crate) struct DelegationFinalization {
    occupied: Mutex<bool>,
    available: Condvar,
}

impl DelegationFinalization {
    /// Acquire an owned lease. Unlike `MutexGuard`, the lease can cross the async lifecycle
    /// boundary and remain held until the executor has retained the native continuation facts.
    pub(crate) fn acquire(self: &Arc<Self>) -> DelegationFinalizationLease {
        let mut occupied = self.occupied.lock().unwrap_or_else(PoisonError::into_inner);
        while *occupied {
            occupied = self
                .available
                .wait(occupied)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *occupied = true;
        DelegationFinalizationLease {
            owner: Arc::clone(self),
        }
    }

    #[cfg(test)]
    pub(crate) fn try_acquire(self: &Arc<Self>) -> Option<DelegationFinalizationLease> {
        let mut occupied = self.occupied.lock().unwrap_or_else(PoisonError::into_inner);
        if *occupied {
            return None;
        }
        *occupied = true;
        Some(DelegationFinalizationLease {
            owner: Arc::clone(self),
        })
    }
}

pub(crate) struct DelegationFinalizationLease {
    owner: Arc<DelegationFinalization>,
}

impl Drop for DelegationFinalizationLease {
    fn drop(&mut self) {
        let mut occupied = self
            .owner
            .occupied
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *occupied = false;
        self.owner.available.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_lease_excludes_until_the_finalizer_drops_it() {
        let finalization = Arc::new(DelegationFinalization::default());
        let first = finalization.acquire();
        assert!(finalization.try_acquire().is_none());
        drop(first);
        assert!(finalization.try_acquire().is_some());
    }
}
