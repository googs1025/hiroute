use super::*;

impl<S, P, F, T> GatewayCoreLifecycle<S, P, F, T>
where
    P: ProviderRuntimePort,
    S: SelectionPublicationPort<P::RouteRequestContext>,
    F: GatewayFilterManagerPort,
    T: AttemptTransportFactory,
{
    pub(super) async fn complete_terminate(
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
            connection_configs,
            attempt_configs,
            ..
        } = attempt;

        let attempt_filter_cleanup = finish_attempt_filter_scope_bounded(
            &mut request_filters.filters,
            self.limits.cleanup_timeout,
        )
        .await;
        let mut terminal_result: Result<SessionReuse, GatewayExecutionError> = async {
            exchange.begin_accepted_response_scope()?;
            let upstream_side_effects = UpstreamSideEffectSnapshot::from(exchange.snapshot());
            finish_attempt_bounded(&mut exchange, self.limits.cleanup_timeout).await?;
            let readiness = readiness
                .as_mut()
                .ok_or(GatewayExecutionError::AcceptedWithoutReadiness)?;
            emit_provider_terminal_response(
                &self.provider,
                session,
                &mut *final_writer,
                &self.filter_executors,
                &mut *driver,
                &mut *binding,
                &connection_configs,
                request_configs,
                &attempt_configs,
                &mut request_filters.filters,
                readiness,
                &published,
                filter_local_reply,
                upstream_side_effects,
                budget,
                request_id,
                selected.clone(),
                deadline,
                cancellation,
                attempt_telemetry.clone(),
                downstream_method,
                downstream_protocol,
            )
            .await
        }
        .await;
        let exchange_cleanup = if exchange.snapshot().finalized {
            Ok(())
        } else {
            finish_attempt_bounded(&mut exchange, self.limits.cleanup_timeout).await
        };
        let accepted_filter_cleanup = finish_accepted_filter_scope_bounded(
            &mut request_filters.filters,
            self.limits.cleanup_timeout,
        )
        .await;
        let cleanup_result = combine_cleanup_results(
            exchange_cleanup,
            combine_cleanup_results(attempt_filter_cleanup, accepted_filter_cleanup),
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
            attempt_commit_facts(&exchange, final_writer),
            exchange.transport_facts(),
            cleanup,
            &terminal_result,
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
        terminal_result.map(AttemptCompletionOutcome::Completed)
    }
}
