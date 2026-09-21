//! Shared physical Skill ownership; collaboration grants remain per context.
//! The existing Operation journals the record and file effects. Revocation must precede cleanup.
use hiroute_domain::CanonicalDigest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[path = "skill_effects.rs"]
mod effects;
pub use effects::*;
#[path = "skill_reference.rs"]
mod reference;
pub use reference::*;

pub use hiroute_domain::{
    CollaborationSkillFileOwnership, MANAGED_COLLABORATION_SKILL_SCHEMA, ManagedCollaborationSkill,
};
impl From<hiroute_domain::InvalidSkillRecord> for SkillPlanningError {
    fn from(_: hiroute_domain::InvalidSkillRecord) -> Self {
        Self::InvalidRecord
    }
}

/// Content is supplied by MVP-20's compiled bundle, never a scanner or WebView payload.
pub struct CollaborationSkillTemplate {
    pub revision: &'static str,
    pub content: &'static str,
    pub digest: CanonicalDigest,
}
impl CollaborationSkillTemplate {
    pub fn bundled(
        revision: &'static str,
        content: &'static str,
    ) -> Result<Self, SkillPlanningError> {
        if !identity(revision)
            || content.is_empty()
            || content.len() > 16 * 1024
            || content.contains('\0')
        {
            return Err(SkillPlanningError::InvalidRecord);
        }
        Ok(Self {
            revision,
            content,
            digest: CanonicalDigest::of_bytes(content.as_bytes()),
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillFileAction {
    Keep,
    Install,
    Remove,
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillOwnershipPlan {
    pub next: ManagedCollaborationSkill,
    pub file_action: SkillFileAction,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SkillPlanningError {
    #[error("Skill ownership record or context is invalid")]
    InvalidRecord,
    #[error("a user-owned Skill exists at the target")]
    UnmanagedFile,
    #[error("managed Skill file has changed; re-preview the conflict")]
    FileChanged,
}

pub fn plan_skill_install(
    root_ref: &str,
    context: &str,
    template: &CollaborationSkillTemplate,
    current: Option<&ManagedCollaborationSkill>,
    observed_file: Option<&CanonicalDigest>,
) -> Result<SkillOwnershipPlan, SkillPlanningError> {
    if !identity(root_ref)
        || !identity(context)
        || !identity(template.revision)
        || CanonicalDigest::of_bytes(template.content.as_bytes()) != template.digest
    {
        return Err(SkillPlanningError::InvalidRecord);
    }
    let mut next = if let Some(current) = current {
        current.validate()?;
        if current.root_ref != root_ref {
            return Err(SkillPlanningError::InvalidRecord);
        }
        if observed_file.is_some_and(|digest| digest != &current.content_digest) {
            return Err(SkillPlanningError::FileChanged);
        }
        if current.file_ownership == CollaborationSkillFileOwnership::BorrowedIdentical
            && current.content_digest != template.digest
        {
            return Err(SkillPlanningError::FileChanged);
        }
        current.clone()
    } else {
        if observed_file.is_some_and(|digest| digest != &template.digest) {
            return Err(SkillPlanningError::UnmanagedFile);
        }
        ManagedCollaborationSkill {
            schema: MANAGED_COLLABORATION_SKILL_SCHEMA.into(),
            root_ref: root_ref.into(),
            template_revision: template.revision.into(),
            content_digest: template.digest.clone(),
            file_ownership: if observed_file == Some(&template.digest) {
                CollaborationSkillFileOwnership::BorrowedIdentical
            } else {
                CollaborationSkillFileOwnership::Managed
            },
            contexts: BTreeSet::new(),
            revision: 1,
            file_effect: None,
        }
    };
    next.contexts.insert(context.to_owned());
    next.template_revision = template.revision.to_owned();
    next.content_digest = template.digest.clone();
    if let Some(current) = current
        && (&next != current || observed_file != Some(&template.digest))
    {
        next.revision = current
            .revision
            .checked_add(1)
            .ok_or(SkillPlanningError::InvalidRecord)?;
    }
    next.validate()?;
    Ok(SkillOwnershipPlan {
        next,
        file_action: if observed_file == Some(&template.digest) {
            SkillFileAction::Keep
        } else {
            SkillFileAction::Install
        },
    })
}

/// This plan never re-adds authorization when a file is changed. The zero-reference record is
/// retained for the original Operation's cleanup/reconcile; actual grant/token revocation is 14/20.
pub fn plan_skill_remove(
    context: &str,
    current: &ManagedCollaborationSkill,
    observed_file: Option<&CanonicalDigest>,
) -> Result<SkillOwnershipPlan, SkillPlanningError> {
    current.validate()?;
    if !identity(context) {
        return Err(SkillPlanningError::InvalidRecord);
    }
    let mut next = current.clone();
    if next.contexts.remove(context) {
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or(SkillPlanningError::InvalidRecord)?;
    }
    let record_changed = &next != current;
    let file_action = if next.file_ownership == CollaborationSkillFileOwnership::BorrowedIdentical {
        // HiRoute owns only the reference. User edits or deletion can make the active
        // collaboration drift, but must never prevent releasing that reference.
        SkillFileAction::Keep
    } else if observed_file == Some(&next.content_digest) {
        if next.contexts.is_empty()
            && next.file_ownership == CollaborationSkillFileOwnership::Managed
        {
            SkillFileAction::Remove
        } else {
            SkillFileAction::Keep
        }
    } else if observed_file.is_none() && !record_changed {
        SkillFileAction::Keep
    } else {
        SkillFileAction::Conflict
    };
    if file_action == SkillFileAction::Remove && next.revision == current.revision {
        next.revision = current
            .revision
            .checked_add(1)
            .ok_or(SkillPlanningError::InvalidRecord)?;
    }
    Ok(SkillOwnershipPlan { next, file_action })
}

fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./:-".contains(&byte))
}

#[cfg(test)]
mod tests;
