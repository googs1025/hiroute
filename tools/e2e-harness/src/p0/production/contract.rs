use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::types::*;
use crate::p0::canonical::canonical_json_digest;
use crate::p0::schema::validate_document;
use crate::p0::types::{AuthorizationFixture, GrantAccess};

const MANIFEST_FILE: &str = "p0-production-manifest.json";
const MANIFEST_SCHEMA_FILE: &str = "p0-production-manifest.schema.json";

const ARTIFACTS: &[(&str, Option<&str>)] = &[
    (
        "fixtures/p0-oracle/production-smoke.json",
        Some("schema/p0-production-fixture.schema.json"),
    ),
    (
        "profiles/gateway-isolated.json",
        Some("schema/p0-gateway-profile.schema.json"),
    ),
    (
        "scenarios/p0-gateway.json",
        Some("schema/p0-gateway-scenario.schema.json"),
    ),
    ("schema/p0-gateway-profile.schema.json", None),
    ("schema/p0-gateway-result.schema.json", None),
    ("schema/p0-gateway-scenario.schema.json", None),
    ("schema/p0-production-collector.schema.json", None),
    ("schema/p0-production-fixture.schema.json", None),
    ("schema/p0-production-launcher.schema.json", None),
    ("schema/p0-production-manifest.schema.json", None),
    ("schema/p0-production-readiness.schema.json", None),
];

#[derive(Clone, Debug)]
pub struct ProductionBundle {
    pub(crate) root: PathBuf,
    pub(super) candidate: Option<super::candidate::CandidateIdentity>,
    pub(crate) scenario: ProductionScenario,
    pub(crate) fixture: ProductionFixture,
    pub(crate) profile: ProductionProfile,
    pub(crate) manifest: ProductionManifest,
    pub(crate) schemas: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProductionValidationSummary {
    pub oracle_version: &'static str,
    pub contract_digest: String,
    pub case_count: usize,
    pub completion_policy: String,
    pub artifact_digests: BTreeMap<String, String>,
    pub schema_digests: BTreeMap<String, String>,
}

impl ProductionBundle {
    pub fn load(
        scenario_path: &Path,
        profile_path: Option<&Path>,
        schema_dir: &Path,
    ) -> Result<Self, ProductionError> {
        let root = schema_dir
            .parent()
            .ok_or_else(|| ProductionError::Contract("schema directory has no E2E root".into()))?
            .canonicalize()?;
        let manifest_path = schema_dir.join(MANIFEST_FILE);
        let manifest_schema = read_value(&schema_dir.join(MANIFEST_SCHEMA_FILE))?;
        let manifest_value = read_value(&manifest_path)?;
        validate_document(&manifest_schema, &manifest_value).map_err(|detail| {
            ProductionError::Contract(format!("production manifest schema: {detail}"))
        })?;
        let manifest: ProductionManifest = decode(&manifest_path, manifest_value)?;
        if manifest.schema_version != MANIFEST_SCHEMA || manifest.oracle_version != ORACLE_VERSION {
            return Err(ProductionError::Contract(
                "unsupported production manifest version".into(),
            ));
        }
        let expected_paths: BTreeSet<_> = ARTIFACTS.iter().map(|(path, _)| *path).collect();
        let manifested_paths: BTreeSet<_> = manifest
            .artifacts
            .iter()
            .map(|item| item.path.as_str())
            .collect();
        if expected_paths != manifested_paths || manifest.artifacts.len() != ARTIFACTS.len() {
            return Err(ProductionError::Contract(
                "production manifest artifact set is not exact".into(),
            ));
        }
        let mut values = BTreeMap::new();
        let mut schema_values = BTreeMap::new();
        for artifact in &manifest.artifacts {
            validate_relative(&artifact.path)?;
            let path = root.join(&artifact.path);
            let canonical = path.canonicalize()?;
            if !canonical.starts_with(&root) {
                return Err(ProductionError::Contract(format!(
                    "artifact escapes E2E root: {}",
                    artifact.path
                )));
            }
            let value = read_value(&canonical)?;
            let actual = canonical_json_digest(&value);
            if actual != artifact.sha256 {
                return Err(ProductionError::Contract(format!(
                    "artifact digest mismatch for {}: expected {}, actual {actual}",
                    artifact.path, artifact.sha256
                )));
            }
            if let Some(schema_path) = &artifact.schema {
                validate_relative(schema_path)?;
                let schema = read_value(&root.join(schema_path))?;
                validate_document(&schema, &value).map_err(|detail| {
                    ProductionError::Contract(format!("{} schema: {detail}", artifact.path))
                })?;
            }
            if artifact.path.starts_with("schema/") {
                schema_values.insert(artifact.path.clone(), value.clone());
            }
            values.insert(artifact.path.clone(), value);
        }
        let actual_contract = aggregate_digest(&manifest.artifacts);
        if actual_contract != manifest.contract_digest {
            return Err(ProductionError::Contract(format!(
                "contract digest mismatch: expected {}, actual {actual_contract}",
                manifest.contract_digest
            )));
        }
        let requested_scenario = scenario_path.canonicalize()?;
        if requested_scenario != root.join("scenarios/p0-gateway.json").canonicalize()? {
            return Err(ProductionError::Contract(
                "requested scenario is not the manifested production scenario".into(),
            ));
        }
        let scenario: ProductionScenario =
            decode(scenario_path, values["scenarios/p0-gateway.json"].clone())?;
        let fixture_path = root.join(&scenario.fixture);
        let fixture: ProductionFixture = decode(
            &fixture_path,
            values["fixtures/p0-oracle/production-smoke.json"].clone(),
        )?;
        let default_profile_path = root.join("profiles/gateway-isolated.json");
        let profile_path = profile_path.unwrap_or(&default_profile_path);
        if profile_path.canonicalize()?
            != root.join("profiles/gateway-isolated.json").canonicalize()?
        {
            return Err(ProductionError::Contract(
                "requested profile is not the manifested production profile".into(),
            ));
        }
        let profile: ProductionProfile = decode(
            profile_path,
            values["profiles/gateway-isolated.json"].clone(),
        )?;
        validate_semantics(&scenario, &fixture, &profile)?;
        Ok(Self {
            root,
            candidate: None,
            scenario,
            fixture,
            profile,
            manifest,
            schemas: schema_values,
        })
    }

