use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::bindings::RuntimeBindings;
use super::canonical::sha256_hex;
use super::contract::P0Bundle;
use super::oracle::EvidenceSet;
use super::privacy::{PrivateFileIdentity, snapshot_private_bounded};
use super::provider::ObservedNativeRequest;
use super::schema::validate_document;
use super::types::{
    ArtifactKind, CHANNEL_SNAPSHOT_SCHEMA, CONVERSATION_CONTENT_SCHEMA, EXECUTION_FACT_SCHEMA,
    EvidenceSource, ObservationChannel, ObservationRequestTerminal, ObservationTerminalReceipt,
    ScenarioDocument, TERMINAL_RECEIPT_SCHEMA,
};

const MAX_CHANNEL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RECEIPT_BYTES: u64 = 128 * 1024;

pub(crate) fn collect_native(
    scenario: &ScenarioDocument,
    observations: &BTreeMap<String, Vec<ObservedNativeRequest>>,
    client_outputs: &BTreeMap<String, Value>,
    evidence: &mut EvidenceSet,
) {
    for case in &scenario.cases {
        let mut entries = observations.get(&case.id).cloned().unwrap_or_default();
        entries.sort_by_key(|entry| entry.sequence);
        let values: Vec<_> = entries
            .into_iter()
            .enumerate()
            .map(|(index, mut entry)| {
                entry
                    .value
                    .as_object_mut()
                    .expect("native evidence is typed")
                    .insert(
                        "ordinal".into(),
                        Value::from(index.saturating_add(1) as u64),
                    );
                entry.value
            })
            .collect();
        evidence.insert(
            &case.id,
            EvidenceSource::NativeRequest,
            json!({"entries": values}),
        );
        if let Some(output) = client_outputs.get(&case.id) {
            evidence.insert(&case.id, EvidenceSource::ClientOutput, output.clone());
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObservationSnapshot {
    evidence: EvidenceSet,
    identities: BTreeMap<String, PrivateFileIdentity>,
    artifact_digests: BTreeMap<String, String>,
}

pub(crate) fn collect_observations(
    bundle: &P0Bundle,
    evidence_dir: &Path,
    run_nonce: &str,
    child_pid: u32,
    bindings: &RuntimeBindings,
    clean_not_implemented: bool,
    evidence: &mut EvidenceSet,
) -> Result<(), CollectorError> {
    match snapshot_observations(bundle, evidence_dir, run_nonce, child_pid, bindings)? {
        Some(snapshot) => evidence.extend(snapshot.evidence),
        None => match audit_unreceipted_directory(evidence_dir)? {
            EvidenceDirectoryState::Empty if clean_not_implemented => {
                insert_not_started(&bundle.artifacts().scenario, evidence)
            }
            EvidenceDirectoryState::Empty => {}
            EvidenceDirectoryState::Artifacts(facts) => {
                insert_unreceipted(&bundle.artifacts().scenario, facts, evidence)
            }
        },
    }
    Ok(())
}

pub(crate) fn snapshot_observations(
    bundle: &P0Bundle,
    evidence_dir: &Path,
    run_nonce: &str,
    child_pid: u32,
    bindings: &RuntimeBindings,
) -> Result<Option<ObservationSnapshot>, CollectorError> {
    let receipt_path = evidence_dir.join("terminal-receipt.json");
    match fs::symlink_metadata(&receipt_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    verify_complete_artifact_set(evidence_dir)?;
    let receipt_snapshot = snapshot_private_bounded(&receipt_path, MAX_RECEIPT_BYTES)?;
    let receipt_value: Value = serde_json::from_slice(&receipt_snapshot.bytes)?;
    validate_artifact_schema(bundle, ArtifactKind::TerminalReceiptSchema, &receipt_value)?;
    let receipt: ObservationTerminalReceipt = serde_json::from_value(receipt_value)?;
    if receipt.schema_version != TERMINAL_RECEIPT_SCHEMA
        || receipt.run_nonce != run_nonce
        || receipt.flush_state != "channels_synced"
        || receipt.producer.component != "hirouted"
        || receipt.producer.instance_id != run_nonce
        || receipt.producer.process_id != child_pid
    {
        return Err(CollectorError::WrongProducer);
    }
    let expected_channels = BTreeSet::from([
        ObservationChannel::CanonicalLedger,
        ObservationChannel::ExecutionFact,
        ObservationChannel::ConversationContent,
        ObservationChannel::Otel,
    ]);
    let actual_channels: BTreeSet<_> = receipt
        .channels
        .iter()
        .map(|channel| channel.channel)
        .collect();
    if actual_channels != expected_channels || receipt.channels.len() != expected_channels.len() {
        return Err(CollectorError::ChannelSet);
    }
    let mut evidence = EvidenceSet::default();
    let mut identities = BTreeMap::from([(
        "terminal-receipt.json".to_owned(),
        receipt_snapshot.identity,
    )]);
    let mut artifact_digests = BTreeMap::from([(
        "terminal-receipt.json".to_owned(),
        sha256_hex(&receipt_snapshot.bytes),
    )]);
    for channel in receipt.channels {
        let path = evidence_dir.join(channel.channel.file_name());
        let snapshot = snapshot_private_bounded(&path, MAX_CHANNEL_BYTES)?;
        let bytes = snapshot.bytes;
        if bytes.len() as u64 != channel.file_size || sha256_hex(&bytes) != channel.file_sha256 {
            return Err(CollectorError::ReceiptMismatch(channel.channel));
        }
        identities.insert(channel.channel.file_name().to_owned(), snapshot.identity);
        artifact_digests.insert(channel.channel.file_name().to_owned(), sha256_hex(&bytes));
        let records = parse_records(&bytes, channel.channel, bindings)?;
        if channel.channel == ObservationChannel::ExecutionFact {
            ensure_record_version(&records, EXECUTION_FACT_SCHEMA)?;
            validate_records(bundle, ArtifactKind::ExecutionFactSchema, &records)?;
        }
        if channel.channel == ObservationChannel::ConversationContent {
            ensure_record_version(&records, CONVERSATION_CONTENT_SCHEMA)?;
            validate_records(bundle, ArtifactKind::ConversationContentSchema, &records)?;
        }
        let terminals = validate_request_terminals(
            &bundle.artifacts().scenario,
            channel.channel,
            &channel.terminals,
            bindings,
        )?;
        insert_channel(
            &bundle.artifacts().scenario,
            channel.channel,
            records,
            &terminals,
            bindings,
            &mut evidence,
        );
    }
    Ok(Some(ObservationSnapshot {
        evidence,
        identities,
        artifact_digests,
    }))
}

fn ensure_record_version(records: &[Value], expected: &'static str) -> Result<(), CollectorError> {
    if records
        .iter()
        .all(|record| record["schema_version"].as_str() == Some(expected))
    {
        Ok(())
    } else {
        Err(CollectorError::RecordVersion(expected))
    }
}

fn parse_records(
    bytes: &[u8],
    channel: ObservationChannel,
    bindings: &RuntimeBindings,
) -> Result<Vec<Value>, CollectorError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CollectorError::Utf8(channel))?;
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(line).map_err(|source| CollectorError::JsonLine {
                channel,
                line: index + 1,
                source,
            })?;
        let request_id = value
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or(CollectorError::MissingRequestId(channel, index + 1))?;
        if bindings.case_for_request(request_id).is_none() {
            return Err(CollectorError::UnknownRequest(
                channel,
                request_id.to_owned(),
            ));
        }
        records.push(value);
    }
    Ok(records)
}

fn validate_request_terminals(
    scenario: &ScenarioDocument,
    channel: ObservationChannel,
    terminals: &[ObservationRequestTerminal],
    bindings: &RuntimeBindings,
) -> Result<BTreeMap<String, Value>, CollectorError> {
    let expected_requests: BTreeSet<_> = scenario
        .cases
        .iter()
        .map(|case| {
            bindings
                .request_id(&case.id)
                .expect("every validated scenario case has a runtime request ID")
                .to_owned()
        })
        .collect();
    let actual_requests: BTreeSet<_> = terminals
        .iter()
        .map(|terminal| terminal.request_id.clone())
        .collect();
    if terminals.len() != expected_requests.len() || actual_requests != expected_requests {
        return Err(CollectorError::RequestTerminalSet(channel));
    }

    let mut by_request = BTreeMap::new();
    for terminal in terminals {
        let case_id = bindings
            .case_for_request(&terminal.request_id)
            .ok_or_else(|| CollectorError::UnknownRequest(channel, terminal.request_id.clone()))?;
        let expected_stream = match channel {
            ObservationChannel::ExecutionFact => bindings.fact_stream_id(case_id),
            ObservationChannel::ConversationContent => bindings.content_stream_id(case_id),
            ObservationChannel::CanonicalLedger | ObservationChannel::Otel => None,
        };
        if terminal.stream_id != expected_stream {
            return Err(CollectorError::RequestTerminalStream(
                channel,
                terminal.request_id.clone(),
            ));
        }
        by_request.insert(terminal.request_id.clone(), serde_json::to_value(terminal)?);
    }
    Ok(by_request)
}

fn insert_channel(
    scenario: &ScenarioDocument,
    channel: ObservationChannel,
    records: Vec<Value>,
    terminals: &BTreeMap<String, Value>,
    bindings: &RuntimeBindings,
    evidence: &mut EvidenceSet,
) {
    for case in &scenario.cases {
        let request_id = bindings
            .request_id(&case.id)
            .expect("every validated scenario case has a runtime request ID");
        let case_records: Vec<_> = records
            .iter()
            .filter(|record| record["request_id"].as_str() == Some(request_id))
            .cloned()
            .collect();
        let source = channel.evidence_source();
        let terminal = terminals
            .get(request_id)
            .expect("validated receipt has every runtime request");
        let value = match channel {
            ObservationChannel::CanonicalLedger => {
                json!({"events": case_records, "terminal": terminal})
            }
            ObservationChannel::Otel => json!({"records": case_records, "terminal": terminal}),
            ObservationChannel::ExecutionFact | ObservationChannel::ConversationContent => {
                json!({
                    "schema_version": CHANNEL_SNAPSHOT_SCHEMA,
                    "channel": channel,
                    "records": case_records,
                    "terminal": terminal
                })
            }
        };
        evidence.insert(&case.id, source, value);
    }
}

#[derive(Clone, Copy, Debug)]
struct UnreceiptedArtifactFacts {
    artifact_count: usize,
    nonempty_artifact_count: usize,
    unexpected_artifact_count: usize,
}

enum EvidenceDirectoryState {
    Empty,
    Artifacts(UnreceiptedArtifactFacts),
}

fn audit_unreceipted_directory(
    evidence_dir: &Path,
) -> Result<EvidenceDirectoryState, CollectorError> {
    let known_channels = BTreeSet::from([
        "semantic-ledger.jsonl",
        "execution-facts.jsonl",
        "conversation-content.jsonl",
        "otel.jsonl",
    ]);
    let mut artifact_count = 0;
    let mut nonempty_artifact_count = 0;
    let mut unexpected_artifact_count = 0;
    for entry in fs::read_dir(evidence_dir)? {
        let entry = entry?;
        artifact_count += 1;
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let known_regular = name
            .to_str()
            .is_some_and(|name| known_channels.contains(name))
            && file_type.is_file();
        if !known_regular {
            unexpected_artifact_count += 1;
        }
        if file_type.is_file() && entry.metadata()?.len() != 0 {
            nonempty_artifact_count += 1;
        }
    }
    if artifact_count == 0 {
        Ok(EvidenceDirectoryState::Empty)
    } else {
        Ok(EvidenceDirectoryState::Artifacts(
            UnreceiptedArtifactFacts {
                artifact_count,
                nonempty_artifact_count,
                unexpected_artifact_count,
            },
        ))
    }
}

fn verify_complete_artifact_set(evidence_dir: &Path) -> Result<(), CollectorError> {
    let expected = BTreeSet::from([
        "terminal-receipt.json".to_owned(),
        "semantic-ledger.jsonl".to_owned(),
        "execution-facts.jsonl".to_owned(),
        "conversation-content.jsonl".to_owned(),
        "otel.jsonl".to_owned(),
    ]);
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(evidence_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Err(CollectorError::ArtifactSet);
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| CollectorError::ArtifactSet)?;
        actual.insert(name);
    }
    if actual == expected {
        Ok(())
    } else {
        Err(CollectorError::ArtifactSet)
    }
}

