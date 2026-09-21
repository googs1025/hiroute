//! Backend-only, single-use confirmation lifetime. It carries no business authority.
use std::time::{Duration, Instant};

pub(crate) const CONFIRMATION_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct ConfirmationGate {
    generation: u64,
    open: bool,
}
pub(crate) struct ConfirmationPermit {
    generation: u64,
    created: Instant,
}
impl ConfirmationGate {
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn begin(&mut self) -> Result<ConfirmationPermit, &'static str> {
        if self.open {
            return Err("CONFIRMATION_ALREADY_OPEN");
        }
        self.generation = self.generation.wrapping_add(1);
        self.open = true;
        Ok(ConfirmationPermit {
            generation: self.generation,
            created: Instant::now(),
        })
    }
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.open = false;
    }
    pub fn finish(
        &mut self,
        permit: &ConfirmationPermit,
        accepted: bool,
    ) -> Result<bool, &'static str> {
        if !self.open || permit.generation != self.generation {
            return Err("CONFIRMATION_STALE");
        }
        self.open = false;
        if accepted && permit.created.elapsed() > CONFIRMATION_TTL {
            return Err("CONFIRMATION_EXPIRED");
        }
        Ok(accepted)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejection_and_duplicate_callback_never_reopen_a_consumed_confirmation() {
        let mut gate = ConfirmationGate::default();
        let cancelled = gate.begin().unwrap();
        assert!(gate.begin().is_err());
        assert!(!gate.finish(&cancelled, false).unwrap());
        assert_eq!(gate.finish(&cancelled, true), Err("CONFIRMATION_STALE"));
        let accepted = gate.begin().unwrap();
        assert!(gate.finish(&accepted, true).unwrap());
        assert_eq!(gate.finish(&accepted, true), Err("CONFIRMATION_STALE"));
    }
    #[test]
    fn window_close_and_reopen_reject_the_old_callback_without_consuming_the_new_one() {
        let mut gate = ConfirmationGate::default();
        let old = gate.begin().unwrap();
        gate.invalidate();
        let new = gate.begin().unwrap();
        assert_eq!(gate.finish(&old, true), Err("CONFIRMATION_STALE"));
        assert!(gate.finish(&new, true).unwrap());
    }
    #[test]
    fn expired_confirmation_is_consumed_and_requires_a_fresh_preview() {
        let mut gate = ConfirmationGate::default();
        let mut expired = gate.begin().unwrap();
        expired.created -= Duration::from_secs(61);
        assert_eq!(gate.finish(&expired, true), Err("CONFIRMATION_EXPIRED"));
        assert_eq!(gate.finish(&expired, true), Err("CONFIRMATION_STALE"));
        let fresh = gate.begin().unwrap();
        assert!(gate.finish(&fresh, true).unwrap());
    }
}
