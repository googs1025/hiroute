//! The runtime one executable entry point owns: settings watch, writer and bounded shutdown.
//!
//! The runtime is started as early as the entry point can determine its diagnostics root,
//! before slow business initialization and without waiting for Local Control readiness.
//! It never takes business locks, and a failure to open files degrades diagnostics instead
//! of blocking or changing business execution.
//!
//! Starting has two phases: an in-memory phase that always completes immediately (handle,
//! queue, levels) and a filesystem phase that opens the root, reads settings, creates the
//! correlation key and spawns the writer. [`DiagnosticRuntime::start`] arms inline for a
//! background entry point (daemon, CLI, tests); [`DiagnosticRuntime::start_background`]
//! arms on a dedicated thread so a UI process never opens files during its setup, and a
//! first start that must create a missing application data root still completes there.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::context::{DiagnosticContext, DiagnosticHandle, EmitOutcome, Emitter};
use crate::error::SubsystemReason;
use crate::event::{
    DiagnosticEvent, HarnessKind, LevelApplied, ProcessRole, ProcessStart, StageBegin, StageEnd,
    StageOutcome, StartupStage, WorkerStage, WorkerStageKind, WorkerStageOutcome,
};
use crate::files::{PrivateDir, role_dir_name};
use crate::identity::{BootId, SessionId, TargetTriple, VersionToken};
use crate::level::{DiagnosticLevel, LevelSource};
use crate::maintenance::{self, MaintenanceContext};
use crate::queue::{QueueReceiver, QueueSender, bounded_queue};
use crate::record::Component;
use crate::settings::{SettingsError, SettingsStore};
use crate::shared::Shared;
use crate::writer::{WriterCompletion, WriterHandle, WriterRecordContext, spawn_writer};

/// How long shutdown waits for the writer and the maintenance thread together.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(1500);
/// The writer's own bounded drain budget inside [`SHUTDOWN_WAIT`].
const WRITER_DRAIN_WAIT: Duration = Duration::from_millis(1000);

/// Configuration for one process role. All platform/env parsing happens at the call site;
/// the crate never reads the environment itself.
pub struct RuntimeConfig {
    /// The diagnostics root directory (`D`).
    pub root: PathBuf,
    pub role: ProcessRole,
    pub component: Component,
    pub parent_session_id: Option<SessionId>,
    /// Smoke-test override; wins over the persisted setting and is never persisted.
    pub level_override: Option<DiagnosticLevel>,
}

#[derive(Debug, Clone, Copy)]
struct StageState {
    stage: StartupStage,
    started: Instant,
    budget_ms: Option<u64>,
}

#[derive(Debug)]
struct StageInner {
    handle: DiagnosticHandle,
    stage: Mutex<Option<StageState>>,
}

/// A cloneable stage reporter for code that receives diagnostics as a port instead of the
/// whole runtime. Begin must be emitted before the blocking operation so a hang still
/// leaves the last stage visible.
#[derive(Clone, Debug, Default)]
pub struct StageReporter {
    inner: Option<Arc<StageInner>>,
}

impl StageReporter {
    pub fn begin(&self, stage: StartupStage) {
        self.begin_with_budget(stage, None);
    }

    pub fn begin_with_budget(&self, stage: StartupStage, budget_ms: Option<u64>) {
        let Some(inner) = &self.inner else {
            return;
        };
        *inner.stage.lock().expect("stage lock") = Some(StageState {
            stage,
            started: Instant::now(),
            budget_ms,
        });
        inner
            .handle
            .try_emit(DiagnosticEvent::StageBegin(StageBegin { stage }));
    }

    pub fn end(&self, stage: StartupStage, outcome: StageOutcome) {
        let Some(inner) = &self.inner else {
            return;
        };
        let elapsed_ms = {
            let mut current = inner.stage.lock().expect("stage lock");
            let elapsed = current
                .filter(|state| state.stage == stage)
                .map(|state| state.started.elapsed().as_millis() as u64)
                .unwrap_or(0);
            *current = None;
            elapsed
        };
        inner.handle.try_emit(DiagnosticEvent::StageEnd(StageEnd {
            stage,
            elapsed_ms,
            outcome,
        }));
    }

