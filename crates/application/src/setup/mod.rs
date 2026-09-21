use hiroute_application_api::{
    CHANGE_SPEC_SCHEMA_V1, CanonicalDigest, ChangeSpecV1, SETUP_REQUEST_SCHEMA_V1, SetupPreviewV1,
    SetupRequestV1,
};
use hiroute_domain::WorkspaceId;
use serde::Serialize;
use serde_json::{Value, json};

use crate::control::{AgentDiscoveryPort, ControlReadError, ControlStatePort};

pub fn setup_change(control: Value) -> ChangeSpecV1 {
    ChangeSpecV1 {
        schema_version: CHANGE_SPEC_SCHEMA_V1,
        command_id: "setup.apply".to_owned(),
        resource_id: Some("personal/default".to_owned()),
        desired_state: json!({
            "control": control,
            "runtime": [],
            "external": []
        }),
    }
}

#[derive(Serialize)]
struct SetupDigestInput<'a> {
    schema: &'static str,
    workspace_id: &'a WorkspaceId,
    spec: &'a SetupRequestV1,
    revisions: &'a hiroute_application_api::RevisionSetV1,
    discovered_agents: &'a [String],
    blockers: &'a [String],
}

pub(crate) fn preview(
    control: &dyn ControlStatePort,
    discovery: &dyn AgentDiscoveryPort,
    workspace_id: &WorkspaceId,
    spec: SetupRequestV1,
) -> Result<SetupPreviewV1, SetupPreviewError> {
    validate_spec(&spec)?;
    let snapshot = control.snapshot(workspace_id)?;
    let discovered = discovery.discover()?;
    let mut discovered_agents = discovered
        .into_iter()
        .filter(|agent| agent.supported)
        .map(|agent| agent.agent_id)
        .collect::<Vec<_>>();
    discovered_agents.sort();
    let selected = if spec.agent_ids.is_empty() {
        discovered_agents.clone()
    } else {
        let mut selected = spec.agent_ids.clone();
        selected.sort();
        selected.dedup();
        if selected
            .iter()
            .any(|agent| !discovered_agents.contains(agent))
        {
            return Err(SetupPreviewError::UnknownAgent);
        }
        selected
    };
    let mut blockers = Vec::new();
    if selected.is_empty() {
        blockers.push("NO_SUPPORTED_AGENT".to_owned());
    }
    // The full setup effect set necessarily includes Gateway publication. This control-only
    // process reports that exact blocker rather than manufacturing a partial setup success.
    blockers.push("GATEWAY_RUNTIME_UNBOUND".to_owned());
    let digest = CanonicalDigest::of(&SetupDigestInput {
        schema: "hiroute.setup-preview-digest/v1",
        workspace_id,
        spec: &spec,
        revisions: &snapshot.revisions,
        discovered_agents: &selected,
        blockers: &blockers,
    })
    .map_err(|_| SetupPreviewError::InvalidInput)?;
    Ok(SetupPreviewV1 {
        schema: "hiroute.change-preview/v1".to_owned(),
        applicable: blockers.is_empty(),
        change_digest: digest,
        expected_revision: snapshot.revisions.target,
        normalized_spec: spec,
        discovered_agents: selected,
        blockers,
        effects: vec![
            "control_desired_state".to_owned(),
            "gateway_publication".to_owned(),
            "agent_owned_configuration".to_owned(),
        ],
    })
}

fn validate_spec(spec: &SetupRequestV1) -> Result<(), SetupPreviewError> {
    if spec.schema != SETUP_REQUEST_SCHEMA_V1
        || spec
            .routing_purpose
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 512 || value.contains('\0'))
        || spec.agent_ids.iter().any(|value| !valid_id(value))
    {
        Err(SetupPreviewError::InvalidInput)
    } else {
        Ok(())
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetupPreviewError {
    InvalidInput,
    UnknownAgent,
    Control(ControlReadError),
}

impl From<ControlReadError> for SetupPreviewError {
    fn from(value: ControlReadError) -> Self {
        Self::Control(value)
    }
}
