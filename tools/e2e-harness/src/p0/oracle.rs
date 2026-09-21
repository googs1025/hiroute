use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::bindings::RuntimeBindings;
use super::canonical::canonical_json_digest;
use super::contract::P0Bundle;
use super::types::*;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EvidenceSet {
    values: BTreeMap<(String, EvidenceSource), Value>,
}

impl EvidenceSet {
    pub fn insert(&mut self, case_id: &str, source: EvidenceSource, value: Value) {
        self.values.insert((case_id.to_owned(), source), value);
    }

    pub fn get(&self, case_id: &str, source: EvidenceSource) -> Option<&Value> {
        self.values.get(&(case_id.to_owned(), source))
    }

    pub(crate) fn extend(&mut self, other: EvidenceSet) {
        self.values.extend(other.values);
    }
}

pub fn exact_json_matches(expected: &Value, actual: &Value) -> bool {
    expected == actual
}

fn derive_assertion(
    assertion: &GoldenAssertion,
    expected: &Value,
    actual: Option<&Value>,
) -> AssertionOutcome {
    let (status, actual_digest, mismatch_path) = match actual {
        Some(actual) if exact_json_matches(expected, actual) => (
            AssertionStatus::Passed,
            Some(canonical_json_digest(actual)),
            None,
        ),
        Some(actual) => (
            AssertionStatus::Failed,
            Some(canonical_json_digest(actual)),
            Some(first_mismatch(expected, actual, "$")),
        ),
        None => (AssertionStatus::Missing, None, Some("$missing".into())),
    };
    AssertionOutcome {
        id: assertion.id.clone(),
        case_id: assertion.case_id.clone(),
        source: assertion.source,
        status,
        expected_digest: canonical_json_digest(expected),
        actual_digest,
        mismatch_path,
    }
}

pub(crate) fn evaluate(
    bundle: &P0Bundle,
    evidence: &EvidenceSet,
    topology: TopologyEvidence,
    bindings: &RuntimeBindings,
) -> VerifiedP0Run {
    let artifacts = bundle.artifacts();
    let evidence_entries: Vec<_> = artifacts
        .golden
        .assertions
        .iter()
        .map(|assertion| {
            let expected = bindings.materialize(&assertion.case_id, &assertion.expected);
            MaterializedAssertionEvidence {
                assertion_id: assertion.id.clone(),
                materialized_expected_digest: canonical_json_digest(&expected),
                actual: evidence.get(&assertion.case_id, assertion.source).cloned(),
            }
        })
        .collect();
    let assertions: Vec<_> = artifacts
        .golden
        .assertions
        .iter()
        .zip(&evidence_entries)
        .map(|(assertion, evidence)| {
            derive_assertion(
                assertion,
                &bindings.materialize(&assertion.case_id, &assertion.expected),
                evidence.actual.as_ref(),
            )
        })
        .collect();
    let status_by_id: BTreeMap<_, _> = assertions
        .iter()
        .map(|item| (item.id.as_str(), item.status))
        .collect();
    let coverage: Vec<_> = artifacts
        .scenario
        .coverage
        .iter()
        .map(|required| CoverageOutcome {
            bit: required.bit.clone(),
            status: derived_status(&required.assertion_ids, &status_by_id),
            assertion_ids: required.assertion_ids.clone(),
        })
        .collect();
    let checkpoints: Vec<_> = artifacts
        .scenario
        .checkpoints
        .iter()
        .map(|required| CheckpointOutcome {
            id: required.id,
            status: derived_status(&required.assertion_ids, &status_by_id),
            assertion_ids: required.assertion_ids.clone(),
        })
        .collect();
    let summary = summary(&assertions, &coverage);
    let green = is_green(&assertions, &coverage, &checkpoints);
    let trusted_expected_red = !green
        && artifacts
            .scenario
            .cases
            .iter()
            .all(|case| is_clean_not_implemented(case, evidence));
    let expected_red = if trusted_expected_red {
        expected_red_entries(&artifacts.scenario, &assertions)
    } else {
        Vec::new()
    };
    let mut report = P0RunReport {
        schema_version: RESULT_SCHEMA.to_owned(),
        oracle_version: ORACLE_VERSION.to_owned(),
        contract_digest: artifacts.manifest.contract_digest.clone(),
        evidence_digest: String::new(),
        result_payload_digest: String::new(),
        status: if green {
            OracleStatus::Green
        } else if trusted_expected_red {
            OracleStatus::ExpectedRed
        } else {
            OracleStatus::Red
        },
        topology,
        evidence: MaterializedEvidenceManifest {
            schema_version: EVIDENCE_MANIFEST_SCHEMA.to_owned(),
            binding_nonce: bindings.binding_nonce().to_owned(),
            producer_nonce: bindings.producer_nonce().to_owned(),
            assertions: evidence_entries,
        },
        assertions,
        coverage,
        checkpoints,
        expected_red,
        summary,
    };
    refresh_digests(&mut report);
    VerifiedP0Run(report)
}

