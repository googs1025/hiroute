use crate::{ApplicationService, failed, succeeded};
use hiroute_application_api::*;
use serde_json::Value;
pub trait ModelCatalogPort: Send + Sync {
    fn reference_query(
        &self,
        query: ModelCatalogQueryV1,
    ) -> Result<ModelCatalogResultV1, ErrorCode>;
}
pub(crate) fn dispatch(
    service: &ApplicationService,
    request: LocalControlRequestV2,
) -> MachineEnvelopeV2<Value> {
    if request.protected_grant.is_some() {
        return failed(ErrorCode::InvalidArguments, request.request_id);
    }
    let Some(port) = service
        .ports
        .as_ref()
        .and_then(|p| p.model_catalog.as_ref())
    else {
        return failed(ErrorCode::DaemonUnavailable, request.request_id);
    };
    let query = match serde_json::from_value(request.payload) {
        Ok(q) => q,
        Err(_) => return failed(ErrorCode::InvalidArguments, request.request_id),
    };
    match port.reference_query(query) {
        Ok(r) => succeeded(r, request.request_id),
        Err(e) => failed(e, request.request_id),
    }
}
