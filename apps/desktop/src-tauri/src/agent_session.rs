//! Agent-specific native flow over shared Client Core and the existing confirmation/recovery.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSettingsInput {
    pub spec: AgentSettingsSpecV2,
    pub language: String,
}
#[derive(Deserialize, Serialize)]
pub struct AgentEntry {
    pub agent_id: String,
    pub version: String,
    pub configuration_state: String,
    #[serde(default)]
    pub available_surfaces: Vec<AgentModelSurfaceV2>,
    #[serde(default)]
    pub native_model_catalog: Option<Value>,
    pub context_id: Option<String>,
    #[serde(default)]
    pub settings: Option<AgentModelSettingsStatusV2>,
    #[serde(default)]
    pub status_error: Option<String>,
}
#[derive(Serialize)]
pub struct AgentSnapshot {
    pub agents: Vec<AgentEntry>,
    pub plans: AgentPlanCatalogViewV2,
    pub trusted_authority: bool,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct AgentResidentServicePreview {
    #[serde(default)]
    pub login_item_required: bool,
    #[serde(default)]
    pub login_item_removal_required: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct AgentPreview {
    pub schema: String,
    pub spec: AgentSettingsSpecV2,
    pub accept_digest: CanonicalDigest,
    pub dependency_digest: CanonicalDigest,
    pub expected_revisions: hiroute_domain::RevisionSetV1,
    pub applicable: bool,
    pub blockers: Vec<Value>,
    pub model_effect: Value,
    #[serde(default)]
    pub collaboration_effect: Option<AgentCollaborationEffectV2>,
    #[serde(default)]
    pub resident_service: AgentResidentServicePreview,
}
#[derive(Serialize)]
pub struct AgentOutcome {
    pub preview: AgentPreview,
    pub mutation: Option<MutationOutcome>,
}
pub enum AgentPreparation {
    Blocked(Box<AgentPreview>),
    Ready(Box<AgentConfirmation>),
}

fn trusted_preview_spec(requested: &AgentSettingsSpecV2, returned: &AgentSettingsSpecV2) -> bool {
    if requested.schema_version != returned.schema_version
        || requested.context_id != returned.context_id
        || requested.collaboration != returned.collaboration
    {
        return false;
    }
    match (&requested.model, &returned.model) {
        (
            AgentFacetIntent::Configure {
                settings:
                    AgentModelSelectionV2::CodexDefault {
                        fixed_models: requested_fixed,
                        allowed_plan_ids: requested_plans,
                        default_selection: requested_default,
                    },
            },
            AgentFacetIntent::Configure {
                settings:
                    AgentModelSelectionV2::CodexDefault {
                        fixed_models: returned_fixed,
                        allowed_plan_ids: returned_plans,
                        default_selection: returned_default,
                    },
            },
        ) => {
            requested_plans == returned_plans
                && requested_default == returned_default
                && requested_fixed
                    .iter()
                    .all(|requested| returned_fixed.contains(requested))
        }
        (requested, returned) => requested == returned,
    }
}
// Only this backend-held value can enter finish_agent_confirmation. The WebView can resolve its
// temporary prompt ID, but never receives this context or an Apply capability.
pub struct AgentConfirmation {
    plan_names: std::collections::BTreeMap<String, String>,
    agent_name: String,
    previous: Option<AgentModelSettingsStatusV2>,
    permit: ConfirmationPermit,
    input: AgentSettingsInput,
    preview: AgentPreview,
    request: AgentSettingsApplyV2,
    intent: IntentEvidence,
}
impl AgentConfirmation {
    pub fn revision(&self) -> u64 {
        self.request.expected_revisions.target
    }
    pub fn requires_confirmation(&self) -> bool {
        // The settings dialog is already the user's explicit review and submit action.
        // Keep an additional confirmation only for restore/disable actions with destructive effect.
        self.input.spec.is_restore_only()
    }
    pub fn english(&self) -> bool {
        self.input.language == "en"
    }
    pub fn message(&self) -> String {
        let action = if self.input.spec.is_restore_only()
            && matches!(&self.input.spec.model, AgentFacetIntent::Restore { .. })
        {
            if self.english() {
                "Restore owned model settings"
            } else {
                "恢复受管模型设置"
            }
        } else if self.input.spec.is_restore_only() {
            if self.english() {
                "Disable Agent routing"
            } else {
                "停用 Agent 路由"
            }
        } else if matches!(&self.input.spec.model, AgentFacetIntent::Configure { .. }) {
            if self.english() {
                "Configure model routing"
            } else {
                "配置模型路由"
            }
        } else if self.english() {
            "Configure Agent routing"
        } else {
            "配置 Agent 路由"
        };
        let route_name = |id: &str| {
            self.plan_names.get(id).cloned().unwrap_or_else(|| {
                if self.english() {
                    "Unavailable route".into()
                } else {
                    "不可用路由".into()
                }
            })
        };
        // The sealed selection is the default authority: a fixed model, a plan route, or the
        // native default. The preview effect no longer carries a resolved alias.
        let default_model = |settings: &AgentModelSelectionV2| match settings {
            AgentModelSelectionV2::CodexDefault {
                default_selection: AgentModelDefaultSelectionV2::FixedModel { client_model_id },
                ..
            } => client_model_id.clone(),
            _ => "—".to_string(),
        };
        let default_route = |settings: &AgentModelSelectionV2| match settings {
            AgentModelSelectionV2::CodexDefault {
                default_selection: AgentModelDefaultSelectionV2::Plan { plan_id },
                ..
            } => route_name(plan_id.as_str()),
            _ => "—".to_string(),
        };
        let effect = &self.preview.model_effect;
        let details = match &self.input.spec.model {
            AgentFacetIntent::Configure { settings } => format!(
                "{}: {}\n{}: {}\n{}: {}\n{}",
                if self.english() {
                    "Endpoint"
                } else {
                    "模型地址"
                },
                effect["endpoint"].as_str().unwrap_or("—"),
                if self.english() {
                    "Default model"
                } else {
                    "默认模型"
                },
                default_model(settings),
                if self.english() {
                    "Smart route"
                } else {
                    "智能路由"
                },
                default_route(settings),
                if self.english() {
                    "Authentication will use a connection-scoped local credential."
                } else {
                    "认证将替换为仅此连接使用的本机凭据。"
                }
            ),
            AgentFacetIntent::Restore { .. } => if self.english() {
                "Restore the previously saved model configuration."
            } else {
                "恢复先前保存的模型配置。"
            }
            .into(),
            AgentFacetIntent::Keep => String::new(),
        };
        let collaboration_details = match &self.input.spec.collaboration {
            AgentFacetIntent::Configure { settings } => match settings.trigger_mode {
                AgentCollaborationTriggerModeV2::Explicit => if self.english() {
                    "Task delegation: only when explicitly requested."
                } else {
                    "任务委派：仅在明确要求时触发。"
                }
                .into(),
                AgentCollaborationTriggerModeV2::DelegateByDefault => if self.english() {
                    "Task delegation: delegate executable work by default."
                } else {
                    "任务委派：默认委派可执行工作。"
                }
                .into(),
            },
            AgentFacetIntent::Restore { .. } => if self.english() {
                "Disable Agent routing for this Agent."
            } else {
                "为此 Agent 停用 Agent 路由。"
            }
            .into(),
            AgentFacetIntent::Keep => String::new(),
        };
        let mut changes = Vec::new();
        if let AgentFacetIntent::Configure { settings } = &self.input.spec.model
            && let Some(before) = self
                .previous
                .as_ref()
                .and_then(|s| s.current_selection.as_ref())
        {
            let before_model = default_model(before);
            let settings_model = default_model(settings);
            if before_model != settings_model {
                changes.push(format!(
                    "{}: {} → {}",
                    if self.english() {
                        "Default model"
                    } else {
                        "默认模型"
                    },
                    before_model,
                    settings_model
                ));
            }
            let before_route = default_route(before);
            let settings_route = default_route(settings);
            if before_route != settings_route {
                changes.push(format!(
                    "{}: {} → {}",
                    if self.english() {
                        "Default route"
                    } else {
                        "默认路由"
                    },
                    before_route,
                    settings_route
                ));
            }
            let before_plans = before.allowed_plan_ids();
            let settings_plans = settings.allowed_plan_ids();
            for id in before_plans.difference(&settings_plans) {
                changes.push(format!("− {}", route_name(id.as_str())));
            }
            for id in settings_plans.difference(&before_plans) {
                changes.push(format!("+ {}", route_name(id.as_str())));
            }
        }
        let model_details = if changes.is_empty() {
            details
        } else {
            format!(
                "{}\n{}",
                changes.join("\n"),
                if self.english() {
                    "This updates the Agent’s managed configuration."
                } else {
                    "将更新此 Agent 的受管配置。"
                }
            )
        };
        let details = [model_details, collaboration_details]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let resident_details = if self.preview.resident_service.login_item_required {
            if self.english() {
                "HiRoute will be added to your login items so the local service keeps running \
                 after this window closes or you sign out."
            } else {
                "HiRoute 将加入登录项，关闭窗口或退出登录后本机服务仍持续可用。"
            }
        } else {
            ""
        };
        let details = [details, resident_details.into()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        format!("{action} · {}\n\n{details}", self.agent_name)
    }
}
impl Session {
    pub async fn agent_snapshot(&mut self) -> Result<AgentSnapshot, DesktopFailure> {
        #[derive(Deserialize)]
        struct Scan {
            agents: Vec<AgentEntry>,
        }
        let mut scan: Scan = query(&self.client, "ScanAgents", &ClientEmptyRequestV1 {}).await?;
        for agent in &mut scan.agents {
            if let Some(context_id) = &agent.context_id {
                match query(
                    &self.client,
                    "GetAgentConnectionStatus",
                    &AgentSettingsStatusRequestV2 {
                        schema_version: AGENT_SETTINGS_SCHEMA_V2,
                        context_id: context_id.clone(),
                    },
                )
                .await
                {
                    Ok(status) => agent.settings = Some(status),
                    Err(_) => agent.status_error = Some("AGENT_STATUS_UNAVAILABLE".into()),
                }
            }
        }
        let snapshot = self.snapshot().await?;
        if let Some(error) = snapshot.catalog_error {
            return Err(error);
        }
        Ok(AgentSnapshot {
            agents: scan.agents,
            plans: snapshot.catalog,
            trusted_authority: snapshot.trusted_authority && snapshot.service.mutation_available,
        })
    }

    pub async fn preview_agent_settings(
        &mut self,
        mut input: AgentSettingsInput,
    ) -> Result<AgentPreparation, DesktopFailure> {
        if !matches!(input.language.as_str(), "zh" | "en")
            || input.spec.schema_version != AGENT_SETTINGS_SCHEMA_V2
        {
            return Err("AGENT_INPUT_INVALID".into());
        }
        if self.confirmation.is_open() {
            return Err("CONFIRMATION_ALREADY_OPEN".into());
        }
        let intent = IntentEvidence {
            schema: "hiroute.desktop-agent-settings-intent/v2".into(),
            digest: CanonicalDigest::of(&input.spec).map_err(|_| "AGENT_INPUT_INVALID")?,
        };
        let retry = self.retry_key(&intent).await?;
        let snapshot = self.snapshot().await?;
        if !snapshot.trusted_authority || !snapshot.service.mutation_available {
            return Err("TRUSTED_AUTHORITY_UNAVAILABLE".into());
        }
        let operation = if input.spec.is_restore_only() {
            "PreviewAgentConnectionRestore"
        } else {
            "PreviewAgentConnectionChange"
        };
        let preview: AgentPreview = query(
            &self.client,
            operation,
            &AgentSettingsPreviewRequestV2 {
                spec: input.spec.clone(),
            },
        )
        .await?;
        if preview.schema != "hiroute.agent-settings-preview/v2"
            || !trusted_preview_spec(&input.spec, &preview.spec)
            || preview.applicable != preview.blockers.is_empty()
        {
            return Err("AGENT_PREVIEW_INVALID".into());
        }
        if !preview.applicable {
            return Ok(AgentPreparation::Blocked(Box::new(preview)));
        }
        let request = AgentSettingsApplyV2 {
            spec: preview.spec.clone(),
            accept_digest: preview.accept_digest.clone(),
            dependency_digest: preview.dependency_digest.clone(),
            expected_revisions: preview.expected_revisions.clone(),
            idempotency_key: match retry {
                Some(key) => key,
                None => crate::random_id()?,
            },
            login_item: None,
        };
        input.spec = preview.spec.clone();
        // Ordinary Save is already the user's explicit confirmation. Only a
        // restore/disable prompt needs display names and previous settings;
        // avoid two extra daemon reads between Preview and Apply for every Save.
        let (agent_name, previous, plan_names) = if input.spec.is_restore_only() {
            #[derive(Deserialize)]
            struct AgentNames {
                agents: Vec<AgentEntry>,
            }
            let names: Option<AgentNames> =
                query(&self.client, "ScanAgents", &ClientEmptyRequestV1 {})
                    .await
                    .ok();
            let agent_name = names
                .as_ref()
                .and_then(|scan| {
                    scan.agents.iter().find(|agent| {
                        agent.context_id.as_deref() == Some(input.spec.context_id.as_str())
                    })
                })
                .map(|agent| match agent.agent_id.as_str() {
                    "agent_codex_default" => "Codex".to_owned(),
                    "agent_claude_default" => "Claude Code".to_owned(),
                    _ => if input.language == "en" {
                        "Scanned Agent"
                    } else {
                        "已扫描 Agent"
                    }
                    .to_owned(),
                })
                .unwrap_or_else(|| {
                    if input.language == "en" {
                        "Agent identity unavailable"
                    } else {
                        "Agent 名称暂不可得"
                    }
                    .to_owned()
                });
            let previous = query(
                &self.client,
                "GetAgentConnectionStatus",
                &AgentSettingsStatusRequestV2 {
                    schema_version: AGENT_SETTINGS_SCHEMA_V2,
                    context_id: input.spec.context_id.clone(),
                },
            )
            .await
            .ok();
            let plan_names = snapshot
                .catalog
                .plans
                .iter()
                .map(|p| {
                    (
                        p.agent_plan_id.as_str().to_owned(),
                        p.desired.display_name.as_str().to_owned(),
                    )
                })
                .collect();
            (agent_name, previous, plan_names)
        } else {
            (String::new(), None, Default::default())
        };
        Ok(AgentPreparation::Ready(Box::new(AgentConfirmation {
            plan_names,
            agent_name,
            previous,
            permit: self.confirmation.begin()?,
            input,
            preview,
            request,
            intent,
        })))
    }

    pub async fn finish_agent_confirmation(
        &mut self,
        context: AgentConfirmation,
        accepted: bool,
    ) -> Result<AgentOutcome, DesktopFailure> {
        if !self.confirmation.finish(&context.permit, accepted)? {
            return Ok(AgentOutcome {
                preview: context.preview,
                mutation: Some(MutationOutcome {
                    state: "cancelled_before_apply".into(),
                    operation: None,
                }),
            });
        }
        let restore = context.request.spec.is_restore_only();
        let operation = if restore {
            "ApplyAgentConnectionRestore"
        } else {
            "ApplyAgentConnectionChange"
        };
        // The first managed connection registers the resident login item inside the host
        // before any intent is recorded: a failed establishment leaves nothing in flight, and
        // an unapproved or failed registration reports SERVICE_UNAVAILABLE without applying.
        // The last managed connection's restore symmetrically unregisters the item the host
        // owns, proven by journal evidence on the backend.
        let login_item = if !restore && context.preview.resident_service.login_item_required {
            Some(crate::login_item::establish_resident_login_item()?)
        } else if restore && context.preview.resident_service.login_item_removal_required {
            Some(crate::login_item::remove_resident_login_item()?)
        } else {
            None
        };
        let mut request = context.request;
        request.login_item = login_item.clone();
        let hint = SubmittedOperation {
            // The legacy plan field is empty for Agent intents; the typed intent digest carries
            // the context/facets, and exact lookup uses principal/operation/key/digest.
            plan_id: String::new(),
            principal_kind: PrincipalKind::InteractiveUser,
            operation_kind: operation.into(),
            idempotency_key: request.idempotency_key.clone(),
            accepted_digest: request.accept_digest.clone(),
            operation_id: None,
            after_sequence: 0,
            intent: Some(context.intent),
            latest_edit_not_applied: false,
        };

        self.hint = Some(hint);
        let response = self
            .client
            .call_wire(LocalControlWireRequestV2 {
                schema_version: LOCAL_CONTROL_SCHEMA_V2,
                request_id: crate::random_id()?,
                operation_id: operation.into(),
                payload: serde_json::to_value(request).map_err(|_| "REQUEST_INVALID")?,
                protected_grant: None,
            })
            .await;
        let mutation = self.reconcile_apply_response(response).await;
        if let Some(declaration) = login_item.as_ref() {
            // Only a definitively failed apply compensates: an admission rejection that never
            // started an Operation (the hint was cleared), or an Operation that rolled back.
            // A lost response or a still-pending Operation keeps the item for recovery.
            let definitively_failed = match &mutation {
                Err(_) => self.hint.is_none(),
                Ok(outcome) => outcome
                    .operation
                    .as_ref()
                    .is_some_and(|view| view.state == "rolled_back"),
            };
            if definitively_failed {
                if declaration.removes_resident_service() {
                    // The rolled-back connection still needs its owned item at the next login.
                    crate::login_item::compensate_resident_login_item_removal();
                } else if declaration.created {
                    crate::login_item::compensate_resident_login_item(true);
                }
            }
        }
        let mutation = mutation?;
        Ok(AgentOutcome {
            preview: context.preview,
            mutation: Some(mutation),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collaboration_preview_decodes_the_v2_effect_shape() {
        let preview: AgentPreview = serde_json::from_value(json!({
            "schema": "hiroute.agent-settings-preview/v2",
            "spec": {
                "schema_version": { "major": 2, "minor": 0 },
                "context_id": "context/codex",
                "model": { "intent": "keep" },
                "collaboration": {
                    "intent": "configure",
                    "settings": { "trigger_mode": "delegate_by_default" }
                }
            },
            "accept_digest": format!("sha256:{}", "0".repeat(64)),
            "dependency_digest": format!("sha256:{}", "1".repeat(64)),
            "expected_revisions": { "target": 1, "dependencies": {} },
            "applicable": true,
            "blockers": [],
            "model_effect": { "action": "keep" },
            "collaboration_effect": {
                "action": "configure",
                "trigger_mode": "delegate_by_default"
            }
        }))
        .expect("the Desktop must accept Application's V2 collaboration effect");

        let effect = preview.collaboration_effect.expect("collaboration effect");
        assert_eq!(effect.action, "configure");
        assert_eq!(
            effect.trigger_mode,
            hiroute_application_api::AgentCollaborationTriggerModeV2::DelegateByDefault
        );
    }
}
