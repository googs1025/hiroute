use super::*;

impl NativeGatewayRequestFilters {
    pub(super) fn allocate_pending_queue<T>(
        descriptors: &[CompiledFilterDescriptor],
        budget: &StreamBudget,
    ) -> Result<(VecDeque<T>, Option<Reservation>), Arc<str>> {
        let Some(max_frames) = descriptors
            .iter()
            .map(|descriptor| descriptor.max_pending_frames)
            .min()
        else {
            return Ok((VecDeque::new(), None));
        };
        // The current callback input remains here while the direction
        // machine may independently retain `max_frames` continuations.
        // Account both fixed owners before allocating either backing.
        let queue_capacity = max_frames
            .checked_add(1)
            .ok_or_else(|| Arc::<str>::from("filter pending queue capacity overflow"))?;
        let metadata_bytes = queue_capacity
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(|| Arc::<str>::from("filter pending queue metadata overflow"))?;
        let reservation = budget
            .reserve(MemoryRole::SemanticState, metadata_bytes)
            .map_err(body_filter_error)?;
        Ok((VecDeque::with_capacity(queue_capacity), Some(reservation)))
    }

    pub(super) fn release_pending_queue<T>(
        queue: &mut VecDeque<T>,
        reservation: &mut Option<Reservation>,
    ) {
        drop(std::mem::take(queue));
        reservation.take();
    }

    pub(super) fn push_pending<T>(
        queue: &mut VecDeque<T>,
        reservation: &Option<Reservation>,
        frame: T,
    ) -> Result<(), Arc<str>> {
        if queue.capacity() == 0 {
            let capacity = reservation
                .as_ref()
                .map(|reservation| reservation.bytes() / std::mem::size_of::<T>())
                .unwrap_or(0);
            if capacity != 0 {
                // A successful result from the previous callback has already
                // transferred and drained the old fixed backing. Recreate the
                // next backing only at the following serial callback boundary;
                // the scope reservation remains the single metadata owner.
                *queue = VecDeque::with_capacity(capacity);
            }
        }
        if queue.len() == queue.capacity() {
            return Err(Arc::from(format!(
                "filter pending queue hard capacity exceeded for {} ({}/{})",
                std::any::type_name::<T>(),
                queue.len(),
                queue.capacity()
            )));
        }
        queue.push_back(frame);
        Ok(())
    }

    pub(super) fn allocate_body_source(&mut self) -> FilterBodySourceId {
        self.next_body_source = self.next_body_source.wrapping_add(1);
        FilterBodySourceId::new(self.next_body_source)
    }

    pub(super) fn build_machine(
        &self,
        descriptors: &[CompiledFilterDescriptor],
        context: GatewayFilterScopeContext,
        encode: bool,
    ) -> Result<Option<DirectionMachine>, Arc<str>> {
        if descriptors.is_empty() {
            return Ok(None);
        }
        let callback = FilterCallbackContext::with_services(context.executors.clone());
        let mut filters = Vec::with_capacity(descriptors.len());
        for descriptor in descriptors {
            let factory = self.factories.get(&descriptor.id).ok_or_else(|| {
                Arc::<str>::from(format!("unregistered filter {}", descriptor.id))
            })?;
            let filter = factory
                .create(descriptor, &context.invocation, callback.services())
                .map_err(filter_error)?;
            let runtime_capabilities = filter.capabilities();
            if !descriptor.capabilities().allows(runtime_capabilities)
                || (filter.may_drop_body() && !descriptor.capabilities().drops_body())
            {
                return Err(filter_error(FilterError::CapabilityEscalation {
                    filter: descriptor.id.to_string(),
                }));
            }
            filters.push(filter);
        }
        let max_pending_frames = descriptors
            .iter()
            .map(|descriptor| descriptor.max_pending_frames)
            .min()
            .ok_or_else(|| Arc::<str>::from("empty filter chain"))?;
        let retention: Box<dyn BodyRetentionPort> = Box::new(
            RuntimeBodyRetention::new(
                self.max_retained_bytes_per_scope,
                max_pending_frames,
                &context.budget,
            )
            .map_err(filter_error)?,
        );
        let framing: Box<dyn FramingLedgerPort> = Box::new(FramingLedger::default());
        let body_output_role = match (context.invocation.scope_kind, encode) {
            (ScopeKind::LogicalRequest, false) => MemoryRole::RawRequest,
            (ScopeKind::RouteAttempt, true) => MemoryRole::AttemptWire,
            (ScopeKind::RouteAttempt, false) => MemoryRole::ResponsePrefix,
            (ScopeKind::AcceptedResponse, true) => MemoryRole::OutputQueue,
            _ => MemoryRole::SemanticState,
        };
        let machine = if encode {
            DirectionMachine::encoder_with_context(
                context.invocation.stream_id,
                context.invocation.scope_id,
                context.invocation.scope_kind,
                filters,
                max_pending_frames,
                retention,
                framing,
                callback,
            )
        } else {
            DirectionMachine::decoder_with_context(
                context.invocation.stream_id,
                context.invocation.scope_id,
                context.invocation.scope_kind,
                filters,
                max_pending_frames,
                retention,
                framing,
                callback,
            )
        }
        .map_err(filter_error)?
        .with_invocation_context(context.invocation)
        .with_body_output_role(body_output_role);
        Ok(Some(match context.telemetry {
            Some(telemetry) => machine.with_telemetry(telemetry),
            None => machine,
        }))
    }

