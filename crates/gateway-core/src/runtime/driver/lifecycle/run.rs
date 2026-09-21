use super::*;

impl<S, P, F, T> GatewayCoreLifecycle<S, P, F, T>
where
    P: ProviderRuntimePort,
    S: SelectionPublicationPort<P::RouteRequestContext>,
    F: GatewayFilterManagerPort,
    T: AttemptTransportFactory,
{
    pub(super) async fn run_request(
        &self,
        session: &mut dyn GatewaySession,
        request_filters: &mut GatewayFilterRequestOwner<F::RequestFilters>,
        request: RequestRun<P::LogicalRequest, S::Session>,
    ) -> Result<SessionReuse, GatewayExecutionError> {
        let RequestRun {
            cancellation,
            request_id,
            mut binding,
            mut request_configs,
            mut driver,
            budget,
            mut final_writer,
            run_filter_callbacks,
            downstream_method,
            downstream_protocol,
            route_binding,
            route_accepted_body_plan,
            deadline,
            mut logical,
            mut decision_session,
            route_decision_id,
            request_telemetry,
            max_attempts,
            candidate_bindings,
            leases,
            mut generation,
            mut completed_attempts,
            mut routing_snapshots,
        } = request;

        loop {
            let selected = SelectionState {
                request_id,
                generation,
                route_binding,
                deadline,
                max_attempts,
                candidate_bindings: &candidate_bindings,
                binding: &binding,
                decision_session: &mut decision_session,
                completed_attempts: &completed_attempts,
                routing_snapshots: &mut routing_snapshots,
            }
            .select()?;
            let Some((selected, routing_facts)) = selected else {
                let local_reply = decision_session
                    .exhaustion_local_reply(Instant::now())
                    .map_err(GatewayExecutionError::Selection)?
                    .unwrap_or_else(|| LocalReply {
                        status: StatusCode::BAD_GATEWAY,
                        headers: HeaderMap::new(),
                        body: Bytes::from_static(b"no upstream candidate"),
                        provenance: SemanticProvenance::NonSemantic,
                    });
                return emit_local_response(
                    session,
                    &mut final_writer,
                    &self.filter_executors,
                    &mut driver,
                    &mut binding,
                    &request_configs,
                    &mut request_filters.filters,
                    local_reply,
                    &budget,
                    request_id,
                    Some(route_binding),
                    None,
                    Some(generation),
                    deadline,
                    &cancellation,
                    request_telemetry.clone(),
                    &downstream_method,
                    downstream_protocol,
                    SessionReuse::Reusable,
                )
                .await;
            };
            let attempt = SelectedAttemptSetup {
                request_id,
                generation,
                binding: &binding,
                request_configs: &request_configs,
                driver: &mut driver,
                run_filter_callbacks,
                route_decision_id,
                deadline,
                request_telemetry: request_telemetry.as_ref(),
            }
            .bind(selected, routing_facts)?;
            let SelectedAttemptRun {
                mut selected,
                routing_facts,
                attempt_binding,
                attempt_deadline,
                has_attempt_request_filters,
                has_attempt_response_filters,
                connection_configs,
                attempt_configs,
                materialize_phase_configs,
                attempt_telemetry,
            } = attempt;
            let plan = attempt_binding.plan();
            // Provider materialization is the ownership boundary for opaque
            // AttemptState. Keeping it outside the preparation future lets a
            // successfully materialized candidate reach its observational
            // finalizer even when AttemptRequest filter setup later fails.
            let PreexchangeAttempt {
                mut provider_state,
                request_global_bootstrap_failure,
                result: preexchange,
            } = self
                .prepare_preexchange(PreexchangeContext {
                    session,
                    request_filters,
                    cancellation: &cancellation,
                    request_id,
                    binding: &binding,
                    request_configs: &request_configs,
                    driver: &mut driver,
                    budget: &budget,
                    route_accepted_body_plan: &route_accepted_body_plan,
                    deadline,
                    logical: &mut logical,
                    leases: &leases,
                    selected: &mut selected,
                    next_attempt_id: &self.next_attempt_id,
                    attempt_binding: &attempt_binding,
                    attempt_deadline,
                    has_attempt_request_filters,
                    has_attempt_response_filters,
                    connection_configs: &connection_configs,
                    attempt_configs: &attempt_configs,
                    materialize_phase_configs,
                    attempt_telemetry: attempt_telemetry.clone(),
                })
                .await;
            let PreexchangeReady {
                mut exchange,
                mut attempt_request_local_reply,
                accepted_sse_limits,
                mut sse_framer,
                mut sse_handoff,
                mut precommit_sse_mailbox,
            } = match preexchange {
                Ok(ready) => ready,
                Err(error) => {
                    match self
                        .finish_preexchange_failure(
                            PreexchangeFailureContext {
                                session,
                                request_filters,
                                cancellation: &cancellation,
                                request_id,
                                binding: &mut binding,
                                request_configs: &request_configs,
                                driver: &mut driver,
                                final_writer: &mut final_writer,
                                budget: &budget,
                                route_binding,
                                deadline,
                                logical: &mut logical,
                                decision_session: &mut decision_session,
                                generation: &mut generation,
                                completed_attempts: &mut completed_attempts,
                                leases: &leases,
                                selected: &selected,
                                routing_facts: &routing_facts,
                                provider_state: &mut provider_state,
                                attempt_telemetry: attempt_telemetry.clone(),
                                downstream_method: &downstream_method,
                                downstream_protocol,
                            },
                            error,
                            request_global_bootstrap_failure,
                        )
                        .await?
                    {
                        PreexchangeFailureOutcome::Continue => continue,
                        PreexchangeFailureOutcome::Completed(reuse) => return Ok(reuse),
                    }
                }
            };
            let mut provider_state = provider_state
                .take()
                .expect("successful pre-exchange preparation retains provider attempt state");
            let AttemptProgress {
                staged_provider_facts,
                mut staged_readiness,
                result: attempt_progress,
            } = self
                .progress_attempt(AttemptProgressContext {
                    session,
                    request_filters,
                    cancellation: &cancellation,
                    deadline,
                    attempt_deadline,
                    attempt_telemetry: attempt_telemetry.clone(),
                    exchange: &mut exchange,
                    attempt_request_local_reply: &mut attempt_request_local_reply,
                    has_attempt_response_filters,
                    attempt_binding: &attempt_binding,
                    connection_configs: &connection_configs,
                    request_configs: &request_configs,
                    attempt_configs: &attempt_configs,
                    driver: &mut driver,
                    budget: &budget,
                    sse_framer: &mut sse_framer,
                    sse_handoff: &mut sse_handoff,
                    precommit_sse_mailbox: &mut precommit_sse_mailbox,
                    provider_state: &mut provider_state,
                    decision_session: &mut decision_session,
                    selected: &selected,
                    leases: &leases,
                    logical: &mut logical,
                })
                .await;
            let AttemptProgressReady {
                disposition,
                published,
                readiness,
                provider_facts,
                filter_local_reply,
                accepted_sse_handoff,
                sse_end_stream_pending,
            } = match attempt_progress {
                Ok(progress) => progress,
                Err(error) => {
                    match self
                        .finish_attempt_progress_failure(
                            AttemptProgressFailureContext {
                                session,
                                request_filters,
                                cancellation: &cancellation,
                                request_id,
                                binding: &mut binding,
                                request_configs: &request_configs,
                                driver: &mut driver,
                                final_writer: &mut final_writer,
                                budget: &budget,
                                route_binding,
                                deadline,
                                logical: &mut logical,
                                decision_session: &mut decision_session,
                                generation: &mut generation,
                                completed_attempts: &mut completed_attempts,
                                leases: &leases,
                                selected: &selected,
                                routing_facts: &routing_facts,
                                provider_state: &mut provider_state,
                                staged_readiness: &mut staged_readiness,
                                staged_provider_facts: &staged_provider_facts,
                                attempt_telemetry: attempt_telemetry.clone(),
                                downstream_method: &downstream_method,
                                downstream_protocol,
                                exchange: &mut exchange,
                                sse_handoff: &mut sse_handoff,
                            },
                            error,
                        )
                        .await?
                    {
                        AttemptProgressFailureOutcome::Continue => continue,
                        AttemptProgressFailureOutcome::Completed(reuse) => return Ok(reuse),
                    }
                }
            };

            let accepted_body_plan = route_accepted_body_plan.clone();
            let precommit_event_capacity = plan.precommit_event_capacity;
            drop(attempt_binding);

            let completion = self
                .complete_published_attempt(PublishedAttempt {
                    disposition,
                    session,
                    request_filters,
                    cancellation: &cancellation,
                    request_id,
                    binding: &mut binding,
                    request_configs: &mut request_configs,
                    driver: &mut driver,
                    final_writer: &mut final_writer,
                    budget: &budget,
                    deadline,
                    run_filter_callbacks,
                    decision_session: &mut decision_session,
                    generation: &mut generation,
                    completed_attempts: &mut completed_attempts,
                    selected,
                    routing_facts,
                    provider_state,
                    readiness,
                    provider_facts,
                    attempt_telemetry,
                    downstream_method: &downstream_method,
                    downstream_protocol,
                    exchange,
                    published,
                    filter_local_reply,
                    accepted_sse_handoff,
                    sse_end_stream_pending,
                    accepted_sse_limits,
                    sse_framer,
                    precommit_sse_mailbox,
                    accepted_body_plan,
                    precommit_event_capacity,
                    connection_configs,
                    attempt_configs,
                })
                .await?;
            match completion {
                AttemptCompletionOutcome::Continue => continue,
                AttemptCompletionOutcome::Completed(reuse) => return Ok(reuse),
            }
        }
    }
}
