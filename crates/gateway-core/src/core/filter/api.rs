use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterInvocationContext {
    pub stream_id: StreamId,
    pub scope_id: ScopeId,
    pub scope_kind: ScopeKind,
    pub plan_revision: u64,
    pub binding_local_id: Option<u32>,
    pub request_id: u64,
    pub attempt_id: Option<u64>,
    pub attempt_generation: Option<u64>,
    pub configs: FilterConfigSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Direction {
    Decode,
    Encode,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FrameKind {
    Headers,
    Data,
    Trailers,
    EndStream,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionMode {
    Buffer,
    Watermark,
    NoBuffer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderStopMode {
    Iteration,
    AllBuffer,
    AllWatermark,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalReply {
    pub status: StatusCode,
    /// Trusted response headers selected by the local-response owner. Final
    /// framing still owns Content-Length and transfer semantics.
    pub headers: HeaderMap,
    pub body: Bytes,
    /// Trusted semantic classification supplied by the filter/local-response
    /// owner. The final writer never infers provenance from status or bytes.
    pub provenance: SemanticProvenance,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HeaderPatch {
    operations: Vec<HeaderOperation>,
}

impl HeaderPatch {
    pub fn insert(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.operations.push(HeaderOperation::Insert(name, value));
        self
    }

    pub fn remove(mut self, name: HeaderName) -> Self {
        self.operations.push(HeaderOperation::Remove(name));
        self
    }

    pub(super) fn apply(
        &self,
        headers: &mut HeaderMap,
        framing: &mut dyn FramingLedgerPort,
    ) -> Result<(), FilterError> {
        for operation in &self.operations {
            match operation {
                HeaderOperation::Insert(name, value) => {
                    reject_pseudo_header(name)?;
                    headers.insert(name, value.clone());
                    framing.header_mutated(name);
                }
                HeaderOperation::Remove(name) => {
                    reject_pseudo_header(name)?;
                    headers.remove(name);
                    framing.header_mutated(name);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HeaderOperation {
    Insert(HeaderName, HeaderValue),
    Remove(HeaderName),
}

fn reject_pseudo_header(name: &HeaderName) -> Result<(), FilterError> {
    if name.as_str().starts_with(':') {
        return Err(FilterError::PseudoHeaderMutation);
    }
    Ok(())
}

#[derive(Debug)]
pub struct HeaderInput {
    pub direction: Direction,
    pub context: FilterInvocationContext,
    pub headers: HeaderMap,
    pub end_stream: bool,
    pub executors: FilterExecutorServices,
    /// A single-use continuation owned by this callback invocation. A filter
    /// that returns an explicit stop action may move this value into its
    /// request-owned async work and resume the exact pause once that work
    /// completes. Dropping it leaves StopIteration resumable by body/EOS, but
    /// makes StopAll fail closed instead of being silently auto-resumed.
    pub continuation: FilterContinuation,
}

/// Callback-scoped body view. The backing cannot be cloned or moved into
/// filter state; retaining bytes beyond `on_data` requires `promote`, which
/// creates a charge-following owner first.
#[derive(Debug)]
pub struct DataInput<'a> {
    pub direction: Direction,
    pub context: FilterInvocationContext,
    pub(super) bytes: &'a [u8],
    pub end_stream: bool,
    pub executors: FilterExecutorServices,
    pub continuation: FilterContinuation,
    pub(super) output_role: MemoryRole,
    pub(super) max_output_units: usize,
    pub(super) sources: FilterBodySourceSet,
}

impl DataInput<'_> {
    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }

    /// Explicitly retains this callback input under the stream's semantic
    /// state budget. Every clone shares a drop-tracked reservation, so the
    /// charge is released only with the final promoted owner.
    pub fn promote(&self) -> Result<PromotedBody, FilterError> {
        self.executors
            .promote_body(self.bytes, self.sources.clone())
    }

    /// Creates the only supported replacement-body allocator. Queue metadata
    /// is reserved before `Vec` allocation and every emitted byte is reserved
    /// before its exact backing is copied.
    pub fn body_emitter(&self) -> Result<FilterBodyEmitter, FilterError> {
        self.executors.body_emitter(
            self.output_role,
            self.max_output_units,
            self.sources.clone(),
        )
    }
}

#[derive(Debug)]
pub struct TrailersInput {
    pub direction: Direction,
    pub context: FilterInvocationContext,
    pub trailers: HeaderMap,
    pub executors: FilterExecutorServices,
    pub continuation: FilterContinuation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeadersAction {
    Continue(HeaderPatch),
    StopIteration(HeaderPatch),
    StopAllIterationAndBuffer(HeaderPatch),
    StopAllIterationAndWatermark(HeaderPatch),
    LocalReply(LocalReply),
}

#[derive(Debug)]
pub enum DataAction {
    Continue(HeaderPatch),
    Emit {
        output: FilterBodyEmission,
        patch: HeaderPatch,
    },
    StopIteration {
        retention: RetentionMode,
        patch: HeaderPatch,
    },
    LocalReply(LocalReply),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrailersAction {
    Continue(HeaderPatch),
    StopIteration(HeaderPatch),
    LocalReply(LocalReply),
}

#[async_trait]
pub trait NativeFilter: Send {
    fn name(&self) -> &str;

    fn may_drop_body(&self) -> bool {
        self.capabilities().drops_body()
    }

    fn capabilities(&self) -> FilterCapabilities {
        FilterCapabilities::observe_only()
    }

    async fn on_headers(&mut self, input: HeaderInput) -> Result<HeadersAction, FilterError>;

    async fn on_data(&mut self, input: DataInput<'_>) -> Result<DataAction, FilterError>;

    async fn on_trailers(&mut self, input: TrailersInput) -> Result<TrailersAction, FilterError>;

    fn on_finalize(&mut self);
}

pub trait NativeFilterFactory: Send + Sync {
    fn create(
        &self,
        descriptor: &CompiledFilterDescriptor,
        context: &FilterInvocationContext,
        executors: &FilterExecutorServices,
    ) -> Result<Box<dyn NativeFilter>, FilterError>;
}

pub trait BodyRetentionPort: Send {
    fn retain(&mut self, bytes: Bytes) -> Result<RetainedFrameId, FilterError>;
    fn take(&mut self, id: RetainedFrameId) -> Result<Bytes, FilterError>;
    /// Accounts an already-charged replacement while it is paused without
    /// copying or adding a second payload reservation.
    fn retain_charged(&mut self, bytes: usize) -> Result<RetainedFrameId, FilterError>;
    fn release_charged(&mut self, id: RetainedFrameId) -> Result<(), FilterError>;
    fn discard(&mut self, id: RetainedFrameId) -> Result<(), FilterError> {
        self.take(id).map(drop)
    }
    fn set_read_paused(&mut self, paused: bool);
    fn read_paused(&self) -> bool {
        false
    }
}

pub trait FramingLedgerPort: Send {
    fn header_mutated(&mut self, name: &HeaderName);
    fn body_transformed(&mut self);
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RetainedFrameId(pub u64);

/// A non-cloneable one-shot identity. Consuming the value makes double resume
/// impossible through safe Rust; epoch validation additionally rejects stale
/// and cross-machine tokens.
#[derive(Debug)]
pub struct ContinuationToken {
    pub(super) stream_id: StreamId,
    pub(super) scope_id: ScopeId,
    pub(super) direction: Direction,
    pub(super) filter_index: usize,
    pub(super) frame_kind: FrameKind,
    pub(super) epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResumeAction {
    Continue(HeaderPatch),
    LocalReply(Box<LocalReply>),
    Fail,
}

/// Owning, single-use resumption capability for one concrete callback. The
/// `DirectionMachine` retains the matching receive half and still validates
/// its non-cloneable continuation token at the atomic resume point.
pub struct FilterContinuation {
    sender: Option<oneshot::Sender<ResumeAction>>,
}

impl FilterContinuation {
    pub(super) fn channel() -> (Self, oneshot::Receiver<ResumeAction>) {
        let (sender, receiver) = oneshot::channel();
        (
            Self {
                sender: Some(sender),
            },
            receiver,
        )
    }

    pub fn resume(mut self, action: ResumeAction) -> Result<(), ResumeAction> {
        self.sender
            .take()
            .expect("continuation sender is consumed exactly once")
            .send(action)
    }
}

impl std::fmt::Debug for FilterContinuation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FilterContinuation")
            .field("available", &self.sender.is_some())
            .finish()
    }
}

#[derive(Debug)]
pub enum MachineOutcome {
    Advanced,
    Paused(ContinuationToken),
    LocalReply(LocalReply),
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterPause {
    /// Header StopIteration: the stopped filter may still consume body/EOS and
    /// resume headers by returning Data Continue.
    HeaderIteration,
    /// Source data may be retained up to the compiled hard bound while an
    /// explicit continuation is outstanding.
    Buffer,
    /// The source read owner must stop pulling until explicit continuation.
    Watermark,
}
