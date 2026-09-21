use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::*;

const CONTROL_SCHEMA: &str = "hiroute.agent-config-permission-control/v1";
const EFFECT_SCHEMA: &str = "hiroute.agent-config-permission-effect/v1";
const COMMAND_ID: &str = "agents.config-permissions.apply";
const SCANNER_ID: &str = "builtin.agent-filesystem";
const SCANNER_VERSION: &str = "1";
const EFFECT_ID: &str = "agent-config-permission-hardening";
const SOURCE_PREFIX: &str = "claude/settings/";
const TARGET_PREFIX: &str = "scanner-source/";
const REQUIRED_MODE: u32 = 0o600;

/// Content-free identity issued by the exact filesystem scanner for one Claude settings file.
/// Private fields and the lack of `Deserialize` prevent callers from choosing a path or mode.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AgentConfigPermissionIntentV1 {
    scanner_id: &'static str,
    scanner_version: &'static str,
    source_ref: String,
    observed_identity: CanonicalDigest,
    observed_revision: u64,
    required_mode: u32,
}

impl AgentConfigPermissionIntentV1 {
    pub fn from_filesystem_scanner(
        source_ref: impl Into<String>,
        observed_identity: CanonicalDigest,
        observed_revision: u64,
    ) -> Result<Self, OperationValidationError> {
        let intent = Self {
            scanner_id: SCANNER_ID,
            scanner_version: SCANNER_VERSION,
            source_ref: source_ref.into(),
            observed_identity,
            observed_revision,
            required_mode: REQUIRED_MODE,
        };
        validate_intent(&intent)?;
        Ok(intent)
    }

    pub fn source_ref(&self) -> &str {
        &self.source_ref
    }

    pub fn observed_identity(&self) -> &CanonicalDigest {
        &self.observed_identity
    }

    pub const fn observed_revision(&self) -> u64 {
        self.observed_revision
    }
}

#[derive(Serialize)]
struct ControlEnvelopeV1<'a> {
    schema: &'static str,
    transaction: &'static str,
    intent: &'a AgentConfigPermissionIntentV1,
    change_spec_digest: CanonicalDigest,
}

#[derive(Serialize)]
struct EffectEnvelopeV1<'a> {
    schema: &'static str,
    transaction: &'static str,
    intent: &'a AgentConfigPermissionIntentV1,
    change_spec_digest: CanonicalDigest,
}