pub(crate) fn verify_report_semantics(
    bundle: &P0Bundle,
    report: &P0RunReport,
) -> Result<(), String> {
    if report.schema_version != RESULT_SCHEMA
        || report.oracle_version != ORACLE_VERSION
        || report.contract_digest != bundle.artifacts().manifest.contract_digest
    {
        return Err("report provenance disagrees with the frozen bundle".into());
    }
    if report.evidence.schema_version != EVIDENCE_MANIFEST_SCHEMA
        || report.evidence.binding_nonce.len() < 32
        || report.evidence.producer_nonce.len() < 32
    {
        return Err("materialized evidence manifest provenance is invalid".into());
    }
    let golden = &bundle.artifacts().golden.assertions;
    if report.assertions.len() != golden.len() || report.evidence.assertions.len() != golden.len() {
        return Err("report assertion count disagrees with golden".into());
    }
    let bindings = RuntimeBindings::from_nonces(
        report.evidence.binding_nonce.clone(),
        report.evidence.producer_nonce.clone(),
        bundle
            .artifacts()
            .scenario
            .cases
            .iter()
            .map(|case| case.id.as_str()),
    );
    for ((outcome, expected), materialized) in report
        .assertions
        .iter()
        .zip(golden)
        .zip(&report.evidence.assertions)
    {
        let expected_value = bindings.materialize(&expected.case_id, &expected.expected);
        let expected_digest = canonical_json_digest(&expected_value);
        if materialized.assertion_id != expected.id
            || materialized.materialized_expected_digest != expected_digest
            || outcome.id != expected.id
            || outcome.case_id != expected.case_id
            || outcome.source != expected.source
            || outcome.expected_digest != expected_digest
        {
            return Err(format!(
                "assertion {} provenance is inconsistent",
                outcome.id
            ));
        }
        if *outcome != derive_assertion(expected, &expected_value, materialized.actual.as_ref()) {
            return Err("assertion outcomes are not derived from materialized evidence".into());
        }
    }
    let statuses: BTreeMap<_, _> = report
        .assertions
        .iter()
        .map(|item| (item.id.as_str(), item.status))
        .collect();
    verify_derived(bundle, report, &statuses)?;
    if report.summary != summary(&report.assertions, &report.coverage) {
        return Err("report summary is not derived from assertion and coverage outcomes".into());
    }
    let green = is_green(&report.assertions, &report.coverage, &report.checkpoints);
    let derived_expected_red =
        expected_red_entries(&bundle.artifacts().scenario, &report.assertions);
    let clean_expected_red = report_is_clean_expected_red(bundle, report);
    match report.status {
        OracleStatus::Green if green && report.expected_red.is_empty() => {}
        OracleStatus::ExpectedRed
            if !green
                && clean_expected_red
                && report.expected_red == derived_expected_red
                && !report.expected_red.is_empty() => {}
        OracleStatus::Red if !green && !clean_expected_red && report.expected_red.is_empty() => {}
        _ => return Err("report status is not derivable from its exact outcomes".into()),
    }
    if report.evidence_digest != evidence_digest(&report.evidence) {
        return Err("report evidence digest mismatch".into());
    }
    if report.result_payload_digest != result_payload_digest(report) {
        return Err("report payload digest mismatch".into());
    }
    Ok(())
}