    pub(super) fn resolve_outcome<T, P>(
        machine: &mut DirectionMachine,
        pending: &mut VecDeque<P>,
        outcome: MachineOutcome,
        emitted: BodyEmitterOutcome<T>,
    ) -> GatewayFilterResult<T> {
        match outcome {
            MachineOutcome::Paused(_) => GatewayFilterResult {
                frames: BodyEmitterOutcome::drop_input(),
                local_reply: None,
                pause: machine.pause(),
            },
            MachineOutcome::LocalReply(reply) => {
                pending.clear();
                machine.take_emitted_body();
                GatewayFilterResult {
                    frames: BodyEmitterOutcome::drop_input(),
                    local_reply: Some(reply),
                    pause: None,
                }
            }
            MachineOutcome::Advanced | MachineOutcome::Complete => GatewayFilterResult {
                frames: emitted,
                local_reply: None,
                pause: None,
            },
        }
    }

    pub(super) fn logical_emission_matches(
        pending: &VecDeque<PendingFilterFrame<LogicalRequestBodyFrame>>,
        emitted: &VecDeque<EmittedBodyFrame>,
    ) -> bool {
        pending.len() == emitted.len()
            && pending.iter().zip(emitted).all(|(input, emitted)| {
                input.source.as_ref().is_some_and(|source| {
                    emitted
                        .runtime_owner
                        .as_ref()
                        .is_some_and(|owner| owner.source() == source)
                }) && input.frame.end_stream == emitted.end_stream
                    && input
                        .frame
                        .bytes
                        .as_ref()
                        .map_or(emitted.bytes().is_empty(), |input| {
                            input.bytes().as_ref() == emitted.bytes()
                        })
            })
    }

    pub(super) fn take_pending_source<T>(
        pending: &mut VecDeque<PendingFilterFrame<T>>,
        source: &FilterBodySourceId,
    ) -> Option<T> {
        let position = pending
            .iter()
            .position(|pending| pending.source.as_ref() == Some(source))?;
        pending.remove(position).map(|pending| pending.frame)
    }

    /// Applies each linear current-backing disposition before a pause is
    /// exposed to the lifecycle. Semantic promotion/merge lineage is kept in
    /// its scope ledger and never used to choose which runtime frame to drop.
    /// A source already released by an earlier transform is idempotent here.
    pub(super) fn apply_dropped_input_ownership<T>(
        machine: &mut DirectionMachine,
        pending: &mut VecDeque<PendingFilterFrame<T>>,
    ) {
        for dropped in machine.drain_dropped_runtime_owners() {
            if let Some(position) = pending
                .iter()
                .position(|pending| pending.source.as_ref() == Some(dropped.source()))
            {
                pending.remove(position);
            }
        }
    }

