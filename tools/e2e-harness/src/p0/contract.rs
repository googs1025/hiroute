use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;

use super::bindings::RuntimeBindings;
use super::canonical::{canonical_json_digest, sha256_hex};
use super::schema::validate_document;
use super::types::*;

const MANIFEST_FILE: &str = "p0-gateway-manifest.json";
const MANIFEST_SCHEMA_FILE: &str = "p0-gateway-manifest.schema.json";

#[derive(Clone, Debug)]
pub struct P0Bundle {
    artifacts: LoadedArtifacts,
    summary: ValidationSummary,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ValidationSummary {
    pub oracle_version: &'static str,
    pub contract_digest: String,
    pub case_count: usize,
    pub assertion_count: usize,
    pub coverage_count: usize,
    pub checkpoint_count: usize,
    pub artifact_digests: BTreeMap<String, String>,
}

impl P0Bundle {
    pub fn load(scenario_path: &Path, schema_dir: &Path) -> Result<Self, BundleError> {
        let manifest_path = schema_dir.join(MANIFEST_FILE);
        let manifest_schema_path = schema_dir.join(MANIFEST_SCHEMA_FILE);
        let manifest_value = read_value(&manifest_path)?;
        let manifest_schema = read_value(&manifest_schema_path)?;
        validate_document(&manifest_schema, &manifest_value).map_err(|detail| {
            BundleError::SchemaValidation {
                artifact: manifest_path.clone(),
                detail,
            }
        })?;
        let manifest: ManifestDocument = from_value(&manifest_path, manifest_value)?;
        if manifest.schema_version != MANIFEST_SCHEMA || manifest.oracle_version != ORACLE_VERSION {
            return Err(BundleError::Version("manifest"));
        }

        let e2e_root = schema_dir
            .parent()
            .ok_or(BundleError::SchemaDirectory)?
            .canonicalize()
            .map_err(|source| BundleError::Io {
                path: schema_dir.to_path_buf(),
                source,
            })?;
        let mut artifact_paths = BTreeMap::new();
        let mut artifact_digests = BTreeMap::new();
        for artifact in &manifest.artifacts {
            validate_relative_path(&artifact.path)?;
            let path = e2e_root.join(&artifact.path);
            let canonical = path.canonicalize().map_err(|source| BundleError::Io {
                path: path.clone(),
                source,
            })?;
            if !canonical.starts_with(&e2e_root) {
                return Err(BundleError::PathEscape(artifact.path.clone()));
            }
            let bytes = std::fs::read(&canonical).map_err(|source| BundleError::Io {
                path: canonical.clone(),
                source,
            })?;
            let instance: Value =
                serde_json::from_slice(&bytes).map_err(|source| BundleError::Json {
                    path: canonical.clone(),
                    source,
                })?;
            let actual = canonical_json_digest(&instance);
            if actual != artifact.sha256 {
                return Err(BundleError::ArtifactDigest {
                    path: artifact.path.clone(),
                    expected: artifact.sha256.clone(),
                    actual,
                });
            }
            if artifact_paths
                .insert(artifact.kind, canonical.clone())
                .is_some()
            {
                return Err(BundleError::DuplicateArtifactKind(artifact.kind));
            }
            artifact_digests.insert(artifact.path.clone(), artifact.sha256.clone());
            if let Some(schema_path) = &artifact.schema {
                validate_relative_path(schema_path)?;
                let schema = read_value(&e2e_root.join(schema_path))?;
                validate_document(&schema, &instance).map_err(|detail| {
                    BundleError::SchemaValidation {
                        artifact: canonical.clone(),
                        detail,
                    }
                })?;
            }
        }
        let computed_contract_digest = contract_digest(&manifest.artifacts);
        if computed_contract_digest != manifest.contract_digest {
            return Err(BundleError::ContractDigest {
                expected: manifest.contract_digest,
                actual: computed_contract_digest,
            });
        }

        let checked_scenario = artifact_paths
            .get(&ArtifactKind::Scenario)
            .ok_or(BundleError::MissingArtifactKind(ArtifactKind::Scenario))?;
        let requested_scenario =
            scenario_path
                .canonicalize()
                .map_err(|source| BundleError::Io {
                    path: scenario_path.to_path_buf(),
                    source,
                })?;
        if requested_scenario != *checked_scenario {
            return Err(BundleError::ScenarioNotManifested);
        }
        let scenario: ScenarioDocument = read_json(checked_scenario)?;
        let corpus: CorpusDocument =
            read_json(required_path(&artifact_paths, ArtifactKind::Corpus)?)?;
        let golden: GoldenDocument =
            read_json(required_path(&artifact_paths, ArtifactKind::Golden)?)?;
        let semantic_ledger: SemanticLedgerDocument = read_json(required_path(
            &artifact_paths,
            ArtifactKind::SemanticLedger,
        )?)?;
        validate_frozen_ledger(&golden, &semantic_ledger)?;
        validate_observation_goldens(&golden, &artifact_paths)?;
        let summary = validate_documents(
            &scenario,
            &corpus,
            &golden,
            &manifest.contract_digest,
            artifact_digests,
        )?;
        Ok(Self {
            artifacts: LoadedArtifacts {
                manifest,
                scenario,
                corpus,
                golden,
                artifact_paths,
            },
            summary,
        })
    }

