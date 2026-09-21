use super::*;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MetricFamily {
    PublicationApply,
    MemoryLive,
    MemoryPeak,
    MemoryRejected,
    ExecutorOutcome,
    DispositionStage,
    SinkFailure,
    ConfigLeaseAcquire,
    ConfigLeaseReleaseLatencyMicros,
    BodyAdmittedBytes,
    BodyQueueHighWaterBytes,
    ResponseTtfbMicros,
    CleanupLatencyMicros,
}

/// Metric keys intentionally contain only closed enums. Revisions, target
/// keys, and request/decision/attempt IDs stay in structured events.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MetricKey {
    pub family: MetricFamily,
    pub budget_level: Option<BudgetLevel>,
    pub role: Option<MemoryRole>,
    pub executor: Option<ExecutorKind>,
    pub executor_outcome: Option<ExecutorOutcome>,
    pub publication_result: Option<PublicationResult>,
    pub disposition_stage: Option<DispositionStage>,
    pub disposition: Option<Disposition>,
}

#[derive(Debug, Default)]
pub struct MetricsCollector {
    values: Mutex<HashMap<MetricKey, u64>>,
}

impl MetricsCollector {
    pub fn observe(&self, event: &LifecycleEvent) {
        match &event.kind {
            LifecycleKind::Publication(fact) => self.add(
                MetricKey {
                    family: MetricFamily::PublicationApply,
                    budget_level: None,
                    role: None,
                    executor: None,
                    executor_outcome: None,
                    publication_result: Some(fact.result),
                    disposition_stage: None,
                    disposition: None,
                },
                1,
            ),
            LifecycleKind::Memory(fact) => {
                if fact.level == BudgetLevel::Stream {
                    // A low-cardinality metric cannot identify an individual
                    // stream, so a live gauge at this level would merely be
                    // whichever request emitted last. Preserve the structured
                    // per-stream fact and export only the max-per-stream HWM;
                    // process/worker events below are the authoritative
                    // concurrent live and rejection totals.
                    self.max(
                        memory_key(MetricFamily::MemoryPeak, fact.level, fact.role),
                        fact.peak_bytes as u64,
                    );
                } else {
                    self.set(
                        memory_key(MetricFamily::MemoryLive, fact.level, fact.role),
                        fact.live_bytes as u64,
                    );
                    self.set(
                        memory_key(MetricFamily::MemoryPeak, fact.level, fact.role),
                        fact.peak_bytes as u64,
                    );
                    self.set(
                        memory_key(MetricFamily::MemoryRejected, fact.level, fact.role),
                        fact.rejections as u64,
                    );
                }
            }
            LifecycleKind::ConfigLease(fact) => match fact.stage {
                ConfigLeaseStage::Acquired => {
                    self.add(family_key(MetricFamily::ConfigLeaseAcquire), 1)
                }
                ConfigLeaseStage::Released => self.set(
                    family_key(MetricFamily::ConfigLeaseReleaseLatencyMicros),
                    fact.release_latency_micros,
                ),
            },
            LifecycleKind::Body(fact) => {
                self.add(
                    family_key(MetricFamily::BodyAdmittedBytes),
                    fact.admitted_bytes as u64,
                );
                self.set(
                    family_key(MetricFamily::BodyQueueHighWaterBytes),
                    fact.queue_high_water_bytes as u64,
                );
            }
            LifecycleKind::Response(fact) => self.set(
                family_key(MetricFamily::ResponseTtfbMicros),
                fact.ttfb_micros,
            ),
            LifecycleKind::Cleanup(fact) => self.set(
                family_key(MetricFamily::CleanupLatencyMicros),
                fact.latency_micros,
            ),
            LifecycleKind::Executor(fact) => self.add(
                MetricKey {
                    family: MetricFamily::ExecutorOutcome,
                    budget_level: None,
                    role: None,
                    executor: Some(fact.executor),
                    executor_outcome: Some(fact.outcome),
                    publication_result: None,
                    disposition_stage: None,
                    disposition: None,
                },
                1,
            ),
            LifecycleKind::Disposition(fact) => self.add(
                MetricKey {
                    family: MetricFamily::DispositionStage,
                    budget_level: None,
                    role: None,
                    executor: None,
                    executor_outcome: None,
                    publication_result: None,
                    disposition_stage: Some(fact.stage),
                    disposition: Some(fact.disposition),
                },
                1,
            ),
            _ => {}
        }
    }

    pub fn get(&self, key: MetricKey) -> u64 {
        *lock(&self.values).get(&key).unwrap_or(&0)
    }

    pub fn keys(&self) -> Vec<MetricKey> {
        lock(&self.values).keys().copied().collect()
    }

