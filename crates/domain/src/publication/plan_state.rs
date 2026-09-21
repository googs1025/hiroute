//! Product V3 keeps configured grants intact while projecting only enabled ordinary aliases.
use super::*;
use crate::{PlanHeadV1, PlanLifecycleV1};

pub const GATEWAY_PUBLICATION_SCHEMA_V3: &str = "hiroute.gateway-publication/v3";

impl GatewayPublicationV1 {
    pub fn next_with_plan_content(
        &self,
        revision: GatewayPublicationRevision,
        aliases: AliasRegistryV1,
        plan: CompiledAgentPlanV1,
        heads: Vec<PlanHeadV1>,
    ) -> Result<Self, PublicationError> {
        self.validate()?;
        let mut next = self.clone();
        next.schema = GATEWAY_PUBLICATION_SCHEMA_V3.into();
        next.compiler_revision = crate::AGENT_PLAN_COMPILER_REVISION_V2.into();
        next.publication_revision = revision;
        next.alias_registry = aliases;
        next.plans
            .retain(|p| p.agent_plan_id() != plan.agent_plan_id());
        next.plans.push(
            plan.into_current()
                .map_err(|_| PublicationError::InvalidPlan)?,
        );
        next.plans = next
            .plans
            .into_iter()
            .map(|plan| {
                plan.into_current()
                    .map_err(|_| PublicationError::InvalidPlan)
            })
            .collect::<Result<Vec<_>, _>>()?;
        next.plans
            .sort_by(|a, b| a.agent_plan_id().cmp(b.agent_plan_id()));
        next.plan_heads = heads;
        next.plan_heads
            .sort_by(|a, b| a.reference.plan_id.cmp(&b.reference.plan_id));
        next.aliases = materialize_aliases(&next.plans, &next.enabled_grants()?)?;
        if self.plans.is_empty()
            && self.grants.is_empty()
            && self.publication_revision.get() == 1
            && revision.get() == 1
        {
            // An unpersisted initial bootstrap is a constructor seed, not an active revision.
            next.validate()?;
        } else {
            next.validate_transition_from(self)?;
        }
        Ok(next)
    }

    pub fn next_with_plan_lifecycle(
        &self,
        revision: GatewayPublicationRevision,
        head: PlanHeadV1,
    ) -> Result<Self, PublicationError> {
        self.validate()?;
        let previous = self
            .plan_heads
            .iter()
            .find(|h| h.reference.plan_id == head.reference.plan_id)
            .ok_or(PublicationError::InvalidPlanSet)?;
        if previous.reference != head.reference
            || previous.model_alias != head.model_alias
            || previous.head_revision.checked_add(1) != Some(head.head_revision)
            || previous.status == PlanLifecycleV1::Deleted
            || previous.status == head.status
        {
            return Err(PublicationError::InvalidTransition);
        }
        let mut next = self.clone();
        next.schema = GATEWAY_PUBLICATION_SCHEMA_V3.into();
        next.compiler_revision = crate::AGENT_PLAN_COMPILER_REVISION_V2.into();
        next.plans = next
            .plans
            .into_iter()
            .map(|plan| {
                plan.into_current()
                    .map_err(|_| PublicationError::InvalidPlan)
            })
            .collect::<Result<Vec<_>, _>>()?;
        next.publication_revision = revision;
        if head.status == PlanLifecycleV1::Deleted {
            // Referencing configured grants must be explicitly removed by their owner first.
            if next
                .grants
                .iter()
                .any(|g| g.permits_plan(&head.model_alias))
            {
                return Err(PublicationError::InvalidGrant);
            }
            next.alias_registry
                .retire(&head.reference.plan_id)
                .map_err(|_| PublicationError::InvalidAliasRegistry)?;
            next.plans
                .retain(|p| p.agent_plan_id() != &head.reference.plan_id);
        }
        let slot = next
            .plan_heads
            .iter_mut()
            .find(|h| h.reference.plan_id == head.reference.plan_id)
            .ok_or(PublicationError::InvalidPlanSet)?;
        *slot = head;
        next.aliases = materialize_aliases(&next.plans, &next.enabled_grants()?)?;
        next.validate_transition_from(self)?;
        Ok(next)
    }

