use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::timeout;

use crate::contract::{
    AttemptScript, ClientProtocol, ContractError, Profile, ResolvedProfile, Scenario, ScenarioStep,
    Source,
};
use crate::fixtures::{
    CLAUDE_DOWNSTREAM_KEY, CLAUDE_MODEL_ALIAS, CLAUDE_NATIVE_KEY, CLAUDE_NATIVE_MODEL,
    CODEX_DOWNSTREAM_KEY, CODEX_MODEL_ALIAS, CODEX_NATIVE_KEY, CODEX_NATIVE_MODEL,
};
use crate::http::{HttpError, post_json};
use crate::mock::{NativeLedgerEntry, NativeMock};
use crate::process::{ManagedChild, ProcessError};

#[derive(Clone, Copy, Debug)]
pub struct RunOptions {
    pub timeout: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Passed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RunReport {
    pub schema_version: u32,
    pub suite: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_case: Option<String>,
    pub profile: String,
    pub status: RunStatus,
    pub steps: Vec<StepReport>,
    pub summary: RunSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RunSummary {
    pub passed: usize,
    pub failed: usize,
    pub native_cpa_processes: usize,
    pub native_mock_sources: usize,
    pub gateway_processes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StepReport {
    pub id: String,
    pub protocol: ClientProtocol,
    pub status: RunStatus,
    pub evidence: StepEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StepEvidence {
    pub http_status: u16,
    pub response_source_header: String,
    pub downstream_sse_terminal: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub attempts: Vec<AttemptEvidence>,
    pub receipts: Vec<ReceiptEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AttemptEvidence {
    pub ordinal: usize,
    pub native_source: Source,
    pub native_path: String,
    pub native_protocol: ClientProtocol,
    pub response_head_status: u16,
    pub protocol_conversion: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReceiptEvidence {
    pub ordinal: usize,
    pub receipt_source: String,
    pub receipt_class: String,
    pub receipt_affinity: String,
    pub receipt_disposition: String,
    pub receipt_http_status: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error(transparent)]
    Contract(#[from] ContractError),
    #[error("cannot create E2E runtime: {0}")]
    Runtime(std::io::Error),
    #[error("cannot encode E2E runtime: {0}")]
    Json(serde_json::Error),
    #[error("E2E child process failed: {0}")]
    Process(String),
    #[error("E2E HTTP exchange failed: {0}")]
    Http(String),
    #[error("E2E native mock failed to start: {0}")]
    Mock(std::io::Error),
    #[error("step {step} failed: {detail}")]
    Assertion { step: String, detail: &'static str },
    #[error("receipt did not appear before the E2E deadline")]
    ReceiptTimeout,
    #[error("native mock evidence did not appear before the E2E deadline")]
    LedgerTimeout,
}

impl RunnerError {
    fn code(&self) -> &'static str {
        match self {
            Self::Contract(_) => "contract_invalid",
            Self::Runtime(_) | Self::Json(_) => "runtime_setup_failed",
            Self::Process(_) => "process_failed",
            Self::Http(_) => "http_exchange_failed",
            Self::Mock(_) => "native_mock_failed",
            Self::Assertion { .. } => "assertion_failed",
            Self::ReceiptTimeout => "receipt_timeout",
            Self::LedgerTimeout => "native_ledger_timeout",
        }
    }
}

impl From<ProcessError> for RunnerError {
    fn from(error: ProcessError) -> Self {
        Self::Process(error.to_string())
    }
}

impl From<HttpError> for RunnerError {
    fn from(error: HttpError) -> Self {
        Self::Http(error.to_string())
    }
}

#[derive(Debug)]
pub struct RunFailure {
    pub report: RunReport,
    source: RunnerError,
}

impl std::fmt::Display for RunFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for RunFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub async fn run_suite(
    scenario: &Scenario,
    profile: &Profile,
    options: RunOptions,
) -> Result<RunReport, RunFailure> {
    let result = run_suite_inner(scenario, profile, options).await;
    match result {
        Ok(steps) => Ok(report(scenario, profile, RunStatus::Passed, steps, None)),
        Err(source) => Err(RunFailure {
            report: report(
                scenario,
                profile,
                RunStatus::Failed,
                Vec::new(),
                Some(source.code().into()),
            ),
            source,
        }),
    }
}

pub fn write_report(path: &Path, report: &RunReport) -> Result<(), RunnerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(RunnerError::Runtime)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(RunnerError::Runtime)?;
    serde_json::to_writer_pretty(&mut file, report).map_err(RunnerError::Json)?;
    file.write_all(b"\n").map_err(RunnerError::Runtime)
}

async fn run_suite_inner(
    scenario: &Scenario,
    profile: &Profile,
    options: RunOptions,
) -> Result<Vec<StepReport>, RunnerError> {
    scenario.validate_with(profile)?;
    let resolved = profile.resolve()?;
    let temp = tempfile::Builder::new()
        .prefix("hiroute-e2e-")
        .tempdir()
        .map_err(RunnerError::Runtime)?;
    private_dir(temp.path())?;
    let attempt_sequence = Arc::new(AtomicU64::new(1));
    let claude_mock = NativeMock::start(Source::ClaudeCompatible, Arc::clone(&attempt_sequence))
        .await
        .map_err(RunnerError::Mock)?;
    let codex_mock = NativeMock::start(Source::CodexChatgpt, attempt_sequence)
        .await
        .map_err(RunnerError::Mock)?;
    let mut children = Vec::new();
    let active_result = run_active(
        scenario,
        &resolved,
        &temp,
        &claude_mock,
        &codex_mock,
        &mut children,
        options,
    )
    .await;

    let mut cleanup_result = Ok(());
    while let Some(child) = children.pop() {
        if let Err(error) = child.shutdown(Duration::from_secs(3)).await
            && cleanup_result.is_ok()
        {
            cleanup_result = Err(RunnerError::from(error));
        }
    }
    claude_mock.shutdown(Duration::from_secs(3)).await;
    codex_mock.shutdown(Duration::from_secs(3)).await;
    match active_result {
        Err(error) => Err(error),
        Ok(steps) => cleanup_result.map(|()| steps),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_active(
    scenario: &Scenario,
    resolved: &ResolvedProfile,
    temp: &TempDir,
    claude_mock: &NativeMock,
    codex_mock: &NativeMock,
    children: &mut Vec<ManagedChild>,
    options: RunOptions,
) -> Result<Vec<StepReport>, RunnerError> {
    debug_assert_eq!(claude_mock.source(), Source::ClaudeCompatible);
    debug_assert_eq!(codex_mock.source(), Source::CodexChatgpt);
    let claude_cpa_addr = reserve_addr()?;
    let codex_cpa_addr = reserve_addr()?;
    let sut_addr = reserve_addr()?;
    let root = temp.path();
    let claude_root = root.join("cpa-claude");
    let codex_root = root.join("cpa-codex");
    let log_dir = root.join("logs");
    let claude_auth = claude_root.join("auth");
    let codex_auth = codex_root.join("auth");
    for path in [
        &claude_root,
        &codex_root,
        &log_dir,
        &claude_auth,
        &codex_auth,
    ] {
        private_dir(path)?;
    }
    let claude_config = claude_root.join("config.yaml");
    let codex_config = codex_root.join("config.yaml");
    private_write(
        &claude_config,
        render_cpa_config(
            Source::ClaudeCompatible,
            claude_cpa_addr.port(),
            &claude_auth,
            claude_mock.addr(),
        )
        .as_bytes(),
    )?;
    private_write(
        &codex_config,
        render_cpa_config(
            Source::CodexChatgpt,
            codex_cpa_addr.port(),
            &codex_auth,
            codex_mock.addr(),
        )
        .as_bytes(),
    )?;
    let runtime_path = root.join("runtime.private.json");
    let client_api_key = random_capability_token()?;
    let runtime = runtime_manifest(RuntimeManifestInput {
        root,
        claude_config: &claude_config,
        codex_config: &codex_config,
        claude_auth: &claude_auth,
        codex_auth: &codex_auth,
        claude_addr: claude_cpa_addr,
        codex_addr: codex_cpa_addr,
        client_api_key: &client_api_key,
    });
    private_write(
        &runtime_path,
        &serde_json::to_vec_pretty(&runtime).map_err(RunnerError::Json)?,
    )?;
    let receipt_path = root.join("receipts.private.jsonl");

    children.push(ManagedChild::spawn(
        "cpa-claude",
        &resolved.cpa_binary,
        &["--config".into(), path_text(&claude_config)?],
        &log_dir,
    )?);
    children
        .last_mut()
        .expect("child was just pushed")
        .wait_ready(claude_cpa_addr, options.timeout)
        .await?;
    children.push(ManagedChild::spawn(
        "cpa-codex",
        &resolved.cpa_binary,
        &["--config".into(), path_text(&codex_config)?],
        &log_dir,
    )?);
    children
        .last_mut()
        .expect("child was just pushed")
        .wait_ready(codex_cpa_addr, options.timeout)
        .await?;
    children.push(ManagedChild::spawn(
        "hiroute-poc",
        &resolved.sut_binary,
        &[
            "serve".into(),
            "--runtime".into(),
            path_text(&runtime_path)?,
            "--listen".into(),
            sut_addr.to_string(),
            "--receipts".into(),
            path_text(&receipt_path)?,
        ],
        &log_dir,
    )?);
    children
        .last_mut()
        .expect("child was just pushed")
        .wait_ready(sut_addr, options.timeout)
        .await?;

    let mut reports = Vec::with_capacity(scenario.steps.len());
    for step in &scenario.steps {
        reports.push(
            execute_step(
                step,
                sut_addr,
                &receipt_path,
                claude_mock,
                codex_mock,
                &client_api_key,
                options.timeout,
            )
            .await?,
        );
    }
    Ok(reports)
}

async fn execute_step(
    step: &ScenarioStep,
    sut_addr: SocketAddr,
    receipt_path: &Path,
    claude_mock: &NativeMock,
    codex_mock: &NativeMock,
    client_api_key: &str,
    deadline: Duration,
) -> Result<StepReport, RunnerError> {
    let before_claude = claude_mock.snapshot().await.len();
    let before_codex = codex_mock.snapshot().await.len();
    let receipts_before = read_receipts(receipt_path)?.len();
    arm_response_heads(step, claude_mock, codex_mock).await;
    let body = serde_json::to_vec(&step.request).map_err(RunnerError::Json)?;
    let authorization = format!("Bearer {client_api_key}");
    let mut headers = protocol_headers(step.protocol, &authorization)
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<Vec<_>>();
    headers.extend(
        step.request_headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    let header_refs = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let response = post_json(
        sut_addr,
        step.protocol.gateway_path(),
        &header_refs,
        &body,
        deadline,
    )
    .await?;
    assert_step(
        step,
        response.status == step.expected_http_status,
        "gateway returned an unexpected status",
    )?;
    if response.status != 200 {
        return verify_rejected_step(
            step,
            response,
            receipt_path,
            claude_mock,
            codex_mock,
            before_claude,
            before_codex,
            receipts_before,
        )
        .await;
    }
    let source_header = response.header("x-hiroute-source").unwrap_or_default();
    assert_step(
        step,
        source_header == step.accepted_source().as_str(),
        "gateway source header did not match the expected source",
    )?;
    assert_step(
        step,
        response
            .header("content-type")
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream")),
        "gateway response was not an SSE stream",
    )?;
    let response_body = String::from_utf8_lossy(&response.body);
    assert_step(
        step,
        response_body.contains(step.protocol.terminal_event()),
        "downstream SSE terminal event was missing",
    )?;

    let (claude_after, codex_after) = wait_for_ledger(
        claude_mock,
        codex_mock,
        before_claude + before_codex + step.attempts.len(),
        deadline,
    )
    .await?;
    assert_step(
        step,
        claude_after.len() + codex_after.len()
            == before_claude + before_codex + step.attempts.len(),
        "downstream request produced an unexpected native attempt count",
    )?;
    let mut native_entries: Vec<_> = claude_after[before_claude..]
        .iter()
        .chain(&codex_after[before_codex..])
        .cloned()
        .collect();
    native_entries.sort_by_key(|entry| entry.sequence);
    for (attempt, entry) in step.attempts.iter().zip(&native_entries) {
        verify_native_entry(step, attempt, entry)?;
    }
    assert_step(
        step,
        claude_mock.pending_response_heads().await == 0
            && codex_mock.pending_response_heads().await == 0,
        "native response-head script was not fully consumed",
    )?;

    let receipts = wait_for_receipts(
        receipt_path,
        receipts_before + step.attempts.len(),
        deadline,
    )
    .await?;
    assert_step(
        step,
        receipts.len() == receipts_before + step.attempts.len(),
        "downstream request produced an unexpected receipt count",
    )?;
    let step_receipts = &receipts[receipts_before..];
    for (attempt, receipt) in step.attempts.iter().zip(step_receipts) {
        verify_receipt(step, attempt, receipt)?;
    }
    let attempt_evidence = native_entries
        .iter()
        .enumerate()
        .map(|(index, entry)| AttemptEvidence {
            ordinal: index + 1,
            native_source: entry.source,
            native_path: entry.path.clone(),
            native_protocol: entry.protocol,
            response_head_status: entry.response_head_status,
            protocol_conversion: step.protocol != entry.protocol,
        })
        .collect();
    let receipt_evidence = step_receipts
        .iter()
        .enumerate()
        .map(|(index, receipt)| ReceiptEvidence {
            ordinal: index + 1,
            receipt_source: receipt.source_id.clone(),
            receipt_class: receipt.complexity_class.clone(),
            receipt_affinity: receipt.affinity.clone(),
            receipt_disposition: receipt.disposition.clone(),
            receipt_http_status: receipt
                .http_status
                .expect("verified receipts always carry an HTTP status"),
        })
        .collect();
    Ok(StepReport {
        id: step.id.clone(),
        protocol: step.protocol,
        status: RunStatus::Passed,
        evidence: StepEvidence {
            http_status: response.status,
            response_source_header: source_header.to_owned(),
            downstream_sse_terminal: step.protocol.terminal_event().into(),
            error_code: None,
            attempts: attempt_evidence,
            receipts: receipt_evidence,
        },
    })
}

#[allow(clippy::too_many_arguments)]
async fn verify_rejected_step(
    step: &ScenarioStep,
    response: crate::http::HttpResponse,
    receipt_path: &Path,
    claude_mock: &NativeMock,
    codex_mock: &NativeMock,
    before_claude: usize,
    before_codex: usize,
    receipts_before: usize,
) -> Result<StepReport, RunnerError> {
    let document: Value = serde_json::from_slice(&response.body).map_err(RunnerError::Json)?;
    let error_code = document.get("code").and_then(Value::as_str);
    assert_step(
        step,
        error_code == step.expected_error_code.as_deref(),
        "gateway error code did not match",
    )?;
    tokio::task::yield_now().await;
    assert_step(
        step,
        claude_mock.snapshot().await.len() == before_claude
            && codex_mock.snapshot().await.len() == before_codex,
        "rejected request reached a native upstream",
    )?;
    assert_step(
        step,
        read_receipts(receipt_path)?.len() == receipts_before,
        "rejected request produced a routing receipt",
    )?;
    Ok(StepReport {
        id: step.id.clone(),
        protocol: step.protocol,
        status: RunStatus::Passed,
        evidence: StepEvidence {
            http_status: response.status,
            response_source_header: String::new(),
            downstream_sse_terminal: String::new(),
            error_code: error_code.map(str::to_owned),
            attempts: Vec::new(),
            receipts: Vec::new(),
        },
    })
}

async fn arm_response_heads(
    step: &ScenarioStep,
    claude_mock: &NativeMock,
    codex_mock: &NativeMock,
) {
    let claude: Vec<_> = step
        .attempts
        .iter()
        .filter(|attempt| attempt.source == Source::ClaudeCompatible)
        .map(|attempt| attempt.response_head_status)
        .collect();
    let codex: Vec<_> = step
        .attempts
        .iter()
        .filter(|attempt| attempt.source == Source::CodexChatgpt)
        .map(|attempt| attempt.response_head_status)
        .collect();
    claude_mock.arm_response_heads(&claude).await;
    codex_mock.arm_response_heads(&codex).await;
}

fn verify_native_entry(
    step: &ScenarioStep,
    attempt: &AttemptScript,
    entry: &NativeLedgerEntry,
) -> Result<(), RunnerError> {
    assert_step(
        step,
        entry.source == attempt.source,
        "native ledger source did not match",
    )?;
    assert_step(
        step,
        entry.path == attempt.source.native_path(),
        "CPA did not use the source-native endpoint",
    )?;
    let expected_model = match attempt.source {
        Source::ClaudeCompatible => CLAUDE_NATIVE_MODEL,
        Source::CodexChatgpt => CODEX_NATIVE_MODEL,
    };
    assert_step(
        step,
        entry.model.as_deref() == Some(expected_model),
        "CPA did not map the gateway model alias to the native model",
    )?;
    assert_step(
        step,
        entry.response_head_status == attempt.response_head_status.as_u16(),
        "native mock response head did not match the declarative script",
    )
}

fn verify_receipt(
    step: &ScenarioStep,
    attempt: &AttemptScript,
    receipt: &Receipt,
) -> Result<(), RunnerError> {
    let expected_disposition = if attempt.response_head_status.is_accept() {
        "accept"
    } else {
        "continue"
    };
    assert_step(
        step,
        receipt.source_id == attempt.source.as_str(),
        "receipt source did not match",
    )?;
    assert_step(
        step,
        receipt.complexity_class == step.accepted_class().as_str(),
        "receipt complexity class did not match",
    )?;
    assert_step(
        step,
        receipt.affinity == step.accepted_affinity().as_str(),
        "receipt affinity did not match",
    )?;
    assert_step(
        step,
        receipt.disposition == expected_disposition
            && receipt.http_status == Some(attempt.response_head_status.as_u16()),
        "receipt disposition/status did not match the response-head script",
    )?;
    let expected_protocol = match step.protocol {
        ClientProtocol::Responses => "open_ai_responses",
        ClientProtocol::Messages => "anthropic_messages",
    };
    assert_step(
        step,
        receipt.client_protocol == expected_protocol,
        "receipt client protocol did not match",
    )
}

#[derive(Clone, Debug, Deserialize)]
struct Receipt {
    client_protocol: String,
    source_id: String,
    complexity_class: String,
    affinity: String,
    disposition: String,
    http_status: Option<u16>,
}

async fn wait_for_receipts(
    path: &Path,
    count: usize,
    deadline: Duration,
) -> Result<Vec<Receipt>, RunnerError> {
    timeout(deadline, async {
        loop {
            let receipts = read_receipts(path)?;
            if receipts.len() >= count {
                return Ok(receipts);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| RunnerError::ReceiptTimeout)?
}

async fn wait_for_ledger(
    claude: &NativeMock,
    codex: &NativeMock,
    total: usize,
    deadline: Duration,
) -> Result<(Vec<NativeLedgerEntry>, Vec<NativeLedgerEntry>), RunnerError> {
    timeout(deadline, async {
        loop {
            let claude_entries = claude.snapshot().await;
            let codex_entries = codex.snapshot().await;
            if claude_entries.len() + codex_entries.len() >= total {
                return (claude_entries, codex_entries);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| RunnerError::LedgerTimeout)
}

fn read_receipts(path: &Path) -> Result<Vec<Receipt>, RunnerError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(RunnerError::Runtime(error)),
    };
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(RunnerError::Json))
        .collect()
}

fn protocol_headers(protocol: ClientProtocol, authorization: &str) -> Vec<(&'static str, &str)> {
    match protocol {
        ClientProtocol::Responses => vec![("Authorization", authorization)],
        ClientProtocol::Messages => vec![
            ("Authorization", authorization),
            ("Anthropic-Version", "2023-06-01"),
        ],
    }
}

struct RuntimeManifestInput<'a> {
    root: &'a Path,
    claude_config: &'a Path,
    codex_config: &'a Path,
    claude_auth: &'a Path,
    codex_auth: &'a Path,
    claude_addr: SocketAddr,
    codex_addr: SocketAddr,
    client_api_key: &'a str,
}

fn runtime_manifest(input: RuntimeManifestInput<'_>) -> Value {
    let RuntimeManifestInput {
        root,
        claude_config,
        codex_config,
        claude_auth,
        codex_auth,
        claude_addr,
        codex_addr,
        client_api_key,
    } = input;
    serde_json::json!({
        "schema_version": 1,
        "root": root,
        "client_api_key": client_api_key,
        "claude": {
            "source_id": "claude-compatible",
            "listen_host": "127.0.0.1",
            "port": claude_addr.port(),
            "base_url": format!("http://{claude_addr}"),
            "api_key": CLAUDE_DOWNSTREAM_KEY,
            "model": CLAUDE_MODEL_ALIAS,
            "config_path": claude_config,
            "auth_dir": claude_auth
        },
        "codex": {
            "source_id": "codex-chatgpt",
            "listen_host": "127.0.0.1",
            "port": codex_addr.port(),
            "base_url": format!("http://{codex_addr}"),
            "api_key": CODEX_DOWNSTREAM_KEY,
            "model": CODEX_MODEL_ALIAS,
            "config_path": codex_config,
            "auth_dir": codex_auth
        }
    })
}

fn render_cpa_config(
    source: Source,
    port: u16,
    auth_dir: &Path,
    native_addr: SocketAddr,
) -> String {
    let (downstream_key, native_key, section, alias, model) = match source {
        Source::ClaudeCompatible => (
            CLAUDE_DOWNSTREAM_KEY,
            CLAUDE_NATIVE_KEY,
            "claude-api-key",
            CLAUDE_MODEL_ALIAS,
            CLAUDE_NATIVE_MODEL,
        ),
        Source::CodexChatgpt => (
            CODEX_DOWNSTREAM_KEY,
            CODEX_NATIVE_KEY,
            "codex-api-key",
            CODEX_MODEL_ALIAS,
            CODEX_NATIVE_MODEL,
        ),
    };
    format!(
        r#"host: "127.0.0.1"
port: {port}
tls:
  enable: false
  cert: ""
  key: ""
remote-management:
  allow-remote: false
  secret-key: ""
  disable-control-panel: true
  disable-auto-update-panel: true
auth-dir: {auth_dir}
api-keys:
  - {downstream_key}
debug: false
logging-to-file: false
usage-statistics-enabled: false
request-log: false
commercial-mode: true
disable-cooling: true
disable-claude-cloak-mode: true
request-retry: 0
max-retry-credentials: 1
max-retry-interval: 0
streaming:
  keepalive-seconds: 0
  bootstrap-retries: 0
{section}:
  - api-key: {native_key}
    base-url: {base_url}
    request-retry: 0
    disable-cooling: true
    models:
      - name: {model}
        alias: {alias}
        force-mapping: true
"#,
        auth_dir = yaml_quote(&auth_dir.to_string_lossy()),
        downstream_key = yaml_quote(downstream_key),
        native_key = yaml_quote(native_key),
        base_url = yaml_quote(&format!("http://{native_addr}")),
        model = yaml_quote(model),
        alias = yaml_quote(alias),
    )
}

fn yaml_quote(value: &str) -> String {
    serde_json::to_string(value).expect("a string always serializes")
}

fn reserve_addr() -> Result<SocketAddr, RunnerError> {
    let listener = StdTcpListener::bind("127.0.0.1:0").map_err(RunnerError::Runtime)?;
    listener.local_addr().map_err(RunnerError::Runtime)
}

fn random_capability_token() -> Result<String, RunnerError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| RunnerError::Runtime(std::io::Error::other(error.to_string())))?;
    let mut token = String::with_capacity("hiroute-e2e-".len() + bytes.len() * 2);
    token.push_str("hiroute-e2e-");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(token)
}

fn private_dir(path: &Path) -> Result<(), RunnerError> {
    fs::create_dir_all(path).map_err(RunnerError::Runtime)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(RunnerError::Runtime)?;
    }
    Ok(())
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<(), RunnerError> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(RunnerError::Runtime)?;
    file.write_all(bytes).map_err(RunnerError::Runtime)
}

fn path_text(path: &Path) -> Result<String, RunnerError> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| RunnerError::Runtime(std::io::Error::other("non-UTF-8 E2E path")))
}

fn assert_step(
    step: &ScenarioStep,
    condition: bool,
    detail: &'static str,
) -> Result<(), RunnerError> {
    condition
        .then_some(())
        .ok_or_else(|| assertion(step, detail))
}

fn assertion(step: &ScenarioStep, detail: &'static str) -> RunnerError {
    RunnerError::Assertion {
        step: step.id.clone(),
        detail,
    }
}

fn report(
    scenario: &Scenario,
    profile: &Profile,
    status: RunStatus,
    steps: Vec<StepReport>,
    failure_code: Option<String>,
) -> RunReport {
    let passed = steps
        .iter()
        .filter(|step| step.status == RunStatus::Passed)
        .count();
    let failed = usize::from(status == RunStatus::Failed);
    RunReport {
        schema_version: 1,
        suite: scenario.name.clone(),
        selected_case: scenario.selected_case().map(str::to_owned),
        profile: profile.name.clone(),
        status,
        steps,
        summary: RunSummary {
            passed,
            failed,
            native_cpa_processes: 2,
            native_mock_sources: 2,
            gateway_processes: 1,
        },
        failure_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configs_pin_each_cpa_to_one_native_provider() {
        let claude = render_cpa_config(
            Source::ClaudeCompatible,
            1234,
            Path::new("/tmp/auth"),
            "127.0.0.1:4321".parse().unwrap(),
        );
        assert!(claude.contains("claude-api-key:"));
        assert!(!claude.contains("codex-api-key:"));
        assert!(claude.contains("request-retry: 0"));
        let codex = render_cpa_config(
            Source::CodexChatgpt,
            1235,
            Path::new("/tmp/auth"),
            "127.0.0.1:4322".parse().unwrap(),
        );
        assert!(codex.contains("codex-api-key:"));
        assert!(!codex.contains("claude-api-key:"));
    }

    #[test]
    fn result_report_contains_no_runtime_paths_or_commands() {
        let scenario = Scenario {
            schema_version: 2,
            name: "redacted".into(),
            case_shards: vec![],
            steps: Vec::new(),
            selected_case: None,
        };
        let profile = Profile {
            schema_version: 1,
            name: "local-process".into(),
            cpa_binary: "ignored".into(),
            sut_binary: "ignored".into(),
        };
        let report = report(
            &scenario,
            &profile,
            RunStatus::Failed,
            Vec::new(),
            Some("setup_failed".into()),
        );
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("/tmp"));
        assert!(!encoded.contains("binary"));
        assert!(!encoded.contains("command"));
    }
}
