use super::*;

mod local_reply;

impl<S, P, F, T> GatewayCoreLifecycle<S, P, F, T>
where
    P: ProviderRuntimePort,
    S: SelectionPublicationPort<P::RouteRequestContext>,
    F: GatewayFilterManagerPort,
    T: AttemptTransportFactory,
{
    pub(super) async fn complete_accept(
        &self,
        attempt: PublishedAttempt<
            '_,
            S::Session,
            F::RequestFilters,
            P::AttemptState,
            P::Readiness,
            T::Transport,
            P::DecodedSseEvent,
        >,
    ) -> Result<AttemptCompletionOutcome, GatewayExecutionError> {
        if attempt.filter_local_reply.is_some() {
            return self.complete_accept_local_reply(attempt).await;
        }
        let PublishedAttempt {
            disposition,
            session,
            request_filters,
            cancellation,
            request_id,
            binding,
            request_configs,
            driver,
            final_writer,
            budget,
            deadline,
            run_filter_callbacks,
            decision_session,
            completed_attempts,
            selected,
            routing_facts,
            mut provider_state,
            mut readiness,
            provider_facts,
            attempt_telemetry,
            downstream_method,
            downstream_protocol,
            mut exchange,
            published,
            filter_local_reply,
            mut accepted_sse_handoff,
            mut sse_end_stream_pending,
            accepted_sse_limits,
            mut sse_framer,
            mut precommit_sse_mailbox,
            accepted_body_plan,
            precommit_event_capacity,
            connection_configs,
            attempt_configs,
            ..
        } = attempt;

        debug_assert!(filter_local_reply.is_none());

        // The precommit filter directions are no longer needed
        // after terminal publication. Join them before a possibly
        // long AcceptedResponse stream so their child work and
        // retained source ledgers do not live until downstream EOS.
        let attempt_filter_cleanup = finish_attempt_filter_scope_bounded(
            &mut request_filters.filters,
            self.limits.cleanup_timeout,
        )
        .await;
        let mut accepted_result: Result<SessionReuse, GatewayExecutionError> = async {
            let accepted_binding = binding.take_accepted_response()?;
            request_configs.retain_only(&accepted_binding.plan().config_cell_ids);
            driver.select_final_response()?;
            driver.begin_accepted_response()?;
            exchange.begin_accepted_response_scope()?;
            let has_accepted_filters =
                run_filter_callbacks && !accepted_binding.plan().filters.is_empty();
            if has_accepted_filters {
                let filter_phase = accepted_binding.acquire_phase_configs()?;
                let filter_event = accepted_binding.acquire_event_configs()?;
                let filter_phase_observation = attempt_telemetry.as_ref().map(|telemetry| {
                    ConfigLeaseObservation::acquire(
                        telemetry,
                        ConfigAcquireScope::Phase,
                        filter_phase.generations(),
                    )
                });
                let filter_event_observation = attempt_telemetry.as_ref().map(|telemetry| {
                    ConfigLeaseObservation::acquire(
                        telemetry,
                        ConfigAcquireScope::Event,
                        filter_event.generations(),
                    )
                });
                let filter_phase =
                    ObservedConfigSnapshot::new(filter_phase, filter_phase_observation);
                let filter_event =
                    ObservedConfigSnapshot::new(filter_event, filter_event_observation);
                let filter_configs = materialize_filter_configs(
                    &accepted_binding.plan().filters,
                    Some(&connection_configs),
                    request_configs,
                    Some(&attempt_configs),
                    &filter_phase,
                    &filter_event,
                )?;
                request_filters
                    .filters
                    .begin_accepted_response(
                        &accepted_binding.plan().filters,
                        filter_scope_context(
                            request_id,
                            binding.plan_revision(),
                            Some(selected.binding),
                            Some(selected.attempt_id),
                            Some(selected.generation),
                            driver
                                .scope_id(ScopeKind::AcceptedResponse)
                                .ok_or(GatewayExecutionError::MissingFilterScope)?,
                            ScopeKind::AcceptedResponse,
                            deadline,
                            cancellation,
                            attempt_telemetry.clone(),
                            budget,
                            filter_configs,
                            &self.filter_executors,
                        )
                        .with_accepted_body_plan(&accepted_body_plan),
                    )
                    .map_err(GatewayExecutionError::Filter)?;
            }
            let readiness = readiness
                .as_mut()
                .ok_or(GatewayExecutionError::AcceptedWithoutReadiness)?;
            let mut response_head = {
                let accepted_head_phase = accepted_binding.acquire_phase_configs()?;
                let accepted_head_observation = attempt_telemetry.as_ref().map(|telemetry| {
                    ConfigLeaseObservation::acquire(
                        telemetry,
                        ConfigAcquireScope::Phase,
                        accepted_head_phase.generations(),
                    )
                });
                let accepted_head_phase =
                    ObservedConfigSnapshot::new(accepted_head_phase, accepted_head_observation);
                let accepted_pinned_configs = PinnedConfigContext {
                    ids: &accepted_binding.plan().config_cell_ids,
                    connection: &connection_configs,
                    request: request_configs,
                    attempt: &attempt_configs,
                    phase: &accepted_head_phase,
                };
                self.provider
                    .accepted_response_head(
                        readiness,
                        &published,
                        &accepted_binding,
                        &accepted_pinned_configs,
                    )
                    .map_err(GatewayExecutionError::Provider)?
            };
            let original_content_length = response_head.headers.get(CONTENT_LENGTH).cloned();
            let original_transfer_encoding = response_head.headers.get(TRANSFER_ENCODING).cloned();
            let body_plan_buffered =
                matches!(&accepted_body_plan, BodyPlan::BufferedTransform { .. });
            let body_forbidden = response_body_forbidden(downstream_method, response_head.status);
            let mut accepted_body_owner = BodyPlanExecutor::new(
                BodyDirection::AcceptedResponse,
                accepted_body_plan.clone(),
                usize::MAX,
            )?;
            let mut buffered_frames: Vec<EncodedOutputUnit> = Vec::new();
            let mut framing = FramingLedger::default();
            if !body_plan_buffered && !matches!(&accepted_body_plan, BodyPlan::PassThrough { .. }) {
                framing.streaming_transform()?;
            }
            let accepted_head_result = if has_accepted_filters {
                let accepted_head_filter_started = Instant::now();
                if let (Some(telemetry), Some(scope_id)) = (
                    attempt_telemetry.as_ref(),
                    driver.scope_id(ScopeKind::AcceptedResponse),
                ) {
                    telemetry.scope(
                        ScopeKind::AcceptedResponse,
                        scope_id,
                        ScopePhase::Paused,
                        0,
                        Duration::ZERO,
                    );
                }
                let reply = await_request_operation(
                    session,
                    cancellation,
                    deadline,
                    attempt_telemetry.as_ref(),
                    async {
                        request_filters
                            .filters
                            .filter_accepted_head(&mut response_head)
                            .await
                            .map_err(GatewayExecutionError::Filter)
                    },
                )
                .await?;
                if let (Some(telemetry), Some(scope_id)) = (
                    attempt_telemetry.as_ref(),
                    driver.scope_id(ScopeKind::AcceptedResponse),
                ) {
                    telemetry.scope(
                        ScopeKind::AcceptedResponse,
                        scope_id,
                        ScopePhase::Headers,
                        0,
                        accepted_head_filter_started.elapsed(),
                    );
                }
                reply
            } else {
                GatewayFilterResult::headers(None, None)
            };
            if let Some(reply) = accepted_head_result.local_reply {
                finish_attempt_bounded(&mut exchange, self.limits.cleanup_timeout).await?;
                write_buffered_accepted_response(
                    session,
                    &mut *final_writer,
                    &mut request_filters.filters,
                    &accepted_binding,
                    request_configs,
                    reply,
                    budget,
                    downstream_method,
                    downstream_protocol,
                    false,
                    deadline,
                    cancellation,
                    attempt_telemetry.as_ref(),
                )
                .await?;
                return Ok(SessionReuse::Close);
            }
            // A stopped encoder header chain owns the response head
            // until body-driven or explicit continuation completes.
            // Keep the final writer behind that fence; P1-4's
            // publication validation guarantees a bounded plan.
            let mut accepted_pause = accepted_head_result.pause;
            let mut buffered = body_plan_buffered || accepted_pause.is_some();
            let mut committed_head = if buffered {
                None
            } else {
                record_framing_mutations(
                    &mut framing,
                    &response_head.headers,
                    original_content_length.as_ref(),
                    original_transfer_encoding.as_ref(),
                );
                framing.finalize(
                    &mut response_head.headers,
                    match downstream_protocol {
                        crate::transport::HttpProtocol::Http1 => HttpFraming::Http1,
                        crate::transport::HttpProtocol::Http2 => HttpFraming::Http2,
                    },
                    Some(downstream_method),
                    Some(response_head.status),
                )?;
                exchange.begin_downstream_header_write()?;
                final_writer.begin_header_write()?;
                let wire_head = if has_accepted_filters {
                    response_head.clone()
                } else {
                    GatewayResponseHead {
                        status: response_head.status,
                        headers: std::mem::take(&mut response_head.headers),
                    }
                };
                await_session_operation(
                    cancellation,
                    deadline,
                    attempt_telemetry.as_ref(),
                    async {
                        session
                            .write_response_head(wire_head)
                            .await
                            .map_err(GatewayExecutionError::Transport)
                    },
                )
                .await?;
                exchange.confirm_downstream_header_write()?;
                final_writer.confirm_header_write()?;
                has_accepted_filters.then(|| response_head.clone())
            };

            // The precommit marker queue no longer owns payloads;
            // release its metadata at the publication boundary.
            precommit_sse_mailbox.take();
            let (mut accepted_handoff, mut accepted_sse_next_sequence) = accepted_sse_handoff
                .take()
                .map_or((None, 0), |(handoff, next_sequence)| {
                    (Some(handoff), next_sequence)
                });
            let mut accepted_sse_mailbox = if sse_framer.is_some() {
                Some(BudgetedResponseMailbox::<PrecommitEvent>::new_budgeted(
                    precommit_event_capacity,
                    budget,
                )?)
            } else {
                None
            };

            let mut filtered_accepted_frames = VecDeque::new();
            let mut accepted_source_eos = false;
            let accepted_sse_output_units = accepted_binding
                .plan()
                .filters
                .iter()
                .filter(|filter| filter.capabilities().expands_body())
                .map(|filter| filter.max_pending_frames)
                .min()
                .unwrap_or(1);
            loop {
                if buffered && !body_plan_buffered && accepted_pause.is_none() {
                    record_framing_mutations(
                        &mut framing,
                        &response_head.headers,
                        original_content_length.as_ref(),
                        original_transfer_encoding.as_ref(),
                    );
                    framing.finalize(
                        &mut response_head.headers,
                        match downstream_protocol {
                            crate::transport::HttpProtocol::Http1 => HttpFraming::Http1,
                            crate::transport::HttpProtocol::Http2 => HttpFraming::Http2,
                        },
                        Some(downstream_method),
                        Some(response_head.status),
                    )?;
                    exchange.begin_downstream_header_write()?;
                    final_writer.begin_header_write()?;
                    await_session_operation(
                        cancellation,
                        deadline,
                        attempt_telemetry.as_ref(),
                        async {
                            session
                                .write_response_head(response_head.clone())
                                .await
                                .map_err(GatewayExecutionError::Transport)
                        },
                    )
                    .await?;
                    exchange.confirm_downstream_header_write()?;
                    final_writer.confirm_header_write()?;
                    committed_head = Some(response_head.clone());
                    for output in buffered_frames.drain(..) {
                        let semantic = output.provenance == SemanticProvenance::ProducesSemantic
                            && !output.bytes.bytes().is_empty();
                        if semantic
                            && exchange.snapshot().downstream_semantic_fence
                                == crate::runtime::attempt::CommitFence::Clear
                        {
                            exchange.begin_semantic_output_write()?;
                        }
                        if semantic && final_writer.semantic_fence() == CommitFence::Clear {
                            final_writer.begin_semantic_write()?;
                        }
                        await_session_operation(
                            cancellation,
                            deadline,
                            attempt_telemetry.as_ref(),
                            async {
                                session
                                    .write_response_body_charged(
                                        (!body_forbidden).then_some(output.bytes),
                                        false,
                                    )
                                    .await
                                    .map_err(GatewayExecutionError::Transport)
                            },
                        )
                        .await?;
                        if semantic
                            && exchange.snapshot().downstream_semantic_fence
                                == crate::runtime::attempt::CommitFence::WriteStartedMayHaveCommitted
                        {
                            exchange.confirm_semantic_output_write()?;
                        }
                        if semantic
                            && final_writer.semantic_fence()
                                == CommitFence::WriteStartedMayHaveCommitted
                        {
                            final_writer.confirm_semantic_write()?;
                        }
                    }
                    buffered = false;
                }

                if filtered_accepted_frames.is_empty() {
                    let resumed = match accepted_pause {
                        Some(FilterPause::Watermark) => Some(
                            await_request_operation(
                                session,
                                cancellation,
                                deadline,
                                attempt_telemetry.as_ref(),
                                async {
                                    request_filters
                                        .filters
                                        .wait_accepted_resume(&mut response_head)
                                        .await
                                        .map_err(GatewayExecutionError::Filter)
                                },
                            )
                            .await?,
                        ),
                        Some(FilterPause::Buffer) if accepted_source_eos => Some(
                            await_request_operation(
                                session,
                                cancellation,
                                deadline,
                                attempt_telemetry.as_ref(),
                                async {
                                    request_filters
                                        .filters
                                        .wait_accepted_resume(&mut response_head)
                                        .await
                                        .map_err(GatewayExecutionError::Filter)
                                },
                            )
                            .await?,
                        ),
                        Some(FilterPause::Buffer) => {
                            await_request_operation(
                                session,
                                cancellation,
                                deadline,
                                attempt_telemetry.as_ref(),
                                async {
                                    request_filters
                                        .filters
                                        .try_accepted_resume(&mut response_head)
                                        .await
                                        .map_err(GatewayExecutionError::Filter)
                                },
                            )
                            .await?
                        }
                        Some(FilterPause::HeaderIteration) if accepted_source_eos => {
                            return Err(GatewayExecutionError::Filter(Arc::from(
                                "accepted header iteration remained paused after EOS",
                            )));
                        }
                        Some(FilterPause::HeaderIteration) | None => None,
                    };
                    if let Some(resumed) = resumed {
                        if let Some(reply) = resumed.local_reply {
                            finish_attempt_bounded(&mut exchange, self.limits.cleanup_timeout)
                                .await?;
                            if buffered {
                                write_buffered_accepted_response(
                                    session,
                                    &mut *final_writer,
                                    &mut request_filters.filters,
                                    &accepted_binding,
                                    request_configs,
                                    reply,
                                    budget,
                                    downstream_method,
                                    downstream_protocol,
                                    false,
                                    deadline,
                                    cancellation,
                                    attempt_telemetry.as_ref(),
                                )
                                .await?;
                                return Ok(SessionReuse::Close);
                            }
                            return Err(GatewayExecutionError::AcceptedBodyLocalReplyAfterCommit);
                        }
                        if let Some(limits) = accepted_sse_limits.as_ref() {
                            validate_sse_transform_batch(
                                limits,
                                &resumed.frames,
                                accepted_sse_output_units,
                                accepted_binding.plan().semantic_replacement_authorized,
                            )?;
                        }
                        filtered_accepted_frames.extend(resumed.frames);
                        accepted_pause = resumed.pause;
                        continue;
                    }
                }

                let mut frame = if let Some(ready) = filtered_accepted_frames.pop_front() {
                    ready
                } else {
                    let provider_prefix = self
                        .provider
                        .take_accepted_prefix(readiness)
                        .map_err(GatewayExecutionError::Provider)?;
                    let (event, sse_source) = if let Some(event) = provider_prefix {
                        (event, None)
                    } else if let Some(event) = accepted_handoff.as_mut().and_then(Iterator::next) {
                        match event {
                            AcceptedEvent::NeedsDecode {
                                sequence,
                                raw,
                                provenance,
                            } => {
                                let source_bytes = raw.raw().len();
                                (
                                    ProviderAcceptedEvent::Raw(PrecommitEvent::SseEvent {
                                        sequence,
                                        bytes: raw
                                            .into_raw()
                                            .transfer_role(MemoryRole::ResponsePrefix)?,
                                        provenance,
                                    }),
                                    Some(SseTransformSource {
                                        sequence,
                                        source_bytes,
                                        provenance,
                                    }),
                                )
                            }
                            AcceptedEvent::Decoded {
                                sequence,
                                decoded,
                                source_bytes,
                                provenance,
                            } => (
                                ProviderAcceptedEvent::DecodedSse {
                                    sequence,
                                    decoded,
                                    provenance,
                                },
                                Some(SseTransformSource {
                                    sequence,
                                    source_bytes,
                                    provenance,
                                }),
                            ),
                        }
                    } else {
                        let raw_event = if let Some(event) = accepted_sse_mailbox
                            .as_mut()
                            .and_then(BudgetedResponseMailbox::pop)
                        {
                            event
                        } else if sse_framer.as_ref().is_some_and(SseFramer::needs_drain) {
                            resume_accepted_sse(
                                sse_framer
                                    .as_mut()
                                    .expect("checked deferred accepted framer"),
                                budget,
                                &mut accepted_sse_next_sequence,
                                accepted_sse_mailbox
                                    .as_mut()
                                    .expect("deferred accepted framer has event mailbox"),
                            )?;
                            continue;
                        } else if sse_end_stream_pending {
                            sse_end_stream_pending = false;
                            PrecommitEvent::EndStream
                        } else {
                            let raw_event = if let Some(event) = exchange.next_precommit_event() {
                                event
                            } else {
                                await_attempt_operation(
                                    session,
                                    cancellation,
                                    deadline,
                                    attempt_telemetry.as_ref(),
                                    exchange.wait_precommit_event(deadline),
                                )
                                .await?
                                .ok_or(GatewayExecutionError::ResponseEndedWithoutEos)?
                            };
                            if let (Some(framer), Some(mailbox)) =
                                (sse_framer.as_mut(), accepted_sse_mailbox.as_mut())
                            {
                                match raw_event {
                                    PrecommitEvent::Body(bytes) => {
                                        feed_accepted_sse(
                                            framer,
                                            bytes,
                                            false,
                                            budget,
                                            &mut accepted_sse_next_sequence,
                                            mailbox,
                                        )?;
                                        continue;
                                    }
                                    PrecommitEvent::EndStream => {
                                        feed_accepted_sse(
                                            framer,
                                            ChargedBytes::copy_from_opaque(
                                                budget,
                                                MemoryRole::ResponsePrefix,
                                                &[],
                                            )?,
                                            true,
                                            budget,
                                            &mut accepted_sse_next_sequence,
                                            mailbox,
                                        )?;
                                        sse_end_stream_pending = true;
                                        continue;
                                    }
                                    event => event,
                                }
                            } else {
                                raw_event
                            }
                        };
                        let sse_source = match &raw_event {
                            PrecommitEvent::SseEvent {
                                sequence,
                                bytes,
                                provenance,
                            } => Some(SseTransformSource {
                                sequence: *sequence,
                                source_bytes: bytes.bytes().len(),
                                provenance: *provenance,
                            }),
                            _ => None,
                        };
                        (ProviderAcceptedEvent::Raw(raw_event), sse_source)
                    };
                    let frame = {
                        let accepted_event_phase = accepted_binding.acquire_phase_configs()?;
                        let accepted_event_configs = accepted_binding.acquire_event_configs()?;
                        let accepted_event_phase_observation =
                            attempt_telemetry.as_ref().map(|telemetry| {
                                ConfigLeaseObservation::acquire(
                                    telemetry,
                                    ConfigAcquireScope::Phase,
                                    accepted_event_phase.generations(),
                                )
                            });
                        let accepted_event_observation =
                            attempt_telemetry.as_ref().map(|telemetry| {
                                ConfigLeaseObservation::acquire(
                                    telemetry,
                                    ConfigAcquireScope::Event,
                                    accepted_event_configs.generations(),
                                )
                            });
                        let accepted_event_phase = ObservedConfigSnapshot::new(
                            accepted_event_phase,
                            accepted_event_phase_observation,
                        );
                        let accepted_event_configs = ObservedConfigSnapshot::new(
                            accepted_event_configs,
                            accepted_event_observation,
                        );
                        let accepted_pinned_configs = PinnedConfigContext {
                            ids: &accepted_binding.plan().config_cell_ids,
                            connection: &connection_configs,
                            request: request_configs,
                            attempt: &attempt_configs,
                            phase: &accepted_event_phase,
                        };
                        self.provider
                            .encode_accepted_event(
                                readiness,
                                event,
                                &published,
                                &accepted_binding,
                                &accepted_pinned_configs,
                                &accepted_event_configs,
                            )
                            .map_err(GatewayExecutionError::Provider)?
                    };
                    let Some(mut frame) = frame else {
                        continue;
                    };
                    frame.sse_sources =
                        sse_source.map(SseTransformSources::one).unwrap_or_default();
                    accepted_source_eos |= frame.end_stream;
                    let filtered = if has_accepted_filters {
                        let filter_configs = {
                            let filter_phase = accepted_binding.acquire_phase_configs()?;
                            let filter_event = accepted_binding.acquire_event_configs()?;
                            materialize_filter_configs(
                                &accepted_binding.plan().filters,
                                Some(&connection_configs),
                                request_configs,
                                Some(&attempt_configs),
                                &filter_phase,
                                &filter_event,
                            )?
                        };
                        let accepted_body_filter_started = Instant::now();
                        if let (Some(telemetry), Some(scope_id)) = (
                            attempt_telemetry.as_ref(),
                            driver.scope_id(ScopeKind::AcceptedResponse),
                        ) {
                            telemetry.scope(
                                ScopeKind::AcceptedResponse,
                                scope_id,
                                ScopePhase::Paused,
                                0,
                                Duration::ZERO,
                            );
                        }
                        let result = await_request_operation(
                            session,
                            cancellation,
                            deadline,
                            attempt_telemetry.as_ref(),
                            async {
                                request_filters
                                    .filters
                                    .filter_accepted_body(&mut response_head, frame, filter_configs)
                                    .await
                                    .map_err(GatewayExecutionError::Filter)
                            },
                        )
                        .await?;
                        if let (Some(telemetry), Some(scope_id)) = (
                            attempt_telemetry.as_ref(),
                            driver.scope_id(ScopeKind::AcceptedResponse),
                        ) {
                            telemetry.scope(
                                ScopeKind::AcceptedResponse,
                                scope_id,
                                ScopePhase::Body,
                                0,
                                accepted_body_filter_started.elapsed(),
                            );
                        }
                        result
                    } else {
                        GatewayFilterResult::forward(frame)
                    };
                    if let Some(reply) = filtered.local_reply {
                        finish_attempt_bounded(&mut exchange, self.limits.cleanup_timeout).await?;
                        if buffered {
                            write_buffered_accepted_response(
                                session,
                                &mut *final_writer,
                                &mut request_filters.filters,
                                &accepted_binding,
                                request_configs,
                                reply,
                                budget,
                                downstream_method,
                                downstream_protocol,
                                false,
                                deadline,
                                cancellation,
                                attempt_telemetry.as_ref(),
                            )
                            .await?;
                            return Ok(SessionReuse::Close);
                        }
                        return Err(GatewayExecutionError::AcceptedBodyLocalReplyAfterCommit);
                    }
                    accepted_pause = filtered.pause;
                    if let Some(limits) = accepted_sse_limits.as_ref() {
                        validate_sse_transform_batch(
                            limits,
                            &filtered.frames,
                            accepted_sse_output_units,
                            accepted_binding.plan().semantic_replacement_authorized,
                        )?;
                    }
                    filtered_accepted_frames.extend(filtered.frames);
                    let Some(ready) = filtered_accepted_frames.pop_front() else {
                        continue;
                    };
                    ready
                };
                if let Some(output) = frame.output.as_ref() {
                    if output.bytes.role() != MemoryRole::OutputQueue {
                        return Err(GatewayExecutionError::Body(BodyError::WrongMemoryRole));
                    }
                    accepted_body_owner.admit_chunk(output.bytes.bytes().len())?;
                }
                if let Some(committed) = &committed_head
                    && (response_head.status != committed.status
                        || response_head.headers != committed.headers)
                {
                    return Err(GatewayExecutionError::AcceptedHeaderMutationAfterCommit);
                }
                let has_semantic_bytes = !body_forbidden
                    && frame.output.as_ref().is_some_and(|output| {
                        output.provenance == SemanticProvenance::ProducesSemantic
                            && !output.bytes.bytes().is_empty()
                    });
                if buffered {
                    if !body_forbidden && let Some(output) = frame.output.take() {
                        buffered_frames.push(output);
                    }
                } else {
                    if has_semantic_bytes
                        && exchange.snapshot().downstream_semantic_fence
                            == crate::runtime::attempt::CommitFence::Clear
                    {
                        exchange.begin_semantic_output_write()?;
                    }
                    if has_semantic_bytes && final_writer.semantic_fence() == CommitFence::Clear {
                        final_writer.begin_semantic_write()?;
                    }
                    let wire_body = if body_forbidden {
                        frame.output.take();
                        None
                    } else {
                        frame.output.take().map(|output| output.bytes)
                    };
                    await_session_operation(
                        cancellation,
                        deadline,
                        attempt_telemetry.as_ref(),
                        async {
                            session
                                .write_response_body_charged(wire_body, frame.end_stream)
                                .await
                                .map_err(GatewayExecutionError::Transport)
                        },
                    )
                    .await?;
                    if has_semantic_bytes
                        && exchange.snapshot().downstream_semantic_fence
                            == crate::runtime::attempt::CommitFence::WriteStartedMayHaveCommitted
                    {
                        exchange.confirm_semantic_output_write()?;
                    }
                    if has_semantic_bytes
                        && final_writer.semantic_fence()
                            == CommitFence::WriteStartedMayHaveCommitted
                    {
                        final_writer.confirm_semantic_write()?;
                    }
                }
                if frame.end_stream && accepted_pause.is_none() {
                    break;
                }
            }
            let accepted_bytes = accepted_body_owner.finish()?;
            if let Some(telemetry) = attempt_telemetry.as_ref() {
                let queue_high_water_bytes = budget
                    .snapshot()
                    .map(|snapshot| snapshot.role_peak[MemoryRole::OutputQueue as usize])
                    .unwrap_or(0);
                telemetry.body(
                    BodyDirection::AcceptedResponse,
                    &accepted_body_plan,
                    accepted_bytes,
                    queue_high_water_bytes,
                );
            }
            if buffered {
                let exact = buffered_frames
                    .iter()
                    .try_fold(0_usize, |total, output| {
                        total.checked_add(output.bytes.bytes().len())
                    })
                    .ok_or(BodyError::BodyLimitExceeded)?;
                record_framing_mutations(
                    &mut framing,
                    &response_head.headers,
                    original_content_length.as_ref(),
                    original_transfer_encoding.as_ref(),
                );
                framing.buffered_eos(exact)?;
                framing.finalize(
                    &mut response_head.headers,
                    match downstream_protocol {
                        crate::transport::HttpProtocol::Http1 => HttpFraming::Http1,
                        crate::transport::HttpProtocol::Http2 => HttpFraming::Http2,
                    },
                    Some(downstream_method),
                    Some(response_head.status),
                )?;
                exchange.begin_downstream_header_write()?;
                final_writer.begin_header_write()?;
                await_session_operation(
                    cancellation,
                    deadline,
                    attempt_telemetry.as_ref(),
                    async {
                        session
                            .write_response_head(response_head)
                            .await
                            .map_err(GatewayExecutionError::Transport)
                    },
                )
                .await?;
                exchange.confirm_downstream_header_write()?;
                final_writer.confirm_header_write()?;
                if buffered_frames.is_empty() {
                    await_session_operation(
                        cancellation,
                        deadline,
                        attempt_telemetry.as_ref(),
                        async {
                            session
                                .write_response_body(Bytes::new(), true)
                                .await
                                .map_err(GatewayExecutionError::Transport)
                        },
                    )
                    .await?;
                } else {
                    let last = buffered_frames.len().saturating_sub(1);
                    for (index, output) in buffered_frames.into_iter().enumerate() {
                        let semantic = output.provenance == SemanticProvenance::ProducesSemantic
                            && !output.bytes.bytes().is_empty();
                        if semantic
                            && exchange.snapshot().downstream_semantic_fence
                                == crate::runtime::attempt::CommitFence::Clear
                        {
                            exchange.begin_semantic_output_write()?;
                        }
                        if semantic && final_writer.semantic_fence() == CommitFence::Clear {
                            final_writer.begin_semantic_write()?;
                        }
                        await_session_operation(
                            cancellation,
                            deadline,
                            attempt_telemetry.as_ref(),
                            async {
                                session
                                    .write_response_body_charged(Some(output.bytes), index == last)
                                    .await
                                    .map_err(GatewayExecutionError::Transport)
                            },
                        )
                        .await?;
                        if semantic
                        && exchange.snapshot().downstream_semantic_fence
                            == crate::runtime::attempt::CommitFence::WriteStartedMayHaveCommitted
                    {
                        exchange.confirm_semantic_output_write()?;
                    }
                        if semantic
                            && final_writer.semantic_fence()
                                == CommitFence::WriteStartedMayHaveCommitted
                        {
                            final_writer.confirm_semantic_write()?;
                        }
                    }
                }
            }
            Ok(SessionReuse::Reusable)
        }
        .await;
        // Once the downstream EOS write above succeeds, the business response
        // is delivered. Upstream release and filter joins remain bounded, but
        // are cleanup facts and must not race the client-disconnect waiter into
        // rewriting that delivery as a cancelled/transport-failed response.
        let accepted_release = if accepted_result.is_ok() && !exchange.snapshot().finalized {
            finish_accepted_response_bounded(
                &mut exchange,
                !cancellation.is_cancelled(),
                self.limits.cleanup_timeout,
            )
            .await
        } else {
            Ok(())
        };
        let exchange_cleanup = if exchange.snapshot().finalized {
            Ok(())
        } else {
            if accepted_result.is_err() {
                exchange.cancellation_token().cancel();
            }
            finish_attempt_bounded(&mut exchange, self.limits.cleanup_timeout).await
        };
        let accepted_filter_cleanup = finish_accepted_filter_scope_bounded(
            &mut request_filters.filters,
            self.limits.cleanup_timeout,
        )
        .await;
        let cleanup_result = combine_cleanup_results(
            accepted_release,
            combine_cleanup_results(
                exchange_cleanup,
                combine_cleanup_results(attempt_filter_cleanup, accepted_filter_cleanup),
            ),
        );
        let cleanup = if cleanup_result.is_ok() {
            AttemptCleanupOutcome::Completed
        } else {
            AttemptCleanupOutcome::Failed
        };
        if accepted_result.is_ok() && (cleanup_result.is_err() || cancellation.is_cancelled()) {
            accepted_result = Ok(SessionReuse::Close);
        }
        let completion = attempt_completion_from_result(
            disposition,
            attempt_commit_facts(&exchange, final_writer),
            exchange.transport_facts(),
            cleanup,
            &accepted_result,
        );
        let final_provider = self
            .provider
            .finalize_attempt_facts(
                &mut provider_state,
                readiness.as_mut(),
                Some(&provider_facts),
                &completion,
            )
            .ok()
            .filter(|facts| validate_provider_facts(facts).is_ok())
            .or_else(|| Some(provider_facts.clone()));
        record_completed_attempt(
            decision_session,
            completed_attempts,
            &selected,
            &routing_facts,
            final_provider,
            None,
            completion,
        );
        driver.complete();
        accepted_result.map(AttemptCompletionOutcome::Completed)
    }
}
