use super::*;

#[test]
fn occupied_durable_claim_is_a_preview_conflict_without_consuming_the_other_capability() {
    let ports = MemoryPorts::default();
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let first = request(&coordinator, Some("capability-a"), false);
    let mut second = first.clone();
    second.idempotency_key = "idem-b".into();
    second.apply_capability = Some("capability-b".into());
    for (capability, request) in [("capability-a", &first), ("capability-b", &second)] {
        ports.grant(
            capability,
            &request.accept_digest,
            &request.expected_revisions,
        );
    }
    let first = coordinator
        .accept(&WorkspaceId::default(), &principal(), first)
        .unwrap();
    let writes = ports.state.borrow().durable_writes;
    let rejected = coordinator
        .accept(&WorkspaceId::default(), &principal(), second)
        .err()
        .expect("occupied claim must reject admission");
    assert!(matches!(rejected, TransactionError::ChangePreviewStale));
    {
        let state = ports.state.borrow();
        assert_eq!(state.operations.len(), 1);
        assert_eq!(state.durable_writes, writes);
        assert_eq!(
            state.writer.as_deref(),
            Some(first.operation().operation_id.as_str())
        );
        assert!(
            state.grants[CanonicalDigest::of_bytes(b"capability-b").as_str()]
                .consumed
                .is_none()
        );
    }
    assert_eq!(
        coordinator
            .run(&first.operation().operation_id)
            .unwrap()
            .state,
        OperationState::Succeeded
    );
}

#[test]
fn accepted_handoff_uses_committed_value_and_reloads_after_another_writer() {
    for intervening_writer in [false, true] {
        let ports = MemoryPorts::default();
        let runtime = TransactionRuntime::default();
        let coordinator = open(&ports, &runtime);
        let request = request(&coordinator, Some("capability-a"), false);
        ports.grant(
            "capability-a",
            &request.accept_digest,
            &request.expected_revisions,
        );
        let accepted = coordinator
            .accept(&WorkspaceId::default(), &principal(), request)
            .unwrap();
        let original_plan = accepted.operation.plan.clone();
        if intervening_writer {
            coordinator.run(&accepted.operation.operation_id).unwrap();
        }
        ports.state.borrow_mut().operation_reads = 0;
        let writes = ports.state.borrow().durable_writes;
        let result = coordinator.run_accepted(accepted).unwrap();
        assert_eq!(result.state, OperationState::Succeeded);
        assert_eq!(
            ports.state.borrow().operation_reads,
            usize::from(intervening_writer)
        );
        if intervening_writer {
            assert_eq!(ports.state.borrow().durable_writes, writes);
        } else {
            assert!(std::sync::Arc::ptr_eq(&original_plan, &result.plan));
        }
    }
}

#[test]
fn startup_reuses_the_validated_recoverable_operation_within_its_writer() {
    let ports = MemoryPorts::default();
    let runtime = TransactionRuntime::default();
    let coordinator = open(&ports, &runtime);
    let request = request(&coordinator, Some("capability-a"), false);
    ports.grant(
        "capability-a",
        &request.accept_digest,
        &request.expected_revisions,
    );
    let accepted = coordinator
        .accept(&WorkspaceId::default(), &principal(), request)
        .unwrap();
    let plan = accepted.operation().plan.clone();
    ports.state.borrow_mut().operation_reads = 0;
    let recovered = coordinator.reconcile_startup_and_open().unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].state, OperationState::Succeeded);
    assert_eq!(ports.state.borrow().operation_reads, 0);
    assert!(std::sync::Arc::ptr_eq(&plan, &recovered[0].plan));
}
