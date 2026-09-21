use super::*;

#[derive(Debug)]
pub struct AcceptedBodyFrame {
    pub output: Option<EncodedOutputUnit>,
    pub end_stream: bool,
    /// Exact SSE source identities for filter transformations. Every 1:N
    /// output retains its source and an explicit merge retains the complete
    /// source set; ordinary body/terminal frames use an empty set.
    pub sse_sources: SseTransformSources,
    pub queue_metadata: BodyMetadataOwner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SseTransformSource {
    pub sequence: u64,
    pub source_bytes: usize,
    pub provenance: SemanticProvenance,
}

#[derive(Debug)]
pub struct ChargedSseTransformSources {
    sources: Box<[SseTransformSource]>,
    _reservation: Reservation,
}

#[derive(Clone, Debug, Default)]
pub enum SseTransformSources {
    #[default]
    None,
    One(SseTransformSource),
    Many(Arc<ChargedSseTransformSources>),
}

impl SseTransformSources {
    pub fn one(source: SseTransformSource) -> Self {
        Self::One(source)
    }

    pub fn as_slice(&self) -> &[SseTransformSource] {
        match self {
            Self::None => &[],
            Self::One(source) => std::slice::from_ref(source),
            Self::Many(sources) => &sources.sources,
        }
    }

    pub(super) fn merge<'a>(
        budget: &StreamBudget,
        sets: impl Clone + Iterator<Item = &'a Self>,
    ) -> Result<Self, Arc<str>> {
        let max_sources = sets
            .clone()
            .try_fold(0_usize, |total, set| {
                total.checked_add(set.as_slice().len())
            })
            .ok_or_else(|| Arc::from("SSE source-set metadata overflow"))?;
        if max_sources == 0 {
            return Ok(Self::None);
        }
        let metadata_bytes = max_sources
            .checked_mul(std::mem::size_of::<SseTransformSource>())
            .ok_or_else(|| Arc::from("SSE source-set metadata overflow"))?;
        let reservation = budget
            .reserve(MemoryRole::OutputQueue, metadata_bytes)
            .map_err(body_filter_error)?;
        let mut sources = Vec::with_capacity(max_sources);
        for source in sets.flat_map(Self::as_slice) {
            if let Some(existing) = sources
                .iter()
                .find(|existing: &&SseTransformSource| existing.sequence == source.sequence)
            {
                if *existing != *source {
                    return Err(Arc::from(
                        "SSE source sequence metadata changed during merge",
                    ));
                }
            } else {
                sources.push(*source);
            }
        }
        match sources.as_slice() {
            [] => Ok(Self::None),
            [source] => Ok(Self::One(*source)),
            _ => Ok(Self::Many(Arc::new(ChargedSseTransformSources {
                sources: sources.into_boxed_slice(),
                _reservation: reservation,
            }))),
        }
    }
}

impl PartialEq for SseTransformSources {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for SseTransformSources {}

#[derive(Debug)]
pub struct LogicalRequestBodyFrame {
    pub bytes: Option<ChargedBytes>,
    pub end_stream: bool,
    pub queue_metadata: BodyMetadataOwner,
}

/// Linear attempt-request encoder input/output. Units always retain the
/// `AttemptWire` reservation created by #15; filters may only replace them
/// through the request-local body emitter owned by core.
#[derive(Debug)]
pub struct AttemptRequestBodyFrame {
    pub bytes: Option<ChargedBytes>,
    pub end_stream: bool,
    pub queue_metadata: BodyMetadataOwner,
}

#[derive(Clone)]
pub struct GatewayFilterScopeContext {
    pub invocation: FilterInvocationContext,
    pub deadline: Instant,
    pub cancellation: CancellationToken,
    pub telemetry: Option<RequestTelemetry>,
    /// The request-local hierarchical budget used to turn callback-owned raw
    /// output into charged linear units before any provider/downstream handoff.
    pub budget: StreamBudget,
    pub executors: FilterExecutorServices,
    /// Accepted-response transforms need a scope-lifetime source ledger. The
    /// plan is present only for that direction; other filter scopes leave it
    /// unset and cannot accidentally apply accepted SSE policy.
    pub accepted_body_plan: Option<BodyPlan>,
}

impl GatewayFilterScopeContext {
    pub(super) fn with_accepted_body_plan(mut self, plan: &BodyPlan) -> Self {
        self.accepted_body_plan = Some(plan.clone());
        self
    }
}

/// Linear output of one production filter-machine step. A paused step retains
/// every input frame in the request-local owner; only `frames` may cross into
/// provider classification or the downstream final writer.
#[derive(Debug)]
pub struct BodyEmitterOutcome<T> {
    units: VecDeque<T>,
}

impl<T> BodyEmitterOutcome<T> {
    pub fn forward(unit: T) -> Self {
        Self {
            units: VecDeque::from([unit]),
        }
    }

