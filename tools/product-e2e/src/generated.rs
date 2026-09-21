use std::collections::BTreeMap;

use hiroute_application_api::{
    CanonicalDigest, PRODUCT_CONTRACT_REVISION, PROPOSAL_MAP_REVISION, descriptor_digest,
    planned_commands, released_commands, staged_control_commands,
};
use serde_json::{Value, json};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedProductContractFile {
    pub relative_path: &'static str,
    pub contents: String,
}

fn pretty(value: &Value) -> String {
    // Workspace feature unification enables serde_json/preserve_order through ACP.
    // Published contract bytes must remain identical to the default dependency graph.
    let value = hiroute_domain::canonicalize_json(value.clone());
    let mut rendered =
        serde_json::to_string_pretty(&value).expect("static Product E2E contract is serializable");
    rendered.push('\n');
    rendered
}

fn product_action_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "hiroute://e2e/product/schema/product-action.v1.schema.json",
        "title": "HiRoute typed Product action v1",
        "oneOf": [
            action("start_formal_daemon", json!({ "role": { "const": "all" }, "proof": proof_schema() })),
            action("run_cli", json!({
                "command_id": { "type": "string", "minLength": 1 },
                "polarity": { "enum": ["positive", "negative"] },
                "fixture_ref": { "type": "string", "pattern": "^[A-Za-z0-9._-]+$" },
                "proof": proof_schema()
            })),
            action("control_api_probe", json!({
                "principal": { "enum": ["current", "other"] },
                "capability": { "enum": ["missing", "wrong", "revoked", "valid"] },
                "operation": { "enum": ["query", "command"] },
                "proof": proof_schema()
            })),
            action("snapshot_side_effects", json!({
                "label": { "type": "string", "pattern": "^[A-Za-z0-9._-]+$" },
                "supports": assertion_array()
            })),
            action("assert_no_side_effects", json!({
                "before_label": { "type": "string", "pattern": "^[A-Za-z0-9._-]+$" },
                "after_label": { "type": "string", "pattern": "^[A-Za-z0-9._-]+$" },
                "proof": proof_schema()
            })),
            action("inject_daemon_fault", json!({
                "fault": { "enum": ["journal", "bridge", "publication", "agent_write", "rollback", "probe_lease"] },
                "supports": assertion_array()
            })),
            action("restart_formal_daemon", json!({ "role": { "const": "all" }, "proof": proof_schema() }))
        ]
    })
}

fn action(name: &str, properties: Value) -> Value {
    let mut properties = properties.as_object().cloned().expect("properties object");
    properties.insert("action".to_owned(), json!({ "const": name }));
    let mut required = properties.keys().cloned().collect::<Vec<_>>();
    required.sort();
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    })
}

fn proof_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["proves", "expected_evidence_digest"],
        "properties": {
            "proves": assertion_array(),
            "expected_evidence_digest": digest_schema()
        }
    })
}

fn assertion_array() -> Value {
    json!({
        "type": "array",
        "minItems": 1,
        "uniqueItems": true,
        "items": {
            "enum": [
                "formal_daemon_started", "real_cli_used", "local_control_boundary_used",
                "descriptor_digest_exact", "version_mismatch_fails_closed",
                "preview_has_zero_side_effects", "apply_rejection_has_zero_side_effects",
                "reserved_operation_fails_closed", "native_payload_exact", "golden_exact"
            ]
        }
    })
}

fn digest_schema() -> Value {
    json!({ "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" })
}

fn oracle_evidence_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "hiroute://e2e/product/schema/product-oracle-evidence.v1.schema.json",
        "title": "HiRoute Product Oracle evidence v1",
        "type": "object",
        "additionalProperties": false,
        "required": [
            "schema_version", "mode", "scenario_id", "descriptor_digest", "assertions",
            "daemon", "boundary", "control_probes", "cli_invocations", "side_effects", "goldens",
            "native_payloads"
        ],
        "properties": {
            "schema_version": {
                "type": "object", "additionalProperties": false,
                "required": ["major", "minor"],
                "properties": { "major": { "const": 1 }, "minor": { "type": "integer", "minimum": 0 } }
            },
            "mode": { "enum": ["product_evaluation", "adversarial_self_test"] },
            "scenario_id": { "type": "string", "pattern": "^[A-Za-z0-9._-]+$" },
            "descriptor_digest": digest_schema(),
            "assertions": assertion_array(),
            "daemon": { "type": ["object", "null"] },
            "boundary": { "enum": ["local_control_v1", "internal_application_seam", "direct_storage", null] },
            "control_probes": { "type": "array", "items": { "type": "object" } },
            "cli_invocations": { "type": "array", "items": { "type": "object" } },
            "side_effects": { "type": "array", "items": { "type": "object" } },
            "goldens": { "type": "array", "items": { "type": "object" } },
            "native_payloads": { "type": "array", "items": { "type": "object" } }
        }
    })
}

