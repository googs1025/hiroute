//! Sealed transaction plan for an explicitly approved connector subscription check.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::*;

pub const COMPUTE_SUBSCRIPTION_CHECK_COMMAND_ID_V2: &str = "compute.subscription.check.apply";
pub const COMPUTE_SUBSCRIPTION_EFFECT_ID_V2: &str = "compute-subscription-materialization";
const CONTROL_SCHEMA: &str = "hiroute.compute-subscription-control/v2";
const EFFECT_SCHEMA: &str = "hiroute.compute-subscription-effect/v2";
const TARGET_PREFIX: &str = "compute-subscription/";

/// Safe facts admitted by the Application planner. The protected source descriptor is resolved
/// later by the trusted adapter and therefore cannot enter this durable plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SubscriptionCheckIntentV2 {
    candidate_ref: String,
    candidate_revision: u64,
    expected_evidence_digest: CanonicalDigest,
    existing_source: Option<SubscriptionSavedSourceV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionSavedSourceV2 {
    source_id: String,
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableChangeV2 {
    schema: String,
    candidate: DurableCandidateV2,
    expected_evidence_digest: CanonicalDigest,
    #[serde(default)]
    existing_source: Option<SubscriptionSavedSourceV2>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableCandidateV2 {
    candidate_ref: String,
    candidate_revision: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeV2 {
    schema: String,
    transaction: String,
    candidate_ref: String,
    candidate_revision: u64,
    expected_evidence_digest: CanonicalDigest,
    existing_source: Option<SubscriptionSavedSourceV2>,
    change_spec_digest: CanonicalDigest,
}

impl SubscriptionCheckIntentV2 {
    pub fn from_application(
        candidate_ref: impl Into<String>,
        candidate_revision: u64,
        expected_evidence_digest: CanonicalDigest,
        existing_source: Option<(String, u64)>,
    ) -> Result<Self, OperationValidationError> {
        let value = Self {
            candidate_ref: candidate_ref.into(),
            candidate_revision,
            expected_evidence_digest,
            existing_source: existing_source.map(|(source_id, expected_revision)| {
                SubscriptionSavedSourceV2 {
                    source_id,
                    expected_revision,
                }
            }),
        };
        validate_intent(&value)?;
        Ok(value)
    }

    pub fn candidate_ref(&self) -> &str {
        &self.candidate_ref
    }

    pub const fn candidate_revision(&self) -> u64 {
        self.candidate_revision
    }

    pub fn expected_evidence_digest(&self) -> &CanonicalDigest {
        &self.expected_evidence_digest
    }

    pub fn existing_source(&self) -> Option<(&str, u64)> {
        self.existing_source
            .as_ref()
            .map(|value| (value.source_id.as_str(), value.expected_revision))
    }

    fn target(&self) -> Result<String, OperationValidationError> {
        let digest = CanonicalDigest::of(&(
            "hiroute.compute-subscription-target/v2",
            &self.candidate_ref,
        ))?;
        Ok(format!(
            "{TARGET_PREFIX}{}",
            digest.as_str().trim_start_matches("sha256:")
        ))
    }
}

impl TransactionPlanV1 {
    pub fn from_compute_subscription_check_planner(
        spec: ChangeSpecV1,
        intent: SubscriptionCheckIntentV2,
    ) -> Result<Self, OperationValidationError> {
        validate_spec(&spec, &intent)?;
        let change_spec_digest = CanonicalDigest::of(&spec)?;
        let control = envelope(CONTROL_SCHEMA, &intent, change_spec_digest.clone())?;
        let desired = envelope(EFFECT_SCHEMA, &intent, change_spec_digest)?;
        let external = vec![ExternalEffectIntentV1 {
            content_publication: None,
            effect_id: COMPUTE_SUBSCRIPTION_EFFECT_ID_V2.to_owned(),
            kind: OwnedEffectKind::AgentArtifact,
            target: intent.target()?,
            before_fingerprint: Some(intent.expected_evidence_digest.clone()),
            desired: desired.into(),
            desired_mode: 0o600,
            sensitive: false,
        }];
        validate_plan(&spec, &control, &[], &[], &external)?;
        Ok(Self {
            spec,
            control,
            credential_pool: None,
            worker_dependency_selection: None,
            secrets: Vec::new(),
            agent_access_grants: Vec::new(),
            runtime: Vec::new(),
            external,
        })
    }
}

fn envelope(
    schema: &str,
    intent: &SubscriptionCheckIntentV2,
    change_spec_digest: CanonicalDigest,
) -> Result<Value, OperationValidationError> {
    Ok(crate::canonicalize_json(serde_json::to_value(
        EnvelopeV2 {
            schema: schema.to_owned(),
            transaction: "materialize".to_owned(),
            candidate_ref: intent.candidate_ref.clone(),
            candidate_revision: intent.candidate_revision,
            expected_evidence_digest: intent.expected_evidence_digest.clone(),
            existing_source: intent.existing_source.clone(),
            change_spec_digest,
        },
    )?))
}

fn validate_intent(intent: &SubscriptionCheckIntentV2) -> Result<(), OperationValidationError> {
    validate_scope_identifier(&intent.candidate_ref)?;
    if intent.candidate_revision == 0
        || intent.expected_evidence_digest == CanonicalDigest::of_bytes(&[])
        || intent.existing_source.as_ref().is_some_and(|source| {
            source.expected_revision == 0 || validate_scope_identifier(&source.source_id).is_err()
        })
    {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    Ok(())
}

fn validate_spec(
    spec: &ChangeSpecV1,
    intent: &SubscriptionCheckIntentV2,
) -> Result<(), OperationValidationError> {
    let change: DurableChangeV2 = serde_json::from_value(spec.desired_state.clone())
        .map_err(|_| OperationValidationError::UnregisteredEffectPlan)?;
    if spec.command_id != COMPUTE_SUBSCRIPTION_CHECK_COMMAND_ID_V2
        || spec.resource_id.as_deref() != Some(intent.candidate_ref())
        || change.schema != "hiroute.compute-subscription-check/v2"
        || change.candidate.candidate_ref != intent.candidate_ref
        || change.candidate.candidate_revision != intent.candidate_revision
        || change.expected_evidence_digest != intent.expected_evidence_digest
        || change.existing_source != intent.existing_source
    {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    Ok(())
}

pub(super) fn validate_plan(
    spec: &ChangeSpecV1,
    control: &Value,
    secrets: &[SecretMutationV1],
    runtime: &[RuntimeMutationV1],
    external: &[ExternalEffectIntentV1],
) -> Result<(), OperationValidationError> {
    if !secrets.is_empty() || !runtime.is_empty() || external.len() != 1 {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    let envelope = decode_envelope(control, CONTROL_SCHEMA)?;
    let intent = intent_from_envelope(&envelope)?;
    validate_spec(spec, &intent)?;
    if envelope.change_spec_digest != CanonicalDigest::of(spec)? {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    validate_external_components(
        &external[0].effect_id,
        external[0].kind,
        &external[0].target,
        external[0].before_fingerprint.as_ref(),
        &external[0].desired,
        external[0].desired_mode,
        external[0].sensitive,
    )?;
    let effect = decode_envelope(&external[0].desired, EFFECT_SCHEMA)?;
    if effect.candidate_ref != envelope.candidate_ref
        || effect.candidate_revision != envelope.candidate_revision
        || effect.expected_evidence_digest != envelope.expected_evidence_digest
        || effect.existing_source != envelope.existing_source
        || effect.change_spec_digest != envelope.change_spec_digest
    {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_external_components(
    effect_id: &str,
    kind: OwnedEffectKind,
    target: &str,
    before_fingerprint: Option<&CanonicalDigest>,
    desired: &Value,
    desired_mode: u32,
    sensitive: bool,
) -> Result<(), OperationValidationError> {
    let envelope = decode_envelope(desired, EFFECT_SCHEMA)?;
    let intent = intent_from_envelope(&envelope)?;
    if effect_id != COMPUTE_SUBSCRIPTION_EFFECT_ID_V2
        || kind != OwnedEffectKind::AgentArtifact
        || target != intent.target()?
        || before_fingerprint != Some(intent.expected_evidence_digest())
        || desired_mode != 0o600
        || sensitive
    {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    Ok(())
}

pub fn decode_subscription_check_intent(
    desired: &Value,
) -> Result<SubscriptionCheckIntentV2, OperationValidationError> {
    let envelope = decode_envelope(desired, EFFECT_SCHEMA)?;
    intent_from_envelope(&envelope)
}

fn decode_envelope(value: &Value, schema: &str) -> Result<EnvelopeV2, OperationValidationError> {
    let envelope: EnvelopeV2 = serde_json::from_value(value.clone())
        .map_err(|_| OperationValidationError::UnregisteredEffectPlan)?;
    if envelope.schema != schema || envelope.transaction != "materialize" {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    Ok(envelope)
}

fn intent_from_envelope(
    envelope: &EnvelopeV2,
) -> Result<SubscriptionCheckIntentV2, OperationValidationError> {
    SubscriptionCheckIntentV2::from_application(
        envelope.candidate_ref.clone(),
        envelope.candidate_revision,
        envelope.expected_evidence_digest.clone(),
        envelope
            .existing_source
            .as_ref()
            .map(|source| (source.source_id.clone(), source.expected_revision)),
    )
}

pub fn is_subscription_check_effect(intent: &ExternalEffectIntentV1) -> bool {
    intent.effect_id() == COMPUTE_SUBSCRIPTION_EFFECT_ID_V2
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plan_contains_only_safe_subscription_identity() {
        let intent = SubscriptionCheckIntentV2::from_application(
            "candidate/cpa/test",
            1,
            CanonicalDigest::of_bytes(b"evidence"),
            None,
        )
        .unwrap();
        let spec = ChangeSpecV1 {
            schema_version: crate::CHANGE_SPEC_SCHEMA_V1,
            command_id: COMPUTE_SUBSCRIPTION_CHECK_COMMAND_ID_V2.into(),
            resource_id: Some("candidate/cpa/test".into()),
            desired_state: json!({
                "schema":"hiroute.compute-subscription-check/v2",
                "candidate":{"candidate_ref":"candidate/cpa/test","candidate_revision":1},
                "expected_evidence_digest": CanonicalDigest::of_bytes(b"evidence")
            }),
        };
        let plan =
            TransactionPlanV1::from_compute_subscription_check_planner(spec, intent).unwrap();
        assert_eq!(plan.external.len(), 1);
        assert!(!serde_json::to_string(&plan).unwrap().contains("auth.json"));
    }
}