    pub fn render_prometheus(&self) -> String {
        let values = lock(&self.values);
        let mut lines = values
            .iter()
            .map(|(key, value)| {
                format!(
                    "hiroute_gateway_core_metric{{family=\"{}\",budget_level=\"{}\",role=\"{}\",executor=\"{}\",executor_outcome=\"{}\",publication_result=\"{}\",disposition_stage=\"{}\",disposition=\"{}\"}} {}",
                    closed_label(key.family),
                    optional_closed_label(key.budget_level),
                    optional_closed_label(key.role),
                    optional_closed_label(key.executor),
                    optional_closed_label(key.executor_outcome),
                    optional_closed_label(key.publication_result),
                    optional_closed_label(key.disposition_stage),
                    optional_closed_label(key.disposition),
                    value,
                )
            })
            .collect::<Vec<_>>();
        lines.sort_unstable();
        lines.push(String::new());
        lines.join("\n")
    }

    fn add(&self, key: MetricKey, value: u64) {
        let mut values = lock(&self.values);
        *values.entry(key).or_insert(0) += value;
    }

    fn set(&self, key: MetricKey, value: u64) {
        lock(&self.values).insert(key, value);
    }

    fn max(&self, key: MetricKey, value: u64) {
        let mut values = lock(&self.values);
        let current = values.entry(key).or_insert(0);
        *current = (*current).max(value);
    }
}

fn closed_label(value: impl fmt::Debug) -> String {
    format!("{value:?}").to_ascii_lowercase()
}

fn optional_closed_label(value: Option<impl fmt::Debug>) -> String {
    value.map_or_else(|| "none".into(), closed_label)
}

pub struct StructuredLogSink<W: Write + Send> {
    writer: Mutex<W>,
}

impl<W: Write + Send> StructuredLogSink<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer: Mutex::new(writer),
        }
    }
}

impl<W: Write + Send> ObservationSink for StructuredLogSink<W> {
    fn try_emit(&self, event: LifecycleEvent) -> Result<(), ObservationError> {
        let config_generations = if event.correlation.config_generations.is_empty() {
            "none".to_owned()
        } else {
            event
                .correlation
                .config_generations
                .iter()
                .map(|(cell, generation)| format!("{}:{}", cell, generation.0))
                .collect::<Vec<_>>()
                .join(",")
        };
        let mut writer = lock(&self.writer);
        writeln!(
            writer,
            "monotonic_nanos={} authority_id={} authority_epoch={} config_revision={} plan_revision={} config_generations={} stable_target_key={} binding_local_id={} request_id={} decision_id={} attempt_id={} attempt_generation={} kind={:?}",
            event.monotonic_nanos,
            sanitize_stable_id(event.correlation.authority_id.as_str()),
            event.correlation.authority_epoch,
            event.correlation.config_revision.0,
            event.correlation.plan_revision.0,
            config_generations,
            event
                .correlation
                .stable_target_key
                .as_ref()
                .map_or_else(|| "none".into(), |key| sanitize_stable_id(key.as_str())),
            optional_u64(event.correlation.binding_local_id.map(u64::from)),
            optional_u64(event.correlation.request_id.map(|id| id.0)),
            optional_u64(event.correlation.decision_id),
            optional_u64(event.correlation.attempt_id.map(|id| id.0)),
            optional_u64(
                event
                    .correlation
                    .attempt_generation
                    .map(|generation| generation.0),
            ),
            event.kind,
        )
        .map_err(|_| ObservationError::Unavailable)
    }
}

fn optional_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "none".into(), |value| value.to_string())
}

fn sanitize_stable_id(value: &str) -> String {
    value
        .chars()
        .take(128)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

pub struct ProductionObservationSink<W: Write + Send> {
    logs: StructuredLogSink<W>,
    metrics: Arc<MetricsCollector>,
}

impl<W: Write + Send> ProductionObservationSink<W> {
    pub fn new(writer: W, metrics: Arc<MetricsCollector>) -> Self {
        Self {
            logs: StructuredLogSink::new(writer),
            metrics,
        }
    }

    pub fn metrics(&self) -> &Arc<MetricsCollector> {
        &self.metrics
    }
}

impl<W: Write + Send> ObservationSink for ProductionObservationSink<W> {
    fn try_emit(&self, event: LifecycleEvent) -> Result<(), ObservationError> {
        self.metrics.observe(&event);
        self.logs.try_emit(event)
    }
}

impl ObservationSink for MetricsCollector {
    fn try_emit(&self, event: LifecycleEvent) -> Result<(), ObservationError> {
        self.observe(&event);
        Ok(())
    }
}

fn family_key(family: MetricFamily) -> MetricKey {
    MetricKey {
        family,
        budget_level: None,
        role: None,
        executor: None,
        executor_outcome: None,
        publication_result: None,
        disposition_stage: None,
        disposition: None,
    }
}

fn memory_key(family: MetricFamily, level: BudgetLevel, role: MemoryRole) -> MetricKey {
    MetricKey {
        family,
        budget_level: Some(level),
        role: Some(role),
        executor: None,
        executor_outcome: None,
        publication_result: None,
        disposition_stage: None,
        disposition: None,
    }
}