    pub fn validate_documents(
        scenario: &ScenarioDocument,
        corpus: &CorpusDocument,
        golden: &GoldenDocument,
        contract_digest: &str,
    ) -> Result<ValidationSummary, BundleError> {
        validate_documents(scenario, corpus, golden, contract_digest, BTreeMap::new())
    }

    pub fn summary(&self) -> &ValidationSummary {
        &self.summary
    }

    pub fn load_profile(&self, path: &Path) -> Result<P0Profile, BundleError> {
        let requested = path.canonicalize().map_err(|source| BundleError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let manifested = required_path(&self.artifacts.artifact_paths, ArtifactKind::Profile)?;
        if requested != manifested {
            return Err(BundleError::ProfileNotManifested);
        }
        read_json(manifested)
    }

    pub(crate) fn artifacts(&self) -> &LoadedArtifacts {
        &self.artifacts
    }
}

fn validate_frozen_ledger(
    golden: &GoldenDocument,
    ledger: &SemanticLedgerDocument,
) -> Result<(), BundleError> {
    if ledger.schema_version != "hiroute.e2e.semantic-ledger/v1"
        || ledger.oracle_version != ORACLE_VERSION
    {
        return Err(BundleError::Version("semantic ledger"));
    }
    let mut expected = Vec::new();
    for assertion in &golden.assertions {
        if assertion.source == EvidenceSource::CanonicalLedger {
            let events = assertion
                .expected
                .get("events")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    BundleError::Semantic("canonical ledger events are invalid".into())
                })?;
            expected.extend(events.iter().cloned());
        }
    }
    if expected != ledger.events {
        return Err(BundleError::Semantic(
            "standalone semantic ledger disagrees with exact golden assertions".into(),
        ));
    }
    Ok(())
}

