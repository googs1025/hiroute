// The progress owner deliberately keeps every associated port type concrete;
// type erasure here would add allocation and obscure linear ownership.
#![allow(clippy::type_complexity)]

use super::*;

pub(super) mod failure;

pub(super) struct AttemptProgressContext<
    'a,
    L,
    D,
    R: GatewayRequestFilterPort,
    A,
    X: AttemptTransport,
    E,
> {
    pub(super) session: &'a mut dyn GatewaySession,
    pub(super) request_filters: &'a mut GatewayFilterRequestOwner<R>,
    pub(super) cancellation: &'a CancellationToken,
    pub(super) deadline: Instant,
    pub(super) attempt_deadline: Instant,
    pub(super) attempt_telemetry: Option<RequestTelemetry>,
    pub(super) exchange: &'a mut AttemptExchange<X>,
    pub(super) attempt_request_local_reply: &'a mut Option<LocalReply>,
    pub(super) has_attempt_response_filters: bool,
    pub(super) attempt_binding: &'a AttemptExecutionBinding,
    pub(super) connection_configs: &'a ObservedConfigSnapshot<ConfigScopeSnapshot>,
    pub(super) request_configs: &'a ObservedConfigSnapshot<RequestConfigSnapshot>,
    pub(super) attempt_configs: &'a ObservedConfigSnapshot<ConfigScopeSnapshot>,
    pub(super) driver: &'a mut LogicalRequestDriver,
    pub(super) budget: &'a StreamBudget,
    pub(super) sse_framer: &'a mut Option<SseFramer>,
    pub(super) sse_handoff: &'a mut Option<PrecommitResponseState<E>>,
    pub(super) precommit_sse_mailbox: &'a mut Option<BudgetedResponseMailbox<u64>>,
    pub(super) provider_state: &'a mut A,
    pub(super) decision_session: &'a mut D,
    pub(super) selected: &'a SelectedGatewayAttempt,
    pub(super) leases: &'a RequestLeaseBook,
    pub(super) logical: &'a mut Option<L>,
}

pub(super) struct AttemptProgressReady<R, E> {
    pub(super) disposition: Disposition,
    pub(super) published: PublishedDisposition,
    pub(super) readiness: Option<R>,
    pub(super) provider_facts: ProviderClassificationFacts,
    pub(super) filter_local_reply: Option<LocalReply>,
    pub(super) accepted_sse_handoff: Option<(AcceptedHandoff<E>, u64)>,
    pub(super) sse_end_stream_pending: bool,
}

pub(super) struct AttemptProgress<R, E> {
    pub(super) staged_provider_facts: Option<ProviderClassificationFacts>,
    pub(super) staged_readiness: Option<R>,
    pub(super) result: Result<AttemptProgressReady<R, E>, GatewayExecutionError>,
}