fn verify_derived(
    bundle: &P0Bundle,
    report: &P0RunReport,
    statuses: &BTreeMap<&str, AssertionStatus>,
) -> Result<(), String> {
    let expected_coverage: Vec<_> = bundle
        .artifacts()
        .scenario
        .coverage
        .iter()
        .map(|item| CoverageOutcome {
            bit: item.bit.clone(),
            status: derived_status(&item.assertion_ids, statuses),
            assertion_ids: item.assertion_ids.clone(),
        })
        .collect();
    let expected_checkpoints: Vec<_> = bundle
        .artifacts()
        .scenario
        .checkpoints
        .iter()
        .map(|item| CheckpointOutcome {
            id: item.id,
            status: derived_status(&item.assertion_ids, statuses),
            assertion_ids: item.assertion_ids.clone(),
        })
        .collect();
    if report.coverage != expected_coverage || report.checkpoints != expected_checkpoints {
        return Err("coverage/checkpoint outcomes are not derived from assertions".into());
    }
    Ok(())
}

pub(crate) fn not_implemented_client_output() -> Value {
    json!({
        "body": {
            "code": NOT_IMPLEMENTED_CODE,
            "phase": "oracle_frozen_before_product",
            "schema_version": "hiroute.gateway.error/v1"
        },
        "content_type": "application/json",
        "status": 501,
        "transport": "http1"
    })
}

fn is_clean_not_implemented(case: &ScenarioCase, evidence: &EvidenceSet) -> bool {
    [
        EvidenceSource::NativeRequest,
        EvidenceSource::CanonicalLedger,
        EvidenceSource::ClientOutput,
        EvidenceSource::ExecutionFact,
        EvidenceSource::ConversationContent,
        EvidenceSource::Otel,
        EvidenceSource::Resource,
    ]
    .into_iter()
    .all(|source| {
        evidence
            .get(&case.id, source)
            .is_some_and(|value| value == &clean_not_implemented_evidence(source))
    })
}

fn clean_not_implemented_evidence(source: EvidenceSource) -> Value {
    let terminal = json!({
        "state": "not_started",
        "ack": null,
        "nack": null,
        "gap": null
    });
    match source {
        EvidenceSource::NativeRequest => json!({"entries": []}),
        EvidenceSource::CanonicalLedger => json!({"events": []}),
        EvidenceSource::ClientOutput => not_implemented_client_output(),
        EvidenceSource::ExecutionFact => json!({
            "schema_version": CHANNEL_SNAPSHOT_SCHEMA,
            "channel": "execution_fact",
            "records": [],
            "terminal": terminal
        }),
        EvidenceSource::ConversationContent => json!({
            "schema_version": CHANNEL_SNAPSHOT_SCHEMA,
            "channel": "conversation_content",
            "records": [],
            "terminal": terminal
        }),
        EvidenceSource::Otel => json!({"records": []}),
        EvidenceSource::Resource => json!({"facts": {
            "listener_allocation": "pingora_reserved_exact_address",
            "native_provider_aborted": 0,
            "native_provider_accepted": 0,
            "native_provider_active": 0,
            "native_provider_internal_failures": 0,
            "native_provider_parse_failed": 0,
            "private_root_mode": "owner_only",
            "secret_bearing_file_mode": "owner_read_write",
            "readiness_file_mode": "owner_read_write",
            "process_artifacts_mode": "owner_only",
            "evidence_artifacts_mode": "owner_only",
            "path_identity_preserved": true,
            "runtime_privacy_revalidated": true,
            "publication_snapshot": "typed_digest_verified",
            "readiness": "child_pid_nonce_digest_verified",
            "secret_scan_occurrences": 0,
            "sut_boundary": "reviewed_external_hirouted",
            "workdir_isolation": "distinct_private"
        }}),
    }
}

fn report_is_clean_expected_red(bundle: &P0Bundle, report: &P0RunReport) -> bool {
    report
        .evidence
        .assertions
        .iter()
        .zip(&bundle.artifacts().golden.assertions)
        .all(|(evidence, golden)| {
            evidence.actual.as_ref() == Some(&clean_not_implemented_evidence(golden.source))
        })
}

