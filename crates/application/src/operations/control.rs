use hiroute_application_api::OperationLookupV1;
use hiroute_domain::OperationId;
use serde_json::{Value, json};

use crate::control::{ControlReadError, ControlStatePort};

pub(crate) fn get(
    control: &dyn ControlStatePort,
    lookup: &OperationLookupV1,
) -> Result<Value, ControlReadError> {
    let operation_id =
        OperationId::parse(lookup.operation_id.clone()).map_err(|_| ControlReadError::NotFound)?;
    let operation = control
        .operation(&operation_id)?
        .ok_or(ControlReadError::NotFound)?;
    serde_json::to_value(operation).map_err(|_| ControlReadError::Corrupt)
}

pub(crate) fn watch_snapshot(
    control: &dyn ControlStatePort,
    lookup: &OperationLookupV1,
) -> Result<Value, ControlReadError> {
    let operation = get(control, lookup)?;
    let sequence = operation
        .get("sequence")
        .or_else(|| operation.get("generation"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    if sequence <= lookup.after_sequence {
        Ok(json!({
            "schema": "hiroute.operation-event/v1",
            "after_sequence": lookup.after_sequence,
            "event": null,
            "operation": operation,
        }))
    } else {
        Ok(json!({
            "schema": "hiroute.operation-event/v1",
            "after_sequence": lookup.after_sequence,
            "event": operation,
        }))
    }
}

pub(crate) fn setup_status(
    control: &dyn ControlStatePort,
    lookup: &OperationLookupV1,
) -> Result<Value, ControlReadError> {
    let operation = get(control, lookup)?;
    if operation
        .pointer("/idempotency/operation_kind")
        .and_then(Value::as_str)
        != Some("ApplySetup")
    {
        return Err(ControlReadError::NotFound);
    }
    Ok(json!({
        "schema": "hiroute.setup-status/v1",
        "operation": operation,
    }))
}
