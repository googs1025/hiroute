use super::{Result, digest, process::Process, require, run::Context};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::process::ExitStatusExt,
    process::Command,
    time::{Duration, Instant},
};

pub(super) fn execute(ctx: &mut Context<'_>) -> Result<()> {
    ctx.enter("gateway_build");
    let tool = ctx.build("hiroute-e2e", "hiroute-e2e")?;
    ctx.record(
        "gateway_build",
        Some(0),
        None,
        &serde_json::to_value(&tool)?,
    )?;
    ctx.enter("gateway_run");
    ctx.verify_artifact(&tool)?;
    let report_path = ctx.private.join("gateway-current.json");
    let mut command = Command::new(&tool.path);
    // This is a trusted build tool, not the SUT. Its existing runtime isolates product HOME/FDs.
    command
        .current_dir(&ctx.source.root)
        .args(["run-current", "--run-id", ctx.run_id, "--result"])
        .arg(&report_path)
        .env("HIROUTE_E2E_KEEP_RUNTIME", "1");
    // An unsuccessful nested runner may not return its build/execution split.
    // Keep that elapsed time unclassified instead of reporting it as product execution.
    ctx.report.timing_complete = false;
    let mut child = Process::spawn(
        &mut command,
        &ctx.private.join("gateway-tool"),
        ctx.cancel.clone(),
    )?;
    let exit = child.wait(Instant::now() + Duration::from_secs(3660), 64 * 1024 * 1024)?;
    if !exit.success() {
        let (stdout, _) = child.output(64 * 1024 * 1024)?;
        let failure: Value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
        ctx.record(
            "gateway_run",
            exit.code(),
            exit.signal(),
            &json!({"child_exit_observed":true}),
        )?;
        let code = if failure["schema"] == "hiroute.e2e.current-failure/v1"
            && failure["request_run_id"] == ctx.run_id
        {
            match failure["code"].as_str() {
                Some("gateway_observation_incomplete") => "gateway_observation_incomplete",
                Some("gateway_readiness_failed") => "gateway_readiness_failed",
                Some("gateway_identity_failed") => "gateway_identity_failed",
                Some("gateway_contract_failed") => "gateway_contract_failed",
                _ => "gateway_process_failed",
            }
        } else {
            "gateway_runner_failed"
        };
        return Err(super::SmokeError(code));
    }
    ctx.verify_artifact(&tool)?;
    let metadata = fs::symlink_metadata(&report_path)?;
    require(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() <= 16 * 1024 * 1024,
        "gateway_report_invalid",
    )?;
    let bytes = fs::read(&report_path)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    require(
        value["schema"] == "hiroute.e2e.current-run/v2"
            && value["request_run_id"] == ctx.run_id
            && value["tool_sha256"] == tool.sha256,
        "gateway_run_identity_mismatch",
    )?;
    ctx.report.build_ms += value["build_ms"]
        .as_u64()
        .ok_or(super::SmokeError("gateway_timing_missing"))? as u128;
    ctx.report.timing_complete = true;
    let report = &value["report"];
    require(
        report["schema_version"] == "hiroute.e2e.production-result/v2"
            && report["scenario_state"] == "green"
            && report["process_exit"]["test_process_code"] == 0
            && report["process_exit"]["sut_reaped"] == true
            && report["launcher"]["sut_source_revision"] == ctx.source.revision
            && report["launcher"]["build_attestation"]["build_input_digest"] == ctx.source.inputs,
        "gateway_evidence_mismatch",
    )?;
    // The live trusted child already ran the complete common Oracle. Bind its immutable result,
    // not a fixture-supplied summary, to this caller's run and actual exit.
    ctx.record(
        "gateway_run",
        exit.code(),
        exit.signal(),
        &json!({"run_id":ctx.run_id,"private_report_sha256":digest(&bytes)}),
    )?;
    ctx.enter("gateway_evidence");
    require(
        report["checks"]
            .as_array()
            .is_some_and(|c| c.len() == 10 && c.iter().all(|c| c["status"] == "passed"))
            && report["privacy"]["sensitive_occurrences"] == 0,
        "gateway_checks_incomplete",
    )?;
    ctx.record("gateway_evidence",None,None,&json!({"evidence_digest":report["evidence_digest"],
        "result_payload_digest":report["result_payload_digest"],"source_revision":ctx.source.revision,
        "gateway_executable_sha256":report["launcher"]["executable_sha256"]}))
}
