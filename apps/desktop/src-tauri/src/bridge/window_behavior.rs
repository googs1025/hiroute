use crate::failure::DesktopFailure;
use tauri::WebviewWindow;

use super::main_window;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TitlebarDoubleClickAction {
    Maximize,
    Minimize,
    None,
}

fn titlebar_double_click_action(value: &str) -> TitlebarDoubleClickAction {
    match value.trim().to_ascii_lowercase().as_str() {
        "minimize" => TitlebarDoubleClickAction::Minimize,
        "none" => TitlebarDoubleClickAction::None,
        _ => TitlebarDoubleClickAction::Maximize,
    }
}

#[tauri::command]
pub async fn perform_titlebar_double_click(window: WebviewWindow) -> Result<(), DesktopFailure> {
    main_window(&window)?;
    #[cfg(target_os = "macos")]
    let action = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleActionOnDoubleClick"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or(TitlebarDoubleClickAction::Maximize, |value| {
            titlebar_double_click_action(&value)
        });
    #[cfg(not(target_os = "macos"))]
    let action = TitlebarDoubleClickAction::Maximize;

    match action {
        TitlebarDoubleClickAction::Maximize => window
            .is_maximized()
            .and_then(|maximized| {
                if maximized {
                    window.unmaximize()
                } else {
                    window.maximize()
                }
            })
            .map_err(|_| "WINDOW_ACTION_FAILED".into()),
        TitlebarDoubleClickAction::Minimize => {
            window.minimize().map_err(|_| "WINDOW_ACTION_FAILED".into())
        }
        TitlebarDoubleClickAction::None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titlebar_action_preserves_the_macos_user_choice() {
        assert_eq!(
            titlebar_double_click_action("Minimize\n"),
            TitlebarDoubleClickAction::Minimize
        );
        assert_eq!(
            titlebar_double_click_action("None"),
            TitlebarDoubleClickAction::None
        );
        assert_eq!(
            titlebar_double_click_action("Maximize"),
            TitlebarDoubleClickAction::Maximize
        );
        assert_eq!(
            titlebar_double_click_action("unexpected"),
            TitlebarDoubleClickAction::Maximize
        );
    }
}
