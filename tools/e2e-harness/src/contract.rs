use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CPA_ENV: &str = "HIROUTE_E2E_CPA_BIN";
pub const SUT_ENV: &str = "HIROUTE_E2E_SUT_BIN";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub schema_version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub case_shards: Vec<ScenarioCaseShard>,
    pub steps: Vec<ScenarioStep>,
    #[serde(skip)]
    pub(crate) selected_case: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioCaseShard {
    pub id: String,
    pub steps: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioStep {
    pub id: String,
    pub protocol: ClientProtocol,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub request_headers: BTreeMap<String, String>,
    pub request: Value,
    pub attempts: Vec<AttemptScript>,
    #[serde(
        default = "accepted_status",
        skip_serializing_if = "is_accepted_status"
    )]
    pub expected_http_status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_source: Option<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_class: Option<ComplexityClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_affinity: Option<Affinity>,
}

fn accepted_status() -> u16 {
    200
}

fn is_accepted_status(status: &u16) -> bool {
    *status == accepted_status()
}

impl ScenarioStep {
    pub(crate) fn accepted_source(&self) -> Source {
        self.expected_source
            .expect("validated accepted steps declare expected_source")
    }

    pub(crate) fn accepted_class(&self) -> ComplexityClass {
        self.expected_class
            .expect("validated accepted steps declare expected_class")
    }

