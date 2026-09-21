//! Native Claude settings effects in the production Operation composition.
use super::LocalControlAdapter;
use hiroute_application::agent_connection::{
    ClaudeModelFileAction, settings_claude_model_file_for_operation,
};
use hiroute_domain::{
    AgentAccessGrantRefV1, AgentIngressProtocolV1, CanonicalDigest, ControlRepositoryPort,
    EffectReconciliation, ExternalEffectIntentV1, NativeAgentArtifactPort, OperationId,
    OperationState, OperationStepKind, OwnedEffectV1, PortError, PortErrorCode, PortResult,
    SecretStorePort, WorkspaceId, is_agent_access_grant_effect,
};

pub(super) fn is_settings_claude_model(intent: &ExternalEffectIntentV1) -> bool {
    intent.effect_id() == "agent-connection-managed-configuration"
        && intent.desired()["transaction"] == "settings"
        && intent.desired()["subject"]["agent_id"] == "agent_claude_default"
}

impl LocalControlAdapter {
    pub(super) fn stage_settings_claude_model(
        &self,
        operation: &hiroute_domain::OperationV1,
        intent: &ExternalEffectIntentV1,
    ) -> PortResult<OwnedEffectV1> {
        let stores = self.stores_lock()?;
        let operation_id = &operation.operation_id;
        if operation.state != OperationState::ApplyingAgentArtifacts {
            return Err(conflict("claude.settings.phase"));
        }
        let payload = settings_claude_model_file_for_operation(operation, intent)?;
        match &payload.change {
            ClaudeModelFileAction::Configure {
                previous_operation,
                snapshot,
                gateway_base_url,
                trusted_hiroute_executable,
            } => {
                let mutation = operation
                    .plan
                    .agent_access_grants()
                    .first()
                    .filter(|_| operation.plan.agent_access_grants().len() == 1)
                    .ok_or_else(|| conflict("claude.settings.grant"))?;
                let scope = mutation
                    .desired_scope()
                    .ok_or_else(|| conflict("claude.settings.scope"))?;
                if mutation.owner_scope() != operation.workspace_id.as_str()
                    || scope.connection_id() != format!("agent-connection/{}", payload.context_id)
                    || scope.protocol() != AgentIngressProtocolV1::Messages
                {
                    return Err(conflict("claude.settings.grant.binding"));
                }
                let runtime = self
                    .managed_agent_runtime
                    .lock()
                    .map_err(|_| conflict("claude.settings.runtime.lock"))?;
                if runtime.as_ref().is_none_or(|runtime| {
                    runtime.gateway_base_url != *gateway_base_url
                        || runtime.trusted_hiroute_executable != *trusted_hiroute_executable
                }) {
                    return Err(conflict("claude.settings.runtime.changed"));
                }
                drop(runtime);
                let active = stores
                    .secrets()
                    .inspect_agent_access_grant(WorkspaceId::DEFAULT, scope.connection_id())?;
                if let Some(previous_operation) = previous_operation {
                    let previous = stores
                        .control()
                        .load_operation(previous_operation)?
                        .ok_or_else(|| conflict("claude.settings.previous.operation"))?;
                    if previous.workspace_id != operation.workspace_id
                        || previous.state != OperationState::Succeeded
                    {
                        return Err(conflict("claude.settings.previous.owner"));
                    }
                    let previous_intent = previous
                        .plan
                        .external()
                        .iter()
                        .find(|candidate| {
                            candidate.target() == intent.target()
                                && is_settings_claude_model(candidate)
                        })
                        .ok_or_else(|| conflict("claude.settings.previous.effect"))?
                        .clone();
                    let previous_payload =
                        settings_claude_model_file_for_operation(&previous, &previous_intent)?;
                    if !matches!(
                        previous_payload.change,
                        ClaudeModelFileAction::Configure { .. }
                    ) {
                        return Err(conflict("claude.settings.previous.action"));
                    }
                    let [previous_mutation] = previous.plan.agent_access_grants() else {
                        return Err(conflict("claude.settings.previous.grant"));
                    };
                    let previous_effect = previous
                        .step(OperationStepKind::ApplySecrets)
                        .effects
                        .iter()
                        .find(|effect| is_agent_access_grant_effect(effect))
                        .ok_or_else(|| conflict("claude.settings.previous.grant.effect"))?;
                    let previous_reference = AgentAccessGrantRefV1::from_ensure_effect(
                        previous_effect,
                        previous_mutation,
                    )
                    .map_err(|_| conflict("claude.settings.previous.grant.reference"))?;
                    if active.as_ref() != Some(&previous_reference)
                        || previous_reference.generation() != mutation.expected_generation()
                    {
                        return Err(conflict("claude.settings.previous.binding"));
                    }
                } else if active.is_some() {
                    return Err(conflict("claude.settings.previous.missing"));
                }
                let rendered = serde_json::to_vec(&serde_json::json!({
                    "schema": "hiroute.claude-launch-snapshot/v1",
                    "operation_id": operation_id,
                    "context_id": payload.context_id,
                    "snapshot": snapshot,
                    "grant_scope": scope,
                    "gateway_base_url": gateway_base_url,
                    "trusted_hiroute_executable": trusted_hiroute_executable,
                }))
                .map_err(|_| conflict("claude.snapshot.encode"))?;
                drop(stores);
                self.stage_claude_snapshot_bytes(
                    operation_id,
                    intent,
                    &payload.expected_content,
                    Some(&rendered),
                )
            }
            ClaudeModelFileAction::Restore { original_operation } => {
                let original = stores
                    .control()
                    .load_operation(original_operation)?
                    .ok_or_else(|| conflict("claude.settings.restore.original"))?;
                if original.workspace_id != operation.workspace_id
                    || original.state != OperationState::Succeeded
                {
                    return Err(conflict("claude.settings.restore.owner"));
                }
                let original_intent = original
                    .plan
                    .external()
                    .iter()
                    .find(|original| {
                        original.target() == intent.target() && is_settings_claude_model(original)
                    })
                    .ok_or_else(|| conflict("claude.settings.restore.effect"))?;
                let before = settings_claude_model_file_for_operation(&original, original_intent)?;
                if before.context_id != payload.context_id
                    || !matches!(before.change, ClaudeModelFileAction::Configure { .. })
                {
                    return Err(conflict("claude.settings.restore.context"));
                }
                drop(stores);
                self.stage_claude_snapshot_bytes(
                    operation_id,
                    intent,
                    &payload.expected_content,
                    None,
                )
            }
        }
    }

    fn stage_claude_snapshot_bytes(
        &self,
        operation: &OperationId,
        intent: &ExternalEffectIntentV1,
        expected_content: &CanonicalDigest,
        bytes: Option<&[u8]>,
    ) -> PortResult<OwnedEffectV1> {
        match self.artifacts.observe_artifact(operation, intent)? {
            EffectReconciliation::Staged(effect) | EffectReconciliation::Applied(effect) => {
                return Ok(effect);
            }
            EffectReconciliation::OwnershipLost(_) => {
                return Err(conflict("claude.snapshot.ownership"));
            }
            EffectReconciliation::Missing => {}
        }
        let current = self.artifacts.read_native_target(intent.target())?;
        if &CanonicalDigest::of_bytes(current.as_deref().map_or(&[], Vec::as_slice))
            != expected_content
        {
            return Err(conflict("claude.snapshot.changed"));
        }
        self.artifacts
            .stage_native_target(operation, intent, bytes, true)
    }
}

fn conflict(context: &'static str) -> PortError {
    PortError::new(PortErrorCode::Conflict, context)
}
