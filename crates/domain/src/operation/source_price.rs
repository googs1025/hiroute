use super::*;
use crate::SourcePriceChangeV2;

impl TransactionPlanV1 {
    pub fn from_source_price_planner(spec: ChangeSpecV1) -> Result<Self, OperationValidationError> {
        let change: SourcePriceChangeV2 = serde_json::from_value(spec.desired_state.clone())
            .map_err(|_| OperationValidationError::UnregisteredEffectPlan)?;
        let control = json!({"source_price_change_v2": change});
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
    pub fn source_price_change(&self) -> Option<SourcePriceChangeV2> {
        self.control
            .get("source_price_change_v2")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}
pub(super) fn validate_plan(
    spec: &ChangeSpecV1,
    control: &Value,
    secrets: &[SecretMutationV1],
    runtime: &[RuntimeMutationV1],
    external: &[ExternalEffectIntentV1],
) -> Result<(), OperationValidationError> {
    if spec.command_id != "prices.override.apply"
        || !secrets.is_empty()
        || !runtime.is_empty()
        || !external.is_empty()
    {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    let change: SourcePriceChangeV2 = serde_json::from_value(spec.desired_state.clone())
        .map_err(|_| OperationValidationError::UnregisteredEffectPlan)?;
    change
        .validate()
        .map_err(|_| OperationValidationError::UnregisteredEffectPlan)?;
    if spec.resource_id.as_deref() != Some(change.target.source_id.as_str())
        || *control != json!({"source_price_change_v2": change})
    {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    Ok(())
}
