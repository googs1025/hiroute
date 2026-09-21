use serde::{Deserialize, Serialize};

use crate::server::request_plan::IngressProtocol;

use super::CriticalFact;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthenticationSemantics {
    Bearer,
    ApiKeyHeader { header: String },
    None,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderSemantics {
    pub content_type: String,
    pub required_headers: Vec<(String, String)>,
    pub forbidden_forward_headers: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorSemantics {
    pub http_status_typed: bool,
    pub sse_error_typed: bool,
    pub retry_after_header: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorProfile {
    pub schema_version: String,
    /// Stable catalog identity for the provider implementation.
    pub provider_id: String,
    /// Exact endpoint identity, not merely a URL selected later at runtime.
    pub endpoint_id: String,
    /// Exact entitlement/credential-class identity authorized for this path.
    pub entitlement_id: String,
    pub connector_id: String,
    pub connector_revision: String,
    pub upstream_protocol: IngressProtocol,
    pub request_path: String,
    pub authentication: CriticalFact<AuthenticationSemantics>,
    pub headers: CriticalFact<HeaderSemantics>,
    pub errors: CriticalFact<ErrorSemantics>,
}

impl ConnectorProfile {
    pub fn exact_json(
        connector_id: impl Into<String>,
        revision: impl Into<String>,
        protocol: IngressProtocol,
    ) -> Self {
        let connector_id = connector_id.into();
        Self {
            schema_version: "hiroute.connector-profile/v1".into(),
            provider_id: format!("fixture-provider-{connector_id}"),
            endpoint_id: format!("fixture-endpoint-{connector_id}"),
            entitlement_id: format!("fixture-entitlement-{connector_id}"),
            connector_id,
            connector_revision: revision.into(),
            upstream_protocol: protocol,
            request_path: protocol.path().into(),
            authentication: CriticalFact::Exact(AuthenticationSemantics::Bearer),
            headers: CriticalFact::Exact(HeaderSemantics {
                content_type: "application/json".into(),
                required_headers: Vec::new(),
                forbidden_forward_headers: vec!["authorization".into()],
            }),
            errors: CriticalFact::Exact(ErrorSemantics {
                http_status_typed: true,
                sse_error_typed: true,
                retry_after_header: Some("retry-after".into()),
            }),
        }
    }

    pub fn critical_facts_are_exact(&self) -> bool {
        let authentication_exact =
            self.authentication
                .exact()
                .is_some_and(|authentication| match authentication {
                    AuthenticationSemantics::Bearer | AuthenticationSemantics::None => true,
                    AuthenticationSemantics::ApiKeyHeader { header } => !header.trim().is_empty(),
                });
        let headers_exact = self.headers.exact().is_some_and(|headers| {
            headers.content_type == "application/json"
                && headers.required_headers.iter().all(|(name, value)| {
                    !name.trim().is_empty()
                        && !value.trim().is_empty()
                        && !name.eq_ignore_ascii_case("authorization")
                })
                && headers
                    .forbidden_forward_headers
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case("authorization"))
        });
        let errors_exact = self.errors.exact().is_some_and(|errors| {
            errors.http_status_typed
                && errors.sse_error_typed
                && errors
                    .retry_after_header
                    .as_ref()
                    .is_none_or(|header| !header.trim().is_empty())
        });
        self.schema_version == "hiroute.connector-profile/v1"
            && !self.provider_id.trim().is_empty()
            && !self.endpoint_id.trim().is_empty()
            && !self.entitlement_id.trim().is_empty()
            && !self.connector_id.trim().is_empty()
            && !self.connector_revision.trim().is_empty()
            && self.request_path.starts_with('/')
            && self.request_path.len() > 1
            && !self.request_path.contains(['?', '#'])
            && authentication_exact
            && headers_exact
            && errors_exact
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_registered_provider_path_need_not_be_the_public_ingress_path() {
        let mut profile =
            ConnectorProfile::exact_json("connector.zhipu.p0", "1", IngressProtocol::Messages);
        profile.request_path = "/api/anthropic/v1/messages".into();
        assert!(profile.critical_facts_are_exact());

        profile.request_path = "https://attacker.invalid/v1/messages".into();
        assert!(!profile.critical_facts_are_exact());
    }
}
