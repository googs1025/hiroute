use serde::Deserialize;

use super::*;

/// Owner-internal DTOs for the minimum registered compute planners. Formal public command DTOs
/// remain owned by their later handler PROCESS; arbitrary effect, URL, protocol, or auth fields
/// cannot deserialize through these closed shapes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ComputeConnectionPlannerInputV1 {
    connection_option_id: String,
    source_id: String,
    explicit_materialization: bool,
    expected_source_revision: u64,
    #[serde(default)]
    projection: Option<hiroute_domain::PreparedComputeProjectionV1>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CredentialPlannerInputV1 {
    connection_option_id: String,
    source_id: String,
    pool_id: String,
    binding_id: String,
    binding_revision: u64,
    offer_ref: String,
    offer_revision: u64,
    model_configuration_id: String,
    credential_id: String,
    #[serde(default)]
    input_slot: Option<String>,
    #[serde(default)]
    secret_source: Option<ProtectedInputSourceDescriptorV1>,
    expected_generation: u64,
    expected_pool_revision: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KeyPoolPlannerInputV1 {
    source_id: String,
    pool_id: String,
    ordered_credential_ids: Vec<String>,
    expected_pool_revision: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PriceOverridePlannerInputV1 {
    override_id: String,
    offer_ref: String,
    model_configuration_id: String,
    currency: String,
    operation: String,
    #[serde(default)]
    target_rule_id: Option<String>,
    #[serde(default)]
    input_value: Option<u64>,
    #[serde(default)]
    output_value: Option<u64>,
    expected_override_revision: u64,
}

pub(super) fn plan_connection<O: ConnectionOptionAuthorizationPort>(
    connection_options: &O,
    input: ComputeConnectionPlannerInputV1,
) -> Result<RegisteredEffectPlan, ChangePreparationError> {
    let authorization =
        registered_option_authorization(connection_options, &input.connection_option_id)?;
    if authorization.requires_explicit_materialization && !input.explicit_materialization {
        return Err(ChangePreparationError::ExplicitMaterializationRequired);
    }
    validate_identifier(&input.source_id)?;
    let source = connection_options
        .compute_source_materialization(
            &input.connection_option_id,
            &input.source_id,
            input.expected_source_revision,
            input.explicit_materialization,
        )?
        .ok_or(ChangePreparationError::SourceIdentityMismatch)?;
    let Some(projection) = input.projection else {
        // Keep the pre-control-plane operation shape readable for existing internal callers. The
        // production Local Control DTO always supplies a complete current projection and therefore
        // takes the transactionally complete branch below.
        return Ok(RegisteredEffectPlan {
            control: Value::Null,
            pending_compute_source: Some(source),
            credential_pool: None,
            pending_pool: None,
            secret_sources: Default::default(),
            secrets: Vec::new(),
            runtime: Vec::new(),
            external: Vec::new(),
        });
    };
    projection
        .validate()
        .map_err(|_| ChangePreparationError::SourceIdentityMismatch)?;
    if projection.desired.source != source.desired
        || projection.expected.source_revision != input.expected_source_revision
    {
        return Err(ChangePreparationError::SourceIdentityMismatch);
    }
    Ok(RegisteredEffectPlan {
        control: json!({
            "compute_projection_plan": {
                "current": source.current,
                "prepared": projection,
                "registry": source.registry,
                "explicit_materialization": source.explicit_materialization,
            }
        }),
        pending_compute_source: None,
        credential_pool: None,
        pending_pool: None,
        secret_sources: Default::default(),
        secrets: Vec::new(),
        runtime: Vec::new(),
        external: Vec::new(),
    })
}

pub(super) fn plan_credential<O: ConnectionOptionAuthorizationPort>(
    connection_options: &O,
    command_id: &str,
    input: CredentialPlannerInputV1,
    remove: bool,
) -> Result<RegisteredEffectPlan, ChangePreparationError> {
    for value in [
        &input.source_id,
        &input.pool_id,
        &input.binding_id,
        &input.offer_ref,
        &input.model_configuration_id,
        &input.credential_id,
    ] {
        validate_identifier(value)?;
    }
    let authorization =
        registered_option_authorization(connection_options, &input.connection_option_id)?;
    if !authorization.accepts_native_secret {
        return Err(ChangePreparationError::SecretNotAcceptedByConnectionOption);
    }
    if !connection_options
        .source_uses_connection_option(&input.source_id, &input.connection_option_id)?
    {
        return Err(ChangePreparationError::SourceIdentityMismatch);
    }
    if let Some(slot) = &input.input_slot {
        validate_input_slot(slot)?;
    }
    if remove != input.input_slot.is_none() || remove != input.secret_source.is_none() {
        return Err(if remove {
            ChangePreparationError::UnexpectedSecretInput
        } else {
            ChangePreparationError::SecretInputRequired
        });
    }
    if let Some(source) = &input.secret_source {
        validate_secret_source(source)?;
    }
    let action = match command_id {
        "compute.credential.add" => CredentialPoolMutationKind::Add,
        "compute.credential.replace" => CredentialPoolMutationKind::Replace,
        "compute.credential.remove" => CredentialPoolMutationKind::Remove,
        _ => return Err(ChangePreparationError::InvalidCredentialPool),
    };
    let identity = connection_options
        .credential_pool_identity(&input.pool_id, &input.binding_id)?
        .ok_or(ChangePreparationError::CredentialPoolNotFound)?;
    identity
        .validate_shape()
        .map_err(ChangePreparationError::ComputeContract)?;
    if identity.pool_id != input.pool_id
        || identity.binding_id != input.binding_id
        || identity.binding_revision != input.binding_revision
        || identity.source_id != input.source_id
        || identity.connection_option_id != input.connection_option_id
        || identity.offer_ref != input.offer_ref
        || identity.offer_revision != input.offer_revision
        || identity.model_configuration_id != input.model_configuration_id
    {
        return Err(ChangePreparationError::SourceIdentityMismatch);
    }
    let current_pool = connection_options.credential_pool(&input.pool_id)?;
    if let Some(current) = &current_pool {
        current
            .validate()
            .map_err(ChangePreparationError::ComputeContract)?;
        if current.identity() != identity {
            return Err(ChangePreparationError::SourceIdentityMismatch);
        }
    }
    if current_pool.as_ref().map_or(0, |pool| pool.revision) != input.expected_pool_revision
        || (current_pool.is_none() && action != CredentialPoolMutationKind::Add)
    {
        return Err(ChangePreparationError::CredentialPoolChanged);
    }
    let existing = current_pool.as_ref().and_then(|pool| {
        pool.credentials
            .iter()
            .find(|entry| entry.credential.credential_id() == input.credential_id)
    });
    match action {
        CredentialPoolMutationKind::Add if existing.is_some() => {
            return Err(ChangePreparationError::InvalidCredentialPool);
        }
        CredentialPoolMutationKind::Replace | CredentialPoolMutationKind::Remove
            if existing
                .is_none_or(|entry| entry.credential.generation() != input.expected_generation) =>
        {
            return Err(ChangePreparationError::SecretGenerationChanged);
        }
        _ => {}
    }
    let reference = CredentialRefV1::new(
        input.credential_id.clone(),
        format!("source/{}", input.source_id),
        "hirouted",
        "provider-auth",
        [format!("connection-option/{}", input.connection_option_id)],
        input.expected_generation,
    )?;
    let mutation = if let Some(slot) = input.input_slot {
        Some(SecretMutationV1::upsert(
            reference,
            input.expected_generation,
            slot,
            None,
        )?)
    } else {
        let references = connection_options.credential_reference_count(&input.credential_id)?;
        if references == 0 {
            return Err(ChangePreparationError::InvalidCredentialPool);
        }
        (references == 1)
            .then(|| SecretMutationV1::delete(reference, input.expected_generation))
            .transpose()?
    };
    let mut secret_sources = std::collections::BTreeMap::new();
    if let Some(source) = input.secret_source {
        secret_sources.insert(input.credential_id.clone(), source);
    }
    Ok(RegisteredEffectPlan {
        control: Value::Null,
        pending_compute_source: None,
        credential_pool: None,
        pending_pool: Some(PendingPoolMutationV1 {
            kind: action,
            identity,
            current: current_pool,
            credential_id: Some(input.credential_id),
        }),
        secret_sources,
        secrets: mutation.into_iter().collect(),
        runtime: Vec::new(),
        external: Vec::new(),
    })
}

pub(super) fn plan_key_pool<O: ConnectionOptionAuthorizationPort>(
    connection_options: &O,
    input: KeyPoolPlannerInputV1,
) -> Result<RegisteredEffectPlan, ChangePreparationError> {
    validate_identifier(&input.source_id)?;
    validate_identifier(&input.pool_id)?;
    if !connection_options.is_registered_compute_source(&input.source_id)? {
        return Err(ChangePreparationError::SourceIdentityMismatch);
    }
    if input.ordered_credential_ids.is_empty() {
        return Err(ChangePreparationError::InvalidDesiredState(
            serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "credential order cannot be empty",
            )),
        ));
    }
    let mut unique = BTreeSet::new();
    for credential_id in &input.ordered_credential_ids {
        validate_identifier(credential_id)?;
        if !unique.insert(credential_id) {
            return Err(ChangePreparationError::DuplicateCredentialId);
        }
    }
    let current = connection_options
        .credential_pool(&input.pool_id)?
        .ok_or(ChangePreparationError::CredentialPoolNotFound)?;
    if current.source_id != input.source_id || current.revision != input.expected_pool_revision {
        return Err(ChangePreparationError::CredentialPoolChanged);
    }
    let desired = current.reorder(current.revision, &input.ordered_credential_ids)?;
    let mutation = CredentialPoolMutationV1::from_registered_planner(
        CredentialPoolMutationKind::Reorder,
        Some(&current),
        desired,
    )?;
    Ok(RegisteredEffectPlan {
        control: json!({"credential_pool_mutation": &mutation}),
        pending_compute_source: None,
        credential_pool: Some(mutation),
        pending_pool: None,
        secret_sources: Default::default(),
        secrets: Vec::new(),
        runtime: Vec::new(),
        external: Vec::new(),
    })
}

