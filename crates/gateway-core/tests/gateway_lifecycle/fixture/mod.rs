pub(crate) use std::collections::{HashMap, VecDeque};
pub(crate) use std::error::Error;
pub(crate) use std::io;
pub(crate) use std::net::TcpListener as StdTcpListener;
pub(crate) use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
pub(crate) use std::sync::{Arc, Condvar, Mutex, Weak};
pub(crate) use std::time::{Duration, Instant};

pub(crate) use async_trait::async_trait;
pub(crate) use bytes::Bytes;
pub(crate) use futures::future::poll_fn;
pub(crate) use h2::{client, server};
pub(crate) use hiroute_gateway_core::core::execution_plan::{
    AtomicityGroupId, AttemptTimeouts, CaPolicy, ConfigBindingPolicy, ConfigBundle,
    ConfigCellDescriptor, ConfigCellGroup, ConfigCellHandle, ConfigCellId, ConfigGeneration,
    ConfigRevision, CredentialRef, ImmutableConfig, PlanRevision, ResolvedTargetBindingId,
    TransportScheme,
};
pub(crate) use hiroute_gateway_core::core::filter::{
    CompiledFilterDescriptor, DataAction, DataInput, FilterBodyEmission, FilterCapabilities,
    FilterConfigDependency, FilterConfigSnapshot, FilterContinuation, FilterError,
    FilterInvocationContext, HeaderInput, HeaderPatch, HeadersAction, LocalReply, NativeFilter,
    NativeFilterFactory, PromotedBody, ResumeAction, RetentionMode, TrailersAction, TrailersInput,
};
pub(crate) use hiroute_gateway_core::core::publication::{
    InstallError, PrepareOutcome, PublicationInstaller,
};
pub(crate) use hiroute_gateway_core::runtime::attempt::{
    AcceptBlockedReason, AttemptGeneration, AttemptTimeoutKind, ChargedResponseHead, CommitFence,
    Disposition, PrecommitEvent, PreparedAttemptBody, PreparedAttemptHttpRequest,
    PreparedRequestHead, PublishedDisposition,
};
pub(crate) use hiroute_gateway_core::runtime::body::{
    BodyDirection, BodyMetadataOwner, BodyPlan, ChargedBodyQueue, ChargedBytes, MemoryRole,
};
pub(crate) use hiroute_gateway_core::runtime::driver::{
    AcceptedBodyFrame, AttemptBudgetGrant, AttemptCleanupOutcome, AttemptDownstreamOutcome,
    AttemptFailureClass, AttemptMaterializationContext, AttemptMaterializationFailure,
    AttemptMaterializationFailureClass, AttemptRequestBodyFrame, AttemptStreamOutcome,
    AttemptTerminationReason, BodyEmitterOutcome, ClassifiedAttemptResult,
    CompletedAttemptObservation, DecisionSessionPort, DecisionSessionRequest, DecodedSseToken,
    FactConfidence, FactScope, FactSubject, FreshRoutingFact, GatewayCoreLifecycle,
    GatewayCoreLifecycleLimits, GatewayExecutionError, GatewayFilterManagerPort,
    GatewayFilterResult, GatewayFilterScopeContext, GatewayRequestFilterPort,
    LogicalRequestBodyFrame, LogicalRequestContext, NativeGatewayFilterManager,
    NormalizedAttemptLocalReply, ObservationLabel, PrecommitClassification, ProviderAcceptedEvent,
    ProviderClassificationFacts, ProviderRuntimePort, RealtimeRoutingFact, RealtimeRoutingFacts,
    RetryabilityFact, RouteDecisionId, RoutingFactState, RoutingFactsSnapshotId,
    SelectedGatewayAttempt, SelectionPublicationPort, SelectionRequest, SseTransformSources,
    UpstreamSideEffectSnapshot, UsageDimension, UsageFact,
};
pub(crate) use hiroute_gateway_core::runtime::executor::ExecutorKind;
pub(crate) use hiroute_gateway_core::runtime::sse::{EncodedOutputUnit, SemanticProvenance};
pub(crate) use hiroute_gateway_core::runtime::telemetry::{
    BodyFact, CleanupFact, CleanupKind, ConfigAcquireScope, ConfigLeaseFact, ConfigLeaseStage,
    DispositionFact, DispositionStage, ErrorClass, FenceKind, LifecycleEvent, LifecycleKind,
    MemoryFact, ObservationError, ObservationSink, PublicationFact, PublicationStage, ReleaseFact,
    ReleasePoint, ResponseFact, ScopeFact, ScopePhase, Telemetry,
};
pub(crate) use hiroute_gateway_core::test_support::{
    BootstrapBodyPlans, BootstrapPublicationBuilder, plain_target,
};
pub(crate) use hiroute_gateway_core::transport::pingora::{
    GatewayHttpApp, PingoraConnectorAdapter,
};
pub(crate) use hiroute_gateway_core::transport::{
    GatewayLifecycle, GatewayRequestHead, GatewayResponseHead, GatewaySession, HttpProtocol,
    SessionReuse, TransportError,
};
pub(crate) use http::header::{CONTENT_LENGTH, HOST};
pub(crate) use http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
pub(crate) use pingora_core::server::ShutdownWatch;
pub(crate) use pingora_core::services::Service as ServiceContract;
pub(crate) use pingora_core::services::listening::Service as ListeningService;
pub(crate) use rcgen::generate_simple_self_signed;
pub(crate) use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub(crate) use tokio::net::TcpListener;
pub(crate) use tokio::sync::{Notify, oneshot, watch};
pub(crate) use tokio::task::JoinHandle;
pub(crate) use tokio_rustls::TlsAcceptor;
pub(crate) use tokio_rustls::rustls::ServerConfig;
pub(crate) use tokio_rustls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer,
};
pub(crate) use tokio_util::sync::CancellationToken;

pub(crate) type TestError = Box<dyn Error + Send + Sync>;

mod filters;
mod network;
mod provider;
mod publication;
mod selection;
mod session;

pub(crate) use filters::*;
pub(crate) use network::*;
pub(crate) use provider::*;
pub(crate) use publication::*;
pub(crate) use selection::*;
pub(crate) use session::*;
