use super::*;

#[test]
fn connector_owned_availability_requires_an_explicit_live_connector_fact() {
    let mut connector = complete_management_source();
    connector.provenance = hiroute_domain::ComputeManagementProvenanceV2::ConnectorOwned {
        connector_id: "connector.cpa.codex".into(),
        account_ref: "account/cpa/test".into(),
    };
    connector.validation = Some(hiroute_domain::ComputeManagementValidationV2 {
        approval_operation_id: "operation/approval".into(),
        validation_ref: "validation/connector".into(),
        validation_revision: 1,
    });
    let mut facts = presentation_facts();
    facts.sources[0].identity = ComputeConnectionIdentityV1 {
        access_kind: ComputeConnectionAccessKindV1::Subscription,
        connection_option_id: Some("codex.subscription.global.v1".into()),
        product_label: Some("OpenAI Codex Subscription".into()),
    };
    facts.models[0].billing_class = BillingClass::Subscription;
    facts.models[0].price_contexts.clear();

    for (runtime_availability, expected, reason, ready_count) in [
        (
            ComputeManagementModelRuntimeAvailabilityFactV1::Available,
            ComputeModelAvailabilityV1::Available,
            None,
            1,
        ),
        (
            ComputeManagementModelRuntimeAvailabilityFactV1::SubscriptionUpdating,
            ComputeModelAvailabilityV1::Unavailable,
            Some(ComputeModelAvailabilityReasonV1::SubscriptionUpdating),
            0,
        ),
        (
            ComputeManagementModelRuntimeAvailabilityFactV1::AuthenticationRequired,
            ComputeModelAvailabilityV1::NeedsCredentials,
            Some(ComputeModelAvailabilityReasonV1::AuthenticationRequired),
            0,
        ),
        (
            ComputeManagementModelRuntimeAvailabilityFactV1::ModelNotAllowed,
            ComputeModelAvailabilityV1::Unavailable,
            Some(ComputeModelAvailabilityReasonV1::ModelNotAllowed),
            0,
        ),
        (
            ComputeManagementModelRuntimeAvailabilityFactV1::RuntimeUnavailable,
            ComputeModelAvailabilityV1::Unavailable,
            Some(ComputeModelAvailabilityReasonV1::RuntimeUnavailable),
            0,
        ),
        (
            ComputeManagementModelRuntimeAvailabilityFactV1::BindingAndKeys,
            ComputeModelAvailabilityV1::Unknown,
            Some(ComputeModelAvailabilityReasonV1::FactsUnavailable),
            0,
        ),
    ] {
        facts.models[0].runtime_availability = runtime_availability;
        let result = query_compute_management_with_presentation(
            &OneSourceRepository(connector.clone()),
            &RuntimeFacts::default(),
            &hiroute_domain::WorkspaceId::default(),
            &hiroute_application_api::ComputeManagementQueryV2::default(),
            Some(&facts),
        )
        .unwrap();
        assert_eq!(
            result.sources[0].connection_identity.access_kind,
            ComputeConnectionAccessKindV1::Subscription
        );
        assert_eq!(
            result.sources[0].models[0].presentation.billing_class,
            BillingClass::Subscription
        );
        assert_eq!(
            result.sources[0].models[0].presentation.availability,
            expected
        );
        assert_eq!(result.sources[0].models[0].presentation.reason_code, reason);
        assert_eq!(result.sources[0].ready_model_count, ready_count);
    }

    connector.models[0].execution_eligible = false;
    facts.models[0].runtime_availability =
        ComputeManagementModelRuntimeAvailabilityFactV1::SubscriptionUpdating;
    let updating = query_compute_management_with_presentation(
        &OneSourceRepository(connector.clone()),
        &RuntimeFacts::default(),
        &hiroute_domain::WorkspaceId::default(),
        &hiroute_application_api::ComputeManagementQueryV2::default(),
        Some(&facts),
    )
    .unwrap();
    assert_eq!(
        updating.sources[0].models[0].presentation.reason_code,
        Some(ComputeModelAvailabilityReasonV1::SubscriptionUpdating)
    );

    facts.models[0].runtime_availability =
        ComputeManagementModelRuntimeAvailabilityFactV1::Available;
    let not_allowed = query_compute_management_with_presentation(
        &OneSourceRepository(connector),
        &RuntimeFacts::default(),
        &hiroute_domain::WorkspaceId::default(),
        &hiroute_application_api::ComputeManagementQueryV2::default(),
        Some(&facts),
    )
    .unwrap();
    assert_eq!(
        not_allowed.sources[0].models[0].presentation.reason_code,
        Some(ComputeModelAvailabilityReasonV1::ModelNotAllowed)
    );
}
