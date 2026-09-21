use super::*;

impl DirectionMachine {
    #[allow(clippy::too_many_arguments)]
    pub fn decoder(
        stream_id: StreamId,
        scope_id: ScopeId,
        scope_kind: ScopeKind,
        filters: Vec<Box<dyn NativeFilter>>,
        max_pending_frames: usize,
        retention: Box<dyn BodyRetentionPort>,
        framing: Box<dyn FramingLedgerPort>,
    ) -> Result<Self, FilterError> {
        Self::decoder_with_context(
            stream_id,
            scope_id,
            scope_kind,
            filters,
            max_pending_frames,
            retention,
            framing,
            FilterCallbackContext::with_timeout(Duration::from_secs(30)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decoder_with_context(
        stream_id: StreamId,
        scope_id: ScopeId,
        scope_kind: ScopeKind,
        filters: Vec<Box<dyn NativeFilter>>,
        max_pending_frames: usize,
        retention: Box<dyn BodyRetentionPort>,
        framing: Box<dyn FramingLedgerPort>,
        callback_context: FilterCallbackContext,
    ) -> Result<Self, FilterError> {
        Self::new(
            stream_id,
            scope_id,
            scope_kind,
            Direction::Decode,
            filters,
            max_pending_frames,
            retention,
            framing,
            callback_context,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn encoder(
        stream_id: StreamId,
        scope_id: ScopeId,
        scope_kind: ScopeKind,
        filters: Vec<Box<dyn NativeFilter>>,
        max_pending_frames: usize,
        retention: Box<dyn BodyRetentionPort>,
        framing: Box<dyn FramingLedgerPort>,
    ) -> Result<Self, FilterError> {
        Self::encoder_with_context(
            stream_id,
            scope_id,
            scope_kind,
            filters,
            max_pending_frames,
            retention,
            framing,
            FilterCallbackContext::with_timeout(Duration::from_secs(30)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn encoder_with_context(
        stream_id: StreamId,
        scope_id: ScopeId,
        scope_kind: ScopeKind,
        mut filters: Vec<Box<dyn NativeFilter>>,
        max_pending_frames: usize,
        retention: Box<dyn BodyRetentionPort>,
        framing: Box<dyn FramingLedgerPort>,
        callback_context: FilterCallbackContext,
    ) -> Result<Self, FilterError> {
        filters.reverse();
        Self::new(
            stream_id,
            scope_id,
            scope_kind,
            Direction::Encode,
            filters,
            max_pending_frames,
            retention,
            framing,
            callback_context,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        stream_id: StreamId,
        scope_id: ScopeId,
        scope_kind: ScopeKind,
        direction: Direction,
        filters: Vec<Box<dyn NativeFilter>>,
        max_pending_frames: usize,
        retention: Box<dyn BodyRetentionPort>,
        framing: Box<dyn FramingLedgerPort>,
        callback_context: FilterCallbackContext,
    ) -> Result<Self, FilterError> {
        if max_pending_frames == 0 {
            return Err(FilterError::ZeroPendingLimit);
        }
        let pending_metadata_bytes = max_pending_frames
            .checked_mul(std::mem::size_of::<PendingFrame>())
            .ok_or(FilterError::BodyOutputBudget)?;
        let emitted_metadata_bytes = max_pending_frames
            .checked_mul(std::mem::size_of::<EmittedBodyFrame>())
            .ok_or(FilterError::BodyOutputBudget)?;
        let dropped_metadata_bytes = max_pending_frames
            .checked_mul(std::mem::size_of::<FilterBodyRuntimeOwner>())
            .ok_or(FilterError::BodyOutputBudget)?;
        let pending_queue_reservation = callback_context
            .services
            .budget
            .reserve(MemoryRole::SemanticState, pending_metadata_bytes)
            .map_err(|_| FilterError::BodyOutputBudget)?;
        let emitted_queue_reservation = callback_context
            .services
            .budget
            .reserve(MemoryRole::SemanticState, emitted_metadata_bytes)
            .map_err(|_| FilterError::BodyOutputBudget)?;
        let dropped_queue_reservation = callback_context
            .services
            .budget
            .reserve(MemoryRole::SemanticState, dropped_metadata_bytes)
            .map_err(|_| FilterError::BodyOutputBudget)?;
        let invocation = FilterInvocationContext {
            stream_id,
            scope_id,
            scope_kind,
            plan_revision: 0,
            binding_local_id: None,
            request_id: stream_id.0,
            attempt_id: None,
            attempt_generation: None,
            configs: FilterConfigSnapshot::default(),
        };
        Ok(Self {
            stream_id,
            scope_id,
            scope_kind,
            direction,
            slots: filters
                .into_iter()
                .map(|filter| FilterSlot {
                    filter,
                    headers_called: false,
                    headers_forwarded: false,
                    end_stream_seen: false,
                    finalized: false,
                })
                .collect(),
            held_headers: HeaderMap::new(),
            headers_end_stream: false,
            next_header: 0,
            pause_epoch: 0,
            pauses: Vec::new(),
            pause_receivers: Vec::new(),
            pending: VecDeque::with_capacity(max_pending_frames),
            emitted_body: VecDeque::with_capacity(max_pending_frames),
            dropped_runtime_owners: VecDeque::with_capacity(max_pending_frames),
            _pending_queue_reservation: pending_queue_reservation,
            _emitted_queue_reservation: emitted_queue_reservation,
            _dropped_queue_reservation: dropped_queue_reservation,
            max_pending_frames,
            retention,
            framing,
            terminal: false,
            finalized: false,
            callback_panicked: false,
            callback_scope: callback_context.scope,
            executor_services: callback_context.services,
            body_output_role: MemoryRole::SemanticState,
            invocation,
            telemetry: None,
        })
    }

    pub fn with_telemetry(mut self, telemetry: RequestTelemetry) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    pub fn with_invocation_context(mut self, invocation: FilterInvocationContext) -> Self {
        debug_assert_eq!(invocation.stream_id, self.stream_id);
        debug_assert_eq!(invocation.scope_id, self.scope_id);
        debug_assert_eq!(invocation.scope_kind, self.scope_kind);
        self.invocation = invocation;
        self
    }

    pub fn with_body_output_role(mut self, role: MemoryRole) -> Self {
        self.body_output_role = role;
        self
    }

    pub fn set_filter_configs(&mut self, configs: FilterConfigSnapshot) {
        self.invocation.configs = configs;
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn scope_kind(&self) -> ScopeKind {
        self.scope_kind
    }

    pub fn held_headers(&self) -> &HeaderMap {
        &self.held_headers
    }

    /// Reconciles framing changes made by the direction owner between
    /// callbacks. The owner remains the only component allowed to commit the
    /// wire head; filters continue from that exact held/committed view instead
    /// of restoring a stale pre-framing snapshot on the next body callback.
    pub fn synchronize_held_headers(&mut self, headers: &HeaderMap) {
        self.held_headers.clone_from(headers);
    }

    pub fn callback_panicked(&self) -> bool {
        self.callback_panicked
    }

    pub fn pause(&self) -> Option<FilterPause> {
        let pause = self.pauses.last()?;
        if pause.header_mode == Some(HeaderStopMode::Iteration) {
            Some(FilterPause::HeaderIteration)
        } else if pause.header_mode == Some(HeaderStopMode::AllWatermark)
            || pause.retention == Some(RetentionMode::Watermark)
        {
            Some(FilterPause::Watermark)
        } else {
            Some(FilterPause::Buffer)
        }
    }

    pub fn source_read_paused(&self) -> bool {
        self.retention.read_paused()
    }

    pub(crate) fn take_emitted_body(&mut self) -> VecDeque<EmittedBodyFrame> {
        std::mem::take(&mut self.emitted_body)
    }

    /// Returns linear input-owner dispositions produced when Drop, Replace,
    /// or `NoBuffer` consumes the current backing. Semantic lineage remains
    /// on promoted/emitted units; terminal continuation is represented
    /// separately by `EmittedBodyBacking::EndStreamControl`.
    pub(crate) fn drain_dropped_runtime_owners(
        &mut self,
    ) -> std::collections::vec_deque::Drain<'_, FilterBodyRuntimeOwner> {
        self.dropped_runtime_owners.drain(..)
    }

    /// Applies a continuation that was completed without blocking the source
    /// owner. `Ok(None)` means the current callback-owned continuation is not
    /// ready yet. Header StopIteration may also have no sender because body/EOS
    /// is itself the valid continuation path.
    pub async fn try_resume_signalled(&mut self) -> Result<Option<MachineOutcome>, FilterError> {
        let Some(pause) = self.pauses.last().copied() else {
            return Ok(None);
        };
        let Some(receiver) = self.pause_receivers.last_mut().and_then(Option::as_mut) else {
            return Ok(None);
        };
        match receiver.try_recv() {
            Ok(action) => {
                let token = self.token_for(pause);
                self.resume(token, action).await.map(Some)
            }
            Err(oneshot::error::TryRecvError::Empty) => Ok(None),
            Err(oneshot::error::TryRecvError::Closed)
                if pause.header_mode == Some(HeaderStopMode::Iteration) =>
            {
                Ok(None)
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                self.enter_failing_terminal();
                Err(FilterError::ContinuationDropped)
            }
        }
    }

    /// Waits for the exact callback-owned continuation while the lifecycle
    /// deliberately does not poll its transport source. The request deadline
    /// and cancellation are inherited from the machine's ChildScope.
    pub async fn wait_for_signalled_resume(&mut self) -> Result<MachineOutcome, FilterError> {
        let pause = self
            .pauses
            .last()
            .copied()
            .ok_or(FilterError::StaleContinuation)?;
        let receiver = self
            .pause_receivers
            .last_mut()
            .and_then(Option::as_mut)
            .ok_or(FilterError::ContinuationDropped)?;
        let scope = self.callback_scope.clone();
        let action = match scope.run_inline(receiver).await {
            Ok(Ok(action)) => action,
            Ok(Err(_)) => {
                self.enter_failing_terminal();
                return Err(FilterError::ContinuationDropped);
            }
            Err(ExecutorError::DeadlineExceeded) => {
                self.enter_failing_terminal();
                return Err(FilterError::CallbackDeadline);
            }
            Err(ExecutorError::Cancelled | ExecutorError::ScopeFinalized) => {
                self.enter_failing_terminal();
                return Err(FilterError::CallbackCancelled);
            }
            Err(error) => {
                self.enter_failing_terminal();
                return Err(FilterError::Callback(error.to_string().into()));
            }
        };
        let token = self.token_for(pause);
        self.resume(token, action).await
    }
}