fn expected_red_entries(
    scenario: &ScenarioDocument,
    assertions: &[AssertionOutcome],
) -> Vec<ProductExpectedRed> {
    scenario
        .cases
        .iter()
        .map(|case| ProductExpectedRed {
            case_id: case.id.clone(),
            invariant_id: case.product_invariant.clone(),
            code: case.expected_red_code.clone(),
            failed_assertion_ids: assertions
                .iter()
                .filter(|assertion| {
                    assertion.case_id == case.id && assertion.status != AssertionStatus::Passed
                })
                .map(|assertion| assertion.id.clone())
                .collect(),
        })
        .collect()
}

fn summary(assertions: &[AssertionOutcome], coverage: &[CoverageOutcome]) -> RunSummary {
    RunSummary {
        assertion_passed: assertions
            .iter()
            .filter(|item| item.status == AssertionStatus::Passed)
            .count(),
        assertion_failed: assertions
            .iter()
            .filter(|item| item.status == AssertionStatus::Failed)
            .count(),
        assertion_missing: assertions
            .iter()
            .filter(|item| item.status == AssertionStatus::Missing)
            .count(),
        coverage_passed: coverage
            .iter()
            .filter(|item| item.status == DerivedStatus::Passed)
            .count(),
        coverage_required: coverage.len(),
    }
}

fn is_green(
    assertions: &[AssertionOutcome],
    coverage: &[CoverageOutcome],
    checkpoints: &[CheckpointOutcome],
) -> bool {
    assertions
        .iter()
        .all(|item| item.status == AssertionStatus::Passed)
        && coverage
            .iter()
            .all(|item| item.status == DerivedStatus::Passed)
        && checkpoints
            .iter()
            .all(|item| item.status == DerivedStatus::Passed)
}

fn derived_status(ids: &[String], statuses: &BTreeMap<&str, AssertionStatus>) -> DerivedStatus {
    if ids
        .iter()
        .all(|id| statuses.get(id.as_str()).copied() == Some(AssertionStatus::Passed))
    {
        DerivedStatus::Passed
    } else {
        DerivedStatus::Failed
    }
}

fn refresh_digests(report: &mut P0RunReport) {
    report.evidence_digest = evidence_digest(&report.evidence);
    report.result_payload_digest = result_payload_digest(report);
}

fn evidence_digest(evidence: &MaterializedEvidenceManifest) -> String {
    canonical_json_digest(&serde_json::to_value(evidence).expect("evidence serializes"))
}

fn result_payload_digest(report: &P0RunReport) -> String {
    let mut value = serde_json::to_value(report).expect("reports serialize");
    value["result_payload_digest"] = Value::String(String::new());
    canonical_json_digest(&value)
}

fn first_mismatch(expected: &Value, actual: &Value, path: &str) -> String {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            let keys: BTreeSet<_> = expected.keys().chain(actual.keys()).collect();
            for key in keys {
                let child = format!("{path}/{}", escape_pointer(key));
                match (expected.get(key), actual.get(key)) {
                    (Some(left), Some(right)) if left == right => {}
                    (Some(left), Some(right)) => return first_mismatch(left, right, &child),
                    _ => return child,
                }
            }
            path.to_owned()
        }
        (Value::Array(expected), Value::Array(actual)) => {
            for index in 0..expected.len().max(actual.len()) {
                let child = format!("{path}/{index}");
                match (expected.get(index), actual.get(index)) {
                    (Some(left), Some(right)) if left == right => {}
                    (Some(left), Some(right)) => return first_mismatch(left, right, &child),
                    _ => return child,
                }
            }
            path.to_owned()
        }
        _ => path.to_owned(),
    }
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_comparison_rejects_missing_extra_and_changed_fields() {
        let expected = json!({"model": "native", "stream": false});
        assert!(!exact_json_matches(&expected, &json!({"model": "native"})));
        assert!(!exact_json_matches(
            &expected,
            &json!({"model": "native", "stream": false, "extra": true})
        ));
    }

    #[test]
    fn mismatch_reports_the_first_exact_json_pointer() {
        let expected = json!({"entries": [{"body": {"model": "native"}}]});
        let actual = json!({"entries": [{"body": {"model": "wrong"}}]});
        assert_eq!(
            first_mismatch(&expected, &actual, "$"),
            "$/entries/0/body/model"
        );
    }
}