    pub fn current(&self) -> Option<(StartupStage, Instant, Option<u64>)> {
        let inner = self.inner.as_ref()?;
        let current = *inner.stage.lock().expect("stage lock");
        current.map(|state| (state.stage, state.started, state.budget_ms))
    }
}

#[derive(Debug, Clone, Copy)]
struct WorkerStageState {
    harness: HarnessKind,
    stage: WorkerStageKind,
    started: Instant,
}

#[derive(Debug)]
struct WorkerStageInner {
    handle: DiagnosticHandle,
    current: Mutex<Option<WorkerStageState>>,
}

/// Tracks the one Worker installation step the calling thread is inside. `begin` must run
/// before the blocking call so a paused probe still has its step readable, and so the
/// maintenance heartbeat can republish the same step (`progress`) without the blocked thread
/// doing any work.
///
/// This snapshot is deliberately separate from [`StageReporter`]: an inner probe step never
/// clears the outer startup stage, and the outer stage's end never clears the probe step.
#[derive(Clone, Debug, Default)]
pub struct WorkerStageReporter {
    inner: Option<Arc<WorkerStageInner>>,
}

impl WorkerStageReporter {
    pub fn begin(&self, harness: HarnessKind, stage: WorkerStageKind) {
        let Some(inner) = &self.inner else {
            return;
        };
        *inner.current.lock().expect("worker stage lock") = Some(WorkerStageState {
            harness,
            stage,
            started: Instant::now(),
        });
        inner
            .handle
            .try_emit(DiagnosticEvent::WorkerStage(WorkerStage {
                harness,
                stage,
                outcome: WorkerStageOutcome::Entered,
                elapsed_ms: 0,
            }));
    }

    pub fn end(&self, harness: HarnessKind, stage: WorkerStageKind, outcome: WorkerStageOutcome) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut current = inner.current.lock().expect("worker stage lock");
        let is_current =
            |state: &WorkerStageState| state.harness == harness && state.stage == stage;
        let elapsed_ms = current
            .filter(is_current)
            .map(|state| state.started.elapsed().as_millis() as u64)
            .unwrap_or(0);
        if current.as_ref().is_some_and(is_current) {
            *current = None;
        }
        // Emitted under the same lock that `progress` reads with, so a heartbeat line for this
        // step can never be ordered after its real end.
        inner
            .handle
            .try_emit(DiagnosticEvent::WorkerStage(WorkerStage {
                harness,
                stage,
                outcome,
                elapsed_ms,
            }));
    }

    /// A fact observed inside the running step, measured against that step's entry. The step
    /// stays current afterwards.
    pub fn note(&self, harness: HarnessKind, stage: WorkerStageKind) {
        let Some(inner) = &self.inner else {
            return;
        };
        let current = inner.current.lock().expect("worker stage lock");
        let elapsed_ms = current
            .filter(|state| state.harness == harness)
            .map(|state| state.started.elapsed().as_millis() as u64)
            .unwrap_or(0);
        inner
            .handle
            .try_emit(DiagnosticEvent::WorkerStage(WorkerStage {
                harness,
                stage,
                outcome: WorkerStageOutcome::Observed,
                elapsed_ms,
            }));
    }

    /// Re-emit the current step as `Entered`. Called from the maintenance heartbeat only, so a
    /// step that is blocked produces bounded progress without depending on the blocked thread.
    pub(crate) fn progress(&self) {
        let Some(inner) = &self.inner else {
            return;
        };
        let current = inner.current.lock().expect("worker stage lock");
        let Some(state) = *current else {
            return;
        };
        inner
            .handle
            .try_emit(DiagnosticEvent::WorkerStage(WorkerStage {
                harness: state.harness,
                stage: state.stage,
                outcome: WorkerStageOutcome::Entered,
                elapsed_ms: state.started.elapsed().as_millis() as u64,
            }));
    }
}

/// The subset of diagnostics a business module may use: the typed handle for allowed
/// events plus stage reporting. Cloneable and independent of the runtime lifetime.
#[derive(Clone, Debug, Default)]
pub struct DiagnosticsPort {
    handle: DiagnosticHandle,
    stages: StageReporter,
    worker_stages: WorkerStageReporter,
}

