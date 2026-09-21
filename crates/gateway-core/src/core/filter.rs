use std::any::Any;
use std::collections::VecDeque;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use futures::FutureExt;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use thiserror::Error;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::core::execution_plan::{
    ConfigBindingPolicy, ConfigCellId, ConfigGeneration, ImmutableConfig,
};
use crate::runtime::body::{
    BodyMetadataOwner, BudgetTree, ChargedBytes, MemoryRole, Reservation, StreamBudget,
};
use crate::runtime::executor::{
    BoundedExecutor, ChildScope, ExecutorError, ExecutorKind, QueuedAdmission, ResumeTiming,
};
use crate::runtime::scope::{ScopeId, ScopeKind, StreamId};
use crate::runtime::sse::SemanticProvenance;
use crate::runtime::telemetry::{ErrorClass, RequestTelemetry, ScopePhase};

mod api;
mod body;
mod descriptor;
mod error;
mod executor;
mod machine;
mod routing;

pub use api::*;
pub use body::*;
pub use descriptor::*;
pub use error::*;
pub use executor::*;
pub use machine::*;
pub use routing::*;

#[allow(unused_imports)]
pub(crate) use body::{
    ChargedFilterBodySources, EmittedBodyBacking, EmittedBodyFrame, FilterBodyRuntimeOwner,
    FilterBodySourceId, FilterBodySourceSet, FilterBodySourceWeak,
};
use body::{FilterBodyOutputData, FilterBodyOwnership};
use executor::filter_executor_error;
