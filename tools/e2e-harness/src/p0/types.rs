use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ORACLE_VERSION: &str = "p0-gateway-oracle-v1";
pub const SCENARIO_SCHEMA: &str = "hiroute.e2e.scenario/v1";
pub const CORPUS_SCHEMA: &str = "hiroute.e2e.corpus/v1";
pub const GOLDEN_SCHEMA: &str = "hiroute.e2e.golden/v1";
pub const MANIFEST_SCHEMA: &str = "hiroute.e2e.manifest/v1";
pub const PROFILE_SCHEMA: &str = "hiroute.e2e.profile/v1";
pub const RESULT_SCHEMA: &str = "hiroute.e2e.oracle-result/v1";
pub const EVIDENCE_MANIFEST_SCHEMA: &str = "hiroute.e2e.materialized-evidence/v1";
pub const FIXTURE_SCHEMA: &str = "hiroute.gateway.e2e-fixture/v2";
pub const PUBLICATION_SCHEMA: &str = "hiroute.gateway.publication-snapshot/v1";
pub const EXECUTION_FACT_SCHEMA: &str = "hiroute.observation.execution-fact-envelope/v1";
pub const CONVERSATION_CONTENT_SCHEMA: &str =
    "hiroute.observation.conversation-content-envelope/v1";