impl DiagnosticsPort {
    pub fn handle(&self) -> &DiagnosticHandle {
        &self.handle
    }

    /// Emit one typed event without a request context.
    pub fn emit(&self, event: DiagnosticEvent) -> EmitOutcome {
        self.handle.try_emit(event)
    }

    pub fn stage_begin(&self, stage: StartupStage) {
        self.stages.begin(stage);
    }

    pub fn stage_begin_with_budget(&self, stage: StartupStage, budget_ms: Option<u64>) {
        self.stages.begin_with_budget(stage, budget_ms);
    }

    pub fn stage_end(&self, stage: StartupStage, outcome: StageOutcome) {
        self.stages.end(stage, outcome);
    }

    /// Record one Worker installation step as entered. Must run before the blocking call.
    pub fn worker_stage_begin(&self, harness: HarnessKind, stage: WorkerStageKind) {
        self.worker_stages.begin(harness, stage);
    }

    /// Record the real end of the current Worker installation step.
    pub fn worker_stage_end(
        &self,
        harness: HarnessKind,
        stage: WorkerStageKind,
        outcome: WorkerStageOutcome,
    ) {
        self.worker_stages.end(harness, stage, outcome);
    }

    /// Record a fact observed inside the running step without ending it.
    pub fn worker_stage_note(&self, harness: HarnessKind, stage: WorkerStageKind) {
        self.worker_stages.note(harness, stage);
    }
}

/// The filesystem side of one runtime. It settles exactly once: either the root, settings
/// and writer are open, or a controlled reason explains why they are not.
struct ArmFiles {
    state: Mutex<FilesState>,
    settle: Condvar,
}

#[derive(Default)]
struct FilesState {
    settled: bool,
    owns_role: bool,
    /// Why this runtime has no usable settings store at all; `None` while starting or
    /// healthy. A runtime with a store reports settings-file findings through the cached
    /// persisted view instead.
    unavailable: Option<SubsystemReason>,
    role_dir: Option<Arc<PrivateDir>>,
    settings: Option<Arc<SettingsStore>>,
    writer: Option<WriterHandle>,
}

/// Everything the filesystem phase needs, so it can run inline or on its own thread.
struct ArmPlan {
    config: RuntimeConfig,
    shared: Arc<Shared>,
    started_at: Instant,
    emitter: Option<Arc<Emitter>>,
    handle: DiagnosticHandle,
    stages: StageReporter,
    worker_stages: WorkerStageReporter,
    boot_id: Option<BootId>,
    sender: QueueSender,
    receiver: QueueReceiver,
    files: Arc<ArmFiles>,
    maintenance_completion: Arc<WriterCompletion>,
}

/// One role's running diagnostics. Dropping the runtime without [`DiagnosticRuntime::shutdown`]
/// leaves at most the bounded writer thread alive; hosts call `shutdown` on exit.
pub struct DiagnosticRuntime {
    handle: DiagnosticHandle,
    stages: StageReporter,
    worker_stages: WorkerStageReporter,
    started: Instant,
    emitter: Option<Arc<Emitter>>,
    shared: Arc<Shared>,
    files: Arc<ArmFiles>,
    queue: Option<QueueSender>,
    override_active: bool,
    maintenance_completion: Arc<WriterCompletion>,
}

/// In-memory snapshot for the native status command: the last settings read this process
/// performed, and whether it has a usable settings store at all.
#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    /// The last successfully read settings revision; 0 before any successful read.
    pub revision: u64,
    /// The last successfully read settings level; the build-specific runtime default
    /// applies before the first read.
    pub level: DiagnosticLevel,
    /// The finding of the most recent settings read; a failed read keeps the values above
    /// and only marks the error.
    pub settings_error: Option<SubsystemReason>,
    /// Why this runtime has no usable settings store (unsafe root, lost role, failed
    /// start). `None` while starting or healthy.
    pub unavailable: Option<SubsystemReason>,
    /// Whether the filesystem phase finished; false means the report is still starting.
    pub armed: bool,
    pub override_active: bool,
}

