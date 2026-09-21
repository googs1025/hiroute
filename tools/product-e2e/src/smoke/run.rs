use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use super::{
    Result, SmokeError,
    build::{self, Artifact, Source},
    digest, nonce,
    process::private_file,
    registry::Case,
    report::{CaseReport, Execution, Report, State, Step},
    require,
};

pub(super) struct Context<'a> {
    pub source: &'a Source,
    pub run_id: &'a str,
    pub private: PathBuf,
    pub runtime: PathBuf,
    pub deadline: Instant,
    pub cancel: Arc<AtomicBool>,
    pub report: &'a mut CaseReport,
    pub artifacts: &'a mut Vec<Artifact>,
    pub phase: &'static str,
}
impl Context<'_> {
    pub fn enter(&mut self, step: &'static str) {
        self.phase = step;
    }
    pub fn record(
        &mut self,
        id: &str,
        exit: Option<i32>,
        signal: Option<i32>,
        evidence: &Value,
    ) -> Result<()> {
        self.report.steps.push(Step {
            id: id.into(),
            process_exit: exit,
            signal,
            evidence_digest: digest(&serde_json::to_vec(evidence)?),
        });
        Ok(())
    }
    pub fn build(&mut self, package: &str, binary: &str) -> Result<Artifact> {
        if let Some(artifact) = self
            .artifacts
            .iter()
            .find(|a| a.package == package && a.binary == binary)
            .cloned()
        {
            self.verify_artifact(&artifact)?;
            return Ok(artifact);
        }
        let started = Instant::now();
        let artifact = build::get_or_build(
            self.source,
            package,
            binary,
            &self.private.join(format!("build-{package}")),
            self.cancel.clone(),
        );
        self.report.build_ms += started.elapsed().as_millis();
        let artifact = artifact?;
        self.artifacts.push(artifact.clone());
        Ok(artifact)
    }
    pub fn product_deadline(&mut self) {
        self.deadline = Instant::now() + Duration::from_secs(30);
    }
    pub fn verify_artifact(&mut self, artifact: &Artifact) -> Result<()> {
        let started = Instant::now();
        let outcome = artifact.verify(self.source);
        // Integrity hashing is runner evidence collection, not time spent waiting on
        // the product. Preserve the product budget while retaining the before/after
        // executable identity checks around every launched command.
        let elapsed = started.elapsed();
        self.report.integrity_ms += elapsed.as_millis();
        self.deadline = extend_deadline(self.deadline, elapsed)?;
        outcome
    }
    pub fn step_deadline(&self) -> Instant {
        self.deadline.min(Instant::now() + Duration::from_secs(10))
    }
}

fn extend_deadline(deadline: Instant, excluded: Duration) -> Result<Instant> {
    deadline
        .checked_add(excluded)
        .ok_or(SmokeError("deadline_overflow"))
}

fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)?;
    Ok(())
}

