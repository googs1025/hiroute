use super::*;
#[test]
fn legacy_routing_requests_are_rejected_before_accessing_any_ports() {
    // No ports exist: rejection must not read facts, admit an Operation, or run a planner.
    let service = ApplicationService::default();
    for operation in ["PreviewAgentPlanChange", "ApplyAgentPlanChange"] {
        for target in [
            serde_json::json!({"intent":"create"}),
            serde_json::json!({"intent":"update","agent_plan_id":"plan/existing","expected_revision":1}),
        ] {
            let response = service.dispatch(LocalControlRequestV2 {
                schema_version: hiroute_application_api::LOCAL_CONTROL_SCHEMA_V2,
                request_id: "reject-legacy".into(), principal: hiroute_application_api::PrincipalV1::interactive_user(),
                operation_id: operation.into(), protected_grant: None,
                payload: serde_json::json!({"change":{"schema":"hiroute.routing-control-change/v1","target":target,"desired":{}}}),
            });
            assert_eq!(response.error.unwrap().code, ErrorCode::SchemaIncompatible);
        }
    }
}
