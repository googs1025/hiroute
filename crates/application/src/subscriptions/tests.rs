use std::collections::BTreeMap;

use hiroute_application_api::{
    ComputeCandidateInputStateV2, ComputeCandidateIssueV2, ComputeCandidateModelViewV2,
    ComputeCandidateProvenanceKindV2, ComputeCheckCorrelationV2, ComputeModelMembershipV2,
};

use super::*;

fn operation(id: &str) -> OperationReferenceV1 {
    OperationReferenceV1 {
        operation_id: id.to_owned(),
        state: "accepted".to_owned(),
        sequence: 1,
        cancellable: true,
    }
}

fn candidate(
    producer: ComputeCandidateProducerV2,
    fact_state: ComputeCandidateFactStateV2,
) -> ComputeCandidateViewV2 {
    ComputeCandidateViewV2 {
        candidate: ComputeCandidateRefV2 {
            candidate_ref: "candidate/one".to_owned(),
            candidate_revision: 42,
        },
        correlation: ComputeCheckCorrelationV2 {
            candidate_ref: "candidate/one".to_owned(),
            edit_revision: 7,
            check_id: "check/7".to_owned(),
            input_digest: CanonicalDigest::of_bytes(b"input"),
        },
        producer,
        provenance: if producer == ComputeCandidateProducerV2::Cpa {
            ComputeCandidateProvenanceKindV2::ConnectorOwned
        } else {
            ComputeCandidateProvenanceKindV2::UserConfigured
        },
        display_name: "Candidate".to_owned(),
        existing_source_id: None,
        models: vec![ComputeCandidateModelViewV2 {
            model_ref: "model/one".to_owned(),
            upstream_model_id: "upstream-one".to_owned(),
            display_name: "One".to_owned(),
            membership: ComputeModelMembershipV2::Observed,
            fact_basis:
                hiroute_application_api::ComputeCandidateModelFactBasisV2::ConnectorVerified,
            selectable: true,
            reason: None,
        }],
        input_state: ComputeCandidateInputStateV2::NotRequired,
        fact_state,
        validation: None,
        issues: Vec::<ComputeCandidateIssueV2>::new(),
    }
}

fn revisions() -> RevisionSetV1 {
    RevisionSetV1 {
        target: 1,
        dependencies: BTreeMap::new(),
    }
}

fn checked_candidate(validation: &ComputeValidationRefV2) -> ComputeCandidateViewV2 {
    let mut checked = candidate(
        ComputeCandidateProducerV2::Cpa,
        ComputeCandidateFactStateV2::Complete,
    );
    checked.candidate.candidate_revision = 43;
    checked.validation = Some(validation.clone());
    checked
}

#[test]
fn native_pending_input_can_be_saved_disabled_but_not_ready() {
    let candidate = candidate(
        ComputeCandidateProducerV2::Native,
        ComputeCandidateFactStateV2::PendingCredential,
    );

    let disabled =
        prepare_compute_management_change(&candidate, Vec::new(), false, revisions()).unwrap();
    assert_eq!(disabled.intent, ComputeManagementIntentV2::SaveDisabled);
    assert_eq!(
        prepare_compute_management_change(
            &candidate,
            vec!["model/one".to_owned()],
            true,
            revisions(),
        ),
        Err(SubscriptionPreparationError::NeedsCredential)
    );
}

#[test]
fn cpa_pending_approval_must_enter_check_a_before_save_b() {
    let candidate = candidate(
        ComputeCandidateProducerV2::Cpa,
        ComputeCandidateFactStateV2::PendingApproval,
    );
    assert_eq!(
        prepare_compute_management_change(&candidate, Vec::new(), false, revisions()),
        Err(SubscriptionPreparationError::NeedsApproval)
    );
    let check =
        prepare_subscription_check(&candidate, CanonicalDigest::of_bytes(b"evidence"), None)
            .unwrap();
    assert_eq!(check.candidate.candidate_revision, 42);
}

