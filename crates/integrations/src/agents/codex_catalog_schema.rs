use serde_json::Value;

use super::{CodexCatalogError, CodexCatalogSelection};

impl CodexCatalogSelection {
    // be6e8eac: protocol/openai_models.rs ModelsResponse and its custom deserializer.
    pub fn validate_schema(&self) -> Result<(), CodexCatalogError> {
        if self.original()["models"]
            .as_array()
            .is_some_and(|models| !models.is_empty() && models.iter().all(model))
        {
            Ok(())
        } else {
            Err(CodexCatalogError::InvalidCatalog)
        }
    }
}

fn optional(value: &Value, key: &str, check: impl FnOnce(&Value) -> bool) -> bool {
    value
        .get(key)
        .is_none_or(|value| value.is_null() || check(value))
}

fn defaulted(value: &Value, key: &str, check: impl FnOnce(&Value) -> bool) -> bool {
    value.get(key).is_none_or(check)
}

fn one_of(value: &Value, values: &[&str]) -> bool {
    value.as_str().is_some_and(|value| values.contains(&value))
}

fn strings(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|values| values.iter().all(Value::is_string))
}

fn effort(value: &Value) -> bool {
    value.as_str().is_some_and(|value| !value.is_empty())
}

fn string_object(value: &Value, required: &[&str], nullable: &[&str]) -> bool {
    value.is_object()
        && required
            .iter()
            .all(|key| value.get(*key).is_some_and(Value::is_string))
        && nullable
            .iter()
            .all(|key| optional(value, key, Value::is_string))
}

fn model(value: &Value) -> bool {
    string_object(
        value,
        &["slug", "display_name"],
        &[
            "description",
            "default_service_tier",
            "comp_hash",
            "auto_review_model_override",
            "model_specialty",
            "base_instructions",
            "tool_mode",
            "multi_agent_version",
        ],
    ) && value["priority"]
        .as_i64()
        .is_some_and(|n| i32::try_from(n).is_ok())
        && one_of(&value["visibility"], &["list", "hide", "none"])
        && one_of(
            &value["shell_type"],
            &[
                "default",
                "local",
                "unified_exec",
                "disabled",
                "shell_command",
            ],
        )
        && ["supported_in_api", "support_verbosity"]
            .iter()
            .all(|key| value.get(*key).is_some_and(Value::is_boolean))
        && [
            "supports_parallel_tool_calls",
            "include_skills_usage_instructions",
            "include_plugin_usage_instructions",
            "include_apps_usage_instructions",
            "supports_reasoning_summary_parameter",
            "supports_image_detail_original",
            "supports_search_tool",
            "use_responses_lite",
        ]
        .iter()
        .all(|key| defaulted(value, key, Value::is_boolean))
        && optional(value, "default_reasoning_level", effort)
        && value["supported_reasoning_levels"]
            .as_array()
            .is_some_and(|levels| {
                levels.iter().all(|level| {
                    string_object(level, &["description"], &[]) && effort(&level["effort"])
                })
            })
        && defaulted(value, "additional_speed_tiers", strings)
        && defaulted(value, "service_tiers", |tiers| {
            tiers.as_array().is_some_and(|tiers| {
                tiers
                    .iter()
                    .all(|tier| string_object(tier, &["id", "name", "description"], &[]))
            })
        })
        && optional(value, "availability_nux", |nux| {
            string_object(nux, &["message"], &[])
        })
        && optional(value, "upgrade", |upgrade| {
            string_object(upgrade, &["model", "migration_markdown"], &[])
        })
        && optional(value, "model_messages", messages)
        && (value
            .pointer("/model_messages/instructions_template")
            .is_some_and(Value::is_string)
            || value.get("base_instructions").is_some_and(Value::is_string))
        && defaulted(value, "default_reasoning_summary", |summary| {
            one_of(summary, &["auto", "concise", "detailed", "none"])
        })
        && optional(value, "default_verbosity", |verbosity| {
            one_of(verbosity, &["low", "medium", "high"])
        })
        && optional(value, "apply_patch_tool_type", |tool| {
            one_of(tool, &["freeform"])
        })
        && defaulted(value, "web_search_tool_type", |tool| {
            one_of(tool, &["text", "text_and_image"])
        })
        && value["truncation_policy"].is_object()
        && one_of(&value["truncation_policy"]["mode"], &["bytes", "tokens"])
        && value["truncation_policy"]["limit"].as_i64().is_some()
        && [
            "context_window",
            "max_context_window",
            "auto_compact_token_limit",
        ]
        .iter()
        .all(|key| optional(value, key, |number| number.as_i64().is_some()))
        && defaulted(value, "effective_context_window_percent", |number| {
            number.as_i64().is_some()
        })
        && strings(&value["experimental_supported_tools"])
        && defaulted(value, "input_modalities", |modalities| {
            modalities.as_array().is_some_and(|modalities| {
                modalities
                    .iter()
                    .all(|modality| one_of(modality, &["text", "image", "audio"]))
            })
        })
}

fn messages(value: &Value) -> bool {
    string_object(value, &[], &["instructions_template"])
        && optional(value, "instructions_variables", |variables| {
            string_object(
                variables,
                &[],
                &[
                    "personality_default",
                    "personality_friendly",
                    "personality_pragmatic",
                ],
            )
        })
        && optional(value, "approvals", |approvals| {
            string_object(
                approvals,
                &[],
                &[
                    "on_request",
                    "on_request_auto_review",
                    "never",
                    "unless_trusted",
                ],
            )
        })
        && optional(value, "collaboration_modes", |modes| {
            string_object(modes, &[], &["default", "plan"])
        })
        && optional(value, "auto_review", |review| {
            string_object(review, &[], &["policy", "policy_template"])
        })
        && optional(value, "permissions", |permissions| {
            string_object(
                permissions,
                &[],
                &["danger_full_access", "workspace_write", "read_only"],
            )
        })
        && optional(value, "token_budget", |budget| {
            string_object(
                budget,
                &[
                    "reminder_message_template",
                    "guidance_message",
                    "auto_compact_fallback_prompt",
                ],
                &[],
            ) && budget["reminder_threshold_tokens"].as_i64().is_some()
                && budget["auto_compact_fallback_buffer_tokens"]
                    .as_i64()
                    .is_some()
        })
}

#[cfg(test)]
#[path = "codex_catalog_schema_tests.rs"]
mod tests;
