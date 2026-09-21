use std::collections::BTreeSet;

use serde_json::Value;

use crate::server::core_runtime::model_ir::{
    ContentPart, ExactProviderPathV1, ModelIrError, ModelRequestIRV1, ToolIdMapEntryV1,
};
use crate::server::request_plan::IngressProtocol;

use super::{decode_ingress_request, decode_ingress_request_with_bindings};

/// Request-owned continuation facts recovered from the accepted logical
/// request scope. Raw client JSON is never allowed to invent physical owner
/// identity or native Tool IDs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IngressRequestBindings {
    pub provider_state_owner: Option<ExactProviderPathV1>,
    pub tool_id_map: Vec<ToolIdMapEntryV1>,
}

/// Canonical ingress decode with request-scoped trusted Tool authority. The
/// caller sees either a fully bound request or an error; an unbound
/// continuation IR never escapes this adapter seam.
pub fn decode_ingress_request_with_tool_resolver<F>(
    protocol: IngressProtocol,
    body: &Value,
    resolve: F,
) -> Result<ModelRequestIRV1, ModelIrError>
where
    F: FnOnce(&[String]) -> Result<Vec<ToolIdMapEntryV1>, ModelIrError>,
{
    decode_ingress_request_with_state_and_tool_resolver(protocol, body, None, resolve)
}

pub fn decode_ingress_request_with_state_and_tool_resolver<F>(
    protocol: IngressProtocol,
    body: &Value,
    provider_state_owner: Option<ExactProviderPathV1>,
    resolve: F,
) -> Result<ModelRequestIRV1, ModelIrError>
where
    F: FnOnce(&[String]) -> Result<Vec<ToolIdMapEntryV1>, ModelIrError>,
{
    let initial = IngressRequestBindings {
        provider_state_owner: provider_state_owner.clone(),
        tool_id_map: Vec::new(),
    };
    let mut request = if provider_state_owner.is_some() {
        decode_ingress_request_with_bindings(protocol, body, &initial)?
    } else {
        decode_ingress_request(protocol, body)?
    };
    let logical_ids = request.continuation_logical_ids()?;
    if logical_ids.is_empty() {
        return Ok(request);
    }
    let tool_id_map = resolve(&logical_ids)?;
    let bindings = IngressRequestBindings {
        provider_state_owner,
        tool_id_map,
    };
    validate_bindings(&bindings)?;
    let resolved_ids = bindings
        .tool_id_map
        .iter()
        .map(|mapping| mapping.logical_id.as_str())
        .collect::<BTreeSet<_>>();
    if bindings.tool_id_map.len() != logical_ids.len()
        || resolved_ids.len() != logical_ids.len()
        || logical_ids
            .iter()
            .any(|logical_id| !resolved_ids.contains(logical_id.as_str()))
    {
        return Err(ModelIrError::ToolContinuationUnavailable);
    }
    bind_tool_continuations(&mut request, bindings.tool_id_map)?;
    Ok(request)
}

pub(super) fn bind_tool_continuations(
    request: &mut ModelRequestIRV1,
    mappings: Vec<ToolIdMapEntryV1>,
) -> Result<(), ModelIrError> {
    let by_logical_id = mappings
        .iter()
        .map(|mapping| (mapping.logical_id.as_str(), mapping))
        .collect::<std::collections::BTreeMap<_, _>>();
    for part in request
        .instructions
        .iter_mut()
        .flat_map(|instruction| instruction.content.iter_mut())
        .chain(
            request
                .messages
                .iter_mut()
                .flat_map(|message| message.content.iter_mut()),
        )
    {
        match part {
            ContentPart::ToolCall {
                logical_id,
                tool_kind,
                namespace,
                name,
                ..
            } => {
                let mapping = by_logical_id
                    .get(logical_id.as_str())
                    .ok_or(ModelIrError::ToolContinuationUnavailable)?;
                if mapping.kind != *tool_kind
                    || mapping.name != *name
                    || mapping.namespace != *namespace
                {
                    return Err(ModelIrError::ToolContinuationConflict);
                }
            }
            ContentPart::ToolResult {
                logical_id,
                tool_kind,
                namespace,
                ..
            } => {
                let mapping = by_logical_id
                    .get(logical_id.as_str())
                    .ok_or(ModelIrError::ToolContinuationUnavailable)?;
                if mapping.kind != *tool_kind {
                    return Err(ModelIrError::ToolContinuationConflict);
                }
                *namespace = mapping.namespace.clone();
            }
            ContentPart::Text { .. }
            | ContentPart::Image { .. }
            | ContentPart::ProviderState { .. } => {}
        }
    }
    request.tool_id_map = mappings;
    Ok(())
}

pub(super) fn validate_bindings(bindings: &IngressRequestBindings) -> Result<(), ModelIrError> {
    if bindings
        .provider_state_owner
        .as_ref()
        .is_some_and(|owner| !owner.is_complete())
    {
        return Err(ModelIrError::ProviderStateOwnershipRequired);
    }
    let mut seen = Vec::new();
    for binding in &bindings.tool_id_map {
        if binding.logical_id.trim().is_empty()
            || binding.native_id.trim().is_empty()
            || binding.name.trim().is_empty()
            || binding
                .namespace
                .as_ref()
                .is_some_and(|namespace| namespace.trim().is_empty())
            || !binding.owner.is_complete()
            || seen.iter().any(|(logical_id, owner)| {
                logical_id == &binding.logical_id && owner == &binding.owner
            })
        {
            return Err(ModelIrError::ToolIdBindingRequired(
                binding.logical_id.clone(),
            ));
        }
        seen.push((binding.logical_id.clone(), binding.owner.clone()));
    }
    Ok(())
}