    pub fn drop_input() -> Self {
        Self {
            units: VecDeque::new(),
        }
    }

    pub fn replace(units: impl IntoIterator<Item = T>) -> Self {
        Self {
            units: units.into_iter().collect(),
        }
    }

    pub fn into_units(self) -> VecDeque<T> {
        self.units
    }
}

impl<T> std::ops::Deref for BodyEmitterOutcome<T> {
    type Target = VecDeque<T>;

    fn deref(&self) -> &Self::Target {
        &self.units
    }
}

impl<T> std::ops::DerefMut for BodyEmitterOutcome<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.units
    }
}

impl<T> IntoIterator for BodyEmitterOutcome<T> {
    type Item = T;
    type IntoIter = std::collections::vec_deque::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.units.into_iter()
    }
}

#[derive(Debug)]
pub struct GatewayFilterResult<T> {
    pub frames: BodyEmitterOutcome<T>,
    pub local_reply: Option<LocalReply>,
    pub pause: Option<FilterPause>,
}

impl<T> GatewayFilterResult<T> {
    pub fn forward(frame: T) -> Self {
        Self {
            frames: BodyEmitterOutcome::forward(frame),
            local_reply: None,
            pause: None,
        }
    }

    pub fn headers(local_reply: Option<LocalReply>, pause: Option<FilterPause>) -> Self {
        Self {
            frames: BodyEmitterOutcome::drop_input(),
            local_reply,
            pause,
        }
    }
}

