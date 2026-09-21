use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::Path;

use serde_json::{Value, json};

use super::collector::verify_collector_evidence_for;
use super::contract::ProductionBundle;
use super::types::*;
use crate::p0::canonical::canonical_json_digest;
use crate::p0::privacy::private_write;
use crate::p0::provider::expected_native_value;
use crate::p0::schema::validate_document;
use crate::p0::types::CorpusCase;

#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    bundle: &ProductionBundle,
    run_nonce: &str,
    freshness_challenge: &str,
    launcher: LauncherRecord,
    readiness: ReadinessEvidence,
    expected_native: Value,
    native_provider: Value,
    expected_client: Value,
    client_output: Value,
    observations: CollectorEvidence,
    privacy: PrivacyEvidence,
) -> Result<VerifiedProductionRun, ProductionError> {
    let checks = derive_checks(
        bundle,
        &launcher,
        &readiness,
        &expected_native,
        &native_provider,
        &expected_client,
        &client_output,
        &observations,
        &privacy,
    );
    let all_green = checks
        .iter()
        .all(|check| check.status == EvidenceStatus::Passed);
    let scenario_state = if all_green {
        ScenarioState::Green
    } else {
        ScenarioState::Red
    };
    let checkpoints = derive_checkpoints(bundle, &checks)?;
    let evidence_digest = canonical_json_digest(&json!({
        "launcher": launcher,
        "readiness": readiness,
        "native_provider": native_provider,
        "client_output": client_output,
        "observations": observations,
        "privacy": privacy,
    }));
    let mut report = ProductionRunReport {
        schema_version: bundle.result_schema().into(),
        oracle_version: ORACLE_VERSION.into(),
        contract_digest: bundle.manifest.contract_digest.clone(),
        aggregate_port_digest: bundle.aggregate_port_digest().into(),
        schema_digests: bundle.schema_digests(),
        evidence_digest,
        result_payload_digest: String::new(),
        process_exit: ProcessExitEvidence {
            test_process_code: if all_green { 0 } else { 1 },
            sut_termination: "harness_initiated_after_independent_terminals".into(),
            sut_reaped: true,
        },
        scenario_state,
        run_nonce: run_nonce.into(),
        freshness_challenge: freshness_challenge.into(),
        launcher,
        readiness,
        native_provider,
        client_output,
        observations,
        privacy,
        checks,
        checkpoints,
    };
    report.result_payload_digest = result_digest(&report)?;
    verify_report(bundle, &report)?;
    Ok(VerifiedProductionRun(report))
}

pub fn verify_report(
    bundle: &ProductionBundle,
    report: &ProductionRunReport,
) -> Result<(), ProductionError> {
    validate_document(
        bundle.schema("schema/p0-gateway-result.schema.json")?,
        &serde_json::to_value(report)?,
    )
    .map_err(|detail| ProductionError::Contract(format!("result evidence: {detail}")))?;
    if report.schema_version != bundle.result_schema()
        || report.oracle_version != ORACLE_VERSION
        || report.contract_digest != bundle.manifest.contract_digest
        || report.aggregate_port_digest != bundle.aggregate_port_digest()
        || report.schema_digests != bundle.schema_digests()
        || report.result_payload_digest != result_digest(report)?
        || report.run_nonce.len() < 32
        || report.freshness_challenge.len() < 32
    {
        return Err(ProductionError::Contract(
            "production result identity/digest is not exact".into(),
        ));
    }
    validate_document(
        bundle.schema("schema/p0-production-launcher.schema.json")?,
        &serde_json::to_value(&report.launcher)?,
    )
    .map_err(|detail| ProductionError::Contract(format!("launcher evidence: {detail}")))?;
    verify_launcher_for(bundle, &report.launcher)?;
    validate_document(
        bundle.schema("schema/p0-production-readiness.schema.json")?,
        &serde_json::to_value(&report.readiness)?,
    )
    .map_err(|detail| ProductionError::Readiness(format!("readiness evidence: {detail}")))?;
    verify_readiness_evidence(&report.launcher, &report.readiness)?;
    validate_document(
        bundle.schema("schema/p0-production-collector.schema.json")?,
        &serde_json::to_value(&report.observations)?,
    )
    .map_err(|detail| ProductionError::Collector(format!("collector evidence: {detail}")))?;
    verify_collector_evidence_for(
        &report.observations,
        &bundle.fixture.expected_observation,
        &report.launcher.publication_digest,
        report.readiness.product.publication_revision,
        &report.freshness_challenge,
        bundle.candidate.is_some(),
    )?;
    let materialized = materialize_freshness(
        serde_json::to_value(&bundle.fixture.case)?,
        &report.freshness_challenge,
    );
    let case: CorpusCase = serde_json::from_value(materialized)?;
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
    let expected_client = bundle.expected_client_for(
        &case,
        materialize_freshness(
            bundle.fixture.expected_client.clone(),
            &report.freshness_challenge,
        ),
    )?;
    let expected_checks = derive_checks(
        bundle,
        &report.launcher,
        &report.readiness,
        &expected_native,
        &report.native_provider,
        &expected_client,
        &report.client_output,
        &report.observations,
        &report.privacy,
    );
    if serde_json::to_value(&expected_checks)? != serde_json::to_value(&report.checks)? {
        return Err(ProductionError::Contract(
            "production evidence checks are not derivable from exact evidence".into(),
        ));
    }
    let checkpoints = derive_checkpoints(bundle, &report.checks)?;
    if serde_json::to_value(&checkpoints)? != serde_json::to_value(&report.checkpoints)? {
        return Err(ProductionError::Contract(
            "production checkpoints are not derived from evidence".into(),
        ));
    }
    let green = report
        .checks
        .iter()
        .all(|check| check.status == EvidenceStatus::Passed);
    if (green && report.scenario_state != ScenarioState::Green)
        || (!green && report.scenario_state != ScenarioState::Red)
        || report.process_exit.test_process_code != if green { 0 } else { 1 }
        || report.process_exit.sut_termination != "harness_initiated_after_independent_terminals"
        || !report.process_exit.sut_reaped
    {
        return Err(ProductionError::Contract(
            "process exit and scenario state are not independently derived".into(),
        ));
    }
    let evidence_digest = canonical_json_digest(&json!({
        "launcher": report.launcher,
        "readiness": report.readiness,
        "native_provider": report.native_provider,
        "client_output": report.client_output,
        "observations": report.observations,
        "privacy": report.privacy,
    }));
    if evidence_digest != report.evidence_digest {
        return Err(ProductionError::Contract(
            "production evidence digest is stale or corrupt".into(),
        ));
    }
    Ok(())
}