impl DiagnosticRuntime {
    /// Start diagnostics for this process and open its files on the calling thread.
    /// Always returns a runtime; file or platform failures degrade the report instead of
    /// failing the process.
    pub fn start(config: RuntimeConfig) -> Self {
        let (runtime, plan) = Self::prepare(config);
        arm(plan);
        runtime
    }

    /// Start diagnostics without touching the filesystem on the calling thread. The
    /// in-memory emit plane exists immediately; the root, settings, key and writer are
    /// opened on a dedicated thread, so a slow or missing application data root can never
    /// block the caller. [`DiagnosticRuntime::wait_armed`] bounds any later wait.
    pub fn start_background(config: RuntimeConfig) -> Self {
        let (runtime, plan) = Self::prepare(config);
        let files = plan.files.clone();
        let spawned = std::thread::Builder::new()
            .name("hiroute-diagnostics-init".to_string())
            .spawn(move || arm(plan))
            .is_ok();
        if !spawned {
            let sender = runtime.queue.clone();
            settle_without_files(&files, sender.as_ref(), SubsystemReason::WriterOpenFailed);
        }
        runtime
    }

    /// A runtime with no usable root: no filesystem work happens, every event is discarded
    /// and `path_unsafe` explains the state. Used where no safe root exists at all, so the
    /// report stays queryable instead of the process inventing a relative location.
    pub fn unavailable(config: RuntimeConfig) -> Self {
        let (runtime, plan) = Self::prepare(config);
        settle_without_files(&plan.files, Some(&plan.sender), SubsystemReason::PathUnsafe);
        runtime
    }

    fn prepare(config: RuntimeConfig) -> (Self, ArmPlan) {
        let shared = Arc::new(Shared::new());
        let (level, source) = match config.level_override {
            Some(level) => (level, LevelSource::SmokeOverride),
            None => (DiagnosticLevel::runtime_default(), LevelSource::Default),
        };
        shared.set_level_state(level, source, 0);
        let boot_id = BootId::random().ok();
        let started = Instant::now();
        let (sender, receiver) = bounded_queue();
        let emitter = boot_id.map(|boot_id| {
            Arc::new(Emitter::new(
                sender.clone(),
                config.level_override,
                config.component,
                config.role,
                boot_id,
                config.parent_session_id,
                started,
                shared.sequence(),
                shared.counters.clone(),
            ))
        });
        let handle = match &emitter {
            Some(emitter) => DiagnosticHandle::attached(emitter.clone()),
            None => DiagnosticHandle::noop(),
        };
        let stages = StageReporter {
            inner: Some(Arc::new(StageInner {
                handle: handle.clone(),
                stage: Mutex::new(None),
            })),
        };
        let worker_stages = WorkerStageReporter {
            inner: Some(Arc::new(WorkerStageInner {
                handle: handle.clone(),
                current: Mutex::new(None),
            })),
        };
        let files = Arc::new(ArmFiles {
            state: Mutex::new(FilesState::default()),
            settle: Condvar::new(),
        });
        let maintenance_completion = Arc::new(WriterCompletion::default());
        let runtime = Self {
            handle: handle.clone(),
            stages: stages.clone(),
            worker_stages: worker_stages.clone(),
            started,
            emitter: emitter.clone(),
            shared: shared.clone(),
            files: files.clone(),
            queue: Some(sender.clone()),
            override_active: config.level_override.is_some(),
            maintenance_completion: maintenance_completion.clone(),
        };
        let plan = ArmPlan {
            config,
            shared,
            started_at: started,
            emitter,
            handle,
            stages,
            worker_stages,
            boot_id,
            sender,
            receiver,
            files,
            maintenance_completion,
        };
        (runtime, plan)
    }

    pub fn handle(&self) -> &DiagnosticHandle {
        &self.handle
    }

    pub fn root_context(&self) -> DiagnosticContext {
        self.handle.root_context()
    }

    pub fn component(&self) -> Component {
        self.emitter
            .as_ref()
            .map(|emitter| emitter.component())
            .unwrap_or(Component::Diagnostics)
    }

    pub fn boot_id(&self) -> Option<BootId> {
        self.emitter.as_ref().map(|emitter| emitter.boot_id())
    }

    pub fn parent_session_id(&self) -> Option<SessionId> {
        self.emitter
            .as_ref()
            .and_then(|emitter| emitter.parent_session_id())
    }