    pub(super) fn resolve_logical_frames(
        machine: &mut DirectionMachine,
        pending: &mut VecDeque<PendingFilterFrame<LogicalRequestBodyFrame>>,
        _budget: &StreamBudget,
        outcome: MachineOutcome,
    ) -> Result<GatewayFilterResult<LogicalRequestBodyFrame>, Arc<str>> {
        Self::apply_dropped_input_ownership(machine, pending);
        if matches!(
            outcome,
            MachineOutcome::Paused(_) | MachineOutcome::LocalReply(_)
        ) {
            return Ok(Self::resolve_outcome(
                machine,
                pending,
                outcome,
                BodyEmitterOutcome::drop_input(),
            ));
        }
        let emitted = machine.take_emitted_body();
        let frames = if Self::logical_emission_matches(pending, &emitted) {
            BodyEmitterOutcome::replace(std::mem::take(pending).into_iter().map(|item| item.frame))
        } else {
            let mut replacements = VecDeque::with_capacity(emitted.len());
            for emitted in emitted {
                let EmittedBodyFrame {
                    backing,
                    end_stream,
                    runtime_owner,
                    sources,
                    queue_metadata,
                } = emitted;
                let bytes = match backing {
                    EmittedBodyBacking::Forwarded(bytes) => {
                        let Some(runtime_owner) = runtime_owner.as_ref() else {
                            return Err(Arc::from(
                                "forwarded logical filter output lost its runtime owner",
                            ));
                        };
                        let source = runtime_owner.source();
                        if !sources.as_slice().contains(source) {
                            return Err(Arc::from(
                                "forwarded logical filter output detached from its semantic source",
                            ));
                        }
                        let frame =
                            Self::take_pending_source(pending, source).ok_or_else(|| {
                                Arc::from("forwarded logical filter output has an unknown source")
                            })?;
                        if frame.end_stream != end_stream
                            || frame
                                .bytes
                                .as_ref()
                                .map_or(!bytes.is_empty(), |input| input.bytes().as_ref() != bytes)
                        {
                            return Err(Arc::from(
                                "forwarded logical filter output changed its source bytes",
                            ));
                        }
                        replacements.push_back(frame);
                        continue;
                    }
                    EmittedBodyBacking::Replacement(bytes) => bytes,
                    EmittedBodyBacking::EndStreamControl => {
                        if !end_stream {
                            return Err(Arc::from(
                                "logical filter emitted a non-terminal EOS control",
                            ));
                        }
                        replacements.push_back(LogicalRequestBodyFrame {
                            bytes: None,
                            end_stream: true,
                            queue_metadata: BodyMetadataOwner::default(),
                        });
                        continue;
                    }
                };
                let bytes = if bytes.bytes().is_empty() {
                    None
                } else {
                    Some(
                        bytes
                            .transfer_role(MemoryRole::RawRequest)
                            .map_err(body_filter_error)?
                            .with_metadata(queue_metadata.clone()),
                    )
                };
                replacements.push_back(LogicalRequestBodyFrame {
                    bytes,
                    end_stream,
                    queue_metadata,
                });
            }
            pending.clear();
            BodyEmitterOutcome::replace(replacements)
        };
        Ok(Self::resolve_outcome(machine, pending, outcome, frames))
    }

    pub(super) fn attempt_request_emission_matches(
        pending: &VecDeque<PendingFilterFrame<AttemptRequestBodyFrame>>,
        emitted: &VecDeque<EmittedBodyFrame>,
    ) -> bool {
        pending.len() == emitted.len()
            && pending.iter().zip(emitted).all(|(input, emitted)| {
                input.source.as_ref().is_some_and(|source| {
                    emitted
                        .runtime_owner
                        .as_ref()
                        .is_some_and(|owner| owner.source() == source)
                }) && input.frame.end_stream == emitted.end_stream
                    && input
                        .frame
                        .bytes
                        .as_ref()
                        .map_or(emitted.bytes().is_empty(), |input| {
                            input.bytes().as_ref() == emitted.bytes()
                        })
            })
    }

