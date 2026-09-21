//! Model publication for independently confirmed settings, using the existing Gateway aggregate.
use super::*;
use crate::{
    AgentFacetIntent, AgentModelGrantV2, AgentModelRouteV2, GatewayAccessGrantV1,
    GatewayPublicationRevision, PublicationRecordV1,
};

const SCHEMA: &str = "hiroute.settings-model-publication/v1";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    schema: String,
    context_id: String,
    source_publication_digest: CanonicalDigest,
    restore_grant: Option<AgentAccessGrantRefV1>,
}

/// The Application renderer supplies the exact previewed base and, for restore, the owned
/// original grant reference. Only verifier metadata enters the journal, never bearer material.
pub fn settings_model_publication_intent(
    control: &AgentConnectionControlIntentV1,
    context_id: &str,
    source_publication_digest: CanonicalDigest,
    restore_grant: Option<AgentAccessGrantRefV1>,
) -> Result<ExternalEffectIntentV1, OperationValidationError> {
    if control.transaction() != AgentConnectionTransactionKindV1::Settings {
        return Err(invalid());
    }
    let payload = Payload {
        schema: SCHEMA.into(),
        context_id: context_id.into(),
        source_publication_digest: source_publication_digest.clone(),
        restore_grant,
    };
    validate_payload(&payload)?;
    ExternalEffectIntentV1::from_agent_connection_planner(
        control,
        AgentConnectionEffectRoleV1::GrantScopedPublication,
        Some(source_publication_digest),
        &payload,
        0o644,
    )
}

fn validate_payload(payload: &Payload) -> Result<(), OperationValidationError> {
    if payload.schema != SCHEMA {
        return Err(invalid());
    }
    validate_scope_identifier(&payload.context_id)?;
    if let Some(reference) = &payload.restore_grant {
        reference.validate()?;
        if reference.connection_id() != format!("agent-connection/{}", payload.context_id) {
            return Err(invalid());
        }
    }
    Ok(())
}

fn decode(intent: &ExternalEffectIntentV1) -> Result<Payload, OperationValidationError> {
    validate_external_components(
        intent.effect_id(),
        intent.kind(),
        intent.target(),
        intent.desired(),
        intent.desired_mode(),
        intent.sensitive(),
    )?;
    let envelope = decode_effect(intent.desired())?;
    if envelope.transaction != "settings" || envelope.role != "grant_scoped_publication" {
        return Err(invalid());
    }
    let payload: Payload = serde_json::from_value(envelope.payload).map_err(|_| invalid())?;
    validate_payload(&payload)?;
    if intent.before_fingerprint() != Some(&payload.source_publication_digest) {
        return Err(invalid());
    }
    Ok(payload)
}

pub fn validate_settings_model_publication_intent(
    intent: &ExternalEffectIntentV1,
) -> Result<(), OperationValidationError> {
    decode(intent).map(|_| ())
}

/// Resolve only the original registered Operation. Existing publication stage/activation and
/// recovery markers remain the sole commit protocol; this function has no storage side effects.
pub fn settings_model_publication_record(
    operation: &OperationV1,
    intent: &ExternalEffectIntentV1,
    base: &PublicationRecordV1,
    grant_effect: &OwnedEffectV1,
) -> Result<PublicationRecordV1, OperationValidationError> {
    validate_plan(
        operation.plan.spec(),
        operation.plan.control(),
        operation.plan.secrets(),
        operation.plan.runtime(),
        operation.plan.external(),
    )?;
    let spec = settings::decode_spec(operation.plan.spec())?;
    let payload = decode(intent)?;
    if !operation.plan.external().contains(intent)
        || payload.context_id != spec.context_id
        || base.workspace_id != operation.workspace_id
        || base.digest != payload.source_publication_digest
    {
        return Err(invalid());
    }
    let control = decode_control(operation.plan.control())?;
    if control.payload["state"]["accept_digest"]
        != serde_json::to_value(&operation.accepted_digest)?
    {
        return Err(invalid());
    }
    let active = base.verify().map_err(|_| invalid())?;
    let [mutation] = operation.plan.agent_access_grants() else {
        return Err(invalid());
    };
    if mutation.owner_scope() != operation.workspace_id.as_str()
        || mutation.connection_id() != format!("agent-connection/{}", spec.context_id)
    {
        return Err(invalid());
    }
    let revision = GatewayPublicationRevision::new(
        active
            .publication_revision
            .get()
            .checked_add(1)
            .ok_or_else(invalid)?,
    )
    .map_err(|_| invalid())?;
    let next = match &spec.model {
        AgentFacetIntent::Configure { settings } => {
            if payload.restore_grant.is_some() {
                return Err(invalid());
            }
            let reference = AgentAccessGrantRefV1::from_ensure_effect(grant_effect, mutation)?;
            let scope = reference.scope();
            let fixed_bindings = scope
                .model_grant()
                .routes
                .iter()
                .filter_map(|(name, route)| match route {
                    AgentModelRouteV2::Fixed { binding, .. } => {
                        Some((name.clone(), binding.as_ref().clone()))
                    }
                    AgentModelRouteV2::Plan { .. } => None,
                })
                .collect();
            let grant =
                AgentModelGrantV2::derive(scope.protocol(), settings, &active, &fixed_bindings)
                    .map_err(|_| invalid())?;
            if reference.owner_scope() != operation.workspace_id.as_str()
                || reference.connection_id() != mutation.connection_id()
                || scope.model_grant() != &grant
                || control.payload["state"]["model_grant"] != serde_json::to_value(&grant)?
            {
                return Err(invalid());
            }
            let access = GatewayAccessGrantV1::new(
                reference.grant_id(),
                reference.generation(),
                reference.material_sha256().clone(),
                scope.protocol(),
                grant,
            )
            .map_err(|_| invalid())?;
            if active.grants.iter().any(|existing| {
                existing.grant_id == access.grant_id
                    && existing.generation == access.generation
                    && existing.bearer_token_sha256 == access.bearer_token_sha256
                    && existing.model_grant == access.model_grant
            }) {
                return Ok(base.clone());
            }
            active
                .next_with_access_grant(revision, access)
                .map_err(|_| invalid())?
        }
        AgentFacetIntent::Restore { .. } => {
            let reference = payload.restore_grant.as_ref().ok_or_else(invalid)?;
            if mutation.kind() != AgentAccessGrantMutationKindV1::Revoke
                || reference.owner_scope() != operation.workspace_id.as_str()
                || reference.generation() != mutation.expected_generation()
                || !active.grants.iter().any(|grant| {
                    grant.grant_id == reference.grant_id()
                        && grant.generation == reference.generation()
                        && grant.bearer_token_sha256 == *reference.material_sha256()
                        && &grant.model_grant == reference.scope().model_grant()
                })
            {
                return Err(invalid());
            }
            active
                .next_without_access_grant(revision, reference.grant_id())
                .map_err(|_| invalid())?
        }
        AgentFacetIntent::Keep => return Err(invalid()),
    };
    PublicationRecordV1::from_publication(operation.workspace_id.clone(), &next)
        .map_err(|_| invalid())
}
fn invalid() -> OperationValidationError {
    OperationValidationError::UnregisteredEffectPlan
}
