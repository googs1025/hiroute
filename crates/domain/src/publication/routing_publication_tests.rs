use serde_json::Value;

use super::*;

#[test]
fn routing_publication_unknown_schema_and_revision_fail_closed() {
    let publication = GatewayPublicationV1::new(
        WorkspaceId::default(),
        GatewayPublicationRevision::new(1).unwrap(),
        AliasRegistryV1::default(),
        Vec::new(),
    )
    .unwrap();
    let mut value = serde_json::to_value(publication).unwrap();
    value["schema"] = serde_json::json!(UNSUPPORTED_GATEWAY_PUBLICATION_SCHEMA_V1);
    let bytes = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        GatewayPublicationV1::decode(&bytes).unwrap_err(),
        PublicationError::UnsupportedSchema
    );

    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    value["schema"] = serde_json::json!(GATEWAY_PUBLICATION_SCHEMA_V3);
    value["publication_revision"] = serde_json::json!(0);
    let bytes = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        GatewayPublicationV1::decode(&bytes).unwrap_err(),
        PublicationError::InvalidRevision
    );
}

#[test]
fn model_grant_projection_rejects_conflicting_or_future_plan_provenance() {
    let fixture: Value = serde_json::from_slice(include_bytes!(
        "../../../../e2e/product/golden/routing/compiled-publication.v2.json"
    ))
    .unwrap();
    let plan: CompiledAgentPlanV1 = serde_json::from_value(fixture["plans"][0].clone()).unwrap();
    let plan = plan.into_current().unwrap();
    let revision = plan.body.agent_plan_revision;
    let accepted_digest = plan.body.materialized_route_digest.clone();
    let name = plan.model_alias().as_str().to_owned();
    let grant = |revision, semantic_digest| GatewayExecutableGrantV2 {
        grant_id: "test-grant".into(),
        generation: 1,
        bearer_token_sha256: CanonicalDigest::of_bytes(b"test-grant-token"),
        model_grant: crate::AgentModelGrantV2::seal(
            AgentIngressProtocolV1::Responses,
            BTreeMap::from([(
                name.clone(),
                crate::AgentModelRouteV2::Plan {
                    plan_id: plan.agent_plan_id().clone(),
                    alias: plan.model_alias().clone(),
                    revision,
                    semantic_digest,
                },
            )]),
        )
        .unwrap(),
    };
    let original_grant = grant(revision, accepted_digest.clone());
    let plans = vec![plan.clone()];
    let aliases = materialize_aliases(&plans, std::slice::from_ref(&original_grant)).unwrap();
    assert!(snapshot::project_grants(vec![original_grant.clone()], &aliases, &plans).is_ok());
    for changed in [
        grant(revision, CanonicalDigest::of_bytes(b"conflicting-route")),
        grant(revision + 1, accepted_digest),
    ] {
        assert_eq!(
            snapshot::project_grants(vec![changed], &aliases, &plans).unwrap_err(),
            PublicationError::InvalidGrant,
        );
    }
    let mut next_body = plan.body.as_ref().clone();
    next_body.agent_plan_revision += 1;
    let next_plan = CompiledAgentPlanV1::seal_current(next_body).unwrap();
    let next_plans = vec![next_plan];
    let next_aliases =
        materialize_aliases(&next_plans, std::slice::from_ref(&original_grant)).unwrap();
    let projected =
        snapshot::project_grants(vec![original_grant], &next_aliases, &next_plans).unwrap();
    assert!(matches!(
        &projected[0].routes[&name],
        GatewayModelRouteV2::Plan { revision: current, semantic_digest, .. }
            if *current == revision + 1
                && semantic_digest == &next_plans[0].body.materialized_route_digest
    ));
}

#[test]
fn routing_publication_record_is_bound_to_its_workspace() {
    let publication = GatewayPublicationV1::new(
        WorkspaceId::default(),
        GatewayPublicationRevision::new(1).unwrap(),
        AliasRegistryV1::default(),
        Vec::new(),
    )
    .unwrap();
    let other = WorkspaceId::parse("personal/other").unwrap();
    assert_eq!(
        PublicationRecordV1::from_publication(other, &publication).unwrap_err(),
        PublicationError::InvalidWorkspace
    );
}