impl TransactionPlanV1 {
    pub fn from_agent_config_permission_planner(
        spec: ChangeSpecV1,
        intent: AgentConfigPermissionIntentV1,
    ) -> Result<Self, OperationValidationError> {
        validate_spec(&spec, &intent)?;
        let change_spec_digest = CanonicalDigest::of(&spec)?;
        let control = crate::canonicalize_json(serde_json::to_value(ControlEnvelopeV1 {
            schema: CONTROL_SCHEMA,
            transaction: "harden",
            intent: &intent,
            change_spec_digest: change_spec_digest.clone(),
        })?);
        let desired = crate::canonicalize_json(serde_json::to_value(EffectEnvelopeV1 {
            schema: EFFECT_SCHEMA,
            transaction: "harden",
            intent: &intent,
            change_spec_digest,
        })?);
        let external = vec![ExternalEffectIntentV1 {
            content_publication: None,
            effect_id: EFFECT_ID.to_owned(),
            kind: OwnedEffectKind::AgentArtifact,
            target: format!("{TARGET_PREFIX}{}", intent.source_ref),
            before_fingerprint: Some(intent.observed_identity.clone()),
            desired: desired.into(),
            desired_mode: REQUIRED_MODE,
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
    let control: DurableControlEnvelopeV1 = serde_json::from_value(control.clone())
        .map_err(|_| OperationValidationError::UnregisteredEffectPlan)?;
    let intent = control.intent.into_typed()?;
    if control.schema != CONTROL_SCHEMA
        || control.transaction != "harden"
        || control.change_spec_digest != CanonicalDigest::of(spec)?
    {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    validate_spec(spec, &intent)?;
    validate_external_components(
        &external[0].effect_id,
        external[0].kind,
        &external[0].target,
        external[0].before_fingerprint.as_ref(),
        &external[0].desired,
        external[0].desired_mode,
        external[0].sensitive,
    )?;
    let effect: DurableEffectEnvelopeV1 =
        serde::Deserialize::deserialize(external[0].desired.as_ref())
            .map_err(|_| OperationValidationError::UnregisteredEffectPlan)?;
    if effect.intent.into_typed()? != intent
        || effect.change_spec_digest != control.change_spec_digest
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
    let effect: DurableEffectEnvelopeV1 = serde_json::from_value(desired.clone())
        .map_err(|_| OperationValidationError::UnregisteredEffectPlan)?;
    let intent = effect.intent.into_typed()?;
    if effect_id != EFFECT_ID
        || kind != OwnedEffectKind::AgentArtifact
        || target != format!("{TARGET_PREFIX}{}", intent.source_ref)
        || before_fingerprint != Some(&intent.observed_identity)
        || desired_mode != REQUIRED_MODE
        || sensitive
        || effect.schema != EFFECT_SCHEMA
        || effect.transaction != "harden"
        || CanonicalDigest::parse(effect.change_spec_digest.as_str().to_owned()).is_err()
    {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    Ok(())
}

fn validate_spec(
    spec: &ChangeSpecV1,
    intent: &AgentConfigPermissionIntentV1,
) -> Result<(), OperationValidationError> {
    validate_intent(intent)?;
    if spec.command_id != COMMAND_ID
        || spec.resource_id.as_deref() != Some(&format!("agent-config/{}", intent.source_ref))
        || spec.desired_state
            != json!({
                "scanner_id": SCANNER_ID,
                "scanner_version": SCANNER_VERSION,
                "source_ref": intent.source_ref,
                "observed_identity": intent.observed_identity,
                "observed_revision": intent.observed_revision,
                "required_mode": REQUIRED_MODE,
            })
    {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    Ok(())
}

fn validate_intent(intent: &AgentConfigPermissionIntentV1) -> Result<(), OperationValidationError> {
    let suffix = intent
        .source_ref
        .strip_prefix(SOURCE_PREFIX)
        .ok_or(OperationValidationError::UnregisteredEffectPlan)?;
    if intent.scanner_id != SCANNER_ID
        || intent.scanner_version != SCANNER_VERSION
        || intent.observed_revision == 0
        || intent.required_mode != REQUIRED_MODE
        || suffix.len() != 32
        || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        || CanonicalDigest::parse(intent.observed_identity.as_str().to_owned()).is_err()
    {
        return Err(OperationValidationError::UnregisteredEffectPlan);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableIntentV1 {
    scanner_id: String,
    scanner_version: String,
    source_ref: String,
    observed_identity: CanonicalDigest,
    observed_revision: u64,
    required_mode: u32,
}

impl DurableIntentV1 {
    fn into_typed(self) -> Result<AgentConfigPermissionIntentV1, OperationValidationError> {
        if self.scanner_id != SCANNER_ID
            || self.scanner_version != SCANNER_VERSION
            || self.required_mode != REQUIRED_MODE
        {
            return Err(OperationValidationError::UnregisteredEffectPlan);
        }
        AgentConfigPermissionIntentV1::from_filesystem_scanner(
            self.source_ref,
            self.observed_identity,
            self.observed_revision,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableControlEnvelopeV1 {
    schema: String,
    transaction: String,
    intent: DurableIntentV1,
    change_spec_digest: CanonicalDigest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableEffectEnvelopeV1 {
    schema: String,
    transaction: String,
    intent: DurableIntentV1,
    change_spec_digest: CanonicalDigest,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (ChangeSpecV1, AgentConfigPermissionIntentV1) {
        let intent = AgentConfigPermissionIntentV1::from_filesystem_scanner(
            "claude/settings/0123456789abcdef0123456789abcdef",
            CanonicalDigest::of_bytes(b"identity"),
            7,
        )
        .unwrap();
        let spec = ChangeSpecV1 {
            schema_version: crate::CHANGE_SPEC_SCHEMA_V1,
            command_id: COMMAND_ID.to_owned(),
            resource_id: Some(format!("agent-config/{}", intent.source_ref)),
            desired_state: json!({
                "scanner_id": SCANNER_ID,
                "scanner_version": SCANNER_VERSION,
                "source_ref": intent.source_ref,
                "observed_identity": intent.observed_identity,
                "observed_revision": intent.observed_revision,
                "required_mode": REQUIRED_MODE,
            }),
        };
        (spec, intent)
    }

    #[test]
    fn permission_plan_fixes_one_mode_only_effect() {
        let (spec, intent) = fixture();
        let plan = TransactionPlanV1::from_agent_config_permission_planner(spec, intent).unwrap();
        assert!(plan.secrets().is_empty());
        assert!(plan.runtime().is_empty());
        assert_eq!(plan.external().len(), 1);
        assert_eq!(plan.external()[0].desired_mode(), 0o600);
        assert_eq!(plan.external()[0].effect_id(), EFFECT_ID);
    }

    #[test]
    fn permission_plan_rejects_caller_selected_target_or_mode() {
        let (spec, intent) = fixture();
        let mut plan =
            TransactionPlanV1::from_agent_config_permission_planner(spec, intent).unwrap();
        plan.external[0].target = "scanner-source/claude/settings/evil".to_owned();
        plan.external[0].desired_mode = 0o644;
        assert!(matches!(
            validate_registered_plan(
                &plan.spec,
                &plan.control,
                plan.credential_pool.as_ref(),
                &plan.secrets,
                &plan.runtime,
                &plan.external,
            ),
            Err(OperationValidationError::UnregisteredEffectPlan)
        ));
    }
}
