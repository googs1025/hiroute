use super::*;

#[async_trait]
impl GatewayRequestFilterPort for NativeGatewayRequestFilters {
    async fn begin_logical_request(
        &mut self,
        descriptors: &[CompiledFilterDescriptor],
        context: GatewayFilterScopeContext,
        head: &mut GatewayRequestHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>> {
        Self::finish_machine(&mut self.logical);
        Self::release_pending_queue(
            &mut self.logical_pending,
            &mut self.logical_pending_reservation,
        );
        let (pending, pending_reservation) = Self::allocate_pending_queue::<
            PendingFilterFrame<LogicalRequestBodyFrame>,
        >(descriptors, &context.budget)?;
        let machine = self.build_machine(descriptors, context.clone(), false)?;
        self.logical_pending = pending;
        self.logical_pending_reservation = pending_reservation;
        self.logical_budget = Some(context.budget.clone());
        self.logical = machine;
        let Some(machine) = self.logical.as_mut() else {
            return Ok(GatewayFilterResult::headers(None, None));
        };
        let outcome = machine
            .on_headers(head.headers.clone(), false)
            .await
            .map_err(filter_error)?;
        head.headers = machine.held_headers().clone();
        Ok(Self::resolve_headers(machine, outcome))
    }

    async fn filter_logical_request_body(
        &mut self,
        head: &mut GatewayRequestHead,
        frame: LogicalRequestBodyFrame,
        configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<LogicalRequestBodyFrame>, Arc<str>> {
        if self.logical.is_none() {
            return Ok(GatewayFilterResult::forward(frame));
        }
        let source = self.allocate_body_source();
        let machine = self.logical.as_mut().expect("checked logical machine");
        machine.synchronize_held_headers(&head.headers);
        machine.set_filter_configs(configs);
        let bytes = frame
            .bytes
            .as_ref()
            .map_or_else(Bytes::new, |bytes| bytes.bytes().clone());
        let end_stream = frame.end_stream;
        let queue_metadata = frame.queue_metadata.clone();
        Self::push_pending(
            &mut self.logical_pending,
            &self.logical_pending_reservation,
            PendingFilterFrame {
                source: Some(source.clone()),
                frame,
            },
        )?;
        let outcome = machine
            .on_data_with_source_and_metadata(bytes, end_stream, source, queue_metadata)
            .await
            .map_err(filter_error)?;
        head.headers = machine.held_headers().clone();
        Ok(Self::resolve_logical_frames(
            machine,
            &mut self.logical_pending,
            self.logical_budget
                .as_ref()
                .ok_or_else(|| Arc::from("logical filter budget is not active"))?,
            outcome,
        )?)
    }

    async fn try_logical_resume(
        &mut self,
        head: &mut GatewayRequestHead,
    ) -> Result<Option<GatewayFilterResult<LogicalRequestBodyFrame>>, Arc<str>> {
        let machine = self
            .logical
            .as_mut()
            .ok_or_else(|| Arc::from("logical filter machine is not active"))?;
        let Some(outcome) = machine.try_resume_signalled().await.map_err(filter_error)? else {
            return Ok(None);
        };
        head.headers = machine.held_headers().clone();
        Ok(Some(Self::resolve_logical_frames(
            machine,
            &mut self.logical_pending,
            self.logical_budget
                .as_ref()
                .ok_or_else(|| Arc::from("logical filter budget is not active"))?,
            outcome,
        )?))
    }

    fn begin_attempt(
        &mut self,
        request_descriptors: &[CompiledFilterDescriptor],
        response_descriptors: &[CompiledFilterDescriptor],
        context: GatewayFilterScopeContext,
    ) -> Result<(), Arc<str>> {
        Self::finish_machine(&mut self.attempt_request);
        Self::finish_machine(&mut self.attempt_response);
        Self::release_pending_queue(
            &mut self.attempt_request_pending,
            &mut self.attempt_request_pending_reservation,
        );
        Self::release_pending_queue(
            &mut self.attempt_response_pending,
            &mut self.attempt_response_pending_reservation,
        );
        self.attempt_response_sources = None;
        let (request_pending, request_pending_reservation) =
            Self::allocate_pending_queue::<PendingFilterFrame<AttemptRequestBodyFrame>>(
                request_descriptors,
                &context.budget,
            )?;
        let (response_pending, response_pending_reservation) =
            Self::allocate_pending_queue::<PendingFilterFrame<PrecommitEvent>>(
                response_descriptors,
                &context.budget,
            )?;
        let request_machine = self.build_machine(request_descriptors, context.clone(), true)?;
        let response_machine = self.build_machine(response_descriptors, context.clone(), false)?;
        let response_sources = (!response_descriptors.is_empty())
            .then(|| AttemptFilterSourceLedger::new(response_descriptors, &context.budget))
            .transpose()?;
        self.attempt_request_pending = request_pending;
        self.attempt_request_pending_reservation = request_pending_reservation;
        self.attempt_response_pending = response_pending;
        self.attempt_response_pending_reservation = response_pending_reservation;
        self.attempt_response_sources = response_sources;
        self.attempt_budget = Some(context.budget.clone());
        self.attempt_request = request_machine;
        self.attempt_response = response_machine;
        Ok(())
    }

    async fn filter_attempt_request_head(
        &mut self,
        head: &mut PreparedRequestHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>> {
        let Some(machine) = self.attempt_request.as_mut() else {
            return Ok(GatewayFilterResult::headers(None, None));
        };
        let outcome = machine
            .on_headers(head.headers.clone(), false)
            .await
            .map_err(filter_error)?;
        head.headers = machine.held_headers().clone();
        Ok(Self::resolve_headers(machine, outcome))
    }

    async fn filter_attempt_request_body(
        &mut self,
        head: &mut PreparedRequestHead,
        frame: AttemptRequestBodyFrame,
        configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<AttemptRequestBodyFrame>, Arc<str>> {
        if self.attempt_request.is_none() {
            return Ok(GatewayFilterResult::forward(frame));
        }
        let source = self.allocate_body_source();
        let machine = self
            .attempt_request
            .as_mut()
            .expect("checked attempt request machine");
        machine.synchronize_held_headers(&head.headers);
        machine.set_filter_configs(configs);
        let bytes = frame
            .bytes
            .as_ref()
            .map_or_else(Bytes::new, |bytes| bytes.bytes().clone());
        let end_stream = frame.end_stream;
        let queue_metadata = frame.queue_metadata.clone();
        Self::push_pending(
            &mut self.attempt_request_pending,
            &self.attempt_request_pending_reservation,
            PendingFilterFrame {
                source: Some(source.clone()),
                frame,
            },
        )?;
        let outcome = machine
            .on_data_with_source_and_metadata(bytes, end_stream, source, queue_metadata)
            .await
            .map_err(filter_error)?;
        head.headers = machine.held_headers().clone();
        Ok(Self::resolve_attempt_request_frames(
            machine,
            &mut self.attempt_request_pending,
            self.attempt_budget
                .as_ref()
                .ok_or_else(|| Arc::from("attempt filter budget is not active"))?,
            outcome,
        )?)
    }

    async fn filter_attempt_response_event(
        &mut self,
        event: PrecommitEvent,
        configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<PrecommitEvent>, Arc<str>> {
        if self.attempt_response.is_none() {
            return Ok(GatewayFilterResult::forward(event));
        }
        let source = if matches!(event, PrecommitEvent::ResponseHead(_)) {
            None
        } else {
            self.next_body_source = self.next_body_source.wrapping_add(1);
            Some(
                self.attempt_response_sources
                    .as_mut()
                    .ok_or_else(|| Arc::from("attempt response source ledger is not active"))?
                    .insert(self.next_body_source, &event)?,
            )
        };
        let machine = self
            .attempt_response
            .as_mut()
            .expect("checked attempt response machine");
        machine.set_filter_configs(configs);
        let outcome = match &event {
            PrecommitEvent::ResponseHead(head) if !head.status().is_informational() => machine
                .on_headers(head.headers().clone(), false)
                .await
                .map_err(filter_error)?,
            PrecommitEvent::ResponseHead(_) if machine.pause().is_some() => {
                Self::push_pending(
                    &mut self.attempt_response_pending,
                    &self.attempt_response_pending_reservation,
                    PendingFilterFrame {
                        source: None,
                        frame: event,
                    },
                )?;
                return Ok(GatewayFilterResult {
                    frames: BodyEmitterOutcome::drop_input(),
                    local_reply: None,
                    pause: machine.pause(),
                });
            }
            PrecommitEvent::ResponseHead(_) => {
                return Ok(GatewayFilterResult::forward(event));
            }
            PrecommitEvent::Body(body) => machine
                .on_data_with_source_and_metadata(
                    body.bytes().clone(),
                    false,
                    source.as_ref().expect("body event source").clone(),
                    body.metadata(),
                )
                .await
                .map_err(filter_error)?,
            PrecommitEvent::SseEvent { bytes, .. } => machine
                .on_data_with_source_and_metadata(
                    bytes.bytes().clone(),
                    false,
                    source.as_ref().expect("SSE event source").clone(),
                    bytes.metadata(),
                )
                .await
                .map_err(filter_error)?,
            PrecommitEvent::EndStream => machine
                .on_data_with_source_and_metadata(
                    Bytes::new(),
                    true,
                    source.as_ref().expect("end-stream source").clone(),
                    BodyMetadataOwner::default(),
                )
                .await
                .map_err(filter_error)?,
        };
        Self::push_pending(
            &mut self.attempt_response_pending,
            &self.attempt_response_pending_reservation,
            PendingFilterFrame {
                source,
                frame: event,
            },
        )?;
        let mut result = Self::resolve_attempt_frames(
            machine,
            &mut self.attempt_response_pending,
            self.attempt_response_sources
                .as_mut()
                .ok_or_else(|| Arc::from("attempt response source ledger is not active"))?,
            self.attempt_budget
                .as_ref()
                .ok_or_else(|| Arc::from("attempt filter budget is not active"))?,
            outcome,
        )?;
        Self::rewrite_attempt_head(machine, &mut result.frames);
        Ok(result)
    }

    fn finish_attempt(&mut self) {
        Self::finish_machine(&mut self.attempt_request);
        Self::finish_machine(&mut self.attempt_response);
        Self::release_pending_queue(
            &mut self.attempt_request_pending,
            &mut self.attempt_request_pending_reservation,
        );
        Self::release_pending_queue(
            &mut self.attempt_response_pending,
            &mut self.attempt_response_pending_reservation,
        );
        self.attempt_response_sources = None;
        self.attempt_budget = None;
    }

    async fn finish_attempt_bounded(&mut self, join_timeout: Duration) -> Result<(), Arc<str>> {
        let request_cleanup =
            Self::finish_machine_bounded(&mut self.attempt_request, join_timeout).await;
        let response_cleanup =
            Self::finish_machine_bounded(&mut self.attempt_response, join_timeout).await;
        Self::release_pending_queue(
            &mut self.attempt_request_pending,
            &mut self.attempt_request_pending_reservation,
        );
        Self::release_pending_queue(
            &mut self.attempt_response_pending,
            &mut self.attempt_response_pending_reservation,
        );
        self.attempt_response_sources = None;
        self.attempt_budget = None;
        request_cleanup.and(response_cleanup)
    }

    fn begin_accepted_response(
        &mut self,
        descriptors: &[CompiledFilterDescriptor],
        context: GatewayFilterScopeContext,
    ) -> Result<(), Arc<str>> {
        Self::finish_machine(&mut self.accepted);
        Self::release_pending_queue(
            &mut self.accepted_pending,
            &mut self.accepted_pending_reservation,
        );
        self.accepted_sources = None;
        let (pending, pending_reservation) = Self::allocate_pending_queue::<
            PendingFilterFrame<AcceptedBodyFrame>,
        >(descriptors, &context.budget)?;
        let sources = AcceptedFilterSourceLedger::new(
            descriptors,
            &context.budget,
            context.accepted_body_plan.as_ref(),
        )?;
        let machine = self.build_machine(descriptors, context.clone(), true)?;
        self.accepted_pending = pending;
        self.accepted_pending_reservation = pending_reservation;
        self.accepted_sources = Some(sources);
        self.accepted_budget = Some(context.budget.clone());
        self.accepted = machine;
        Ok(())
    }

    async fn filter_accepted_head(
        &mut self,
        head: &mut GatewayResponseHead,
    ) -> Result<GatewayFilterResult<()>, Arc<str>> {
        let Some(machine) = self.accepted.as_mut() else {
            return Ok(GatewayFilterResult::headers(None, None));
        };
        let outcome = machine
            .on_headers(head.headers.clone(), false)
            .await
            .map_err(filter_error)?;
        head.headers = machine.held_headers().clone();
        Ok(Self::resolve_headers(machine, outcome))
    }

    async fn filter_accepted_body(
        &mut self,
        head: &mut GatewayResponseHead,
        frame: AcceptedBodyFrame,
        configs: FilterConfigSnapshot,
    ) -> Result<GatewayFilterResult<AcceptedBodyFrame>, Arc<str>> {
        if self.accepted.is_none() {
            return Ok(GatewayFilterResult::forward(frame));
        }
        self.next_body_source = self.next_body_source.wrapping_add(1);
        let source = self
            .accepted_sources
            .as_mut()
            .ok_or_else(|| Arc::from("accepted source ledger is not active"))?
            .insert(self.next_body_source, &frame)?;
        let machine = self.accepted.as_mut().expect("checked accepted machine");
        machine.synchronize_held_headers(&head.headers);
        machine.set_filter_configs(configs);
        let bytes = frame
            .output
            .as_ref()
            .map_or_else(Bytes::new, |output| output.bytes.bytes().clone());
        let end_stream = frame.end_stream;
        let queue_metadata = frame.queue_metadata.clone();
        Self::push_pending(
            &mut self.accepted_pending,
            &self.accepted_pending_reservation,
            PendingFilterFrame {
                source: Some(source.clone()),
                frame,
            },
        )?;
        let outcome = machine
            .on_data_with_source_and_metadata(bytes, end_stream, source, queue_metadata)
            .await
            .map_err(filter_error)?;
        head.headers = machine.held_headers().clone();
        Ok(Self::resolve_accepted_frames(
            machine,
            &mut self.accepted_pending,
            self.accepted_sources
                .as_mut()
                .ok_or_else(|| Arc::from("accepted source ledger is not active"))?,
            self.accepted_budget
                .as_ref()
                .ok_or_else(|| Arc::from("accepted filter budget is not active"))?,
            outcome,
        )?)
    }

    async fn wait_logical_resume(
        &mut self,
        head: &mut GatewayRequestHead,
    ) -> Result<GatewayFilterResult<LogicalRequestBodyFrame>, Arc<str>> {
        let machine = self
            .logical
            .as_mut()
            .ok_or_else(|| Arc::from("logical filter machine is not active"))?;
        let outcome = machine
            .wait_for_signalled_resume()
            .await
            .map_err(filter_error)?;
        head.headers = machine.held_headers().clone();
        Ok(Self::resolve_logical_frames(
            machine,
            &mut self.logical_pending,
            self.logical_budget
                .as_ref()
                .ok_or_else(|| Arc::from("logical filter budget is not active"))?,
            outcome,
        )?)
    }

    async fn wait_attempt_request_resume(
        &mut self,
        head: &mut PreparedRequestHead,
    ) -> Result<GatewayFilterResult<AttemptRequestBodyFrame>, Arc<str>> {
        let machine = self
            .attempt_request
            .as_mut()
            .ok_or_else(|| Arc::from("attempt request filter machine is not active"))?;
        let outcome = machine
            .wait_for_signalled_resume()
            .await
            .map_err(filter_error)?;
        head.headers = machine.held_headers().clone();
        Ok(Self::resolve_attempt_request_frames(
            machine,
            &mut self.attempt_request_pending,
            self.attempt_budget
                .as_ref()
                .ok_or_else(|| Arc::from("attempt filter budget is not active"))?,
            outcome,
        )?)
    }

    async fn try_attempt_request_resume(
        &mut self,
        head: &mut PreparedRequestHead,
    ) -> Result<Option<GatewayFilterResult<AttemptRequestBodyFrame>>, Arc<str>> {
        let machine = self
            .attempt_request
            .as_mut()
            .ok_or_else(|| Arc::from("attempt request filter machine is not active"))?;
        let Some(outcome) = machine.try_resume_signalled().await.map_err(filter_error)? else {
            return Ok(None);
        };
        head.headers = machine.held_headers().clone();
        Ok(Some(Self::resolve_attempt_request_frames(
            machine,
            &mut self.attempt_request_pending,
            self.attempt_budget
                .as_ref()
                .ok_or_else(|| Arc::from("attempt filter budget is not active"))?,
            outcome,
        )?))
    }

    async fn wait_attempt_response_resume(
        &mut self,
    ) -> Result<GatewayFilterResult<PrecommitEvent>, Arc<str>> {
        let machine = self
            .attempt_response
            .as_mut()
            .ok_or_else(|| Arc::from("attempt response filter machine is not active"))?;
        let outcome = machine
            .wait_for_signalled_resume()
            .await
            .map_err(filter_error)?;
        let mut result = Self::resolve_attempt_frames(
            machine,
            &mut self.attempt_response_pending,
            self.attempt_response_sources
                .as_mut()
                .ok_or_else(|| Arc::from("attempt response source ledger is not active"))?,
            self.attempt_budget
                .as_ref()
                .ok_or_else(|| Arc::from("attempt filter budget is not active"))?,
            outcome,
        )?;
        Self::rewrite_attempt_head(machine, &mut result.frames);
        Ok(result)
    }

    async fn try_attempt_response_resume(
        &mut self,
    ) -> Result<Option<GatewayFilterResult<PrecommitEvent>>, Arc<str>> {
        let machine = self
            .attempt_response
            .as_mut()
            .ok_or_else(|| Arc::from("attempt response filter machine is not active"))?;
        let Some(outcome) = machine.try_resume_signalled().await.map_err(filter_error)? else {
            return Ok(None);
        };
        let mut result = Self::resolve_attempt_frames(
            machine,
            &mut self.attempt_response_pending,
            self.attempt_response_sources
                .as_mut()
                .ok_or_else(|| Arc::from("attempt response source ledger is not active"))?,
            self.attempt_budget
                .as_ref()
                .ok_or_else(|| Arc::from("attempt filter budget is not active"))?,
            outcome,
        )?;
        Self::rewrite_attempt_head(machine, &mut result.frames);
        Ok(Some(result))
    }

    async fn wait_accepted_resume(
        &mut self,
        head: &mut GatewayResponseHead,
    ) -> Result<GatewayFilterResult<AcceptedBodyFrame>, Arc<str>> {
        let machine = self
            .accepted
            .as_mut()
            .ok_or_else(|| Arc::from("accepted filter machine is not active"))?;
        let outcome = machine
            .wait_for_signalled_resume()
            .await
            .map_err(filter_error)?;
        head.headers = machine.held_headers().clone();
        Ok(Self::resolve_accepted_frames(
            machine,
            &mut self.accepted_pending,
            self.accepted_sources
                .as_mut()
                .ok_or_else(|| Arc::from("accepted source ledger is not active"))?,
            self.accepted_budget
                .as_ref()
                .ok_or_else(|| Arc::from("accepted filter budget is not active"))?,
            outcome,
        )?)
    }

    async fn try_accepted_resume(
        &mut self,
        head: &mut GatewayResponseHead,
    ) -> Result<Option<GatewayFilterResult<AcceptedBodyFrame>>, Arc<str>> {
        let machine = self
            .accepted
            .as_mut()
            .ok_or_else(|| Arc::from("accepted filter machine is not active"))?;
        let Some(outcome) = machine.try_resume_signalled().await.map_err(filter_error)? else {
            return Ok(None);
        };
        head.headers = machine.held_headers().clone();
        Ok(Some(Self::resolve_accepted_frames(
            machine,
            &mut self.accepted_pending,
            self.accepted_sources
                .as_mut()
                .ok_or_else(|| Arc::from("accepted source ledger is not active"))?,
            self.accepted_budget
                .as_ref()
                .ok_or_else(|| Arc::from("accepted filter budget is not active"))?,
            outcome,
        )?))
    }

    async fn finish_accepted_response_bounded(
        &mut self,
        join_timeout: Duration,
    ) -> Result<(), Arc<str>> {
        let cleanup = Self::finish_machine_bounded(&mut self.accepted, join_timeout).await;
        Self::release_pending_queue(
            &mut self.accepted_pending,
            &mut self.accepted_pending_reservation,
        );
        self.accepted_sources = None;
        self.accepted_budget = None;
        cleanup
    }

    fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        Self::finish_machine(&mut self.logical);
        Self::release_pending_queue(
            &mut self.logical_pending,
            &mut self.logical_pending_reservation,
        );
        self.logical_budget = None;
        Self::finish_machine(&mut self.attempt_request);
        Self::finish_machine(&mut self.attempt_response);
        Self::release_pending_queue(
            &mut self.attempt_request_pending,
            &mut self.attempt_request_pending_reservation,
        );
        Self::release_pending_queue(
            &mut self.attempt_response_pending,
            &mut self.attempt_response_pending_reservation,
        );
        self.attempt_response_sources = None;
        self.attempt_budget = None;
        Self::finish_machine(&mut self.accepted);
        Self::release_pending_queue(
            &mut self.accepted_pending,
            &mut self.accepted_pending_reservation,
        );
        self.accepted_sources = None;
        self.accepted_budget = None;
        self.finalized = true;
    }

    async fn finalize_bounded(&mut self, join_timeout: Duration) -> Result<(), Arc<str>> {
        if self.finalized {
            return Ok(());
        }
        for machine in [
            &mut self.logical,
            &mut self.attempt_request,
            &mut self.attempt_response,
            &mut self.accepted,
        ] {
            if let Some(machine) = machine.as_mut() {
                machine
                    .finalize_bounded(join_timeout)
                    .await
                    .map_err(filter_error)?;
            }
        }
        self.finalize();
        Ok(())
    }
}
