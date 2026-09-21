use super::{MAX_PRICE_RULES_PER_TARGET, PriceIndexEntryV1};
use hiroute_domain::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn validate_entry(entry: &PriceIndexEntryV1) -> Result<(), ComputeContractError> {
    entry.target.validate()?;
    let bounded = |s: &str| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control);
    if entry
        .actual_offer_ref
        .as_deref()
        .is_some_and(|s| !bounded(s))
        || entry
            .reference_model_offer_ref
            .as_ref()
            .is_some_and(|r| !bounded(&r.offer_ref) || !bounded(&r.model_configuration_id))
    {
        return Err(ComputeContractError::InvalidPrice);
    }
    if entry.catalog_rules.len() > MAX_PRICE_RULES_PER_TARGET
        || entry.legacy_overrides.len() > MAX_PRICE_RULES_PER_TARGET
    {
        return Err(ComputeContractError::InvalidPrice);
    }
    if let Some(value) = &entry.source_override {
        value.validate()?;
        if value.target != entry.target {
            return Err(ComputeContractError::InvalidPrice);
        }
    }
    if entry.reference_model_offer_ref.is_some()
        && entry.target.valuation_kind != PriceValuationKindV1::ApiEquivalent
    {
        return Err(ComputeContractError::InvalidPrice);
    }
    // Unmapped catalog prices cannot enter a source's index merely because a name matches.
    if !entry.catalog_rules.is_empty()
        && (entry.actual_offer_ref.is_none()
            || (entry.target.valuation_kind == PriceValuationKindV1::ApiEquivalent
                && entry.reference_model_offer_ref.is_none()))
    {
        return Err(ComputeContractError::CrossReference);
    }
    let mut rule_ids = BTreeSet::new();
    for rule in &entry.catalog_rules {
        rule.rule_ref.validate()?;
        if rule.rule_ref.id.is_empty()
            || rule.rule_ref.id.len() > 256
            || rule.rule_ref.revision == 0
            || !rule_ids.insert(&rule.rule_ref.id)
        {
            return Err(ComputeContractError::InvalidPrice);
        }
        if let Some(s) = &rule.schedule {
            // Mirror the stable schedule contract without inferring timezone transitions.
            if !bounded(&s.timezone_id)
                || !bounded(&s.timezone_data_revision)
                || !bounded(&s.schedule_compile_version)
                || s.weekdays.len() > 7
                || s.weekdays.is_empty()
                || s.weekdays.iter().any(|d| !(1..=7).contains(d))
                || s.start_minute > 1439
                || s.end_minute > 1439
                || s.start_minute == s.end_minute
                || s.effective_from >= s.effective_until
                || !(-840..=840).contains(&s.utc_offset_minutes)
            {
                return Err(ComputeContractError::InvalidPrice);
            }
        }
    }
    for legacy in &entry.legacy_overrides {
        legacy.validate()?;
        let expected_offer = entry
            .reference_model_offer_ref
            .as_ref()
            .map(|r| &r.offer_ref)
            .or(entry.actual_offer_ref.as_ref());
        let expected_model = entry
            .reference_model_offer_ref
            .as_ref()
            .map(|r| &r.model_configuration_id)
            .or(match &entry.target.model_identity {
                PriceModelIdentityV1::CatalogModel(id) => Some(id),
                _ => None,
            });
        if Some(&legacy.offer_ref) != expected_offer
            || Some(&legacy.model_configuration_id) != expected_model
            || legacy.currency != entry.target.currency
        {
            return Err(ComputeContractError::CrossReference);
        }
        if legacy
            .target_rule_id
            .as_ref()
            .is_some_and(|id| !rule_ids.contains(id))
        {
            return Err(ComputeContractError::InvalidPrice);
        }
    }
    Ok(())
}

