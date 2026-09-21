use super::*;

impl<S, P, F, T> GatewayCoreLifecycle<S, P, F, T>
where
    P: ProviderRuntimePort,
    S: SelectionPublicationPort<P::RouteRequestContext>,
    F: GatewayFilterManagerPort,
    T: AttemptTransportFactory,
{
    pub(super) async fn complete_continue(
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
            request_filters,
            driver,
            final_writer,
            decision_session,
            generation,
            completed_attempts,
            selected,
            routing_facts,
            mut provider_state,
            mut readiness,
            provider_facts,
            mut exchange,
            ..
        } = attempt;

        let exchange_cleanup =
            finish_attempt_bounded(&mut exchange, self.limits.cleanup_timeout).await;
        let filter_cleanup = finish_attempt_filter_scope_bounded(
            &mut request_filters.filters,
            self.limits.cleanup_timeout,
        )
        .await;
        let cleanup_result = combine_cleanup_results(exchange_cleanup, filter_cleanup);
        let cleanup = if cleanup_result.is_ok() {
            AttemptCleanupOutcome::Completed
        } else {
            AttemptCleanupOutcome::Failed
        };
        let completion = attempt_completion_from_result(
            disposition,
            attempt_commit_facts(&exchange, final_writer),
            exchange.transport_facts(),
            cleanup,
            &cleanup_result,
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
        if let Err(error) = cleanup_result {
            driver.complete();
            return Err(error);
        }
        driver.continue_after_attempt()?;
        *generation = AttemptGeneration(generation.0.wrapping_add(1));

        Ok(AttemptCompletionOutcome::Continue)
    }
}
