use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use hiroute_gateway_core::runtime::body::{BudgetTree, StreamBudget};
use hiroute_gateway_core::transport::{
    GatewayRequestHead, GatewayResponseHead, GatewaySession, HttpProtocol, TransportError,
};
use http::{HeaderMap, Method};

use super::*;
use crate::ports::{
    InMemoryToolContinuationAuthority, ToolContinuationAuthority, ToolContinuationScopeV1,
};
use crate::server::core_runtime::model_ir::{ToolIdMapEntryV1, ToolKindV1};
use crate::server::core_runtime::profiles::{CandidateProtocolProfile, fixed_reasoning};

struct FailSecondBodyWrite {
    writes: usize,
    accepted: Vec<Bytes>,
}

#[async_trait]
impl GatewaySession for FailSecondBodyWrite {
    fn request_head(&self) -> Result<GatewayRequestHead, TransportError> {
        Ok(request_head())
    }

    async fn read_request_body(&mut self) -> Result<Option<Bytes>, TransportError> {
        Ok(None)
    }

    async fn write_response_head(
        &mut self,
        _head: GatewayResponseHead,
    ) -> Result<(), TransportError> {
        Ok(())
    }

    async fn write_response_body(
        &mut self,
        body: Bytes,
        _end_stream: bool,
    ) -> Result<(), TransportError> {
        self.writes += 1;
        if self.writes == 2 {
            return Err(TransportError::Io("controlled downstream reset".into()));
        }
        self.accepted.push(body);
        Ok(())
    }
}

#[tokio::test]
async fn production_response_sink_keeps_only_the_tool_unit_accepted_before_reset() {
    let authority: Arc<dyn ToolContinuationAuthority> =
        Arc::new(InMemoryToolContinuationAuthority::new(4, Duration::from_secs(30)).unwrap());
    let scope = ToolContinuationScopeV1 {
        authority_id: "authority:runtime-test".into(),
        authority_epoch: 1,
        grant_id: "grant:runtime-test".into(),
        grant_generation: 1,
        served_model_id: "runtime-test".into(),
        route: hiroute_domain::ModelRequestRouteV2::Plan {
            revision: 1,
            semantic_digest: hiroute_domain::CanonicalDigest::of_bytes(b"runtime-test-plan"),
        },
    };
    let issuance = authority.begin(scope.clone()).unwrap();
    let profile = CandidateProtocolProfile::exact_portable_path(
        IngressProtocol::Responses,
        IngressProtocol::Responses,
        "native-runtime-test",
        fixed_reasoning("fixed"),
    );
    let owner = profile.exact_provider_path().unwrap();
    let first_id = format!("hiroute_tool_v1_{}", "a".repeat(64));
    let second_id = format!("hiroute_tool_v1_{}", "b".repeat(64));
    let first = ToolIdMapEntryV1 {
        logical_id: first_id.clone(),
        native_id: "native-first".into(),
        kind: ToolKindV1::Function,
        namespace: Some("first-group".into()),
        name: "shared".into(),
        owner: owner.clone(),
    };
    let second = ToolIdMapEntryV1 {
        logical_id: second_id.clone(),
        native_id: "native-second".into(),
        kind: ToolKindV1::Function,
        namespace: Some("second-group".into()),
        name: "shared".into(),
        owner,
    };
    let now = Instant::now();
    authority
        .record_pending(&issuance, first.clone(), now)
        .unwrap();
    authority.record_pending(&issuance, second, now).unwrap();
    let active = adapters::ActiveToolContinuation::new(Arc::clone(&authority), issuance.clone());
    let guard = ToolContinuationRequestGuard(active.clone());
    let observation = observation::accepted_request_for_runtime_test();
    let budget = budget();
    let mut downstream = FailSecondBodyWrite {
        writes: 0,
        accepted: Vec::new(),
    };
    let mut session = ReplayBodySession {
        inner: &mut downstream,
        request_head: request_head(),
        request_body: None,
        budget,
        response_capture: AcceptedResponseCapture::new(observation.clone()),
        response_started: true,
        runtime_state_authority: Default::default(),
        continuation_scanner: active.scanner(),
    };

    session
        .write_response_body(
            Bytes::from(format!("data: {{\"call_id\":\"{first_id}\"}}\n\n")),
            false,
        )
        .await
        .unwrap();
    assert!(observation.has_accepted_attempt());
    assert_eq!(
        observation::accepted_frame_count_for_runtime_test(&observation),
        1
    );

    let error = session
        .write_response_body(
            Bytes::from(format!("data: {{\"call_id\":\"{second_id}\"}}\n\n")),
            true,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, TransportError::Io(_)));
    assert_eq!(
        observation::accepted_frame_count_for_runtime_test(&observation),
        1
    );
    drop(session);
    drop(guard);

    assert_eq!(
        authority.resolve(&scope, &[first_id], now).unwrap(),
        vec![first]
    );
    assert!(authority.resolve(&scope, &[second_id], now).is_err());
    assert_eq!(downstream.accepted.len(), 1);
}

fn request_head() -> GatewayRequestHead {
    GatewayRequestHead {
        method: Method::POST,
        path_and_query: Arc::from("/v1/responses"),
        authority: Some(Arc::from("gateway.test")),
        headers: HeaderMap::new(),
        protocol: HttpProtocol::Http1,
    }
}

fn budget() -> StreamBudget {
    BudgetTree::new(1024 * 1024, 1024 * 1024)
        .unwrap()
        .stream(512 * 1024)
        .unwrap()
}
