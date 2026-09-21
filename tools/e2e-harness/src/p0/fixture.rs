use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::Path;

use serde_json::{Value, json};

use super::canonical::canonical_json_digest;
use super::contract::P0Bundle;
use super::types::{
    FIXTURE_SCHEMA, GrantAccess, ORACLE_VERSION, PUBLICATION_SCHEMA, ProviderScript,
};

pub(crate) struct FixtureInputs<'a> {
    pub endpoints: &'a BTreeMap<String, SocketAddr>,
    pub client_authorization: &'a str,
    pub provider_authorization: &'a str,
    pub evidence_dir: &'a Path,
    pub run_nonce: &'a str,
    pub readiness_path: &'a Path,
    pub readiness_nonce: &'a str,
}

pub(crate) fn build(bundle: &P0Bundle, inputs: FixtureInputs<'_>) -> Value {
    let mut routes = Vec::new();
    let mut allowed_aliases = Vec::new();
    let mut plans = Vec::new();
    let mut catalog = Vec::new();
    let mut provider_bindings = Vec::new();
    let mut adapter_protocols = BTreeSet::new();
    for case in &bundle.artifacts().corpus.cases {
        let alias = case.ingress.body["model"]
            .as_str()
            .expect("validated corpus aliases are strings");
        let plan_id = format!("agent-plan-{}", plans.len() + 1);
        let plan_revision = format!("{plan_id}@1");
        if case.grant_access == GrantAccess::Allowed {
            allowed_aliases.push(alias.to_owned());
        }
        let upstream_protocols: Vec<_> = case
            .providers
            .iter()
            .map(|provider| provider.protocol.as_str())
            .collect();
        routes.push(json!({
            "agent_plan_revision": plan_revision,
            "ingress_protocol": case.ingress.protocol.as_str(),
            "model_alias": alias,
            "upstream_protocols": upstream_protocols
        }));
        let candidates = build_candidates(&case.providers);
        plans.push(json!({
            "candidates": candidates,
            "plan_id": plan_id,
            "revision": plan_revision,
            "served_model_id": alias
        }));
        catalog.push(json!({
            "model_alias": alias,
            "plan_revision": plan_revision,
            "purpose": format!("P0 exact {} ingress", case.ingress.protocol.as_str())
        }));
        for provider in &case.providers {
            provider_bindings.push(json!({
                "binding_ref": provider.id,
                "credential_ref": "credential:p0-native-provider",
                "endpoint": format!("http://{}", inputs.endpoints[&provider.id]),
                "protocol": provider.protocol.as_str()
            }));
            adapter_protocols.insert(provider.protocol.as_str());
        }
    }
    let adapters: Vec<_> = adapter_protocols
        .into_iter()
        .map(|protocol| {
            json!({
                "capability_fingerprint": canonical_json_digest(&json!({
                    "protocol": protocol,
                    "streaming": true,
                    "tool_calls": true
                })),
                "descriptor_revision": format!("adapter:{protocol}@1"),
                "protocol": protocol
            })
        })
        .collect();
    let alias_index: Vec<_> = routes
        .iter()
        .map(|route| {
            json!({
                "agent_plan_revision": route["agent_plan_revision"],
                "model_alias": route["model_alias"]
            })
        })
        .collect();
    let mut publication = json!({
        "schema_version": PUBLICATION_SCHEMA,
        "authority": {
            "issuer": "hiroute-control-plane",
            "subject": "workspace:p0",
            "policy_revision": "authority:p0@1"
        },
        "epoch": 7,
        "revision": "gateway-publication@7",
        "protocol_routes": routes,
        "alias_index": alias_index,
        "grant_index": [{
            "allowed_aliases": allowed_aliases,
            "grant_ref": "grant:p0-client"
        }],
        "agent_plans": plans,
        "adapter_catalog": adapters,
        "model_catalog": {
            "entries": catalog,
            "revision": "model-catalog@1"
        }
    });
    let payload_digest = canonical_json_digest(&publication);
    publication
        .as_object_mut()
        .expect("publication is an object")
        .insert("payload_digest".into(), Value::String(payload_digest));
    json!({
        "schema_version": FIXTURE_SCHEMA,
        "oracle_version": ORACLE_VERSION,
        "contract_digest": bundle.artifacts().manifest.contract_digest,
        "publication": publication,
        "provider_bindings": provider_bindings,
        "secret_store": {
            "client_grants": {"grant:p0-client": inputs.client_authorization},
            "credentials": {"credential:p0-native-provider": inputs.provider_authorization}
        },
        "observation": {
            "directory": inputs.evidence_dir,
            "run_nonce": inputs.run_nonce,
            "terminal_receipt": inputs.evidence_dir.join("terminal-receipt.json")
        },
        "readiness": {
            "nonce": inputs.readiness_nonce,
            "path": inputs.readiness_path
        }
    })
}

fn build_candidates(providers: &[ProviderScript]) -> Vec<Value> {
    providers
        .iter()
        .enumerate()
        .map(|(index, provider)| {
            json!({
                "adapter_ref": format!("adapter:{}@1", provider.protocol.as_str()),
                "binding_ref": provider.id,
                "capability_fingerprint": canonical_json_digest(&provider.expected_request.body),
                "model_id": provider.expected_request.body["model"],
                "ordinal": index + 1,
                "protocol": provider.protocol.as_str()
            })
        })
        .collect()
}

pub(crate) fn publication_digest(fixture: &Value) -> Option<&str> {
    fixture["publication"]["payload_digest"].as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p0::types::{NativeRequestExpectation, Protocol, ProviderBody, ProviderResponse};

    fn provider(id: &str, protocol: Protocol) -> ProviderScript {
        ProviderScript {
            id: id.into(),
            protocol,
            expected_calls: 0,
            expected_request: NativeRequestExpectation {
                path: protocol.provider_path().into(),
                body: json!({"model": format!("model-{id}")}),
            },
            response: ProviderResponse {
                status: 200,
                content_type: "application/json".into(),
                body: ProviderBody::Json {
                    value: json!({"challenge": "${RUN_CHALLENGE}"}),
                },
            },
        }
    }

    #[test]
    fn publication_plan_enumerates_every_candidate_in_order() {
        let candidates = build_candidates(&[
            provider("primary", Protocol::Messages),
            provider("fallback", Protocol::Responses),
        ]);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0]["binding_ref"], "primary");
        assert_eq!(candidates[0]["ordinal"], 1);
        assert_eq!(candidates[1]["binding_ref"], "fallback");
        assert_eq!(candidates[1]["ordinal"], 2);
    }
}
