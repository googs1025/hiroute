//! Agent endpoint/model indexing; release authentication belongs to the owning catalog.
use super::filesystem::AgentFilesystemScanError;
use hiroute_domain::{ConnectorRegistryBundleV1, ModelDataBundleV1, UpstreamProtocol};
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub struct ClaudeRegistrationIndexV1 {
    entries: Vec<ClaudeRegistrationEntryV1>,
}

#[derive(Clone)]
pub(super) struct ClaudeRegistrationEntryV1 {
    pub(super) connection_option_id: String,
    pub(super) endpoint_profile_id: String,
    pub(super) endpoint_profile_revision: u64,
    pub(super) model_configuration_id: Option<String>,
    pub(super) base_url: String,
    pub(super) messages_url: String,
    pub(super) upstream_models: BTreeMap<String, String>,
    pub(super) resolved_upstream_model_id: Option<String>,
}

impl ClaudeRegistrationIndexV1 {
    /// Builds a discovery index from the stable subpayload of an ALREADY VERIFIED release.
    /// The owning client-bundled catalog must validate the complete current envelope
    /// before supplying this reference. This function does not authenticate arbitrary input
    /// and never adapts configuration ratings into model-level scores. It checks only
    /// index-relevant consistency.
    pub fn from_verified_model_data(
        registry: &ConnectorRegistryBundleV1,
        model_data: &ModelDataBundleV1,
    ) -> Result<Self, AgentFilesystemScanError> {
        registry
            .validate()
            .map_err(|_| AgentFilesystemScanError::InvalidRegistry)?;
        if model_data.schema != hiroute_domain::MODEL_DATA_SCHEMA_V1
            || model_data.product_release != registry.product_release
            || model_data.connector_registry_version != registry.registry_version
        {
            return Err(AgentFilesystemScanError::InvalidRegistry);
        }
        let mut entries = Vec::new();
        for option in &registry.connection_options {
            let Some(profile) = registry.endpoint_profile(&option.endpoint_profile_id) else {
                return Err(AgentFilesystemScanError::InvalidRegistry);
            };
            for endpoint in profile
                .protocol_endpoints
                .iter()
                .filter(|endpoint| endpoint.protocol == UpstreamProtocol::Messages)
            {
                let messages_url = format!("{}{}", endpoint.base_url, endpoint.request_path);
                let Some(base_url) = messages_url.strip_suffix("/v1/messages") else {
                    return Err(AgentFilesystemScanError::InvalidRegistry);
                };
                if base_url.is_empty() || base_url.ends_with('/') {
                    return Err(AgentFilesystemScanError::InvalidRegistry);
                }
                let mut upstream_models = BTreeMap::new();
                for capability in
                    model_data
                        .model_endpoint_capabilities
                        .iter()
                        .filter(|capability| {
                            capability.endpoint_profile_id == profile.endpoint_profile_id
                                && capability.protocol_endpoint_id == endpoint.protocol_endpoint_id
                        })
                {
                    if capability.upstream_model_id.is_empty()
                        || !model_data.models.iter().any(|model| {
                            model.model_configuration_id == capability.model_configuration_id
                        })
                        || upstream_models
                            .insert(
                                capability.upstream_model_id.clone(),
                                capability.model_configuration_id.clone(),
                            )
                            .is_some()
                    {
                        return Err(AgentFilesystemScanError::InvalidRegistry);
                    }
                }
                entries.push(ClaudeRegistrationEntryV1 {
                    connection_option_id: option.connection_option_id.clone(),
                    endpoint_profile_id: profile.endpoint_profile_id.clone(),
                    endpoint_profile_revision: profile.revision,
                    model_configuration_id: None,
                    base_url: base_url.to_owned(),
                    messages_url,
                    upstream_models,
                    resolved_upstream_model_id: None,
                });
            }
        }
        entries.sort_by(|left, right| {
            (&left.base_url, &left.connection_option_id)
                .cmp(&(&right.base_url, &right.connection_option_id))
        });
        Ok(Self { entries })
    }

    pub(super) fn resolve(
        &self,
        endpoint: &str,
        models: &[String],
    ) -> Option<ClaudeRegistrationEntryV1> {
        let candidate = format!("{endpoint}/v1/messages");
        for model in models {
            let mut matches = self.entries.iter().filter(|entry| {
                entry.messages_url == candidate && entry.upstream_models.contains_key(model)
            });
            let Some(mut found) = matches.next().cloned() else {
                continue;
            };
            if matches.next().is_some() {
                return None;
            }
            found.model_configuration_id = found.upstream_models.get(model).cloned();
            found.resolved_upstream_model_id = Some(model.clone());
            return Some(found);
        }
        None
    }

    pub(super) fn has_endpoint(&self, endpoint: &str) -> bool {
        let candidate = format!("{endpoint}/v1/messages");
        self.entries
            .iter()
            .any(|entry| entry.messages_url == candidate)
    }
}

#[cfg(test)]
mod tests;
