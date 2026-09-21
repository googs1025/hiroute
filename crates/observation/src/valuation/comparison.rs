use super::*;
use hiroute_domain::FrozenPriceQuoteV1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReferenceComparisonV2 {
    pub reference_quote: FrozenPriceQuoteV1,
    pub baseline_micros: Option<u64>,
    pub observed_estimate_micros: Option<u64>,
    pub savings_micros: Option<i64>,
    /// Estimates and subscription API equivalents never prove an actual bill.
    pub actual_cash_micros: Option<u64>,
    pub disclosure: String,
}

/// Compare the accepted request's proven usage against the explicit reference,
/// including all observed attempts on the actual side. No current price lookup.
pub fn compare_reference(
    reference: &FrozenPriceQuoteV1,
    accepted_usage: &UsageBucketsV2,
    attempts: &[AttemptValuationV2],
    complete_terminal: bool,
) -> Result<ReferenceComparisonV2, ValuationError> {
    let baseline = value_attempt(reference, accepted_usage)?;
    let comparable = complete_terminal
        && !attempts.is_empty()
        && reference.generation_ref.is_some()
        && baseline.coverage == MetricCoverage::Complete
        && attempts.iter().all(|attempt| {
            attempt.coverage == MetricCoverage::Complete
                && attempt.quote.generation_ref == reference.generation_ref
                && attempt.quote.exact_target.workspace_id == reference.exact_target.workspace_id
                && attempt.currency == reference.currency
                && attempt.quote.billing_context == reference.billing_context
        });
    let actual = if comparable {
        attempts.iter().try_fold(0u64, |sum, attempt| {
            sum.checked_add(attempt.known_sum_micros?)
        })
    } else {
        None
    };
    let savings = baseline
        .known_sum_micros
        .zip(actual)
        .and_then(|(baseline, actual)| {
            i64::try_from(i128::from(baseline) - i128::from(actual)).ok()
        });
    Ok(ReferenceComparisonV2 {
        reference_quote: reference.clone(), baseline_micros: baseline.known_sum_micros,
        observed_estimate_micros: actual, savings_micros: savings, actual_cash_micros: None,
        disclosure: "Estimated from observed tokens; output and quality equivalence are not established. This is not a bill or cash saving.".into(),
    })
}
