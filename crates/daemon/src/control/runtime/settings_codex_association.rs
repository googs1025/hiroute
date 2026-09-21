//! Codex native-source and active-binding association checks used by settings fact capture.

use hiroute_domain::{ComputeManagementProvenanceV2, SupportedAgentInstallationV1};

pub(super) fn endpoint_matches_target(
    endpoint: &str,
    target: &hiroute_domain::ComputeManagementTargetV2,
) -> bool {
    let Ok(uri) = endpoint.parse::<http::Uri>() else {
        return false;
    };
    let Some(scheme) = uri.scheme_str() else {
        return false;
    };
    let Some(authority) = uri.authority() else {
        return false;
    };
    if authority.as_str().contains('@') || uri.query().is_some() {
        return false;
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    let base_path = uri.path().trim_end_matches('/');
    let responses_path = format!("{base_path}/responses");
    scheme == target.scheme
        && authority.host() == target.authority
        && authority.port_u16().unwrap_or(default_port) == target.port
        && responses_path == target.request_path
        && target.upstream_protocol == hiroute_domain::UpstreamProtocol::Responses
}

pub(super) fn connector_account_matches(
    provenance: &ComputeManagementProvenanceV2,
    account_ref: &str,
) -> bool {
    matches!(
        provenance,
        ComputeManagementProvenanceV2::ConnectorOwned {
            connector_id,
            account_ref: saved,
            ..
        } if connector_id == crate::control::runtime::subscriptions::CONNECTOR_ID
            && saved == account_ref
    )
}

/// The evidence stays bound to this scan's observation digest; the settings snapshot rebinds
/// it to the settings dependency digest below.
pub(super) fn mark_catalog_structure_proven(installation: &mut SupportedAgentInstallationV1) {
    for proof in &mut installation.capability_evidence {
        if proof.capability == hiroute_domain::AgentCapability::ModelCatalog {
            proof.state = hiroute_domain::CapabilityState::Proven;
            proof.reason = None;
            proof.adapter_contract = "hiroute.codex-catalog/v1".into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hiroute_domain::{ComputeManagementTargetV2, UpstreamProtocol};

    fn responses_target() -> ComputeManagementTargetV2 {
        ComputeManagementTargetV2 {
            scheme: "https".into(),
            authority: "api.openai.com".into(),
            port: 443,
            request_path: "/v1/responses".into(),
            upstream_protocol: UpstreamProtocol::Responses,
            protocol_profile_id: "protocol.openai.responses.v1".into(),
            protocol_profile_revision: 1,
        }
    }

    #[test]
    fn codex_api_base_matches_only_its_exact_responses_target() {
        let target = responses_target();
        assert!(endpoint_matches_target(
            "https://api.openai.com/v1",
            &target
        ));
        assert!(endpoint_matches_target(
            "https://api.openai.com:443/v1/",
            &target
        ));
        assert!(!endpoint_matches_target(
            "https://api.openai.com/v2",
            &target
        ));
        assert!(!endpoint_matches_target(
            "https://other.openai.com/v1",
            &target
        ));
        assert!(!endpoint_matches_target(
            "https://api.openai.com/v1?tenant=other",
            &target
        ));
    }

    #[test]
    fn codex_subscription_account_never_crosses_connector_or_account() {
        let provenance = ComputeManagementProvenanceV2::ConnectorOwned {
            connector_id: crate::control::runtime::subscriptions::CONNECTOR_ID.into(),
            account_ref: "account/current".into(),
        };
        assert!(connector_account_matches(&provenance, "account/current"));
        assert!(!connector_account_matches(&provenance, "account/other"));
        let other_connector = ComputeManagementProvenanceV2::ConnectorOwned {
            connector_id: "connector.cpa.other".into(),
            account_ref: "account/current".into(),
        };
        assert!(!connector_account_matches(
            &other_connector,
            "account/current"
        ));
    }
}
