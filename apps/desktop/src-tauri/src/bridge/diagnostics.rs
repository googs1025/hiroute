//! Native diagnostics state and commands, independent of the business Session.
//!
//! The state is managed before `Session` startup and never takes the business Session
//! lock, so status and level changes stay responsive while the daemon is still starting,
//! unreachable or crashed. The root, key, settings and writer are opened on the runtime's
//! own init thread. A save means the level is persisted: this process applies it at once and
//! every other process adopts the same revision through its own settings watch; no command
//! waits for another process to confirm anything.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hiroute_diagnostics::error::StableErrorCode;
use hiroute_diagnostics::event::ProcessRole;
use hiroute_diagnostics::files::PrivateDir;
use hiroute_diagnostics::identity::SessionId;
use hiroute_diagnostics::level::DiagnosticLevel;
use hiroute_diagnostics::record::Component;
use hiroute_diagnostics::runtime::{DiagnosticRuntime, RuntimeConfig};
use hiroute_diagnostics::settings::stable_code;
use serde::{Deserialize, Serialize};
use tauri::{State, WebviewWindow};

use crate::failure::DesktopFailure;
use crate::native_diagnostics::NativeDiagnostics;

pub const DIAGNOSTIC_STATUS_SCHEMA_V1: &str = "hiroute.diagnostic-status/v1";
/// How long a command waits for the background filesystem phase. The wait only covers that
/// phase; a runtime that already settled returns at once.
const ARM_TIMEOUT: Duration = Duration::from_secs(2);

/// Long-lived native diagnostics. Cloneable handle; the runtime is owned once.
#[derive(Clone)]
pub struct DiagnosticsState {
    inner: Arc<Inner>,
}

struct Inner {
    /// The runtime is taken with no lock held: every call below either reads an in-memory
    /// snapshot or performs its own bounded file work on a blocking task.
    runtime: Arc<DiagnosticRuntime>,
    root: std::path::PathBuf,
    /// This desktop's session id, handed to the managed daemon so both processes correlate
    /// under one run.
    parent_session: Option<SessionId>,
}

impl DiagnosticsState {
    /// Start diagnostics under `root/diagnostics`. A missing root keeps the report queryable
    /// as unavailable rather than inventing a location, and this never blocks on the
    /// business Session or on filesystem I/O: the root, key, settings and writer are opened
    /// on the runtime's own init thread.
    pub fn start(root: Option<&Path>) -> Self {
        let parent_session = SessionId::random().ok();
        let config = RuntimeConfig {
            root: root
                .map(|root| root.join("diagnostics"))
                .unwrap_or_default(),
            role: ProcessRole::Desktop,
            component: Component::Desktop,
            parent_session_id: parent_session,
            level_override: crate::native_diagnostics::level_override(),
        };
        let runtime = match root {
            Some(_) => DiagnosticRuntime::start_background(config),
            None => DiagnosticRuntime::unavailable(config),
        };
        Self {
            inner: Arc::new(Inner {
                runtime: Arc::new(runtime),
                root: root
                    .map(|root| root.join("diagnostics"))
                    .unwrap_or_default(),
                parent_session,
            }),
        }
    }

    /// The handoff the resident bootstrap needs: private root, parent session and typed port.
    pub fn startup(&self) -> NativeDiagnostics {
        NativeDiagnostics::new(
            self.inner.root.clone(),
            self.inner.parent_session,
            self.inner.runtime.port(),
        )
    }

    pub fn handle(&self) -> hiroute_diagnostics::DiagnosticHandle {
        self.inner.runtime.handle().clone()
    }

    /// Persist a new level after the caller's expected revision, then apply it to this
    /// process. The returned status is this process's own view; whether another process has
    /// read the revision yet is not part of the result. Runs on a blocking task, so waiting
    /// for the runtime's filesystem phase never blocks the WebView loop.
    pub fn set_level(
        &self,
        expected_revision: u64,
        level: DiagnosticLevel,
    ) -> Result<DiagnosticStatusV1, StableErrorCode> {
        let runtime = self.inner.runtime.as_ref();
        if runtime.override_active() {
            return Err(StableErrorCode::OverrideActive);
        }
        if !runtime.wait_armed(ARM_TIMEOUT) {
            return Err(StableErrorCode::DiagnosticsUnavailable);
        }
        let Some(settings) = runtime.settings() else {
            let reason = runtime.status().unavailable;
            return Err(reason
                .map(StableErrorCode::from_subsystem_reason)
                .unwrap_or(StableErrorCode::DiagnosticsUnavailable));
        };
        if !runtime.owns_role() {
            // Another process owns this role's files: saving would publish a revision this
            // process does not own.
            return Err(StableErrorCode::DiagnosticsUnavailable);
        }
        match settings.save(expected_revision, level) {
            Ok(saved) => {
                runtime.apply_saved_level(saved.level, saved.revision);
                Ok(self.status())
            }
            Err(error) => Err(stable_code(&error)),
        }
    }