    pub fn summary(&self) -> ProductionValidationSummary {
        ProductionValidationSummary {
            oracle_version: ORACLE_VERSION,
            contract_digest: self.manifest.contract_digest.clone(),
            case_count: 1,
            completion_policy: self.profile.completion_policy.clone(),
            artifact_digests: self
                .manifest
                .artifacts
                .iter()
                .map(|item| (item.path.clone(), item.sha256.clone()))
                .collect(),
            schema_digests: self.schema_digests(),
        }
    }

    pub fn resolve_sut(&self) -> Result<ResolvedSut, ProductionError> {
        if let Some(candidate) = &self.candidate {
            candidate.resolve_sut()
        } else {
            self.profile.resolve_sut()
        }
    }

    pub(crate) fn schema(&self, path: &str) -> Result<&Value, ProductionError> {
        let path = if self.candidate.is_some() {
            match path {
                "schema/p0-production-launcher.schema.json" => {
                    "schema/current-production-launcher.schema.json"
                }
                "schema/p0-production-collector.schema.json" => {
                    "schema/current-production-collector.schema.json"
                }
                "schema/p0-gateway-result.schema.json" => {
                    "schema/current-gateway-result.schema.json"
                }
                other => other,
            }
        } else {
            path
        };
        self.schemas
            .get(path)
            .ok_or_else(|| ProductionError::Contract(format!("manifested schema missing: {path}")))
    }

    pub fn expected_observation(&self) -> &ExpectedObservation {
        &self.fixture.expected_observation
    }

    pub(crate) fn schema_digests(&self) -> BTreeMap<String, String> {
        self.schemas
            .iter()
            .map(|(path, value)| (path.clone(), canonical_json_digest(value)))
            .collect()
    }

    pub(crate) fn aggregate_port_digest(&self) -> &'static str {
        if self.candidate.is_some() {
            CURRENT_AGGREGATE_PORT_DIGEST
        } else {
            LEGACY_AGGREGATE_PORT_DIGEST
        }
    }

