use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

use super::collector;
use super::contract::ProductionBundle;
use super::legacy_inputs::{RuntimeFiles, prepare as prepare_runtime_files};
use super::report;
use super::types::*;
use crate::http;
use crate::p0::bindings::RuntimeBindings;
use crate::p0::canonical::sha256_hex;
use crate::p0::client;
use crate::p0::privacy::{create_private_dir, private_write, verify_private_dir};
use crate::p0::provider::{NativeProvider, expected_native_value};
use crate::p0::schema::validate_document;
use crate::p0::types::CorpusCase;
use crate::process::ManagedChild;

pub(super) const PUBLICATION_REVISION: u64 = 22_012;
const CLIENT_TOKEN_PREFIX: &str = "oracle-client";
const PROVIDER_TOKEN_PREFIX: &str = "oracle-provider";

pub async fn run(
    bundle: &ProductionBundle,
    options: ProductionRunOptions,
) -> Result<VerifiedProductionRun, ProductionError> {
    verify_product_revision(bundle, &options.sut.source_revision)?;
    if let Some(candidate) = &bundle.candidate {
        candidate.verify_live_sut(&options.sut)?;
    }
    let mut runtime = tempfile::Builder::new()
        .prefix("hiroute-production-oracle-")
        .tempdir()?;
    create_private_dir(runtime.path())?;
    if bundle.candidate.is_some() && std::env::var("HIROUTE_E2E_KEEP_RUNTIME").as_deref() == Ok("1")
    {
        runtime.disable_cleanup(true);
        eprintln!("private current runtime: {}", runtime.path().display());
    }
    let run_nonce = random_id("run")?;
    let binding_nonce = random_id("binding")?;
    let client_token = random_id(CLIENT_TOKEN_PREFIX)?;
    let provider_token = random_id(PROVIDER_TOKEN_PREFIX)?;
    let bindings = RuntimeBindings::from_nonces(
        binding_nonce,
        run_nonce.clone(),
        [bundle.fixture.case.id.as_str()],
    );
    let case = materialized_case(&bundle.fixture.case, &bindings)?;
    let expected_client = bundle.expected_client_for(
        &case,
        bindings.materialize_global(&bundle.fixture.expected_client),
    )?;
    let client_authorization = format!("Bearer {client_token}");
    let provider_authorization = format!("Bearer {provider_token}");
    let sequence = Arc::new(AtomicU64::new(1));
    let script = case
        .providers
        .first()
        .ok_or_else(|| ProductionError::Contract("production Provider script missing".into()))?;
    let mut provider =
        NativeProvider::start_exact(script, &provider_authorization, sequence, &bindings).await?;
    let mut files = prepare_runtime_files(
        runtime.path(),
        provider.addr(),
        &client_token,
        &provider_token,
    )?;
    let mut current_upstream = if bundle.candidate.is_some() {
        let (upstream, digest) = super::current_inputs::CurrentUpstream::start(
            runtime.path(),
            provider.addr(),
            &sha256_hex(client_token.as_bytes()),
        )?;
        files.publication_digest = digest;
        Some(upstream)
    } else {
        None
    };
    let reserved_addr = reserve_loopback_addr()?;
    let args = production_arguments(reserved_addr, &files)?;
    let mut environment = production_environment(&files)?;
    if bundle.candidate.is_some() {
        environment.insert(
            hiroute_gateway::server::LAUNCHER_EXECUTABLE_SHA256_ENV.into(),
            options.sut.executable_sha256.clone(),
        );
    }
    if current_upstream.is_some() {
        environment.insert(
            crate::gateway_fixture::E2E_DIAL_CONFIG_ENV.into(),
            path_text(
                &runtime
                    .path()
                    .join("inputs")
                    .join(crate::gateway_fixture::E2E_DIAL_CONFIG_FILE),
            )?,
        );
    }
    let log_dir = runtime.path().join("process");
    let prepared = ManagedChild::prepare("hirouted", &log_dir)?;
    let mut child = prepared.spawn_with_env(
        &options.sut.canonical_path,
        &args,
        &environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>(),
    )?;
    let child_pid = child.id()?;
    let launcher = LauncherRecord {
        schema_version: bundle.launcher_schema().into(),
        mode: "production_publication_credentials".into(),
        executable_path: path_text(&options.sut.canonical_path)?,
        executable_sha256: options.sut.executable_sha256.clone(),
        sut_source_revision: options.sut.source_revision.clone(),
        build_attestation: options.sut.build_attestation.clone(),
        child_pid,
        listen_address: reserved_addr.to_string(),
        arguments: args,
        environment,
        publication_digest: files.publication_digest.clone(),
        credential_manifest_digest: files.credential_manifest_digest.clone(),
        credential_lease_digest: files.credential_lease_digest.clone(),
    };
    validate_document(
        bundle.schema("schema/p0-production-launcher.schema.json")?,
        &serde_json::to_value(&launcher)?,
    )
    .map_err(|detail| ProductionError::Contract(format!("launcher schema: {detail}")))?;
    report::verify_launcher_for(bundle, &launcher)?;

    let active = run_active(
        bundle,
        &options,
        runtime.path(),
        &files,
        &case,
        &expected_client,
        &client_authorization,
        &run_nonce,
        bindings.challenge(),
        reserved_addr,
        &launcher,
        &mut child,
        &mut provider,
    )
    .await;
    if active.is_err() {
        let _ = child.shutdown(Duration::from_secs(3)).await;
        if bundle.candidate.is_some() {
            let snapshot = provider.seal_and_snapshot(Duration::from_secs(2)).await;
            let observations = snapshot
                .observations
                .iter()
                .map(|v| &v.value)
                .collect::<Vec<_>>();
            let _ = private_write(
                &runtime.path().join("native.observed.json"),
                &serde_json::to_vec(&observations)?,
            );
        }
        provider.shutdown(Duration::from_secs(3)).await;
    }
    let transport = current_upstream.as_mut().map(|u| u.finish()).transpose();
    let active = active.and_then(|result| {
        transport?;
        Ok(result)
    });
    match active {
        Err(error) if bundle.candidate.is_some() => Err(error),
        Err(error) if std::env::var("HIROUTE_E2E_KEEP_RUNTIME").as_deref() == Ok("1") => {
            let path = runtime.keep();
            Err(ProductionError::Contract(format!(
                "{error}; retained diagnostic runtime at {}",
                path.display()
            )))
        }
        result => {
            if bundle.candidate.is_some() && result.is_ok() {
                runtime.close()?;
            }
            result
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_active(
    bundle: &ProductionBundle,
    options: &ProductionRunOptions,
    runtime_root: &Path,
    files: &RuntimeFiles,
    case: &CorpusCase,
    expected_client: &Value,
    client_authorization: &str,
    run_nonce: &str,
    freshness_challenge: &str,
    address: SocketAddr,
    launcher: &LauncherRecord,
    child: &mut ManagedChild,
    provider: &mut NativeProvider,
) -> Result<VerifiedProductionRun, ProductionError> {
    let case_deadline = Instant::now() + options.timeout;
    let remaining = || {
        if bundle.candidate.is_some() {
            case_deadline
                .saturating_duration_since(Instant::now())
                .saturating_sub(Duration::from_millis(250))
        } else {
            options.timeout
        }
    };
    let readiness = wait_ready(
        child,
        address,
        &options.sut.executable_sha256,
        &files.publication_digest,
        remaining(),
    )
    .await?;
    validate_document(
        bundle.schema("schema/p0-production-readiness.schema.json")?,
        &serde_json::to_value(&readiness)?,
    )
    .map_err(|detail| ProductionError::Readiness(format!("readiness schema: {detail}")))?;
    child.ensure_running()?;
    if bundle.candidate.is_some() {
        private_write(&runtime_root.join("ready.observed"), b"ready")?;
    }
    let client_output = client::send(
        address,
        case,
        client_authorization,
        &format!("oracle-client-{run_nonce}"),
        remaining(),
    )
    .await?;
    child.ensure_running()?;
    if bundle.candidate.is_some() {
        private_write(
            &runtime_root.join("client.observed.json"),
            &serde_json::to_vec(&client_output)?,
        )?;
    }
    collector::wait_for_independent_terminals(
        &files.observation_dir,
        &bundle.fixture.expected_observation,
        &files.publication_digest,
        PUBLICATION_REVISION,
        freshness_challenge,
        remaining(),
        bundle.candidate.is_some(),
    )
    .await
    .map_err(|error| {
        ProductionError::Collector(format!(
            "{error}; client output was {}",
            serde_json::to_string(&client_output).unwrap_or_else(|_| "<invalid>".into())
        ))
    })?;
    child.stop(Duration::from_secs(3)).await?;
    let observations = collector::collect(
        &files.observation_dir,
        &bundle.fixture.expected_observation,
        &files.publication_digest,
        PUBLICATION_REVISION,
        freshness_challenge,
        bundle.candidate.is_some(),
    )?;
    validate_document(
        bundle.schema("schema/p0-production-collector.schema.json")?,
        &serde_json::to_value(&observations)?,
    )
    .map_err(|detail| ProductionError::Collector(format!("collector schema: {detail}")))?;
    let snapshot = provider.seal_and_snapshot(Duration::from_secs(2)).await;
    let mut observed = snapshot
        .observations
        .into_iter()
        .map(|item| (item.sequence, item.value))
        .collect::<Vec<_>>();
    observed.sort_by_key(|(sequence, _)| *sequence);
    let entries = observed
        .into_iter()
        .enumerate()
        .map(|(index, (_, mut value))| {
            value
                .as_object_mut()
                .expect("native capture is an object")
                .insert("ordinal".into(), Value::from(index + 1));
            value
        })
        .collect::<Vec<_>>();
    let native_provider = json!({
        "entries": entries,
        "accepted": snapshot.accepted,
        "active": snapshot.active,
        "parse_failed": snapshot.parse_failed,
        "aborted": snapshot.aborted,
        "infrastructure_failures": snapshot.infrastructure_failures,
    });
    let mut expected_native_entry = expected_native_value(&case.providers[0], 1);
    expected_native_entry
        .as_object_mut()
        .expect("native expectation is an object")
        .insert(
            "body".into(),
            case.providers[0].expected_request.body.clone(),
        );
    let expected_native = json!({
        "entries": [expected_native_entry],
        "accepted": 1,
        "active": 0,
        "parse_failed": 0,
        "aborted": 0,
        "infrastructure_failures": [],
    });
    let privacy = verify_runtime_privacy(
        runtime_root,
        &files.observation_dir,
        &[
            client_authorization.as_bytes(),
            PROVIDER_TOKEN_PREFIX.as_bytes(),
        ],
    )?;
    let report = report::build(
        bundle,
        run_nonce,
        freshness_challenge,
        launcher.clone(),
        readiness,
        expected_native,
        native_provider,
        expected_client.clone(),
        client_output,
        observations,
        privacy,
    )?;
    validate_document(
        bundle.schema("schema/p0-gateway-result.schema.json")?,
        &serde_json::to_value(report.as_report())?,
    )
    .map_err(|detail| ProductionError::Contract(format!("result schema: {detail}")))?;
    Ok(report)
}

fn production_arguments(
    address: SocketAddr,
    files: &RuntimeFiles,
) -> Result<Vec<String>, ProductionError> {
    Ok(vec![
        "--role".into(),
        "gateway".into(),
        "--listen".into(),
        address.to_string(),
        "--lkg".into(),
        path_text(&files.lkg_path)?,
        "--publication".into(),
        path_text(&files.publication_path)?,
        "--credentials".into(),
        path_text(&files.credential_manifest_path)?,
    ])
}

fn production_environment(
    files: &RuntimeFiles,
) -> Result<BTreeMap<String, String>, ProductionError> {
    Ok(BTreeMap::from([
        ("HIROUTE_E2E_OBSERVATION_CAPTURE".into(), "1".into()),
        (
            "HIROUTE_OBSERVATION_DIRECTORY".into(),
            path_text(&files.observation_dir)?,
        ),
        ("HIROUTE_OBSERVATION_QUEUE_BYTES".into(), "4194304".into()),
        (
            "HIROUTE_OBSERVATION_LIFECYCLE_SINK".into(),
            "healthy".into(),
        ),
        (
            "HIROUTE_OBSERVATION_EXECUTION_SINK".into(),
            "healthy".into(),
        ),
        ("HIROUTE_OBSERVATION_CONTENT_SINK".into(), "healthy".into()),
        ("HIROUTE_OBSERVATION_OTEL_SINK".into(), "healthy".into()),
    ]))
}

async fn wait_ready(
    child: &mut ManagedChild,
    address: SocketAddr,
    executable_sha256: &str,
    publication_digest: &str,
    bound: Duration,
) -> Result<ReadinessEvidence, ProductionError> {
    let child_pid = child.id()?;
    let deadline = Instant::now() + bound;
    loop {
        child.ensure_running()?;
        let (listener_owner_pid_before_probe, listener_owner_verifier) =
            match super::listener::owner(child_pid, address) {
                Ok(evidence) => evidence,
                Err(_) if Instant::now() < deadline => {
                    sleep(Duration::from_millis(5)).await;
                    continue;
                }
                Err(error) => return Err(error),
            };
        let response =
            match http::get(address, "/_hiroute/ready", &[], Duration::from_millis(250)).await {
                Ok(response) => response,
                Err(_) if Instant::now() < deadline => {
                    sleep(Duration::from_millis(5)).await;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
        if response.status != 200 {
            if Instant::now() >= deadline {
                return Err(ProductionError::Readiness(format!(
                    "readiness returned HTTP {}",
                    response.status
                )));
            }
            sleep(Duration::from_millis(5)).await;
            continue;
        }
        let product: ProductReady = serde_json::from_slice(&response.body).map_err(|error| {
            ProductionError::Readiness(format!("readiness body is corrupt: {error}"))
        })?;
        child.ensure_running()?;
        let (listener_owner_pid_after_probe, verifier_after_probe) =
            super::listener::owner(child_pid, address)?;
        if verifier_after_probe != listener_owner_verifier {
            return Err(ProductionError::Readiness(
                "listener ownership verifier changed across the readiness probe".into(),
            ));
        }
        if product.schema_version != PRODUCT_READINESS_SCHEMA
            || product.status != "ready"
            || product.publication_revision != PUBLICATION_REVISION
            || product.publication_digest != publication_digest
            || product.executable_sha256 != executable_sha256
        {
            return Err(ProductionError::Readiness(
                "product readiness revision/publication/binary evidence is not exact".into(),
            ));
        }
        return Ok(ReadinessEvidence {
            schema_version: READINESS_SCHEMA.into(),
            child_pid,
            listen_address: address.to_string(),
            probe_path: "/_hiroute/ready".into(),
            product,
            listener_owner_verifier: listener_owner_verifier.into(),
            listener_owner_pid_before_probe,
            listener_owner_pid_after_probe,
            child_alive_before_probe: true,
            child_alive_after_probe: true,
        });
    }
}

fn materialized_case(
    case: &CorpusCase,
    bindings: &RuntimeBindings,
) -> Result<CorpusCase, ProductionError> {
    let value = serde_json::to_value(case)?;
    Ok(serde_json::from_value(bindings.materialize_global(&value))?)
}

fn reserve_loopback_addr() -> Result<SocketAddr, ProductionError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    Ok(address)
}

fn random_id(prefix: &str) -> Result<String, ProductionError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| ProductionError::Contract(format!("randomness unavailable: {error}")))?;
    Ok(format!("{prefix}-{}", hex(&bytes)))
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

fn verify_product_revision(
    bundle: &ProductionBundle,
    revision: &str,
) -> Result<(), ProductionError> {
    if revision != bundle.profile.sut_source_revision {
        return Err(ProductionError::Provenance(
            "resolved SUT revision differs from the sealed profile".into(),
        ));
    }
    let repository = bundle
        .root
        .parent()
        .ok_or_else(|| ProductionError::Provenance("E2E root has no repository".into()))?;
    let exists = std::process::Command::new("git")
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .current_dir(repository)
        .status()?;
    let unchanged = std::process::Command::new("git")
        .args([
            "diff",
            "--quiet",
            revision,
            "--",
            "crates/gateway",
            "crates/gateway-core",
        ])
        .current_dir(repository)
        .status()?;
    let untracked_or_modified = std::process::Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--",
            "crates/gateway",
            "crates/gateway-core",
        ])
        .current_dir(repository)
        .output()?;
    if !exists.success()
        || !unchanged.success()
        || !untracked_or_modified.status.success()
        || !untracked_or_modified.stdout.is_empty()
    {
        return Err(ProductionError::Provenance(
            "Gateway product sources differ from the sealed SUT revision".into(),
        ));
    }
    Ok(())
}

fn verify_runtime_privacy(
    runtime_root: &Path,
    observation_root: &Path,
    sensitive: &[&[u8]],
) -> Result<PrivacyEvidence, ProductionError> {
    verify_private_dir(runtime_root)?;
    verify_private_dir(observation_root)?;
    let process_root = runtime_root.join("process");
    verify_private_dir(&process_root)?;
    let occurrences = count_occurrences(&[observation_root, &process_root], sensitive)?;
    Ok(PrivacyEvidence {
        private_root_mode: "owner_only".into(),
        observation_root_mode: "owner_only".into(),
        process_root_mode: "owner_only".into(),
        sensitive_occurrences: occurrences,
    })
}

fn count_occurrences(roots: &[&Path], sensitive: &[&[u8]]) -> Result<usize, ProductionError> {
    let mut count = 0;
    let mut pending = roots
        .iter()
        .map(|path| path.to_path_buf())
        .collect::<Vec<_>>();
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let bytes = std::fs::read(entry.path())?;
                for marker in sensitive {
                    count += bytes
                        .windows(marker.len())
                        .filter(|window| window == marker)
                        .count();
                }
            } else {
                return Err(ProductionError::Contract(
                    "non-file runtime artifact is forbidden".into(),
                ));
            }
        }
    }
    Ok(count)
}
