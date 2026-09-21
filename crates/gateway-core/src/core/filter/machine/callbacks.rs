use super::*;

impl DirectionMachine {
    pub async fn on_headers(
        &mut self,
        headers: HeaderMap,
        end_stream: bool,
    ) -> Result<MachineOutcome, FilterError> {
        self.ensure_active()?;
        self.observe_scope(ScopePhase::Headers, Duration::ZERO);
        if self.next_header != 0 || !self.pauses.is_empty() {
            return Err(FilterError::HeadersAlreadyStarted);
        }
        self.held_headers = headers;
        self.headers_end_stream = end_stream;
        self.run_headers_from(0).await
    }

    pub(super) async fn run_headers_from(
        &mut self,
        start: usize,
    ) -> Result<MachineOutcome, FilterError> {
        let mut index = start;
        while index < self.slots.len() {
            let (continuation, receiver) = FilterContinuation::channel();
            let input = HeaderInput {
                direction: self.direction,
                context: self.invocation.clone(),
                headers: self.held_headers.clone(),
                end_stream: self.headers_end_stream,
                executors: self.executor_services.clone(),
                continuation,
            };
            self.slots[index].headers_called = true;
            let scope = self.callback_scope.clone();
            let callback = scope
                .run_inline(
                    AssertUnwindSafe(self.slots[index].filter.on_headers(input)).catch_unwind(),
                )
                .await;
            let action = self.resolve_callback(callback)?;
            match action {
                HeadersAction::Continue(patch) => {
                    patch.apply(&mut self.held_headers, self.framing.as_mut())?;
                    self.slots[index].headers_forwarded = true;
                    index += 1;
                    self.next_header = index;
                }
                HeadersAction::StopIteration(patch) => {
                    patch.apply(&mut self.held_headers, self.framing.as_mut())?;
                    self.next_header = index + 1;
                    return self.pause_at(
                        index,
                        FrameKind::Headers,
                        Some(HeaderStopMode::Iteration),
                        None,
                        receiver,
                    );
                }
                HeadersAction::StopAllIterationAndBuffer(patch) => {
                    patch.apply(&mut self.held_headers, self.framing.as_mut())?;
                    self.next_header = index + 1;
                    return self.pause_at(
                        index,
                        FrameKind::Headers,
                        Some(HeaderStopMode::AllBuffer),
                        None,
                        receiver,
                    );
                }
                HeadersAction::StopAllIterationAndWatermark(patch) => {
                    patch.apply(&mut self.held_headers, self.framing.as_mut())?;
                    self.retention.set_read_paused(true);
                    self.next_header = index + 1;
                    return self.pause_at(
                        index,
                        FrameKind::Headers,
                        Some(HeaderStopMode::AllWatermark),
                        None,
                        receiver,
                    );
                }
                HeadersAction::LocalReply(reply) => return Ok(self.set_terminal(reply)),
            }
        }
        Ok(if self.headers_end_stream {
            MachineOutcome::Complete
        } else {
            MachineOutcome::Advanced
        })
    }

    pub async fn on_data(
        &mut self,
        bytes: Bytes,
        end_stream: bool,
    ) -> Result<MachineOutcome, FilterError> {
        self.on_data_with_sources(
            bytes,
            end_stream,
            FilterBodySourceSet::Anonymous,
            None,
            BodyMetadataOwner::default(),
        )
        .await
    }

    pub(crate) async fn on_data_with_source_and_metadata(
        &mut self,
        bytes: Bytes,
        end_stream: bool,
        source: FilterBodySourceId,
        queue_metadata: BodyMetadataOwner,
    ) -> Result<MachineOutcome, FilterError> {
        let runtime_owner = FilterBodyRuntimeOwner::new(source.clone());
        self.on_data_with_sources(
            bytes,
            end_stream,
            FilterBodySourceSet::one(source),
            Some(runtime_owner),
            queue_metadata,
        )
        .await
    }

