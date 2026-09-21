pub(crate) use std::collections::VecDeque;
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use std::time::{Duration, Instant};

pub(crate) use async_trait::async_trait;
pub(crate) use bytes::Bytes;
pub(crate) use hiroute_gateway_core::core::execution_plan::{
    AuthorityId, ConfigRevision, PlanRevision,
};
pub(crate) use hiroute_gateway_core::core::filter::{
    BodyRetentionPort, DataAction, DataInput, DirectionMachine, FilterBodyEmission,
    FilterCallbackContext, FilterCapabilities, FilterError, FilterExecutorServices, HeaderInput,
    HeaderPatch, HeadersAction, LocalReply, MachineOutcome, NativeFilter, PromotedBody,
    ResumeAction, RetainedFrameId, RoutedLocalReply, TrailersAction, TrailersInput,
    UpstreamSemanticUse, route_local_reply,
};
pub(crate) use hiroute_gateway_core::runtime::body::{BudgetTree, MemoryRole};
pub(crate) use hiroute_gateway_core::runtime::executor::{BoundedExecutor, ExecutorKind};
pub(crate) use hiroute_gateway_core::runtime::scope::{
    ScopeError, ScopeId, ScopeKind, ScopeSupervisor, StreamId,
};
pub(crate) use hiroute_gateway_core::runtime::telemetry::{
    Correlation, LifecycleEvent, LifecycleKind, ObservationError, ObservationSink,
    RequestTelemetry, ScopeFact, ScopePhase, Telemetry,
};
pub(crate) use hiroute_gateway_core::test_support::{BoundedBodyRetention, FakeFramingLedger};
pub(crate) use http::{HeaderMap, HeaderValue, StatusCode};
pub(crate) use static_assertions::assert_not_impl_any;
pub(crate) use tokio_util::sync::CancellationToken;

assert_not_impl_any!(hiroute_gateway_core::core::filter::ContinuationToken: Clone);
assert_not_impl_any!(hiroute_gateway_core::core::filter::DataInput<'static>: Clone, Copy);
assert_not_impl_any!(hiroute_gateway_core::core::execution_plan::ConfigCellGuard: Send, Sync, Clone);
assert_not_impl_any!(hiroute_gateway_core::core::execution_plan::ConfigEventSnapshot: Send, Sync, Clone);

#[derive(Debug)]
pub(crate) enum ScriptAction {
    Headers(HeadersAction),
    Data(DataAction),
    Trailers(TrailersAction),
}

pub(crate) struct ScriptFilter {
    pub(crate) name: &'static str,
    pub(crate) may_drop: bool,
    pub(crate) may_mutate: bool,
    pub(crate) script: VecDeque<ScriptAction>,
    pub(crate) trace: Arc<Mutex<Vec<String>>>,
    pub(crate) finalize_count: Arc<Mutex<usize>>,
}

impl ScriptFilter {
    pub(crate) fn new(
        name: &'static str,
        script: impl IntoIterator<Item = ScriptAction>,
        trace: Arc<Mutex<Vec<String>>>,
    ) -> (Self, Arc<Mutex<usize>>) {
        let finalize_count = Arc::new(Mutex::new(0));
        (
            Self {
                name,
                may_drop: false,
                may_mutate: false,
                script: script.into_iter().collect(),
                trace,
                finalize_count: Arc::clone(&finalize_count),
            },
            finalize_count,
        )
    }

    pub(crate) fn dropping(mut self) -> Self {
        self.may_drop = true;
        self
    }

    pub(crate) fn mutating(mut self) -> Self {
        self.may_mutate = true;
        self
    }

    pub(crate) fn next(&mut self) -> ScriptAction {
        self.script
            .pop_front()
            .unwrap_or_else(|| panic!("{} exhausted script", self.name))
    }
}

#[async_trait]
impl NativeFilter for ScriptFilter {
    fn name(&self) -> &str {
        self.name
    }

    fn may_drop_body(&self) -> bool {
        self.may_drop
    }

    fn capabilities(&self) -> FilterCapabilities {
        let mut capabilities = FilterCapabilities::observe_only();
        if self.may_drop {
            capabilities = capabilities.with_body_drop();
        }
        if self.may_mutate {
            capabilities = capabilities.with_body_mutation();
        }
        capabilities
    }

    async fn on_headers(&mut self, _: HeaderInput) -> Result<HeadersAction, FilterError> {
        self.trace.lock().unwrap().push(format!("{}:H", self.name));
        let ScriptAction::Headers(action) = self.next() else {
            panic!("expected headers action")
        };
        Ok(action)
    }

    async fn on_data(&mut self, input: DataInput<'_>) -> Result<DataAction, FilterError> {
        self.trace
            .lock()
            .unwrap()
            .push(if self.name == "eos-observer" {
                format!(
                    "{}:D:{}:{}",
                    self.name,
                    input.bytes().len(),
                    input.end_stream
                )
            } else {
                format!("{}:D", self.name)
            });
        let ScriptAction::Data(action) = self.next() else {
            panic!("expected data action")
        };
        Ok(action)
    }

    async fn on_trailers(&mut self, _: TrailersInput) -> Result<TrailersAction, FilterError> {
        self.trace.lock().unwrap().push(format!("{}:T", self.name));
        let ScriptAction::Trailers(action) = self.next() else {
            panic!("expected trailers action")
        };
        Ok(action)
    }

    fn on_finalize(&mut self) {
        *self.finalize_count.lock().unwrap() += 1;
        self.trace.lock().unwrap().push(format!("{}:F", self.name));
    }
}

pub(crate) fn machine(filters: Vec<Box<dyn NativeFilter>>) -> DirectionMachine {
    DirectionMachine::decoder(
        StreamId(1),
        ScopeId(1),
        ScopeKind::LogicalRequest,
        filters,
        8,
        Box::new(BoundedBodyRetention::new(1024)),
        Box::new(FakeFramingLedger::default()),
    )
    .unwrap()
}

#[derive(Default)]
pub(crate) struct FilterTelemetrySink {
    pub(crate) events: Mutex<Vec<LifecycleEvent>>,
}

impl ObservationSink for FilterTelemetrySink {
    fn try_emit(&self, event: LifecycleEvent) -> Result<(), ObservationError> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
}