    pub(super) fn validate_plan_heads(&self) -> Result<(), PublicationError> {
        if self.schema != GATEWAY_PUBLICATION_SCHEMA_V3 {
            return if self.plan_heads.is_empty() {
                Ok(())
            } else {
                Err(PublicationError::UnsupportedSchema)
            };
        }
        if self.plan_heads.is_empty() {
            return Ok(());
        }
        let mut ids = BTreeSet::new();
        let mut previous = None;
        for head in &self.plan_heads {
            head.validate().map_err(|_| PublicationError::InvalidPlan)?;
            if head.reference.workspace_id != self.workspace_id
                || !ids.insert(&head.reference.plan_id)
                || previous.is_some_and(|id| id >= &head.reference.plan_id)
            {
                return Err(PublicationError::InvalidPlanSet);
            }
            previous = Some(&head.reference.plan_id);
            let plan = self
                .plans
                .iter()
                .find(|p| p.agent_plan_id() == &head.reference.plan_id);
            if head.status == PlanLifecycleV1::Deleted {
                if plan.is_some()
                    || !self
                        .alias_registry
                        .retired_plan_ids
                        .contains(&head.reference.plan_id)
                    || !self.alias_registry.tombstones.contains(&head.model_alias)
                {
                    return Err(PublicationError::InvalidPlanSet);
                }
            } else if !plan.is_some_and(|p| {
                p.body.agent_plan_revision == head.reference.content_revision
                    && p.model_alias() == &head.model_alias
            }) {
                return Err(PublicationError::InvalidPlanSet);
            }
        }
        if self.plans.iter().any(|p| !ids.contains(p.agent_plan_id())) {
            return Err(PublicationError::InvalidPlanSet);
        }
        Ok(())
    }

    pub(super) fn enabled_grants(&self) -> Result<Vec<GatewayExecutableGrantV2>, PublicationError> {
        if self.schema != GATEWAY_PUBLICATION_SCHEMA_V3 || self.plan_heads.is_empty() {
            return Ok(self.grants.clone());
        }
        let allowed = self
            .plan_heads
            .iter()
            .filter(|h| h.status == PlanLifecycleV1::Enabled)
            .map(|h| &h.model_alias)
            .collect::<BTreeSet<_>>();
        let mut enabled = Vec::new();
        for grant in &self.grants {
            let mut routes = grant.model_grant.routes.clone();
            routes.retain(|_, route| match route {
                crate::AgentModelRouteV2::Plan { alias, .. } => allowed.contains(alias),
                crate::AgentModelRouteV2::Fixed { .. } => true,
            });
            if !routes.is_empty() {
                let mut projected = grant.clone();
                projected.model_grant =
                    crate::AgentModelGrantV2::seal(grant.model_grant.protocol, routes)
                        .map_err(|_| PublicationError::InvalidGrant)?;
                enabled.push(projected);
            }
        }
        Ok(enabled)
    }

    pub(super) fn preserve_heads(mut self, source: &Self) -> Result<Self, PublicationError> {
        if source.schema == GATEWAY_PUBLICATION_SCHEMA_V3 {
            self.schema = GATEWAY_PUBLICATION_SCHEMA_V3.into();
            self.plan_heads = source.plan_heads.clone();
            self.aliases = materialize_aliases(&self.plans, &self.enabled_grants()?)?;
            self.validate()?;
        }
        Ok(self)
    }