    pub(super) async fn on_data_with_sources(
        &mut self,
        bytes: Bytes,
        end_stream: bool,
        sources: FilterBodySourceSet,
        runtime_owner: Option<FilterBodyRuntimeOwner>,
        queue_metadata: BodyMetadataOwner,
    ) -> Result<MachineOutcome, FilterError> {
        self.ensure_active()?;
        self.observe_scope(ScopePhase::Body, Duration::ZERO);
        if let Some(pause) = self.pauses.last().copied() {
            if pause.frame_kind == FrameKind::Headers {
                match pause.header_mode.expect("header pause mode") {
                    HeaderStopMode::Iteration => {
                        return self
                            .deliver_data_range(
                                EmittedBodyBacking::Forwarded(bytes),
                                end_stream,
                                0,
                                Some(pause.filter_index),
                                FilterBodyOwnership::new(runtime_owner, sources),
                                queue_metadata,
                            )
                            .await;
                    }
                    HeaderStopMode::AllBuffer | HeaderStopMode::AllWatermark => {
                        self.queue_data(
                            EmittedBodyBacking::Forwarded(bytes),
                            end_stream,
                            pause.filter_index,
                            None,
                            FilterBodyOwnership::new(runtime_owner, sources),
                            queue_metadata,
                        )?;
                        return Ok(MachineOutcome::Paused(self.token_for(pause)));
                    }
                }
            }
            self.queue_data(
                EmittedBodyBacking::Forwarded(bytes),
                end_stream,
                pause.filter_index + 1,
                self.suspended_header_stop_after(),
                FilterBodyOwnership::new(runtime_owner, sources),
                queue_metadata,
            )?;
            return Ok(MachineOutcome::Paused(self.token_for(pause)));
        }
        self.deliver_data_range(
            EmittedBodyBacking::Forwarded(bytes),
            end_stream,
            0,
            None,
            FilterBodyOwnership::new(runtime_owner, sources),
            queue_metadata,
        )
        .await
    }