pub fn verify_launcher_evidence(launcher: &LauncherRecord) -> Result<(), ProductionError> {
    verify_launcher_identity(launcher, LAUNCHER_SCHEMA, SEALED_SUT_REVISION)?;
    super::attestation::verify(&launcher.build_attestation)
}

pub(super) fn verify_launcher_for(
    bundle: &ProductionBundle,
    launcher: &LauncherRecord,
) -> Result<(), ProductionError> {
    if let Some(candidate) = &bundle.candidate {
        verify_launcher_identity(launcher, super::candidate::LAUNCHER_V2, &candidate.revision)?;
        candidate.verify_attestation(&launcher.build_attestation)
    } else {
        verify_launcher_evidence(launcher)
    }
}

fn verify_launcher_identity(
    launcher: &LauncherRecord,
    schema: &str,
    revision: &str,
) -> Result<(), ProductionError> {
    let address = launcher
        .listen_address
        .parse::<SocketAddr>()
        .map_err(|_| ProductionError::Contract("launcher address is invalid".into()))?;
    let args = &launcher.arguments;
    if launcher.schema_version != schema
        || launcher.mode != "production_publication_credentials"
        || launcher.sut_source_revision != revision
        || !valid_sha256(&launcher.executable_sha256)
        || launcher.build_attestation.source_revision != launcher.sut_source_revision
        || launcher.build_attestation.executable_path != launcher.executable_path
        || launcher.build_attestation.executable_sha256 != launcher.executable_sha256
        || !valid_sha256(&launcher.publication_digest)
        || !valid_sha256(&launcher.credential_manifest_digest)
        || !valid_sha256(&launcher.credential_lease_digest)
        || launcher.child_pid == 0
        || !address.ip().is_loopback()
        || address.port() == 0
        || !Path::new(&launcher.executable_path).is_absolute()
        || args.len() != 10
        || args[0] != "--role"
        || args[1] != "gateway"
        || args[2] != "--listen"
        || args[3] != launcher.listen_address
        || args[4] != "--lkg"
        || args[6] != "--publication"
        || args[8] != "--credentials"
        || args.iter().any(|argument| argument == "--fixture")
    {
        return Err(ProductionError::Contract(
            "launcher is not the exact normal production hirouted contract".into(),
        ));
    }
    let lkg = Path::new(&args[5]);
    let publication = Path::new(&args[7]);
    let credentials = Path::new(&args[9]);
    if !lkg.is_absolute()
        || !publication.is_absolute()
        || !credentials.is_absolute()
        || lkg.file_name().and_then(|name| name.to_str()) != Some("publication-lkg.json")
        || publication.file_name().and_then(|name| name.to_str()) != Some("publication.json")
        || credentials.file_name().and_then(|name| name.to_str()) != Some("credentials.json")
        || lkg.parent() != publication.parent()
        || publication.parent() != credentials.parent()
    {
        return Err(ProductionError::Contract(
            "launcher publication/credential runtime inputs are not exact".into(),
        ));
    }
    let observation = launcher
        .environment
        .get("HIROUTE_OBSERVATION_DIRECTORY")
        .map(Path::new)
        .ok_or_else(|| ProductionError::Contract("observation directory is missing".into()))?;
    let mut expected_environment = BTreeMap::from([
        ("HIROUTE_E2E_OBSERVATION_CAPTURE", "1"),
        (
            "HIROUTE_OBSERVATION_DIRECTORY",
            launcher.environment["HIROUTE_OBSERVATION_DIRECTORY"].as_str(),
        ),
        ("HIROUTE_OBSERVATION_QUEUE_BYTES", "4194304"),
        ("HIROUTE_OBSERVATION_LIFECYCLE_SINK", "healthy"),
        ("HIROUTE_OBSERVATION_EXECUTION_SINK", "healthy"),
        ("HIROUTE_OBSERVATION_CONTENT_SINK", "healthy"),
        ("HIROUTE_OBSERVATION_OTEL_SINK", "healthy"),
    ]);
    let dial_path = publication
        .parent()
        .unwrap()
        .join(crate::gateway_fixture::E2E_DIAL_CONFIG_FILE);
    if schema == super::candidate::LAUNCHER_V2 {
        expected_environment.insert(
            hiroute_gateway::server::LAUNCHER_EXECUTABLE_SHA256_ENV,
            launcher.executable_sha256.as_str(),
        );
        expected_environment.insert(
            crate::gateway_fixture::E2E_DIAL_CONFIG_ENV,
            dial_path
                .to_str()
                .ok_or_else(|| ProductionError::Contract("invalid controlled dial path".into()))?,
        );
    }
    if !observation.is_absolute()
        || observation.file_name().and_then(|name| name.to_str()) != Some("observation")
        || lkg.parent().and_then(Path::parent) != observation.parent()
        || launcher.environment.len() != expected_environment.len()
        || launcher.environment.iter().any(|(key, value)| {
            expected_environment.get(key.as_str()).copied() != Some(value.as_str())
        })
    {
        return Err(ProductionError::Contract(
            "launcher observation runtime inputs are not exact".into(),
        ));
    }
    Ok(())
}

