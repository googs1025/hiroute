use std::collections::BTreeSet;

use hiroute_domain::{
    AgentActivationModeV1, AgentPlanAllowedScopeV1, AgentPlanId, CanonicalDigest, RevisionSetV1,
    SchemaVersion,
};
use serde::{Deserialize, Serialize};

use crate::WarningV1;

pub const AGENT_CONNECT_SPEC_SCHEMA_V1: SchemaVersion = SchemaVersion::new(1, 0);
pub const AGENT_CONNECTION_PREVIEW_SCHEMA_V1: &str = "hiroute.agent-connection-preview/v1";
pub const AGENT_CONNECTION_STATUS_SCHEMA_V1: &str = "hiroute.agent-connection-status/v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConnectSpecV1 {
    pub schema_version: SchemaVersion,
    pub agent_id: String,
    pub profile_id: String,
    pub installed_version: String,
    pub default_agent_plan_id: AgentPlanId,
    #[serde(default)]
    pub allowed_scope: AgentPlanAllowedScopeV1,
    #[serde(default)]
    pub allowed_agent_plan_ids: BTreeSet<AgentPlanId>,
    #[serde(default)]
    pub native_subagent_routing: bool,
    #[serde(default)]
    pub dynamic_catalog_available: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConnectionPreviewRequestV1 {
    pub spec: AgentConnectSpecV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConnectionApplyRequestV1 {
    pub spec: AgentConnectSpecV1,
    pub accept_digest: CanonicalDigest,
    pub expected_revisions: RevisionSetV1,
    pub idempotency_key: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentConnectionEffectStateV1 {
    Planned,
    NoFieldChange,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConnectionEffectPreviewV1 {
    pub role: String,
    pub state: AgentConnectionEffectStateV1,
    pub desired_digest: CanonicalDigest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConnectionPreviewResultV1 {
    pub schema: String,
    pub applicable: bool,
    pub change_digest: CanonicalDigest,
    pub expected_revisions: RevisionSetV1,
    pub agent_id: String,
    pub profile_id: String,
    pub installed_version: String,
    pub activation_mode: AgentActivationModeV1,
    #[serde(default)]
    pub warnings: Vec<WarningV1>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub effects: Vec<AgentConnectionEffectPreviewV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConnectionStatusRequestV1 {
    pub connection_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentConnectionStateV1 {
    Active,
    Revoked,
    Disconnected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConnectionStatusV1 {
    pub schema: String,
    pub connection_id: String,
    pub agent_id: String,
    pub profile_id: String,
    pub installed_version: String,
    pub state: AgentConnectionStateV1,
    pub activation_mode: AgentActivationModeV1,
    pub revision: u64,
    pub launch_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_digest: Option<CanonicalDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_generation: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLaunchDescriptorRequestV1 {
    pub connection_id: String,
}