    pub(super) fn validate_head_transition(&self, active: &Self) -> Result<(), PublicationError> {
        if active.schema == GATEWAY_PUBLICATION_SCHEMA_V3
            && self.schema != GATEWAY_PUBLICATION_SCHEMA_V3
        {
            return Err(PublicationError::InvalidTransition);
        }
        for old in &active.plan_heads {
            let new = self
                .plan_heads
                .iter()
                .find(|h| h.reference.plan_id == old.reference.plan_id)
                .ok_or(PublicationError::InvalidTransition)?;
            if new.head_revision < old.head_revision
                || (new.head_revision == old.head_revision && new != old)
                || (old.status == PlanLifecycleV1::Deleted && new != old)
            {
                return Err(PublicationError::InvalidTransition);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replays the aggregate update from master 501453a2. The aggregate kept compiler V1,
    /// while edited Plans could already be V2. This is a persisted-record fixture, not a writer.
    fn master_501453a2_plan_content_publication() -> GatewayPublicationV1 {
        let source: GatewayPublicationV1 = GatewayPublicationV1::decode_persisted(include_bytes!(
            "../../../../e2e/product/golden/routing/compiled-publication.v2.json"
        ))
        .unwrap();
        let plan = source.plans[0].clone();
        let heads = source
            .plans
            .iter()
            .cloned()
            .map(|plan| {
                let version =
                    crate::PlanVersionV1::from_legacy_compiled(source.workspace_id.clone(), plan)
                        .unwrap();
                PlanHeadV1 {
                    head_revision: version.reference.content_revision,
                    model_alias: version.compiled.model_alias().clone(),
                    reference: version.reference,
                    status: PlanLifecycleV1::Enabled,
                }
            })
            .collect::<Vec<_>>();

        let mut next = source.clone();
        next.schema = GATEWAY_PUBLICATION_SCHEMA_V3.into();
        next.publication_revision = GatewayPublicationRevision::new(12).unwrap();
        next.alias_registry = source.alias_registry.clone();
        next.plans
            .retain(|candidate| candidate.agent_plan_id() != plan.agent_plan_id());
        next.plans.push(plan);
        next.plans
            .sort_by(|left, right| left.agent_plan_id().cmp(right.agent_plan_id()));
        next.plan_heads = heads;
        next.plan_heads
            .sort_by(|left, right| left.reference.plan_id.cmp(&right.reference.plan_id));
        next.aliases = materialize_aliases(&next.plans, &next.enabled_grants().unwrap()).unwrap();
        next.validate_transition_from(&source).unwrap();
        next
    }

    #[test]
    fn master_v3_compiler_v1_record_authenticates_migrates_and_republishes_current() {
        let persisted = master_501453a2_plan_content_publication();
        assert_eq!(persisted.schema, GATEWAY_PUBLICATION_SCHEMA_V3);
        assert_eq!(
            persisted.compiler_revision,
            crate::AGENT_PLAN_COMPILER_REVISION_V1
        );
        assert!(
            persisted
                .plans
                .iter()
                .all(|plan| { plan.body.schema == crate::AGENT_PLAN_COMPILED_SCHEMA_V1 })
        );

        let original_heads = persisted.plan_heads.clone();
        let original_grants = persisted.grants.clone();
        let original_materialized = persisted
            .plans
            .iter()
            .map(|plan| plan.body.materialized.clone())
            .collect::<Vec<_>>();
        let original_snapshot = persisted.gateway_snapshot().unwrap();
        let old_record =
            PublicationRecordV1::from_publication(persisted.workspace_id.clone(), &persisted)
                .unwrap();

        // The old digest is authenticated before any migration changes the covered bytes.
        let authenticated = old_record.verify().unwrap();
        assert_eq!(authenticated, persisted);
        // A persisted aggregate with one already-currentized Plan keeps its legacy name-set
        // grants; recovery derives the route bindings from the Plans exactly as persisted.
        let mut raw: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../../../e2e/product/golden/routing/compiled-publication.v2.json"
        ))
        .unwrap();
        let upgraded_plan = serde_json::to_value(
            serde_json::from_value::<crate::CompiledAgentPlanV1>(raw["plans"][0].clone())
                .unwrap()
                .into_current()
                .unwrap(),
        )
        .unwrap();
        raw["plans"][0] = upgraded_plan;
        let mixed =
            GatewayPublicationV1::decode_persisted(&serde_json::to_vec(&raw).unwrap()).unwrap();
        mixed.validate().unwrap();
        mixed
            .clone()
            .into_current()
            .unwrap()
            .validate_current_contract()
            .unwrap();
        let mut invalid = mixed.clone();
        invalid.compiler_revision = "unsupported-compiler".into();
        assert_eq!(
            invalid.validate(),
            Err(PublicationError::UnsupportedCompilerRevision)
        );
        let mut tampered = mixed;
        std::sync::Arc::make_mut(&mut tampered.plans[0].body).compiler_revision =
            crate::AGENT_PLAN_COMPILER_REVISION_V1.into();
        assert!(tampered.validate().is_err());
        let current = authenticated.into_current().unwrap();
        current.validate_current_contract().unwrap();
        assert_eq!(current.plan_heads, original_heads);
        // The plan upgrade re-binds grant route digests to the current compiled form; grant
        // identities, generations and route coverage stay sealed, and the upgrade is
        // idempotent over the current aggregate.
        for (current_grant, original_grant) in current.grants.iter().zip(&original_grants) {
            assert_eq!(current_grant.grant_id, original_grant.grant_id);
            assert_eq!(current_grant.generation, original_grant.generation);
            assert_eq!(
                current_grant.bearer_token_sha256,
                original_grant.bearer_token_sha256
            );
            assert_eq!(
                current_grant.model_grant.protocol,
                original_grant.model_grant.protocol
            );
            assert_eq!(
                current_grant.model_grant.routes.keys().collect::<Vec<_>>(),
                original_grant.model_grant.routes.keys().collect::<Vec<_>>()
            );
        }
        assert_eq!(current.clone().into_current().unwrap(), current);
        assert_eq!(current.gateway_snapshot().unwrap(), original_snapshot);
        assert_eq!(
            current
                .plans
                .iter()
                .map(|plan| plan.body.materialized.clone())
                .collect::<Vec<_>>(),
            original_materialized
                .into_iter()
                .map(|mut materialized| {
                    // Only obsolete ordering provenance changes; compare all remaining fields,
                    // including candidate order, limits and exact execution configuration.
                    for group in &mut materialized.attempt_owned.groups {
                        group.ordering_evidence = crate::MaterializedOrderingV1::ExplicitOrder;
                        group.pinned_ratings.clear();
                    }
                    materialized
                })
                .collect::<Vec<_>>()
        );

        // Re-emission stores only current bytes. A subsequent lifecycle write retains the Plan,
        // grant and immutable content identities while using the current compiler contract.
        let current_record =
            PublicationRecordV1::from_publication(current.workspace_id.clone(), &current).unwrap();
        let recovered = current_record.verify().unwrap();
        recovered.validate_current_contract().unwrap();
        let mut disabled = recovered.plan_heads[0].clone();
        disabled.head_revision += 1;
        disabled.status = PlanLifecycleV1::Disabled;
        let republished = recovered
            .next_with_plan_lifecycle(GatewayPublicationRevision::new(13).unwrap(), disabled)
            .unwrap();
        republished.validate_current_contract().unwrap();
        assert_eq!(republished.plans, recovered.plans);
        assert_eq!(republished.grants, recovered.grants);
        assert_eq!(
            republished.plan_heads[0].reference,
            recovered.plan_heads[0].reference
        );
    }

    #[test]
    fn persisted_master_accepts_v2_and_mixed_plans_without_rewriting_before_verification() {
        let source = master_501453a2_plan_content_publication();
        for converted_count in 0..=source.plans.len() {
            // Rebuild the aggregate exactly as the era's writer persisted it: legacy
            // name-set grants around a plan set that may already contain currentized Plans.
            let mut body = serde_json::to_value(&source).unwrap();
            body["grants"] = serde_json::Value::Array(
                source
                    .grants
                    .iter()
                    .map(|grant| {
                        serde_json::json!({
                            "grant_id": grant.grant_id,
                            "generation": grant.generation,
                            "bearer_token_sha256": grant.bearer_token_sha256,
                            "allowed_protocols": [grant.model_grant.protocol],
                            "allowed_aliases": grant
                                .model_grant
                                .routes
                                .keys()
                                .collect::<Vec<_>>(),
                        })
                    })
                    .collect(),
            );
            for plan in body["plans"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .take(converted_count)
            {
                let compiled =
                    serde_json::from_value::<crate::CompiledAgentPlanV1>(plan.clone()).unwrap();
                *plan = serde_json::to_value(compiled.into_current().unwrap()).unwrap();
            }
            let persisted =
                GatewayPublicationV1::decode_persisted(&serde_json::to_vec(&body).unwrap())
                    .unwrap();
            let record =
                PublicationRecordV1::from_publication(persisted.workspace_id.clone(), &persisted)
                    .unwrap();
            let authenticated = record.verify().unwrap();
            assert_eq!(authenticated, persisted);
            let current = authenticated.into_current().unwrap();
            current.validate_current_contract().unwrap();
            assert_eq!(current.plan_heads, persisted.plan_heads);
            assert_eq!(
                current.gateway_snapshot().unwrap(),
                persisted.gateway_snapshot().unwrap()
            );
            // Upgrading the remaining legacy Plans deterministically re-binds grant route
            // digests; once every Plan is current the grants pass through unchanged.
            assert_eq!(current.clone().into_current().unwrap(), current);
            if converted_count == source.plans.len() {
                assert_eq!(current.grants, persisted.grants);
            }
        }
    }

    fn migrated_publication_with_heads() -> GatewayPublicationV1 {
        let legacy: GatewayPublicationV1 = GatewayPublicationV1::decode_persisted(include_bytes!(
            "../../../../e2e/product/golden/routing/compiled-publication.v2.json"
        ))
        .unwrap();
        let heads = legacy
            .plans
            .iter()
            .cloned()
            .map(|plan| {
                let version =
                    crate::PlanVersionV1::from_legacy_compiled(legacy.workspace_id.clone(), plan)
                        .unwrap();
                PlanHeadV1 {
                    head_revision: version.reference.content_revision,
                    model_alias: version.compiled.model_alias().clone(),
                    reference: version.reference,
                    status: PlanLifecycleV1::Enabled,
                }
            })
            .collect();
        let mut current = legacy.into_current().unwrap();
        current.plan_heads = heads;
        current.validate_current_contract().unwrap();
        current
    }
    fn publication() -> GatewayPublicationV1 {
        let current = migrated_publication_with_heads();
        current
            .next_with_plan_content(
                GatewayPublicationRevision::new(12).unwrap(),
                current.alias_registry.clone(),
                current.plans[0].clone(),
                current.plan_heads.clone(),
            )
            .unwrap()
    }
    #[test]
    fn lifecycle_write_keeps_every_compiled_plan_current() {
        let current = migrated_publication_with_heads();
        let mut head = current.plan_heads[0].clone();
        head.head_revision += 1;
        head.status = PlanLifecycleV1::Disabled;
        let next = current
            .next_with_plan_lifecycle(GatewayPublicationRevision::new(12).unwrap(), head)
            .unwrap();
        next.validate_current_contract().unwrap();
        assert!(
            next.plans
                .iter()
                .all(|plan| plan.body.schema == crate::AGENT_PLAN_COMPILED_SCHEMA_V2)
        );
    }
    #[test]
    fn disable_projects_no_new_calls_without_revoking_configured_grants() {
        let mut current = publication();
        let grants = current.grants.clone();
        let saved = current.plans.clone();
        for original in current.plan_heads.clone() {
            let mut head = original;
            head.head_revision += 1;
            head.status = PlanLifecycleV1::Disabled;
            current = current
                .next_with_plan_lifecycle(
                    GatewayPublicationRevision::new(current.publication_revision.get() + 1)
                        .unwrap(),
                    head,
                )
                .unwrap();
        }
        assert_eq!(current.plans, saved);
        assert_eq!(current.grants, grants);
        assert!(current.aliases.is_empty());
        let projection = current.gateway_snapshot().unwrap();
        assert_eq!(projection.admission, GatewayAdmissionStateV1::NoNewCalls);
        assert!(projection.aliases.is_empty() && projection.grants.is_empty());
        assert_eq!(
            GatewayPublicationV1::decode(&current.canonical_bytes().unwrap()).unwrap(),
            current
        );
        let mut head = current.plan_heads[0].clone();
        let reference = head.reference.clone();
        head.status = PlanLifecycleV1::Enabled;
        head.head_revision += 1;
        let restored = current
            .next_with_plan_lifecycle(
                GatewayPublicationRevision::new(current.publication_revision.get() + 1).unwrap(),
                head,
            )
            .unwrap();
        assert_eq!(restored.plan_heads[0].reference, reference);
        assert!(!restored.aliases.is_empty());
        assert_eq!(restored.grants, grants);
    }
    #[test]
    fn referenced_delete_and_state_downgrade_fail_closed() {
        let current = publication();
        let mut head = current.plan_heads[0].clone();
        head.head_revision += 1;
        head.status = PlanLifecycleV1::Deleted;
        assert_eq!(
            current.next_with_plan_lifecycle(GatewayPublicationRevision::new(13).unwrap(), head),
            Err(PublicationError::InvalidGrant)
        );
        let mut downgraded = current.clone();
        downgraded.schema = LEGACY_GATEWAY_PUBLICATION_SCHEMA_V2.into();
        assert!(downgraded.validate().is_err());
        let mut inconsistent = current;
        inconsistent.plan_heads[0].reference.content_revision += 1;
        assert!(inconsistent.validate().is_err());
    }
}
