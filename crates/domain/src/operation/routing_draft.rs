//! Draft mutations use the existing protected routing writer, with no external effects.
use super::*;
use crate::{PlanDraftActionV1, PlanDraftChangeV1, PlanVersionV1};
const CONTROL_SCHEMA: &str = "hiroute.plan-draft-control/v1";
impl TransactionPlanV1 {
    pub fn from_plan_draft_planner(spec: ChangeSpecV1) -> Result<Self, OperationValidationError> {
        let change: PlanDraftChangeV1 = serde_json::from_value(spec.desired_state.clone())
            .map_err(|_| OperationValidationError::UnregisteredEffectPlan)?;
        let control = json!({"schema": CONTROL_SCHEMA, "change": change});
        validate_plan(&spec, &control, &[], &[], &[])?;
        Ok(Self {
            spec,
            control,
            credential_pool: None,
            worker_dependency_selection: None,
            secrets: vec![],
            agent_access_grants: vec![],
            runtime: vec![],
            external: vec![],
        })
    }
    pub fn with_draft_legacy_source(
        mut self,
        legacy: PlanVersionV1,
    ) -> Result<Self, OperationValidationError> {
        self.control["legacy_source"] = serde_json::to_value(legacy)
            .map_err(|_| OperationValidationError::UnregisteredEffectPlan)?;
        self.plan_draft_change()?;
        Ok(self)
    }
    pub fn draft_legacy_source(&self) -> Result<Option<PlanVersionV1>, OperationValidationError> {
        if self.plan_draft_change()?.is_none() {
            return Ok(None);
        }
        legacy_source(&self.control)
    }
    pub fn plan_draft_change(&self) -> Result<Option<PlanDraftChangeV1>, OperationValidationError> {
        if !is_control(&self.control) {
            return Ok(None);
        }
        validate_plan(
            &self.spec,
            &self.control,
            &self.secrets,
            &self.runtime,
            &self.external,
        )?;
        serde_json::from_value(self.spec.desired_state.clone())
            .map(Some)
            .map_err(|_| OperationValidationError::UnregisteredEffectPlan)
    }
}
pub(super) fn is_control(value: &Value) -> bool {
    value.get("schema").and_then(Value::as_str) == Some(CONTROL_SCHEMA)
}
pub(super) fn validate_plan(
    spec: &ChangeSpecV1,
    control: &Value,
    secrets: &[SecretMutationV1],
    runtime: &[RuntimeMutationV1],
    external: &[ExternalEffectIntentV1],
) -> Result<(), OperationValidationError> {
    let invalid = || OperationValidationError::UnregisteredEffectPlan;
    let change: PlanDraftChangeV1 =
        serde_json::from_value(spec.desired_state.clone()).map_err(|_| invalid())?;
    change.validate().map_err(|_| invalid())?;
    let legacy = legacy_source(control)?;
    let mut expected = json!({"schema": CONTROL_SCHEMA, "change": change});
    if let Some(legacy) = legacy {
        legacy.validate().map_err(|_| invalid())?;
        let PlanDraftActionV1::Save { draft } = &change.action else {
            return Err(invalid());
        };
        if legacy.reference.workspace_id != change.workspace_id
            || draft.plan_id.as_ref() != Some(&legacy.reference.plan_id)
            || draft.base_head_revision != Some(legacy.reference.content_revision)
            || legacy
                != PlanVersionV1::from_unversioned_compiled_recovery(
                    change.workspace_id.clone(),
                    legacy.compiled.clone(),
                )
                .map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        expected["legacy_source"] = serde_json::to_value(legacy).map_err(|_| invalid())?;
    }
    if spec.command_id != "routing.apply"
        || !secrets.is_empty()
        || !runtime.is_empty()
        || !external.is_empty()
        || spec.resource_id.as_deref() != Some(&format!("plan-draft/{}", change.draft_id))
        || *control != expected
    {
        return Err(invalid());
    }
    Ok(())
}

fn legacy_source(control: &Value) -> Result<Option<PlanVersionV1>, OperationValidationError> {
    control
        .get("legacy_source")
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| OperationValidationError::UnregisteredEffectPlan)
        })
        .transpose()
}
