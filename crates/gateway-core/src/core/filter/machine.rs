use super::*;

type NativeCallbackResult<T> =
    Result<Result<Result<T, FilterError>, Box<dyn Any + Send>>, ExecutorError>;

mod callbacks;
mod constructor;
mod lifecycle;
mod resume;

struct FilterSlot {
    filter: Box<dyn NativeFilter>,
    headers_called: bool,
    headers_forwarded: bool,
    end_stream_seen: bool,
    finalized: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PauseState {
    filter_index: usize,
    frame_kind: FrameKind,
    epoch: u64,
    header_mode: Option<HeaderStopMode>,
    retention: Option<RetentionMode>,
    paused_at: Instant,
}

#[derive(Debug)]
enum PendingFrame {
    Data {
        backing: PendingBodyBacking,
        end_stream: bool,
        cursor: usize,
        stop_after: Option<usize>,
        runtime_owner: Option<FilterBodyRuntimeOwner>,
        sources: FilterBodySourceSet,
        queue_metadata: BodyMetadataOwner,
    },
    /// A `NoBuffer` data pause drops the payload immediately, but its
    /// continuation still has to preserve any outer HeaderIteration cursor.
    /// Resuming this marker advances headers without fabricating a data frame.
    DroppedDataControl {
        cursor: usize,
        stop_after: Option<usize>,
    },
    /// Dropping the terminal payload must not drop the stream terminator.
    /// This marker retains only source identity and emits one zero-byte EOS
    /// from the exact downstream cursor after the continuation is resumed.
    DroppedEndStreamControl {
        cursor: usize,
        stop_after: Option<usize>,
        sources: FilterBodySourceSet,
    },
    Trailers {
        trailers: HeaderMap,
        cursor: usize,
        stop_after: Option<usize>,
    },
}

#[derive(Debug)]
enum PendingBodyBacking {
    Retained(RetainedFrameId),
    Charged {
        retention: RetainedFrameId,
        bytes: ChargedBytes,
    },
    EndStreamControl,
}

pub struct DirectionMachine {
    stream_id: StreamId,
    scope_id: ScopeId,
    scope_kind: ScopeKind,
    direction: Direction,
    slots: Vec<FilterSlot>,
    held_headers: HeaderMap,
    headers_end_stream: bool,
    next_header: usize,
    pause_epoch: u64,
    pauses: Vec<PauseState>,
    pause_receivers: Vec<Option<oneshot::Receiver<ResumeAction>>>,
    pending: VecDeque<PendingFrame>,
    emitted_body: VecDeque<EmittedBodyFrame>,
    dropped_runtime_owners: VecDeque<FilterBodyRuntimeOwner>,
    _pending_queue_reservation: Reservation,
    _emitted_queue_reservation: Reservation,
    _dropped_queue_reservation: Reservation,
    max_pending_frames: usize,
    retention: Box<dyn BodyRetentionPort>,
    framing: Box<dyn FramingLedgerPort>,
    terminal: bool,
    finalized: bool,
    callback_panicked: bool,
    callback_scope: ChildScope,
    executor_services: FilterExecutorServices,
    body_output_role: MemoryRole,
    invocation: FilterInvocationContext,
    telemetry: Option<RequestTelemetry>,
}
