use super::*;
use crate::publication::admission::AdmissionAction;

fn binding() -> RunSafetyBinding {
    RunSafetyBinding {
        workspace: WorkspaceId::default(),
        daemon_epoch: "epoch-a".into(),
        permit_id: "permit-a".into(),
        permit_generation: 2,
        expires_at_ms: 1000,
    }
}
fn op(ch: char) -> OperationId {
    OperationId::parse(format!("op_{}", ch.to_string().repeat(32))).unwrap()
}
fn scope() -> Vec<RunAuthorizationScope> {
    vec![RunAuthorizationScope::Permit {
        id: "permit-a".into(),
        through_generation: 3,
    }]
}

#[test]
fn cached_run_binding_rechecks_barrier_without_taking_the_held_gate() {
    let gate = Arc::new(SharedAdmissionGate::new());
    let safety = RunSafetyProjection::new(gate.clone(), "epoch-a".into()).unwrap();
    let cached = binding();
    assert!(safety.check(&cached, 10).is_err());
    safety.finish_startup_recovery();
    assert!(safety.check(&cached, 10).is_ok());
    let operation = op('a');
    let guard = gate
        .enter(
            &cached.workspace,
            &BTreeSet::from([AdmissionSubject::Permit("permit-a".into())]),
            AdmissionAction::AuthorizationChange,
            operation.as_str(),
        )
        .unwrap();
    safety.install_deny(&guard, &operation, &scope()).unwrap();
    // Same thread still holds gate: this would deadlock if Gateway took the management lock.
    assert_eq!(
        safety.check(&cached, 10),
        Err(DelegationErrorV1::PermissionDenied)
    );
    let mut independent = cached.clone();
    independent.permit_id = "permit-b".into();
    assert!(safety.check(&independent, 10).is_ok());
    safety.mark_recorded(&guard, &operation, &scope()).unwrap();
    assert!(safety.check(&cached, 10).is_err());
    let mut renewed = cached;
    renewed.permit_generation = 4;
    assert!(safety.check(&renewed, 10).is_ok());
}

#[test]
fn unresolved_operation_cannot_be_cleared_by_another_operation_or_generation_bump() {
    let gate = Arc::new(SharedAdmissionGate::new());
    let safety = RunSafetyProjection::new(gate.clone(), "epoch-a".into()).unwrap();
    safety.finish_startup_recovery();
    let mut lease = binding();
    let operation_a = op('a');
    let guard_a = gate
        .enter(
            &lease.workspace,
            &BTreeSet::from([AdmissionSubject::Permit("permit-a".into())]),
            AdmissionAction::AuthorizationChange,
            operation_a.as_str(),
        )
        .unwrap();
    safety
        .install_deny(&guard_a, &operation_a, &scope())
        .unwrap();
    drop(guard_a);
    let operation_b = op('b');
    let guard_b = gate
        .enter(
            &lease.workspace,
            &BTreeSet::from([AdmissionSubject::Permit("permit-a".into())]),
            AdmissionAction::AuthorizationChange,
            operation_b.as_str(),
        )
        .unwrap();
    safety
        .install_deny(&guard_b, &operation_b, &scope())
        .unwrap();
    drop(guard_b);
    let guard_a = gate
        .enter(
            &lease.workspace,
            &BTreeSet::from([AdmissionSubject::Permit("permit-a".into())]),
            AdmissionAction::AuthorizationChange,
            operation_a.as_str(),
        )
        .unwrap();
    assert!(
        safety
            .mark_recorded(&guard_a, &operation_b, &scope())
            .is_err()
    );
    safety
        .mark_recorded(&guard_a, &operation_a, &scope())
        .unwrap();
    drop(guard_a);
    lease.permit_generation = 4;
    assert!(safety.check(&lease, 10).is_err());
    let guard_b = gate
        .enter(
            &lease.workspace,
            &BTreeSet::from([AdmissionSubject::Permit("permit-a".into())]),
            AdmissionAction::AuthorizationChange,
            operation_b.as_str(),
        )
        .unwrap();
    safety
        .mark_recorded(&guard_b, &operation_b, &scope())
        .unwrap();
    assert!(safety.check(&lease, 10).is_ok());
    assert!(safety.check(&lease, 1000).is_err());
    lease.daemon_epoch = "old-epoch".into();
    assert!(safety.check(&lease, 10).is_err());
}

#[test]
fn second_gate_or_uncovered_scope_cannot_mutate_safety_projection() {
    let gate = Arc::new(SharedAdmissionGate::new());
    let other = SharedAdmissionGate::new();
    let safety = RunSafetyProjection::new(gate.clone(), "epoch-a".into()).unwrap();
    let workspace = WorkspaceId::default();
    let guard = other
        .enter(
            &workspace,
            &BTreeSet::from([AdmissionSubject::Grant("grant-a".into())]),
            AdmissionAction::AuthorizationChange,
            "other",
        )
        .unwrap();
    assert!(safety.install_deny(&guard, &op('a'), &scope()).is_err());
    let guard = gate
        .enter(
            &workspace,
            &BTreeSet::from([AdmissionSubject::Permit("permit-a".into())]),
            AdmissionAction::AuthorizationChange,
            "other",
        )
        .unwrap();
    assert!(safety.install_deny(&guard, &op('a'), &scope()).is_err());
}