fn validate_documents(
    scenario: &ScenarioDocument,
    corpus: &CorpusDocument,
    golden: &GoldenDocument,
    contract_digest: &str,
    artifact_digests: BTreeMap<String, String>,
) -> Result<ValidationSummary, BundleError> {
    if scenario.schema_version != SCENARIO_SCHEMA
        || corpus.schema_version != CORPUS_SCHEMA
        || golden.schema_version != GOLDEN_SCHEMA
        || scenario.oracle_version != ORACLE_VERSION
        || corpus.oracle_version != ORACLE_VERSION
        || golden.oracle_version != ORACLE_VERSION
    {
        return Err(BundleError::Version("scenario/corpus/golden"));
    }
    if scenario.artifact_manifest != MANIFEST_FILE || scenario.name.trim().is_empty() {
        return Err(BundleError::Semantic(
            "scenario must name the frozen manifest and have a name".into(),
        ));
    }
    if !contract_digest.starts_with("sha256:") {
        return Err(BundleError::Semantic(
            "contract digest must be sha256-prefixed".into(),
        ));
    }

    let case_ids = unique(
        scenario.cases.iter().map(|case| case.id.as_str()),
        "scenario case",
    )?;
    let corpus_ids = unique(
        corpus.cases.iter().map(|case| case.id.as_str()),
        "corpus case",
    )?;
    for case in &scenario.cases {
        if case.id != case.corpus_case_id || !corpus_ids.contains(case.corpus_case_id.as_str()) {
            return Err(BundleError::Semantic(format!(
                "scenario case {} must bind the same corpus case id",
                case.id
            )));
        }
        if case.product_invariant.trim().is_empty()
            || case.expected_red_code != NOT_IMPLEMENTED_CODE
        {
            return Err(BundleError::Semantic(format!(
                "scenario case {} has an invalid expected-red contract",
                case.id
            )));
        }
    }
    if case_ids != corpus_ids {
        return Err(BundleError::Semantic(
            "scenario and corpus case sets must be identical".into(),
        ));
    }
    validate_corpus(corpus)?;

    let assertion_ids = unique(
        golden.assertions.iter().map(|item| item.id.as_str()),
        "golden assertion",
    )?;
    let mut assertions = BTreeMap::new();
    let mut assertions_by_source = BTreeMap::new();
    for assertion in &golden.assertions {
        if !case_ids.contains(assertion.case_id.as_str()) {
            return Err(BundleError::Semantic(format!(
                "assertion {} names unknown case {}",
                assertion.id, assertion.case_id
            )));
        }
        validate_evidence_shape(assertion.source, &assertion.expected)?;
        reject_boolean_coverage_keys(&assertion.expected, "$expected")?;
        if assertions_by_source
            .insert((assertion.case_id.as_str(), assertion.source), assertion)
            .is_some()
        {
            return Err(BundleError::Semantic(format!(
                "case {} has duplicate {} evidence",
                assertion.case_id,
                assertion.source.as_str()
            )));
        }
        assertions.insert(assertion.id.as_str(), assertion);
    }
    let required_sources = [
        EvidenceSource::NativeRequest,
        EvidenceSource::CanonicalLedger,
        EvidenceSource::ClientOutput,
        EvidenceSource::ExecutionFact,
        EvidenceSource::ConversationContent,
        EvidenceSource::Otel,
        EvidenceSource::Resource,
    ];
    for case in &corpus.cases {
        for source in required_sources {
            if !assertions_by_source.contains_key(&(case.id.as_str(), source)) {
                return Err(BundleError::MissingEvidence {
                    case_id: case.id.clone(),
                    evidence_source: source,
                });
            }
        }
        let mut expected_entries = Vec::new();
        for provider in &case.providers {
            for _ in 0..provider.expected_calls {
                expected_entries.push(super::provider::expected_native_value(
                    provider,
                    expected_entries.len() + 1,
                ));
            }
        }
        let assertion = assertions_by_source[&(case.id.as_str(), EvidenceSource::NativeRequest)];
        if assertion.expected != serde_json::json!({"entries": expected_entries}) {
            return Err(BundleError::NativeGolden(case.id.clone()));
        }
        if case
            .providers
            .iter()
            .any(|provider| provider.expected_calls > 0)
        {
            for source in [
                EvidenceSource::CanonicalLedger,
                EvidenceSource::ClientOutput,
                EvidenceSource::ConversationContent,
            ] {
                let expected = &assertions_by_source[&(case.id.as_str(), source)].expected;
                if !contains_runtime_challenge(expected) {
                    return Err(BundleError::ChallengeLink {
                        case_id: case.id.clone(),
                        evidence_source: source,
                    });
                }
            }
        }
    }

    let coverage_ids = unique(
        scenario.coverage.iter().map(|item| item.bit.as_str()),
        "coverage bit",
    )?;
    let mut referenced = BTreeSet::new();
    for coverage in &scenario.coverage {
        let mut sources = BTreeSet::new();
        for id in &coverage.assertion_ids {
            let assertion =
                assertions
                    .get(id.as_str())
                    .ok_or_else(|| BundleError::MissingAssertion {
                        container: coverage.bit.clone(),
                        assertion_id: id.clone(),
                    })?;
            sources.insert(assertion.source);
            referenced.insert(id.as_str());
        }
        let base = [
            EvidenceSource::NativeRequest,
            EvidenceSource::CanonicalLedger,
            EvidenceSource::ClientOutput,
        ];
        if !base.iter().all(|source| sources.contains(source))
            || !sources.iter().any(|source| source.is_supplemental())
        {
            return Err(BundleError::CoverageTrace(coverage.bit.clone()));
        }
    }
    if referenced != assertion_ids {
        let missing: Vec<_> = assertion_ids.difference(&referenced).copied().collect();
        return Err(BundleError::Semantic(format!(
            "golden assertions are not coverage-linked: {}",
            missing.join(",")
        )));
    }

    let expected_checkpoints = BTreeSet::from([
        CheckpointId::ListenerClient,
        CheckpointId::NativeProvider,
        CheckpointId::SemanticObservation,
        CheckpointId::PrivacyResource,
    ]);
    let checkpoint_ids: BTreeSet<_> = scenario.checkpoints.iter().map(|item| item.id).collect();
    if checkpoint_ids != expected_checkpoints || scenario.checkpoints.len() != 4 {
        return Err(BundleError::Semantic(
            "scenario must define exactly the four frozen checkpoints".into(),
        ));
    }
    let mut checkpoint_assertions = BTreeSet::new();
    for checkpoint in &scenario.checkpoints {
        for id in &checkpoint.assertion_ids {
            if !assertions.contains_key(id.as_str()) {
                return Err(BundleError::MissingAssertion {
                    container: format!("checkpoint:{:?}", checkpoint.id),
                    assertion_id: id.clone(),
                });
            }
            checkpoint_assertions.insert(id.as_str());
        }
    }
    if checkpoint_assertions != assertion_ids {
        return Err(BundleError::Semantic(
            "every assertion must be assigned to an automatic checkpoint".into(),
        ));
    }
    Ok(ValidationSummary {
        oracle_version: ORACLE_VERSION,
        contract_digest: contract_digest.to_owned(),
        case_count: scenario.cases.len(),
        assertion_count: golden.assertions.len(),
        coverage_count: coverage_ids.len(),
        checkpoint_count: scenario.checkpoints.len(),
        artifact_digests,
    })
}

