//! Exact catalog controls replace incoming controls before native request serialization.
use super::*;
use crate::server::core_runtime::profiles::{
    CandidateProtocolProfile, NativeReasoningFieldAssignment, NativeReasoningRender,
    NativeReasoningValue, ReasoningControlKind, fixed_reasoning,
};
use crate::server::request_plan::IngressProtocol;
use serde_json::json;
#[test]
fn messages_adaptive_effort_overwrites_caller_disabled_thinking_and_effort() {
    let request = decode_ingress_request(
        IngressProtocol::Messages,
        &json!({
            "model":"alias", "max_tokens":2048, "messages":[{"role":"user","content":"hello"}],
            "thinking":{"type":"disabled"}, "output_config":{"effort":"low"}
        }),
    )
    .unwrap();
    let mut reasoning = fixed_reasoning("high");
    reasoning.control_kind = ReasoningControlKind::Discrete;
    reasoning.render = NativeReasoningRender::ExactFields {
        protocol: IngressProtocol::Messages,
        fields: vec![
            NativeReasoningFieldAssignment {
                path: vec!["output_config".into(), "effort".into()],
                value: NativeReasoningValue::String("high".into()),
            },
            NativeReasoningFieldAssignment {
                path: vec!["thinking".into(), "type".into()],
                value: NativeReasoningValue::String("adaptive".into()),
            },
        ],
    };
    let profile = CandidateProtocolProfile::exact_portable_path(
        IngressProtocol::Messages,
        IngressProtocol::Messages,
        "physical",
        reasoning,
    );
    let native = project_candidate_request(&request, &profile).unwrap();
    assert_eq!(native.body["thinking"], json!({"type":"adaptive"}));
    assert_eq!(native.body["output_config"], json!({"effort":"high"}));
}
