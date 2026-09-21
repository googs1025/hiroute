use std::collections::{BTreeMap, BTreeSet};

use hiroute_domain::{
    DailyValueAggregateV1, FactsCompleteness, ObservationQueryError, VALUE_CALCULATION_BASIS_V1,
    ValueGroupByV1, ValueGroupedSummaryV1, ValueLedgerEntryV1, ValueQueryV1, ValueViewV1,
};

#[derive(Clone, Debug)]
struct Summary {
    seen: bool,
    tokens: [u64; 5],
    unsigned_amounts: [Option<u64>; 3],
    signed_amounts: [Option<i64>; 3],
    billing_unit_refs: BTreeSet<String>,
    price_version_refs: BTreeSet<String>,
    price_override_revision_refs: BTreeSet<String>,
    facts_completeness: FactsCompleteness,
    detail_available: bool,
}

impl Default for Summary {
    fn default() -> Self {
        Self {
            seen: false,
            tokens: [0; 5],
            unsigned_amounts: [None; 3],
            signed_amounts: [None; 3],
            billing_unit_refs: BTreeSet::new(),
            price_version_refs: BTreeSet::new(),
            price_override_revision_refs: BTreeSet::new(),
            facts_completeness: FactsCompleteness::Unknown,
            detail_available: true,
        }
    }
}

impl Summary {
    fn start_value(&mut self) {
        if !self.seen {
            self.seen = true;
            self.unsigned_amounts = [Some(0); 3];
            self.signed_amounts = [Some(0); 3];
            self.facts_completeness = FactsCompleteness::Complete;
        }
    }

    fn add_entry(&mut self, entry: &ValueLedgerEntryV1) -> Result<(), ObservationQueryError> {
        self.start_value();
        self.add_tokens([
            entry.usage.input_tokens,
            entry.usage.output_tokens,
            entry.usage.cache_read_tokens,
            entry.usage.cache_write_tokens,
            entry.usage.reasoning_tokens,
        ])?;
        self.add_amounts(
            [
                entry.frozen.baseline_api_equivalent_cost_micros,
                entry.frozen.chosen_api_equivalent_cost_micros,
                entry.frozen.actual_incremental_cost_micros,
            ],
            [
                entry.frozen.routing_savings_micros,
                entry.frozen.entitlement_savings_micros,
                entry.frozen.estimated_total_savings_micros,
            ],
        )?;
        self.billing_unit_refs
            .insert(entry.frozen.billing_unit.clone());
        self.price_version_refs
            .insert(entry.frozen.price_version.clone());
        self.price_override_revision_refs
            .extend(entry.frozen.price_override_revision.clone());
        self.facts_completeness = combine(self.facts_completeness, entry.facts_completeness);
        Ok(())
    }

    fn add_daily(
        &mut self,
        aggregate: &DailyValueAggregateV1,
    ) -> Result<(), ObservationQueryError> {
        self.start_value();
        self.detail_available = false;
        self.add_tokens([
            aggregate.input_tokens,
            aggregate.output_tokens,
            aggregate.cache_read_tokens,
            aggregate.cache_write_tokens,
            aggregate.reasoning_tokens,
        ])?;
        self.add_amounts(
            [
                aggregate.baseline_api_equivalent_cost_micros,
                aggregate.chosen_api_equivalent_cost_micros,
                aggregate.actual_incremental_cost_micros,
            ],
            [
                aggregate.routing_savings_micros,
                aggregate.entitlement_savings_micros,
                aggregate.estimated_total_savings_micros,
            ],
        )?;
        self.billing_unit_refs
            .insert(aggregate.billing_unit.clone());
        self.price_version_refs
            .extend(aggregate.price_version_refs.iter().cloned());
        self.price_override_revision_refs
            .extend(aggregate.price_override_revision_refs.iter().cloned());
        self.facts_completeness = combine(self.facts_completeness, aggregate.facts_completeness);
        Ok(())
    }

    fn add_tokens(&mut self, tokens: [u64; 5]) -> Result<(), ObservationQueryError> {
        for (total, value) in self.tokens.iter_mut().zip(tokens) {
            *total = total
                .checked_add(value)
                .ok_or(ObservationQueryError::Corrupt)?;
        }
        Ok(())
    }