    /// The settings store once the filesystem phase finished successfully.
    pub fn settings(&self) -> Option<Arc<SettingsStore>> {
        self.files
            .state
            .lock()
            .expect("files lock")
            .settings
            .clone()
    }

    pub fn role_dir(&self) -> Option<Arc<PrivateDir>> {
        self.files
            .state
            .lock()
            .expect("files lock")
            .role_dir
            .clone()
    }

    pub fn override_active(&self) -> bool {
        self.override_active
    }

    /// Whether this runtime won its role's writer lock. A host that lost the claim may keep
    /// reporting in memory but must not write that role's files, including its settings.
    pub fn owns_role(&self) -> bool {
        self.files.state.lock().expect("files lock").owns_role
    }

    pub fn armed(&self) -> bool {
        self.files.state.lock().expect("files lock").settled
    }

    /// Wait up to `timeout` for the filesystem phase to settle. Returns immediately when it
    /// already did, including when it settled with an error.
    pub fn wait_armed(&self, timeout: Duration) -> bool {
        let state = self.files.state.lock().expect("files lock");
        if state.settled {
            return true;
        }
        let (state, _) = self
            .files
            .settle
            .wait_timeout(state, timeout)
            .expect("files lock");
        state.settled
    }

    /// Milliseconds since this runtime started, used for `startup_end`.
    pub fn started_elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Update the Desktop's effective level immediately after a successful save. The
    /// daemon learns the same revision through its own settings watch.
    pub fn apply_saved_level(&self, level: DiagnosticLevel, revision: u64) {
        if self.override_active {
            return;
        }
        self.set_level_everywhere(level, LevelSource::Persisted, revision);
        self.shared.set_persisted_read(revision, level, None);
        self.handle
            .try_emit(DiagnosticEvent::LevelApplied(LevelApplied {
                level,
                revision,
                source: LevelSource::Persisted,
            }));
    }

    pub(crate) fn set_level_everywhere(
        &self,
        level: DiagnosticLevel,
        source: LevelSource,
        revision: u64,
    ) {
        self.shared.set_level_state(level, source, revision);
        if let Some(emitter) = &self.emitter {
            emitter.set_level(level, revision);
        }
    }

    /// Mark a startup stage as entered. Must be called before the blocking operation so a
    /// hang still leaves the last stage visible.
    pub fn stage_begin(&self, stage: StartupStage) {
        self.stages.begin(stage);
    }

    pub fn stage_begin_with_budget(&self, stage: StartupStage, budget_ms: Option<u64>) {
        self.stages.begin_with_budget(stage, budget_ms);
    }

    /// Mark a startup stage as finished and forget the current stage.
    pub fn stage_end(&self, stage: StartupStage, outcome: StageOutcome) {
        self.stages.end(stage, outcome);
    }

    /// A cloneable port for business modules: the typed handle plus stage reporting.
    pub fn port(&self) -> DiagnosticsPort {
        DiagnosticsPort {
            handle: self.handle.clone(),
            stages: self.stages.clone(),
            worker_stages: self.worker_stages.clone(),
        }
    }

    pub fn record_ready_io(&self, event: DiagnosticEvent) {
        self.handle.try_emit(event);
    }

    /// Report a controlled degradation as a bounded log event. Repeated reports of the
    /// same reason are ignored so a persistent fault stays one record.
    pub fn note_degraded(&self, reason: SubsystemReason) {
        self.handle.try_emit(DiagnosticEvent::DiagnosticsDegraded(
            crate::event::DiagnosticsDegraded {
                reason,
                os_errno: None,
            },
        ));
    }

    /// The bounded in-memory snapshot of the last settings read. No file is read and no
    /// lock a save holds is taken, so a status query stays responsive while settings I/O is
    /// in flight.
    pub fn status(&self) -> RuntimeStatus {
        let persisted = self.shared.persisted();
        let (unavailable, armed) = {
            let files = self.files.state.lock().expect("files lock");
            let reason = files.unavailable.or_else(|| {
                // Another process owns this role's files: this runtime can never read a
                // level of its own from them.
                (files.settled && !files.owns_role).then_some(SubsystemReason::WriterOwned)
            });
            (reason, files.settled)
        };
        RuntimeStatus {
            revision: persisted.map(|view| view.revision).unwrap_or(0),
            level: persisted
                .map(|view| view.level)
                .unwrap_or_else(DiagnosticLevel::runtime_default),
            settings_error: persisted.and_then(|view| view.error),
            unavailable,
            armed,
            override_active: self.override_active,
        }
    }