    pub(super) fn resolve_attempt_request_frames(
        machine: &mut DirectionMachine,
        pending: &mut VecDeque<PendingFilterFrame<AttemptRequestBodyFrame>>,
        _budget: &StreamBudget,
        outcome: MachineOutcome,
    ) -> Result<GatewayFilterResult<AttemptRequestBodyFrame>, Arc<str>> {
        Self::apply_dropped_input_ownership(machine, pending);
        if matches!(
            outcome,
            MachineOutcome::Paused(_) | MachineOutcome::LocalReply(_)
        ) {
            return Ok(Self::resolve_outcome(
                machine,
                pending,
                outcome,
                BodyEmitterOutcome::drop_input(),
            ));
        }
        let emitted = machine.take_emitted_body();
        let frames = if Self::attempt_request_emission_matches(pending, &emitted) {
            BodyEmitterOutcome::replace(std::mem::take(pending).into_iter().map(|item| item.frame))
        } else {
            let mut replacements = VecDeque::with_capacity(emitted.len());
            for emitted in emitted {
                let EmittedBodyFrame {
                    backing,
                    end_stream,
                    runtime_owner,
                    sources,
                    queue_metadata,
                } = emitted;
                let bytes = match backing {
                    EmittedBodyBacking::Forwarded(bytes) => {
                        let Some(runtime_owner) = runtime_owner.as_ref() else {
                            return Err(Arc::from(
                                "forwarded attempt-request output lost its runtime owner",
                            ));
                        };
                        let source = runtime_owner.source();
                        if !sources.as_slice().contains(source) {
                            return Err(Arc::from(
                                "forwarded attempt-request output detached from its semantic source",
                            ));
                        }
                        let frame =
                            Self::take_pending_source(pending, source).ok_or_else(|| {
                                Arc::from("forwarded attempt-request output has an unknown source")
                            })?;
                        if frame.end_stream != end_stream
                            || frame
                                .bytes
                                .as_ref()
                                .map_or(!bytes.is_empty(), |input| input.bytes().as_ref() != bytes)
                        {
                            return Err(Arc::from(
                                "forwarded attempt-request output changed its source bytes",
                            ));
                        }
                        replacements.push_back(frame);
                        continue;
                    }
                    EmittedBodyBacking::Replacement(bytes) => bytes,
                    EmittedBodyBacking::EndStreamControl => {
                        if !end_stream {
                            return Err(Arc::from(
                                "attempt-request filter emitted a non-terminal EOS control",
                            ));
                        }
                        replacements.push_back(AttemptRequestBodyFrame {
                            bytes: None,
                            end_stream: true,
                            queue_metadata: BodyMetadataOwner::default(),
                        });
                        continue;
                    }
                };
                let bytes = if bytes.bytes().is_empty() {
                    None
                } else {
                    Some(
                        bytes
                            .transfer_role(MemoryRole::AttemptWire)
                            .map_err(body_filter_error)?
                            .with_metadata(queue_metadata.clone()),
                    )
                };
                replacements.push_back(AttemptRequestBodyFrame {
                    bytes,
                    end_stream,
                    queue_metadata,
                });
            }
            pending.clear();
            BodyEmitterOutcome::replace(replacements)
        };
        Ok(Self::resolve_outcome(machine, pending, outcome, frames))
    }

    pub(super) fn attempt_emission_matches(
        pending: &VecDeque<PendingFilterFrame<PrecommitEvent>>,
        emitted: &VecDeque<EmittedBodyFrame>,
    ) -> bool {
        let mut emitted = emitted.iter();
        for pending in pending {
            let expected = match &pending.frame {
                PrecommitEvent::ResponseHead(_) => continue,
                PrecommitEvent::Body(bytes) => (bytes.bytes().as_ref(), false),
                PrecommitEvent::SseEvent { bytes, .. } => (bytes.bytes().as_ref(), false),
                PrecommitEvent::EndStream => (&[][..], true),
            };
            let Some(emitted) = emitted.next() else {
                return false;
            };
            if pending.source.as_ref().is_none_or(|source| {
                emitted
                    .runtime_owner
                    .as_ref()
                    .is_none_or(|owner| owner.source() != source)
            }) || emitted.bytes() != expected.0
                || emitted.end_stream != expected.1
            {
                return false;
            }
        }
        emitted.next().is_none()
    }