pub(super) fn fill_quote(
    entry: &PriceIndexEntryV1,
    quote: &mut FrozenPriceQuoteV1,
) -> Result<(), ComputeContractError> {
    if let Some(value) = &entry.source_override {
        quote.applied_override_refs.push(PriceFactRefV1 {
            id: value.target.digest()?.to_string(),
            revision: value.revision,
            digest: CanonicalDigest::of(value).map_err(|_| ComputeContractError::InvalidPrice)?,
        });
        if let SourcePriceSettingV1::Set { rates } = &value.setting {
            quote.rates = rates.clone();
            quote.origin = PriceOriginV1::Manual;
            return Ok(());
        }
    }
    let follow_catalog = entry
        .source_override
        .as_ref()
        .is_some_and(|o| matches!(o.setting, SourcePriceSettingV1::FollowCatalog));
    let mut latest = BTreeMap::<&str, &PriceOverrideV1>::new();
    if !follow_catalog {
        for value in &entry.legacy_overrides {
            match latest.get(value.override_id.as_str()) {
                Some(old) if old.revision == value.revision && *old != value => {
                    quote.rates = TokenRatesV1::unknown(PriceUnknownReasonV1::AmbiguousRule);
                    return Ok(());
                }
                Some(old) if old.revision >= value.revision => (),
                _ => {
                    latest.insert(&value.override_id, value);
                }
            }
        }
    }
    let disabled = latest
        .values()
        .filter_map(|v| v.target_rule_id.as_ref())
        .collect::<BTreeSet<_>>();
    let mut base = vec![];
    let mut scheduled = vec![];
    for rule in &entry.catalog_rules {
        if disabled.contains(&rule.rule_ref.id) {
            continue;
        }
        match &rule.schedule {
            None => base.push(rule),
            Some(s) if s.matches(quote.attempt_execution_at) => scheduled.push(rule),
            _ => (),
        }
    }
    // Preserve V1: a scheduled price requires exactly one enabled base price.
    if base.len() != 1 {
        quote.rates = TokenRatesV1::unknown(if base.is_empty() {
            if disabled.is_empty() {
                PriceUnknownReasonV1::PriceNotCollected
            } else {
                PriceUnknownReasonV1::DisabledRule
            }
        } else {
            PriceUnknownReasonV1::AmbiguousRule
        });
        return Ok(());
    }
    let candidates = if scheduled.is_empty() {
        base
    } else {
        scheduled
    };
    let rule = match candidates.as_slice() {
        [rule] => *rule,
        [] => {
            quote.rates = TokenRatesV1::unknown(if disabled.is_empty() {
                PriceUnknownReasonV1::PriceNotCollected
            } else {
                PriceUnknownReasonV1::DisabledRule
            });
            return Ok(());
        }
        _ => {
            quote.rates = TokenRatesV1::unknown(PriceUnknownReasonV1::AmbiguousRule);
            return Ok(());
        }
    };
    quote.rates = rule.rates.clone();
    quote.applied_schedule = rule.schedule.clone();
    quote.selected_rule_refs.push(rule.rule_ref.clone());
    quote.origin = if entry.reference_model_offer_ref.is_some() {
        PriceOriginV1::ApiReference
    } else {
        PriceOriginV1::Catalog
    };
    let mut value_override = None;
    for value in latest.values() {
        quote.applied_override_refs.push(PriceFactRefV1 {
            id: value.override_id.clone(),
            revision: value.revision,
            digest: CanonicalDigest::of(value).map_err(|_| ComputeContractError::InvalidPrice)?,
        });
        if !matches!(value.operation, PriceOverrideOperation::DisableCatalogRule)
            && value_override.replace(*value).is_some()
        {
            quote.rates = TokenRatesV1::unknown(PriceUnknownReasonV1::AmbiguousRule);
            quote.origin = PriceOriginV1::Unknown;
            return Ok(());
        }
    }
    if !latest.is_empty() {
        quote.origin = PriceOriginV1::LegacyOverride;
    }
    if let Some(value) = value_override {
        quote.rates = match value.operation {
            PriceOverrideOperation::Replace => TokenRatesV1::from_legacy(
                value
                    .input_value
                    .ok_or(ComputeContractError::InvalidPrice)?,
                value
                    .output_value
                    .ok_or(ComputeContractError::InvalidPrice)?,
            ),
            PriceOverrideOperation::MultiplyPartsPerMillion => {
                let factor = value
                    .input_value
                    .ok_or(ComputeContractError::InvalidPrice)?;
                let multiply = |r: &TokenRateV1| -> Result<TokenRateV1, ComputeContractError> {
                    match r {
                        TokenRateV1::Known {
                            micros_per_million_tokens,
                        } => Ok(TokenRateV1::known(
                            micros_per_million_tokens
                                .checked_mul(factor)
                                .ok_or(ComputeContractError::InvalidPrice)?
                                / 1_000_000,
                        )),
                        TokenRateV1::Unknown { .. } => Ok(r.clone()),
                    }
                };
                TokenRatesV1 {
                    input_uncached: multiply(&quote.rates.input_uncached)?,
                    output: multiply(&quote.rates.output)?,
                    cache_read: TokenRateV1::unknown(PriceUnknownReasonV1::CacheRateNotCollected),
                    cache_write: TokenRateV1::unknown(PriceUnknownReasonV1::CacheRateNotCollected),
                }
            }
            PriceOverrideOperation::DisableCatalogRule => unreachable!("classified above"),
        };
    }
    Ok(())
}