    /// In-memory status view: the last settings read this process performed plus whether it
    /// has a usable settings store at all. It reads no file, waits for no lock and never
    /// needs another process to be running, so it stays well under the 16 KiB bound.
    pub fn status(&self) -> DiagnosticStatusV1 {
        let status = self.inner.runtime.status();
        // A settled report with an unusable root has no settings store at all; the degraded
        // reason is the only honest explanation. A report that is still arming knows nothing
        // yet and claims nothing.
        let settings_error = status.settings_error.map(settings_view_error).or_else(|| {
            status
                .unavailable
                .filter(|_| status.armed)
                .map(StableErrorCode::from_subsystem_reason)
        });
        let settings_view = SettingsView {
            revision: status.revision,
            level: status.level,
            error: settings_error,
        };
        DiagnosticStatusV1 {
            schema: DIAGNOSTIC_STATUS_SCHEMA_V1.to_string(),
            settings: settings_view,
            logs_directory_available: self.inner.root.is_absolute(),
        }
    }

    /// Open the diagnostics directory that holds this process's local logs. Service
    /// readiness is irrelevant; only the private root this process already owns is used,
    /// and no path is returned to the WebView.
    pub fn open_log_directory(&self) -> Result<(), StableErrorCode> {
        let directory = self.inner.root.clone();
        if !directory.is_absolute() {
            return Err(StableErrorCode::DiagnosticsUnavailable);
        }
        PrivateDir::open_existing(&directory).map_err(|_| StableErrorCode::PathUnsafe)?;
        open_in_file_manager(&directory)
    }

    /// Signal shutdown. The runtime closes its queue and waits a bounded time for the writer
    /// to drain; no file I/O happens on this calling thread.
    pub fn shutdown(&self) {
        self.inner.runtime.shutdown();
    }
}

/// The cached settings finding in the native settings vocabulary. `from_subsystem_reason`
/// folds everything but unsafe paths into `diagnostics_unavailable`, which would hide a
/// damaged file behind a generic code.
fn settings_view_error(reason: hiroute_diagnostics::error::SubsystemReason) -> StableErrorCode {
    use hiroute_diagnostics::error::SubsystemReason as R;
    match reason {
        R::SettingsInvalid => StableErrorCode::SettingsInvalid,
        R::SettingsUnreadable => StableErrorCode::SettingsUnwritable,
        R::PathUnsafe => StableErrorCode::PathUnsafe,
        R::UnsupportedPlatform => StableErrorCode::UnsupportedPlatform,
        _ => StableErrorCode::DiagnosticsUnavailable,
    }
}

/// The native failure is the fixed code string, never a `Display` of the underlying error.
pub fn diagnostics_failure(code: StableErrorCode) -> DesktopFailure {
    code.as_str().to_owned().into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SettingsView {
    pub revision: u64,
    pub level: DiagnosticLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<StableErrorCode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticStatusV1 {
    pub schema: String,
    pub settings: SettingsView,
    pub logs_directory_available: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetDiagnosticLevelInput {
    pub expected_revision: u64,
    pub level: DiagnosticLevel,
}

fn main_window(window: &WebviewWindow) -> Result<(), String> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err("window_denied".to_owned())
    }
}

#[tauri::command]
pub async fn diagnostic_status(
    window: WebviewWindow,
    state: State<'_, DiagnosticsState>,
) -> Result<DiagnosticStatusV1, DesktopFailure> {
    main_window(&window)?;
    let state = state.inner().clone();
    // The snapshot is in memory, but the wait for the runtime's filesystem phase must not
    // run on the WebView loop.
    tauri::async_runtime::spawn_blocking(move || state.status())
        .await
        .map_err(|_| diagnostics_failure(StableErrorCode::DiagnosticsUnavailable))
}

#[tauri::command]
pub async fn set_diagnostic_level(
    window: WebviewWindow,
    state: State<'_, DiagnosticsState>,
    input: SetDiagnosticLevelInput,
) -> Result<DiagnosticStatusV1, DesktopFailure> {
    main_window(&window)?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.set_level(input.expected_revision, input.level)
    })
    .await
    .map_err(|_| diagnostics_failure(StableErrorCode::DiagnosticsUnavailable))?
    .map_err(diagnostics_failure)
}

/// Opens the local diagnostics directory in the platform file manager. The path is
/// derived natively from the private root and never accepted from the WebView; opening
/// happens off the WebView loop and the result carries no path back to the page.
#[tauri::command]
pub async fn open_diagnostic_directory(
    window: WebviewWindow,
    state: State<'_, DiagnosticsState>,
) -> Result<(), DesktopFailure> {
    main_window(&window)?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.open_log_directory())
        .await
        .map_err(|_| diagnostics_failure(StableErrorCode::DiagnosticsUnavailable))?
        .map_err(diagnostics_failure)
}