    pub(super) fn resolve_attempt_frames(
        machine: &mut DirectionMachine,
        pending: &mut VecDeque<PendingFilterFrame<PrecommitEvent>>,
        sources: &mut AttemptFilterSourceLedger,
        _budget: &StreamBudget,
        outcome: MachineOutcome,
    ) -> Result<GatewayFilterResult<PrecommitEvent>, Arc<str>> {
        Self::apply_dropped_input_ownership(machine, pending);
        sources.reclaim_dead();
        if matches!(
            outcome,
            MachineOutcome::Paused(_) | MachineOutcome::LocalReply(_)
        ) {
            return Ok(Self::resolve_outcome(
                machine,
                pending,
                outcome,
                BodyEmitterOutcome::drop_input(),
            ));
        }
        let emitted = machine.take_emitted_body();
        let frames = if Self::attempt_emission_matches(pending, &emitted) {
            BodyEmitterOutcome::replace(std::mem::take(pending).into_iter().map(|item| item.frame))
        } else {
            let mut charged = VecDeque::with_capacity(emitted.len());
            for emitted in emitted {
                let EmittedBodyFrame {
                    backing,
                    end_stream,
                    runtime_owner,
                    sources: lineage,
                    queue_metadata,
                } = emitted;
                let (bytes, typed_end_stream) = match backing {
                    EmittedBodyBacking::Replacement(bytes) => (
                        (!bytes.bytes().is_empty())
                            .then(|| {
                                bytes
                                    .transfer_role(MemoryRole::ResponsePrefix)
                                    .map_err(body_filter_error)
                                    .map(|bytes| bytes.with_metadata(queue_metadata.clone()))
                            })
                            .transpose()?,
                        false,
                    ),
                    EmittedBodyBacking::Forwarded(view) => {
                        let Some(runtime_owner) = runtime_owner.as_ref() else {
                            return Err(Arc::from(
                                "forwarded attempt-response output lost its runtime owner",
                            ));
                        };
                        let source = runtime_owner.source();
                        if !lineage.as_slice().contains(source) {
                            return Err(Arc::from(
                                "forwarded attempt-response output detached from its semantic source",
                            ));
                        }
                        let frame =
                            Self::take_pending_source(pending, source).ok_or_else(|| {
                                Arc::from("forwarded attempt-response output has an unknown source")
                            })?;
                        match frame {
                            PrecommitEvent::Body(bytes)
                            | PrecommitEvent::SseEvent { bytes, .. } => {
                                if end_stream || bytes.bytes().as_ref() != view {
                                    return Err(Arc::from(
                                        "forwarded attempt-response output changed its source bytes",
                                    ));
                                }
                                (Some(bytes), false)
                            }
                            PrecommitEvent::EndStream => {
                                if !end_stream || !view.is_empty() {
                                    return Err(Arc::from(
                                        "forwarded attempt-response EOS changed its source",
                                    ));
                                }
                                (None, false)
                            }
                            PrecommitEvent::ResponseHead(_) => {
                                return Err(Arc::from(
                                    "response head cannot be a body emission source",
                                ));
                            }
                        }
                    }
                    EmittedBodyBacking::EndStreamControl => {
                        if !end_stream {
                            return Err(Arc::from(
                                "attempt-response filter emitted a non-terminal EOS control",
                            ));
                        }
                        (None, true)
                    }
                };
                charged.push_back((bytes, end_stream, lineage, queue_metadata, typed_end_stream));
            }
            let mut replacements: VecDeque<_> = pending
                .drain(..)
                .filter_map(|pending| {
                    matches!(pending.frame, PrecommitEvent::ResponseHead(_))
                        .then_some(pending.frame)
                })
                .collect();
            for (bytes, end_stream, lineage, _queue_metadata, typed_end_stream) in charged {
                if typed_end_stream {
                    replacements.push_back(PrecommitEvent::EndStream);
                    continue;
                }
                let [source] = lineage.as_slice() else {
                    return Err(Arc::from(
                        "attempt-response filter merged multiple source events; precommit merge is unsupported",
                    ));
                };
                let source = sources.get(source.value()).ok_or_else(|| {
                    Arc::from("attempt-response filter emitted an unknown source identity")
                })?;
                if end_stream {
                    if bytes.is_some() || !matches!(source, AttemptFilterSource::EndStream) {
                        return Err(Arc::from(
                            "end-stream filter emission changed its source ownership",
                        ));
                    }
                    replacements.push_back(PrecommitEvent::EndStream);
                } else if let Some(bytes) = bytes {
                    match source {
                        AttemptFilterSource::Body => {
                            replacements.push_back(PrecommitEvent::Body(bytes));
                        }
                        AttemptFilterSource::Sse {
                            sequence,
                            provenance,
                        } => replacements.push_back(PrecommitEvent::SseEvent {
                            sequence,
                            bytes,
                            provenance,
                        }),
                        AttemptFilterSource::EndStream => {
                            return Err(Arc::from(
                                "end-stream source emitted non-terminal body bytes",
                            ));
                        }
                    }
                }
            }
            BodyEmitterOutcome::replace(replacements)
        };
        let result = Self::resolve_outcome(machine, pending, outcome, frames);
        sources.reclaim_dead();
        Ok(result)
    }

