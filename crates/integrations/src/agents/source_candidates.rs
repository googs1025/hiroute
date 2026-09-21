//! Draft source facts are independent of release-catalog registration and model connection.
//! Selection always rereads the scanner's own bounded context. No path/selector API is exposed.
use super::filesystem_config::{ObservedClaudeSettings, same_layer_secret_conflict};
use super::{
    AgentFilesystemScanError, DiscoveredCredentialRefV1, FILESYSTEM_AGENT_SCANNER_ID_V1,
    FILESYSTEM_AGENT_SCANNER_VERSION_V1, FilesystemAgentScannerV1,
};
use hiroute_domain::{AgentIngressProtocolV1, AgentKindV1, CanonicalDigest, ProtectedSecret};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSourceCandidate {
    pub candidate_ref: String,
    pub context_ref: String,
    pub observed_digest: CanonicalDigest,
    /// Origin only: raw paths, userinfo and query parameters are protected import inputs.
    pub endpoint_origin: Option<String>,
    /// This describes what the native client emits, not verified upstream compatibility.
    pub native_ingress: AgentIngressProtocolV1,
    pub authentication: DiscoveredAuthSource,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveredAuthSource {
    EnvironmentToken,
    EnvironmentKey,
    InlineToken,
    NativeSessionNeedsConfirmation,
    HelperNeedsInput,
    Missing,
    Ambiguous,
}

/// Only a privileged source-import port consumes this value; never serialize into Preview.
pub struct ProtectedAgentSource {
    pub endpoint: Zeroizing<String>,
    pub model: Option<Zeroizing<String>>,
    pub credential: Option<ProtectedSecret>,
}

impl FilesystemAgentScannerV1 {
    /// Does not execute a native client, import credentials, invoke a helper or change permissions.
    pub fn claude_source_candidates(
        &self,
    ) -> Result<Vec<AgentSourceCandidate>, AgentFilesystemScanError> {
        Ok(candidate(&self.claude_observations()?)?
            .into_iter()
            .collect())
    }

    pub fn read_selected_claude_source(
        &self,
        selected: &AgentSourceCandidate,
    ) -> Result<ProtectedAgentSource, AgentFilesystemScanError> {
        let observations = self.claude_observations()?;
        if candidate(&observations)?.as_ref() != Some(selected) {
            return Err(AgentFilesystemScanError::SourceChanged);
        }
        let endpoint = effective(&observations, |item| item.settings.env.base_url.is_some())
            .and_then(|item| item.settings.env.base_url.clone())
            .ok_or(AgentFilesystemScanError::SourceUnavailable)?;
        let model = effective(&observations, |item| item.settings.env.model.is_some())
            .and_then(|item| item.settings.env.model.clone())
            .map(Zeroizing::new);
        let auth_field = match selected.authentication {
            DiscoveredAuthSource::EnvironmentToken => Some("ANTHROPIC_AUTH_TOKEN"),
            DiscoveredAuthSource::EnvironmentKey => Some("ANTHROPIC_API_KEY"),
            _ => None,
        };
        let credential = if let Some(auth_field) = auth_field {
            let source = effective(&observations, |item| {
                item.settings
                    .env
                    .present_environment_fields
                    .contains(auth_field)
            })
            .ok_or(AgentFilesystemScanError::SourceChanged)?;
            Some(self.read_discovered_secret(&DiscoveredCredentialRefV1 {
                source: "discovered_config".to_owned(),
                scanner_id: FILESYSTEM_AGENT_SCANNER_ID_V1.to_owned(),
                scanner_version: FILESYSTEM_AGENT_SCANNER_VERSION_V1.to_owned(),
                discovered_source_ref: source.source_ref.clone(),
                field_selector: format!("env.{auth_field}"),
                observed_revision: source.revision,
            })?)
        } else {
            None
        };
        // A second bounded pass detects changes to other consumed layers during secret reread.
        if candidate(&self.claude_observations()?)?.as_ref() != Some(selected) {
            return Err(AgentFilesystemScanError::SourceChanged);
        }
        Ok(ProtectedAgentSource {
            endpoint: Zeroizing::new(endpoint),
            model,
            credential,
        })
    }
}

fn candidate(
    observations: &[ObservedClaudeSettings],
) -> Result<Option<AgentSourceCandidate>, AgentFilesystemScanError> {
    let Some(source) = effective(observations, |item| item.settings.env.base_url.is_some()) else {
        return Ok(None);
    };
    let refs = observations
        .iter()
        .map(|item| item.source_ref.as_str())
        .collect::<Vec<_>>();
    let context_digest = CanonicalDigest::of(&("claude-native-source-context/1", refs))
        .map_err(|_| AgentFilesystemScanError::InvalidConfig)?;
    let context_ref = format!("agent-context/{}", context_digest.as_str());
    let revisions = observations
        .iter()
        .map(|item| (&item.source_ref, &item.digest))
        .collect::<Vec<_>>();
    let observed_digest = CanonicalDigest::of(&(
        FILESYSTEM_AGENT_SCANNER_ID_V1,
        FILESYSTEM_AGENT_SCANNER_VERSION_V1,
        &context_ref,
        revisions,
    ))
    .map_err(|_| AgentFilesystemScanError::InvalidConfig)?;
    let auth = effective(observations, |item| {
        item.settings.api_key_helper_present
            || item
                .settings
                .env
                .present_environment_fields
                .contains("ANTHROPIC_AUTH_TOKEN")
            || item
                .settings
                .env
                .present_environment_fields
                .contains("ANTHROPIC_API_KEY")
    });
    let has = |field: &str| {
        observations
            .iter()
            .any(|item| item.settings.env.present_environment_fields.contains(field))
    };
    let mechanisms = usize::from(has("ANTHROPIC_AUTH_TOKEN"))
        + usize::from(has("ANTHROPIC_API_KEY"))
        + usize::from(
            observations
                .iter()
                .any(|item| item.settings.api_key_helper_present),
        );
    let alternate_auth = [
        "ANTHROPIC_CUSTOM_HEADERS",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ]
    .iter()
    .any(|field| has(field));
    let authentication =
        if same_layer_secret_conflict(observations) || mechanisms > 1 || alternate_auth {
            DiscoveredAuthSource::Ambiguous
        } else if let Some(auth) = auth {
            let fields = &auth.settings.env.present_environment_fields;
            match (
                fields.contains("ANTHROPIC_AUTH_TOKEN"),
                fields.contains("ANTHROPIC_API_KEY"),
                auth.settings.api_key_helper_present,
            ) {
                (true, false, false) => DiscoveredAuthSource::EnvironmentToken,
                (false, true, false) => DiscoveredAuthSource::EnvironmentKey,
                (false, false, true) => DiscoveredAuthSource::HelperNeedsInput,
                _ => DiscoveredAuthSource::Ambiguous,
            }
        } else {
            DiscoveredAuthSource::Missing
        };
    let endpoint_origin = source
        .settings
        .env
        .base_url
        .as_deref()
        .and_then(safe_origin);
    Ok(Some(AgentSourceCandidate {
        candidate_ref: format!("source/claude/{}", context_digest.as_str()),
        context_ref,
        observed_digest,
        endpoint_origin,
        native_ingress: AgentIngressProtocolV1::Messages,
        authentication,
    }))
}

fn effective(
    values: &[ObservedClaudeSettings],
    includes: impl Fn(&ObservedClaudeSettings) -> bool,
) -> Option<&ObservedClaudeSettings> {
    values
        .iter()
        .filter(|value| includes(value))
        .min_by_key(|value| value.layer.precedence_for(AgentKindV1::ClaudeCode))
}

pub(super) fn safe_origin(value: &str) -> Option<String> {
    let uri = value.parse::<http::Uri>().ok()?;
    let scheme = uri.scheme_str()?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let authority = uri.authority()?.as_str();
    if authority.contains('@') {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}
