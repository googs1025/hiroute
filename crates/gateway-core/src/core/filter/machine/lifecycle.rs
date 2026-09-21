use super::*;

impl DirectionMachine {
    pub(super) fn resolve_callback<T>(
        &mut self,
        callback: NativeCallbackResult<T>,
    ) -> Result<T, FilterError> {
        match callback {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => {
                self.enter_failing_terminal();
                Err(error)
            }
            Ok(Err(_)) => {
                self.callback_panicked = true;
                self.observe_error(ErrorClass::CallbackPanic);
                self.enter_failing_terminal();
                Err(FilterError::CallbackPanic)
            }
            Err(ExecutorError::DeadlineExceeded) => {
                self.observe_error(ErrorClass::Deadline);
                self.enter_failing_terminal();
                Err(FilterError::CallbackDeadline)
            }
            Err(ExecutorError::Cancelled | ExecutorError::ScopeFinalized) => {
                self.observe_error(ErrorClass::Cancelled);
                self.enter_failing_terminal();
                Err(FilterError::CallbackCancelled)
            }
            Err(error) => {
                self.enter_failing_terminal();
                Err(FilterError::Callback(error.to_string().into()))
            }
        }
    }

    pub(super) fn discard_pending(&mut self) {
        while let Some(frame) = self.pending.pop_front() {
            if let PendingFrame::Data { backing, .. } = frame {
                match backing {
                    PendingBodyBacking::Retained(retained) => {
                        let _ = self.retention.discard(retained);
                    }
                    PendingBodyBacking::Charged { retention, bytes } => {
                        let _ = self.retention.release_charged(retention);
                        drop(bytes);
                    }
                    PendingBodyBacking::EndStreamControl => {}
                }
            }
        }
        self.dropped_runtime_owners.clear();
    }

    pub(super) fn enter_failing_terminal(&mut self) {
        self.terminal = true;
        self.pauses.clear();
        self.pause_receivers.clear();
        self.pause_epoch = self.pause_epoch.wrapping_add(1);
        self.discard_pending();
        self.retention.set_read_paused(false);
        self.callback_scope.cancel();
    }

    pub(super) fn set_terminal(&mut self, reply: LocalReply) -> MachineOutcome {
        self.terminal = true;
        self.pauses.clear();
        self.pause_receivers.clear();
        self.pause_epoch = self.pause_epoch.wrapping_add(1);
        self.discard_pending();
        self.retention.set_read_paused(false);
        self.callback_scope.cancel();
        MachineOutcome::LocalReply(reply)
    }

    pub(super) fn ensure_active(&self) -> Result<(), FilterError> {
        if self.finalized || self.terminal {
            return Err(FilterError::MachineTerminal);
        }
        Ok(())
    }

    pub fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        self.discard_pending();
        self.pauses.clear();
        self.pause_receivers.clear();
        self.pause_epoch = self.pause_epoch.wrapping_add(1);
        self.retention.set_read_paused(false);
        self.callback_scope.cancel();
        for slot in &mut self.slots {
            if !slot.finalized {
                slot.finalized = true;
                if catch_unwind(AssertUnwindSafe(|| slot.filter.on_finalize())).is_err() {
                    self.callback_panicked = true;
                }
            }
        }
        let finalize_count = self.slots.iter().filter(|slot| slot.finalized).count();
        self.finalized = true;
        self.terminal = true;
        if let Some(telemetry) = &self.telemetry {
            telemetry.scope(
                self.scope_kind,
                self.scope_id,
                ScopePhase::Finalized,
                finalize_count,
                Duration::ZERO,
            );
        }
    }

    pub async fn finalize_bounded(&mut self, join_timeout: Duration) -> Result<(), FilterError> {
        self.finalize();
        self.callback_scope
            .cancel_and_finalize(join_timeout)
            .await
            .map_err(filter_executor_error)
    }

    pub(super) fn observe_scope(&self, phase: ScopePhase, latency: Duration) {
        if let Some(telemetry) = &self.telemetry {
            telemetry.scope(self.scope_kind, self.scope_id, phase, 0, latency);
        }
    }

    pub(super) fn observe_error(&self, class: ErrorClass) {
        if let Some(telemetry) = &self.telemetry {
            telemetry.error(class);
        }
    }
}

impl Drop for DirectionMachine {
    fn drop(&mut self) {
        self.finalize();
    }
}