fn insert_unreceipted(
    scenario: &ScenarioDocument,
    facts: UnreceiptedArtifactFacts,
    evidence: &mut EvidenceSet,
) {
    let fault = json!({
        "state": "unreceipted_artifacts",
        "artifact_count": facts.artifact_count,
        "nonempty_artifact_count": facts.nonempty_artifact_count,
        "unexpected_artifact_count": facts.unexpected_artifact_count
    });
    for case in &scenario.cases {
        evidence.insert(
            &case.id,
            EvidenceSource::CanonicalLedger,
            json!({"events": [], "observation_fault": fault}),
        );
        evidence.insert(
            &case.id,
            EvidenceSource::Otel,
            json!({"records": [], "observation_fault": fault}),
        );
        for (source, channel) in [
            (
                EvidenceSource::ExecutionFact,
                ObservationChannel::ExecutionFact,
            ),
            (
                EvidenceSource::ConversationContent,
                ObservationChannel::ConversationContent,
            ),
        ] {
            evidence.insert(
                &case.id,
                source,
                json!({
                    "schema_version": CHANNEL_SNAPSHOT_SCHEMA,
                    "channel": channel,
                    "records": [],
                    "terminal": {
                        "state": "unreceipted_artifacts",
                        "ack": null,
                        "nack": null,
                        "gap": null,
                        "artifact_count": facts.artifact_count,
                        "nonempty_artifact_count": facts.nonempty_artifact_count,
                        "unexpected_artifact_count": facts.unexpected_artifact_count
                    }
                }),
            );
        }
    }
}