    /// Emit an event from the entry point without a request context.
    pub fn emit(&self, event: DiagnosticEvent) -> EmitOutcome {
        self.handle.try_emit(event)
    }

    /// The bounded queue's current occupancy, for a host that wants to include diagnostics
    /// pressure in its own reporting. Reads no file and never blocks on the writer.
    pub fn queue_snapshot(&self) -> Option<crate::queue::QueueSnapshot> {
        self.emitter
            .as_ref()
            .map(|emitter| emitter.queue_snapshot())
    }

    /// Signal shutdown and wait a bounded time for the writer to drain. The caller performs
    /// no file I/O itself, so a hung filesystem cannot extend the exit budget. Only the
    /// writer thread, which holds the role lock for its whole life, writes the role's files;
    /// once it finished, nothing writes there again.
    pub fn shutdown(&self) {
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        self.shared.shutdown.store(true, Ordering::SeqCst);
        if let Some(queue) = &self.queue {
            queue.close();
        }
        let (writer, owns_role) = {
            let files = self.files.state.lock().expect("files lock");
            (files.writer.clone(), files.owns_role)
        };
        if let Some(writer) = &writer {
            let budget = WRITER_DRAIN_WAIT.min(deadline.saturating_duration_since(Instant::now()));
            writer.completion.wait(budget);
        }
        if owns_role {
            self.maintenance_completion
                .wait(deadline.saturating_duration_since(Instant::now()));
        }
    }
}

impl Drop for DiagnosticRuntime {
    fn drop(&mut self) {
        if !self.shared.shutdown.load(Ordering::SeqCst) {
            self.shutdown();
        }
    }
}