#[test]
fn adding_first_grant_does_not_mutate_the_independent_plan_revision() {
    let granted: GatewayPublicationV1 = GatewayPublicationV1::decode_persisted(include_bytes!(
        "../../../../e2e/product/golden/routing/compiled-publication.v2.json"
    ))
    .unwrap();
    let mut plan_only = granted.clone();
    plan_only.publication_revision = GatewayPublicationRevision::new(10).unwrap();
    plan_only.aliases.clear();
    plan_only.grants.clear();
    plan_only.validate().unwrap();

    granted.validate_transition_from(&plan_only).unwrap();
    let plan_only_projection = plan_only.gateway_snapshot().unwrap();
    assert_eq!(
        plan_only_projection.admission,
        GatewayAdmissionStateV1::NoNewCalls
    );
    assert!(plan_only_projection.aliases.is_empty());
    assert!(plan_only_projection.grants.is_empty());
    let projection = granted.gateway_snapshot().unwrap();
    for alias in &projection.aliases {
        let plan = granted
            .plans
            .iter()
            .find(|plan| plan.model_alias() == &alias.served_model_id)
            .unwrap();
        let attempts = ordered_candidates(plan)
            .unwrap()
            .into_iter()
            .map(|candidate| (candidate.binding_id.as_str(), candidate))
            .collect::<BTreeMap<_, _>>();
        for projected in &alias.candidates {
            let attempt = attempts[projected.stable_target_key.as_str()];
            let pricing = projected.pricing_identity.as_ref().unwrap();
            assert_eq!(pricing.source_id, attempt.source_id);
            assert_eq!(
                pricing.source_identity_digest,
                attempt.source_identity_digest
            );
            assert_eq!(
                pricing.model_configuration_id,
                attempt.model_configuration_id
            );
            assert_eq!(pricing.actual_offer_ref, attempt.offer_ref);
        }
    }
}

#[test]
fn revoking_last_grant_preserves_the_independent_plan_publication() {
    let granted: GatewayPublicationV1 = GatewayPublicationV1::decode_persisted(include_bytes!(
        "../../../../e2e/product/golden/routing/compiled-publication.v2.json"
    ))
    .unwrap();
    let first_grant_id = granted.grants[0].grant_id.clone();
    let last_grant_id = granted.grants[1].grant_id.clone();
    let one_grant = granted
        .next_without_access_grant(GatewayPublicationRevision::new(3).unwrap(), &first_grant_id)
        .unwrap();
    let revoked = one_grant
        .next_without_access_grant(GatewayPublicationRevision::new(4).unwrap(), &last_grant_id)
        .unwrap();

    assert_eq!(revoked.plans.len(), granted.plans.len());
    assert!(
        revoked
            .plans
            .iter()
            .all(|plan| plan.body.schema == AGENT_PLAN_COMPILED_SCHEMA_V2)
    );
    for (current, legacy) in revoked.plans.iter().zip(&granted.plans) {
        assert_eq!(current.agent_plan_id(), legacy.agent_plan_id());
        assert_eq!(
            current.body.materialized.request_owned,
            legacy.body.materialized.request_owned
        );
        assert_eq!(
            current.body.materialized.attempt_owned.groups.len(),
            legacy.body.materialized.attempt_owned.groups.len()
        );
    }
    assert!(revoked.grants.is_empty());
    assert!(revoked.aliases.is_empty());
    let revoked_projection = revoked.gateway_snapshot().unwrap();
    assert_eq!(
        revoked_projection.admission,
        GatewayAdmissionStateV1::NoNewCalls
    );
    assert!(revoked_projection.aliases.is_empty());
    assert!(revoked_projection.grants.is_empty());
    revoked.validate_transition_from(&one_grant).unwrap();
}

