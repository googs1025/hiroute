use std::collections::BTreeSet;

use super::*;
use crate::{CanonicalDigest, CredentialRefV1};
use serde_json::json;

fn digest(label: &str) -> CanonicalDigest {
    CanonicalDigest::of_bytes(label.as_bytes())
}
#[test]
fn runtime_failures_keep_credential_and_binding_scopes_distinct() {
    let credential = RuntimeAvailabilityStateV1::ready("credential/one").unwrap();
    let binding = RuntimeAvailabilityStateV1::ready("binding/one").unwrap();
    let credential = credential.credential_quota(0, 100, None).unwrap();
    let binding = binding.binding_overload(0, 100, Some(130)).unwrap();
    assert_eq!(credential.reason, RuntimeReason::CredentialQuota429);
    assert_eq!(binding.reason, RuntimeReason::BindingOverload);
    assert_eq!(
        credential.credential_quota(0, 100, None),
        Err(ComputeContractError::GenerationConflict)
    );
    let credential = credential
        .credential_quota(1, 200, None)
        .unwrap()
        .credential_quota(2, 500, None)
        .unwrap();
    assert_eq!(credential.availability, RuntimeAvailability::Cooling);
    assert_eq!(credential.cooldown_until, Some(2_300));
    let credential = credential.credential_quota(3, 2_400, None).unwrap();
    assert_eq!(credential.availability, RuntimeAvailability::Disabled);
    assert_eq!(credential.cooldown_until, None);
    assert_eq!(
        credential.successful(4),
        Err(ComputeContractError::InvalidRuntimeState)
    );
    let recovered = RuntimeAvailabilityStateV1::ready("credential/recovered")
        .unwrap()
        .credential_quota(0, 100, None)
        .unwrap()
        .successful(1)
        .unwrap();
    assert_eq!(recovered.availability, RuntimeAvailability::Ready);
    assert_eq!(recovered.failure_window, 0);
    let binding = binding
        .binding_overload(1, 200, Some(300))
        .unwrap()
        .binding_overload(2, 400, None)
        .unwrap();
    assert_eq!(binding.cooldown_until, Some(2_200));
    let binding = binding.binding_overload(3, 2_300, None).unwrap();
    assert_eq!(binding.availability, RuntimeAvailability::Disabled);
    let permanent = RuntimeAvailabilityStateV1::ready("credential/two")
        .unwrap()
        .permanent_insufficient(0)
        .unwrap();
    assert_eq!(permanent.reason, RuntimeReason::PermanentInsufficient);
    permanent.validate().unwrap();
}

