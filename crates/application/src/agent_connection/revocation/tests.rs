use super::*;
use crate::delegation::safety::RunSafetyBinding;
use hiroute_domain::{
    AgentCollaborationCredential, AgentCollaborationGrant, OperationId, PortError, PortErrorCode,
    PortResult, WorkspaceId,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Store {
    fail: Mutex<bool>,
    committed: Mutex<Vec<AgentCollaborationRevocation>>,
}
impl AgentCollaborationRevocationStorePort for Store {
    fn persist_collaboration_revocation(
        &self,
        checkpoint: &AgentCollaborationRevocation,
    ) -> PortResult<()> {
        if *self.fail.lock().unwrap() {
            return Err(PortError::new(
                PortErrorCode::Unavailable,
                "test.commit.uncertain",
            ));
        }
        let mut committed = self.committed.lock().unwrap();
        if !committed.contains(checkpoint) {
            committed.push(checkpoint.clone());
        }
        Ok(())
    }
}
fn checkpoint() -> AgentCollaborationRevocation {
    let grant = AgentCollaborationGrant::issue(
        WorkspaceId::default(),
        "context/one".into(),
        "collaboration-grant/one".into(),
        1,
        BTreeSet::new(),
        &AgentCollaborationCredential::from_csprng_entropy([1; 32]),
    )
    .unwrap();
    grant
        .plan_revocation(
            OperationId::parse("op_00112233445566778899aabbccddeeff").unwrap(),
            1,
        )
        .unwrap()
}
fn binding() -> RunSafetyBinding {
    RunSafetyBinding {
        workspace: WorkspaceId::default(),
        daemon_epoch: "epoch".into(),
        permit_id: "permit/one".into(),
        permit_generation: 1,
        expires_at_ms: 1000,
    }
}
#[test]
fn collaboration_revoke_does_not_revoke_instance_worker_permits() {
    let gate = Arc::new(SharedAdmissionGate::new());
    let safety = RunSafetyProjection::new(gate.clone(), "epoch".into()).unwrap();
    safety.finish_startup_recovery();
    let checkpoint = checkpoint();
    let store = Store::default();
    *store.fail.lock().unwrap() = true;
    assert!(safety.check(&binding(), 1).is_ok());
    assert!(record_collaboration_revocation(&gate, &safety, &store, &checkpoint, false).is_err());
    // The public Worker runtime is instance scoped: collaboration-grant revocation still
    // serializes admissions, but cannot revoke an independent run permit.
    assert!(safety.check(&binding(), 1).is_ok());
    *store.fail.lock().unwrap() = false;
    record_collaboration_revocation(&gate, &safety, &store, &checkpoint, true).unwrap();
    record_collaboration_revocation(&gate, &safety, &store, &checkpoint, true).unwrap();
    assert_eq!(store.committed.lock().unwrap().as_slice(), &[checkpoint]);
    assert!(safety.check(&binding(), 1).is_ok());
}
#[test]
fn collaboration_revoke_cannot_use_a_second_composition_gate() {
    let gate = Arc::new(SharedAdmissionGate::new());
    let safety = RunSafetyProjection::new(gate.clone(), "epoch".into()).unwrap();
    let accidental_second_gate = SharedAdmissionGate::new();
    let store = Store::default();
    assert!(
        record_collaboration_revocation(
            &accidental_second_gate,
            &safety,
            &store,
            &checkpoint(),
            false
        )
        .is_err()
    );
    assert!(store.committed.lock().unwrap().is_empty());
}
#[test]
fn collaboration_revoke_sorts_after_an_admission_holding_the_shared_guard() {
    let gate = Arc::new(SharedAdmissionGate::new());
    let safety = Arc::new(RunSafetyProjection::new(gate.clone(), "epoch".into()).unwrap());
    safety.finish_startup_recovery();
    let store = Arc::new(Store::default());
    let checkpoint = checkpoint();
    let subjects = BTreeSet::from([AdmissionSubject::Grant(checkpoint.after.grant_id.clone())]);
    let admission = gate
        .enter(
            &WorkspaceId::default(),
            &subjects,
            AdmissionAction::Start,
            "task/start",
        )
        .unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let worker_gate = gate.clone();
    let worker_safety = safety.clone();
    let worker_store = store.clone();
    let worker = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        record_collaboration_revocation(
            &worker_gate,
            &worker_safety,
            worker_store.as_ref(),
            &checkpoint,
            false,
        )
        .unwrap();
    });
    ready_rx.recv().unwrap();
    assert!(store.committed.lock().unwrap().is_empty());
    assert!(safety.check(&binding(), 1).is_ok());
    // The real 20 caller commits runtime acceptance before dropping this guard. This test
    // proves the shared exclusion boundary, not production Worker acceptance/cancellation.
    drop(admission);
    worker.join().unwrap();
    assert_eq!(store.committed.lock().unwrap().len(), 1);
    assert!(safety.check(&binding(), 1).is_ok());
}
