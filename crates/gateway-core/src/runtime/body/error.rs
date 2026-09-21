use super::*;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum BodyError {
    #[error("compiled body plan limits must be non-zero")]
    InvalidPlanLimit,
    #[error("budget limits must be non-zero and hierarchical")]
    InvalidBudgetLimit,
    #[error("body budget exceeded")]
    BudgetExceeded,
    #[error("unknown stream budget")]
    UnknownStreamBudget,
    #[error("reservation was already released")]
    ReservationReleased,
    #[error("body limit exceeded")]
    BodyLimitExceeded,
    #[error("body data arrived after end-of-stream")]
    BodyAfterEos,
    #[error("body end-of-stream was observed more than once")]
    DuplicateBodyEos,
    #[error("PassThrough cannot create a raw body store")]
    PassThroughCannotStore,
    #[error("body allocation has the wrong semantic memory role")]
    WrongMemoryRole,
    #[error("request body lease cannot be acquired after terminal disposition")]
    LeaseAfterTerminal,
    #[error("request body lease counter overflow")]
    LeaseOverflow,
    #[error("{0} request body leases are still outstanding")]
    OutstandingRequestLease(usize),
    #[error("request is already terminal")]
    AlreadyTerminal,
    #[error("IR release proof was already issued")]
    ReleaseProofAlreadyIssued,
    #[error("framing is already committed")]
    FramingAlreadyCommitted,
    #[error("Content-Length and Transfer-Encoding conflict")]
    ContentLengthTransferEncodingConflict,
    #[error("computed Content-Length is invalid")]
    InvalidContentLength,
    #[error("observed body length does not match Content-Length")]
    ContentLengthMismatch,
}