/// Open the root, read settings, create the correlation key and start the writer. Every
/// failure settles the runtime with a controlled reason instead of blocking or panicking.
fn arm(plan: ArmPlan) {
    let root = match PrivateDir::open_or_create(&plan.config.root) {
        Ok(root) => root,
        Err(error) => {
            return settle_without_files(
                &plan.files,
                Some(&plan.sender),
                reason_for_file_error(&error),
            );
        }
    };
    let role_dir = match root.child_dir(role_dir_name(plan.config.role)) {
        Ok(dir) => dir,
        Err(error) => {
            return settle_without_files(
                &plan.files,
                Some(&plan.sender),
                reason_for_file_error(&error),
            );
        }
    };
    let settings = Arc::new(SettingsStore::new(root));
    let (tokenizer, key_error) = match settings.ensure_correlation_key() {
        Ok(key) => (Some(crate::correlation::Tokenizer::available(key)), None),
        Err(error) => (None, Some(SubsystemReason::from_settings_error(error))),
    };
    let snapshot = settings.load();
    let level_source = snapshot.level_source();
    let settings_error = snapshot.error.map(SubsystemReason::from_settings_error);
    plan.shared.set_persisted_read(
        snapshot.settings.revision,
        snapshot.settings.level,
        settings_error,
    );
    if let (Some(emitter), Some(tokenizer)) = (&plan.emitter, tokenizer) {
        emitter.set_tokenizer(tokenizer);
    }
    let (level, source) = match plan.config.level_override {
        // A smoke override wins over the persisted value and is never persisted.
        Some(level) => (level, LevelSource::SmokeOverride),
        None => (snapshot.settings.level, level_source),
    };
    plan.shared
        .set_level_state(level, source, snapshot.settings.revision);
    if let Some(emitter) = &plan.emitter {
        // The first installed level also judges the records that were admitted while no
        // level was known; this runs before the writer starts.
        emitter.set_level(level, snapshot.settings.revision);
    }
    // The startup log states the level this process uses, and why, for every start.
    plan.handle
        .try_emit(DiagnosticEvent::LevelApplied(LevelApplied {
            level,
            revision: snapshot.settings.revision,
            source,
        }));
    if let Some(reason) = key_error {
        plan.handle.try_emit(DiagnosticEvent::DiagnosticsDegraded(
            crate::event::DiagnosticsDegraded {
                reason,
                os_errno: None,
            },
        ));
    }
    let role_dir = Arc::new(role_dir);
    let process_emitter = plan.emitter.clone();
    let process_handle = plan.handle.clone();
    let process_role = plan.config.role;
    let record_context = plan.boot_id.map(|boot_id| {
        WriterRecordContext::new(
            plan.config.component,
            boot_id,
            plan.emitter
                .as_ref()
                .and_then(|emitter| emitter.parent_session_id()),
            plan.started_at,
            plan.shared.clone(),
        )
    });
    let writer = spawn_writer(
        role_dir.clone(),
        plan.receiver,
        plan.shared.writer_counters.clone(),
        plan.shared.writer_health.clone(),
        record_context,
    );
    let owns_role = writer.owns_role();
    {
        let mut state = plan.files.state.lock().expect("files lock");
        state.settled = true;
        state.owns_role = owns_role;
        state.role_dir = Some(role_dir.clone());
        state.settings = Some(settings.clone());
        state.writer = Some(writer.clone());
    }
    plan.files.settle.notify_all();
    emit_process_start(process_emitter.as_ref(), &process_handle, process_role);
    if plan.boot_id.is_none() {
        // No process identity: nothing may be written under this role's files.
        plan.files.state.lock().expect("files lock").unavailable =
            Some(SubsystemReason::WriterOpenFailed);
        plan.maintenance_completion.mark_finished();
        return;
    }
    if !owns_role {
        // Another process owns this role's files, including the settings it reads. This
        // runtime keeps reporting its own memory state but writes nothing.
        plan.maintenance_completion.mark_finished();
        return;
    }
    let context = MaintenanceContext {
        shared: plan.shared.clone(),
        emitter: plan.emitter.clone(),
        stages: plan.stages.clone(),
        worker_stages: plan.worker_stages.clone(),
        settings,
        override_active: plan.config.level_override.is_some(),
        writer: Some(writer),
        completion: plan.maintenance_completion.clone(),
    };
    if maintenance::spawn(context).is_none() {
        plan.files.state.lock().expect("files lock").unavailable =
            Some(SubsystemReason::WriterOpenFailed);
        plan.maintenance_completion.mark_finished();
    }
}

fn emit_process_start(
    emitter: Option<&Arc<Emitter>>,
    handle: &DiagnosticHandle,
    role: ProcessRole,
) {
    let Some(emitter) = emitter else {
        return;
    };
    handle.try_emit(DiagnosticEvent::ProcessStart(ProcessStart {
        role,
        version: VersionToken::current_package(),
        source_revision: VersionToken::source_revision(),
        target: TargetTriple::current(),
        parent_session_id: emitter.parent_session_id(),
    }));
}

/// Settle this runtime without files. Events are dropped instead of piling up unread:
/// the queue is closed so every later emit fails fast and is counted.
fn settle_without_files(
    files: &Arc<ArmFiles>,
    sender: Option<&QueueSender>,
    reason: SubsystemReason,
) {
    {
        let mut state = files.state.lock().expect("files lock");
        if state.settled {
            return;
        }
        state.settled = true;
        state.unavailable = Some(reason);
    }
    if let Some(sender) = sender {
        sender.close();
    }
    files.settle.notify_all();
}

/// Convenience for hosts that only need the settings error mapping.
pub fn settings_reason(error: SettingsError) -> SubsystemReason {
    SubsystemReason::from_settings_error(error)
}

fn reason_for_file_error(error: &crate::files::FileSafetyError) -> SubsystemReason {
    match error {
        crate::files::FileSafetyError::UnsupportedPlatform => SubsystemReason::UnsupportedPlatform,
        crate::files::FileSafetyError::UnsafeDirectory
        | crate::files::FileSafetyError::UnsafeFile => SubsystemReason::PathUnsafe,
        _ => SubsystemReason::WriterOpenFailed,
    }
}

#[cfg(all(test, unix))]
mod tests;
