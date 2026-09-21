use hiroute_application_api::{LOCAL_CONTROL_SCHEMA_V2, SystemStatusV1};
use hiroute_domain::WorkspaceId;

use super::{AgentDiscoveryPort, ControlReadError, ControlStatePort};

pub fn system_status(
    control: &dyn ControlStatePort,
    discovery: &dyn AgentDiscoveryPort,
    workspace_id: &WorkspaceId,
    observation_available: bool,
    role_all_ready: bool,
) -> Result<SystemStatusV1, ControlReadError> {
    let snapshot = control.snapshot(workspace_id)?;
    let agents = discovery.discover()?;
    Ok(SystemStatusV1 {
        schema: "hiroute.system-status/v1".to_owned(),
        daemon: if role_all_ready {
            "role_all"
        } else {
            "control_only"
        }
        .to_owned(),
        local_control: format!(
            "v{}.{}:authenticated",
            LOCAL_CONTROL_SCHEMA_V2.major, LOCAL_CONTROL_SCHEMA_V2.minor
        ),
        control_store: "ready".to_owned(),
        observation_store: if observation_available {
            "ready"
        } else {
            "unavailable"
        }
        .to_owned(),
        discovery: format!(
            "ready:{}_supported",
            agents.iter().filter(|agent| agent.supported).count()
        ),
        gateway: if role_all_ready {
            "ready"
        } else {
            "unavailable:not_composed"
        }
        .to_owned(),
        setup_revision: snapshot.revisions.target,
        recoverable_operations: snapshot.recoverable_operations.len(),
    })
}
