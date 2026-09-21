#![cfg(unix)]

use std::process::Command;

#[test]
fn explicit_invalid_or_missing_service_has_installation_neutral_recovery() {
    let root = tempfile::tempdir().unwrap();
    for value in ["", "relative", root.path().to_str().unwrap()] {
        let output = Command::new(env!("CARGO_BIN_EXE_hiroute"))
            .env("HIROUTE_RUNTIME_DIR", value)
            .env("XDG_RUNTIME_DIR", root.path())
            .args(["worker", "list", "--output", "json"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["error"]["code"], "DAEMON_UNAVAILABLE");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("HiRoute 服务不可用"));
        assert!(stderr.contains("hiroute service status"));
        assert!(stderr.contains("hiroute service start"));
        assert!(stderr.contains("Desktop"));
        assert!(stderr.contains("隔离实例"));
        assert!(stderr.contains("不会自动启动或重放请求"));
        assert!(!stderr.contains("请启动或恢复 HiRoute Desktop 后重试"));
        assert!(!root.path().join("hiroute/control.sock").exists());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn default_macos_cli_reaches_same_user_desktop_directory() {
    use hiroute_application::ApplicationService;
    use hiroute_daemon::control::{ProductionControlRuntime, start_control};
    use hiroute_integrations::TrustedReleaseCatalog;
    use std::time::Duration;

    let temporary_home = tempfile::tempdir_in("/tmp").unwrap();
    let home = temporary_home.path().canonicalize().unwrap();
    let desktop = home.join("Library/Application Support/ai.hiroute.desktop");
    let manifest = include_bytes!("../../../assets/release-facts/current/bundle/manifest.json");
    let catalog = TrustedReleaseCatalog::load_bundled_release_facts(
        manifest,
        manifest,
        include_bytes!("../../../assets/release-facts/current/bundle/connector-registry.json"),
        include_bytes!("../../../assets/release-facts/current/bundle/model-data.json"),
    )
    .unwrap();
    let runtime =
        ProductionControlRuntime::open_with_release_catalog(desktop.join("storage"), catalog)
            .unwrap();
    let mut listener = start_control(
        ApplicationService::new(runtime.application_ports()),
        desktop.join("run"),
    )
    .unwrap();
    let invoke = |explicit: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hiroute"));
        command
            .env("HOME", &home)
            .env_remove("HIROUTE_RUNTIME_DIR")
            .env_remove("XDG_RUNTIME_DIR")
            .args(["worker", "list", "--output", "json"]);
        if let Some(root) = explicit {
            command.env("HIROUTE_RUNTIME_DIR", root);
        }
        command.output().unwrap()
    };
    let result = invoke(None);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert!(!invoke(Some("relative")).status.success());
    assert!(
        !invoke(Some(home.join("isolated").to_str().unwrap()))
            .status
            .success()
    );
    listener.shutdown();
    listener.join(Duration::from_secs(5)).unwrap();
    assert!(!invoke(None).status.success());
}