    pub(super) async fn deliver_data_range(
        &mut self,
        backing: EmittedBodyBacking,
        end_stream: bool,
        start: usize,
        mut stop_after: Option<usize>,
        ownership: FilterBodyOwnership,
        queue_metadata: BodyMetadataOwner,
    ) -> Result<MachineOutcome, FilterError> {
        let FilterBodyOwnership {
            runtime_owner,
            sources,
        } = ownership;
        let mut index = start;
        while index < self.slots.len() {
            let (continuation, receiver) = FilterContinuation::channel();
            let scope = self.callback_scope.clone();
            let max_output_units = if self.slots[index].filter.capabilities().expands_body() {
                self.max_pending_frames
            } else {
                1
            };
            let callback = scope
                .run_inline(
                    AssertUnwindSafe(self.slots[index].filter.on_data(DataInput {
                        direction: self.direction,
                        context: self.invocation.clone(),
                        bytes: backing.bytes(),
                        end_stream,
                        executors: self.executor_services.clone(),
                        continuation,
                        output_role: self.body_output_role,
                        max_output_units,
                        sources: sources.clone(),
                    }))
                    .catch_unwind(),
                )
                .await;
            let action = self.resolve_callback(callback)?;
            self.slots[index].end_stream_seen |= end_stream;
            match action {
                DataAction::Continue(patch) => {
                    patch.apply(&mut self.held_headers, self.framing.as_mut())?;
                    if stop_after == Some(index) {
                        // Data Continue by a header-paused filter implicitly
                        // resumes headers before forwarding this same frame.
                        match self.resume_headers_after_body_frame(index).await? {
                            MachineOutcome::LocalReply(reply) => {
                                return Ok(MachineOutcome::LocalReply(reply));
                            }
                            MachineOutcome::Paused(token) => {
                                let next_pause = self
                                    .pauses
                                    .last()
                                    .copied()
                                    .ok_or(FilterError::StaleContinuation)?;
                                if next_pause.frame_kind == FrameKind::Headers
                                    && next_pause.header_mode == Some(HeaderStopMode::Iteration)
                                {
                                    // A later header StopIteration is driven
                                    // by this same frame. Preserve the exact
                                    // downstream cursor and stop at that
                                    // filter after intervening filters have
                                    // observed the frame in chain order.
                                    stop_after = Some(next_pause.filter_index);
                                } else {
                                    self.queue_data(
                                        backing,
                                        end_stream,
                                        index + 1,
                                        None,
                                        FilterBodyOwnership::new(runtime_owner, sources),
                                        queue_metadata,
                                    )?;
                                    return Ok(MachineOutcome::Paused(token));
                                }
                            }
                            MachineOutcome::Advanced | MachineOutcome::Complete => {
                                stop_after = None;
                            }
                        }
                    }
                    index += 1;
                }
                DataAction::Emit { output, patch } => {
                    patch.apply(&mut self.held_headers, self.framing.as_mut())?;
                    match output {
                        FilterBodyEmission::Forward => {
                            if stop_after == Some(index) {
                                match self.resume_headers_after_body_frame(index).await? {
                                    MachineOutcome::LocalReply(reply) => {
                                        return Ok(MachineOutcome::LocalReply(reply));
                                    }
                                    MachineOutcome::Paused(token) => {
                                        let next_pause = self
                                            .pauses
                                            .last()
                                            .copied()
                                            .ok_or(FilterError::StaleContinuation)?;
                                        if next_pause.frame_kind == FrameKind::Headers
                                            && next_pause.header_mode
                                                == Some(HeaderStopMode::Iteration)
                                        {
                                            stop_after = Some(next_pause.filter_index);
                                        } else {
                                            self.queue_data(
                                                backing,
                                                end_stream,
                                                index + 1,
                                                None,
                                                FilterBodyOwnership::new(runtime_owner, sources),
                                                queue_metadata,
                                            )?;
                                            return Ok(MachineOutcome::Paused(token));
                                        }
                                    }
                                    MachineOutcome::Advanced | MachineOutcome::Complete => {
                                        stop_after = None;
                                    }
                                }
                            }
                            index += 1;
                        }
                        FilterBodyEmission::Drop => {
                            if !self.slots[index].filter.capabilities().drops_body() {
                                return Err(FilterError::BodyMutationNotDeclared {
                                    filter: self.slots[index].filter.name().to_owned(),
                                });
                            }
                            self.framing.body_transformed();
                            self.record_dropped_runtime_owner(runtime_owner)?;
                            let downstream_stop_after = if stop_after == Some(index) {
                                match self.resume_headers_after_body_frame(index).await? {
                                    MachineOutcome::LocalReply(reply) => {
                                        return Ok(MachineOutcome::LocalReply(reply));
                                    }
                                    MachineOutcome::Paused(token) => {
                                        let next_pause = self
                                            .pauses
                                            .last()
                                            .copied()
                                            .ok_or(FilterError::StaleContinuation)?;
                                        if next_pause.frame_kind == FrameKind::Headers
                                            && next_pause.header_mode
                                                == Some(HeaderStopMode::Iteration)
                                            && end_stream
                                        {
                                            Some(next_pause.filter_index)
                                        } else {
                                            if end_stream {
                                                self.queue_data(
                                                    EmittedBodyBacking::EndStreamControl,
                                                    true,
                                                    index + 1,
                                                    None,
                                                    FilterBodyOwnership::new(None, sources),
                                                    BodyMetadataOwner::default(),
                                                )?;
                                            }
                                            return Ok(MachineOutcome::Paused(token));
                                        }
                                    }
                                    MachineOutcome::Advanced | MachineOutcome::Complete => None,
                                }
                            } else {
                                stop_after
                            };
                            if end_stream {
                                // Dropping the final payload does not drop the
                                // stream terminator. Every later filter still
                                // observes one zero-byte EOS in chain order.
                                return Box::pin(self.deliver_data_range(
                                    EmittedBodyBacking::EndStreamControl,
                                    true,
                                    index + 1,
                                    downstream_stop_after,
                                    FilterBodyOwnership::new(None, sources),
                                    BodyMetadataOwner::default(),
                                ))
                                .await;
                            }
                            return Ok(MachineOutcome::Advanced);
                        }
                        FilterBodyEmission::Replace(outputs) => {
                            if !self.slots[index].filter.capabilities().mutates_body() {
                                return Err(FilterError::BodyMutationNotDeclared {
                                    filter: self.slots[index].filter.name().to_owned(),
                                });
                            }
                            self.framing.body_transformed();
                            self.record_dropped_runtime_owner(runtime_owner)?;
                            let downstream_stop_after = if stop_after == Some(index) {
                                match self.resume_headers_after_body_frame(index).await? {
                                    MachineOutcome::LocalReply(reply) => {
                                        return Ok(MachineOutcome::LocalReply(reply));
                                    }
                                    MachineOutcome::Paused(token) => {
                                        let next_pause = self
                                            .pauses
                                            .last()
                                            .copied()
                                            .ok_or(FilterError::StaleContinuation)?;
                                        if next_pause.frame_kind == FrameKind::Headers
                                            && next_pause.header_mode
                                                == Some(HeaderStopMode::Iteration)
                                            && (!outputs.is_empty() || end_stream)
                                        {
                                            Some(next_pause.filter_index)
                                        } else {
                                            if outputs.is_empty() {
                                                if end_stream {
                                                    self.queue_data(
                                                        EmittedBodyBacking::EndStreamControl,
                                                        true,
                                                        index + 1,
                                                        None,
                                                        FilterBodyOwnership::new(None, sources),
                                                        BodyMetadataOwner::default(),
                                                    )?;
                                                }
                                            } else {
                                                let last = outputs.len().saturating_sub(1);
                                                for (position, output) in
                                                    outputs.into_iter().enumerate()
                                                {
                                                    self.queue_data(
                                                        EmittedBodyBacking::Replacement(
                                                            output.bytes,
                                                        ),
                                                        end_stream && position == last,
                                                        index + 1,
                                                        None,
                                                        FilterBodyOwnership::new(
                                                            None,
                                                            output.sources,
                                                        ),
                                                        output.queue_metadata,
                                                    )?;
                                                }
                                            }
                                            return Ok(MachineOutcome::Paused(token));
                                        }
                                    }
                                    MachineOutcome::Advanced | MachineOutcome::Complete => None,
                                }
                            } else {
                                stop_after
                            };
                            if outputs.is_empty() {
                                if end_stream {
                                    return Box::pin(self.deliver_data_range(
                                        EmittedBodyBacking::EndStreamControl,
                                        true,
                                        index + 1,
                                        downstream_stop_after,
                                        FilterBodyOwnership::new(None, sources),
                                        BodyMetadataOwner::default(),
                                    ))
                                    .await;
                                }
                                return Ok(MachineOutcome::Advanced);
                            }
                            let last = outputs.len().saturating_sub(1);
                            let mut outputs = outputs.into_iter().enumerate();
                            while let Some((position, output)) = outputs.next() {
                                let output_eos = end_stream && position == last;
                                let outcome = Box::pin(self.deliver_data_range(
                                    EmittedBodyBacking::Replacement(output.bytes),
                                    output_eos,
                                    index + 1,
                                    downstream_stop_after,
                                    FilterBodyOwnership::new(None, output.sources),
                                    output.queue_metadata,
                                ))
                                .await?;
                                match outcome {
                                    MachineOutcome::Paused(token) => {
                                        for (remaining, output) in outputs {
                                            self.queue_data(
                                                EmittedBodyBacking::Replacement(output.bytes),
                                                end_stream && remaining == last,
                                                index + 1,
                                                downstream_stop_after,
                                                FilterBodyOwnership::new(None, output.sources),
                                                output.queue_metadata,
                                            )?;
                                        }
                                        return Ok(MachineOutcome::Paused(token));
                                    }
                                    MachineOutcome::LocalReply(reply) => {
                                        return Ok(MachineOutcome::LocalReply(reply));
                                    }
                                    MachineOutcome::Advanced | MachineOutcome::Complete => {}
                                }
                            }
                            return Ok(if end_stream {
                                MachineOutcome::Complete
                            } else {
                                MachineOutcome::Advanced
                            });
                        }
                    }
                }
                DataAction::StopIteration { retention, patch } => {
                    patch.apply(&mut self.held_headers, self.framing.as_mut())?;
                    if retention == RetentionMode::NoBuffer
                        && !self.slots[index].filter.may_drop_body()
                    {
                        return Err(FilterError::BodyDropNotDeclared {
                            filter: self.slots[index].filter.name().to_owned(),
                        });
                    }
                    if retention != RetentionMode::NoBuffer {
                        self.queue_data(
                            backing,
                            end_stream,
                            index + 1,
                            stop_after,
                            FilterBodyOwnership::new(runtime_owner, sources),
                            queue_metadata,
                        )?;
                    } else {
                        self.framing.body_transformed();
                        self.queue_dropped_data_control(
                            end_stream,
                            index + 1,
                            stop_after,
                            runtime_owner,
                            sources,
                        )?;
                    }
                    if retention == RetentionMode::Watermark {
                        self.retention.set_read_paused(true);
                    }
                    return self.pause_at(index, FrameKind::Data, None, Some(retention), receiver);
                }
                DataAction::LocalReply(reply) => return Ok(self.set_terminal(reply)),
            }
        }
        self.emitted_body.push_back(EmittedBodyFrame {
            backing,
            end_stream,
            runtime_owner,
            sources,
            queue_metadata,
        });
        Ok(if end_stream {
            MachineOutcome::Complete
        } else {
            MachineOutcome::Advanced
        })
    }

