//! Guard contract for future domain-owned formal Agent adapters. Never a product verdict.
use super::{Result, require};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::time::timeout;

#[derive(Debug)]
pub struct AgentIsolation {
    pub configuration_roots: Vec<PathBuf>,
    pub working_directory: PathBuf,
    pub configuration_precedence_proven: bool,
    pub environment_allowlist_proven: bool,
    pub launch_arguments_proven: bool,
    pub formal_restore_available: bool,
}
impl AgentIsolation {
    pub fn validate(&self, private_root: &Path) -> Result<()> {
        require(
            self.configuration_precedence_proven
                && self.environment_allowlist_proven
                && self.launch_arguments_proven
                && self.formal_restore_available,
            "agent_isolation_unproven",
        )?;
        require(
            !self.configuration_roots.is_empty(),
            "agent_configuration_roots_missing",
        )?;
        let root = private_root.canonicalize()?;
        for path in self
            .configuration_roots
            .iter()
            .chain(std::iter::once(&self.working_directory))
        {
            let actual = path.canonicalize()?;
            require(
                actual.starts_with(&root) && actual != root,
                "agent_path_outside_isolation",
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RestoreState {
    NotRequired,
    Restored,
    Conflict,
    NeedsAttention,
}

/// Domain implementations must use public product operations; backups are not restoration.
#[allow(async_fn_in_trait)]
pub trait FormalAgentOperations {
    async fn preview_apply_and_call(&mut self) -> Result<()>;
    fn may_have_applied(&self) -> bool;
    async fn restore_preview_confirm_apply(&mut self) -> Result<RestoreState>;
    async fn verify_restored_fields_and_unknown_fields(&mut self) -> Result<()>;
    async fn cleanup_owned_resources(&mut self) -> Result<()>;
}

#[derive(Debug)]
pub struct AgentGuardResult {
    pub operation_error: Option<&'static str>,
    pub restore: RestoreState,
    pub cleanup_complete: bool,
}

/// Component lifecycle only. This function cannot issue a production smoke green.
pub async fn guarded_agent_run(
    adapter: &mut impl FormalAgentOperations,
    isolation: &AgentIsolation,
    private_root: &Path,
    cancel: Arc<AtomicBool>,
) -> Result<AgentGuardResult> {
    guarded_with_bound(
        adapter,
        isolation,
        private_root,
        Duration::from_secs(30),
        cancel,
    )
    .await
}

async fn guarded_with_bound(
    adapter: &mut impl FormalAgentOperations,
    isolation: &AgentIsolation,
    private_root: &Path,
    bound: Duration,
    cancel: Arc<AtomicBool>,
) -> Result<AgentGuardResult> {
    isolation.validate(private_root)?;
    let operation_error = if cancel.load(Ordering::SeqCst) {
        Some("cancelled")
    } else {
        let cancelled = async {
            while !cancel.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::select! {
            result = timeout(bound, adapter.preview_apply_and_call()) => result
                .unwrap_or(Err(super::SmokeError("agent_operation_timeout"))).err().map(|e| e.0),
            _ = cancelled => Some("cancelled"),
        }
    };
    let restore = if adapter.may_have_applied() {
        let restore_and_verify = async {
            match adapter.restore_preview_confirm_apply().await? {
                RestoreState::Restored => {
                    adapter.verify_restored_fields_and_unknown_fields().await?;
                    Ok(RestoreState::Restored)
                }
                RestoreState::Conflict => Ok(RestoreState::Conflict),
                _ => Ok(RestoreState::NeedsAttention),
            }
        };
        timeout(bound, restore_and_verify)
            .await
            .unwrap_or(Err(super::SmokeError("agent_restore_timeout")))
            .unwrap_or(RestoreState::NeedsAttention)
    } else {
        RestoreState::NotRequired
    };
    // Do not discard the domain's recovery context after conflict or uncertain restoration.
    let cleanup_complete = matches!(restore, RestoreState::Restored | RestoreState::NotRequired)
        && matches!(
            timeout(
                bound.min(Duration::from_secs(10)),
                adapter.cleanup_owned_resources()
            )
            .await,
            Ok(Ok(()))
        );
    Ok(AgentGuardResult {
        operation_error,
        restore,
        cleanup_complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Adapter {
        calls: Vec<&'static str>,
        conflict: bool,
    }
    impl FormalAgentOperations for Adapter {
        async fn preview_apply_and_call(&mut self) -> Result<()> {
            self.calls.push("apply");
            Err(super::super::SmokeError("cancelled"))
        }
        fn may_have_applied(&self) -> bool {
            true
        }
        async fn restore_preview_confirm_apply(&mut self) -> Result<RestoreState> {
            self.calls.push("restore");
            Ok(if self.conflict {
                RestoreState::Conflict
            } else {
                RestoreState::Restored
            })
        }
        async fn verify_restored_fields_and_unknown_fields(&mut self) -> Result<()> {
            self.calls.push("verify");
            Ok(())
        }
        async fn cleanup_owned_resources(&mut self) -> Result<()> {
            self.calls.push("cleanup");
            Ok(())
        }
    }
    #[tokio::test]
    async fn restoration_precedes_cleanup_and_conflict_preserves_recovery_context() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("home");
        std::fs::create_dir(&path).unwrap();
        let isolation = AgentIsolation {
            configuration_roots: vec![path.clone()],
            working_directory: path,
            configuration_precedence_proven: true,
            environment_allowlist_proven: true,
            launch_arguments_proven: true,
            formal_restore_available: true,
        };
        let mut adapter = Adapter {
            calls: vec![],
            conflict: false,
        };
        let result = guarded_agent_run(
            &mut adapter,
            &isolation,
            temp.path(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(result.operation_error, Some("cancelled"));
        assert!(result.cleanup_complete);
        assert_eq!(adapter.calls, ["apply", "restore", "verify", "cleanup"]);
        adapter.calls.clear();
        adapter.conflict = true;
        assert!(
            !guarded_agent_run(
                &mut adapter,
                &isolation,
                temp.path(),
                Arc::new(AtomicBool::new(false))
            )
            .await
            .unwrap()
            .cleanup_complete
        );
        assert_eq!(adapter.calls, ["apply", "restore"]);
    }
    #[tokio::test]
    async fn missing_restore_or_escaping_config_rejects_before_any_operation() {
        let temp = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut input = AgentIsolation {
            configuration_roots: vec![other.path().into()],
            working_directory: other.path().into(),
            configuration_precedence_proven: true,
            environment_allowlist_proven: true,
            launch_arguments_proven: true,
            formal_restore_available: false,
        };
        let mut adapter = Adapter {
            calls: vec![],
            conflict: false,
        };
        assert!(
            guarded_agent_run(
                &mut adapter,
                &input,
                temp.path(),
                Arc::new(AtomicBool::new(false))
            )
            .await
            .is_err()
        );
        input.formal_restore_available = true;
        assert!(
            guarded_agent_run(
                &mut adapter,
                &input,
                temp.path(),
                Arc::new(AtomicBool::new(false))
            )
            .await
            .is_err()
        );
        assert!(adapter.calls.is_empty());
    }
    #[tokio::test]
    async fn hung_restore_keeps_recovery_context_and_never_cleans_as_success() {
        struct Hung;
        impl FormalAgentOperations for Hung {
            async fn preview_apply_and_call(&mut self) -> Result<()> {
                Ok(())
            }
            fn may_have_applied(&self) -> bool {
                true
            }
            async fn restore_preview_confirm_apply(&mut self) -> Result<RestoreState> {
                std::future::pending().await
            }
            async fn verify_restored_fields_and_unknown_fields(&mut self) -> Result<()> {
                panic!("restore never finished")
            }
            async fn cleanup_owned_resources(&mut self) -> Result<()> {
                panic!("must retain recovery evidence")
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("isolated");
        std::fs::create_dir(&home).unwrap();
        let input = AgentIsolation {
            configuration_roots: vec![home.clone()],
            working_directory: home,
            configuration_precedence_proven: true,
            environment_allowlist_proven: true,
            launch_arguments_proven: true,
            formal_restore_available: true,
        };
        let result = guarded_with_bound(
            &mut Hung,
            &input,
            temp.path(),
            Duration::from_millis(10),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(result.restore, RestoreState::NeedsAttention);
        assert!(!result.cleanup_complete);
    }
    #[tokio::test]
    async fn actual_cancellation_and_success_both_restore_before_cleanup() {
        struct Changing {
            cancel: Arc<AtomicBool>,
            cancel_during_call: bool,
            fail_verification: bool,
            applied: bool,
            restored: bool,
            verified: bool,
            unknown: String,
        }
        impl FormalAgentOperations for Changing {
            async fn preview_apply_and_call(&mut self) -> Result<()> {
                self.applied = true;
                if self.cancel_during_call {
                    self.cancel.store(true, Ordering::SeqCst);
                    std::future::pending().await
                } else {
                    Ok(())
                }
            }
            fn may_have_applied(&self) -> bool {
                self.applied
            }
            async fn restore_preview_confirm_apply(&mut self) -> Result<RestoreState> {
                assert!(self.applied);
                self.restored = true;
                Ok(RestoreState::Restored)
            }
            async fn verify_restored_fields_and_unknown_fields(&mut self) -> Result<()> {
                assert!(self.restored);
                assert_eq!(self.unknown, "daily-user-field");
                if self.fail_verification {
                    return Err(super::super::SmokeError("restored_fields_mismatch"));
                }
                self.verified = true;
                Ok(())
            }
            async fn cleanup_owned_resources(&mut self) -> Result<()> {
                assert!(self.verified);
                Ok(())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let isolation = AgentIsolation {
            configuration_roots: vec![home.clone()],
            working_directory: home,
            configuration_precedence_proven: true,
            environment_allowlist_proven: true,
            launch_arguments_proven: true,
            formal_restore_available: true,
        };
        for (cancel_during_call, fail_verification) in
            [(false, false), (true, false), (false, true)]
        {
            let cancel = Arc::new(AtomicBool::new(false));
            let mut adapter = Changing {
                cancel: cancel.clone(),
                cancel_during_call,
                fail_verification,
                applied: false,
                restored: false,
                verified: false,
                unknown: "daily-user-field".into(),
            };
            let result = guarded_agent_run(&mut adapter, &isolation, temp.path(), cancel)
                .await
                .unwrap();
            assert_eq!(
                result.operation_error,
                cancel_during_call.then_some("cancelled")
            );
            assert_eq!(
                result.restore,
                if fail_verification {
                    RestoreState::NeedsAttention
                } else {
                    RestoreState::Restored
                }
            );
            assert_eq!(result.cleanup_complete, !fail_verification);
        }
    }
    #[tokio::test]
    async fn every_unproven_configuration_layer_leaves_daily_configuration_untouched() {
        let private = tempfile::tempdir().unwrap();
        let home = private.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let daily = tempfile::tempdir().unwrap();
        let sentinel = daily.path().join("settings.json");
        let original = br#"{"unknown":"daily-secret-sentinel"}"#;
        std::fs::write(&sentinel, original).unwrap();
        for missing in 0..4 {
            let isolation = AgentIsolation {
                configuration_roots: vec![home.clone()],
                working_directory: home.clone(),
                configuration_precedence_proven: missing != 0,
                environment_allowlist_proven: missing != 1,
                launch_arguments_proven: missing != 2,
                formal_restore_available: missing != 3,
            };
            let mut adapter = Adapter {
                calls: vec![],
                conflict: false,
            };
            let error = guarded_agent_run(
                &mut adapter,
                &isolation,
                private.path(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap_err();
            assert_eq!(error.0, "agent_isolation_unproven");
            assert!(adapter.calls.is_empty());
            assert_eq!(std::fs::read(&sentinel).unwrap(), original);
        }
    }
}
