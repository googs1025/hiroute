#![cfg(unix)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const REQUIRED_INSTALLATIONS: [&str; 5] = [
    "HIROUTE_WORKER_CODEX_BINARY",
    "HIROUTE_WORKER_CODEX_ACP_ADAPTER",
    "HIROUTE_WORKER_CLAUDE_BINARY",
    "HIROUTE_WORKER_CLAUDE_ACP_ADAPTER",
    "HIROUTE_WORKER_NODE",
];

pub fn run_real_worker_scenario(mode: &str, expected_scenario: &str) {
    let workspace = workspace_root();
    let candidate = required("HIROUTE_PRODUCT_CANDIDATE_SHA");
    assert_eq!(candidate.len(), 40, "candidate must be a full Git SHA");
    assert_eq!(
        git_head(&workspace),
        candidate,
        "candidate checkout mismatch"
    );
    for variable in REQUIRED_INSTALLATIONS {
        let path = PathBuf::from(required(variable));
        assert!(
            path.is_absolute() && path.is_file(),
            "{variable} must name an installed file"
        );
    }

    build_product_binaries(&workspace);
    for harness in ["codex", "claude"] {
        let output = Command::new(python())
            .arg(workspace.join("crates/daemon/tests/support/delegation_product.py"))
            .arg(&workspace)
            .arg(&candidate)
            .env(mode, "1")
            .env("HIROUTE_PRODUCT_WORKER_HARNESS", harness)
            .current_dir(&workspace)
            .output()
            .expect("run the real Worker product scenario");
        assert_scenario(output, expected_scenario, harness, &candidate);
    }
}

fn build_product_binaries(workspace: &Path) {
    let status = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "build",
            "--locked",
            "-p",
            "hiroute-daemon",
            "--bin",
            "hirouted",
            "-p",
            "hiroute-cli",
            "--bin",
            "hiroute",
        ])
        .current_dir(workspace)
        .status()
        .expect("build production Worker binaries");
    assert!(status.success(), "production Worker binary build failed");
}

fn assert_scenario(output: Output, scenario: &str, harness: &str, candidate: &str) {
    if !output.status.success() {
        panic!(
            "real {harness} Worker scenario failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let report = String::from_utf8(output.stdout).expect("scenario output is UTF-8");
    let report = report
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value.get("scenario").and_then(Value::as_str) == Some(scenario))
        .unwrap_or_else(|| panic!("missing {scenario} report for {harness}: {report}"));
    assert_eq!(report["state"], "green", "scenario report: {report}");
    assert_eq!(report["candidate"], candidate, "scenario report: {report}");
    assert_eq!(
        report["worker_harness"], harness,
        "scenario report: {report}"
    );
    println!("{report}");
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical workspace root")
}

fn git_head(workspace: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace)
        .output()
        .expect("read exact candidate SHA");
    assert!(output.status.success(), "git rev-parse failed");
    String::from_utf8(output.stdout)
        .expect("Git SHA is UTF-8")
        .trim()
        .to_owned()
}

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is required for this opt-in product test"))
}

fn python() -> String {
    env::var("PYTHON").unwrap_or_else(|_| "python3".into())
}