    pub fn seal_manifest(e2e_root: &Path) -> Result<ProductionManifest, ProductionError> {
        let mut artifacts = Vec::new();
        for (relative, schema) in ARTIFACTS {
            let value = read_value(&e2e_root.join(relative))?;
            artifacts.push(ProductionArtifact {
                path: (*relative).into(),
                schema: schema.map(str::to_owned),
                sha256: canonical_json_digest(&value),
            });
        }
        let contract_digest = aggregate_digest(&artifacts);
        Ok(ProductionManifest {
            schema_version: MANIFEST_SCHEMA.into(),
            oracle_version: ORACLE_VERSION.into(),
            contract_digest,
            artifacts,
        })
    }

    pub fn write_sealed_manifest(e2e_root: &Path) -> Result<(), ProductionError> {
        let manifest = Self::seal_manifest(e2e_root)?;
        let path = e2e_root.join("schema").join(MANIFEST_FILE);
        let mut bytes = serde_json::to_vec_pretty(&manifest)?;
        bytes.push(b'\n');
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

fn validate_semantics(
    scenario: &ProductionScenario,
    fixture: &ProductionFixture,
    profile: &ProductionProfile,
) -> Result<(), ProductionError> {
    if scenario.schema_version != SCENARIO_SCHEMA
        || scenario.oracle_version != ORACLE_VERSION
        || fixture.schema_version != FIXTURE_SCHEMA
        || fixture.oracle_version != ORACLE_VERSION
        || scenario.contract_manifest != "schema/p0-production-manifest.json"
        || scenario.fixture != "fixtures/p0-oracle/production-smoke.json"
    {
        return Err(ProductionError::Contract(
            "scenario or fixture version/reference is not frozen".into(),
        ));
    }
    profile.resolve_contract_only()?;
    let forbidden = serde_json::to_value((scenario, fixture, profile))?;
    reject_forbidden_completion_terms(&forbidden, "$")?;
    if fixture.case.grant_access != GrantAccess::Allowed
        || fixture.case.ingress.authorization != AuthorizationFixture::Valid
        || fixture.case.providers.len() != 1
        || fixture.case.providers[0].expected_calls != 1
        || fixture.case.providers[0].protocol != fixture.case.ingress.protocol
    {
        return Err(ProductionError::Contract(
            "production smoke must be one allowed request and one native Provider call".into(),
        ));
    }
    let fixture_value = serde_json::to_value(fixture)?;
    if count_exact_string(&fixture_value, "${RUN_CHALLENGE}") != 4 {
        return Err(ProductionError::Contract(
            "production fixture must bind exactly four fresh challenge projections".into(),
        ));
    }
    let checkpoint_ids = scenario
        .checkpoints
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    let expected_checkpoint_ids = vec![
        "listener_client",
        "native_provider",
        "semantic_observation",
        "privacy_resource",
    ];
    if checkpoint_ids != expected_checkpoint_ids {
        return Err(ProductionError::Contract(
            "production scenario must define the four automatic checkpoints in frozen order".into(),
        ));
    }
    let expected_evidence = vec![
        "launch_production",
        "readiness_exact",
        "listener_client",
        "native_provider",
        "lifecycle_stream",
        "execution_stream",
        "content_stream",
        "otel_stream",
        "privacy_resource",
        "aggregate_port",
    ];
    let expected_checkpoint_evidence = [
        vec!["launch_production", "readiness_exact", "listener_client"],
        vec!["native_provider"],
        vec![
            "lifecycle_stream",
            "execution_stream",
            "content_stream",
            "otel_stream",
            "aggregate_port",
        ],
        vec!["privacy_resource"],
    ];
    if scenario.coverage.len() != 1
        || scenario.coverage[0].id != "normal_hirouted_listener_exact_evidence"
        || scenario.coverage[0]
            .evidence
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            != expected_evidence
        || scenario
            .checkpoints
            .iter()
            .zip(expected_checkpoint_evidence)
            .any(|(checkpoint, expected)| {
                checkpoint
                    .evidence
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    != expected
            })
    {
        return Err(ProductionError::Contract(
            "production coverage/checkpoints must consume every exact evidence check once".into(),
        ));
    }
    let expected_protocol_pairs = [
        "responses_to_responses",
        "responses_to_chat_completions",
        "responses_to_messages",
        "chat_completions_to_responses",
        "chat_completions_to_chat_completions",
        "chat_completions_to_messages",
        "messages_to_responses",
        "messages_to_chat_completions",
        "messages_to_messages",
    ];
    if scenario.matrix_extension.owner != "PROCESS-22009"
        || scenario
            .matrix_extension
            .protocol_pairs
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            != expected_protocol_pairs
    {
        return Err(ProductionError::Contract(
            "production smoke must preserve the bounded PROCESS-22009 matrix extension".into(),
        ));
    }
    let expected_execution_facts = [
        "route_decision",
        "attempt_started",
        "semantic_commit",
        "attempt_finished",
        "request_finished",
        "usage_and_cache",
    ];
    if fixture
        .expected_observation
        .required_execution_facts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        != expected_execution_facts
        || fixture.expected_observation.lifecycle_terminal != "request_finished"
        || fixture.expected_observation.execution_terminal != "request_finished"
        || fixture.expected_observation.required_otel_signal != "span"
        || fixture.expected_observation.freshness_binding != "request_and_response_content"
        || fixture
            .expected_observation
            .content_terminals
            .iter()
            .map(|terminal| (terminal.direction.as_str(), terminal.phase.as_str()))
            .collect::<Vec<_>>()
            != vec![
                ("request_input", "finish"),
                ("response_delivered", "finish"),
            ]
    {
        return Err(ProductionError::Contract(
            "production observation terminals/facts are not the frozen independent contract".into(),
        ));
    }
    Ok(())
}

impl ProductionProfile {
    fn resolve_contract_only(&self) -> Result<(), ProductionError> {
        if self.schema_version != PROFILE_SCHEMA
            || self.oracle_version != ORACLE_VERSION
            || self.name != "gateway-isolated"
            || self.completion_policy != "green_only"
            || self.launch_mode != "production_publication_credentials"
            || self.sut_binary != "${HIROUTE_E2E_SUT_BIN}"
            || self.sut_source_revision != SEALED_SUT_REVISION
            || self.aggregate_port_digest != LEGACY_AGGREGATE_PORT_DIGEST
        {
            return Err(ProductionError::Contract(
                "profile is not the sealed green-only production profile".into(),
            ));
        }
        super::types::validate_revision(&self.sut_source_revision)
    }
}

fn count_exact_string(value: &Value, expected: &str) -> usize {
    match value {
        Value::String(value) => usize::from(value == expected),
        Value::Array(values) => values
            .iter()
            .map(|value| count_exact_string(value, expected))
            .sum(),
        Value::Object(values) => values
            .values()
            .map(|value| count_exact_string(value, expected))
            .sum(),
        _ => 0,
    }
}

fn reject_forbidden_completion_terms(value: &Value, path: &str) -> Result<(), ProductionError> {
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                if matches!(
                    key.as_str(),
                    "expected_red" | "expected_red_code" | "skip" | "skipped"
                ) {
                    return Err(ProductionError::Contract(format!(
                        "forbidden release completion field at {path}/{key}"
                    )));
                }
                reject_forbidden_completion_terms(value, &format!("{path}/{key}"))?;
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                reject_forbidden_completion_terms(value, &format!("{path}/{index}"))?;
            }
        }
        Value::String(value) if matches!(value.as_str(), "expected_red" | "skipped") => {
            return Err(ProductionError::Contract(format!(
                "forbidden release completion value at {path}"
            )));
        }
        _ => {}
    }
    Ok(())
}

fn aggregate_digest(artifacts: &[ProductionArtifact]) -> String {
    let value = serde_json::json!({
        "schema_version": "hiroute.e2e.production-contract-digest/v1",
        "aggregate_port_digest": LEGACY_AGGREGATE_PORT_DIGEST,
        "artifacts": artifacts.iter().map(|item| serde_json::json!({
            "path": item.path,
            "sha256": item.sha256,
        })).collect::<Vec<_>>()
    });
    canonical_json_digest(&value)
}

fn validate_relative(value: &str) -> Result<(), ProductionError> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        Err(ProductionError::Contract(format!(
            "artifact path is not a safe relative path: {value}"
        )))
    } else {
        Ok(())
    }
}

fn read_value(path: &Path) -> Result<Value, ProductionError> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn decode<T: DeserializeOwned>(path: &Path, value: Value) -> Result<T, ProductionError> {
    serde_json::from_value(value).map_err(|error| {
        ProductionError::Contract(format!("cannot decode {}: {error}", path.display()))
    })
}
