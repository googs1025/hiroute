use std::collections::BTreeMap;
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

use crate::http;
use crate::process::ManagedChild;

use super::bindings::RuntimeBindings;
use super::canonical::sha256_hex;
use super::client;
use super::collector;
use super::contract::P0Bundle;
use super::fixture::{self, FixtureInputs};
use super::oracle::{
    EvidenceSet, evaluate, not_implemented_client_output, verify_report_semantics,
};
use super::privacy::{
    PrivateTreeSeal, audit_private_runtime, create_private_dir, private_write,
    read_private_bounded, seal_private_tree, verify_private_dir,
};
use super::provider::{NativeProvider, ProviderSnapshot};
use super::schema::validate_document;
use super::types::*;

const MAX_READY_BYTES: u64 = 128 * 1024;

#[derive(Clone, Debug)]
pub struct P0RunOptions {
    pub reviewed_sut: ReviewedSut,
    pub timeout: Duration,
}

#[derive(Clone, Copy, Debug, Default)]
struct ProviderFacts {
    accepted: u64,
    active: u64,
    parse_failed: u64,
    aborted: u64,
    internal_failures: u64,
}

pub async fn run_oracle(
    bundle: &P0Bundle,
    options: P0RunOptions,
) -> Result<VerifiedP0Run, P0RunError> {
    verify_reviewed_sut(&options.reviewed_sut)?;
    let runtime = tempfile::Builder::new().prefix("hiroute-p0-").tempdir()?;
    create_private_dir(runtime.path())?;
    let client_token = random_secret("client")?;
    let provider_token = random_secret("provider")?;
    let run_nonce = random_secret("run")?;
    let readiness_nonce = random_secret("ready")?;
    let binding_nonce = random_secret("binding")?;
    let bindings = RuntimeBindings::from_nonces(
        binding_nonce,
        run_nonce.clone(),
        bundle
            .artifacts()
            .scenario
            .cases
            .iter()
            .map(|case| case.id.as_str()),
    );
    let sensitive_markers = sensitive_markers(
        bundle,
        runtime.path(),
        &[&client_token, &provider_token, bindings.challenge()],
    );
    let client_authorization = format!("Bearer {client_token}");
    let provider_authorization = format!("Bearer {provider_token}");
    let sequence = Arc::new(AtomicU64::new(1));
    let mut providers = Vec::new();
    let mut endpoints = BTreeMap::new();
    for case in &bundle.artifacts().corpus.cases {
        for script in &case.providers {
            let provider = NativeProvider::start(
                script,
                &provider_authorization,
                Arc::clone(&sequence),
                &bindings,
            )
            .await?;
            endpoints.insert(script.id.clone(), provider.addr());
            providers.push((case.id.clone(), provider));
        }
    }
    let evidence_dir = runtime.path().join("evidence");
    create_private_dir(&evidence_dir)?;
    let readiness_path = runtime.path().join("hirouted-ready.json");
    private_write(&readiness_path, b"")?;
    let fixture_path = runtime.path().join("gateway-fixture.json");
    let fixture = fixture::build(
        bundle,
        FixtureInputs {
            endpoints: &endpoints,
            client_authorization: &client_authorization,
            provider_authorization: &provider_authorization,
            evidence_dir: &evidence_dir,
            run_nonce: &run_nonce,
            readiness_path: &readiness_path,
            readiness_nonce: &readiness_nonce,
        },
    );
    validate_fixture(bundle, &fixture)?;
    private_write(&fixture_path, &serde_json::to_vec(&fixture)?)?;
    let reserved_addr = reserve_loopback_addr()?;
    let args = vec![
        "--role".into(),
        "gateway".into(),
        "--listen".into(),
        reserved_addr.to_string(),
        "--fixture".into(),
        path_text(&fixture_path)?,
    ];
    let log_dir = runtime.path().join("process");
    let prepared = ManagedChild::prepare("hirouted", &log_dir)?;
    let privacy_seal = seal_private_tree(runtime.path())?;
    let launch_environment = vec![(
        hiroute_gateway::server::LAUNCHER_EXECUTABLE_SHA256_ENV.to_owned(),
        options.reviewed_sut.executable_sha256.clone(),
    )];
    let mut sut = prepared.spawn_with_env(
        &options.reviewed_sut.canonical_path,
        &args,
        &launch_environment,
    )?;
    let active = run_active(
        bundle,
        &options,
        runtime.path(),
        &fixture_path,
        &fixture,
        &evidence_dir,
        &readiness_path,
        &readiness_nonce,
        reserved_addr,
        &run_nonce,
        &client_authorization,
        &sensitive_markers,
        &bindings,
        &mut providers,
        &mut sut,
        &privacy_seal,
    )
    .await;
    let cleanup = sut.shutdown(Duration::from_secs(3)).await;
    for (_, provider) in providers {
        provider.shutdown(Duration::from_secs(3)).await;
    }
    match active {
        Err(error) => Err(error),
        Ok(report) => {
            cleanup?;
            let leaked = sensitive_occurrences(runtime.path(), &fixture_path, &sensitive_markers)?;
            if leaked != 0 {
                return Err(P0RunError::SensitiveLeak(leaked));
            }
            Ok(report)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_active(
    bundle: &P0Bundle,
    options: &P0RunOptions,
    runtime_root: &Path,
    fixture_path: &Path,
    fixture_value: &Value,
    evidence_dir: &Path,
    readiness_path: &Path,
    readiness_nonce: &str,
    reserved_addr: SocketAddr,
    run_nonce: &str,
    client_authorization: &str,
    sensitive_markers: &[Vec<u8>],
    bindings: &RuntimeBindings,
    providers: &mut [(String, NativeProvider)],
    sut: &mut ManagedChild,
    privacy_seal: &PrivateTreeSeal,
) -> Result<VerifiedP0Run, P0RunError> {
    let ready = wait_ready(
        sut,
        readiness_path,
        readiness_nonce,
        &options.reviewed_sut.executable_sha256,
        reserved_addr,
        options.timeout,
    )
    .await?;
    let sut_addr: SocketAddr = ready
        .listen_address
        .parse()
        .map_err(|_| P0RunError::Readiness("invalid listen address".into()))?;
    let mut client_outputs = BTreeMap::new();
    for case in &bundle.artifacts().corpus.cases {
        sut.ensure_running()?;
        let output = client::send(
            sut_addr,
            case,
            client_authorization,
            bindings
                .request_id(&case.id)
                .expect("every case has a request ID"),
            options.timeout,
        )
        .await?;
        client_outputs.insert(case.id.clone(), output);
    }
    sut.ensure_running()?;
    let clean_not_implemented = client_outputs
        .values()
        .all(|value| value == &not_implemented_client_output());
    let terminal_barrier = if clean_not_implemented {
        None
    } else {
        wait_for_terminal(
            bundle,
            evidence_dir,
            run_nonce,
            ready.process_id,
            bindings,
            sut,
            options.timeout,
        )
        .await?
    };
    // A terminal receipt is the sink flush promise for implemented paths. The
    // child is then reaped before any evidence snapshot, so neither a clean
    // 501 seam nor a malicious asynchronous writer can mutate files after the
    // collector decides that a channel is absent or complete.
    sut.stop(Duration::from_secs(3)).await?;
    let privacy = audit_private_runtime(runtime_root, fixture_path, readiness_path, privacy_seal);
    let mut native_by_case: BTreeMap<String, Vec<_>> = BTreeMap::new();
    let mut provider_facts = BTreeMap::<String, ProviderFacts>::new();
    for (case_id, provider) in providers.iter_mut() {
        let snapshot = provider.seal_and_snapshot(Duration::from_secs(1)).await;
        add_provider_snapshot(case_id, snapshot, &mut native_by_case, &mut provider_facts);
    }
    let mut evidence = EvidenceSet::default();
    collector::collect_native(
        &bundle.artifacts().scenario,
        &native_by_case,
        &client_outputs,
        &mut evidence,
    );
    if let Some(before) = terminal_barrier {
        let after = collector::snapshot_observations(
            bundle,
            evidence_dir,
            run_nonce,
            ready.process_id,
            bindings,
        )?
        .ok_or_else(|| {
            P0RunError::Collector("terminal evidence disappeared after child stop".into())
        })?;
        if before != after {
            return Err(P0RunError::Collector(
                "terminal evidence changed after its complete flush barrier".into(),
            ));
        }
    }
    collector::collect_observations(
        bundle,
        evidence_dir,
        run_nonce,
        ready.process_id,
        bindings,
        clean_not_implemented,
        &mut evidence,
    )?;
    collect_resources(
        &bundle.artifacts().scenario,
        runtime_root,
        fixture_path,
        sensitive_markers,
        &provider_facts,
        &privacy,
        &mut evidence,
    )?;
    let topology = TopologyEvidence {
        sut_boundary: "reviewed_external_hirouted".into(),
        executable_sha256: options.reviewed_sut.executable_sha256.clone(),
        review_head: options.reviewed_sut.review_head.clone(),
        canonical_path_verified: true,
        child_pid: ready.process_id,
        readiness_nonce_verified: true,
        publication_digest: fixture::publication_digest(fixture_value)
            .ok_or_else(|| P0RunError::Readiness("publication digest missing".into()))?
            .to_owned(),
        listener_allocation: "pingora_reserved_exact_address".into(),
        provider_listener_count: providers.len(),
        private_root_mode: privacy.private_root_mode.into(),
        secret_bearing_file_mode: privacy.secret_bearing_file_mode.into(),
        readiness_file_mode: privacy.readiness_file_mode.into(),
        process_artifacts_mode: privacy.process_artifacts_mode.into(),
        evidence_artifacts_mode: privacy.evidence_artifacts_mode.into(),
        path_identity_preserved: privacy.path_identity_preserved,
        runtime_privacy_revalidated: true,
    };
    let report = evaluate(bundle, &evidence, topology, bindings);
    validate_result(bundle, &report)?;
    verify_report_semantics(bundle, &report).map_err(P0RunError::ResultSemantics)?;
    Ok(report)
}

fn add_provider_snapshot(
    case_id: &str,
    snapshot: ProviderSnapshot,
    native_by_case: &mut BTreeMap<String, Vec<super::provider::ObservedNativeRequest>>,
    facts: &mut BTreeMap<String, ProviderFacts>,
) {
    native_by_case
        .entry(case_id.to_owned())
        .or_default()
        .extend(snapshot.observations);
    let case_facts = facts.entry(case_id.to_owned()).or_default();
    case_facts.accepted += snapshot.accepted;
    case_facts.active += snapshot.active;
    case_facts.parse_failed += snapshot.parse_failed;
    case_facts.aborted += snapshot.aborted;
    case_facts.internal_failures += snapshot.infrastructure_failures.len() as u64;
}

async fn wait_ready(
    child: &mut ManagedChild,
    path: &Path,
    nonce: &str,
    executable_sha256: &str,
    expected_addr: SocketAddr,
    bound: Duration,
) -> Result<ReadyDocument, P0RunError> {
    let child_pid = child.id()?;
    let deadline = Instant::now() + bound;
    loop {
        child.ensure_running()?;
        let response = match http::get(
            expected_addr,
            &format!("/_hiroute/oracle-ready/{nonce}"),
            &[("X-HiRoute-Oracle-Nonce", nonce)],
            Duration::from_millis(250),
        )
        .await
        {
            Ok(response) => response,
            Err(_) if Instant::now() < deadline => {
                child.ensure_running()?;
                sleep(Duration::from_millis(5)).await;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        child.ensure_running()?;
        if response.status == 200
            && let Ok(ready) = serde_json::from_slice::<ReadyDocument>(&response.body)
        {
            if ready.schema_version != READY_SCHEMA
                || ready.run_nonce != nonce
                || ready.process_id != child_pid
                || ready.executable_sha256 != executable_sha256
                || ready.listen_address != expected_addr.to_string()
            {
                return Err(P0RunError::Readiness(
                    "reported PID/nonce/digest/address is untrusted".into(),
                ));
            }
            let bytes = match read_private_bounded(path, MAX_READY_BYTES) {
                Ok(bytes) => bytes,
                Err(error) if is_snapshot_race(&error) && Instant::now() < deadline => {
                    sleep(Duration::from_millis(5)).await;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let file_ready: ReadyDocument = serde_json::from_slice(&bytes).map_err(|_| {
                P0RunError::Readiness("private readiness record is incomplete".into())
            })?;
            child.ensure_running()?;
            if serde_json::to_value(&file_ready)? != serde_json::to_value(&ready)? {
                return Err(P0RunError::Readiness(
                    "listener handshake does not match the live child".into(),
                ));
            }
            return Ok(ready);
        }
        if Instant::now() >= deadline {
            return Err(P0RunError::ReadinessTimeout);
        }
        sleep(Duration::from_millis(5)).await;
    }
}

fn reserve_loopback_addr() -> Result<SocketAddr, P0RunError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    drop(listener);
    Ok(addr)
}

fn is_snapshot_race(error: &std::io::Error) -> bool {
    matches!(
        error.to_string().as_str(),
        "private file changed during atomic snapshot"
            | "private file length changed during snapshot"
    )
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_terminal(
    bundle: &P0Bundle,
    evidence_dir: &Path,
    run_nonce: &str,
    child_pid: u32,
    bindings: &RuntimeBindings,
    child: &mut ManagedChild,
    bound: Duration,
) -> Result<Option<collector::ObservationSnapshot>, P0RunError> {
    let deadline = Instant::now() + bound;
    let mut last_error = None;
    loop {
        child.ensure_running()?;
        match collector::snapshot_observations(bundle, evidence_dir, run_nonce, child_pid, bindings)
        {
            Ok(Some(snapshot)) => return Ok(Some(snapshot)),
            Ok(None) => {}
            Err(error) => last_error = Some(error),
        }
        if Instant::now() >= deadline {
            return match last_error {
                Some(error) => Err(error.into()),
                None => Ok(None),
            };
        }
        sleep(Duration::from_millis(5)).await;
    }
}

fn collect_resources(
    scenario: &ScenarioDocument,
    root: &Path,
    fixture_path: &Path,
    sensitive_markers: &[Vec<u8>],
    provider_facts: &BTreeMap<String, ProviderFacts>,
    privacy: &super::privacy::RuntimePrivacyAudit,
    evidence: &mut EvidenceSet,
) -> Result<(), P0RunError> {
    let occurrences = sensitive_occurrences(root, fixture_path, sensitive_markers)?;
    for case in &scenario.cases {
        let facts = provider_facts.get(&case.id).copied().unwrap_or_default();
        evidence.insert(
            &case.id,
            EvidenceSource::Resource,
            json!({"facts": {
                "listener_allocation": "pingora_reserved_exact_address",
                "native_provider_aborted": facts.aborted,
                "native_provider_accepted": facts.accepted,
                "native_provider_active": facts.active,
                "native_provider_internal_failures": facts.internal_failures,
                "native_provider_parse_failed": facts.parse_failed,
                "private_root_mode": privacy.private_root_mode,
                "secret_bearing_file_mode": privacy.secret_bearing_file_mode,
                "readiness_file_mode": privacy.readiness_file_mode,
                "process_artifacts_mode": privacy.process_artifacts_mode,
                "evidence_artifacts_mode": privacy.evidence_artifacts_mode,
                "path_identity_preserved": privacy.path_identity_preserved,
                "runtime_privacy_revalidated": true,
                "publication_snapshot": "typed_digest_verified",
                "readiness": "child_pid_nonce_digest_verified",
                "secret_scan_occurrences": occurrences,
                "sut_boundary": "reviewed_external_hirouted",
                "workdir_isolation": verify_workdir(root)?
            }}),
        );
    }
    Ok(())
}

fn verify_reviewed_sut(sut: &ReviewedSut) -> Result<(), P0RunError> {
    let canonical = sut.canonical_path.canonicalize()?;
    if canonical != sut.canonical_path {
        return Err(P0RunError::SutProvenance(
            "reviewed SUT path is not canonical".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&canonical)?.permissions().mode() & 0o111 == 0 {
            return Err(P0RunError::SutProvenance(
                "reviewed SUT is not executable".into(),
            ));
        }
    }
    let actual = sha256_hex(&fs::read(&canonical)?);
    if actual != sut.executable_sha256 {
        return Err(P0RunError::SutProvenance(format!(
            "executable digest mismatch: expected {}, actual {actual}",
            sut.executable_sha256
        )));
    }
    Ok(())
}

fn validate_result(bundle: &P0Bundle, report: &P0RunReport) -> Result<(), P0RunError> {
    validate_artifact(
        bundle,
        ArtifactKind::ResultSchema,
        &serde_json::to_value(report)?,
    )
    .map_err(P0RunError::ResultSchema)
}

fn validate_fixture(bundle: &P0Bundle, fixture: &Value) -> Result<(), P0RunError> {
    validate_artifact(bundle, ArtifactKind::FixtureSchema, fixture)
        .map_err(P0RunError::FixtureSchema)?;
    validate_artifact(
        bundle,
        ArtifactKind::PublicationSchema,
        &fixture["publication"],
    )
    .map_err(P0RunError::PublicationSchema)
}

fn validate_artifact(bundle: &P0Bundle, kind: ArtifactKind, value: &Value) -> Result<(), String> {
    let path = bundle
        .artifacts()
        .artifact_paths
        .get(&kind)
        .ok_or_else(|| format!("manifest is missing schema {kind:?}"))?;
    let schema: Value = serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    validate_document(&schema, value)
}

pub fn write_report(
    bundle: &P0Bundle,
    path: &Path,
    verified: &VerifiedP0Run,
) -> Result<(), P0RunError> {
    let report = verified.as_report();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        if parent.exists() {
            verify_private_dir(parent)?;
        } else {
            create_private_dir(parent)?;
        }
    }
    validate_result(bundle, report)?;
    verify_report_semantics(bundle, report).map_err(P0RunError::ResultSemantics)?;
    let mut bytes = serde_json::to_vec_pretty(report)?;
    bytes.push(b'\n');
    private_write(path, &bytes)?;
    let persisted_report = read_report(bundle, path)?;
    if serde_json::to_value(&persisted_report)? != serde_json::to_value(report)? {
        return Err(P0RunError::ResultSemantics(
            "persisted report changed after its verified write".into(),
        ));
    }
    Ok(())
}

pub fn read_report(bundle: &P0Bundle, path: &Path) -> Result<P0RunReport, P0RunError> {
    let persisted = read_private_bounded(path, 8 * 1024 * 1024)?;
    let report: P0RunReport = serde_json::from_slice(&persisted)?;
    validate_result(bundle, &report)?;
    verify_report_semantics(bundle, &report).map_err(P0RunError::ResultSemantics)?;
    Ok(report)
}

fn random_secret(label: &str) -> Result<String, P0RunError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| P0RunError::Random(error.to_string()))?;
    let mut token = format!("p0-{label}-");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(token, "{byte:02x}").expect("String writes cannot fail");
    }
    Ok(token)
}

fn sensitive_markers(bundle: &P0Bundle, root: &Path, generated: &[&str]) -> Vec<Vec<u8>> {
    let mut markers = vec![root.as_os_str().to_string_lossy().as_bytes().to_vec()];
    markers.extend(generated.iter().map(|value| value.as_bytes().to_vec()));
    markers.extend(
        bundle
            .artifacts()
            .corpus
            .privacy_markers
            .iter()
            .map(|marker| marker.as_bytes().to_vec()),
    );
    markers
}

fn sensitive_occurrences(
    root: &Path,
    fixture: &Path,
    needles: &[Vec<u8>],
) -> Result<usize, std::io::Error> {
    let mut count = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(std::io::Error::other("symlink found in private P0 runtime"));
            }
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() && path != fixture {
                let bytes = fs::read(path)?;
                for needle in needles {
                    count += bytes
                        .windows(needle.len())
                        .filter(|value| *value == needle.as_slice())
                        .count();
                }
            } else if !file_type.is_file() {
                return Err(std::io::Error::other(
                    "non-file entry found in private P0 runtime",
                ));
            }
        }
    }
    Ok(count)
}

fn verify_workdir(root: &Path) -> Result<&'static str, P0RunError> {
    let root = root.canonicalize()?;
    let workdir = root.join("process/work/hirouted").canonicalize()?;
    if workdir != root && workdir.starts_with(&root) {
        verify_private_dir(&workdir)?;
        Ok("distinct_private")
    } else {
        Err(P0RunError::ResourceProof("workdir isolation"))
    }
}

