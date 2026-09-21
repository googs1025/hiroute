use std::collections::BTreeMap;

use hiroute_domain::{
    AgentIngressProtocolV1, AgentKindV1, BuiltInAgentProbeV1, CanonicalDigest, ConfigLayerV1,
    ModelAlias,
};
use serde_json::json;

use super::*;

fn observation(kind: AgentKindV1, version: &str) -> AgentScanObservationV1 {
    AgentScanObservationV1 {
        schema: AGENT_SCAN_OBSERVATION_SCHEMA_V1.to_owned(),
        agent_id: format!("agent/{}", kind.as_str()),
        kind,
        version: version.to_owned(),
        config: Vec::new(),
    }
}

#[test]
fn agents_registry_binds_codex_to_responses_and_claude_to_messages() {
    let profiles = builtin_agent_profiles();
    assert_eq!(profiles.len(), 2);
    for profile in &profiles {
        profile.validate().unwrap();
    }
    assert_eq!(
        codex_profile_v1().client_protocol(),
        AgentIngressProtocolV1::Responses
    );
    assert_eq!(
        claude_code_profile_v1().client_protocol(),
        AgentIngressProtocolV1::Messages
    );
    if std::env::var_os("HIROUTE_PRINT_AGENT_DIGESTS").is_some() {
        println!(
            "codex_profile_digest={}",
            CanonicalDigest::of(&codex_profile_v1()).unwrap()
        );
        println!(
            "claude_profile_digest={}",
            CanonicalDigest::of(&claude_code_profile_v1()).unwrap()
        );
    }
}

#[test]
fn agents_unknown_versions_preserve_installation_but_do_not_fabricate_action_proof() {
    let outcome = resolve_agent_observation(observation(AgentKindV1::Codex, "0.116.1"));
    let AgentDiscoveryOutcomeV1::Supported { installation } = outcome else {
        panic!("a diagnostic version must not hide a parsed installation");
    };
    assert_eq!(installation.version, "0.116.1");
    assert!(
        installation
            .require_action(hiroute_domain::AgentAction::ConfigureModel)
            .is_err()
    );
}