#[test]
fn compute_homogeneous_two_key_pool_preserves_order_and_rejects_mixed_scope() {
    let identity = digest("source-identity");
    let offer_evidence = digest("offer-fixture-payg");
    let destination = "connection-option/fixture.payg.test.v1".to_owned();
    let credential = |id: &str| {
        CredentialRefV1::new(
            id,
            "source/primary",
            "hirouted",
            "provider-auth",
            [destination.clone()],
            1,
        )
        .unwrap()
    };
    let binding = SourceBindingV1 {
        binding_id: "binding/primary".into(),
        revision: 1,
        source_id: "primary".into(),
        source_revision: 1,
        source_identity_digest: identity.clone(),
        model_data_bundle_version: "models-v1".into(),
        capability_slice_version: "capabilities-v1".into(),
        offer_ref: "offer.fixture.payg".into(),
        offer_evidence_digest: offer_evidence.clone(),
        billing_class: BillingClass::Paid,
        model_configuration_id: "model.primary".into(),
        upstream_model_id: "upstream-primary".into(),
        capability_id: "capability.primary".into(),
        credential_pool_id: Some("pool/primary".into()),
    };
    let pool = CredentialPoolV1 {
        pool_id: "pool/primary".into(),
        binding_id: binding.binding_id.clone(),
        binding_revision: binding.revision,
        binding_digest: CanonicalDigest::of(&binding).unwrap(),
        source_id: "primary".into(),
        source_revision: 1,
        connection_option_id: "fixture.payg.test.v1".into(),
        source_identity_digest: identity,
        offer_ref: binding.offer_ref.clone(),
        offer_revision: 1,
        offer_evidence_digest: offer_evidence,
        billing_class: BillingClass::Paid,
        model_configuration_id: binding.model_configuration_id.clone(),
        authentication: AuthenticationKind::ProviderApiKey,
        revision: 1,
        credentials: vec![
            PoolCredentialV1 {
                credential: credential("credential/key-a"),
                fingerprint: digest("key-a"),
                ordinal: 0,
                enabled: true,
            },
            PoolCredentialV1 {
                credential: credential("credential/key-b"),
                fingerprint: digest("key-b"),
                ordinal: 1,
                enabled: true,
            },
        ],
    };
    pool.validate().unwrap();
    pool.validate_against_binding(&binding).unwrap();
    let first = pool
        .identity()
        .materialize_first(credential("credential/first"), digest("first"))
        .unwrap();
    CredentialPoolMutationV1::from_registered_planner(CredentialPoolMutationKind::Add, None, first)
        .unwrap();
    let mut second_binding = binding.clone();
    second_binding.binding_id = "binding/secondary".into();
    second_binding.model_configuration_id = "model.secondary".into();
    second_binding.upstream_model_id = "upstream-secondary".into();
    second_binding.capability_id = "capability.secondary".into();
    assert_eq!(
        pool.validate_against_binding(&second_binding),
        Err(ComputeContractError::InvalidCredentialPool)
    );
    let mut different_offer_binding = binding;
    different_offer_binding.offer_ref = "offer.fixture.other".into();
    assert_eq!(
        pool.validate_against_binding(&different_offer_binding),
        Err(ComputeContractError::InvalidCredentialPool)
    );
    let reordered = pool
        .reorder(1, &["credential/key-b".into(), "credential/key-a".into()])
        .unwrap();
    assert_eq!(
        reordered.credentials[0].credential.credential_id(),
        "credential/key-b"
    );
    let rotated_ref = CredentialRefV1::new(
        "credential/key-a",
        "source/primary",
        "hirouted",
        "provider-auth",
        [destination.clone()],
        2,
    )
    .unwrap();
    let rotated = pool
        .replace(1, "credential/key-a", rotated_ref, digest("key-a-rotated"))
        .unwrap();
    let replace_mutation = CredentialPoolMutationV1::from_registered_planner(
        CredentialPoolMutationKind::Replace,
        Some(&pool),
        rotated.clone(),
    )
    .unwrap();
    assert_eq!(replace_mutation.kind(), CredentialPoolMutationKind::Replace);
    assert_eq!(replace_mutation.expected_revision(), 1);
    assert!(
        CredentialPoolMutationV1::from_registered_planner(
            CredentialPoolMutationKind::Add,
            Some(&pool),
            rotated.clone(),
        )
        .is_err()
    );
    let mut different_offer = rotated.clone();
    different_offer.offer_ref = "offer.fixture.other".into();
    assert!(
        CredentialPoolMutationV1::from_registered_planner(
            CredentialPoolMutationKind::Replace,
            Some(&pool),
            different_offer,
        )
        .is_err()
    );
    assert_eq!(rotated.credentials[0].credential.generation(), 2);
    assert_eq!(rotated.credentials[0].ordinal, 0);
    let one_key = rotated.remove(2, "credential/key-b").unwrap();
    CredentialPoolMutationV1::from_registered_planner(
        CredentialPoolMutationKind::Remove,
        Some(&rotated),
        one_key.clone(),
    )
    .unwrap();
    assert_eq!(one_key.credentials.len(), 1);
    assert_eq!(
        one_key.remove(3, "credential/key-a"),
        Err(ComputeContractError::LastUsableCredential)
    );

    let mut mixed = pool;
    mixed.credentials[1].credential = CredentialRefV1::new(
        "credential/key-b",
        "source/other",
        "hirouted",
        "provider-auth",
        ["source-identity/other".into()],
        1,
    )
    .unwrap();
    assert_eq!(
        mixed.validate(),
        Err(ComputeContractError::InvalidCredentialPool)
    );
}

