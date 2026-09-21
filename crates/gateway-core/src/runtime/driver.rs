use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use futures::FutureExt;
use http::header::{CONTENT_LENGTH, HOST, TRANSFER_ENCODING};
use http::{HeaderMap, Method, StatusCode};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::core::execution_plan::{
    AcceptedResponseExecutionBinding, AttemptExecutionBinding, CompiledAttemptPlan,
    CompiledIngressPlan, CompiledRoute, ConfigBindingPolicy, ConfigCellId, ConfigEventSnapshot,
    ConfigScopeSnapshot, CredentialRef, ImmutableConfig, PlanRevision, RequestConfigSnapshot,
    RequestExecutionBinding, ResolvedTargetBindingId, TransportTarget,
};
use crate::core::filter::{
    BodyRetentionPort, CompiledFilterDescriptor, DirectionMachine, EmittedBodyBacking,
    EmittedBodyFrame, FilterBodySourceId, FilterBodySourceWeak, FilterCallbackContext,
    FilterConfigSnapshot, FilterConfigValue, FilterError, FilterExecutorServices,
    FilterInvocationContext, FilterPause, FramingLedgerPort, LocalReply, MachineOutcome,
    NativeFilterFactory, RetainedFrameId,
};
use crate::core::publication::PublicationInstaller;
use crate::runtime::attempt::{
    AcceptBlockedReason, AttemptError, AttemptExchange, AttemptGeneration, AttemptId,
    AttemptTimeoutKind, AttemptTransport, AttemptTransportFactory, AttemptTransportFacts,
    CommitFence, Disposition, PrecommitEvent, PreexchangeDispositionGate,
    PreparedAttemptHttpRequest, PreparedRequestHead, PublishedDisposition, RequestId, WriterGate,
    WriterState,
};
use crate::runtime::body::{
    BodyDirection, BodyError, BodyMetadataOwner, BodyPlan, BodyPlanExecutor, BudgetTree,
    BudgetTreeSnapshot, ChargedBodyQueue, ChargedBytes, FramingLedger, HttpFraming, MemoryRole,
    RequestLeaseBook, Reservation, StreamBudget,
};
use crate::runtime::executor::{BoundedExecutor, ExecutorError, ExecutorKind};
use crate::runtime::scope::{
    ScopeError, ScopeGuard, ScopeId, ScopeKind, ScopeSupervisor, StreamId,
};
use crate::runtime::sse::{
    AcceptedEvent, AcceptedHandoff, BoundedEventEmitter, BoundedOutputSink,
    BudgetedResponseMailbox, EncodedOutputUnit, EofPolicy, PrecommitResponseState,
    SemanticProvenance, SseError, SseEventView, SseFeedOutcome, SseFramer, SseLimits, SseVisitor,
};
use crate::runtime::telemetry::{
    ConfigAcquireScope, Correlation, DispositionStage, ErrorClass, FenceKind, ReleasePoint,
    RequestTelemetry, ScopePhase, Telemetry,
};
use crate::transport::{
    GatewayLifecycle, GatewayRequestHead, GatewayResponseHead, GatewaySession, HttpProtocol,
    SessionReuse, TransportError,
};

mod decision;
mod error;
mod filter_api;
mod lifecycle;
mod native_filter;
mod operations;
mod provider;
mod request_state;
mod response;
mod sse_bridge;

pub use decision::*;
pub use error::*;
pub use filter_api::*;
pub use lifecycle::*;
pub use native_filter::*;
pub use provider::*;
pub use request_state::*;

pub(crate) use operations::path_prefix_matches;

use decision::{
    attempt_failure_facts, attempt_request_preparation_failure_facts,
    materialization_failure_facts, preexchange_transport_facts, record_completed_attempt,
    validate_realtime_routing_facts, validate_selection,
};
use filter_api::{GatewayFilterRequestOwner, append_attempt_request_filter_frames};
use lifecycle::observe_runtime_error;
use operations::{
    FilterExecutorPool, await_attempt_operation, await_preexchange_operation,
    await_request_operation, await_session_operation, filter_scope_context, match_compiled_route,
    materialize_filter_configs, request_content_length,
};
use provider::{AttemptDecision, finalize_preexchange_provider_facts, validate_provider_facts};
use response::{
    RequestFinalWriter, emit_local_response, emit_provider_terminal_response, protocol_framing,
    record_framing_mutations, response_body_forbidden, write_buffered_accepted_response,
};
use sse_bridge::{
    feed_accepted_sse, feed_precommit_sse, resume_accepted_sse, resume_precommit_sse,
    sse_limits_from_plan, validate_sse_source_totals, validate_sse_transform_batch,
};
