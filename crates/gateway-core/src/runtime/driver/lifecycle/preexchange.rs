use super::*;

pub(super) mod failure;

pub(super) struct PreexchangeContext<'a, L, R: GatewayRequestFilterPort> {
    pub(super) session: &'a mut dyn GatewaySession,
    pub(super) request_filters: &'a mut GatewayFilterRequestOwner<R>,
    pub(super) cancellation: &'a CancellationToken,
    pub(super) request_id: RequestId,
    pub(super) binding: &'a RequestExecutionBinding,
    pub(super) request_configs: &'a ObservedConfigSnapshot<RequestConfigSnapshot>,
    pub(super) driver: &'a mut LogicalRequestDriver,
    pub(super) budget: &'a StreamBudget,
    pub(super) route_accepted_body_plan: &'a BodyPlan,
    pub(super) deadline: Instant,
    pub(super) logical: &'a mut Option<L>,
    pub(super) leases: &'a RequestLeaseBook,
    pub(super) selected: &'a mut SelectedGatewayAttempt,
    pub(super) next_attempt_id: &'a AtomicU64,
    pub(super) attempt_binding: &'a AttemptExecutionBinding,
    pub(super) attempt_deadline: Instant,
    pub(super) has_attempt_request_filters: bool,
    pub(super) has_attempt_response_filters: bool,
    pub(super) connection_configs: &'a ObservedConfigSnapshot<ConfigScopeSnapshot>,
    pub(super) attempt_configs: &'a ObservedConfigSnapshot<ConfigScopeSnapshot>,
    pub(super) materialize_phase_configs: ObservedConfigSnapshot<ConfigScopeSnapshot>,
    pub(super) attempt_telemetry: Option<RequestTelemetry>,
}

pub(super) struct PreexchangeReady<D, T: AttemptTransport> {
    pub(super) exchange: AttemptExchange<T>,
    pub(super) attempt_request_local_reply: Option<LocalReply>,
    pub(super) accepted_sse_limits: Option<SseLimits>,
    pub(super) sse_framer: Option<SseFramer>,
    pub(super) sse_handoff: Option<PrecommitResponseState<D>>,
    pub(super) precommit_sse_mailbox: Option<BudgetedResponseMailbox<u64>>,
}

pub(super) struct PreexchangeAttempt<A, D, T: AttemptTransport> {
    pub(super) provider_state: Option<A>,
    pub(super) request_global_bootstrap_failure: bool,
    pub(super) result: Result<PreexchangeReady<D, T>, GatewayExecutionError>,
}