fn validate_corpus(corpus: &CorpusDocument) -> Result<(), BundleError> {
    unique(
        corpus.privacy_markers.iter().map(String::as_str),
        "privacy marker",
    )?;
    if corpus.privacy_markers.iter().any(|marker| marker.len() < 5) {
        return Err(BundleError::Semantic(
            "privacy markers must be at least five bytes".into(),
        ));
    }
    let mut provider_ids = BTreeSet::new();
    for case in &corpus.cases {
        if case.providers.is_empty()
            || (case.grant_access == GrantAccess::Allowed
                && !case
                    .providers
                    .iter()
                    .any(|provider| provider.expected_calls > 0))
            || (case.grant_access == GrantAccess::Denied
                && case
                    .providers
                    .iter()
                    .any(|provider| provider.expected_calls != 0))
        {
            return Err(BundleError::Semantic(format!(
                "corpus case {} does not bind grant access to explicit provider call counts",
                case.id
            )));
        }
        if case.ingress.path != case.ingress.protocol.ingress_path()
            || !case.ingress.body.is_object()
        {
            return Err(BundleError::Semantic(format!(
                "corpus case {} has a non-native ingress fixture",
                case.id
            )));
        }
        if case
            .ingress
            .body
            .get("model")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err(BundleError::Semantic(format!(
                "corpus case {} has no exact model alias",
                case.id
            )));
        }
        for provider in &case.providers {
            if provider.expected_calls > 4 {
                return Err(BundleError::Semantic(format!(
                    "provider {} has an unbounded expected call count",
                    provider.id
                )));
            }
            if !provider_ids.insert(provider.id.as_str()) {
                return Err(BundleError::Duplicate("provider id"));
            }
            if !matches!(
                provider.response.status,
                200 | 400 | 429 | 500 | 502 | 503 | 504
            ) {
                return Err(BundleError::Semantic(format!(
                    "provider {} has an unsupported scripted status",
                    provider.id
                )));
            }
            if provider.expected_request.path != provider.protocol.provider_path()
                || !provider.expected_request.body.is_object()
                || provider
                    .expected_request
                    .body
                    .get("model")
                    .and_then(Value::as_str)
                    .is_none()
            {
                return Err(BundleError::Semantic(format!(
                    "provider {} has an invalid native request golden",
                    provider.id
                )));
            }
            match &provider.response.body {
                ProviderBody::Json { .. }
                    if provider.response.content_type != "application/json" =>
                {
                    return Err(BundleError::Semantic(format!(
                        "provider {} JSON response has the wrong content type",
                        provider.id
                    )));
                }
                ProviderBody::Sse { events }
                    if provider.response.content_type != "text/event-stream"
                        || events.is_empty() =>
                {
                    return Err(BundleError::Semantic(format!(
                        "provider {} SSE response is incomplete",
                        provider.id
                    )));
                }
                _ => {}
            }
            let response_value = serde_json::to_value(&provider.response.body)
                .expect("provider response fixtures serialize");
            if !contains_provider_challenge(&response_value) {
                return Err(BundleError::ProviderChallenge(provider.id.clone()));
            }
        }
    }
    Ok(())
}