pub fn verify_readiness_evidence(
    launcher: &LauncherRecord,
    readiness: &ReadinessEvidence,
) -> Result<(), ProductionError> {
    if readiness.schema_version != READINESS_SCHEMA
        || readiness.product.schema_version != PRODUCT_READINESS_SCHEMA
        || readiness.product.status != "ready"
        || readiness.child_pid != launcher.child_pid
        || readiness.listen_address != launcher.listen_address
        || readiness.probe_path != "/_hiroute/ready"
        || readiness.product.publication_revision != 22_012
        || readiness.product.publication_digest != launcher.publication_digest
        || readiness.product.executable_sha256 != launcher.executable_sha256
        || !matches!(
            readiness.listener_owner_verifier.as_str(),
            "darwin_lsof_tcp_listener/v1" | "linux_procfs_tcp_listener/v1"
        )
        || readiness.listener_owner_pid_before_probe != launcher.child_pid
        || readiness.listener_owner_pid_after_probe != launcher.child_pid
        || !readiness.child_alive_before_probe
        || !readiness.child_alive_after_probe
    {
        return Err(ProductionError::Readiness(
            "readiness does not bind the exact live launcher child".into(),
        ));
    }
    Ok(())
}

pub fn verify_native_evidence(expected: &Value, actual: &Value) -> Result<(), ProductionError> {
    if expected == actual {
        Ok(())
    } else {
        Err(ProductionError::Contract(
            "native Provider payload/call evidence is not exact".into(),
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn derive_checks(
    bundle: &ProductionBundle,
    launcher: &LauncherRecord,
    readiness: &ReadinessEvidence,
    expected_native: &Value,
    native_provider: &Value,
    expected_client: &Value,
    client_output: &Value,
    observations: &CollectorEvidence,
    privacy: &PrivacyEvidence,
) -> Vec<EvidenceCheck> {
    let exact = |id: &str, expected: &Value, actual: &Value, detail: &str| EvidenceCheck {
        id: id.into(),
        status: if expected == actual {
            EvidenceStatus::Passed
        } else {
            EvidenceStatus::Failed
        },
        expected_digest: canonical_json_digest(expected),
        actual_digest: canonical_json_digest(actual),
        detail: detail.into(),
    };
    let launcher_value = serde_json::to_value(launcher).expect("launcher serializes");
    let readiness_value = serde_json::to_value(readiness).expect("readiness serializes");
    let lifecycle = serde_json::to_value(&observations.lifecycle).expect("channel serializes");
    let execution = serde_json::to_value(&observations.execution_fact).expect("channel serializes");
    let content =
        serde_json::to_value(&observations.conversation_content).expect("channel serializes");
    let otel = serde_json::to_value(&observations.otel).expect("channel serializes");
    let aggregate = json!({
        "port_digest": bundle.aggregate_port_digest(),
        "independent_streams": observations.independent_streams,
        "content_terminal_independent": observations.content_terminal_independent,
    });
    let privacy_expected = serde_json::to_value(PrivacyEvidence {
        private_root_mode: "owner_only".into(),
        observation_root_mode: "owner_only".into(),
        process_root_mode: "owner_only".into(),
        sensitive_occurrences: 0,
    })
    .expect("privacy serializes");
    let privacy = serde_json::to_value(privacy).expect("privacy serializes");
    vec![
        exact(
            "launch_production",
            &launcher_value,
            &launcher_value,
            "normal hirouted --publication --credentials launch",
        ),
        exact(
            "readiness_exact",
            &readiness_value,
            &readiness_value,
            "live child readiness binds publication and executable digests",
        ),
        exact(
            "listener_client",
            expected_client,
            client_output,
            "production listener client response",
        ),
        exact(
            "native_provider",
            expected_native,
            native_provider,
            "native Provider payload and call accounting",
        ),
        exact(
            "lifecycle_stream",
            &lifecycle,
            &lifecycle,
            "independent Lifecycle terminal/order/gap evidence",
        ),
        exact(
            "execution_stream",
            &execution,
            &execution,
            "independent ExecutionFact terminal/order/gap evidence",
        ),
        exact(
            "content_stream",
            &content,
            &content,
            "independent ConversationContent finish barrier",
        ),
        exact(
            "otel_stream",
            &otel,
            &otel,
            "supplemental content-free OTel evidence",
        ),
        exact(
            "privacy_resource",
            &privacy_expected,
            &privacy,
            "private runtime and secret-free evidence",
        ),
        exact(
            "aggregate_port",
            &json!({
                "port_digest": bundle.aggregate_port_digest(),
                "independent_streams": true,
                "content_terminal_independent": true,
            }),
            &aggregate,
            &format!("{} contract aggregate", bundle.scenario.name),
        ),
    ]
}

fn derive_checkpoints(
    bundle: &ProductionBundle,
    checks: &[EvidenceCheck],
) -> Result<Vec<CheckpointResult>, ProductionError> {
    let statuses = checks
        .iter()
        .map(|check| (check.id.as_str(), check.status))
        .collect::<BTreeMap<_, _>>();
    if statuses.len() != checks.len() {
        return Err(ProductionError::Contract(
            "production evidence check identity is duplicated".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    let checkpoints = bundle
        .scenario
        .checkpoints
        .iter()
        .map(|checkpoint| {
            let evidence_statuses = checkpoint
                .evidence
                .iter()
                .map(|id| {
                    seen.insert(id.as_str());
                    statuses.get(id.as_str()).copied().ok_or_else(|| {
                        ProductionError::Contract(format!(
                            "checkpoint names unknown evidence: {id}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let status = if evidence_statuses
                .iter()
                .all(|status| *status == EvidenceStatus::Passed)
            {
                EvidenceStatus::Passed
            } else {
                EvidenceStatus::Failed
            };
            Ok::<CheckpointResult, ProductionError>(CheckpointResult {
                id: checkpoint.id.clone(),
                status,
                evidence: checkpoint.evidence.clone(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if seen != statuses.keys().copied().collect() {
        return Err(ProductionError::Contract(
            "production checkpoints do not consume every evidence check".into(),
        ));
    }
    Ok(checkpoints)
}

fn materialize_freshness(value: Value, challenge: &str) -> Value {
    match value {
        Value::String(value) if value == "${RUN_CHALLENGE}" => Value::String(challenge.into()),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| materialize_freshness(value, challenge))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, materialize_freshness(value, challenge)))
                .collect(),
        ),
        value => value,
    }
}

fn result_digest(report: &ProductionRunReport) -> Result<String, ProductionError> {
    let mut value = serde_json::to_value(report)?;
    value["result_payload_digest"] = Value::String(String::new());
    Ok(canonical_json_digest(&value))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn write_report(
    path: &std::path::Path,
    report: &ProductionRunReport,
) -> Result<(), ProductionError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(report)?;
    bytes.push(b'\n');
    private_write(path, &bytes)?;
    Ok(())
}

pub fn read_report(
    bundle: &ProductionBundle,
    path: &std::path::Path,
) -> Result<ProductionRunReport, ProductionError> {
    let report: ProductionRunReport = serde_json::from_slice(&std::fs::read(path)?)?;
    verify_report(bundle, &report)?;
    Ok(report)
}
