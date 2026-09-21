use serde::{Deserialize, Serialize};
use tauri::{Manager, State, WebviewWindow};

use super::{DesktopFailure, DesktopState, main_window};

#[derive(Clone, Debug, Serialize)]
pub struct CliEntryView {
    #[serde(flatten)]
    status: crate::cli_entry::CliEntryStatus,
    daemon_available: bool,
}

async fn cli_view(state: State<'_, DesktopState>) -> Result<CliEntryView, DesktopFailure> {
    Ok(CliEntryView {
        status: crate::cli_entry::inspect_current()?,
        daemon_available: state.0.lock().await.is_some(),
    })
}

#[tauri::command]
pub async fn cli_entry_status(
    window: WebviewWindow,
    state: State<'_, DesktopState>,
) -> Result<CliEntryView, DesktopFailure> {
    main_window(&window)?;
    cli_view(state).await
}

#[tauri::command]
pub async fn cli_entry_install(
    window: WebviewWindow,
    state: State<'_, DesktopState>,
) -> Result<CliEntryView, DesktopFailure> {
    main_window(&window)?;
    crate::cli_entry::install_current()?;
    cli_view(state).await
}

#[tauri::command]
pub async fn cli_entry_remove(
    window: WebviewWindow,
    state: State<'_, DesktopState>,
) -> Result<CliEntryView, DesktopFailure> {
    main_window(&window)?;
    crate::cli_entry::remove_current()?;
    cli_view(state).await
}

#[derive(Clone, Debug, Serialize)]
pub struct GatewayListenerView {
    config: crate::gateway_address::GatewayListenerConfigV1,
    connect_address: Option<String>,
    suggested_agent_base_url: Option<String>,
    existing_agent_count: Option<usize>,
    ready: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum GatewayListenerScope {
    Local,
    External,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayListenerInput {
    scope: GatewayListenerScope,
    #[serde(default)]
    accept_remote_risk: bool,
}

fn root(window: &WebviewWindow) -> Result<std::path::PathBuf, DesktopFailure> {
    super::desktop_data_root(window.app_handle()).map_err(|_| "APP_DATA_UNAVAILABLE".into())
}

async fn gateway_view(
    window: &WebviewWindow,
    state: State<'_, DesktopState>,
) -> Result<GatewayListenerView, DesktopFailure> {
    let config = crate::gateway_address::status(&root(window)?)?;
    let connect_address = config
        .applied_address()
        .map_err(|error| error.to_string())?
        .and_then(|listen| hiroute_host_runtime::connect_address(listen).ok())
        .map(|address| address.to_string());
    let mut resident = state.0.lock().await;
    let ready = resident.is_some()
        && config
            .applied_address()
            .map_err(|error| error.to_string())?
            .is_some();
    let existing_agent_count = match resident.as_mut() {
        Some(session) => session.agent_snapshot().await.ok().map(|snapshot| {
            snapshot
                .agents
                .iter()
                .filter(|agent| agent.context_id.is_some())
                .count()
        }),
        None => None,
    };
    let suggested_agent_base_url = connect_address
        .as_ref()
        .map(|address| format!("http://{address}/v1"));
    Ok(GatewayListenerView {
        config,
        connect_address,
        suggested_agent_base_url,
        existing_agent_count,
        ready,
    })
}

#[tauri::command]
pub async fn gateway_listener_status(
    window: WebviewWindow,
    state: State<'_, DesktopState>,
) -> Result<GatewayListenerView, DesktopFailure> {
    main_window(&window)?;
    gateway_view(&window, state).await
}

#[tauri::command]
pub async fn gateway_listener_apply(
    window: WebviewWindow,
    input: GatewayListenerInput,
) -> Result<(), DesktopFailure> {
    main_window(&window)?;
    configure_listener(&root(&window)?, input)?;
    window.app_handle().restart()
}

fn configure_listener(
    root: &std::path::Path,
    input: GatewayListenerInput,
) -> Result<(), DesktopFailure> {
    if input.scope == GatewayListenerScope::External && !input.accept_remote_risk {
        return Err("REMOTE_LISTENER_CONFIRMATION_REQUIRED".into());
    }
    let mut desired = crate::gateway_address::status(root)?.desired;
    desired.address = match input.scope {
        GatewayListenerScope::Local => std::net::Ipv4Addr::LOCALHOST.to_string(),
        GatewayListenerScope::External => std::net::Ipv4Addr::UNSPECIFIED.to_string(),
    };
    crate::gateway_address::configure(root, desired)?;
    Ok(())
}

#[tauri::command]
pub async fn gateway_listener_recover(window: WebviewWindow) -> Result<(), DesktopFailure> {
    main_window(&window)?;
    crate::gateway_address::recover(&root(&window)?)?;
    window.app_handle().restart()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_scope_requires_explicit_risk_acknowledgment_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let input = GatewayListenerInput {
            scope: GatewayListenerScope::External,
            accept_remote_risk: false,
        };
        assert!(matches!(
            configure_listener(root.path(), input),
            Err(DesktopFailure::Native { code }) if code == "REMOTE_LISTENER_CONFIRMATION_REQUIRED"
        ));
        assert!(!crate::gateway_address::path(root.path()).exists());

        configure_listener(
            root.path(),
            GatewayListenerInput {
                accept_remote_risk: true,
                ..input
            },
        )
        .unwrap();
        let config = crate::gateway_address::status(root.path()).unwrap();
        assert_eq!(config.desired.address, "0.0.0.0");
        assert_eq!(
            config.desired.port_mode,
            crate::gateway_address::GatewayPortModeV1::Automatic
        );
        assert!(config.operation.unwrap().active());
    }

    #[test]
    fn scope_switch_preserves_the_current_port_choice() {
        let root = tempfile::tempdir().unwrap();
        crate::gateway_address::configure(
            root.path(),
            crate::gateway_address::GatewayListenerDesiredV1::fixed("127.0.0.1", 56272),
        )
        .unwrap();
        configure_listener(
            root.path(),
            GatewayListenerInput {
                scope: GatewayListenerScope::External,
                accept_remote_risk: true,
            },
        )
        .unwrap();
        let config = crate::gateway_address::status(root.path()).unwrap();
        assert_eq!(config.desired.address, "0.0.0.0");
        assert_eq!(config.desired.port, Some(56272));
        assert_eq!(
            config.desired.port_mode,
            crate::gateway_address::GatewayPortModeV1::Fixed
        );

        configure_listener(
            root.path(),
            GatewayListenerInput {
                scope: GatewayListenerScope::Local,
                accept_remote_risk: false,
            },
        )
        .unwrap();
        assert_eq!(
            crate::gateway_address::status(root.path())
                .unwrap()
                .desired
                .address,
            "127.0.0.1"
        );
    }

    #[test]
    fn desktop_apply_input_accepts_scopes_not_custom_addresses() {
        assert!(
            serde_json::from_value::<GatewayListenerInput>(serde_json::json!({
                "scope": "external", "accept_remote_risk": true
            }))
            .is_ok()
        );
        assert!(
            serde_json::from_value::<GatewayListenerInput>(serde_json::json!({
                "address": "192.0.2.10", "port_mode": "automatic", "accept_remote_risk": true
            }))
            .is_err()
        );
    }
}