#[test]
fn legacy_publication_recovery_rederives_route_mapping_from_persisted_name_sets() {
    let raw: Value = serde_json::from_slice(include_bytes!(
        "../../../../e2e/product/golden/routing/compiled-publication.v2.json"
    ))
    .unwrap();
    let recovered = GatewayPublicationV1::decode_persisted(include_bytes!(
        "../../../../e2e/product/golden/routing/compiled-publication.v2.json"
    ))
    .unwrap();
    // Recovery reads an exact legacy record; only a new publish upgrades the tag.
    assert_eq!(recovered.schema, LEGACY_GATEWAY_PUBLICATION_SCHEMA_V2);
    let raw_grants = raw["grants"].as_array().unwrap();
    assert_eq!(recovered.grants.len(), raw_grants.len());
    for (grant, raw_grant) in recovered.grants.iter().zip(raw_grants) {
        assert_eq!(grant.grant_id, raw_grant["grant_id"].as_str().unwrap());
        let protocol: AgentIngressProtocolV1 =
            serde_json::from_value(raw_grant["allowed_protocols"][0].clone()).unwrap();
        assert_eq!(grant.model_grant.protocol, protocol);
        let aliases: Vec<ModelAlias> = raw_grant["allowed_aliases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| serde_json::from_value(value.clone()).unwrap())
            .collect();
        for alias in &aliases {
            let plan = recovered
                .plans
                .iter()
                .find(|plan| plan.model_alias() == alias)
                .unwrap();
            match grant.model_grant.routes.get(alias.as_str()).unwrap() {
                crate::AgentModelRouteV2::Plan {
                    plan_id,
                    alias: route_alias,
                    revision,
                    semantic_digest,
                } => {
                    assert_eq!(plan_id, plan.agent_plan_id());
                    assert_eq!(route_alias, alias);
                    assert_eq!(*revision, plan.body.agent_plan_revision);
                    assert_eq!(semantic_digest, &plan.body.materialized_route_digest);
                }
                crate::AgentModelRouteV2::Fixed { .. } => {
                    panic!("a V2 grant carries only published plan aliases");
                }
            }
        }
    }
    assert!(recovered.clone().into_current().is_ok());
}

#[test]
fn legacy_publication_recovery_rejects_grant_shapes_the_v2_writer_never_sealed() {
    let mut raw: Value = serde_json::from_slice(include_bytes!(
        "../../../../e2e/product/golden/routing/compiled-publication.v2.json"
    ))
    .unwrap();
    let mut two_protocols = raw.clone();
    two_protocols["grants"][0]["allowed_protocols"] = serde_json::json!(["messages", "responses"]);
    assert_eq!(
        GatewayPublicationV1::decode_persisted(&serde_json::to_vec(&two_protocols).unwrap())
            .unwrap_err(),
        PublicationError::InvalidGrant
    );
    let mut unknown_alias = raw.clone();
    unknown_alias["grants"][0]["allowed_aliases"] = serde_json::json!(["hiroute/not-published"]);
    assert_eq!(
        GatewayPublicationV1::decode_persisted(&serde_json::to_vec(&unknown_alias).unwrap())
            .unwrap_err(),
        PublicationError::InvalidGrant
    );
    let mut duplicate_alias = raw.clone();
    duplicate_alias["grants"][0]["allowed_aliases"] =
        serde_json::json!(["hiroute/2590c10eeae4f930", "hiroute/2590c10eeae4f930"]);
    assert_eq!(
        GatewayPublicationV1::decode_persisted(&serde_json::to_vec(&duplicate_alias).unwrap())
            .unwrap_err(),
        PublicationError::InvalidGrant
    );
    raw["schema"] = serde_json::json!(UNSUPPORTED_GATEWAY_PUBLICATION_SCHEMA_V1);
    assert_eq!(
        GatewayPublicationV1::decode_persisted(&serde_json::to_vec(&raw).unwrap()).unwrap_err(),
        PublicationError::UnsupportedSchema
    );
}