    pub(super) fn accepted_emission_matches(
        pending: &VecDeque<PendingFilterFrame<AcceptedBodyFrame>>,
        emitted: &VecDeque<EmittedBodyFrame>,
    ) -> bool {
        pending.len() == emitted.len()
            && pending.iter().zip(emitted).all(|(input, emitted)| {
                input.source.as_ref().is_some_and(|source| {
                    emitted
                        .runtime_owner
                        .as_ref()
                        .is_some_and(|owner| owner.source() == source)
                }) && input.frame.end_stream == emitted.end_stream
                    && input
                        .frame
                        .output
                        .as_ref()
                        .map_or(emitted.bytes().is_empty(), |input| {
                            input.bytes.bytes().as_ref() == emitted.bytes()
                        })
            })
    }

    pub(super) fn resolve_accepted_frames(
        machine: &mut DirectionMachine,
        pending: &mut VecDeque<PendingFilterFrame<AcceptedBodyFrame>>,
        sources: &mut AcceptedFilterSourceLedger,
        budget: &StreamBudget,
        outcome: MachineOutcome,
    ) -> Result<GatewayFilterResult<AcceptedBodyFrame>, Arc<str>> {
        Self::apply_dropped_input_ownership(machine, pending);
        sources.reclaim_dead();
        if matches!(
            outcome,
            MachineOutcome::Paused(_) | MachineOutcome::LocalReply(_)
        ) {
            return Ok(Self::resolve_outcome(
                machine,
                pending,
                outcome,
                BodyEmitterOutcome::drop_input(),
            ));
        }
        let emitted = machine.take_emitted_body();
        sources.begin_batch();
        let frames = if Self::accepted_emission_matches(pending, &emitted) {
            for item in pending.iter() {
                if let Some(output) = item.frame.output.as_ref() {
                    let source = item.source.as_ref().ok_or_else(|| {
                        Arc::from("accepted filter input lost its source ownership")
                    })?;
                    sources
                        .stage_output(std::slice::from_ref(source), output.bytes.bytes().len())?;
                }
            }
            sources.commit_batch()?;
            BodyEmitterOutcome::replace(std::mem::take(pending).into_iter().map(|item| item.frame))
        } else {
            let mut replacements = VecDeque::with_capacity(emitted.len());
            for emitted in emitted {
                let EmittedBodyFrame {
                    backing,
                    end_stream,
                    runtime_owner,
                    sources: lineage,
                    queue_metadata,
                } = emitted;
                if lineage.as_slice().is_empty() {
                    return Err(Arc::from(
                        "accepted filter emitted output without source ownership",
                    ));
                }
                let referenced = || {
                    lineage.as_slice().iter().map(|source| {
                        sources.get(source.value()).ok_or_else(|| {
                            Arc::from("accepted filter emitted an unknown source identity")
                        })
                    })
                };
                let mut provenance = SemanticProvenance::NonSemantic;
                let mut contains_non_sse_payload = false;
                for source in referenced() {
                    let source = source?;
                    contains_non_sse_payload |= source.contains_non_sse_payload;
                    provenance = provenance.merge(source.provenance);
                }
                let sse_sources = SseTransformSources::merge(
                    budget,
                    lineage.as_slice().iter().map(|source| {
                        &sources
                            .get(source.value())
                            .expect("source identity was validated above")
                            .sse_sources
                    }),
                )?;
                if !sse_sources.as_slice().is_empty() && contains_non_sse_payload {
                    return Err(Arc::from(
                        "accepted SSE filter cannot merge SSE and ordinary body ownership",
                    ));
                }
                let (output, queue_metadata) = match backing {
                    EmittedBodyBacking::Replacement(bytes) => {
                        let output = if bytes.bytes().is_empty() {
                            None
                        } else {
                            sources.stage_output(lineage.as_slice(), bytes.bytes().len())?;
                            Some(EncodedOutputUnit {
                                bytes: bytes
                                    .transfer_role(MemoryRole::OutputQueue)
                                    .map_err(body_filter_error)?
                                    .with_metadata(queue_metadata.clone()),
                                provenance,
                            })
                        };
                        (output, queue_metadata)
                    }
                    EmittedBodyBacking::Forwarded(view) => {
                        let Some(runtime_owner) = runtime_owner.as_ref() else {
                            return Err(Arc::from(
                                "forwarded accepted output lost its runtime owner",
                            ));
                        };
                        let source = runtime_owner.source();
                        if !lineage.as_slice().contains(source) {
                            return Err(Arc::from(
                                "forwarded accepted output detached from its semantic source",
                            ));
                        }
                        let frame =
                            Self::take_pending_source(pending, source).ok_or_else(|| {
                                Arc::from("forwarded accepted output has an unknown source")
                            })?;
                        if frame.end_stream != end_stream
                            || frame.output.as_ref().map_or(!view.is_empty(), |output| {
                                output.bytes.bytes().as_ref() != view
                                    || output.provenance != provenance
                            })
                        {
                            return Err(Arc::from(
                                "forwarded accepted output changed its source owner",
                            ));
                        }
                        if let Some(output) = frame.output.as_ref() {
                            sources.stage_output(lineage.as_slice(), output.bytes.bytes().len())?;
                        }
                        (frame.output, frame.queue_metadata)
                    }
                    EmittedBodyBacking::EndStreamControl => {
                        if !end_stream {
                            return Err(Arc::from(
                                "accepted filter emitted a non-terminal EOS control",
                            ));
                        }
                        replacements.push_back(AcceptedBodyFrame {
                            output: None,
                            end_stream: true,
                            sse_sources: SseTransformSources::default(),
                            queue_metadata: BodyMetadataOwner::default(),
                        });
                        continue;
                    }
                };
                replacements.push_back(AcceptedBodyFrame {
                    output,
                    end_stream,
                    sse_sources: if end_stream {
                        SseTransformSources::default()
                    } else {
                        sse_sources
                    },
                    queue_metadata,
                });
            }
            sources.commit_batch()?;
            pending.clear();
            BodyEmitterOutcome::replace(replacements)
        };
        let result = Self::resolve_outcome(machine, pending, outcome, frames);
        sources.reclaim_dead();
        Ok(result)
    }