    pub(crate) fn accepted_affinity(&self) -> Affinity {
        self.expected_affinity
            .expect("validated accepted steps declare expected_affinity")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptScript {
    pub source: Source,
    pub response_head_status: ResponseHeadStatus,
}

/// A deliberately finite response-head script. It cannot express paths,
/// commands, delays, bodies, or arbitrary status codes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct ResponseHeadStatus(u16);

impl ResponseHeadStatus {
    pub const ACCEPT: Self = Self(200);

    pub fn as_u16(self) -> u16 {
        self.0
    }

    pub fn is_accept(self) -> bool {
        self == Self::ACCEPT
    }
}

impl TryFrom<u16> for ResponseHeadStatus {
    type Error = String;

    fn try_from(status: u16) -> Result<Self, Self::Error> {
        match status {
            200 | 429 | 500 | 502 | 503 | 504 | 529 => Ok(Self(status)),
            _ => Err(format!(
                "response_head_status {status} is outside the finite E2E set"
            )),
        }
    }
}

impl From<ResponseHeadStatus> for u16 {
    fn from(status: ResponseHeadStatus) -> Self {
        status.as_u16()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientProtocol {
    Responses,
    Messages,
}

impl ClientProtocol {
    pub fn gateway_path(self) -> &'static str {
        match self {
            Self::Responses => "/v1/responses",
            Self::Messages => "/v1/messages",
        }
    }

    pub fn terminal_event(self) -> &'static str {
        match self {
            Self::Responses => "response.completed",
            Self::Messages => "message_stop",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum Source {
    #[serde(rename = "claude-compatible")]
    ClaudeCompatible,
    #[serde(rename = "codex-chatgpt")]
    CodexChatgpt,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCompatible => "claude-compatible",
            Self::CodexChatgpt => "codex-chatgpt",
        }
    }

    pub fn native_path(self) -> &'static str {
        match self {
            Self::ClaudeCompatible => "/v1/messages",
            Self::CodexChatgpt => "/responses",
        }
    }

    pub fn native_protocol(self) -> ClientProtocol {
        match self {
            Self::ClaudeCompatible => ClientProtocol::Messages,
            Self::CodexChatgpt => ClientProtocol::Responses,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplexityClass {
    Simple,
    Complex,
}

impl ComplexityClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Complex => "complex",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Affinity {
    NewTurn,
    ContinuationHit,
    ContinuationMiss,
}

impl Affinity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewTurn => "new_turn",
            Self::ContinuationHit => "continuation_hit",
            Self::ContinuationMiss => "continuation_miss",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema_version: u32,
    pub name: String,
    pub cpa_binary: String,
    pub sut_binary: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedProfile {
    pub cpa_binary: PathBuf,
    pub sut_binary: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ValidationSummary {
    pub schema_version: u32,
    pub scenario: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_case: Option<String>,
    pub profile: String,
    pub steps: usize,
    pub case_shards: Vec<CaseShardSummary>,
    pub protocols: Vec<String>,
    pub sources: Vec<String>,
    pub executable_environment_required: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CaseShardSummary {
    pub id: String,
    pub steps: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    #[error("cannot read contract {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse contract {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("invalid E2E contract: {0}")]
    Invalid(String),
    #[error("required executable environment variable {0} is not set")]
    MissingEnvironment(&'static str),
    #[error("{variable} does not identify a file")]
    InvalidExecutable { variable: &'static str },
}

impl Scenario {
    pub fn read(path: &Path) -> Result<Self, ContractError> {
        read_json(path)
    }

    /// Select one explicitly declared, self-contained case shard. A scenario
    /// without a complete shard partition cannot be sliced implicitly because
    /// ordering and continuation state may be part of its contract.
    pub fn select_case(&self, case: &str) -> Result<Self, ContractError> {
        self.validate()?;
        let selected = self
            .case_shards
            .iter()
            .find(|shard| shard.id == case)
            .ok_or_else(|| {
                invalid(if self.case_shards.is_empty() {
                    "scenario does not declare case_shards".to_owned()
                } else {
                    format!("unknown scenario case shard {case}")
                })
            })?;
        let ids: HashSet<_> = selected.steps.iter().map(String::as_str).collect();
        let mut scenario = self.clone();
        scenario.steps.retain(|step| ids.contains(step.id.as_str()));
        scenario.case_shards = vec![selected.clone()];
        scenario.selected_case = Some(selected.id.clone());
        scenario.validate()?;
        Ok(scenario)
    }

    pub fn selected_case(&self) -> Option<&str> {
        self.selected_case.as_deref()
    }

    pub fn validate_with(&self, profile: &Profile) -> Result<ValidationSummary, ContractError> {
        self.validate()?;
        profile.validate()?;
        let mut protocols = HashSet::new();
        let mut sources = HashSet::new();
        for step in &self.steps {
            protocols.insert(match step.protocol {
                ClientProtocol::Responses => "responses".to_owned(),
                ClientProtocol::Messages => "messages".to_owned(),
            });
            sources.extend(
                step.attempts
                    .iter()
                    .map(|attempt| attempt.source.as_str().to_owned()),
            );
        }
        let mut protocols: Vec<_> = protocols.into_iter().collect();
        let mut sources: Vec<_> = sources.into_iter().collect();
        protocols.sort();
        sources.sort();
        Ok(ValidationSummary {
            schema_version: 1,
            scenario: self.name.clone(),
            selected_case: self.selected_case.clone(),
            profile: profile.name.clone(),
            steps: self.steps.len(),
            case_shards: self
                .case_shards
                .iter()
                .map(|shard| CaseShardSummary {
                    id: shard.id.clone(),
                    steps: shard.steps.clone(),
                })
                .collect(),
            protocols,
            sources,
            executable_environment_required: vec![CPA_ENV.into(), SUT_ENV.into()],
        })
    }

    fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != 2 {
            return Err(invalid("scenario schema_version must be 2"));
        }
        if self.name.trim().is_empty() {
            return Err(invalid("scenario name must not be empty"));
        }
        if self.steps.is_empty() {
            return Err(invalid("scenario must contain at least one step"));
        }
        let mut ids = HashSet::new();
        for step in &self.steps {
            if step.id.trim().is_empty() || !ids.insert(step.id.as_str()) {
                return Err(invalid("scenario step IDs must be non-empty and unique"));
            }
            let request = step
                .request
                .as_object()
                .ok_or_else(|| invalid(format!("step {} request must be an object", step.id)))?;
            if request.get("stream").and_then(Value::as_bool) != Some(true) {
                return Err(invalid(format!(
                    "step {} must request a streaming response",
                    step.id
                )));
            }
            match step.protocol {
                ClientProtocol::Responses => {
                    if !request
                        .get("input")
                        .is_some_and(|value| value.is_string() || value.is_array())
                    {
                        return Err(invalid(format!(
                            "Responses step {} must contain string or array input",
                            step.id
                        )));
                    }
                }
                ClientProtocol::Messages => {
                    if !request.get("messages").is_some_and(Value::is_array) {
                        return Err(invalid(format!(
                            "Messages step {} must contain a messages array",
                            step.id
                        )));
                    }
                }
            }
            for (name, value) in &step.request_headers {
                if !matches!(name.as_str(), "session-id" | "thread-id") {
                    return Err(invalid(format!(
                        "step {} request_headers only allow session-id and thread-id",
                        step.id
                    )));
                }
                if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
                    return Err(invalid(format!(
                        "step {} request header values must be bounded visible text",
                        step.id
                    )));
                }
            }
            if step.expected_http_status == 400 {
                if !step.attempts.is_empty()
                    || step.expected_source.is_some()
                    || step.expected_class.is_some()
                    || step.expected_affinity.is_some()
                    || !step
                        .expected_error_code
                        .as_deref()
                        .is_some_and(valid_error_code)
                {
                    return Err(invalid(format!(
                        "rejected step {} must declare only an expected error code and no native attempts",
                        step.id
                    )));
                }
                continue;
            }
            if step.expected_http_status != accepted_status()
                || step.expected_error_code.is_some()
                || step.expected_source.is_none()
                || step.expected_class.is_none()
                || step.expected_affinity.is_none()
            {
                return Err(invalid(format!(
                    "accepted step {} must declare source, class, affinity, and HTTP 200",
                    step.id
                )));
            }
            if step.attempts.is_empty() || step.attempts.len() > 2 {
                return Err(invalid(format!(
                    "step {} must declare one or two native attempts",
                    step.id
                )));
            }
            let final_attempt = step
                .attempts
                .last()
                .expect("a non-empty attempt script was just established");
            if !final_attempt.response_head_status.is_accept() {
                return Err(invalid(format!(
                    "step {} must finish with an accepted 200 response head",
                    step.id
                )));
            }
            if Some(final_attempt.source) != step.expected_source {
                return Err(invalid(format!(
                    "step {} expected_source must match its final attempt",
                    step.id
                )));
            }
            if step.attempts[..step.attempts.len() - 1]
                .iter()
                .any(|attempt| attempt.response_head_status.is_accept())
            {
                return Err(invalid(format!(
                    "step {} cannot accept before its final attempt",
                    step.id
                )));
            }
            let attempt_sources: HashSet<_> =
                step.attempts.iter().map(|attempt| attempt.source).collect();
            if attempt_sources.len() != step.attempts.len() {
                return Err(invalid(format!(
                    "step {} cannot retry the same native source",
                    step.id
                )));
            }
        }
        if !self.case_shards.is_empty() {
            let step_ids: HashSet<_> = self.steps.iter().map(|step| step.id.as_str()).collect();
            let mut shard_ids = HashSet::new();
            let mut assigned_steps = HashSet::new();
            for shard in &self.case_shards {
                if shard.id.trim().is_empty() || !shard_ids.insert(shard.id.as_str()) {
                    return Err(invalid(
                        "scenario case shard IDs must be non-empty and unique",
                    ));
                }
                if shard.steps.is_empty() {
                    return Err(invalid(format!(
                        "scenario case shard {} must contain at least one step",
                        shard.id
                    )));
                }
                for step_id in &shard.steps {
                    if step_id.trim().is_empty() || !step_ids.contains(step_id.as_str()) {
                        return Err(invalid(format!(
                            "scenario case shard {} references an unknown step",
                            shard.id
                        )));
                    }
                    if !assigned_steps.insert(step_id.as_str()) {
                        return Err(invalid(format!(
                            "scenario step {step_id} belongs to more than one case shard"
                        )));
                    }
                }
            }
            if assigned_steps.len() != step_ids.len() {
                return Err(invalid(
                    "scenario case shards must partition every scenario step exactly once",
                ));
            }
        }
        Ok(())
    }
}

fn valid_error_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

impl Profile {
    pub fn read(path: &Path) -> Result<Self, ContractError> {
        read_json(path)
    }

    pub(crate) fn resolve(&self) -> Result<ResolvedProfile, ContractError> {
        self.validate()?;
        Ok(ResolvedProfile {
            cpa_binary: resolve_binary(CPA_ENV)?,
            sut_binary: resolve_binary(SUT_ENV)?,
        })
    }

    fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != 1 {
            return Err(invalid("profile schema_version must be 1"));
        }
        if self.name.trim().is_empty() {
            return Err(invalid("profile name must not be empty"));
        }
        if self.cpa_binary != format!("${{{CPA_ENV}}}") {
            return Err(invalid(format!(
                "profile cpa_binary must be exactly ${{{CPA_ENV}}}"
            )));
        }
        if self.sut_binary != format!("${{{SUT_ENV}}}") {
            return Err(invalid(format!(
                "profile sut_binary must be exactly ${{{SUT_ENV}}}"
            )));
        }
        Ok(())
    }
}

fn resolve_binary(variable: &'static str) -> Result<PathBuf, ContractError> {
    let value = env::var_os(variable).ok_or(ContractError::MissingEnvironment(variable))?;
    let path = PathBuf::from(value);
    if !path.is_file() {
        return Err(ContractError::InvalidExecutable { variable });
    }
    fs::canonicalize(path).map_err(|_| ContractError::InvalidExecutable { variable })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ContractError> {
    let bytes = fs::read(path).map_err(|source| ContractError::Read {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| ContractError::Parse {
        path: path.to_owned(),
        source,
    })
}

fn invalid(message: impl Into<String>) -> ContractError {
    ContractError::Invalid(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_rejects_command_or_path_escape_hatches() {
        let raw = r#"{
            "schema_version": 2,
            "name": "bad",
            "steps": [{
                "id": "one",
                "protocol": "responses",
                "request": {"stream": true, "input": "hello"},
                "attempts": [{"source": "claude-compatible", "response_head_status": 200}],
                "expected_source": "claude-compatible",
                "expected_class": "simple",
                "expected_affinity": "new_turn",
                "command": "curl example"
            }]
        }"#;
        let error = serde_json::from_str::<Scenario>(raw).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn profile_requires_fixed_environment_references() {
        let profile = Profile {
            schema_version: 1,
            name: "bad".into(),
            cpa_binary: "/tmp/cpa".into(),
            sut_binary: format!("${{{SUT_ENV}}}"),
        };
        assert!(profile.validate().is_err());
    }

    #[test]
    fn valid_minimal_contract_has_a_stable_summary() {
        let scenario = Scenario {
            schema_version: 2,
            name: "one".into(),
            case_shards: vec![],
            steps: vec![ScenarioStep {
                id: "simple".into(),
                protocol: ClientProtocol::Responses,
                request_headers: BTreeMap::new(),
                request: serde_json::json!({"stream": true, "input": "hello"}),
                attempts: vec![AttemptScript {
                    source: Source::ClaudeCompatible,
                    response_head_status: ResponseHeadStatus::ACCEPT,
                }],
                expected_http_status: 200,
                expected_error_code: None,
                expected_source: Some(Source::ClaudeCompatible),
                expected_class: Some(ComplexityClass::Simple),
                expected_affinity: Some(Affinity::NewTurn),
            }],
            selected_case: None,
        };
        let profile = Profile {
            schema_version: 1,
            name: "local".into(),
            cpa_binary: format!("${{{CPA_ENV}}}"),
            sut_binary: format!("${{{SUT_ENV}}}"),
        };
        let summary = scenario.validate_with(&profile).unwrap();
        assert_eq!(summary.steps, 1);
        assert_eq!(summary.protocols, ["responses"]);
    }

    #[test]
    fn response_head_script_is_finite_and_must_finish_in_accept() {
        assert!(ResponseHeadStatus::try_from(500).is_ok());
        assert!(ResponseHeadStatus::try_from(418).is_err());
        let raw = r#"{
            "schema_version": 2,
            "name": "bad",
            "steps": [{
                "id": "one",
                "protocol": "responses",
                "request": {"stream": true, "input": "hello"},
                "attempts": [{"source": "claude-compatible", "response_head_status": 500}],
                "expected_source": "claude-compatible",
                "expected_class": "simple",
                "expected_affinity": "new_turn"
            }]
        }"#;
        let scenario = serde_json::from_str::<Scenario>(raw).unwrap();
        let profile = Profile {
            schema_version: 1,
            name: "local".into(),
            cpa_binary: format!("${{{CPA_ENV}}}"),
            sut_binary: format!("${{{SUT_ENV}}}"),
        };
        assert!(scenario.validate_with(&profile).is_err());
    }

    #[test]
    fn rejected_step_is_bounded_and_has_no_native_attempt() {
        let valid = r#"{
            "schema_version": 2,
            "name": "rejection",
            "steps": [{
                "id": "unsupported",
                "protocol": "responses",
                "request_headers": {"session-id": "bounded-session"},
                "request": {"stream": true, "input": "hello", "previous_response_id": "r"},
                "attempts": [],
                "expected_http_status": 400,
                "expected_error_code": "RESPONSES_PREVIOUS_RESPONSE_ID_UNSUPPORTED"
            }]
        }"#;
        let scenario = serde_json::from_str::<Scenario>(valid).unwrap();
        assert!(scenario.validate().is_ok());

        let invalid = valid.replace(
            "\"attempts\": []",
            "\"attempts\": [{\"source\": \"claude-compatible\", \"response_head_status\": 200}]",
        );
        let scenario = serde_json::from_str::<Scenario>(&invalid).unwrap();
        assert!(scenario.validate().is_err());

        let invalid = valid.replace("session-id", "authorization");
        let scenario = serde_json::from_str::<Scenario>(&invalid).unwrap();
        assert!(scenario.validate().is_err());
    }

    #[test]
    fn checked_in_core_scenario_covers_routing_hold_and_early_rejection() {
        let scenario: Scenario =
            serde_json::from_str(include_str!("../../../e2e/scenarios/core-routing.json")).unwrap();
        let profile: Profile =
            serde_json::from_str(include_str!("../../../e2e/profiles/local-process.json")).unwrap();
        scenario.validate_with(&profile).unwrap();
        assert_eq!(scenario.steps.len(), 17);
        assert_eq!(scenario.case_shards.len(), 10);
        let quadrants: HashSet<_> = scenario
            .steps
            .iter()
            .map(|step| (step.protocol, step.expected_source))
            .collect();
        assert!(quadrants.contains(&(ClientProtocol::Responses, Some(Source::ClaudeCompatible))));
        assert!(quadrants.contains(&(ClientProtocol::Responses, Some(Source::CodexChatgpt))));
        assert!(quadrants.contains(&(ClientProtocol::Messages, Some(Source::ClaudeCompatible))));
        assert!(quadrants.contains(&(ClientProtocol::Messages, Some(Source::CodexChatgpt))));
        assert!(
            scenario
                .steps
                .iter()
                .any(|step| step.expected_affinity == Some(Affinity::ContinuationHit))
        );
        assert!(scenario.steps.iter().any(|step| {
            step.attempts.len() == 2
                && step.attempts[0].response_head_status.as_u16() == 500
                && step.attempts[1].response_head_status.is_accept()
        }));
        assert!(scenario.steps.iter().any(|step| {
            step.expected_error_code.as_deref()
                == Some("RESPONSES_PREVIOUS_RESPONSE_ID_UNSUPPORTED")
                && step.attempts.is_empty()
        }));
        assert!(scenario.steps.iter().any(|step| {
            step.id == "responses-context-hold-simple-append"
                && step.expected_class == Some(ComplexityClass::Simple)
                && step.expected_source == Some(Source::CodexChatgpt)
        }));
        let selected = scenario
            .select_case("responses-complex-continuation")
            .unwrap();
        assert_eq!(
            selected.selected_case(),
            Some("responses-complex-continuation")
        );
        assert_eq!(
            selected
                .steps
                .iter()
                .map(|step| step.id.as_str())
                .collect::<Vec<_>>(),
            [
                "responses-complex-to-codex",
                "responses-tool-continuation-affinity-hit"
            ]
        );
        assert!(scenario.select_case("unknown").is_err());
    }

    #[test]
    fn case_shards_reject_a_missing_or_overlapping_step() {
        let missing = r#"{
            "schema_version": 2,
            "name": "partition",
            "case_shards": [{"id": "one", "steps": ["one"]}],
            "steps": [
                {"id": "one", "protocol": "responses", "request": {"stream": true, "input": "a"}, "attempts": [{"source": "claude-compatible", "response_head_status": 200}], "expected_source": "claude-compatible", "expected_class": "simple", "expected_affinity": "new_turn"},
                {"id": "two", "protocol": "responses", "request": {"stream": true, "input": "b"}, "attempts": [{"source": "claude-compatible", "response_head_status": 200}], "expected_source": "claude-compatible", "expected_class": "simple", "expected_affinity": "new_turn"}
            ]
        }"#;
        let scenario = serde_json::from_str::<Scenario>(missing).unwrap();
        assert!(scenario.validate().is_err());

        let overlapping = missing.replace(
            r#"[{"id": "one", "steps": ["one"]}]"#,
            r#"[{"id": "one", "steps": ["one", "two"]}, {"id": "two", "steps": ["two"]}]"#,
        );
        let scenario = serde_json::from_str::<Scenario>(&overlapping).unwrap();
        assert!(scenario.validate().is_err());
    }
}