impl<S, P, F, T> GatewayCoreLifecycle<S, P, F, T>
where
    P: ProviderRuntimePort,
    S: SelectionPublicationPort<P::RouteRequestContext>,
    F: GatewayFilterManagerPort,
    T: AttemptTransportFactory,
{
    pub(super) async fn prepare_preexchange(
        &self,
        context: PreexchangeContext<'_, P::LogicalRequest, F::RequestFilters>,
    ) -> PreexchangeAttempt<P::AttemptState, P::DecodedSseEvent, T::Transport> {
        let PreexchangeContext {
            session,
            request_filters,
            cancellation,
            request_id,
            binding,
            request_configs,
            driver,
            budget,
            route_accepted_body_plan,
            deadline,
            logical,
            leases,
            selected,
            next_attempt_id,
            attempt_binding,
            attempt_deadline,
            has_attempt_request_filters,
            has_attempt_response_filters,
            connection_configs,
            attempt_configs,
            materialize_phase_configs,
            attempt_telemetry,
        } = context;
        let plan = attempt_binding.plan();
        // Provider materialization is the ownership boundary for opaque
        // AttemptState. Keeping it outside the preparation future lets a
        // successfully materialized candidate reach its observational
        // finalizer even when AttemptRequest filter setup later fails.
        let mut provider_state = None;
        // Response-prefix reservations draw from the request-wide stream
        // budget. Exhausting that shared resource cannot be repaired by
        // selecting another candidate, unlike an attempt-local deadline or
        // a candidate-specific prepared-body validation failure.
        let mut request_global_bootstrap_failure = false;
        let result = async {
            let (mut prepared, materialized_state) = await_preexchange_operation(
                session,
                cancellation,
                deadline,
                attempt_deadline,
                attempt_telemetry.as_ref(),
                async {
                    self.provider
                        .materialize_attempt(
                            logical
                                .as_mut()
                                .expect("logical owner exists until terminal publication"),
                            AttemptMaterializationContext {
                                binding: attempt_binding,
                                selected,
                                configs: PinnedConfigContext {
                                    ids: &plan.config_cell_ids,
                                    connection: connection_configs,
                                    request: request_configs,
                                    attempt: attempt_configs,
                                    phase: &materialize_phase_configs,
                                },
                                budget,
                                leases,
                                write_quantum: self.limits.write_quantum,
                                cancellation,
                            },
                        )
                        .await
                        .map_err(GatewayExecutionError::Provider)
                },
            )
            .await?;
            provider_state = Some(materialized_state);
            if selected.attempt_id.0 == 0 {
                selected.attempt_id = AttemptId(
                    next_attempt_id
                        .fetch_add(1, Ordering::Relaxed)
                        .wrapping_add(1),
                );
                driver.begin_attempt()?;
            }
            let resolved_target = self
                .provider
                .resolved_transport_target(
                    provider_state
                        .as_ref()
                        .expect("provider state exists after materialization"),
                    attempt_binding,
                )
                .map_err(GatewayExecutionError::Provider)?;
            drop(materialize_phase_configs);
            if has_attempt_request_filters || has_attempt_response_filters {
                let filter_phase = attempt_binding.acquire_phase_configs()?;
                let filter_event = attempt_binding.acquire_event_configs()?;
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
                let mut descriptors = Vec::with_capacity(
                    plan.attempt_request_filters.len() + plan.attempt_response_filters.len(),
                );
                descriptors.extend(plan.attempt_request_filters.iter().cloned());
                descriptors.extend(plan.attempt_response_filters.iter().cloned());
                let filter_configs = materialize_filter_configs(
                    &descriptors,
                    Some(connection_configs),
                    request_configs,
                    Some(attempt_configs),
                    &filter_phase,
                    &filter_event,
                )?;
                request_filters
                    .filters
                    .begin_attempt(
                        &plan.attempt_request_filters,
                        &plan.attempt_response_filters,
                        filter_scope_context(
                            request_id,
                            binding.plan_revision(),
                            Some(selected.binding),
                            Some(selected.attempt_id),
                            Some(selected.generation),
                            driver
                                .scope_id(ScopeKind::RouteAttempt)
                                .ok_or(GatewayExecutionError::MissingFilterScope)?,
                            ScopeKind::RouteAttempt,
                            attempt_deadline,
                            cancellation,
                            attempt_telemetry.clone(),
                            budget,
                            filter_configs,
                            &self.filter_executors,
                        ),
                    )
                    .map_err(GatewayExecutionError::Filter)?;
            }
            let mut attempt_request_local_reply = None;
            if has_attempt_request_filters {
                let header_result = await_preexchange_operation(
                    session,
                    cancellation,
                    deadline,
                    attempt_deadline,
                    attempt_telemetry.as_ref(),
                    async {
                        request_filters
                            .filters
                            .filter_attempt_request_head(&mut prepared.head)
                            .await
                            .map_err(GatewayExecutionError::Filter)
                    },
                )
                .await?;
                let mut attempt_request_pause = header_result.pause;
                attempt_request_local_reply = header_result.local_reply;

                if attempt_request_local_reply.is_none() {
                    let mut source = prepared.body.take_filter_chunks()?;
                    let mut filtered = ChargedBodyQueue::new(
                        budget,
                        MemoryRole::AttemptWire,
                        &plan.body_plans.attempt_request,
                        self.limits.max_request_body_bytes,
                        plan.attempt_request_chunk_capacity,
                    )?;
                    let mut source_eos = false;
                    let mut filtered_eos = false;
                    loop {
                        let frame = if let Some(bytes) = source.pop_front() {
                            AttemptRequestBodyFrame {
                                bytes: Some(bytes),
                                end_stream: false,
                                queue_metadata: BodyMetadataOwner::default(),
                            }
                        } else if !source_eos {
                            source_eos = true;
                            AttemptRequestBodyFrame {
                                bytes: None,
                                end_stream: true,
                                queue_metadata: BodyMetadataOwner::default(),
                            }
                        } else {
                            break;
                        };
                        loop {
                            let resumed = match attempt_request_pause {
                                Some(FilterPause::Watermark) => Some(
                                    await_preexchange_operation(
                                        session,
                                        cancellation,
                                        deadline,
                                        attempt_deadline,
                                        attempt_telemetry.as_ref(),
                                        async {
                                            request_filters
                                                .filters
                                                .wait_attempt_request_resume(&mut prepared.head)
                                                .await
                                                .map_err(GatewayExecutionError::Filter)
                                        },
                                    )
                                    .await?,
                                ),
                                Some(FilterPause::Buffer) => {
                                    await_preexchange_operation(
                                        session,
                                        cancellation,
                                        deadline,
                                        attempt_deadline,
                                        attempt_telemetry.as_ref(),
                                        async {
                                            request_filters
                                                .filters
                                                .try_attempt_request_resume(&mut prepared.head)
                                                .await
                                                .map_err(GatewayExecutionError::Filter)
                                        },
                                    )
                                    .await?
                                }
                                Some(FilterPause::HeaderIteration) | None => None,
                            };
                            let Some(resumed) = resumed else {
                                break;
                            };
                            if let Some(reply) = resumed.local_reply {
                                attempt_request_local_reply = Some(reply);
                                break;
                            }
                            append_attempt_request_filter_frames(
                                &mut filtered,
                                &mut filtered_eos,
                                resumed.frames,
                            )?;
                            attempt_request_pause = resumed.pause;
                        }
                        if attempt_request_local_reply.is_some() {
                            break;
                        }
                        let filter_configs = {
                            let filter_phase = attempt_binding.acquire_phase_configs()?;
                            let filter_event = attempt_binding.acquire_event_configs()?;
                            materialize_filter_configs(
                                &plan.attempt_request_filters,
                                Some(connection_configs),
                                request_configs,
                                Some(attempt_configs),
                                &filter_phase,
                                &filter_event,
                            )?
                        };
                        let result = await_preexchange_operation(
                            session,
                            cancellation,
                            deadline,
                            attempt_deadline,
                            attempt_telemetry.as_ref(),
                            async {
                                request_filters
                                    .filters
                                    .filter_attempt_request_body(
                                        &mut prepared.head,
                                        frame,
                                        filter_configs,
                                    )
                                    .await
                                    .map_err(GatewayExecutionError::Filter)
                            },
                        )
                        .await?;
                        if let Some(reply) = result.local_reply {
                            attempt_request_local_reply = Some(reply);
                            break;
                        }
                        append_attempt_request_filter_frames(
                            &mut filtered,
                            &mut filtered_eos,
                            result.frames,
                        )?;
                        attempt_request_pause = result.pause;
                    }
                    while attempt_request_local_reply.is_none()
                        && let Some(pause) = attempt_request_pause
                    {
                        if pause == FilterPause::HeaderIteration {
                            return Err(GatewayExecutionError::Filter(Arc::from(
                                "attempt request header iteration remained paused after EOS",
                            )));
                        }
                        let resumed = await_preexchange_operation(
                            session,
                            cancellation,
                            deadline,
                            attempt_deadline,
                            attempt_telemetry.as_ref(),
                            async {
                                request_filters
                                    .filters
                                    .wait_attempt_request_resume(&mut prepared.head)
                                    .await
                                    .map_err(GatewayExecutionError::Filter)
                            },
                        )
                        .await?;
                        if let Some(reply) = resumed.local_reply {
                            attempt_request_local_reply = Some(reply);
                        } else {
                            append_attempt_request_filter_frames(
                                &mut filtered,
                                &mut filtered_eos,
                                resumed.frames,
                            )?;
                            attempt_request_pause = resumed.pause;
                        }
                    }
                    if attempt_request_local_reply.is_none() && !filtered_eos {
                        return Err(GatewayExecutionError::Filter(Arc::from(
                            "attempt request filter did not emit EOS",
                        )));
                    }
                    // The input and output queue reservations are both live
                    // while transformation is in progress. Dropping `source`
                    // releases its backing before the output queue is moved
                    // into the linear attempt-body owner.
                    drop(source);
                    prepared.body.replace_filter_chunks(filtered)?;
                }
            }
            let mut exchange = AttemptExchange::new_from_compiled_plan(
                request_id,
                selected.attempt_id,
                selected.generation,
                binding.plan_revision(),
                attempt_binding.plan_handle(),
                self.transports.create_transport(connection_configs),
                prepared,
                attempt_deadline,
                budget.clone(),
                self.limits.write_quantum,
                cancellation.clone(),
                resolved_target,
            )
            .map_err(|error| {
                if matches!(&error, AttemptError::DeadlineExceeded) {
                    let now = Instant::now();
                    if now >= deadline {
                        cancellation.cancel();
                        observe_runtime_error(attempt_telemetry.as_ref(), ErrorClass::Deadline);
                        GatewayExecutionError::Deadline
                    } else {
                        GatewayExecutionError::PreexchangeAttemptDeadline
                    }
                } else {
                    if matches!(&error, AttemptError::Body(BodyError::BudgetExceeded)) {
                        request_global_bootstrap_failure = true;
                    }
                    GatewayExecutionError::Attempt(error)
                }
            })?;
            if let Some(telemetry) = attempt_telemetry.clone() {
                exchange = exchange.with_telemetry(telemetry);
            }
            if attempt_request_local_reply.is_some() {
                exchange.quiesce_without_upstream_exchange()?;
            }

            let precommit_sse_limits =
                sse_limits_from_plan(&plan.body_plans.attempt_response_precommit);
            let accepted_sse_limits = sse_limits_from_plan(route_accepted_body_plan)
                .or_else(|| precommit_sse_limits.clone());
            let sse_framer = precommit_sse_limits
                .clone()
                .map(|limits| SseFramer::new(limits, budget.clone()))
                .transpose()
                .map_err(|error| {
                    if error == SseError::BudgetExceeded {
                        request_global_bootstrap_failure = true;
                    }
                    GatewayExecutionError::Sse(error)
                })?;
            let sse_handoff = if sse_framer.is_some() {
                Some(
                    PrecommitResponseState::<P::DecodedSseEvent>::new_budgeted(
                        plan.precommit_event_capacity,
                        budget,
                    )
                    .map_err(|error| {
                        if error == SseError::BudgetExceeded {
                            request_global_bootstrap_failure = true;
                        }
                        GatewayExecutionError::Sse(error)
                    })?,
                )
            } else {
                None
            };
            let precommit_sse_mailbox = if sse_framer.is_some() {
                Some(
                    BudgetedResponseMailbox::<u64>::new_budgeted(
                        plan.precommit_event_capacity,
                        budget,
                    )
                    .map_err(|error| {
                        if error == SseError::BudgetExceeded {
                            request_global_bootstrap_failure = true;
                        }
                        GatewayExecutionError::Sse(error)
                    })?,
                )
            } else {
                None
            };
            Ok::<_, GatewayExecutionError>(PreexchangeReady {
                exchange,
                attempt_request_local_reply,
                accepted_sse_limits,
                sse_framer,
                sse_handoff,
                precommit_sse_mailbox,
            })
        }
        .await;
        PreexchangeAttempt {
            provider_state,
            request_global_bootstrap_failure,
            result,
        }
    }
}