    pub(super) fn resolve_headers(
        machine: &DirectionMachine,
        outcome: MachineOutcome,
    ) -> GatewayFilterResult<()> {
        match outcome {
            MachineOutcome::Paused(_) => GatewayFilterResult::headers(None, machine.pause()),
            MachineOutcome::LocalReply(reply) => GatewayFilterResult::headers(Some(reply), None),
            MachineOutcome::Advanced | MachineOutcome::Complete => {
                GatewayFilterResult::headers(None, None)
            }
        }
    }

    pub(super) fn rewrite_attempt_head(
        machine: &DirectionMachine,
        frames: &mut VecDeque<PrecommitEvent>,
    ) {
        if let Some(PrecommitEvent::ResponseHead(head)) = frames
            .iter_mut()
            .find(|event| matches!(event, PrecommitEvent::ResponseHead(head) if !head.status().is_informational()))
        {
            *head.headers_mut() = machine.held_headers().clone();
        }
    }

    pub(super) fn finish_machine(machine: &mut Option<DirectionMachine>) {
        if let Some(machine) = machine {
            machine.finalize();
        }
        machine.take();
    }

    pub(super) async fn finish_machine_bounded(
        machine: &mut Option<DirectionMachine>,
        join_timeout: Duration,
    ) -> Result<(), Arc<str>> {
        if let Some(machine) = machine.as_mut() {
            machine
                .finalize_bounded(join_timeout)
                .await
                .map_err(filter_error)?;
        }
        machine.take();
        Ok(())
    }
}
