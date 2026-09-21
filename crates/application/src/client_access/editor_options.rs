use super::*;
use crate::compiler::{AgentPlanCompilerError, compile_agent_plan_v2};
use hiroute_domain::{AgentPlanId, AgentPlanIdentityV1, ModelAlias, PlanEditorStateV2};

pub(super) fn dispatch(
    ports: &crate::control::ApplicationPorts,
    request: LocalControlRequestV2,
) -> MachineEnvelopeV2<Value> {
    let payload: PlanEditorOptionsRequestV1 = match serde_json::from_value(request.payload) {
        Ok(value) => value,
        Err(_) => return failed(ErrorCode::InvalidArguments, request.request_id),
    };
    let Some(port) = &ports.routing else {
        return failed(ErrorCode::DaemonUnavailable, request.request_id);
    };
    if payload.rating_items.len() > MAX_RATING_QUERY_ITEMS
        || payload.native_selections.len() > MAX_RATING_QUERY_ITEMS
    {
        return failed(ErrorCode::InvalidArguments, request.request_id);
    }
    let result = (|| {
        let state = port
            .routing_compilation_snapshot(&WorkspaceId::default())
            .map_err(map_control_error)?;
        if state.facts.candidates.len() > MAX_RATING_QUERY_ITEMS {
            return Err(ErrorCode::InvalidArguments);
        }
        let free_suggestions = if payload.suggest_free {
            Some(
                port.free_plan_suggestions(
                    &WorkspaceId::default(),
                    &payload.requirements,
                    &payload.native_selections,
                    RatingSnapshotSelectionV1::Latest,
                )
                .map_err(map_control_error)?,
            )
        } else {
            None
        };
        let ratings = if payload.rating_items.is_empty() {
            None
        } else {
            match port.resolve_model_ratings(&ResolveModelRatingsV1 {
                snapshot: RatingSnapshotSelectionV1::Latest,
                items: payload.rating_items,
            }) {
                Ok(result) => Some(result),
                Err(crate::model_catalog::RatingQueryError::SnapshotUnavailable) => None,
                Err(_) => return Err(ErrorCode::InvalidArguments),
            }
        };
        let suggested_alias = payload
            .display_name
            .as_ref()
            .map(|name| {
                state
                    .active_publication
                    .as_ref()
                    .map(|p| p.alias_registry.clone())
                    .unwrap_or_default()
                    .suggest_alias(name)
                    .map_err(|_| ErrorCode::InvalidArguments)
            })
            .transpose()?;
        let codex_capabilities =
            codex_capabilities(payload.editor.as_ref(), &state.facts, port.as_ref())?;
        let candidates = state
            .facts
            .candidates
            .iter()
            .map(|fact| PlanCandidateOptionV1 {
                binding_id: fact.binding.binding_id.clone(),
                model_configuration_id: fact.model.model_configuration_id.clone(),
                display_name: fact.model.display_name.clone(),
                reasoning: fact.reasoning.clone(),
                billing_class: fact.binding.billing_class,
                routable: fact.is_routable(),
                ingress_protocols: fact
                    .protocol_profiles
                    .iter()
                    .map(|p| p.ingress_protocol)
                    .collect(),
            })
            .collect();
        Ok(PlanEditorOptionsV1 {
            suggested_alias,
            candidates,
            ratings,
            free_suggestions,
            codex_capabilities,
            revisions: state.expected_revisions,
        })
    })();
    match result {
        Ok(value) => succeeded(value, request.request_id),
        Err(error) => failed(error, request.request_id),
    }
}

fn codex_capabilities(
    editor: Option<&PlanEditorStateV2>,
    facts: &crate::compiler::AgentPlanCompilationFactsV1,
    port: &dyn crate::control::RoutingFactsPort,
) -> Result<Option<CodexClientCapabilityPreviewV1>, ErrorCode> {
    let Some(editor) = editor else {
        return Ok(None);
    };
    let Ok(configuration) = editor.effective() else {
        // Incomplete draft rows are ordinary while editing and do not claim a capability state.
        return Ok(None);
    };
    let identity = AgentPlanIdentityV1 {
        agent_plan_id: AgentPlanId::parse("plan/codex-capability-preview")
            .map_err(|_| ErrorCode::Internal)?,
        model_alias: ModelAlias::parse("hiroute-codex-capability-preview")
            .map_err(|_| ErrorCode::Internal)?,
        display_name: configuration.display_name.clone(),
        purpose: configuration.purpose.clone(),
    };
    let plan = match compile_agent_plan_v2(identity, 1, &configuration, facts) {
        Ok(plan) => plan,
        Err(error) => return Ok(Some(compilation_unavailable(&error))),
    };
    port.codex_client_capability_preview(&plan)
        .map(Some)
        .map_err(map_control_error)
}

