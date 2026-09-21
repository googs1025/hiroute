use super::*;

pub(in super::super) enum AttemptProgressFailureOutcome {
    Continue,
    Completed(SessionReuse),
}

pub(in super::super) struct AttemptProgressFailureContext<
    'a,
    L,
    D,
    R: GatewayRequestFilterPort,
    A,
    W,
    X: AttemptTransport,
    E,
> {
    pub(in super::super) session: &'a mut dyn GatewaySession,
    pub(in super::super) request_filters: &'a mut GatewayFilterRequestOwner<R>,
    pub(in super::super) cancellation: &'a CancellationToken,
    pub(in super::super) request_id: RequestId,
    pub(in super::super) binding: &'a mut RequestExecutionBinding,
    pub(in super::super) request_configs: &'a ObservedConfigSnapshot<RequestConfigSnapshot>,
    pub(in super::super) driver: &'a mut LogicalRequestDriver,
    pub(in super::super) final_writer: &'a mut RequestFinalWriter,
    pub(in super::super) budget: &'a StreamBudget,
    pub(in super::super) route_binding: ResolvedTargetBindingId,
    pub(in super::super) deadline: Instant,
    pub(in super::super) logical: &'a mut Option<L>,
    pub(in super::super) decision_session: &'a mut D,
    pub(in super::super) generation: &'a mut AttemptGeneration,
    pub(in super::super) completed_attempts: &'a mut Vec<CompletedAttemptObservation>,
    pub(in super::super) leases: &'a RequestLeaseBook,
    pub(in super::super) selected: &'a SelectedGatewayAttempt,
    pub(in super::super) routing_facts: &'a RealtimeRoutingFacts,
    pub(in super::super) provider_state: &'a mut A,
    pub(in super::super) staged_readiness: &'a mut Option<W>,
    pub(in super::super) staged_provider_facts: &'a Option<ProviderClassificationFacts>,
    pub(in super::super) attempt_telemetry: Option<RequestTelemetry>,
    pub(in super::super) downstream_method: &'a Method,
    pub(in super::super) downstream_protocol: HttpProtocol,
    pub(in super::super) exchange: &'a mut AttemptExchange<X>,
    pub(in super::super) sse_handoff: &'a mut Option<PrecommitResponseState<E>>,
}

