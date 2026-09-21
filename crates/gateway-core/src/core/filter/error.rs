use super::*;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum FilterError {
    #[error("compiled filter descriptor is invalid")]
    InvalidDescriptor,
    #[error("compiled filter descriptor declares the same config cell more than once")]
    DuplicateConfigDependency,
    #[error("pending frame limit must be non-zero")]
    ZeroPendingLimit,
    #[error("headers were already started")]
    HeadersAlreadyStarted,
    #[error("direction machine is terminal")]
    MachineTerminal,
    #[error("pending frame hard limit exceeded")]
    PendingFrameLimit,
    #[error("retained body hard limit exceeded")]
    RetentionLimit,
    #[error("retained frame does not exist")]
    UnknownRetainedFrame,
    #[error("filter {filter} used NoBuffer without may_drop_body")]
    BodyDropNotDeclared { filter: String },
    #[error("filter {filter} emitted a body mutation outside compiled capabilities")]
    BodyMutationNotDeclared { filter: String },
    #[error("filter {filter} runtime capabilities exceed its compiled descriptor")]
    CapabilityEscalation { filter: String },
    #[error("pseudo-header mutation is not allowed")]
    PseudoHeaderMutation,
    #[error("continuation belongs to a different stream")]
    CrossStreamContinuation,
    #[error("continuation belongs to a different scope")]
    CrossScopeContinuation,
    #[error("continuation direction is wrong")]
    WrongDirectionContinuation,
    #[error("continuation is stale")]
    StaleContinuation,
    #[error("resume after terminal/finalize")]
    ResumeAfterFinalize,
    #[error("filter resume failed")]
    ResumeFailed,
    #[error("explicit filter continuation was dropped without resuming")]
    ContinuationDropped,
    #[error("native filter callback failed: {0}")]
    Callback(Arc<str>),
    #[error("native filter callback panicked")]
    CallbackPanic,
    #[error("native filter callback exceeded its request deadline")]
    CallbackDeadline,
    #[error("native filter callback was cancelled")]
    CallbackCancelled,
    #[error("gateway I/O executor is not available to native filters")]
    UnsupportedExecutorKind,
    #[error("native filter executor payload exceeded the request budget")]
    ExecutorPayloadBudget,
    #[error("native filter body output exceeded the request budget")]
    BodyOutputBudget,
    #[error("native filter body output exceeded its compiled unit limit")]
    BodyOutputUnitLimit,
    #[error("native filter executor failed: {0}")]
    Executor(Arc<str>),
}