#[test]
fn complete_candidate_still_only_produces_save_intent() {
    let candidate = candidate(
        ComputeCandidateProducerV2::Native,
        ComputeCandidateFactStateV2::Complete,
    );
    let change = prepare_compute_management_change(
        &candidate,
        vec!["model/one".to_owned()],
        true,
        revisions(),
    )
    .unwrap();
    assert_eq!(change.intent, ComputeManagementIntentV2::SaveReady);
    assert!(matches!(
        change.subject,
        ComputeManagementSubjectV2::Candidate { .. }
    ));
}

#[test]
fn disabled_save_still_rejects_an_unselectable_model() {
    let mut candidate = candidate(
        ComputeCandidateProducerV2::Native,
        ComputeCandidateFactStateV2::Complete,
    );
    candidate.models[0].selectable = false;
    candidate.models[0].reason = Some("inventory_only".to_owned());

    assert_eq!(
        prepare_compute_management_change(
            &candidate,
            vec!["model/one".to_owned()],
            false,
            revisions(),
        ),
        Err(SubscriptionPreparationError::InvalidModelSelection)
    );
}

#[test]
fn edit_revision_filters_late_results_without_becoming_candidate_revision() {
    let candidate = candidate(
        ComputeCandidateProducerV2::Native,
        ComputeCandidateFactStateV2::Complete,
    );
    assert!(candidate_result_is_current(&candidate, 7, "check/7"));
    assert!(!candidate_result_is_current(&candidate, 42, "check/7"));
    assert_eq!(candidate.candidate.candidate_revision, 42);
}

#[test]
fn close_after_save_b_only_observes_b() {
    let operation_a = operation("operation-a");
    let operation_b = operation("operation-b");
    let validation = ComputeValidationRefV2 {
        approval_operation: operation_a.clone(),
        validation_ref: "validation/one".to_owned(),
        validation_revision: 1,
    };
    let check = ComputeSubscriptionCheckResultV2 {
        candidate: ComputeCandidateRefV2 {
            candidate_ref: "candidate/one".to_owned(),
            candidate_revision: 42,
        },
        approval_operation: operation_a,
        status: ComputeSubscriptionCheckStatusV2::Verified,
        save_operation: None,
        validation: Some(validation.clone()),
        checked_candidate: Some(checked_candidate(&validation)),
        reason: None,
    };

    assert_eq!(
        subscription_close_action(Some(&check), Some(&operation_b), "cancel-a").unwrap(),
        SubscriptionCloseAction::ObserveSave(operation("operation-b"))
    );

    let mut check_with_b = check;
    check_with_b.status = ComputeSubscriptionCheckStatusV2::Failed;
    check_with_b.save_operation = Some(operation_b);
    assert_eq!(
        subscription_close_action(Some(&check_with_b), None, "cancel-a").unwrap(),
        SubscriptionCloseAction::ObserveSave(operation("operation-b"))
    );
}

#[test]
fn verified_without_b_releases_validation_and_checking_cancels_a() {
    let operation_a = operation("operation-a");
    let validation = ComputeValidationRefV2 {
        approval_operation: operation_a.clone(),
        validation_ref: "validation/one".to_owned(),
        validation_revision: 1,
    };
    let mut check = ComputeSubscriptionCheckResultV2 {
        candidate: ComputeCandidateRefV2 {
            candidate_ref: "candidate/one".to_owned(),
            candidate_revision: 42,
        },
        approval_operation: operation_a.clone(),
        status: ComputeSubscriptionCheckStatusV2::Verified,
        save_operation: None,
        validation: Some(validation.clone()),
        checked_candidate: Some(checked_candidate(&validation)),
        reason: None,
    };
    assert_eq!(
        subscription_close_action(Some(&check), None, "cancel-a").unwrap(),
        SubscriptionCloseAction::ReleaseValidation(validation)
    );

    check.status = ComputeSubscriptionCheckStatusV2::Checking;
    check.validation = None;
    check.checked_candidate = None;
    assert_eq!(
        subscription_close_action(Some(&check), None, "cancel-a").unwrap(),
        SubscriptionCloseAction::CancelApproval(OperationCancelRequestV1 {
            operation_id: operation_a.operation_id,
            idempotency_key: "cancel-a".to_owned(),
        })
    );
}
