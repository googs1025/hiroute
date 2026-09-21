mod history;
mod identity;
mod store;

use std::time::Instant;

use http::HeaderMap;
use serde_json::Value;

use crate::server::core_runtime::model_ir::ModelRequestIRV1;
use crate::server::request_plan::{AuthorizedRequestPlan, IngressProtocol};

pub(crate) use history::{HistoryEvidence, visible_history};
pub(crate) use identity::{ContextIdentityFacts, identity_facts};
pub(crate) use store::{ContextHoldKey, ContextHoldStore, HoldCompletion, HoldTicket};

pub(crate) struct ContextRequest<'a> {
    pub(crate) ingress: IngressProtocol,
    pub(crate) headers: &'a HeaderMap,
    pub(crate) document: &'a Value,
    pub(crate) request: &'a ModelRequestIRV1,
}

pub(crate) fn begin_context(
    store: &ContextHoldStore,
    observation_key: &[u8; 32],
    authorized: &AuthorizedRequestPlan,
    input: ContextRequest<'_>,
    now: Instant,
) -> (ContextIdentityFacts, Option<HoldTicket>) {
    let ContextRequest {
        ingress,
        headers,
        document,
        request,
    } = input;
    let identity =
        store
            .digest_key()
            .map_or_else(ContextIdentityFacts::request_scoped, |hold_key| {
                identity_facts(
                    authorized,
                    ingress,
                    headers,
                    document,
                    request,
                    &hold_key,
                    observation_key,
                )
            });
    let ticket = store.digest_key().and_then(|hold_key| {
        let history = visible_history(request, &hold_key)?;
        let key = ContextHoldKey::from_request(authorized, ingress, &identity, &hold_key)?;
        store.begin(key, &history, now)
    });
    (identity, ticket)
}
