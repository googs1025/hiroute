use super::*;

#[derive(Clone, Debug, Default)]
pub(crate) struct TrackingFilters {
    pub(crate) logical: Arc<AtomicUsize>,
    pub(crate) attempt: Arc<AtomicUsize>,
    pub(crate) attempt_request_head: Arc<AtomicUsize>,
    pub(crate) attempt_request_body: Arc<AtomicUsize>,
    pub(crate) accepted_head: Arc<AtomicUsize>,
    pub(crate) accepted_body: Arc<AtomicUsize>,
    pub(crate) attempt_cleanup: Arc<AtomicUsize>,
    pub(crate) accepted_cleanup: Arc<AtomicUsize>,
    pub(crate) finalized: Arc<AtomicUsize>,
    pub(crate) logical_reply: Arc<Mutex<Option<LocalReply>>>,
    pub(crate) attempt_reply: Arc<Mutex<Option<LocalReply>>>,
    pub(crate) attempt_request_reply: Arc<Mutex<Option<LocalReply>>>,
    pub(crate) accepted_body_reply: Arc<Mutex<Option<LocalReply>>>,
    pub(crate) attempt_entered: Arc<Notify>,
    pub(crate) attempt_cleanup_entered: Arc<Notify>,
    pub(crate) accepted_cleanup_entered: Arc<Notify>,
    pub(crate) accepted_cleanup_release: Option<Arc<Notify>>,
    pub(crate) attempt_delay: Option<Duration>,
    pub(crate) attempt_request_eos_blocking_delay: Option<Duration>,
    pub(crate) attempt_cleanup_delay: Option<Duration>,
    pub(crate) accepted_cleanup_delay: Option<Duration>,
    pub(crate) panic_attempt: bool,
    pub(crate) rewrite_logical_header_on_body: bool,
    pub(crate) rewrite_content_length: bool,
    pub(crate) rewrite_content_length_on_body: bool,
    pub(crate) fail_accepted_head: bool,
    pub(crate) fail_attempt_cleanup: bool,
    pub(crate) fail_accepted_cleanup: bool,
    pub(crate) fail_attempt_request_head: bool,
    pub(crate) preexchange_completion_trace: Option<Arc<Mutex<Vec<PreexchangeCompletionStep>>>>,
}

impl GatewayFilterManagerPort for TrackingFilters {
    type RequestFilters = TrackingFilters;

    fn instantiate_request(&self) -> Result<Self::RequestFilters, Arc<str>> {
        Ok(self.clone())
    }
}

