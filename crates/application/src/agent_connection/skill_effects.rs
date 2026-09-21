//! File stage for an already confirmed shared-root ownership plan.
use super::*;
use hiroute_domain::{
    EffectReconciliation, ExternalEffectIntentV1, NativeAgentArtifactPort, OperationId,
    OwnedEffectV1, PortError, PortErrorCode, PortResult,
};

pub fn stage_collaboration_skill(
    port: &dyn NativeAgentArtifactPort,
    operation: &OperationId,
    intent: &ExternalEffectIntentV1,
    previous: Option<&ManagedCollaborationSkill>,
    plan: &mut SkillOwnershipPlan,
    template: Option<&CollaborationSkillTemplate>,
) -> PortResult<Option<OwnedEffectV1>> {
    let invalid = || PortError::new(PortErrorCode::InvalidData, "skill.file.intent");
    let desired = match plan.file_action {
        SkillFileAction::Install => {
            let template = template.ok_or_else(invalid)?;
            if template.revision != plan.next.template_revision
                || template.content.is_empty()
                || template.content.len() > 16 * 1024
                || template.content.contains('\0')
                || CanonicalDigest::of_bytes(template.content.as_bytes())
                    != plan.next.content_digest
            {
                return Err(invalid());
            }
            Some(template.content.as_bytes())
        }
        SkillFileAction::Remove => None,
        SkillFileAction::Keep | SkillFileAction::Conflict => None,
    };
    stage_skill_bytes(port, operation, intent, previous, plan, desired)
}

fn stage_skill_bytes(
    port: &dyn NativeAgentArtifactPort,
    operation: &OperationId,
    intent: &ExternalEffectIntentV1,
    previous: Option<&ManagedCollaborationSkill>,
    plan: &mut SkillOwnershipPlan,
    desired: Option<&[u8]>,
) -> PortResult<Option<OwnedEffectV1>> {
    let invalid = || PortError::new(PortErrorCode::InvalidData, "skill.file.intent");
    plan.next.validate().map_err(|_| invalid())?;
    if let Some(previous) = previous {
        previous.validate().map_err(|_| invalid())?;
        if previous.root_ref != plan.next.root_ref {
            return Err(invalid());
        }
    }
    match plan.file_action {
        SkillFileAction::Keep => return Ok(None),
        SkillFileAction::Conflict => {
            return Err(PortError::new(
                PortErrorCode::Conflict,
                "skill.file.user_edit",
            ));
        }
        SkillFileAction::Remove if !plan.next.contexts.is_empty() || previous.is_none() => {
            return Err(invalid());
        }
        SkillFileAction::Install if plan.next.contexts.is_empty() => return Err(invalid()),
        _ => {}
    }
    match port.observe_artifact(operation, intent)? {
        EffectReconciliation::Staged(effect) | EffectReconciliation::Applied(effect) => {
            plan.next.file_effect = Some(
                hiroute_domain::SkillFileEffectRef::from_intent(operation, intent)
                    .map_err(|_| invalid())?,
            );
            return Ok(Some(effect));
        }
        EffectReconciliation::OwnershipLost(_) => {
            return Err(PortError::new(
                PortErrorCode::Conflict,
                "skill.file.ownership",
            ));
        }
        EffectReconciliation::Missing => {}
    }
    let current = port.read_native_target(intent.target())?;
    if let Some(bytes) = &current {
        let current_digest = CanonicalDigest::of_bytes(bytes);
        let valid = previous.is_some_and(|owned| owned.content_digest == current_digest);
        if !valid {
            return Err(PortError::new(
                PortErrorCode::Conflict,
                "skill.file.user_edit",
            ));
        }
    }
    // Durable grant revocation and reference CAS precede removal in the existing Operation.
    // A cleanup failure must never compensate that authorization transition.
    let effect = port.stage_native_skill_target(
        operation,
        intent,
        desired,
        previous.and_then(|record| record.file_effect.as_ref()),
    )?;
    plan.next.file_effect = Some(
        hiroute_domain::SkillFileEffectRef::from_intent(operation, intent)
            .map_err(|_| invalid())?,
    );
    Ok(Some(effect))
}

/// Call after the zero-reference record and the removal effect are durable. File/cleanup errors
/// never roll collaboration authorization back. False means owned directories were retained.
pub fn finish_collaboration_skill_removal(
    port: &dyn NativeAgentArtifactPort,
    operation: &OperationId,
    intent: &ExternalEffectIntentV1,
    current: &ManagedCollaborationSkill,
) -> PortResult<bool> {
    current
        .validate()
        .map_err(|_| PortError::new(PortErrorCode::InvalidData, "skill.cleanup.record"))?;
    let expected = hiroute_domain::SkillFileEffectRef::from_intent(operation, intent)
        .map_err(|_| PortError::new(PortErrorCode::InvalidData, "skill.cleanup.intent"))?;
    if !current.contexts.is_empty()
        || current.file_effect.as_ref() != Some(&expected)
        || !matches!(
            port.observe_artifact(operation, intent)?,
            EffectReconciliation::Applied(_)
        )
        || port.read_native_target(intent.target())?.is_some()
    {
        return Err(PortError::new(
            PortErrorCode::Conflict,
            "skill.cleanup.not_removed",
        ));
    }
    port.cleanup_native_parents(operation, intent)
}

#[path = "skill_journal.rs"]
mod journal;
pub use journal::*;