impl<S, P, F, T> GatewayCoreLifecycle<S, P, F, T>
where
    P: ProviderRuntimePort,
    S: SelectionPublicationPort<P::RouteRequestContext>,
    F: GatewayFilterManagerPort,
    T: AttemptTransportFactory,
{
    pub(super) async fn progress_attempt(
        &self,
        context: AttemptProgressContext<
            '_,
            P::LogicalRequest,
            S::Session,
            F::RequestFilters,
            P::AttemptState,
            T::Transport,
            P::DecodedSseEvent,
        >,
    ) -> AttemptProgress<P::Readiness, P::DecodedSseEvent> {
        let AttemptProgressContext {
            session,
            request_filters,
            cancellation,
            deadline,
            attempt_deadline,
            attempt_telemetry,
            exchange,
            attempt_request_local_reply,
            has_attempt_response_filters,
            attempt_binding,
            connection_configs,
            request_configs,
            attempt_configs,
            driver,
            budget,
            sse_framer,
            sse_handoff,
            precommit_sse_mailbox,
            provider_state,
            decision_session,
            selected,
            leases,
            logical,
        } = context;
        let plan = attempt_binding.plan();
        let mut sse_end_stream_pending = false;
        let mut filtered_attempt_events = VecDeque::new();
        let mut filter_owned_sse_sequences = HashSet::new();
        let mut attempt_response_pause = None;
        // Core retains both opaque readiness and its bounded facts as soon
        // as classification succeeds. DecisionSession only borrows facts;
        // fallible gate/handoff/release work cannot consume this state.
        let mut staged_provider_facts = None;
        let mut staged_readiness = None;
        let result = async {
            let decision = if let Some(reply) = attempt_request_local_reply.take() {
                AttemptDecision::FilterLocalReply(reply)
            } else {
                loop {
                    if Instant::now() >= attempt_deadline {
                        exchange.mark_attempt_deadline_exceeded();
                        observe_runtime_error(attempt_telemetry.as_ref(), ErrorClass::Deadline);
                        return Err(GatewayExecutionError::Attempt(
                            AttemptError::DeadlineExceeded,
                        ));
                    }
                    if filtered_attempt_events.is_empty() {
                        let resumed = match attempt_response_pause {
                            Some(FilterPause::Watermark) => Some(
                                await_request_operation(
                                    session,
                                    cancellation,
                                    deadline,
                                    attempt_telemetry.as_ref(),
                                    async {
                                        request_filters
                                            .filters
                                            .wait_attempt_response_resume()
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
                                            .try_attempt_response_resume()
                                            .await
                                            .map_err(GatewayExecutionError::Filter)
                                    },
                                )
                                .await?
                            }
                            Some(FilterPause::HeaderIteration) | None => None,
                        };
                        if let Some(resumed) = resumed {
                            if let Some(reply) = resumed.local_reply {
                                break AttemptDecision::FilterLocalReply(reply);
                            }
                            for sequence in resumed.frames.iter().filter_map(|event| match event {
                                PrecommitEvent::SseEvent { sequence, .. } => Some(*sequence),
                                _ => None,
                            }) {
                                filter_owned_sse_sequences.remove(&sequence);
                            }
                            if resumed.pause.is_none() {
                                let dropped =
                                    filter_owned_sse_sequences.drain().collect::<Vec<_>>();
                                if let Some(handoff) = sse_handoff.as_mut() {
                                    for sequence in dropped {
                                        handoff.drop_inflight(sequence)?;
                                    }
                                }
                            }
                            filtered_attempt_events.extend(resumed.frames);
                            attempt_response_pause = resumed.pause;
                            if !filtered_attempt_events.is_empty()
                                || attempt_response_pause == Some(FilterPause::Watermark)
                            {
                                continue;
                            }
                        }
                    }
                    let (event, needs_filter) =
                        if let Some(event) = filtered_attempt_events.pop_front() {
                            (event, false)
                        } else if let Some(sequence) = precommit_sse_mailbox
                            .as_mut()
                            .and_then(BudgetedResponseMailbox::pop)
                        {
                            let (raw, provenance) = sse_handoff
                                .as_mut()
                                .expect("precommit SSE mailbox has a handoff owner")
                                .take_raw_for_classification(sequence)?;
                            let bytes = raw.into_raw().transfer_role(MemoryRole::ResponsePrefix)?;
                            (
                                PrecommitEvent::SseEvent {
                                    sequence,
                                    bytes,
                                    provenance,
                                },
                                true,
                            )
                        } else if sse_framer.as_ref().is_some_and(SseFramer::needs_drain) {
                            resume_precommit_sse(
                                sse_framer
                                    .as_mut()
                                    .expect("checked deferred precommit framer"),
                                budget,
                                sse_handoff
                                    .as_mut()
                                    .expect("deferred precommit framer has handoff owner"),
                                precommit_sse_mailbox
                                    .as_mut()
                                    .expect("deferred precommit framer has marker mailbox"),
                            )?;
                            continue;
                        } else if sse_end_stream_pending {
                            sse_end_stream_pending = false;
                            (PrecommitEvent::EndStream, true)
                        } else {
                            await_attempt_operation(
                                session,
                                cancellation,
                                deadline,
                                attempt_telemetry.as_ref(),
                                exchange.drive_writer_once(),
                            )
                            .await?;
                            let raw_event = if let Some(event) = exchange.next_precommit_event() {
                                event
                            } else if matches!(
                                exchange.snapshot().writer_state,
                                WriterState::QuiescedNormalEos
                            ) {
                                await_attempt_operation(
                                    session,
                                    cancellation,
                                    deadline,
                                    attempt_telemetry.as_ref(),
                                    exchange.wait_precommit_event(attempt_deadline),
                                )
                                .await?
                                .ok_or(GatewayExecutionError::ResponseEndedWithoutDisposition)?
                            } else {
                                continue;
                            };
                            let event = if let (Some(framer), Some(handoff), Some(mailbox)) = (
                                sse_framer.as_mut(),
                                sse_handoff.as_mut(),
                                precommit_sse_mailbox.as_mut(),
                            ) {
                                match raw_event {
                                    PrecommitEvent::Body(bytes) => {
                                        feed_precommit_sse(
                                            framer, bytes, false, budget, handoff, mailbox,
                                        )?;
                                        continue;
                                    }
                                    PrecommitEvent::EndStream => {
                                        feed_precommit_sse(
                                            framer,
                                            ChargedBytes::copy_from_opaque(
                                                budget,
                                                MemoryRole::ResponsePrefix,
                                                &[],
                                            )?,
                                            true,
                                            budget,
                                            handoff,
                                            mailbox,
                                        )?;
                                        sse_end_stream_pending = true;
                                        continue;
                                    }
                                    event => event,
                                }
                            } else {
                                raw_event
                            };
                            (event, true)
                        };
                    let filter_input_sequence = match &event {
                        PrecommitEvent::SseEvent { sequence, .. } => Some(*sequence),
                        _ => None,
                    };
                    let filtered = if has_attempt_response_filters && needs_filter {
                        if let Some(sequence) = filter_input_sequence {
                            filter_owned_sse_sequences.insert(sequence);
                        }
                        let filter_configs = {
                            let filter_phase = attempt_binding.acquire_phase_configs()?;
                            let filter_event = attempt_binding.acquire_event_configs()?;
                            materialize_filter_configs(
                                &plan.attempt_response_filters,
                                Some(connection_configs),
                                request_configs,
                                Some(attempt_configs),
                                &filter_phase,
                                &filter_event,
                            )?
                        };
                        let attempt_filter_started = Instant::now();
                        if let (Some(telemetry), Some(scope_id)) = (
                            attempt_telemetry.as_ref(),
                            driver.scope_id(ScopeKind::RouteAttempt),
                        ) {
                            telemetry.scope(
                                ScopeKind::RouteAttempt,
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
                                    .filter_attempt_response_event(event, filter_configs)
                                    .await
                                    .map_err(GatewayExecutionError::Filter)
                            },
                        )
                        .await?;
                        if let (Some(telemetry), Some(scope_id)) = (
                            attempt_telemetry.as_ref(),
                            driver.scope_id(ScopeKind::RouteAttempt),
                        ) {
                            telemetry.scope(
                                ScopeKind::RouteAttempt,
                                scope_id,
                                ScopePhase::Body,
                                0,
                                attempt_filter_started.elapsed(),
                            );
                        }
                        reply
                    } else {
                        GatewayFilterResult::forward(event)
                    };
                    let GatewayFilterResult {
                        frames,
                        local_reply,
                        pause,
                    } = filtered;
                    if let Some(reply) = local_reply {
                        break AttemptDecision::FilterLocalReply(reply);
                    }
                    attempt_response_pause = pause;
                    if has_attempt_response_filters && needs_filter {
                        for sequence in frames.iter().filter_map(|event| match event {
                            PrecommitEvent::SseEvent { sequence, .. } => Some(*sequence),
                            _ => None,
                        }) {
                            filter_owned_sse_sequences.remove(&sequence);
                        }
                        if pause.is_none() {
                            let dropped = filter_owned_sse_sequences.drain().collect::<Vec<_>>();
                            if let Some(handoff) = sse_handoff.as_mut() {
                                for sequence in dropped {
                                    handoff.drop_inflight(sequence)?;
                                }
                            }
                        }
                    }
                    filtered_attempt_events.extend(frames);
                    if filtered_attempt_events.is_empty() {
                        continue;
                    }
                    let event = filtered_attempt_events
                        .pop_front()
                        .expect("checked filtered attempt output");
                    let classified_sequence = match &event {
                        PrecommitEvent::SseEvent { sequence, .. } => Some(*sequence),
                        _ => None,
                    };
                    let classification = {
                        let classify_phase_configs = attempt_binding.acquire_phase_configs()?;
                        let event_configs = attempt_binding.acquire_event_configs()?;
                        let classify_phase_observation =
                            attempt_telemetry.as_ref().map(|telemetry| {
                                ConfigLeaseObservation::acquire(
                                    telemetry,
                                    ConfigAcquireScope::Phase,
                                    classify_phase_configs.generations(),
                                )
                            });
                        let event_observation = attempt_telemetry.as_ref().map(|telemetry| {
                            ConfigLeaseObservation::acquire(
                                telemetry,
                                ConfigAcquireScope::Event,
                                event_configs.generations(),
                            )
                        });
                        let classify_phase_configs = ObservedConfigSnapshot::new(
                            classify_phase_configs,
                            classify_phase_observation,
                        );
                        let event_configs =
                            ObservedConfigSnapshot::new(event_configs, event_observation);
                        let classify_configs = PinnedConfigContext {
                            ids: &plan.config_cell_ids,
                            connection: connection_configs,
                            request: request_configs,
                            attempt: attempt_configs,
                            phase: &classify_phase_configs,
                        };
                        self.provider
                            .classify_precommit(
                                provider_state,
                                event,
                                &classify_configs,
                                &event_configs,
                            )
                            .map_err(GatewayExecutionError::Provider)?
                    };
                    match (classified_sequence, classification.decoded_sse) {
                        (Some(sequence), Some(decoded)) if sequence == decoded.sequence => {
                            sse_handoff
                                .as_mut()
                                .expect("classified SSE event has a handoff owner")
                                .mark_decoded(sequence, decoded.decoded)?;
                        }
                        (Some(sequence), Some(decoded)) => {
                            return Err(GatewayExecutionError::DecodedSseSequenceMismatch {
                                expected: sequence,
                                actual: decoded.sequence,
                            });
                        }
                        (Some(sequence), None) => {
                            return Err(GatewayExecutionError::MissingDecodedSse(sequence));
                        }
                        (None, Some(decoded)) => {
                            return Err(GatewayExecutionError::UnexpectedDecodedSse(
                                decoded.sequence,
                            ));
                        }
                        (None, None) => {}
                    }
                    if let Some(classified) = classification.classified {
                        break AttemptDecision::Provider(Box::new(classified));
                    }
                }
            };

            let (classified, filter_local_reply) = match decision {
                AttemptDecision::Provider(classified) => (*classified, None),
                AttemptDecision::FilterLocalReply(reply) => {
                    let upstream_side_effects =
                        UpstreamSideEffectSnapshot::from(exchange.snapshot());
                    let normalized = self
                        .provider
                        .normalize_attempt_local_reply(provider_state, reply, upstream_side_effects)
                        .map_err(GatewayExecutionError::Provider)?;
                    if normalized.upstream_side_effects != upstream_side_effects {
                        return Err(GatewayExecutionError::InvalidAttemptLocalReplySideEffects);
                    }
                    (normalized.classified, Some(normalized.reply))
                }
            };
            let ClassifiedAttemptResult { facts, readiness } = classified;
            validate_provider_facts(&facts)?;
            staged_provider_facts = Some(facts);
            staged_readiness = Some(readiness);
            await_request_operation(
                session,
                cancellation,
                deadline,
                attempt_telemetry.as_ref(),
                async {
                    self.provider
                        .confirm_precommit(
                            provider_state,
                            staged_provider_facts
                                .as_ref()
                                .expect("classification facts are staged before confirmation"),
                            attempt_deadline,
                            cancellation,
                        )
                        .await
                        .map_err(GatewayExecutionError::Provider)
                },
            )
            .await?;
            let transport_facts = exchange.transport_facts();
            let disposition = decision_session
                .decide(
                    selected,
                    staged_provider_facts
                        .as_ref()
                        .expect("classification facts are staged before decision"),
                    &transport_facts,
                )
                .map_err(GatewayExecutionError::Selection)?;
            exchange.submit_disposition_candidate(disposition)?;
            driver.disposition_pending()?;
            let gate = await_attempt_operation(
                session,
                cancellation,
                deadline,
                attempt_telemetry.as_ref(),
                exchange.wait_writer_gate(attempt_deadline),
            )
            .await?;
            let (disposition, permit) = match gate {
                WriterGate::ReadyToPublishAccept { permit, .. }
                | WriterGate::ReadyToPublishNonAccept { permit, .. } => (disposition, permit),
                WriterGate::AcceptBlocked { reason } => {
                    let replacement = decision_session
                        .replace_blocked_accept(selected, reason)
                        .map_err(GatewayExecutionError::Selection)?;
                    exchange.replace_accept_blocked_candidate(replacement)?;
                    match await_attempt_operation(
                        session,
                        cancellation,
                        deadline,
                        attempt_telemetry.as_ref(),
                        exchange.wait_writer_gate(attempt_deadline),
                    )
                    .await?
                    {
                        WriterGate::ReadyToPublishNonAccept { permit, .. } => (replacement, permit),
                        _ => return Err(GatewayExecutionError::InvalidBlockedReplacement),
                    }
                }
            };
            let published = exchange.publish_disposition(disposition, permit)?;
            let terminal_published_at = Instant::now();
            let _ = decision_session.observe_published(&published);
            let accepted_sse_handoff = if let Some(handoff) = sse_handoff.take() {
                let next_sequence = handoff.next_sequence();
                if disposition == Disposition::Accept {
                    Some((handoff.publish_accept()?, next_sequence))
                } else {
                    handoff.publish_non_accept();
                    None
                }
            } else {
                None
            };

            if disposition == Disposition::Continue {
                let _lease_zero = leases.lease_zero_for_continue()?;
                if let Some(telemetry) = &attempt_telemetry {
                    telemetry.release(
                        ReleasePoint::LastRequestBodyLeaseDrop,
                        terminal_published_at.elapsed(),
                    );
                }
            } else {
                self.provider
                    .release_terminal_request(
                        logical
                            .take()
                            .expect("terminal publication consumes logical owner once"),
                        &published,
                    )
                    .map_err(GatewayExecutionError::ProviderTerminalRelease)?;
                let _release_ready = leases.mark_terminal_and_release_ready()?;
                if let Some(telemetry) = &attempt_telemetry {
                    let latency = terminal_published_at.elapsed();
                    telemetry.release(ReleasePoint::LastRawConsumer, latency);
                    telemetry.release(ReleasePoint::LastRequestBodyLeaseDrop, latency);
                    telemetry.release(ReleasePoint::ModelIrReleased, latency);
                }
            }

            let readiness = staged_readiness.take();
            let provider_facts = staged_provider_facts
                .take()
                .expect("successful disposition publication retains classified facts");
            Ok(AttemptProgressReady {
                disposition,
                published,
                readiness,
                provider_facts,
                filter_local_reply,
                accepted_sse_handoff,
                sse_end_stream_pending,
            })
        }
        .await;
        AttemptProgress {
            staged_provider_facts,
            staged_readiness,
            result,
        }
    }
}
