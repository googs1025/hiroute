use std::collections::BTreeSet;

use hiroute_application_api::{CommandLifecycle, command_by_id};
use hiroute_domain::{
    AuthenticationKind, COMPUTE_PROBE_LEASE_SCHEMA_V1, COMPUTE_RUNTIME_IDENTITY_SCHEMA_V1,
    COMPUTE_RUNTIME_STATE_SCHEMA_V1, CanonicalDigest, ComputeRuntimeHealthV1, CredentialPoolV1,
    CredentialRefV1, PoolCredentialV1, RuntimeClockSampleV1, RuntimeProbeLeaseRequestV1,
    RuntimeStateIdentityV1, RuntimeStateV1,
};
use serde::Deserialize;

#[path = "../src/compute/mod.rs"]
mod compute;

use compute::{ComputePoolContractV1, PRODUCTION_EVIDENCE, PRODUCTION_EVIDENCE_OWNER};

const SCENARIO: &str =
    include_str!("../../../e2e/product/scenarios/compute/compute-pool-contract.v1.json");
const GOLDEN: &str =
    include_str!("../../../e2e/product/golden/compute/compute-pool-boundary.v1.json");
const FIXTURE: &str =
    include_str!("../../../e2e/product/fixtures/compute/registered-options.v1.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Golden {
    scenario_id: String,
    formal_composition: String,
    evidence: String,
    composition_owner: String,
    runtime_adapter_owner: String,
    runtime_state_schema: String,
    runtime_identity_schema: String,
    runtime_lease_schema: String,
    runtime_authority: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionFixture {
    schema: String,
    operational_data_owner: String,
    registered_option_ids: BTreeSet<String>,
    arbitrary_destination_fields: BTreeSet<String>,
    contains_secret_literal: bool,
}

#[test]
fn compute_pool_contract_is_typed_and_formal_composition_is_green() {
    let scenario: ComputePoolContractV1 = serde_json::from_str(SCENARIO).unwrap();
    scenario.validate().unwrap();
    let golden: Golden = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(golden.scenario_id, scenario.scenario_id);
    assert_eq!(golden.formal_composition, "green");
    assert_eq!(golden.evidence, PRODUCTION_EVIDENCE);
    assert_eq!(golden.composition_owner, PRODUCTION_EVIDENCE_OWNER);
    assert_eq!(golden.runtime_adapter_owner, "PROCESS-25015");
    assert_eq!(golden.runtime_state_schema, COMPUTE_RUNTIME_STATE_SCHEMA_V1);
    assert_eq!(
        golden.runtime_identity_schema,
        COMPUTE_RUNTIME_IDENTITY_SCHEMA_V1
    );
    assert_eq!(golden.runtime_lease_schema, COMPUTE_PROBE_LEASE_SCHEMA_V1);
    assert_eq!(
        golden.runtime_authority,
        "single_sqlite_compare_and_set_store"
    );
    for command_id in [
        "compute.scan",
        "compute.list",
        "compute.show",
        "compute.connection.options",
        "compute.connection.preview",
        "compute.connection.apply",
        "compute.connection.authorize",
        "compute.connection.test",
    ] {
        assert_eq!(
            command_by_id(command_id).unwrap().lifecycle,
            CommandLifecycle::Released
        );
    }
}

#[test]
fn compute_pool_fixture_freezes_four_schema_options_without_operational_claims() {
    let fixture: OptionFixture = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(fixture.schema, "hiroute.compute-contract-fixture/v1");
    assert_eq!(fixture.operational_data_owner, "PROCESS-25007");
    assert_eq!(
        fixture.registered_option_ids,
        BTreeSet::from([
            "bailian.payg.cn.v1".to_owned(),
            "deepseek.official.global.v1".to_owned(),
            "kimi.open-platform.cn.v1".to_owned(),
            "zhipu.general.cn.v1".to_owned(),
        ])
    );
    assert_eq!(
        fixture.arbitrary_destination_fields,
        BTreeSet::from([
            "auth".to_owned(),
            "connector_target".to_owned(),
            "endpoint_url".to_owned(),
            "host".to_owned(),
            "protocol".to_owned(),
            "redirect".to_owned(),
        ])
    );
    assert!(!fixture.contains_secret_literal);
}

#[test]
fn compute_pool_typed_oracle_proves_two_key_identity_and_exact_runtime_scope() {
    let identity = CanonicalDigest::of_bytes(b"source-identity");
    let destination = "connection-option/bailian.payg.cn.v1".to_owned();
    let entry = |id: &str, ordinal| PoolCredentialV1 {
        credential: CredentialRefV1::new(
            id,
            "source/primary",
            "hirouted",
            "provider-auth",
            [destination.clone()],
            1,
        )
        .unwrap(),
        fingerprint: CanonicalDigest::of_bytes(id.as_bytes()),
        ordinal,
        enabled: true,
    };
    let pool = CredentialPoolV1 {
        pool_id: "pool/primary".into(),
        binding_id: "binding/primary".into(),
        binding_revision: 1,
        binding_digest: CanonicalDigest::of_bytes(b"binding-primary"),
        source_id: "primary".into(),
        source_revision: 1,
        connection_option_id: "bailian.payg.cn.v1".into(),
        source_identity_digest: identity,
        offer_ref: "offer/bailian-payg".into(),
        offer_revision: 1,
        offer_evidence_digest: CanonicalDigest::of_bytes(b"offer-bailian-payg"),
        billing_class: hiroute_domain::BillingClass::Paid,
        model_configuration_id: "model.primary".into(),
        authentication: AuthenticationKind::ProviderApiKey,
        revision: 1,
        credentials: vec![entry("credential/key-a", 0), entry("credential/key-b", 1)],
    };
    pool.validate().unwrap();
    let key_entry = &pool.credentials[0];
    let opaque_key_id = "provider key @ slot?#/🔥";
    let key_identity = RuntimeStateIdentityV1::credential(
        &pool.binding_id,
        key_entry.credential.credential_id(),
        opaque_key_id,
        key_entry.credential.generation(),
    )
    .unwrap();
    let other_key_identity = RuntimeStateIdentityV1::credential(
        &pool.binding_id,
        key_entry.credential.credential_id(),
        "not-a-digest:key-b",
        pool.credentials[1].credential.generation(),
    )
    .unwrap();
    let rotated_key_identity = RuntimeStateIdentityV1::credential(
        &pool.binding_id,
        key_entry.credential.credential_id(),
        opaque_key_id,
        key_entry.credential.generation() + 1,
    )
    .unwrap();
    let binding_identity = RuntimeStateIdentityV1::binding(&pool.binding_id).unwrap();
    assert_ne!(
        key_identity.canonical_key().unwrap(),
        other_key_identity.canonical_key().unwrap()
    );
    assert_ne!(
        key_identity.canonical_key().unwrap(),
        rotated_key_identity.canonical_key().unwrap()
    );
    assert_ne!(
        key_identity.canonical_key().unwrap(),
        binding_identity.canonical_key().unwrap()
    );
    let encoded_identity = serde_json::to_value(&key_identity).unwrap();
    assert_eq!(encoded_identity["stable_binding_id"], pool.binding_id);
    assert_eq!(
        encoded_identity["subject"]["credential_ref"],
        key_entry.credential.credential_id()
    );
    assert_eq!(encoded_identity["subject"]["key_id"], opaque_key_id);
    assert!(encoded_identity.get("binding_revision").is_none());
    assert!(encoded_identity.get("binding_digest").is_none());
    assert!(encoded_identity["subject"].get("key_fingerprint").is_none());

    let clock = |value| RuntimeClockSampleV1::from_unix_millis(value).unwrap();
    let mut exact = RuntimeStateV1::ready(key_identity.clone(), 0, clock(100)).unwrap();
    for generation in 1..=5 {
        let next = RuntimeStateV1::cooling_down(
            key_identity.clone(),
            generation,
            100 + i64::try_from(generation).unwrap(),
            None,
            clock(100 + i64::try_from(generation).unwrap()),
        )
        .unwrap();
        exact.validate_direct_successor(&next).unwrap();
        exact = next;
    }
    assert!(matches!(
        exact.health(),
        ComputeRuntimeHealthV1::CoolingDown { .. }
    ));
    let disabled = RuntimeStateV1::disabled(key_identity.clone(), 6, clock(106)).unwrap();
    exact.validate_direct_successor(&disabled).unwrap();
    assert_eq!(disabled.health(), ComputeRuntimeHealthV1::Disabled);

    let key_state =
        RuntimeStateV1::cooling_down(key_identity, 1, 60_100, None, clock(100)).unwrap();
    let binding_state =
        RuntimeStateV1::cooling_down(binding_identity, 1, 200, None, clock(100)).unwrap();
    assert_eq!(key_state.cooldown_until_unix_millis(), Some(60_100));
    assert_eq!(binding_state.cooldown_until_unix_millis(), Some(200));
    let encoded_state = serde_json::to_value(&key_state).unwrap();
    assert!(encoded_state.get("reason").is_none());
    assert!(encoded_state.get("failure_window").is_none());
    let lease_request = RuntimeProbeLeaseRequestV1::new(clock(60_100), 1_000).unwrap();
    let leased = key_state.acquire_probe(1, &lease_request).unwrap();
    assert_eq!(leased.probe_lease().unwrap().fence_generation(), 2);
}