pub const CHANNEL_SNAPSHOT_SCHEMA: &str = "hiroute.e2e.observation-channel-snapshot/v1";
pub const TERMINAL_RECEIPT_SCHEMA: &str = "hiroute.e2e.observation-terminal/v1";
pub const READY_SCHEMA: &str = "hiroute.gateway.ready/v1";
pub const NOT_IMPLEMENTED_CODE: &str = "GATEWAY_EXECUTION_NOT_IMPLEMENTED";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDocument {
    pub schema_version: String,
    pub oracle_version: String,
    pub name: String,
    pub artifact_manifest: String,
    pub cases: Vec<ScenarioCase>,
    pub coverage: Vec<CoverageRequirement>,
    pub checkpoints: Vec<CheckpointRequirement>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioCase {
    pub id: String,
    pub corpus_case_id: String,
    pub product_invariant: String,
    pub expected_red_code: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageRequirement {
    pub bit: String,
    pub assertion_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRequirement {
    pub id: CheckpointId,
    pub assertion_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointId {
    ListenerClient,
    NativeProvider,
    SemanticObservation,
    PrivacyResource,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusDocument {
    pub schema_version: String,
    pub oracle_version: String,
    pub privacy_markers: Vec<String>,
    pub cases: Vec<CorpusCase>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusCase {
    pub id: String,
    pub grant_access: GrantAccess,
    pub ingress: IngressFixture,
    pub providers: Vec<ProviderScript>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantAccess {
    Allowed,
    Denied,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IngressFixture {
    pub protocol: Protocol,
    pub path: String,
    pub authorization: AuthorizationFixture,
    pub body: Value,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationFixture {
    Missing,
    Invalid,
    Valid,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Responses,
    ChatCompletions,
    Messages,
}

impl Protocol {
    pub fn ingress_path(self) -> &'static str {
        match self {
            Self::Responses => "/v1/responses",
            Self::ChatCompletions => "/v1/chat/completions",
            Self::Messages => "/v1/messages",
        }
    }

    pub fn provider_path(self) -> &'static str {
        self.ingress_path()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
            Self::Messages => "messages",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderScript {
    pub id: String,
    pub protocol: Protocol,
    pub expected_calls: usize,
    pub expected_request: NativeRequestExpectation,
    pub response: ProviderResponse,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRequestExpectation {
    pub path: String,
    pub body: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderResponse {
    pub status: u16,
    pub content_type: String,
    pub body: ProviderBody,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderBody {
    Json { value: Value },
    Sse { events: Vec<SseFixtureEvent> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SseFixtureEvent {
    pub event: Option<String>,
    pub data: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenDocument {
    pub schema_version: String,
    pub oracle_version: String,
    pub assertions: Vec<GoldenAssertion>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticLedgerDocument {
    pub schema_version: String,
    pub oracle_version: String,
    pub events: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenAssertion {
    pub id: String,
    pub case_id: String,
    pub source: EvidenceSource,
    pub expected: Value,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    NativeRequest,
    CanonicalLedger,
    ClientOutput,
    ExecutionFact,
    ConversationContent,
    Otel,
    Resource,
}

impl EvidenceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NativeRequest => "native_request",
            Self::CanonicalLedger => "canonical_ledger",
            Self::ClientOutput => "client_output",
            Self::ExecutionFact => "execution_fact",
            Self::ConversationContent => "conversation_content",
            Self::Otel => "otel",
            Self::Resource => "resource",
        }
    }

    pub fn is_supplemental(self) -> bool {
        matches!(
            self,
            Self::ExecutionFact | Self::ConversationContent | Self::Otel | Self::Resource
        )
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestDocument {
    pub schema_version: String,
    pub oracle_version: String,
    pub contract_digest: String,
    pub artifacts: Vec<ManifestArtifact>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestArtifact {
    pub kind: ArtifactKind,
    pub path: String,
    pub schema: Option<String>,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Scenario,
    Corpus,
    Golden,
    SemanticLedger,
    Profile,
    ScenarioSchema,
    CorpusSchema,
    GoldenSchema,
    LedgerSchema,
    ResultSchema,
    ProfileSchema,
    FixtureSchema,
    PublicationSchema,
    ExecutionFactSchema,
    ConversationContentSchema,
    TerminalReceiptSchema,
    ManifestSchema,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct P0Profile {
    pub schema_version: String,
    pub oracle_version: String,
    pub name: String,
    pub sut_binary: String,
    pub sut_sha256: String,
    pub review_head: String,
}

impl P0Profile {
    pub fn read(path: &Path) -> Result<Self, std::io::Error> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(std::io::Error::other)
    }

    pub fn resolve_sut(&self) -> Result<ReviewedSut, String> {
        if self.schema_version != PROFILE_SCHEMA || self.oracle_version != ORACLE_VERSION {
            return Err("unsupported P0 profile version".into());
        }
        if self.sut_binary != "${HIROUTE_E2E_SUT_BIN}" {
            return Err("P0 profile must use ${HIROUTE_E2E_SUT_BIN}".into());
        }
        if self.sut_sha256 != "${HIROUTE_E2E_SUT_SHA256}"
            || self.review_head != "${HIROUTE_E2E_REVIEW_HEAD}"
        {
            return Err("P0 profile must use trusted provenance environment references".into());
        }
        let path = std::env::var_os("HIROUTE_E2E_SUT_BIN")
            .map(PathBuf::from)
            .ok_or_else(|| "HIROUTE_E2E_SUT_BIN is not set".to_owned())?;
        if !path.is_file() {
            return Err("HIROUTE_E2E_SUT_BIN is not a file".into());
        }
        let path = path
            .canonicalize()
            .map_err(|error| format!("cannot resolve HIROUTE_E2E_SUT_BIN: {error}"))?;
        let sha256 = std::env::var("HIROUTE_E2E_SUT_SHA256")
            .map_err(|_| "HIROUTE_E2E_SUT_SHA256 is not set".to_owned())?;
        let review_head = std::env::var("HIROUTE_E2E_REVIEW_HEAD")
            .map_err(|_| "HIROUTE_E2E_REVIEW_HEAD is not set".to_owned())?;
        ReviewedSut::new(path, sha256, review_head)
    }
}

#[derive(Clone, Debug)]
pub struct ReviewedSut {
    pub canonical_path: PathBuf,
    pub executable_sha256: String,
    pub review_head: String,
}

impl ReviewedSut {
    pub fn new(
        path: PathBuf,
        executable_sha256: String,
        review_head: String,
    ) -> Result<Self, String> {
        let canonical_path = path
            .canonicalize()
            .map_err(|error| format!("cannot canonicalize reviewed SUT: {error}"))?;
        if !canonical_path.is_file() {
            return Err("reviewed SUT is not a file".into());
        }
        if !is_sha256(&executable_sha256) {
            return Err("reviewed SUT digest is not sha256-prefixed lowercase hex".into());
        }
        if review_head.len() != 40
            || !review_head
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("review head must be 40 lowercase hexadecimal characters".into());
        }
        Ok(Self {
            canonical_path,
            executable_sha256,
            review_head,
        })
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct P0RunReport {
    pub schema_version: String,
    pub oracle_version: String,
    pub contract_digest: String,
    pub evidence_digest: String,
    pub result_payload_digest: String,
    pub status: OracleStatus,
    pub topology: TopologyEvidence,
    pub evidence: MaterializedEvidenceManifest,
    pub assertions: Vec<AssertionOutcome>,
    pub coverage: Vec<CoverageOutcome>,
    pub checkpoints: Vec<CheckpointOutcome>,
    pub expected_red: Vec<ProductExpectedRed>,
    pub summary: RunSummary,
}

#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct VerifiedP0Run(pub(crate) P0RunReport);

impl std::ops::Deref for VerifiedP0Run {
    type Target = P0RunReport;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl VerifiedP0Run {
    pub fn as_report(&self) -> &P0RunReport {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializedEvidenceManifest {
    pub schema_version: String,
    pub binding_nonce: String,
    pub producer_nonce: String,
    pub assertions: Vec<MaterializedAssertionEvidence>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializedAssertionEvidence {
    pub assertion_id: String,
    pub materialized_expected_digest: String,
    pub actual: Option<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleStatus {
    Green,
    ExpectedRed,
    Red,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TopologyEvidence {
    pub sut_boundary: String,
    pub executable_sha256: String,
    pub review_head: String,
    pub canonical_path_verified: bool,
    pub child_pid: u32,
    pub readiness_nonce_verified: bool,
    pub publication_digest: String,
    pub listener_allocation: String,
    pub provider_listener_count: usize,
    pub private_root_mode: String,
    pub secret_bearing_file_mode: String,
    pub readiness_file_mode: String,
    pub process_artifacts_mode: String,
    pub evidence_artifacts_mode: String,
    pub path_identity_preserved: bool,
    pub runtime_privacy_revalidated: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionOutcome {
    pub id: String,
    pub case_id: String,
    pub source: EvidenceSource,
    pub status: AssertionStatus,
    pub expected_digest: String,
    pub actual_digest: Option<String>,
    pub mismatch_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionStatus {
    Passed,
    Failed,
    Missing,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageOutcome {
    pub bit: String,
    pub status: DerivedStatus,
    pub assertion_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointOutcome {
    pub id: CheckpointId,
    pub status: DerivedStatus,
    pub assertion_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedStatus {
    Passed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductExpectedRed {
    pub case_id: String,
    pub invariant_id: String,
    pub code: String,
    pub failed_assertion_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunSummary {
    pub assertion_passed: usize,
    pub assertion_failed: usize,
    pub assertion_missing: usize,
    pub coverage_passed: usize,
    pub coverage_required: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationTerminalReceipt {
    pub schema_version: String,
    pub run_nonce: String,
    pub producer: ObservationProducer,
    pub flush_state: String,
    pub channels: Vec<ObservationChannelReceipt>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyDocument {
    pub schema_version: String,
    pub run_nonce: String,
    pub process_id: u32,
    pub listen_address: String,
    pub executable_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationProducer {
    pub component: String,
    pub instance_id: String,
    pub process_id: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationChannelReceipt {
    pub channel: ObservationChannel,
    pub file_size: u64,
    pub file_sha256: String,
    pub terminals: Vec<ObservationRequestTerminal>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationRequestTerminal {
    pub request_id: String,
    pub stream_id: Option<String>,
    pub state: String,
    pub ack: Value,
    pub nack: Value,
    pub gap: Value,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationChannel {
    CanonicalLedger,
    ExecutionFact,
    ConversationContent,
    Otel,
}

impl ObservationChannel {
    pub fn evidence_source(self) -> EvidenceSource {
        match self {
            Self::CanonicalLedger => EvidenceSource::CanonicalLedger,
            Self::ExecutionFact => EvidenceSource::ExecutionFact,
            Self::ConversationContent => EvidenceSource::ConversationContent,
            Self::Otel => EvidenceSource::Otel,
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Self::CanonicalLedger => "semantic-ledger.jsonl",
            Self::ExecutionFact => "execution-facts.jsonl",
            Self::ConversationContent => "conversation-content.jsonl",
            Self::Otel => "otel.jsonl",
        }
    }
}

#[derive(Clone, Debug)]
pub struct LoadedArtifacts {
    pub manifest: ManifestDocument,
    pub scenario: ScenarioDocument,
    pub corpus: CorpusDocument,
    pub golden: GoldenDocument,
    pub artifact_paths: BTreeMap<ArtifactKind, PathBuf>,
}