    fn add_amounts(
        &mut self,
        unsigned: [Option<u64>; 3],
        signed: [Option<i64>; 3],
    ) -> Result<(), ObservationQueryError> {
        for (total, value) in self.unsigned_amounts.iter_mut().zip(unsigned) {
            *total = match (*total, value) {
                (Some(total), Some(value)) => Some(
                    total
                        .checked_add(value)
                        .ok_or(ObservationQueryError::Corrupt)?,
                ),
                _ => None,
            };
        }
        for (total, value) in self.signed_amounts.iter_mut().zip(signed) {
            *total = match (*total, value) {
                (Some(total), Some(value)) => Some(
                    total
                        .checked_add(value)
                        .ok_or(ObservationQueryError::Corrupt)?,
                ),
                _ => None,
            };
        }
        Ok(())
    }

    fn grouped(&self, day_number: Option<i64>) -> ValueGroupedSummaryV1 {
        ValueGroupedSummaryV1 {
            day_number,
            billing_unit_refs: self.billing_unit_refs.clone(),
            input_tokens: self.tokens[0],
            output_tokens: self.tokens[1],
            cache_read_tokens: self.tokens[2],
            cache_write_tokens: self.tokens[3],
            reasoning_tokens: self.tokens[4],
            baseline_api_equivalent_cost_micros: self.unsigned_amounts[0],
            chosen_api_equivalent_cost_micros: self.unsigned_amounts[1],
            actual_incremental_cost_micros: self.unsigned_amounts[2],
            routing_savings_micros: self.signed_amounts[0],
            entitlement_savings_micros: self.signed_amounts[1],
            estimated_total_savings_micros: self.signed_amounts[2],
            price_version_refs: self.price_version_refs.clone(),
            price_override_revision_refs: self.price_override_revision_refs.clone(),
            facts_completeness: self.facts_completeness,
            detail_available: self.detail_available,
        }
    }
}

pub(crate) fn aggregate_entries(
    query: &ValueQueryV1,
    entries: Vec<ValueLedgerEntryV1>,
    daily_aggregates: Vec<DailyValueAggregateV1>,
) -> Result<ValueViewV1, ObservationQueryError> {
    let mut overall = Summary::default();
    let mut days = BTreeMap::<i64, Summary>::new();
    for entry in &entries {
        overall.add_entry(entry)?;
        if query.group_by == ValueGroupByV1::Day {
            days.entry(entry.frozen_at_ms.div_euclid(86_400_000))
                .or_default()
                .add_entry(entry)?;
        }
    }
    for aggregate in &daily_aggregates {
        overall.add_daily(aggregate)?;
        if query.group_by == ValueGroupByV1::Day {
            days.entry(aggregate.day_number)
                .or_default()
                .add_daily(aggregate)?;
        }
    }
    let groups = match query.group_by {
        ValueGroupByV1::None if overall.seen => vec![overall.grouped(None)],
        ValueGroupByV1::None => Vec::new(),
        ValueGroupByV1::Day => days
            .into_iter()
            .map(|(day, summary)| summary.grouped(Some(day)))
            .collect(),
    };
    Ok(ValueViewV1 {
        value_calculation_basis: VALUE_CALCULATION_BASIS_V1.to_owned(),
        agent_plan_id: query.agent_plan_id.clone(),
        currency: query.currency.clone(),
        group_by: query.group_by,
        groups,
        entries,
        daily_aggregates,
        input_tokens: overall.tokens[0],
        output_tokens: overall.tokens[1],
        cache_read_tokens: overall.tokens[2],
        cache_write_tokens: overall.tokens[3],
        reasoning_tokens: overall.tokens[4],
        baseline_api_equivalent_cost_micros: overall.unsigned_amounts[0],
        chosen_api_equivalent_cost_micros: overall.unsigned_amounts[1],
        actual_incremental_cost_micros: overall.unsigned_amounts[2],
        routing_savings_micros: overall.signed_amounts[0],
        entitlement_savings_micros: overall.signed_amounts[1],
        estimated_total_savings_micros: overall.signed_amounts[2],
        price_version_refs: overall.price_version_refs,
        price_override_revision_refs: overall.price_override_revision_refs,
        facts_completeness: overall.facts_completeness,
        detail_available: overall.detail_available,
    })
}

const fn combine(left: FactsCompleteness, right: FactsCompleteness) -> FactsCompleteness {
    match (left, right) {
        (FactsCompleteness::Unknown, _) | (_, FactsCompleteness::Unknown) => {
            FactsCompleteness::Unknown
        }
        (FactsCompleteness::Partial, _) | (_, FactsCompleteness::Partial) => {
            FactsCompleteness::Partial
        }
        _ => FactsCompleteness::Complete,
    }
}