fn path_text(path: &Path) -> Result<String, P0RunError> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or(P0RunError::NonUtf8Path)
}

#[derive(Debug, thiserror::Error)]
pub enum P0RunError {
    #[error("P0 reviewed SUT provenance failed: {0}")]
    SutProvenance(String),
    #[error("P0 runtime I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("P0 runtime JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("P0 process failed: {message}")]
    Process {
        kind: P0ProcessFailureKind,
        message: String,
    },
    #[error("P0 native client failed: {0}")]
    Client(String),
    #[error("P0 HTTP handshake failed: {0}")]
    Http(String),
    #[error("P0 evidence collection failed: {0}")]
    Collector(String),
    #[error("P0 readiness handshake failed: {0}")]
    Readiness(String),
    #[error("P0 child did not complete its readiness handshake before the deadline")]
    ReadinessTimeout,
    #[error("P0 result schema validation failed: {0}")]
    ResultSchema(String),
    #[error("P0 fixture schema validation failed: {0}")]
    FixtureSchema(String),
    #[error("P0 publication schema validation failed: {0}")]
    PublicationSchema(String),
    #[error("P0 result semantic verification failed: {0}")]
    ResultSemantics(String),
    #[error("P0 runtime path is not UTF-8")]
    NonUtf8Path,
    #[error("P0 random capability generation failed: {0}")]
    Random(String),
    #[error("P0 exact assertions did not produce the expected command status")]
    OracleStatus,
    #[error("P0 runtime artifacts contain {0} sensitive marker occurrences")]
    SensitiveLeak(usize),
    #[error("P0 resource proof failed for {0}")]
    ResourceProof(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum P0ProcessFailureKind {
    WorkDir,
    Log,
    Spawn,
    EarlyExit,
    ExitedBeforeTermination,
    ReadinessTimeout,
    Inspect,
    Terminate,
    UnprovenTermination,
    CleanupTimeout,
}

impl From<crate::process::ProcessError> for P0RunError {
    fn from(error: crate::process::ProcessError) -> Self {
        use crate::process::ProcessError;

        let kind = match &error {
            ProcessError::WorkDir { .. } => P0ProcessFailureKind::WorkDir,
            ProcessError::Log(_) => P0ProcessFailureKind::Log,
            ProcessError::Spawn { .. } => P0ProcessFailureKind::Spawn,
            ProcessError::EarlyExit(_) => P0ProcessFailureKind::EarlyExit,
            ProcessError::ExitedBeforeTermination(_) => {
                P0ProcessFailureKind::ExitedBeforeTermination
            }
            ProcessError::ReadinessTimeout(_) => P0ProcessFailureKind::ReadinessTimeout,
            ProcessError::Inspect { .. } => P0ProcessFailureKind::Inspect,
            ProcessError::Terminate { .. } => P0ProcessFailureKind::Terminate,
            ProcessError::UnprovenTermination { .. } => P0ProcessFailureKind::UnprovenTermination,
            ProcessError::CleanupTimeout(_) => P0ProcessFailureKind::CleanupTimeout,
        };
        Self::Process {
            kind,
            message: error.to_string(),
        }
    }
}

impl From<super::client::ClientError> for P0RunError {
    fn from(error: super::client::ClientError) -> Self {
        Self::Client(error.to_string())
    }
}

impl From<crate::http::HttpError> for P0RunError {
    fn from(error: crate::http::HttpError) -> Self {
        Self::Http(error.to_string())
    }
}

impl From<super::collector::CollectorError> for P0RunError {
    fn from(error: super::collector::CollectorError) -> Self {
        Self::Collector(error.to_string())
    }
}