impl<S, P, F, T> GatewayCoreLifecycle<S, P, F, T>
where
    P: ProviderRuntimePort,
    S: SelectionPublicationPort<P::RouteRequestContext>,
    F: GatewayFilterManagerPort,
    T: AttemptTransportFactory,
{
    pub(in super::super) async fn finish_attempt_progress_failure(
        &self,
        context: AttemptProgressFailureContext<
            '_,
            P::LogicalRequest,
            S::Session,
            F::RequestFilters,
            P::AttemptState,
            P::Readiness,
            T::Transport,
            P::DecodedSseEvent,
        >,
        error: GatewayExecutionError,
    ) -> Result<AttemptProgressFailureOutcome, GatewayExecutionError> {
        let AttemptProgressFailureContext {
            session,
            request_filters,
            cancellation,
            request_id,
            binding,
            request_configs,
            driver,
            final_writer,
            budget,
            route_binding,
            deadline,
            logical,
            decision_session,
            generation,
            completed_attempts,
            leases,
            selected,
            routing_facts,
            provider_state,
            staged_readiness,
            staged_provider_facts,
            attempt_telemetry,
            downstream_method,
            downstream_protocol,
            exchange,
            sse_handoff,
        } = context;

        let mut confirmed_failure = attempt_failure_facts(&error, exchange);
        if let Some(failure) = confirmed_failure.as_mut() {
            if failure.provider.is_none() {
                failure.provider = staged_provider_facts.clone();
            }
            let confirmation = await_session_operation(
                cancellation,
                deadline.min(selected.budget.deadline),
                attempt_telemetry.as_ref(),
                async {
                    self.provider
                        .confirm_attempt_failure(
                            &mut *provider_state,
                            failure,
                            selected.budget.deadline,
                            cancellation,
                        )
                        .await
                        .map_err(GatewayExecutionError::Provider)
                },
            )
            .await;
            match confirmation {
                Ok(Some(provider)) => {
                    validate_provider_facts(&provider)?;
                    failure.provider = Some(provider);
                }
                Ok(None) => {}
                Err(confirmation_error) => {
                    exchange.cancellation_token().cancel();
                    let exchange_cleanup = if exchange.snapshot().finalized {
                        Ok(())
                    } else {
                        finish_attempt_bounded(&mut *exchange, self.limits.cleanup_timeout).await
                    };
                    let filter_cleanup = finish_attempt_filter_scope_bounded(
                        &mut request_filters.filters,
                        self.limits.cleanup_timeout,
                    )
                    .await;
                    driver.complete();
                    combine_cleanup_results(exchange_cleanup, filter_cleanup)?;
                    return Err(confirmation_error);
                }
            }
        }

        if let Some(disposition) = exchange.snapshot().published {
            let failure = confirmed_failure.clone();
            let continue_lease_cleanup = if disposition == Disposition::Continue {
                leases
                    .lease_zero_for_continue()
                    .map(|_| ())
                    .map_err(GatewayExecutionError::Body)
            } else {
                Ok(())
            };
            let exchange_cleanup = if exchange.snapshot().finalized {
                Ok(())
            } else {
                exchange.cancellation_token().cancel();
                finish_attempt_bounded(&mut *exchange, self.limits.cleanup_timeout).await
            };
            let terminal_lease_cleanup = if disposition == Disposition::Continue {
                Ok(())
            } else {
                leases
                    .mark_terminal_and_release_ready()
                    .map(|_| ())
                    .map_err(GatewayExecutionError::Body)
            };
            let attempt_filter_cleanup = finish_attempt_filter_scope_bounded(
                &mut request_filters.filters,
                self.limits.cleanup_timeout,
            )
            .await;
            let accepted_filter_cleanup = finish_accepted_filter_scope_bounded(
                &mut request_filters.filters,
                self.limits.cleanup_timeout,
            )
            .await;
            let cleanup_result = combine_cleanup_results(
                continue_lease_cleanup,
                combine_cleanup_results(
                    exchange_cleanup,
                    combine_cleanup_results(
                        terminal_lease_cleanup,
                        combine_cleanup_results(attempt_filter_cleanup, accepted_filter_cleanup),
                    ),
                ),
            );
            let cleanup = if cleanup_result.is_ok() {
                AttemptCleanupOutcome::Completed
            } else {
                AttemptCleanupOutcome::Failed
            };
            let result = Err::<(), _>(error);
            let completion = attempt_completion_from_result(
                disposition,
                attempt_commit_facts(exchange, final_writer),
                exchange.transport_facts(),
                cleanup,
                &result,
            );
            let final_provider = self
                .provider
                .finalize_attempt_facts(
                    &mut *provider_state,
                    staged_readiness.as_mut(),
                    staged_provider_facts.as_ref(),
                    &completion,
                )
                .ok()
                .filter(|facts| validate_provider_facts(facts).is_ok())
                .or_else(|| staged_provider_facts.clone());
            record_completed_attempt(
                decision_session,
                completed_attempts,
                selected,
                routing_facts,
                final_provider,
                failure,
                completion,
            );
            driver.complete();
            return result.map(|()| AttemptProgressFailureOutcome::Completed(SessionReuse::Close));
        }
        if let Some(failure) = confirmed_failure {
            let disposition = decision_session
                .decide_failure(selected, &failure)
                .map_err(GatewayExecutionError::Selection)?;
            if disposition == Disposition::Accept {
                abort_attempt_bounded(&mut *exchange, &mut *driver, self.limits.cleanup_timeout)
                    .await?;
                return Err(GatewayExecutionError::InvalidFailureDisposition);
            }
            exchange.submit_disposition_candidate(disposition)?;
            driver.disposition_pending()?;
            let gate = await_attempt_operation(
                session,
                cancellation,
                deadline,
                attempt_telemetry.as_ref(),
                exchange.wait_writer_gate(deadline),
            )
            .await?;
            let permit = match gate {
                WriterGate::ReadyToPublishNonAccept { permit, .. } => permit,
                _ => return Err(GatewayExecutionError::InvalidFailureDisposition),
            };
            let published = exchange.publish_disposition(disposition, permit)?;
            let _ = decision_session.observe_published(&published);
            if let Some(handoff) = sse_handoff.take() {
                handoff.publish_non_accept();
            }
            if disposition == Disposition::Continue {
                let lease_cleanup = leases
                    .lease_zero_for_continue()
                    .map(|_| ())
                    .map_err(GatewayExecutionError::Body);
                let exchange_cleanup =
                    finish_attempt_bounded(&mut *exchange, self.limits.cleanup_timeout).await;
                let filter_cleanup = finish_attempt_filter_scope_bounded(
                    &mut request_filters.filters,
                    self.limits.cleanup_timeout,
                )
                .await;
                let cleanup_result = combine_cleanup_results(
                    lease_cleanup,
                    combine_cleanup_results(exchange_cleanup, filter_cleanup),
                );
                let cleanup = if cleanup_result.is_ok() {
                    AttemptCleanupOutcome::Completed
                } else {
                    AttemptCleanupOutcome::Failed
                };
                let commits = attempt_commit_facts(exchange, final_writer);
                let transport = exchange.transport_facts();
                let completion = attempt_completion_from_result(
                    disposition,
                    commits,
                    transport,
                    cleanup,
                    &cleanup_result,
                );
                let final_provider = self
                    .provider
                    .finalize_attempt_facts(
                        &mut *provider_state,
                        staged_readiness.as_mut(),
                        staged_provider_facts.as_ref().or(failure.provider.as_ref()),
                        &completion,
                    )
                    .ok()
                    .filter(|facts| validate_provider_facts(facts).is_ok())
                    .or_else(|| {
                        staged_provider_facts
                            .clone()
                            .or_else(|| failure.provider.clone())
                    });
                record_completed_attempt(
                    decision_session,
                    completed_attempts,
                    selected,
                    routing_facts,
                    final_provider,
                    Some(failure),
                    completion,
                );
                if let Err(error) = cleanup_result {
                    driver.complete();
                    return Err(error);
                }
                driver.continue_after_attempt()?;
                *generation = AttemptGeneration(generation.0.wrapping_add(1));
                return Ok(AttemptProgressFailureOutcome::Continue);
            }

            let mut terminal_leases_released = false;
            let mut terminal_result: Result<SessionReuse, GatewayExecutionError> = async {
                self.provider
                    .release_terminal_request(
                        logical
                            .take()
                            .expect("failure termination consumes logical owner once"),
                        &published,
                    )
                    .map_err(GatewayExecutionError::ProviderTerminalRelease)?;
                let _release_ready = leases.mark_terminal_and_release_ready()?;
                terminal_leases_released = true;
                finish_attempt_bounded(&mut *exchange, self.limits.cleanup_timeout).await?;
                emit_local_response(
                    session,
                    &mut *final_writer,
                    &self.filter_executors,
                    &mut *driver,
                    &mut *binding,
                    request_configs,
                    &mut request_filters.filters,
                    LocalReply {
                        status: StatusCode::BAD_GATEWAY,
                        headers: HeaderMap::new(),
                        body: Bytes::from_static(b"upstream attempt failed"),
                        provenance: SemanticProvenance::NonSemantic,
                    },
                    budget,
                    request_id,
                    Some(route_binding),
                    Some(selected.attempt_id),
                    Some(selected.generation),
                    deadline,
                    cancellation,
                    attempt_telemetry.clone(),
                    downstream_method,
                    downstream_protocol,
                    SessionReuse::Reusable,
                )
                .await
            }
            .await;
            let exchange_cleanup = if exchange.snapshot().finalized {
                Ok(())
            } else {
                finish_attempt_bounded(&mut *exchange, self.limits.cleanup_timeout).await
            };
            let lease_cleanup = if terminal_leases_released {
                Ok(())
            } else {
                leases
                    .mark_terminal_and_release_ready()
                    .map(|_| ())
                    .map_err(GatewayExecutionError::Body)
            };
            let attempt_filter_cleanup = finish_attempt_filter_scope_bounded(
                &mut request_filters.filters,
                self.limits.cleanup_timeout,
            )
            .await;
            let accepted_filter_cleanup = finish_accepted_filter_scope_bounded(
                &mut request_filters.filters,
                self.limits.cleanup_timeout,
            )
            .await;
            let cleanup_result = combine_cleanup_results(
                exchange_cleanup,
                combine_cleanup_results(
                    lease_cleanup,
                    combine_cleanup_results(attempt_filter_cleanup, accepted_filter_cleanup),
                ),
            );
            let cleanup = if cleanup_result.is_ok() {
                AttemptCleanupOutcome::Completed
            } else {
                AttemptCleanupOutcome::Failed
            };
            if terminal_result.is_ok()
                && let Err(error) = cleanup_result
            {
                terminal_result = Err(error);
            }
            let completion = attempt_completion_from_result(
                disposition,
                attempt_commit_facts(exchange, final_writer),
                exchange.transport_facts(),
                cleanup,
                &terminal_result,
            );
            let final_provider = self
                .provider
                .finalize_attempt_facts(
                    &mut *provider_state,
                    staged_readiness.as_mut(),
                    staged_provider_facts.as_ref().or(failure.provider.as_ref()),
                    &completion,
                )
                .ok()
                .filter(|facts| validate_provider_facts(facts).is_ok())
                .or_else(|| {
                    staged_provider_facts
                        .clone()
                        .or_else(|| failure.provider.clone())
                });
            record_completed_attempt(
                decision_session,
                completed_attempts,
                selected,
                routing_facts,
                final_provider,
                Some(failure),
                completion,
            );
            driver.complete();
            return terminal_result.map(AttemptProgressFailureOutcome::Completed);
        }
        let records_postexchange_provider_completion = selected.attempt_id.0 != 0
            && exchange.snapshot().semantic_upstream_calls != 0
            && matches!(&error, GatewayExecutionError::Provider(_));
        let result = Err::<(), _>(error);
        let exchange_cleanup =
            abort_attempt_bounded(&mut *exchange, &mut *driver, self.limits.cleanup_timeout).await;
        let filter_cleanup = finish_attempt_filter_scope_bounded(
            &mut request_filters.filters,
            self.limits.cleanup_timeout,
        )
        .await;
        let cleanup_result = combine_cleanup_results(exchange_cleanup, filter_cleanup);
        if records_postexchange_provider_completion {
            let cleanup = if cleanup_result.is_ok() {
                AttemptCleanupOutcome::Completed
            } else {
                AttemptCleanupOutcome::Failed
            };
            // No disposition was published: `Terminate` records the actual
            // fail-closed terminal outcome without opening a fallback path.
            let mut completion = attempt_completion_from_result(
                Disposition::Terminate,
                attempt_commit_facts(exchange, final_writer),
                exchange.transport_facts(),
                cleanup,
                &result,
            );
            if completion.termination_reason == AttemptTerminationReason::ProviderEncoderFailure {
                completion.termination_reason = AttemptTerminationReason::AttemptFailure;
            }
            let final_provider = self
                .provider
                .finalize_attempt_facts(
                    &mut *provider_state,
                    staged_readiness.as_mut(),
                    staged_provider_facts.as_ref(),
                    &completion,
                )
                .ok()
                .filter(|facts| validate_provider_facts(facts).is_ok())
                .or_else(|| staged_provider_facts.clone());
            record_completed_attempt(
                decision_session,
                completed_attempts,
                selected,
                routing_facts,
                final_provider,
                None,
                completion,
            );
        }
        cleanup_result?;
        result.map(|()| AttemptProgressFailureOutcome::Completed(SessionReuse::Close))
    }
}