fn product_result_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "hiroute://e2e/product/schema/product-result.v1.schema.json",
        "title": "HiRoute Product Oracle result v1",
        "oneOf": [
            {
                "type": "object", "additionalProperties": false,
                "required": ["schema_version", "scope", "scenario_id", "descriptor_digest", "verified_assertions"],
                "properties": {
                    "schema_version": { "type": "object" },
                    "scope": { "enum": ["product_evaluation", "oracle_self_test_only"] },
                    "scenario_id": { "type": "string" },
                    "descriptor_digest": digest_schema(),
                    "verified_assertions": assertion_array()
                }
            },
            {
                "type": "array", "minItems": 1,
                "items": {
                    "type": "object", "additionalProperties": false,
                    "required": ["code", "anchor", "details"],
                    "properties": {
                        "code": { "type": "string", "pattern": "^[A-Z][A-Z0-9_]+$" },
                        "anchor": { "type": "string", "minLength": 1 },
                        "details": { "type": "string", "minLength": 1 }
                    }
                }
            }
        ]
    })
}

fn dependency_gate() -> Value {
    json!({
        "schema_version": { "major": 1, "minor": 0 },
        "edges": {
            "hiroute-domain": [],
            "hiroute-application-api": ["hiroute-domain"],
            "hiroute-application": ["hiroute-application-api", "hiroute-domain"],
            "hiroute-cli": ["hiroute-application-api", "hiroute-integrations"],
            "hiroute-daemon": [
                "hiroute-application", "hiroute-application-api", "hiroute-domain",
                "hiroute-integrations", "hiroute-local-storage", "hiroute-observation"
            ],
            "hiroute-local-storage": ["hiroute-domain"],
            "hiroute-integrations": ["hiroute-application", "hiroute-application-api", "hiroute-domain"],
            "hiroute-observation": ["hiroute-domain"],
            "hiroute-gateway": ["hiroute-domain"],
            "hiroute-product-e2e": ["hiroute-application-api", "hiroute-domain"]
        },
        "forbidden": [
            "cli_to_application_implementation", "cli_to_storage", "product_to_gateway_internals",
            "gateway_to_application", "gateway_to_storage", "domain_to_adapter"
        ]
    })
}

fn oracle_manifest() -> Value {
    json!({
        "schema_version": { "major": 1, "minor": 0 },
        "product_contract_revision": PRODUCT_CONTRACT_REVISION,
        "proposal_map_revision": PROPOSAL_MAP_REVISION,
        "descriptor_digest": descriptor_digest(),
        "release_state": "released",
        "expected_red_reasons": [],
        "planned_commands": planned_commands().into_iter().map(|command| command.command_id).collect::<Vec<_>>(),
        "public_commands": released_commands().into_iter().map(|command| command.command_id).collect::<Vec<_>>(),
        "staged_control_commands": staged_control_commands().into_iter().map(|command| command.command_id).collect::<Vec<_>>(),
        "planned_native_paths": ["r_to_r", "r_to_c", "r_to_m", "m_to_r", "m_to_c", "m_to_m"],
        "required_adversaries": [
            "formal_daemon_missing", "typed_assertion_deleted", "golden_corrupt",
            "native_payload_wrong", "side_effect_changed", "raw_secret_present",
            "internal_seam_bypass", "fixture_claims_product_completion"
        ]
    })
}

pub fn generated_product_contract_files() -> Vec<GeneratedProductContractFile> {
    let mut files = BTreeMap::from([
        ("dependency-gate.v1.json", pretty(&dependency_gate())),
        (
            "product-action.v1.schema.json",
            pretty(&product_action_schema()),
        ),
        (
            "product-oracle-evidence.v1.schema.json",
            pretty(&oracle_evidence_schema()),
        ),
        (
            "product-oracle-manifest.v1.json",
            pretty(&oracle_manifest()),
        ),
        (
            "product-result.v1.schema.json",
            pretty(&product_result_schema()),
        ),
    ]);
    let digests = files
        .iter()
        .map(|(path, contents)| {
            (
                (*path).to_owned(),
                CanonicalDigest::of_bytes(contents.as_bytes()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    files.insert(
        "contract-set.v1.json",
        pretty(&json!({
            "schema_version": { "major": 1, "minor": 0 },
            "descriptor_digest": descriptor_digest(),
            "files": digests,
        })),
    );
    files
        .into_iter()
        .map(|(relative_path, contents)| GeneratedProductContractFile {
            relative_path,
            contents,
        })
        .collect()
}