#[async_trait]
impl GatewayRequestFilterPort for TrackingFilters {
    async fn begin_logical_request(
        &mut self,
        _descriptors: &[CompiledFilterDescriptor],
        _context: GatewayFilterScopeContext,
        _head: &mut GatewayRequestHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>> {
        self.logical.fetch_add(1, Ordering::Relaxed);
        Ok(GatewayFilterResult::headers(
            self.logical_reply.lock().expect("logical reply").take(),
            None,
        ))
    }

    async fn filter_logical_request_body(
        &mut self,
        head: &mut GatewayRequestHead,
        frame: LogicalRequestBodyFrame,
        _configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<LogicalRequestBodyFrame>, Arc<str>> {
        if self.rewrite_logical_header_on_body {
            head.headers.insert(
                http::HeaderName::from_static("x-logical-body"),
                HeaderValue::from_static("filtered"),
            );
        }
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
        self.attempt_request_head.fetch_add(1, Ordering::Relaxed);
        if self.fail_attempt_request_head {
            return Err(Arc::from("synthetic attempt request head filter failure"));
        }
        Ok(GatewayFilterResult::headers(None, None))
    }

    async fn filter_attempt_request_body(
        &mut self,
        _head: &mut PreparedRequestHead,
        frame: AttemptRequestBodyFrame,
        _configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<AttemptRequestBodyFrame>, Arc<str>> {
        self.attempt_request_body.fetch_add(1, Ordering::Relaxed);
        if frame.end_stream
            && let Some(delay) = self.attempt_request_eos_blocking_delay
        {
            // Model a synchronous native callback that returns Ready only after
            // its attempt grant expires. The biased callback owner receives the
            // successful result; exchange construction must still re-check the
            // absolute deadline rather than relying on timer scheduling order.
            std::thread::sleep(delay);
        }
        let reply = self
            .attempt_request_reply
            .lock()
            .expect("attempt request reply")
            .take();
        Ok(if let Some(reply) = reply {
            GatewayFilterResult {
                frames: BodyEmitterOutcome::drop_input(),
                local_reply: Some(reply),
                pause: None,
            }
        } else {
            GatewayFilterResult::forward(frame)
        })
    }

    async fn filter_attempt_response_event(
        &mut self,
        event: PrecommitEvent,
        _configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<PrecommitEvent>, Arc<str>> {
        self.attempt.fetch_add(1, Ordering::Relaxed);
        self.attempt_entered.notify_one();
        if let Some(delay) = self.attempt_delay {
            tokio::time::sleep(delay).await;
        }
        if self.panic_attempt {
            panic!("request-local attempt filter panic");
        }
        let reply = self.attempt_reply.lock().expect("attempt reply").take();
        Ok(if let Some(reply) = reply {
            GatewayFilterResult {
                frames: BodyEmitterOutcome::drop_input(),
                local_reply: Some(reply),
                pause: None,
            }
        } else {
            GatewayFilterResult::forward(event)
        })
    }

    fn finish_attempt(&mut self) {}

    async fn finish_attempt_bounded(&mut self, _join_timeout: Duration) -> Result<(), Arc<str>> {
        self.attempt_cleanup.fetch_add(1, Ordering::Relaxed);
        self.attempt_cleanup_entered.notify_waiters();
        if let Some(delay) = self.attempt_cleanup_delay {
            tokio::time::sleep(delay).await;
        }
        let result = if self.fail_attempt_cleanup {
            Err(Arc::from("synthetic attempt filter cleanup failure"))
        } else {
            Ok(())
        };
        if let Some(trace) = self.preexchange_completion_trace.as_ref() {
            trace
                .lock()
                .map_err(|_| Arc::from("poisoned pre-exchange completion trace"))?
                .push(PreexchangeCompletionStep::AttemptFilterCleanup);
        }
        result
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
        head: &mut GatewayResponseHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>> {
        self.accepted_head.fetch_add(1, Ordering::Relaxed);
        if self.fail_accepted_head {
            return Err(Arc::from("synthetic accepted filter failure"));
        }
        if self.rewrite_content_length {
            head.headers
                .insert(CONTENT_LENGTH, HeaderValue::from_static("1"));
        }
        Ok(GatewayFilterResult::headers(None, None))
    }

    async fn filter_accepted_body(
        &mut self,
        head: &mut GatewayResponseHead,
        frame: AcceptedBodyFrame,
        _configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<AcceptedBodyFrame>, Arc<str>> {
        self.accepted_body.fetch_add(1, Ordering::Relaxed);
        if self.rewrite_content_length_on_body {
            head.headers
                .insert(CONTENT_LENGTH, HeaderValue::from_static("1"));
        }
        let reply = self
            .accepted_body_reply
            .lock()
            .expect("accepted body reply")
            .take();
        Ok(if let Some(reply) = reply {
            GatewayFilterResult {
                frames: BodyEmitterOutcome::drop_input(),
                local_reply: Some(reply),
                pause: None,
            }
        } else {
            GatewayFilterResult::forward(frame)
        })
    }

    async fn finish_accepted_response_bounded(
        &mut self,
        _join_timeout: Duration,
    ) -> Result<(), Arc<str>> {
        self.accepted_cleanup.fetch_add(1, Ordering::Relaxed);
        self.accepted_cleanup_entered.notify_waiters();
        if let Some(release) = self.accepted_cleanup_release.as_ref() {
            release.notified().await;
        }
        if let Some(delay) = self.accepted_cleanup_delay {
            tokio::time::sleep(delay).await;
        }
        if self.fail_accepted_cleanup {
            Err(Arc::from("synthetic accepted filter cleanup failure"))
        } else {
            Ok(())
        }
    }

    fn finalize(&mut self) {
        self.finalized.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(crate) struct NativeFilterFacts {
    pub(crate) next_instance: AtomicUsize,
    pub(crate) instances: Mutex<Vec<(usize, Arc<str>, FilterInvocationContext)>>,
    pub(crate) finalized: Mutex<HashMap<usize, usize>>,
    pub(crate) trace: Mutex<Vec<String>>,
    pub(crate) watermark_continuation: Mutex<Option<FilterContinuation>>,
    pub(crate) watermark_entered: Notify,
    pub(crate) config_updates: Mutex<Vec<(ConfigCellHandle, Arc<ConfigBundle>)>>,
    pub(crate) config_observations:
        Mutex<Vec<(char, ConfigGeneration, ConfigGeneration, ConfigGeneration)>>,
    pub(crate) retired_config_bundles: Mutex<Vec<Weak<ConfigBundle>>>,
    pub(crate) retired_configs_released_before_body: AtomicUsize,
    pub(crate) executor_entered: Notify,
    pub(crate) executor_release: Notify,
    pub(crate) executor_builder_calls: AtomicUsize,
    pub(crate) executor_payload_bytes: AtomicUsize,
    pub(crate) executor_services:
        Mutex<Vec<hiroute_gateway_core::core::filter::FilterExecutorServices>>,
    pub(crate) offloaded_threads: Mutex<Vec<(ExecutorKind, String, String)>>,
    pub(crate) attempt_cleanup_child_started: AtomicUsize,
    pub(crate) attempt_cleanup_child_running: AtomicBool,
    pub(crate) attempt_cleanup_child_finished: AtomicUsize,
    pub(crate) attempt_cleanup_child_entered: Notify,
    pub(crate) attempt_cleanup_child_release: Mutex<bool>,
    pub(crate) attempt_cleanup_child_release_cv: Condvar,
}

pub(crate) struct LifecycleNativeFactory {
    pub(crate) facts: Arc<NativeFilterFacts>,
}

impl NativeFilterFactory for LifecycleNativeFactory {
    fn create(
        &self,
        descriptor: &CompiledFilterDescriptor,
        context: &FilterInvocationContext,
        executors: &hiroute_gateway_core::core::filter::FilterExecutorServices,
    ) -> Result<Box<dyn NativeFilter>, FilterError> {
        let instance = self.facts.next_instance.fetch_add(1, Ordering::Relaxed) + 1;
        self.facts
            .instances
            .lock()
            .expect("native filter instances")
            .push((instance, descriptor.id.clone(), context.clone()));
        if matches!(
            descriptor.id.as_ref(),
            "logical-sidecall"
                | "logical-compute"
                | "logical-blocking"
                | "attempt-background-child"
        ) {
            self.facts
                .executor_services
                .lock()
                .expect("native executor services")
                .push(executors.clone());
        }
        Ok(Box::new(LifecycleNativeFilter {
            instance,
            id: descriptor.id.clone(),
            facts: Arc::clone(&self.facts),
            data_calls: 0,
            promoted: None,
            promoted_many: Vec::new(),
        }))
    }
}

pub(crate) struct LifecycleNativeFilter {
    pub(crate) instance: usize,
    pub(crate) id: Arc<str>,
    pub(crate) facts: Arc<NativeFilterFacts>,
    pub(crate) data_calls: usize,
    pub(crate) promoted: Option<PromotedBody>,
    pub(crate) promoted_many: Vec<PromotedBody>,
}

#[async_trait]
impl NativeFilter for LifecycleNativeFilter {
    fn name(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> FilterCapabilities {
        match self.id.as_ref() {
            "logical-replace" => FilterCapabilities::observe_only().with_body_expansion(),
            "logical-drop" => FilterCapabilities::observe_only().with_body_drop(),
            "accepted-sse-expand" | "attempt-sse-expand" => FilterCapabilities::observe_only()
                .with_body_expansion()
                .with_semantic_provenance(),
            "accepted-sse-merge-two" | "accepted-sse-repeat-promoted" => {
                FilterCapabilities::observe_only()
                    .with_body_expansion()
                    .with_body_drop()
                    .with_semantic_provenance()
            }
            "attempt-sse-buffer-drop-first" => FilterCapabilities::observe_only()
                .with_body_drop()
                .with_semantic_provenance(),
            "accepted-stop" => {
                FilterCapabilities::observe_only().with_header_mutation_during_body()
            }
            _ => FilterCapabilities::observe_only(),
        }
    }

    async fn on_headers(&mut self, input: HeaderInput) -> Result<HeadersAction, FilterError> {
        self.facts
            .trace
            .lock()
            .expect("native filter trace")
            .push(format!("{}:H:{:?}", self.id, input.context.scope_kind));
        if self.id.as_ref() == "logical-config" {
            let request = input
                .context
                .configs
                .value(ConfigCellId(201))
                .expect("declared request config")
                .generation;
            let phase = input
                .context
                .configs
                .value(ConfigCellId(202))
                .expect("declared phase config")
                .generation;
            let event = input
                .context
                .configs
                .value(ConfigCellId(203))
                .expect("declared event config")
                .generation;
            self.facts
                .config_observations
                .lock()
                .expect("config observations")
                .push(('H', request, phase, event));
            for (handle, bundle) in self
                .facts
                .config_updates
                .lock()
                .expect("config updates")
                .drain(..)
            {
                handle
                    .publish(bundle)
                    .map_err(|error| FilterError::Callback(Arc::from(error.to_string())))?;
            }
        }
        if self.id.as_ref() == "logical-delay" {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if self.id.as_ref() == "logical-sidecall" {
            let admission = input.executors.admit(ExecutorKind::SidecallIo, 32)?;
            let facts = Arc::clone(&self.facts);
            let (patch, timing) = admission
                .run(move |payload| async move {
                    facts.executor_builder_calls.fetch_add(1, Ordering::Relaxed);
                    facts
                        .executor_payload_bytes
                        .store(payload.bytes(), Ordering::Relaxed);
                    facts.executor_entered.notify_one();
                    facts.executor_release.notified().await;
                    HeaderPatch::default()
                })
                .await?;
            assert!(timing.request_resumed_at >= timing.io_completed_at);
            return Ok(HeadersAction::Continue(patch));
        }
        if self.id.as_ref() == "attempt-background-child"
            && self
                .facts
                .attempt_cleanup_child_started
                .fetch_add(1, Ordering::Relaxed)
                == 0
        {
            let admission = input.executors.admit(ExecutorKind::BlockingControl, 8)?;
            let worker_facts = Arc::clone(&self.facts);
            tokio::spawn(async move {
                let _ = admission
                    .run_offloaded(move |_| {
                        worker_facts
                            .attempt_cleanup_child_running
                            .store(true, Ordering::Release);
                        worker_facts.attempt_cleanup_child_entered.notify_waiters();
                        let released = worker_facts
                            .attempt_cleanup_child_release
                            .lock()
                            .expect("attempt child release");
                        let _ = worker_facts
                            .attempt_cleanup_child_release_cv
                            .wait_timeout_while(released, Duration::from_secs(2), |released| {
                                !*released
                            })
                            .expect("attempt child release wait");
                        worker_facts
                            .attempt_cleanup_child_running
                            .store(false, Ordering::Release);
                        worker_facts
                            .attempt_cleanup_child_finished
                            .fetch_add(1, Ordering::Relaxed);
                    })
                    .await;
            });
            while !self
                .facts
                .attempt_cleanup_child_running
                .load(Ordering::Acquire)
            {
                tokio::task::yield_now().await;
            }
            return Ok(HeadersAction::Continue(HeaderPatch::default()));
        }
        let offloaded_kind = match self.id.as_ref() {
            "logical-compute" => Some(ExecutorKind::Compute),
            "logical-blocking" => Some(ExecutorKind::BlockingControl),
            _ => None,
        };
        if let Some(kind) = offloaded_kind {
            let gateway_thread = std::thread::current()
                .name()
                .unwrap_or("unnamed-gateway")
                .to_owned();
            let admission = input.executors.admit(kind, 8)?;
            let ((worker_thread, bytes), timing) = admission
                .run_offloaded(move |permit| {
                    (
                        std::thread::current()
                            .name()
                            .unwrap_or("unnamed-worker")
                            .to_owned(),
                        permit.bytes(),
                    )
                })
                .await?;
            assert_eq!(bytes, 8);
            assert!(timing.request_resumed_at >= timing.io_completed_at);
            self.facts
                .offloaded_threads
                .lock()
                .expect("offloaded filter threads")
                .push((kind, gateway_thread, worker_thread));
            return Ok(HeadersAction::Continue(HeaderPatch::default()));
        }
        if matches!(
            self.id.as_ref(),
            "logical-b" | "accepted-stop" | "accepted-stream-stop"
        ) {
            Ok(HeadersAction::StopIteration(HeaderPatch::default()))
        } else if matches!(self.id.as_ref(), "accepted-watermark" | "logical-watermark") {
            *self
                .facts
                .watermark_continuation
                .lock()
                .expect("watermark continuation") = Some(input.continuation);
            self.facts.watermark_entered.notify_one();
            Ok(HeadersAction::StopAllIterationAndWatermark(
                HeaderPatch::default(),
            ))
        } else if matches!(
            self.id.as_ref(),
            "logical-buffer" | "attempt-buffer" | "attempt-sse-buffer-drop-first"
        ) {
            *self
                .facts
                .watermark_continuation
                .lock()
                .expect("buffer continuation") = Some(input.continuation);
            self.facts.watermark_entered.notify_one();
            Ok(HeadersAction::StopAllIterationAndBuffer(
                HeaderPatch::default(),
            ))
        } else if self.id.as_ref() == "panic" {
            panic!("production native callback panic")
        } else {
            Ok(HeadersAction::Continue(HeaderPatch::default()))
        }
    }

    async fn on_data(&mut self, input: DataInput<'_>) -> Result<DataAction, FilterError> {
        self.data_calls += 1;
        self.facts
            .trace
            .lock()
            .expect("native filter trace")
            .push(format!("{}:D:{:?}", self.id, input.context.scope_kind));
        if self.id.as_ref() == "logical-config" && !input.end_stream {
            let released = self
                .facts
                .retired_config_bundles
                .lock()
                .expect("retired config bundles")
                .iter()
                .filter(|bundle| bundle.upgrade().is_none())
                .count();
            self.facts
                .retired_configs_released_before_body
                .store(released, Ordering::Relaxed);
            let request = input
                .context
                .configs
                .value(ConfigCellId(201))
                .expect("declared request config")
                .generation;
            let phase = input
                .context
                .configs
                .value(ConfigCellId(202))
                .expect("declared phase config")
                .generation;
            let event = input
                .context
                .configs
                .value(ConfigCellId(203))
                .expect("declared event config")
                .generation;
            self.facts
                .config_observations
                .lock()
                .expect("config observations")
                .push(('D', request, phase, event));
        }
        if self.id.as_ref() == "accepted-body-resume-local-reply"
            && !input.end_stream
            && self.data_calls == 1
        {
            *self
                .facts
                .watermark_continuation
                .lock()
                .expect("accepted body continuation") = Some(input.continuation);
            self.facts.watermark_entered.notify_one();
            return Ok(DataAction::StopIteration {
                retention: RetentionMode::Watermark,
                patch: HeaderPatch::default(),
            });
        }
        if self.id.as_ref() == "logical-replace" && !input.end_stream {
            let mut output = input.body_emitter()?;
            output.emit_copy(b"filtered-")?;
            output.emit_copy(b"body")?;
            return Ok(DataAction::Emit {
                output: FilterBodyEmission::Replace(output.finish()),
                patch: HeaderPatch::default(),
            });
        }
        if self.id.as_ref() == "logical-drop" && !input.end_stream {
            return Ok(DataAction::Emit {
                output: FilterBodyEmission::Drop,
                patch: HeaderPatch::default(),
            });
        }
        if self.id.as_ref() == "accepted-sse-expand" && !input.end_stream {
            let mut output = input.body_emitter()?;
            output.emit_copy(b"data:a\n\n")?;
            output.emit_copy(b"data:b\n\n")?;
            return Ok(DataAction::Emit {
                output: FilterBodyEmission::Replace(output.finish()),
                patch: HeaderPatch::default(),
            });
        }
        if self.id.as_ref() == "accepted-sse-merge-two" && !input.end_stream {
            if self.promoted.is_none() {
                self.promoted = Some(input.promote()?);
                return Ok(DataAction::Emit {
                    output: FilterBodyEmission::Drop,
                    patch: HeaderPatch::default(),
                });
            }
            let first = self.promoted.take().expect("checked promoted SSE source");
            let mut output = input.body_emitter()?;
            output.emit_copy_from_promoted(&[&first], b"data: merged\n\n")?;
            return Ok(DataAction::Emit {
                output: FilterBodyEmission::Replace(output.finish()),
                patch: HeaderPatch::default(),
            });
        }
        if self.id.as_ref() == "accepted-sse-repeat-promoted" && !input.end_stream {
            if self.promoted.is_none() {
                self.promoted = Some(input.promote()?);
                return Ok(DataAction::Emit {
                    output: FilterBodyEmission::Drop,
                    patch: HeaderPatch::default(),
                });
            }
            let mut output = input.body_emitter()?;
            output.emit_copy_from_promoted(
                &[self.promoted.as_ref().expect("promoted first SSE source")],
                b"data:o\n\n",
            )?;
            return Ok(DataAction::Emit {
                output: FilterBodyEmission::Replace(output.finish()),
                patch: HeaderPatch::default(),
            });
        }
        if self.id.as_ref() == "accepted-sse-retain-many" && !input.end_stream {
            self.promoted_many.push(input.promote()?);
        }
        if self.id.as_ref() == "attempt-sse-buffer-drop-first"
            && !input.end_stream
            && self.data_calls == 1
        {
            return Ok(DataAction::Emit {
                output: FilterBodyEmission::Drop,
                patch: HeaderPatch::default(),
            });
        }
        let patch = if self.id.as_ref() == "accepted-stop" {
            HeaderPatch::default().insert(
                http::HeaderName::from_static("x-accepted-body"),
                HeaderValue::from_static("filtered-before-commit"),
            )
        } else {
            HeaderPatch::default()
        };
        Ok(DataAction::Continue(patch))
    }

    async fn on_trailers(&mut self, _input: TrailersInput) -> Result<TrailersAction, FilterError> {
        Ok(TrailersAction::Continue(HeaderPatch::default()))
    }

    fn on_finalize(&mut self) {
        *self
            .facts
            .finalized
            .lock()
            .expect("native filter finalization")
            .entry(self.instance)
            .or_default() += 1;
    }
}
