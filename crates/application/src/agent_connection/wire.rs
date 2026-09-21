pub use hiroute_application_api::{AGENT_CONNECT_SPEC_SCHEMA_V1, AgentConnectSpecV1};
use hiroute_domain::SchemaVersion;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRestoreSpecV1 {
    pub schema_version: SchemaVersion,
    pub agent_id: String,
    pub profile_id: String,
    pub installed_version: String,
    pub restore_point_ref: String,
}