#[test]
fn agents_effective_config_uses_exact_precedence_and_blocks_conflicts() {
    let mut observed = observation(AgentKindV1::Codex, "diagnostic-build");
    observed.config = vec![
        ConfigObservationV1 {
            path: "model_provider".to_owned(),
            layer: ConfigLayerV1::User,
            value: json!("hiroute"),
            source_digest: CanonicalDigest::of_bytes(b"user"),
        },
        ConfigObservationV1 {
            path: "model_provider".to_owned(),
            layer: ConfigLayerV1::Process,
            value: json!("other"),
            source_digest: CanonicalDigest::of_bytes(b"process"),
        },
    ];
    let AgentDiscoveryOutcomeV1::Supported { installation } = resolve_agent_observation(observed)
    else {
        panic!("diagnostic version must resolve");
    };
    assert_eq!(
        installation.effective_config["model_provider"].layer,
        ConfigLayerV1::Process
    );
    assert_eq!(
        installation
            .writable_values(&BTreeMap::from([(
                "model_provider".to_owned(),
                Some(json!("hiroute"))
            )]))
            .unwrap_err(),
        AgentDiscoveryError::HigherPrecedenceConflict
    );
    assert!(
        installation
            .writable_values(&BTreeMap::from([(
                "model_provider".to_owned(),
                Some(json!("other"))
            )]))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn agents_codex_and_claude_emulators_have_closed_native_shapes() {
    for profile in [codex_profile_v1(), claude_code_profile_v1()] {
        let probe = BuiltInAgentProbeV1::for_profile(
            &profile,
            ModelAlias::parse("hiroute/0011223344556677").unwrap(),
        )
        .unwrap();
        let rendered = render_agent_probe(&profile, &probe).unwrap();
        assert_eq!(rendered.protocol, profile.client_protocol());
        assert_eq!(
            rendered.payload["metadata"]["traffic_kind"],
            "connectivity_probe"
        );
        assert_eq!(rendered.payload["tools"], json!([]));
        match rendered.protocol {
            AgentIngressProtocolV1::Responses => {
                assert!(rendered.payload.get("input").is_some());
                assert!(rendered.payload.get("messages").is_none());
            }
            AgentIngressProtocolV1::Messages => {
                assert!(rendered.payload.get("messages").is_some());
                assert!(rendered.payload.get("input").is_none());
            }
        }
    }
}

#[test]
fn agents_catalog_source_is_dynamic_or_static_never_both() {
    let profile = codex_profile_v1();
    assert_eq!(
        profile.catalog_delivery(true),
        Some(hiroute_domain::CatalogDeliveryV1::DynamicWithEtag)
    );
    assert_eq!(
        profile.catalog_delivery(false),
        Some(hiroute_domain::CatalogDeliveryV1::StaticRestartRequired)
    );
}

#[test]
fn agents_claude_launch_overrides_user_settings() {
    let mut observed = observation(AgentKindV1::ClaudeCode, CLAUDE_CODE_VERIFIED_VERSION_V1);
    observed.config = vec![
        ConfigObservationV1 {
            path: "env.ANTHROPIC_BASE_URL".to_owned(),
            layer: ConfigLayerV1::User,
            value: json!("http://127.0.0.1:5837/v1"),
            source_digest: CanonicalDigest::of_bytes(b"claude-user"),
        },
        ConfigObservationV1 {
            path: "env.ANTHROPIC_BASE_URL".to_owned(),
            layer: ConfigLayerV1::Launch,
            value: json!("http://127.0.0.1:9999/v1"),
            source_digest: CanonicalDigest::of_bytes(b"claude-launch"),
        },
    ];
    let AgentDiscoveryOutcomeV1::Supported { installation } = resolve_agent_observation(observed)
    else {
        panic!("exact Claude Code version must resolve");
    };
    assert_eq!(
        installation.effective_config["env.ANTHROPIC_BASE_URL"].layer,
        ConfigLayerV1::Launch
    );
}

#[test]
fn agents_claude_managed_policy_blocks_lower_layer_writes() {
    let mut observed = observation(AgentKindV1::ClaudeCode, CLAUDE_CODE_VERIFIED_VERSION_V1);
    observed.config = [
        ConfigLayerV1::User,
        ConfigLayerV1::Launch,
        ConfigLayerV1::Process,
        ConfigLayerV1::Managed,
    ]
    .into_iter()
    .map(|layer| ConfigObservationV1 {
        path: "env.ANTHROPIC_BASE_URL".to_owned(),
        layer,
        value: json!(if layer == ConfigLayerV1::Managed {
            "https://policy.example"
        } else {
            "https://user.example"
        }),
        source_digest: CanonicalDigest::of_bytes(format!("{layer:?}").as_bytes()),
    })
    .collect();
    let AgentDiscoveryOutcomeV1::Supported { installation } = resolve_agent_observation(observed)
    else {
        panic!("valid configuration must resolve");
    };
    assert_eq!(
        installation.effective_config["env.ANTHROPIC_BASE_URL"].layer,
        ConfigLayerV1::Managed
    );
    assert_eq!(
        installation
            .writable_values(&BTreeMap::from([(
                "env.ANTHROPIC_BASE_URL".to_owned(),
                Some(json!("http://127.0.0.1:5837"))
            )]))
            .unwrap_err(),
        AgentDiscoveryError::HigherPrecedenceConflict
    );
}

#[test]
fn agents_invalid_owned_observation_never_becomes_effective() {
    let mut observed = observation(AgentKindV1::Codex, "diagnostic-build");
    observed.config.push(ConfigObservationV1 {
        path: "model".to_owned(),
        layer: ConfigLayerV1::User,
        value: json!("unsafe\u{0}model"),
        source_digest: CanonicalDigest::of_bytes(b"invalid"),
    });
    assert!(matches!(
        resolve_agent_observation(observed),
        AgentDiscoveryOutcomeV1::ReportOnly {
            reason: AgentReportOnlyReasonV1::InvalidObservation,
            ..
        }
    ));
}

#[test]
fn agents_conflicting_same_layer_is_report_only() {
    let mut observed = observation(AgentKindV1::Codex, "diagnostic-build");
    observed.config = vec![
        ConfigObservationV1 {
            path: "model_provider".to_owned(),
            layer: ConfigLayerV1::User,
            value: json!("hiroute"),
            source_digest: CanonicalDigest::of_bytes(b"first"),
        },
        ConfigObservationV1 {
            path: "model_provider".to_owned(),
            layer: ConfigLayerV1::User,
            value: json!("other"),
            source_digest: CanonicalDigest::of_bytes(b"second"),
        },
    ];
    assert!(matches!(
        resolve_agent_observation(observed),
        AgentDiscoveryOutcomeV1::ReportOnly {
            reason: AgentReportOnlyReasonV1::ConflictingEffectiveConfig,
            ..
        }
    ));
}

#[test]
fn agents_unknown_observation_schema_is_report_only() {
    let mut observed = observation(AgentKindV1::Codex, "diagnostic-build");
    observed.schema = "hiroute.agent-scan-observation/v2".to_owned();
    assert!(matches!(
        resolve_agent_observation(observed),
        AgentDiscoveryOutcomeV1::ReportOnly {
            reason: AgentReportOnlyReasonV1::UnknownObservationSchema,
            ..
        }
    ));
}

#[test]
fn agents_emulator_response_parser_is_protocol_exact_and_rejects_tools() {
    let codex = codex_profile_v1();
    let responses = json!({
        "output": [{
            "content": [{
                "text": hiroute_domain::CONNECTIVITY_PROBE_RESPONSE_V1,
                "type": "output_text"
            }],
            "type": "message"
        }]
    });
    assert!(
        parse_agent_probe_response(&codex, &responses)
            .unwrap()
            .ready
    );
    assert!(parse_agent_probe_response(&claude_code_profile_v1(), &responses).is_err());

    let with_tool = json!({
        "output": [{
            "content": [{
                "text": hiroute_domain::CONNECTIVITY_PROBE_RESPONSE_V1,
                "type": "output_text"
            }],
            "type": "message"
        }],
        "tool_call": {"name": "unexpected"}
    });
    assert!(parse_agent_probe_response(&codex, &with_tool).is_err());
}
