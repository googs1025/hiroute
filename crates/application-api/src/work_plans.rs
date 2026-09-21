//! Collaboration discovery contains data only; these values never grant execution authority.
use hiroute_domain::delegation::WorkerHarnessV1;
use hiroute_domain::{AgentIngressProtocolV1, AgentPlanId, WorkspaceId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkPlanListRequestV1 {
    pub workspace_id: WorkspaceId,
    pub context_id: String,
    pub grant_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPlanAvailabilityV1 {
    Ready,
    Unavailable,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkPlanViewV1 {
    pub agent_plan_id: AgentPlanId,
    pub alias: String,
    pub display_name: String,
    pub purpose: String,
    pub harness: WorkerHarnessV1,
    pub protocol: AgentIngressProtocolV1,
    pub availability: WorkPlanAvailabilityV1,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkPlanListV1 {
    pub schema: String,
    pub plans: Vec<WorkPlanViewV1>,
}
