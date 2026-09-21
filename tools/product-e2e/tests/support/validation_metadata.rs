use std::path::Path;
use std::process::Command;

use serde_json::Value;

pub fn current() -> Value {
    let bytes = if std::env::var_os("HIROUTE_VALIDATION_EXECUTION").is_some() {
        let path = std::env::var_os("HIROUTE_VALIDATION_METADATA_JSON")
            .expect("validation execution requires prepared Cargo metadata");
        std::fs::read(path).expect("read prepared Cargo metadata")
    } else {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let output = Command::new(env!("CARGO"))
            .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
            .current_dir(workspace)
            .output()
            .expect("run Cargo metadata");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    serde_json::from_slice(&bytes).expect("parse Cargo metadata")
}