fn compilation_unavailable(error: &AgentPlanCompilerError) -> CodexClientCapabilityPreviewV1 {
    let binding_id = match error {
        AgentPlanCompilerError::UnknownBinding(binding_id)
        | AgentPlanCompilerError::CandidateNotRoutable(binding_id)
        | AgentPlanCompilerError::CapabilityUnqualified(binding_id) => Some(binding_id.clone()),
        _ => None,
    };
    CodexClientCapabilityPreviewV1::Unavailable {
        issues: vec![CodexCapabilityIssueV1 {
            kind: CodexCapabilityIssueKindV1::PlanCompilation,
            binding_id,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::test_fixtures::{compilation_facts, custom_desired};
    use crate::control::{ControlReadError, RoutingCompilationSnapshotV1};
    use hiroute_domain::{
        AgentPlanStrategyV1, ComplexityClassifierModeV1, FreeEditorV2, PLAN_EDITOR_SCHEMA_V2,
        PlanEditorMode, SmartEditorV2, WorkspaceId,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct PreviewPort {
        calls: AtomicUsize,
    }

    impl crate::control::RoutingFactsPort for PreviewPort {
        fn codex_client_capability_preview(
            &self,
            _plan: &hiroute_domain::CompiledAgentPlanV1,
        ) -> Result<CodexClientCapabilityPreviewV1, ControlReadError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CodexClientCapabilityPreviewV1::Available {
                context_window: 64_000,
                input_modalities: vec![CodexInputModalityV1::Text],
                reasoning: CodexReasoningControlV1::RouteConfiguration,
                limitations: Vec::new(),
                fixed_limits: vec![CodexFixedCapabilityLimitV1::ParallelToolCallsDisabled],
            })
        }

        fn routing_compilation_snapshot(
            &self,
            _workspace_id: &WorkspaceId,
        ) -> Result<RoutingCompilationSnapshotV1, ControlReadError> {
            Err(ControlReadError::Unavailable)
        }
    }

    fn editor() -> PlanEditorStateV2 {
        let desired = custom_desired();
        let AgentPlanStrategyV1::Custom { candidates } = desired.strategy else {
            unreachable!()
        };
        PlanEditorStateV2 {
            schema: PLAN_EDITOR_SCHEMA_V2.into(),
            display_name: desired.display_name.as_str().to_owned(),
            purpose: desired.purpose.as_str().to_owned(),
            custom_alias: None,
            mode: PlanEditorMode::FixedModel,
            candidates,
            smart: SmartEditorV2 {
                economy: Vec::new(),
                primary: Vec::new(),
                primary_fallback: false,
                classifier: ComplexityClassifierModeV1::LocalRules,
                complex_keywords: Vec::new(),
            },
            free: FreeEditorV2 {
                candidates: Vec::new(),
                primary: Vec::new(),
                primary_fallback: false,
            },
            delegation_enabled: false,
            work: None,
            requirements: desired.requirements,
            limits: desired.limits,
        }
    }

    #[test]
    fn complete_editor_compiles_with_current_facts_before_capability_analysis() {
        let port = PreviewPort::default();
        let preview = codex_capabilities(Some(&editor()), &compilation_facts(), &port).unwrap();
        assert!(matches!(
            preview,
            Some(CodexClientCapabilityPreviewV1::Available {
                context_window: 64_000,
                ..
            })
        ));
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn incomplete_editor_has_no_premature_capability_warning() {
        let port = PreviewPort::default();
        let mut editor = editor();
        editor.display_name.clear();
        assert_eq!(
            codex_capabilities(Some(&editor), &compilation_facts(), &port).unwrap(),
            None
        );
        assert_eq!(port.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn compilation_failure_identifies_the_selected_binding_without_calling_integration() {
        let port = PreviewPort::default();
        let mut editor = editor();
        editor.candidates[0].binding_id = "binding/missing".into();
        assert_eq!(
            codex_capabilities(Some(&editor), &compilation_facts(), &port).unwrap(),
            Some(CodexClientCapabilityPreviewV1::Unavailable {
                issues: vec![CodexCapabilityIssueV1 {
                    kind: CodexCapabilityIssueKindV1::PlanCompilation,
                    binding_id: Some("binding/missing".into()),
                }],
            })
        );
        assert_eq!(port.calls.load(Ordering::SeqCst), 0);
    }
}
