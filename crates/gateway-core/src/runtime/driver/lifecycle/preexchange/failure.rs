use super::*;

pub(in super::super) enum PreexchangeFailureOutcome {
    Continue,
    Completed(SessionReuse),
}

pub(in super::super) struct PreexchangeFailureContext<'a, L, D, R: GatewayRequestFilterPort, A> {
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
    pub(in super::super) provider_state: &'a mut Option<A>,
    pub(in super::super) attempt_telemetry: Option<RequestTelemetry>,
    pub(in super::super) downstream_method: &'a Method,
    pub(in super::super) downstream_protocol: HttpProtocol,
}

impl<S, P, F, T> GatewayCoreLifecycle<S, P, F, T>
where
    P: ProviderRuntimePort,
    S: SelectionPublicationPort<P::RouteRequestContext>,
    F: GatewayFilterManagerPort,
    T: AttemptTransportFactory,
{
    pub(in super::super) async fn finish_preexchange_failure(
        &self,
        context: PreexchangeFailureContext<
            '_,
            P::LogicalRequest,
            S::Session,
            F::RequestFilters,
            P::AttemptState,
        >,
        error: GatewayExecutionError,
        request_global_bootstrap_failure: bool,
    ) -> Result<PreexchangeFailureOutcome, GatewayExecutionError> {
        let PreexchangeFailureContext {
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
            attempt_telemetry,
            downstream_method,
            downstream_protocol,
        } = context;

        let failure = if request_global_bootstrap_failure {
            None
        } else if provider_state.is_some() {
            attempt_request_preparation_failure_facts(&error, selected)
        } else {
            materialization_failure_facts(&error, &self.provider, selected)
        };
        let Some(failure) = failure else {
            let cleanup_result = finish_attempt_filter_scope_bounded(
                &mut request_filters.filters,
                self.limits.cleanup_timeout,
            )
            .await;
            let cleanup = if cleanup_result.is_ok() {
                AttemptCleanupOutcome::Completed
            } else {
                AttemptCleanupOutcome::Failed
            };
            let result: Result<SessionReuse, GatewayExecutionError> =
                Err(cleanup_result.err().unwrap_or(error));
            if provider_state.is_some() {
                // No disposition was published, so this technical
                // completion is private finalizer context only. It
                // deliberately does not enter the completed-attempt
                // ledger or a later selection input.
                let completion = attempt_completion_from_result(
                    Disposition::Terminate,
                    AttemptCommitFacts {
                        upstream_request: CommitFence::Clear,
                        downstream_headers: final_writer.header_fence(),
                        downstream_semantic: final_writer.semantic_fence(),
                    },
                    preexchange_transport_facts(selected, None),
                    cleanup,
                    &result,
                );
                let _ = finalize_preexchange_provider_facts(
                    &self.provider,
                    provider_state.as_mut(),
                    None,
                    &completion,
                );
            }
            driver.complete();
            return result.map(PreexchangeFailureOutcome::Completed);
        };
        if let Some(provider) = failure.provider.as_ref() {
            validate_provider_facts(provider)?;
        }
        let disposition = decision_session
            .decide_failure(selected, &failure)
            .map_err(GatewayExecutionError::Selection)?;
        if disposition == Disposition::Accept {
            let cleanup = finish_attempt_filter_scope_bounded(
                &mut request_filters.filters,
                self.limits.cleanup_timeout,
            )
            .await;
            return Err(cleanup
                .err()
                .unwrap_or(GatewayExecutionError::InvalidFailureDisposition));
        }
        // A provisional candidate failed before an AttemptExchange existed.
        // It must neither consume max_attempts nor enter the completed-Attempt
        // ledger. Selection may advance to another frozen candidate only when
        // the provider supplied explicit Retryable facts.
        if selected.attempt_id.0 == 0 {
            let filter_cleanup = finish_attempt_filter_scope_bounded(
                &mut request_filters.filters,
                self.limits.cleanup_timeout,
            )
            .await;
            let lease_cleanup = leases
                .lease_zero_for_continue()
                .map(|_| ())
                .map_err(GatewayExecutionError::Body);
            combine_cleanup_results(filter_cleanup, lease_cleanup)?;
            if disposition == Disposition::Continue {
                return Ok(PreexchangeFailureOutcome::Continue);
            }

            let published = PreexchangeDispositionGate::new(
                request_id,
                selected.attempt_id,
                selected.generation,
            )
            .publish(Disposition::Terminate)?;
            self.provider
                .release_terminal_request(
                    logical
                        .take()
                        .expect("candidate-preparation termination consumes logical owner once"),
                    &published,
                )
                .map_err(GatewayExecutionError::ProviderTerminalRelease)?;
            let _release_ready = leases.mark_terminal_and_release_ready()?;
            let result = emit_local_response(
                session,
                final_writer,
                &self.filter_executors,
                driver,
                binding,
                request_configs,
                &mut request_filters.filters,
                LocalReply {
                    status: StatusCode::BAD_GATEWAY,
                    headers: HeaderMap::new(),
                    body: Bytes::from_static(b"upstream candidate preparation failed"),
                    provenance: SemanticProvenance::NonSemantic,
                },
                budget,
                request_id,
                Some(route_binding),
                None,
                Some(selected.generation),
                deadline,
                cancellation,
                None,
                downstream_method,
                downstream_protocol,
                SessionReuse::Reusable,
            )
            .await;
            driver.complete();
            return result.map(PreexchangeFailureOutcome::Completed);
        }
        if let Some(telemetry) = &attempt_telemetry {
            telemetry.disposition(
                DispositionStage::Candidate,
                disposition,
                None,
                false,
                false,
                false,
                Duration::ZERO,
            );
        }
        let gate =
            PreexchangeDispositionGate::new(request_id, selected.attempt_id, selected.generation);
        driver.disposition_pending()?;
        let published = gate.publish(disposition)?;
        if let Some(telemetry) = &attempt_telemetry {
            telemetry.disposition(
                DispositionStage::Published,
                disposition,
                None,
                false,
                false,
                false,
                Duration::ZERO,
            );
        }
        let _ = decision_session.observe_published(&published);
        let mut stateful_filter_cleanup = if provider_state.is_some() {
            Some(
                finish_attempt_filter_scope_bounded(
                    &mut request_filters.filters,
                    self.limits.cleanup_timeout,
                )
                .await,
            )
        } else {
            None
        };

        if disposition == Disposition::Continue {
            let result = if let Some(filter_cleanup) = stateful_filter_cleanup.take() {
                let lease_result = leases
                    .lease_zero_for_continue()
                    .map(|_| ())
                    .map_err(GatewayExecutionError::Body);
                combine_cleanup_results(filter_cleanup, lease_result)
            } else {
                let lease_result = leases
                    .lease_zero_for_continue()
                    .map(|_| ())
                    .map_err(GatewayExecutionError::Body);
                let filter_cleanup = finish_attempt_filter_scope_bounded(
                    &mut request_filters.filters,
                    self.limits.cleanup_timeout,
                )
                .await;
                combine_cleanup_results(lease_result, filter_cleanup)
            };
            let cleanup = if result.is_ok() {
                AttemptCleanupOutcome::Completed
            } else {
                AttemptCleanupOutcome::Failed
            };
            let mut completion = attempt_completion_from_result(
                disposition,
                AttemptCommitFacts {
                    upstream_request: CommitFence::Clear,
                    downstream_headers: final_writer.header_fence(),
                    downstream_semantic: final_writer.semantic_fence(),
                },
                failure.transport.clone(),
                cleanup,
                &result,
            );
            if result.is_ok() {
                completion.termination_reason = AttemptTerminationReason::PreexchangeFailure;
            }
            let final_provider = finalize_preexchange_provider_facts(
                &self.provider,
                provider_state.as_mut(),
                failure.provider.as_ref(),
                &completion,
            );
            record_completed_attempt(
                decision_session,
                completed_attempts,
                selected,
                routing_facts,
                final_provider,
                Some(failure),
                completion,
            );
            if let Err(error) = result {
                driver.complete();
                return Err(error);
            }
            driver.continue_after_attempt()?;
            *generation = AttemptGeneration(generation.0.wrapping_add(1));
            return Ok(PreexchangeFailureOutcome::Continue);
        }

        let mut terminal_leases_released = false;
        let mut result: Result<SessionReuse, GatewayExecutionError> = async {
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
            emit_local_response(
                session,
                final_writer,
                &self.filter_executors,
                driver,
                binding,
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
        let lease_cleanup = if terminal_leases_released {
            Ok(())
        } else {
            leases
                .mark_terminal_and_release_ready()
                .map(|_| ())
                .map_err(GatewayExecutionError::Body)
        };
        let attempt_filter_cleanup = match stateful_filter_cleanup.take() {
            Some(cleanup) => cleanup,
            None => {
                finish_attempt_filter_scope_bounded(
                    &mut request_filters.filters,
                    self.limits.cleanup_timeout,
                )
                .await
            }
        };
        let accepted_filter_cleanup = finish_accepted_filter_scope_bounded(
            &mut request_filters.filters,
            self.limits.cleanup_timeout,
        )
        .await;
        let cleanup_result = combine_cleanup_results(
            lease_cleanup,
            combine_cleanup_results(attempt_filter_cleanup, accepted_filter_cleanup),
        );
        let cleanup = if cleanup_result.is_ok() {
            AttemptCleanupOutcome::Completed
        } else {
            AttemptCleanupOutcome::Failed
        };
        if result.is_ok()
            && let Err(error) = cleanup_result
        {
            result = Err(error);
        }
        let mut completion = attempt_completion_from_result(
            disposition,
            AttemptCommitFacts {
                upstream_request: CommitFence::Clear,
                downstream_headers: final_writer.header_fence(),
                downstream_semantic: final_writer.semantic_fence(),
            },
            failure.transport.clone(),
            cleanup,
            &result,
        );
        if result.is_ok() {
            completion.termination_reason = AttemptTerminationReason::PreexchangeFailure;
        }
        let final_provider = finalize_preexchange_provider_facts(
            &self.provider,
            provider_state.as_mut(),
            failure.provider.as_ref(),
            &completion,
        );
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
        result.map(PreexchangeFailureOutcome::Completed)
    }
}
