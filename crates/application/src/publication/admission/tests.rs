use std::sync::{Arc, Barrier, TryLockError};

use super::*;

fn scope(id: &str) -> BTreeSet<AdmissionSubject> {
    BTreeSet::from([AdmissionSubject::Plan(AgentPlanId::parse(id).unwrap())])
}

#[test]
fn shared_gate_keeps_validation_reservation_and_acceptance_in_one_critical_section() {
    let gate = Arc::new(SharedAdmissionGate::new());
    let workspace = WorkspaceId::default();
    let guard = gate
        .enter(
            &workspace,
            &scope("plan/a"),
            AdmissionAction::Start,
            "run/a",
        )
        .unwrap();
    let rendezvous = Arc::new(Barrier::new(2));
    let peer_gate = gate.clone();
    let peer_barrier = rendezvous.clone();
    let peer = std::thread::spawn(move || {
        // A lifecycle writer cannot enter between reservation and runtime acceptance.
        assert!(matches!(
            peer_gate.state.try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        peer_barrier.wait();
        let _switch = peer_gate
            .enter(
                &workspace,
                &scope("plan/a"),
                AdmissionAction::PlanChange,
                "op/disable",
            )
            .unwrap();
    });
    rendezvous.wait();
    assert!(guard.belongs_to(&gate));
    assert!(!guard.belongs_to(&SharedAdmissionGate::new()));
    drop(guard); // Only after the caller has committed its runtime acceptance.
    peer.join().unwrap();
}

#[test]
fn pending_operation_blocks_only_affected_scope_and_its_owner_can_reconcile() {
    let gate = SharedAdmissionGate::new();
    let workspace = WorkspaceId::default();
    gate.enter(
        &workspace,
        &scope("plan/a"),
        AdmissionAction::PlanChange,
        "op/a",
    )
    .unwrap()
    .mark_recovery_required()
    .unwrap();
    assert!(matches!(
        gate.enter(
            &workspace,
            &scope("plan/a"),
            AdmissionAction::Start,
            "run/a"
        ),
        Err(AdmissionGateError::RecoveryRequired)
    ));
    assert!(
        gate.enter(
            &workspace,
            &scope("plan/b"),
            AdmissionAction::Start,
            "run/b"
        )
        .is_ok()
    );
    assert!(matches!(
        gate.enter(
            &workspace,
            &scope("plan/a"),
            AdmissionAction::Recovery,
            "op/b"
        ),
        Err(AdmissionGateError::RecoveryRequired)
    ));
    gate.enter(
        &workspace,
        &scope("plan/a"),
        AdmissionAction::Recovery,
        "op/a",
    )
    .unwrap()
    .complete_recovery()
    .unwrap();
    assert!(
        gate.enter(
            &workspace,
            &scope("plan/a"),
            AdmissionAction::Continue,
            "run/continue"
        )
        .is_ok()
    );
}

#[test]
fn grant_and_permit_scopes_participate_without_transferring_their_business_state() {
    let gate = SharedAdmissionGate::new();
    let workspace = WorkspaceId::default();
    let grant = AdmissionSubject::Grant("grant/a".into());
    let permit = AdmissionSubject::Permit("permit/a".into());
    let affected = BTreeSet::from([grant.clone(), permit.clone()]);
    gate.enter(
        &workspace,
        &affected,
        AdmissionAction::AuthorizationChange,
        "op/revoke",
    )
    .unwrap()
    .mark_recovery_required()
    .unwrap();
    for subject in [grant, permit] {
        let mut requested = scope("plan/a");
        requested.insert(subject);
        assert!(matches!(
            gate.enter(&workspace, &requested, AdmissionAction::Start, "run/a"),
            Err(AdmissionGateError::RecoveryRequired)
        ));
    }
    assert!(
        gate.enter(
            &WorkspaceId::parse("workspace/other").unwrap(),
            &affected,
            AdmissionAction::Start,
            "run/a"
        )
        .is_ok()
    );
}

#[test]
fn invalid_or_poisoned_gate_never_returns_a_guard() {
    let gate = SharedAdmissionGate::new();
    assert!(matches!(
        gate.enter(
            &WorkspaceId::default(),
            &BTreeSet::new(),
            AdmissionAction::Start,
            "run/a"
        ),
        Err(AdmissionGateError::InvalidIdentity)
    ));
    let _ = std::panic::catch_unwind(|| {
        let _guard = gate
            .enter(
                &WorkspaceId::default(),
                &scope("plan/a"),
                AdmissionAction::Start,
                "run/a",
            )
            .unwrap();
        panic!("acceptance state uncertain");
    });
    assert!(matches!(
        gate.enter(
            &WorkspaceId::default(),
            &scope("plan/a"),
            AdmissionAction::Start,
            "run/b"
        ),
        Err(AdmissionGateError::Unavailable)
    ));
}