/// One non-cloneable request owner. Implementations create fresh logical,
/// attempt and accepted-response filter instances from the compiled chain;
/// no filter object is shared between requests or semantic attempts.
#[async_trait]
pub trait GatewayRequestFilterPort: Send {
    async fn begin_logical_request(
        &mut self,
        descriptors: &[CompiledFilterDescriptor],
        context: GatewayFilterScopeContext,
        head: &mut GatewayRequestHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>>;

    async fn filter_logical_request_body(
        &mut self,
        head: &mut GatewayRequestHead,
        frame: LogicalRequestBodyFrame,
        configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<LogicalRequestBodyFrame>, Arc<str>>;

    fn begin_attempt(
        &mut self,
        request_descriptors: &[CompiledFilterDescriptor],
        response_descriptors: &[CompiledFilterDescriptor],
        context: GatewayFilterScopeContext,
    ) -> Result<(), Arc<str>>;

    async fn filter_attempt_request_head(
        &mut self,
        head: &mut PreparedRequestHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>>;

    async fn filter_attempt_request_body(
        &mut self,
        head: &mut PreparedRequestHead,
        frame: AttemptRequestBodyFrame,
        configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<AttemptRequestBodyFrame>, Arc<str>>;

    async fn filter_attempt_response_event(
        &mut self,
        event: PrecommitEvent,
        configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<PrecommitEvent>, Arc<str>>;

    fn finish_attempt(&mut self);

    /// Cancels and boundedly joins only the AttemptRequest and
    /// ResponsePrecommit callback children. LogicalRequest remains live for a
    /// possible fallback and AcceptedResponse has not started yet.
    async fn finish_attempt_bounded(&mut self, join_timeout: Duration) -> Result<(), Arc<str>>;

    fn begin_accepted_response(
        &mut self,
        descriptors: &[CompiledFilterDescriptor],
        context: GatewayFilterScopeContext,
    ) -> Result<(), Arc<str>>;

    async fn filter_accepted_head(
        &mut self,
        head: &mut GatewayResponseHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>>;

    async fn filter_accepted_body(
        &mut self,
        head: &mut GatewayResponseHead,
        frame: AcceptedBodyFrame,
        configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<AcceptedBodyFrame>, Arc<str>>;

    async fn wait_logical_resume(
        &mut self,
        _head: &mut GatewayRequestHead,
    ) -> Result<GatewayFilterResult<LogicalRequestBodyFrame>, Arc<str>> {
        Err(Arc::from(
            "logical filter owner cannot resume an explicit pause",
        ))
    }

    async fn try_logical_resume(
        &mut self,
        _head: &mut GatewayRequestHead,
    ) -> Result<Option<GatewayFilterResult<LogicalRequestBodyFrame>>, Arc<str>> {
        Ok(None)
    }

    async fn wait_attempt_request_resume(
        &mut self,
        _head: &mut PreparedRequestHead,
    ) -> Result<GatewayFilterResult<AttemptRequestBodyFrame>, Arc<str>> {
        Err(Arc::from(
            "attempt request filter owner cannot resume an explicit pause",
        ))
    }

    async fn try_attempt_request_resume(
        &mut self,
        _head: &mut PreparedRequestHead,
    ) -> Result<Option<GatewayFilterResult<AttemptRequestBodyFrame>>, Arc<str>> {
        Ok(None)
    }

    async fn wait_attempt_response_resume(
        &mut self,
    ) -> Result<GatewayFilterResult<PrecommitEvent>, Arc<str>> {
        Err(Arc::from(
            "attempt filter owner cannot resume an explicit pause",
        ))
    }

    async fn try_attempt_response_resume(
        &mut self,
    ) -> Result<Option<GatewayFilterResult<PrecommitEvent>>, Arc<str>> {
        Ok(None)
    }

    async fn wait_accepted_resume(
        &mut self,
        _head: &mut GatewayResponseHead,
    ) -> Result<GatewayFilterResult<AcceptedBodyFrame>, Arc<str>> {
        Err(Arc::from(
            "accepted filter owner cannot resume an explicit pause",
        ))
    }

    async fn try_accepted_resume(
        &mut self,
        _head: &mut GatewayResponseHead,
    ) -> Result<Option<GatewayFilterResult<AcceptedBodyFrame>>, Arc<str>> {
        Ok(None)
    }

    /// Cancels and boundedly joins only the AcceptedResponse callback
    /// children. Request-wide finalization remains owned by `finalize_bounded`.
    async fn finish_accepted_response_bounded(
        &mut self,
        join_timeout: Duration,
    ) -> Result<(), Arc<str>>;

    fn finalize(&mut self);

    async fn finalize_bounded(&mut self, _join_timeout: Duration) -> Result<(), Arc<str>> {
        self.finalize();
        Ok(())
    }
}

/// Factory boundary for request-local filter ownership. #11 supplies compiled
/// descriptors; #9 resolves registered native factories and owns execution.
pub trait GatewayFilterManagerPort: Send + Sync + 'static {
    type RequestFilters: GatewayRequestFilterPort;

    fn instantiate_request(&self) -> Result<Self::RequestFilters, Arc<str>>;

    /// Signals that this manager has no callback implementation at all. The
    /// generic lifecycle may then bypass async callback scaffolding while
    /// retaining every compiled-plan, body, budget and disposition check.
    fn is_noop_fast_path(&self) -> bool {
        false
    }
}

pub(super) struct GatewayFilterRequestOwner<R: GatewayRequestFilterPort> {
    pub(super) filters: R,
    pub(super) finalized: bool,
}

impl<R: GatewayRequestFilterPort> GatewayFilterRequestOwner<R> {
    pub(super) fn new(filters: R) -> Self {
        Self {
            filters,
            finalized: false,
        }
    }
}

impl<R: GatewayRequestFilterPort> Drop for GatewayFilterRequestOwner<R> {
    fn drop(&mut self) {
        if self.finalized {
            return;
        }
        // A third-party native finalizer is observational cleanup. Its panic
        // cannot unwind through the request owner or suppress transport
        // teardown already in progress.
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| self.filters.finalize()));
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopGatewayFilterManager;

#[derive(Debug, Default)]
pub struct NoopGatewayRequestFilters;

impl GatewayFilterManagerPort for NoopGatewayFilterManager {
    type RequestFilters = NoopGatewayRequestFilters;

    fn instantiate_request(&self) -> Result<Self::RequestFilters, Arc<str>> {
        Ok(NoopGatewayRequestFilters)
    }

    fn is_noop_fast_path(&self) -> bool {
        true
    }
}

#[async_trait]
impl GatewayRequestFilterPort for NoopGatewayRequestFilters {
    async fn begin_logical_request(
        &mut self,
        _descriptors: &[CompiledFilterDescriptor],
        _context: GatewayFilterScopeContext,
        _head: &mut GatewayRequestHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>> {
        Ok(GatewayFilterResult::headers(None, None))
    }

    async fn filter_logical_request_body(
        &mut self,
        _head: &mut GatewayRequestHead,
        frame: LogicalRequestBodyFrame,
        _configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<LogicalRequestBodyFrame>, Arc<str>> {
        Ok(GatewayFilterResult::forward(frame))
    }

    fn begin_attempt(
        &mut self,
        _request_descriptors: &[CompiledFilterDescriptor],
        _response_descriptors: &[CompiledFilterDescriptor],
        _context: GatewayFilterScopeContext,
    ) -> Result<(), Arc<str>> {
        Ok(())
    }

    async fn filter_attempt_request_head(
        &mut self,
        _head: &mut PreparedRequestHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>> {
        Ok(GatewayFilterResult::headers(None, None))
    }

    async fn filter_attempt_request_body(
        &mut self,
        _head: &mut PreparedRequestHead,
        frame: AttemptRequestBodyFrame,
        _configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<AttemptRequestBodyFrame>, Arc<str>> {
        Ok(GatewayFilterResult::forward(frame))
    }

    async fn filter_attempt_response_event(
        &mut self,
        event: PrecommitEvent,
        _configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<PrecommitEvent>, Arc<str>> {
        Ok(GatewayFilterResult::forward(event))
    }

    fn finish_attempt(&mut self) {}

    async fn finish_attempt_bounded(&mut self, _join_timeout: Duration) -> Result<(), Arc<str>> {
        Ok(())
    }

    fn begin_accepted_response(
        &mut self,
        _descriptors: &[CompiledFilterDescriptor],
        _context: GatewayFilterScopeContext,
    ) -> Result<(), Arc<str>> {
        Ok(())
    }

    async fn filter_accepted_head(
        &mut self,
        _head: &mut GatewayResponseHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>> {
        Ok(GatewayFilterResult::headers(None, None))
    }

    async fn filter_accepted_body(
        &mut self,
        _head: &mut GatewayResponseHead,
        frame: AcceptedBodyFrame,
        _configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<AcceptedBodyFrame>, Arc<str>> {
        Ok(GatewayFilterResult::forward(frame))
    }

    async fn finish_accepted_response_bounded(
        &mut self,
        _join_timeout: Duration,
    ) -> Result<(), Arc<str>> {
        Ok(())
    }

    fn finalize(&mut self) {}
}

pub(super) fn append_attempt_request_filter_frames(
    output: &mut ChargedBodyQueue,
    saw_eos: &mut bool,
    frames: impl IntoIterator<Item = AttemptRequestBodyFrame>,
) -> Result<(), GatewayExecutionError> {
    for frame in frames {
        if *saw_eos {
            return Err(GatewayExecutionError::Filter(Arc::from(
                "attempt request filter emitted data after EOS",
            )));
        }
        if let Some(bytes) = frame.bytes {
            output.push_back(bytes)?;
        }
        *saw_eos = frame.end_stream;
    }
    Ok(())
}

pub(super) fn body_filter_error(error: BodyError) -> Arc<str> {
    Arc::from(format!("filter body emitter admission failed: {error}"))
}