    pub(super) async fn resume_headers_after_body_frame(
        &mut self,
        filter_index: usize,
    ) -> Result<MachineOutcome, FilterError> {
        let suspended = self.pauses.pop().ok_or(FilterError::StaleContinuation)?;
        if suspended.frame_kind != FrameKind::Headers
            || suspended.header_mode != Some(HeaderStopMode::Iteration)
            || suspended.filter_index != filter_index
        {
            self.pauses.push(suspended);
            return Err(FilterError::StaleContinuation);
        }
        self.pause_receivers.pop();
        self.observe_scope(ScopePhase::Headers, suspended.paused_at.elapsed());
        self.pause_epoch = self.pause_epoch.wrapping_add(1);
        self.run_headers_from(self.next_header).await
    }

    pub async fn on_trailers(
        &mut self,
        trailers: HeaderMap,
    ) -> Result<MachineOutcome, FilterError> {
        self.ensure_active()?;
        self.observe_scope(ScopePhase::Body, Duration::ZERO);
        if let Some(pause) = self.pauses.last().copied() {
            if pause.frame_kind == FrameKind::Headers
                && pause.header_mode == Some(HeaderStopMode::Iteration)
            {
                // Header StopIteration is body driven. Trailers are the final
                // body frame, so every filter whose headers have run,
                // including the stopped filter, observes them. A Continue by
                // that filter resumes later headers before the same trailers
                // continue down the chain.
                return self
                    .deliver_trailers_from(trailers, 0, Some(pause.filter_index))
                    .await;
            }
            self.queue_trailers(
                trailers,
                pause.filter_index,
                self.suspended_header_stop_after(),
            )?;
            return Ok(MachineOutcome::Paused(self.token_for(pause)));
        }
        self.deliver_trailers_from(trailers, 0, None).await
    }