fn contains_provider_challenge(value: &Value) -> bool {
    contains_exact_string(value, "${RUN_CHALLENGE}")
        || contains_exact_string(value, "${RUN_CHALLENGE_JSON}")
}

fn contains_runtime_challenge(value: &Value) -> bool {
    [
        "${RUN_CHALLENGE}",
        "${RUN_CHALLENGE_JSON}",
        "${RUN_CHALLENGE_SHA256}",
        "${RUN_CHALLENGE_JSON_SHA256}",
    ]
    .iter()
    .any(|placeholder| contains_exact_string(value, placeholder))
}

fn contains_exact_string(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(value) => value == expected,
        Value::Array(values) => values
            .iter()
            .any(|value| contains_exact_string(value, expected)),
        Value::Object(values) => values
            .values()
            .any(|value| contains_exact_string(value, expected)),
        _ => false,
    }
}

fn validate_observation_goldens(
    golden: &GoldenDocument,
    paths: &BTreeMap<ArtifactKind, PathBuf>,
) -> Result<(), BundleError> {
    let bindings = RuntimeBindings::validation(
        golden
            .assertions
            .iter()
            .map(|assertion| assertion.case_id.as_str()),
    );
    for (source, kind) in [
        (
            EvidenceSource::ExecutionFact,
            ArtifactKind::ExecutionFactSchema,
        ),
        (
            EvidenceSource::ConversationContent,
            ArtifactKind::ConversationContentSchema,
        ),
    ] {
        let schema_path = required_path(paths, kind)?;
        let schema = read_value(schema_path)?;
        for assertion in golden
            .assertions
            .iter()
            .filter(|assertion| assertion.source == source)
        {
            let records = assertion.expected["records"].as_array().ok_or_else(|| {
                BundleError::Semantic(format!("{} records are not an array", assertion.id))
            })?;
            for record in records {
                let materialized = bindings.materialize(&assertion.case_id, record);
                validate_document(&schema, &materialized).map_err(|detail| {
                    BundleError::SchemaValidation {
                        artifact: schema_path.to_path_buf(),
                        detail: format!("golden {}: {detail}", assertion.id),
                    }
                })?;
            }
            validate_channel_snapshot(assertion, source, records)?;
        }
    }
    Ok(())
}

fn validate_channel_snapshot(
    assertion: &GoldenAssertion,
    source: EvidenceSource,
    records: &[Value],
) -> Result<(), BundleError> {
    if records
        .iter()
        .any(|record| record["event_id"] != record["idempotency_key"])
    {
        return Err(BundleError::Semantic(format!(
            "{} does not bind idempotency to its stable event ID",
            assertion.id
        )));
    }
    if assertion.expected["schema_version"] != CHANNEL_SNAPSHOT_SCHEMA
        || assertion.expected["channel"] != source.as_str()
    {
        return Err(BundleError::Semantic(format!(
            "{} is not a typed channel snapshot",
            assertion.id
        )));
    }
    let terminal = &assertion.expected["terminal"];
    if terminal["state"] != "acked"
        || !terminal["ack"].is_object()
        || !terminal["nack"].is_null()
        || !terminal["gap"].is_null()
    {
        return Err(BundleError::Semantic(format!(
            "{} lacks exact ACK/NACK/gap terminal evidence",
            assertion.id
        )));
    }
    if source == EvidenceSource::ConversationContent && !records.is_empty() {
        let phases: Vec<_> = records
            .iter()
            .map(|record| record["phase"].as_str().unwrap_or_default())
            .collect();
        if phases != ["begin", "append", "append", "finish"]
            || records[1]["payload"]["acceptance"] != "canonical_request"
            || records[2]["payload"]["acceptance"] != "downstream_transport"
            || !records[2]["payload"]["frame_id"].is_string()
        {
            return Err(BundleError::Semantic(format!(
                "{} does not freeze begin/append/finish transport acceptance",
                assertion.id
            )));
        }
    }
    Ok(())
}

fn validate_evidence_shape(source: EvidenceSource, value: &Value) -> Result<(), BundleError> {
    let object = value.as_object().ok_or_else(|| {
        BundleError::Semantic(format!("{} evidence must be an object", source.as_str()))
    })?;
    let required = match source {
        EvidenceSource::NativeRequest => "entries",
        EvidenceSource::CanonicalLedger => "events",
        EvidenceSource::ClientOutput => "status",
        EvidenceSource::ExecutionFact
        | EvidenceSource::ConversationContent
        | EvidenceSource::Otel => "records",
        EvidenceSource::Resource => "facts",
    };
    if !object.contains_key(required) {
        return Err(BundleError::Semantic(format!(
            "{} evidence is missing {required}",
            source.as_str()
        )));
    }
    Ok(())
}

