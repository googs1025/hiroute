use super::*;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PlanError {
    #[error("authority id must not be empty")]
    EmptyAuthority,
    #[error("stable target key must not be empty")]
    EmptyStableTargetKey,
    #[error("adapter id must not be empty")]
    EmptyAdapterId,
    #[error("credential reference must not be empty")]
    EmptyCredentialRef,
    #[error("transport authority must not be empty")]
    EmptyTransportAuthority,
    #[error("transport target requires at least one address")]
    NoTransportAddress,
    #[error("transport address state and sealed socket addresses disagree")]
    TransportAddressStateMismatch,
    #[error("transport connect timeout must be non-zero")]
    ZeroTransportTimeout,
    #[error("transport codec and H2 flow-control limits are invalid")]
    InvalidTransportFlowControl,
    #[error("HTTPS transport requires non-empty SNI")]
    MissingTlsSni,
    #[error("plain HTTP transport cannot carry TLS options")]
    TlsOptionsOnPlainHttp,
    #[error("custom CA bundle must not be empty")]
    EmptyCaBundle,
    #[error("transport ALPN set must not be empty")]
    EmptyAlpnSet,
    #[error("transport ALPN protocol is unsupported: {0}")]
    UnsupportedAlpn(Arc<str>),
    #[error("transport ALPN protocol is duplicated: {0}")]
    DuplicateAlpn(Arc<str>),
    #[error("transport reuse class and pool epoch must be non-zero")]
    InvalidTransportReuseMetadata,
    #[error("transport connection fingerprint is missing or does not match connection metadata")]
    InvalidConnectionFingerprint,
    #[error("managed loopback transport policy requires an exact numeric loopback HTTP target")]
    InvalidManagedLoopbackTarget,
    #[error("compiled attempt requires at least one credential reference")]
    EmptyCredentialClosure,
    #[error("attempt request-write, first-byte, and stream-idle timeouts must be non-zero")]
    InvalidAttemptTimeouts,
    #[error("overall request timeout must be non-zero")]
    InvalidRequestTimeout,
    #[error("config atomicity group mismatch")]
    AtomicityGroupMismatch,
    #[error("config atomicity group {0} is backed by multiple live bundle pointers")]
    SplitConfigAtomicityGroup(AtomicityGroupId),
    #[error("config atomicity group must contain at least one descriptor")]
    EmptyConfigAtomicityGroup,
    #[error("config atomicity group contains duplicate cell {0}")]
    DuplicateConfigCell(ConfigCellId),
    #[error("config bundle is missing cell {0}")]
    MissingConfigCell(ConfigCellId),
    #[error("config bundle contains unknown cell {0}")]
    UnknownConfigCell(ConfigCellId),
    #[error("config compatibility mismatch for cell {0}")]
    CompatibilityMismatch(ConfigCellId),
    #[error("config atomicity group contains values from multiple generations")]
    MixedConfigGeneration,
    #[error("config generation {0} was reused with different immutable values")]
    ConfigGenerationConflict(ConfigGeneration),
    #[error("config generation regressed from active {active} to candidate {candidate}")]
    StaleConfigGeneration {
        active: ConfigGeneration,
        candidate: ConfigGeneration,
    },
    #[error("config cell {id} requires {actual:?}, not requested {expected:?} scope")]
    ConfigBindingPolicyMismatch {
        id: ConfigCellId,
        expected: ConfigBindingPolicy,
        actual: ConfigBindingPolicy,
    },
    #[error("binding belongs to plan {actual}, expected {expected}")]
    CrossRevisionBinding {
        expected: PlanRevision,
        actual: PlanRevision,
    },
    #[error("unknown target binding {0:?}")]
    UnknownBinding(ResolvedTargetBindingId),
    #[error("route candidate closure is empty, duplicated, cross-revision, or misses its primary")]
    InvalidRouteCandidateClosure,
    #[error("target binding {0:?} is outside the matched route candidate closure")]
    BindingOutsideRouteClosure(ResolvedTargetBindingId),
    #[error("request route closure was already bound")]
    RouteAlreadyBound,
    #[error("compiled attempt contains an invalid directional body plan")]
    InvalidBodyPlan,
    #[error("compiled body queue capacities must be non-zero")]
    ZeroBodyQueueCapacity,
    #[error("request execution segment was already released: {0}")]
    ExecutionSegmentReleased(&'static str),
}

/// Request headers are represented independently of Pingora so concrete
/// transport types never leak into core contracts.
#[derive(Clone, Debug, Default)]
pub struct RequestHead {
    pub headers: HeaderMap,
    pub path_and_query: Arc<str>,
}