fn insert_not_started(scenario: &ScenarioDocument, evidence: &mut EvidenceSet) {
    for case in &scenario.cases {
        evidence.insert(
            &case.id,
            EvidenceSource::CanonicalLedger,
            json!({"events": []}),
        );
        evidence.insert(&case.id, EvidenceSource::Otel, json!({"records": []}));
        for (source, channel) in [
            (
                EvidenceSource::ExecutionFact,
                ObservationChannel::ExecutionFact,
            ),
            (
                EvidenceSource::ConversationContent,
                ObservationChannel::ConversationContent,
            ),
        ] {
            evidence.insert(
                &case.id,
                source,
                json!({
                    "schema_version": CHANNEL_SNAPSHOT_SCHEMA,
                    "channel": channel,
                    "records": [],
                    "terminal": {
                        "state": "not_started",
                        "ack": null,
                        "nack": null,
                        "gap": null
                    }
                }),
            );
        }
    }
}

fn validate_records(
    bundle: &P0Bundle,
    kind: ArtifactKind,
    records: &[Value],
) -> Result<(), CollectorError> {
    for record in records {
        validate_artifact_schema(bundle, kind, record)?;
    }
    Ok(())
}

fn validate_artifact_schema(
    bundle: &P0Bundle,
    kind: ArtifactKind,
    value: &Value,
) -> Result<(), CollectorError> {
    let path = bundle
        .artifacts()
        .artifact_paths
        .get(&kind)
        .ok_or(CollectorError::MissingSchema(kind))?;
    let schema: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    validate_document(&schema, value).map_err(|detail| CollectorError::Schema(kind, detail))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CollectorError {
    #[error("cannot read an evidence artifact: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot decode an evidence artifact: {0}")]
    Json(#[from] serde_json::Error),
    #[error("evidence receipt has the wrong producer identity, nonce, or PID")]
    WrongProducer,
    #[error("evidence receipt does not contain each exact channel once")]
    ChannelSet,
    #[error("evidence directory does not contain the exact receipt/channel artifact set")]
    ArtifactSet,
    #[error("{0:?} receipt terminals do not name every runtime request exactly once")]
    RequestTerminalSet(ObservationChannel),
    #[error("{0:?} receipt terminal has the wrong stream binding for request {1}")]
    RequestTerminalStream(ObservationChannel, String),
    #[error("evidence receipt size/digest mismatch for {0:?}")]
    ReceiptMismatch(ObservationChannel),
    #[error("evidence channel {0:?} is not UTF-8")]
    Utf8(ObservationChannel),
    #[error("invalid {channel:?} JSON at line {line}: {source}")]
    JsonLine {
        channel: ObservationChannel,
        line: usize,
        source: serde_json::Error,
    },
    #[error("{0:?} line {1} has no request_id")]
    MissingRequestId(ObservationChannel, usize),
    #[error("{0:?} names unknown runtime request {1}")]
    UnknownRequest(ObservationChannel, String),
    #[error("manifest is missing frozen schema {0:?}")]
    MissingSchema(ArtifactKind),
    #[error("frozen schema {0:?} rejected evidence: {1}")]
    Schema(ArtifactKind, String),
    #[error("observation record does not use frozen envelope {0}")]
    RecordVersion(&'static str),
}
