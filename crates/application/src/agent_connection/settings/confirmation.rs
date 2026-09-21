//! Revalidate the exact independent-facet Preview immediately before existing Operation sealing.
use super::{AgentSettingsFacts, AgentSettingsPreview, preview_agent_settings};
use hiroute_application_api::{AgentLoginItemDeclarationV2, AgentSettingsApplyV2};

pub struct ConfirmedAgentSettings {
    preview: AgentSettingsPreview,
    idempotency_key: String,
    login_item: Option<AgentLoginItemDeclarationV2>,
}
impl ConfirmedAgentSettings {
    pub fn preview(&self) -> &AgentSettingsPreview {
        &self.preview
    }
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }
    /// The host-declared login-item observation carried by this apply, if any.
    pub fn login_item(&self) -> Option<&AgentLoginItemDeclarationV2> {
        self.login_item.as_ref()
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SettingsConfirmationError {
    #[error("Agent settings confirmation is invalid")]
    Invalid,
    #[error("Agent settings changed since Preview")]
    Stale,
    #[error("Agent settings have unresolved action blockers")]
    Blocked,
}

/// Caller captures fresh native/Plan/permit facts at the existing Operation admission boundary.
/// This validates intent only: it does not confer management authority, acquire a writer/gate,
/// consume the one-shot Apply capability, or create a second transaction coordinator.
pub fn confirm_agent_settings(
    request: AgentSettingsApplyV2,
    fresh: &AgentSettingsFacts,
) -> Result<ConfirmedAgentSettings, SettingsConfirmationError> {
    hiroute_domain::validate_idempotency_key(&request.idempotency_key)
        .map_err(|_| SettingsConfirmationError::Invalid)?;
    if request.dependency_digest != fresh.dependency_digest {
        return Err(SettingsConfirmationError::Stale);
    }
    let preview = preview_agent_settings(request.spec, fresh)
        .map_err(|_| SettingsConfirmationError::Invalid)?;
    if request.accept_digest != preview.accept_digest {
        return Err(SettingsConfirmationError::Stale);
    }
    if !preview.blockers.is_empty() {
        return Err(SettingsConfirmationError::Blocked);
    }
    Ok(ConfirmedAgentSettings {
        preview,
        idempotency_key: request.idempotency_key,
        login_item: request.login_item,
    })
}
