use super::*;

#[test]
fn selection_grant_deadline_must_fit_both_attempt_and_overall_budget() {
    let now = Instant::now();
    let overall_deadline = now + Duration::from_millis(200);
    let binding = ResolvedTargetBindingId::new(PlanRevision(77), 1);
    let mut selected = SelectedGatewayAttempt {
        request_id: RequestId(9),
        attempt_id: AttemptId(1),
        generation: AttemptGeneration(1),
        binding,
        credential_ref: CredentialRef::new("credential-a").expect("credential ref"),
        route_decision_id: RouteDecisionId(99),
        budget: AttemptBudgetGrant {
            issued_at: now,
            allocated: Duration::from_millis(10),
            deadline: overall_deadline,
        },
    };
    assert!(matches!(
        validate_selection(
            RequestId(9),
            AttemptGeneration(1),
            &selected,
            PlanRevision(77),
            RouteDecisionId(99),
            overall_deadline,
        )
        .unwrap_err(),
        GatewayExecutionError::InvalidSelection
    ));

    selected.budget.allocated = Duration::from_millis(200);
    validate_selection(
        RequestId(9),
        AttemptGeneration(1),
        &selected,
        PlanRevision(77),
        RouteDecisionId(99),
        overall_deadline,
    )
    .expect("deadline fits both explicit budgets");

    selected.budget.allocated = Duration::from_millis(201);
    assert!(matches!(
        validate_selection(
            RequestId(9),
            AttemptGeneration(1),
            &selected,
            PlanRevision(77),
            RouteDecisionId(99),
            overall_deadline,
        )
        .unwrap_err(),
        GatewayExecutionError::InvalidSelection
    ));

    selected.budget.allocated = Duration::from_millis(200);
    selected.budget.deadline = overall_deadline + Duration::from_millis(1);
    assert!(matches!(
        validate_selection(
            RequestId(9),
            AttemptGeneration(1),
            &selected,
            PlanRevision(77),
            RouteDecisionId(99),
            overall_deadline,
        )
        .unwrap_err(),
        GatewayExecutionError::InvalidSelection
    ));
}