fn reject_boolean_coverage_keys(value: &Value, path: &str) -> Result<(), BundleError> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(key.as_str(), "pass" | "passed" | "covered" | "coverage") {
                    return Err(BundleError::Semantic(format!(
                        "hard-coded coverage key is forbidden at {path}/{key}"
                    )));
                }
                reject_boolean_coverage_keys(value, &format!("{path}/{key}"))?;
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                reject_boolean_coverage_keys(value, &format!("{path}/{index}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn unique<'a>(
    values: impl Iterator<Item = &'a str>,
    label: &'static str,
) -> Result<BTreeSet<&'a str>, BundleError> {
    let mut result = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() || !result.insert(value) {
            return Err(BundleError::Duplicate(label));
        }
    }
    if result.is_empty() {
        return Err(BundleError::Semantic(format!(
            "{label} set must not be empty"
        )));
    }
    Ok(result)
}

fn contract_digest(artifacts: &[ManifestArtifact]) -> String {
    let mut entries: Vec<_> = artifacts
        .iter()
        .map(|item| (item.path.as_str(), item.sha256.as_str()))
        .collect();
    entries.sort_unstable();
    let mut bytes = Vec::new();
    for (path, digest) in entries {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(digest.as_bytes());
        bytes.push(b'\n');
    }
    sha256_hex(&bytes)
}

fn validate_relative_path(path: &str) -> Result<(), BundleError> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(BundleError::PathEscape(path.display().to_string()));
    }
    Ok(())
}

fn required_path(
    paths: &BTreeMap<ArtifactKind, PathBuf>,
    kind: ArtifactKind,
) -> Result<&Path, BundleError> {
    paths
        .get(&kind)
        .map(PathBuf::as_path)
        .ok_or(BundleError::MissingArtifactKind(kind))
}

fn read_value(path: &Path) -> Result<Value, BundleError> {
    let bytes = std::fs::read(path).map_err(|source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| BundleError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn from_value<T: DeserializeOwned>(path: &Path, value: Value) -> Result<T, BundleError> {
    serde_json::from_value(value).map_err(|source| BundleError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, BundleError> {
    from_value(path, read_value(path)?)
}

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("schema validation failed for {artifact}: {detail}")]
    SchemaValidation { artifact: PathBuf, detail: String },
    #[error("schema directory has no E2E parent")]
    SchemaDirectory,
    #[error("artifact path escapes the E2E root: {0}")]
    PathEscape(String),
    #[error("artifact digest mismatch for {path}: expected {expected}, actual {actual}")]
    ArtifactDigest {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("contract digest mismatch: expected {expected}, actual {actual}")]
    ContractDigest { expected: String, actual: String },
    #[error("duplicate manifest artifact kind: {0:?}")]
    DuplicateArtifactKind(ArtifactKind),
    #[error("manifest is missing artifact kind {0:?}")]
    MissingArtifactKind(ArtifactKind),
    #[error("requested scenario is not the exact manifested scenario")]
    ScenarioNotManifested,
    #[error("requested profile is not the exact manifested profile")]
    ProfileNotManifested,
    #[error("unsupported {0} version")]
    Version(&'static str),
    #[error("duplicate or empty {0}")]
    Duplicate(&'static str),
    #[error("{0}")]
    Semantic(String),
    #[error("{container} references missing assertion {assertion_id}")]
    MissingAssertion {
        container: String,
        assertion_id: String,
    },
    #[error("case {case_id} is missing exact {evidence_source:?} evidence")]
    MissingEvidence {
        case_id: String,
        evidence_source: EvidenceSource,
    },
    #[error("case {0} native request golden disagrees with its provider corpus")]
    NativeGolden(String),
    #[error("provider {0} response has no exact runtime challenge placeholder")]
    ProviderChallenge(String),
    #[error("case {case_id} {evidence_source:?} does not propagate the runtime challenge")]
    ChallengeLink {
        case_id: String,
        evidence_source: EvidenceSource,
    },
    #[error("coverage bit {0} lacks native/canonical/client/supplemental exact evidence")]
    CoverageTrace(String),
}