pub(super) fn lock_smoke(root: &Path) -> Result<(PathBuf, std::fs::File)> {
    let target = root.join("target");
    require(
        !fs::symlink_metadata(&target)?.file_type().is_symlink(),
        "external_target_forbidden",
    )?;
    let smoke = target.join("smoke");
    match private_dir(&smoke) {
        Ok(()) => {}
        Err(_) if smoke.exists() => {
            require(
                !fs::symlink_metadata(&smoke)?.file_type().is_symlink()
                    && fs::metadata(&smoke)?.permissions().mode() & 0o077 == 0,
                "unsafe_artifact_root",
            )?;
        }
        Err(e) => return Err(e),
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(smoke.join("run.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| SmokeError("checkout_smoke_busy"))?;
    Ok((smoke, lock))
}

/// Run only registered cases. A report can be produced even when source or environment preflight fails.
pub fn run(
    root: &Path,
    domains: Vec<String>,
    ids: Vec<String>,
    cancel: Arc<AtomicBool>,
) -> Result<(PathBuf, Report)> {
    let cases = super::select(&domains, &ids)?;
    let timestamp = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .map_err(|_| SmokeError("clock_before_epoch"))
    };
    let started_unix_ms = timestamp()?;
    let root = root.canonicalize()?;
    let (smoke, _lock) = lock_smoke(&root)?;
    let run_id = nonce()?;
    let directory = smoke.join(&run_id);
    private_dir(&directory)?;
    let private = directory.join("private");
    private_dir(&private)?;
    let mut journal = private_file(&private.join("run.journal"))?;
    writeln!(journal, "running {run_id}")?;
    journal.sync_all()?;
    let source = Source::capture(&root);
    let mut report = Report {
        schema: "hiroute.smoke.result/v1".into(),
        run_id: run_id.clone(),
        tool_sha256: digest(&fs::read(std::env::current_exe()?)?),
        started_unix_ms,
        finished_unix_ms: started_unix_ms,
        scenario_timeout_ms: 30_000,
        step_timeout_ms: 10_000,
        source_revision: source.as_ref().ok().map(|s| s.revision.clone()),
        source_tree: source.as_ref().ok().map(|s| s.tree.clone()),
        build_input_digest: source.as_ref().ok().map(|s| s.inputs.clone()),
        registry_digest: digest(&serde_json::to_vec(&cases)?),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        requested_domains: domains,
        requested_cases: ids,
        expected_cases: cases.iter().map(|c| c.id.into()).collect(),
        actual_case_count: 0,
        cases: vec![],
        artifacts: vec![],
        tool_process_exit: 1,
        rust_test_process_exit: None,
        capability_gaps: vec![
            "agent_connection_restore:not_verified".into(),
            "desktop:not_verified".into(),
            "real_accounts:not_verified".into(),
            "installation:not_verified".into(),
            "control_publication_success:not_verified".into(),
        ],
    };
    // The managed runner has already prepared every recipe. Separate private
    // directories and runtime roots let the three real cases execute together.
    // Unprepared standalone runs remain serial because they may build Cargo
    // recipes while holding this checkout's writer lock.
    let parallel = std::env::var_os("HIROUTE_SMOKE_REQUIRE_PREPARED").is_some() && cases.len() > 1;
    let results = if parallel {
        std::thread::scope(|scope| {
            let workers = cases
                .into_iter()
                .map(|case| {
                    let cancel = cancel.clone();
                    let private = &private;
                    let source = &source;
                    let run_id = &run_id;
                    scope.spawn(move || execute_case(case, source, run_id, private, cancel))
                })
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .map(|worker| {
                    worker
                        .join()
                        .map_err(|_| SmokeError("scenario_thread_panicked"))?
                })
                .collect::<Result<Vec<_>>>()
        })?
    } else {
        cases
            .into_iter()
            .map(|case| execute_case(case, &source, &run_id, &private, cancel.clone()))
            .collect::<Result<Vec<_>>>()?
    };
    for (case, artifacts) in results {
        if case.execution != Execution::NotExecuted {
            report.actual_case_count += 1;
        }
        for artifact in artifacts {
            if let Some(existing) = report
                .artifacts
                .iter()
                .find(|known| known.package == artifact.package && known.binary == artifact.binary)
            {
                require(existing == &artifact, "conflicting_case_artifact")?;
            } else {
                report.artifacts.push(artifact);
            }
        }
        report.cases.push(case);
    }
    report.tool_process_exit = if report.cases.iter().all(|c| c.state == Some(State::Green)) {
        0
    } else {
        1
    };
    report.finished_unix_ms = timestamp()?;
    report.verify()?;
    let output = directory.join("report.json");
    private_file(&output)?.write_all(&serde_json::to_vec_pretty(&report)?)?;
    writeln!(journal, "finished {}", report.tool_process_exit)?;
    journal.sync_all()?;
    Ok((output, report))
}

fn execute_case(
    case: Case,
    source: &Result<Source>,
    run_id: &str,
    private: &Path,
    cancel: Arc<AtomicBool>,
) -> Result<(CaseReport, Vec<Artifact>)> {
    let mut result = empty_case(case);
    let mut artifacts = Vec::new();
    if let Some(reason) = case.unavailable {
        result.reason = Some(reason.into());
    } else if cancel.load(Ordering::SeqCst) {
        result.reason = Some("cancelled_before_case".into());
    } else if let Err(error) = source {
        result.reason = Some(error.0.into());
    } else if let (Ok(source), Some(execute)) = (source, case.execute) {
        result.execution = Execution::Executed;
        let case_private = private.join(case.id);
        private_dir(&case_private)?;
        // Unix socket paths must remain short even in deeply nested worktrees.
        // Retain failed runtimes for diagnosis; only the private journal names them.
        let runtime = tempfile::Builder::new()
            .prefix("hs-")
            .tempdir_in(std::env::temp_dir().canonicalize()?)?
            .keep();
        private_file(&case_private.join("runtime-location"))?
            .write_all(runtime.as_os_str().as_encoded_bytes())?;
        let started = Instant::now();
        let mut context = Context {
            source,
            run_id,
            private: case_private,
            runtime: runtime.clone(),
            deadline: Instant::now() + Duration::from_secs(30),
            cancel: cancel.clone(),
            report: &mut result,
            artifacts: &mut artifacts,
            phase: "preflight",
        };
        let outcome = execute(&mut context).and_then(|()| {
            let started = Instant::now();
            let result = source.verify();
            context.report.integrity_ms += started.elapsed().as_millis();
            result
        });
        let phase = context.phase;
        let remaining_ms = started
            .elapsed()
            .as_millis()
            .saturating_sub(result.build_ms);
        if result.timing_complete {
            result.execution_ms = remaining_ms;
        } else {
            result.unclassified_ms = remaining_ms;
        }
        match outcome {
            Ok(()) => match fs::remove_dir_all(runtime) {
                Ok(()) => {
                    result.state = Some(State::Green);
                    result.cleanup = "complete".into();
                }
                Err(_) => {
                    result.state = Some(State::Red);
                    result.reason = Some("cleanup_failed".into());
                    result.cleanup = "needs_attention".into();
                }
            },
            Err(error) => {
                result.state = Some(State::Red);
                result.reason = Some(format!("{phase}:{}", error.0));
                result.cleanup = "needs_attention".into();
                if cancel.load(Ordering::SeqCst) {
                    result.execution = Execution::Interrupted;
                }
            }
        }
    }
    Ok((result, artifacts))
}

fn empty_case(case: Case) -> CaseReport {
    CaseReport {
        id: case.id.into(),
        domain: case.domain.into(),
        scope: case.scope.into(),
        owner: case.owner.into(),
        execution: Execution::NotExecuted,
        state: None,
        reason: None,
        steps: vec![],
        build_ms: 0,
        execution_ms: 0,
        integrity_ms: 0,
        unclassified_ms: 0,
        timing_complete: true,
        cleanup: "not_started".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_deadline_excludes_runner_integrity_time() {
        let deadline = Instant::now() + Duration::from_secs(30);
        let integrity_time = Duration::from_secs(7);
        assert_eq!(
            extend_deadline(deadline, integrity_time).unwrap(),
            deadline + integrity_time
        );
    }
}