pub(super) fn plan_price_override<O: ConnectionOptionAuthorizationPort>(
    connection_options: &O,
    input: PriceOverridePlannerInputV1,
) -> Result<RegisteredEffectPlan, ChangePreparationError> {
    for value in [
        &input.override_id,
        &input.offer_ref,
        &input.model_configuration_id,
        &input.currency,
        &input.operation,
    ] {
        validate_identifier(value)?;
    }
    if !matches!(
        input.operation.as_str(),
        "replace" | "multiply_parts_per_million" | "disable_catalog_rule"
    ) {
        return Err(ChangePreparationError::InvalidIdentifier);
    }
    if input.currency.len() != 3 || !input.currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(ChangePreparationError::InvalidIdentifier);
    }
    let values_valid = match input.operation.as_str() {
        "replace" => {
            input.target_rule_id.is_none()
                && input.input_value.is_some()
                && input.output_value.is_some()
        }
        "multiply_parts_per_million" => {
            input.target_rule_id.is_none()
                && input.input_value.is_some_and(|factor| factor > 0)
                && input.output_value.is_none()
        }
        "disable_catalog_rule" => {
            input
                .target_rule_id
                .as_deref()
                .is_some_and(|id| validate_identifier(id).is_ok())
                && input.input_value.is_none()
                && input.output_value.is_none()
        }
        _ => false,
    };
    if !values_valid {
        return Err(ChangePreparationError::InvalidIdentifier);
    }
    if !connection_options.is_registered_price_target(
        &input.offer_ref,
        &input.model_configuration_id,
        &input.currency,
        input.target_rule_id.as_deref(),
    )? {
        return Err(ChangePreparationError::UnknownPriceTarget);
    }
    let mut change = serde_json::Map::from_iter([
        ("override_id".into(), json!(input.override_id)),
        ("offer_ref".into(), json!(input.offer_ref)),
        (
            "model_configuration_id".into(),
            json!(input.model_configuration_id),
        ),
        ("currency".into(), json!(input.currency)),
        ("operation".into(), json!(input.operation)),
        (
            "expected_override_revision".into(),
            json!(input.expected_override_revision),
        ),
    ]);
    if let Some(value) = input.target_rule_id {
        change.insert("target_rule_id".into(), json!(value));
    }
    if let Some(value) = input.input_value {
        change.insert("input_value".into(), json!(value));
    }
    if let Some(value) = input.output_value {
        change.insert("output_value".into(), json!(value));
    }
    Ok(RegisteredEffectPlan {
        control: json!({"price_override_change": Value::Object(change)}),
        pending_compute_source: None,
        credential_pool: None,
        pending_pool: None,
        secret_sources: Default::default(),
        secrets: Vec::new(),
        runtime: Vec::new(),
        external: Vec::new(),
    })
}

fn validate_secret_source(
    source: &ProtectedInputSourceDescriptorV1,
) -> Result<(), ChangePreparationError> {
    match source {
        ProtectedInputSourceDescriptorV1::ManualInput => Ok(()),
        ProtectedInputSourceDescriptorV1::DiscoveredConfig {
            scanner_id,
            scanner_version,
            source_ref,
            field_selector,
            observed_revision,
        } => {
            for value in [scanner_id, scanner_version, source_ref, field_selector] {
                validate_identifier(value)?;
            }
            if *observed_revision == 0 {
                return Err(ChangePreparationError::InvalidIdentifier);
            }
            Ok(())
        }
    }
}