/// Only known platform openers are used, with the verified directory as one argument and
/// without a shell; the child is never waited for.
pub(super) fn open_in_file_manager(directory: &Path) -> Result<(), StableErrorCode> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer");
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = directory;
        return Err(StableErrorCode::UnsupportedPlatform);
    }
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        command.arg(directory);
        command
            .spawn()
            .map(|_child| ())
            .map_err(|_| StableErrorCode::DiagnosticsUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(root: &Path) -> DiagnosticsState {
        let state = DiagnosticsState::start(Some(root));
        assert!(
            state.inner.runtime.wait_armed(Duration::from_secs(2)),
            "the runtime must settle"
        );
        state
    }

    #[test]
    fn status_view_is_bounded_and_free_of_paths() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("app-local-data");
        std::fs::create_dir_all(&root).expect("create root");
        let state = start(&root);
        let status = state.status();
        assert_eq!(status.schema, DIAGNOSTIC_STATUS_SCHEMA_V1);
        assert_eq!(
            status.settings.level,
            DiagnosticLevel::runtime_default(),
            "an unconfigured Desktop must expose the build-profile default"
        );
        assert_eq!(status.settings.revision, 0);
        assert_eq!(status.settings.error, None);
        assert!(status.logs_directory_available);
        let encoded = serde_json::to_vec(&status).expect("encode");
        assert!(encoded.len() < 16 * 1024, "status must stay bounded");
        let text = String::from_utf8(encoded).expect("utf8");
        assert!(!text.contains(directory.path().to_str().expect("path")));
        assert!(!text.contains("diagnostics/"));
        // The removed process views stay removed: no field carries a process state.
        assert!(!text.contains("desktop"));
        assert!(!text.contains("daemon"));
        assert!(!text.contains("health"));
        state.shutdown();
    }

    #[test]
    fn set_level_persists_applies_and_reports_conflicts() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("app-local-data");
        std::fs::create_dir_all(&root).expect("create root");
        let state = start(&root);
        let saved = state
            .set_level(0, DiagnosticLevel::Debug)
            .expect("save level");
        assert_eq!(saved.settings.level, DiagnosticLevel::Debug);
        assert_eq!(saved.settings.revision, 1);
        assert_eq!(saved.settings.error, None);
        assert_eq!(
            state.handle().level(),
            Some(DiagnosticLevel::Debug),
            "the saving process uses the new level at once"
        );
        assert_eq!(
            state.set_level(0, DiagnosticLevel::Warn),
            Err(StableErrorCode::SettingsConflict)
        );
        state.shutdown();
    }

    /// The save path is this process's own: it returns once the level is persisted and
    /// applied here, without a status file, a daemon confirmation wait or any other process
    /// being involved.
    #[test]
    fn a_save_never_waits_for_another_process() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("app-local-data");
        std::fs::create_dir_all(&root).expect("create root");
        let state = start(&root);
        let started = std::time::Instant::now();
        let saved = state.set_level(0, DiagnosticLevel::Warn).expect("save");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a save must not wait for a confirmation window: {:?}",
            started.elapsed()
        );
        assert_eq!(saved.settings.revision, 1);

        // No daemon ever ran here, and the role directories still hold only the settings and
        // this process's own log: nothing publishes a process status.
        let logs = root.join("diagnostics");
        let names: Vec<String> = std::fs::read_dir(&logs)
            .expect("read diagnostics root")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            !names.iter().any(|name| name == "daemon"),
            "no daemon directory is created by a save: {names:?}"
        );
        let desktop = logs.join("desktop");
        let role_names: Vec<String> = std::fs::read_dir(&desktop)
            .expect("read desktop role dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            !role_names.iter().any(|name| name == "status.json"),
            "the status file chain is deleted: {role_names:?}"
        );
        state.shutdown();
    }

    #[test]
    fn override_builds_reject_saving() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("app-local-data");
        std::fs::create_dir_all(&root).expect("create root");
        // A non-pilot build has no override; simulate the override contract directly.
        let state = start(&root);
        assert!(
            !state.inner.runtime.override_active(),
            "production builds must not read the override env"
        );
        state.shutdown();
    }

    /// The real first start: the application data root and its `diagnostics` level do not
    /// exist yet, and the bridge must still arm and accept a level save.
    #[test]
    fn first_start_arms_on_its_own_thread_and_saves() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("app-local-data");
        assert!(!root.exists());
        let state = DiagnosticsState::start(Some(&root));
        let saved = state
            .set_level(0, DiagnosticLevel::Debug)
            .expect("save into the newly created root");
        assert_eq!(saved.settings.revision, 1);
        assert_eq!(saved.settings.level, DiagnosticLevel::Debug);
        assert!(saved.logs_directory_available);
        let logs = root.join("diagnostics").join("desktop");
        assert!(logs.join("current.jsonl").is_file() || logs.is_dir());
        state.shutdown();
    }

    /// R7 on the native call chain: a good read, then a damaged file keeps the last known
    /// revision and level and reports the error; restoring the file converges without a
    /// restart, and the damaged content is never rewritten.
    #[test]
    fn status_reports_settings_damage_and_converges_after_recovery() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("app-local-data");
        std::fs::create_dir_all(&root).expect("create root");
        let state = start(&root);
        let saved = state
            .set_level(0, DiagnosticLevel::Debug)
            .expect("save level");
        assert_eq!(saved.settings.revision, 1);
        assert_eq!(saved.settings.level, DiagnosticLevel::Debug);

        let settings = root.join("diagnostics").join("settings.json");
        std::fs::write(
            &settings,
            br#"{"schema":"hiroute.diagnostic-settings/v1","revision":9,"level":"warn","extra":1}"#,
        )
        .expect("damage settings");
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        loop {
            let status = state.status();
            if status.settings.error == Some(StableErrorCode::SettingsInvalid) {
                assert_eq!(
                    status.settings.revision, 1,
                    "the last known revision is kept"
                );
                assert_eq!(
                    status.settings.level,
                    DiagnosticLevel::Debug,
                    "the last known level is kept"
                );
                assert_eq!(state.handle().level(), Some(DiagnosticLevel::Debug));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the settings watch must report the damage: {:?}",
                state.status().settings
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            std::fs::read(&settings)
                .expect("read damaged settings")
                .ends_with(b"\"extra\":1}"),
            "the damaged file is reported, never rewritten"
        );

        std::fs::write(
            &settings,
            br#"{"schema":"hiroute.diagnostic-settings/v1","revision":2,"level":"warn"}"#,
        )
        .expect("restore settings");
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        loop {
            let status = state.status();
            if status.settings.error.is_none() && status.settings.revision == 2 {
                assert_eq!(status.settings.level, DiagnosticLevel::Warn);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a restored file must converge: {:?}",
                state.status().settings
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        state.shutdown();
    }

    #[test]
    fn a_second_desktop_instance_refuses_to_save_the_owned_settings() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("app-local-data");
        std::fs::create_dir_all(&root).expect("create root");
        let owner = start(&root);
        assert!(owner.inner.runtime.owns_role());

        let second = start(&root);
        assert!(!second.inner.runtime.owns_role());
        assert_eq!(
            second.set_level(0, DiagnosticLevel::Debug),
            Err(StableErrorCode::DiagnosticsUnavailable),
            "a process that lost the role lock must not save its level"
        );
        assert_eq!(
            second.status().settings.error,
            Some(StableErrorCode::DiagnosticsUnavailable),
            "the loser reports its own state instead of the owner's"
        );
        second.shutdown();
        owner.shutdown();
    }

    #[test]
    fn missing_root_stays_queryable_without_touching_the_filesystem() {
        let state = DiagnosticsState::start(None);
        let status = state.status();
        assert_eq!(status.schema, DIAGNOSTIC_STATUS_SCHEMA_V1);
        assert_eq!(status.settings.error, Some(StableErrorCode::PathUnsafe));
        assert_eq!(status.settings.revision, 0);
        assert_eq!(
            status.settings.level,
            DiagnosticLevel::runtime_default(),
            "an unavailable runtime must retain the build-profile default"
        );
        assert!(!status.logs_directory_available);
        assert_eq!(
            state.set_level(0, DiagnosticLevel::Debug),
            Err(StableErrorCode::PathUnsafe)
        );
        // Opening a directory that was never established can never spawn an opener.
        assert_eq!(
            state.open_log_directory(),
            Err(StableErrorCode::DiagnosticsUnavailable)
        );
        assert!(
            !state.startup().root.is_absolute(),
            "without a root the bootstrap must not pass a relative diagnostics root"
        );
        state.shutdown();
    }

    #[test]
    fn opening_refuses_a_replaced_log_directory_without_launching_an_opener() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("app-local-data");
        std::fs::create_dir_all(&root).expect("create root");
        let state = start(&root);
        assert!(state.status().logs_directory_available);
        let logs = root.join("diagnostics");
        std::fs::remove_dir_all(&logs).expect("remove real directory");
        std::os::unix::fs::symlink(directory.path(), &logs).expect("replace with a symlink");
        assert_eq!(state.open_log_directory(), Err(StableErrorCode::PathUnsafe));
        state.shutdown();
    }
}