    pub(super) async fn deliver_trailers_from(
        &mut self,
        trailers: HeaderMap,
        start: usize,
        mut stop_after: Option<usize>,
    ) -> Result<MachineOutcome, FilterError> {
        let mut index = start;
        while index < self.slots.len() {
            let (continuation, receiver) = FilterContinuation::channel();
            let scope = self.callback_scope.clone();
            let callback = scope
                .run_inline(
                    AssertUnwindSafe(self.slots[index].filter.on_trailers(TrailersInput {
                        direction: self.direction,
                        context: self.invocation.clone(),
                        trailers: trailers.clone(),
                        executors: self.executor_services.clone(),
                        continuation,
                    }))
                    .catch_unwind(),
                )
                .await;
            let action = self.resolve_callback(callback)?;
            match action {
                TrailersAction::Continue(patch) => {
                    patch.apply(&mut self.held_headers, self.framing.as_mut())?;
                    self.slots[index].end_stream_seen = true;
                    if stop_after == Some(index) {
                        match self.resume_headers_after_body_frame(index).await? {
                            MachineOutcome::LocalReply(reply) => {
                                return Ok(MachineOutcome::LocalReply(reply));
                            }
                            MachineOutcome::Paused(token) => {
                                let next_pause = self
                                    .pauses
                                    .last()
                                    .copied()
                                    .ok_or(FilterError::StaleContinuation)?;
                                if next_pause.frame_kind == FrameKind::Headers
                                    && next_pause.header_mode == Some(HeaderStopMode::Iteration)
                                {
                                    stop_after = Some(next_pause.filter_index);
                                } else {
                                    self.queue_trailers(trailers, index + 1, None)?;
                                    return Ok(MachineOutcome::Paused(token));
                                }
                            }
                            MachineOutcome::Advanced | MachineOutcome::Complete => {
                                stop_after = None;
                            }
                        }
                    }
                }
                TrailersAction::StopIteration(patch) => {
                    patch.apply(&mut self.held_headers, self.framing.as_mut())?;
                    self.queue_trailers(trailers, index + 1, stop_after)?;
                    return self.pause_at(index, FrameKind::Trailers, None, None, receiver);
                }
                TrailersAction::LocalReply(reply) => return Ok(self.set_terminal(reply)),
            }
            index += 1;
        }
        Ok(MachineOutcome::Complete)
    }
}
