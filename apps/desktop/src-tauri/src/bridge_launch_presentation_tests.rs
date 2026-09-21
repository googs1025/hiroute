use super::LaunchPresentation;

fn presentation(background: bool, duplicate: bool, fallback_at_ready: bool) -> LaunchPresentation {
    LaunchPresentation {
        background,
        duplicate,
        fallback_at_ready,
    }
}

#[test]
fn only_a_foreground_first_host_may_present_the_window() {
    assert!(!presentation(false, false, true).headless());
    assert!(presentation(true, false, true).headless(), "background");
    assert!(
        presentation(false, true, false).headless(),
        "duplicate host"
    );
}

#[test]
fn the_launch_window_starts_hidden_in_the_bundle_configuration() {
    let config = include_str!("../tauri.conf.json");
    assert!(
        config.contains("\"visible\": false"),
        "the main window must be created hidden so the native launch reason decides first"
    );
}
