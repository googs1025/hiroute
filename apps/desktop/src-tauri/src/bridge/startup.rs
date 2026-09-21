use super::{DesktopState, Session};
use hiroute_diagnostics::context::DiagnosticHandle;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU64},
};
use tauri::{State, WebviewWindow};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct StartupState {
    pub(super) error: OnceLock<String>,
    pub(super) cancelled: AtomicBool,
    pub(super) data_root: Option<PathBuf>,
}

pub(super) fn start_session(
    diagnostics: DiagnosticHandle,
    data_root: Option<PathBuf>,
    initialize: impl FnOnce(&AtomicBool) -> Result<Session, String> + Send + 'static,
) -> DesktopState {
    let state = DesktopState(
        Arc::new(Mutex::new(None)),
        Arc::new(StartupState {
            data_root,
            ..StartupState::default()
        }),
        AtomicU64::new(0),
        diagnostics,
    );
    // Commands must await initialization without holding the WebView event loop.
    let mut session = state.0.clone().try_lock_owned().expect("new session lock");
    let startup = state.1.clone();
    tauri::async_runtime::spawn_blocking(move || match initialize(&startup.cancelled) {
        Ok(ready) => *session = Some(ready),
        Err(error) => {
            let _ = startup.error.set(error);
        }
    });
    state
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct StartupReport {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    recovery_available: bool,
}

#[tauri::command]
pub async fn startup_status(
    window: WebviewWindow,
    state: State<'_, DesktopState>,
) -> Result<StartupReport, String> {
    super::main_window(&window)?;
    if let Some(code) = state.1.error.get() {
        return Ok(StartupReport {
            state: "failed",
            code: Some(code.clone()),
            recovery_available: state.1.data_root.as_ref().is_some_and(|root| {
                root.is_absolute()
                    && hiroute_diagnostics::files::PrivateDir::open_existing(root).is_ok()
            }),
        });
    }
    let ready = state.0.try_lock().is_ok_and(|session| session.is_some());
    Ok(StartupReport {
        state: if ready { "ready" } else { "starting" },
        code: None,
        recovery_available: false,
    })
}

/// Opens the preserved application data root after startup failure. Recovery stays explicit:
/// this command never renames, deletes, migrates or rewrites the failed store.
#[tauri::command]
pub async fn open_startup_recovery_directory(
    window: WebviewWindow,
    state: State<'_, DesktopState>,
) -> Result<(), String> {
    super::main_window(&window)?;
    if state.1.error.get().is_none() {
        return Err("STARTUP_RECOVERY_NOT_REQUIRED".into());
    }
    let root = state
        .1
        .data_root
        .clone()
        .filter(|root| root.is_absolute())
        .ok_or("PRIVATE_PATH_UNAVAILABLE")?;
    hiroute_diagnostics::files::PrivateDir::open_existing(&root)
        .map_err(|_| "PRIVATE_PATH_INVALID")?;
    super::diagnostics::open_in_file_manager(&root)
        .map_err(|_| "STARTUP_RECOVERY_OPEN_FAILED".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[tokio::test]
    async fn startup_does_not_block_the_caller_or_expose_an_uninitialized_session() {
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let state = start_session(DiagnosticHandle::noop(), None, move |_| {
            let _ = entered.send(());
            wait.recv().unwrap();
            Err("DAEMON_READY_INVALID".into())
        });
        started.await.unwrap();
        assert!(state.0.try_lock().is_err());
        assert!(state.1.error.get().is_none());
        release.send(()).unwrap();
        let session = tokio::time::timeout(Duration::from_secs(2), state.0.lock())
            .await
            .unwrap();
        assert!(session.is_none());
        assert_eq!(
            state.1.error.get().map(String::as_str),
            Some("DAEMON_READY_INVALID")
        );
    }

    #[tokio::test]
    async fn exit_can_cancel_pending_startup_before_waiting_for_session_cleanup() {
        let (entered, started) = tokio::sync::oneshot::channel();
        let state = start_session(DiagnosticHandle::noop(), None, move |cancelled| {
            let _ = entered.send(());
            while !cancelled.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err("DAEMON_START_CANCELLED".into())
        });
        started.await.unwrap();
        state.1.cancelled.store(true, Ordering::SeqCst);
        let session = tokio::time::timeout(Duration::from_secs(2), state.0.lock())
            .await
            .unwrap();
        assert!(session.is_none());
        assert_eq!(
            state.1.error.get().map(String::as_str),
            Some("DAEMON_START_CANCELLED")
        );
    }
}