#[test]
fn compute_effective_price_uses_exact_override_and_old_freeze_does_not_recompute() {
    let mut data: ModelDataBundleV1 = serde_json::from_value(json!({
        "schema": MODEL_DATA_SCHEMA_V1, "bundle_version": "bundle-v1",
        "product_release": "release-fixture-v1", "connector_registry_version": "registry-fixture-v1",
        "models_slice_version": "models-v1", "capability_slice_version": "cap-v1",
        "ratings_slice_version": "rating-v1", "prices_slice_version": "price-v1",
        "free_offers_slice_version": "free-v1",
        "models": [], "model_endpoint_capabilities": [], "ratings": [],
        "offers": [{"offer_id":"offer.one","revision":1,"endpoint_profile_id":"endpoint.fixture","endpoint_profile_revision":1,"service_offering_id":"offering.fixture","entitlement_id":"payg","usage_scope":"account","region_id":"test","model_configuration_ids":["model.one"],"billing_class":"paid","evidence_digest":digest("offer")}],
        "free_offers": [],
        "price_rates": [{"price_rate_id":"rate.one","revision":1,"offer_ref":"offer.one","model_configuration_id":"model.one","currency":"USD","input_micros_per_million":100,"output_micros_per_million":200}]
    })).unwrap();
    data.models.push(ModelDefinitionV1 {
        model_configuration_id: "model.one".into(),
        revision: 1,
        display_name: "One".into(),
        publisher_id: "publisher.one".into(),
        capabilities: CapabilityFactsV1 {
            tool: true,
            vision: false,
            streaming: true,
            context_tokens: 100,
            max_output_tokens: 10,
        },
    });
    let base = effective_price(&data, &[], "offer.one", "model.one", "USD", 0).unwrap();
    let override_value = PriceOverrideV1 {
        override_id: "override.one".into(),
        revision: 1,
        offer_ref: "offer.one".into(),
        model_configuration_id: "model.one".into(),
        currency: "USD".into(),
        operation: PriceOverrideOperation::Replace,
        target_rule_id: None,
        input_value: Some(70),
        output_value: Some(140),
    };
    let latest =
        effective_price(&data, &[override_value], "offer.one", "model.one", "USD", 0).unwrap();
    assert_eq!(latest.input_micros_per_million, 70);
    assert_eq!(latest.applied_overrides[0].revision, 1);
    assert_eq!(base.input_micros_per_million, 100);
    assert_eq!(base.model_data_bundle_version, "bundle-v1");
    assert_eq!(base.model_data_digest, CanonicalDigest::of(&data).unwrap());
    assert_ne!(base.frozen_digest, latest.frozen_digest);
    base.verify_frozen_digest().unwrap();
    latest.verify_frozen_digest().unwrap();
    let revised_override = PriceOverrideV1 {
        revision: 2,
        input_value: Some(65),
        output_value: Some(130),
        ..PriceOverrideV1 {
            override_id: "override.one".into(),
            revision: 1,
            offer_ref: "offer.one".into(),
            model_configuration_id: "model.one".into(),
            currency: "USD".into(),
            operation: PriceOverrideOperation::Replace,
            target_rule_id: None,
            input_value: Some(70),
            output_value: Some(140),
        }
    };
    let revised = effective_price(
        &data,
        &[revised_override],
        "offer.one",
        "model.one",
        "USD",
        0,
    )
    .unwrap();
    assert_eq!(revised.applied_overrides[0].revision, 2);
    assert_ne!(latest.frozen_digest, revised.frozen_digest);
    let duplicate_revision = PriceOverrideV1 {
        override_id: "override.two".into(),
        revision: 1,
        offer_ref: "offer.one".into(),
        model_configuration_id: "model.one".into(),
        currency: "USD".into(),
        operation: PriceOverrideOperation::DisableCatalogRule,
        target_rule_id: Some("rate.one".into()),
        input_value: None,
        output_value: None,
    };
    assert_eq!(
        effective_price(
            &data,
            &[
                PriceOverrideV1 {
                    override_id: "override.one".into(),
                    revision: 1,
                    offer_ref: "offer.one".into(),
                    model_configuration_id: "model.one".into(),
                    currency: "USD".into(),
                    operation: PriceOverrideOperation::Replace,
                    target_rule_id: None,
                    input_value: Some(70),
                    output_value: Some(140),
                },
                duplicate_revision,
            ],
            "offer.one",
            "model.one",
            "USD",
            0,
        ),
        Err(ComputeContractError::PriceDisabled)
    );
    let different_override_identity = PriceOverrideV1 {
        override_id: "override.two".into(),
        revision: 2,
        offer_ref: "offer.one".into(),
        model_configuration_id: "model.one".into(),
        currency: "USD".into(),
        operation: PriceOverrideOperation::Replace,
        target_rule_id: None,
        input_value: Some(60),
        output_value: Some(120),
    };
    assert_eq!(
        effective_price(
            &data,
            &[
                PriceOverrideV1 {
                    override_id: "override.one".into(),
                    revision: 1,
                    offer_ref: "offer.one".into(),
                    model_configuration_id: "model.one".into(),
                    currency: "USD".into(),
                    operation: PriceOverrideOperation::Replace,
                    target_rule_id: None,
                    input_value: Some(70),
                    output_value: Some(140),
                },
                different_override_identity,
            ],
            "offer.one",
            "model.one",
            "USD",
            0,
        ),
        Err(ComputeContractError::AmbiguousPrice)
    );

    let schedule = PriceScheduleV1 {
        timezone_id: "Etc/UTC".into(),
        timezone_data_revision: "tzdata-v1".into(),
        schedule_compile_version: "schedule-v1".into(),
        effective_from: 0,
        effective_until: 1_000_000,
        utc_offset_minutes: 0,
        weekdays: BTreeSet::from([4]),
        start_minute: 0,
        end_minute: 60,
    };
    data.price_rates.push(PriceRateV1 {
        price_rate_id: "rate.scheduled".into(),
        revision: 1,
        offer_ref: "offer.one".into(),
        model_configuration_id: "model.one".into(),
        currency: "USD".into(),
        input_micros_per_million: 300,
        output_micros_per_million: 600,
        schedule: Some(schedule.clone()),
    });
    let scheduled = effective_price(&data, &[], "offer.one", "model.one", "USD", 0).unwrap();
    assert_eq!(scheduled.price_rate_id, "rate.scheduled");
    let disabled = PriceOverrideV1 {
        override_id: "override.disable-schedule".into(),
        revision: 7,
        offer_ref: "offer.one".into(),
        model_configuration_id: "model.one".into(),
        currency: "USD".into(),
        operation: PriceOverrideOperation::DisableCatalogRule,
        target_rule_id: Some("rate.scheduled".into()),
        input_value: None,
        output_value: None,
    };
    let fallback = effective_price(&data, &[disabled], "offer.one", "model.one", "USD", 0).unwrap();
    assert_eq!(fallback.price_rate_id, "rate.one");
    assert_eq!(fallback.applied_overrides[0].revision, 7);
    fallback.verify_frozen_digest().unwrap();

    let mut duplicate_base = data.clone();
    duplicate_base.price_rates.push(PriceRateV1 {
        price_rate_id: "rate.duplicate-base".into(),
        revision: 1,
        offer_ref: "offer.one".into(),
        model_configuration_id: "model.one".into(),
        currency: "USD".into(),
        input_micros_per_million: 1,
        output_micros_per_million: 2,
        schedule: None,
    });
    assert_eq!(
        effective_price(&duplicate_base, &[], "offer.one", "model.one", "USD", 0,),
        Err(ComputeContractError::InvalidPrice)
    );

    let mut overlapping = data.clone();
    overlapping.price_rates.push(PriceRateV1 {
        price_rate_id: "rate.overlapping".into(),
        revision: 1,
        offer_ref: "offer.one".into(),
        model_configuration_id: "model.one".into(),
        currency: "USD".into(),
        input_micros_per_million: 4,
        output_micros_per_million: 8,
        schedule: Some(schedule),
    });
    assert_eq!(
        effective_price(&overlapping, &[], "offer.one", "model.one", "USD", 0),
        Err(ComputeContractError::AmbiguousPrice)
    );

    let overnight = PriceScheduleV1 {
        timezone_id: "Etc/UTC".into(),
        timezone_data_revision: "tzdata-v1".into(),
        schedule_compile_version: "schedule-v1".into(),
        effective_from: 0,
        effective_until: 1_000_000,
        utc_offset_minutes: 0,
        weekdays: BTreeSet::from([1]),
        start_minute: 22 * 60,
        end_minute: 2 * 60,
    };
    let monday = 4 * 86_400;
    assert!(overnight.matches(monday + 23 * 3_600));
    assert!(overnight.matches(monday + 25 * 3_600));
    assert!(!overnight.matches(monday + 27 * 3_600));
}
