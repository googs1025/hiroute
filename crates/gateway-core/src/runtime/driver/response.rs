use super::*;

/// Request-wide owner for the only downstream final response. Attempt-local
/// fences still protect disposition publication, while this owner covers
/// logical local replies, normalized Terminate responses, and accepted
/// upstream responses uniformly. A failed I/O intentionally leaves the fence
/// in `WriteStartedMayHaveCommitted`.
pub(super) struct RequestFinalWriter {
    header_fence: CommitFence,
    semantic_fence: CommitFence,
    telemetry: Option<RequestTelemetry>,
}

impl RequestFinalWriter {
    pub(super) fn new(telemetry: Option<RequestTelemetry>) -> Self {
        Self {
            header_fence: CommitFence::Clear,
            semantic_fence: CommitFence::Clear,
            telemetry,
        }
    }

    pub(super) fn begin_header_write(&mut self) -> Result<(), AttemptError> {
        self.header_fence.begin_write()?;
        self.observe_fence(FenceKind::DownstreamFinalHeaders, self.header_fence);
        Ok(())
    }

    pub(super) fn confirm_header_write(&mut self) -> Result<(), AttemptError> {
        self.header_fence.confirm()?;
        self.observe_fence(FenceKind::DownstreamFinalHeaders, self.header_fence);
        Ok(())
    }

    pub(super) fn begin_semantic_write(&mut self) -> Result<(), AttemptError> {
        if self.header_fence != CommitFence::WriteConfirmed {
            return Err(AttemptError::DownstreamHeaderNotCommitted);
        }
        self.semantic_fence.begin_write()?;
        self.observe_fence(FenceKind::DownstreamSemanticOutput, self.semantic_fence);
        Ok(())
    }

    pub(super) fn confirm_semantic_write(&mut self) -> Result<(), AttemptError> {
        self.semantic_fence.confirm()?;
        self.observe_fence(FenceKind::DownstreamSemanticOutput, self.semantic_fence);
        Ok(())
    }

    pub(super) fn semantic_fence(&self) -> CommitFence {
        self.semantic_fence
    }

    pub(super) fn header_fence(&self) -> CommitFence {
        self.header_fence
    }

    pub(super) fn observe_fence(&self, kind: FenceKind, fence: CommitFence) {
        if let Some(telemetry) = &self.telemetry {
            telemetry.commit(kind, fence, 0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn emit_local_response<R: GatewayRequestFilterPort>(
    session: &mut dyn GatewaySession,
    final_writer: &mut RequestFinalWriter,
    executor_pool: &FilterExecutorPool,
    driver: &mut LogicalRequestDriver,
    binding: &mut RequestExecutionBinding,
    request_configs: &RequestConfigSnapshot,
    filters: &mut R,
    reply: LocalReply,
    budget: &StreamBudget,
    request_id: RequestId,
    route_binding: Option<ResolvedTargetBindingId>,
    attempt_id: Option<AttemptId>,
    generation: Option<AttemptGeneration>,
    deadline: Instant,
    cancellation: &CancellationToken,
    telemetry: Option<RequestTelemetry>,
    method: &Method,
    protocol: crate::transport::HttpProtocol,
    reuse: SessionReuse,
) -> Result<SessionReuse, GatewayExecutionError> {
    let plan_revision = binding.plan_revision();
    let accepted_binding = binding.take_accepted_response()?;
    let has_accepted_filters = !accepted_binding.plan().filters.is_empty();
    if driver.state() == RequestState::DispositionPending {
        driver.select_final_response()?;
    } else {
        driver.select_local_response()?;
    }
    driver.begin_accepted_response()?;
    if has_accepted_filters {
        let filter_configs = {
            let filter_phase = accepted_binding.acquire_phase_configs()?;
            let filter_event = accepted_binding.acquire_event_configs()?;
            materialize_filter_configs(
                &accepted_binding.plan().filters,
                None,
                request_configs,
                None,
                &filter_phase,
                &filter_event,
            )?
        };
        filters
            .begin_accepted_response(
                &accepted_binding.plan().filters,
                filter_scope_context(
                    request_id,
                    plan_revision,
                    route_binding,
                    attempt_id,
                    generation,
                    driver
                        .scope_id(ScopeKind::AcceptedResponse)
                        .ok_or(GatewayExecutionError::MissingFilterScope)?,
                    ScopeKind::AcceptedResponse,
                    deadline,
                    cancellation,
                    telemetry.clone(),
                    budget,
                    filter_configs,
                    executor_pool,
                ),
            )
            .map_err(GatewayExecutionError::Filter)?;
    }
    write_buffered_accepted_response(
        session,
        final_writer,
        filters,
        &accepted_binding,
        request_configs,
        reply,
        budget,
        method,
        protocol,
        has_accepted_filters,
        deadline,
        cancellation,
        telemetry.as_ref(),
    )
    .await?;
    driver.complete();
    Ok(reuse)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn emit_provider_terminal_response<P, R>(
    provider: &P,
    session: &mut dyn GatewaySession,
    final_writer: &mut RequestFinalWriter,
    executor_pool: &FilterExecutorPool,
    driver: &mut LogicalRequestDriver,
    binding: &mut RequestExecutionBinding,
    connection_configs: &ConfigScopeSnapshot,
    request_configs: &RequestConfigSnapshot,
    attempt_configs: &ConfigScopeSnapshot,
    filters: &mut R,
    readiness: &mut P::Readiness,
    published: &PublishedDisposition,
    normalized_local_reply: Option<LocalReply>,
    upstream_side_effects: UpstreamSideEffectSnapshot,
    budget: &StreamBudget,
    request_id: RequestId,
    selected: SelectedGatewayAttempt,
    deadline: Instant,
    cancellation: &CancellationToken,
    telemetry: Option<RequestTelemetry>,
    method: &Method,
    protocol: HttpProtocol,
) -> Result<SessionReuse, GatewayExecutionError>
where
    P: ProviderRuntimePort,
    R: GatewayRequestFilterPort,
{
    if !matches!(
        published.disposition,
        Disposition::Accept | Disposition::Terminate
    ) {
        return Err(GatewayExecutionError::TerminalEncoderDidNotComplete);
    }
    let plan_revision = binding.plan_revision();
    let accepted_binding = binding.take_accepted_response()?;
    let has_accepted_filters = !accepted_binding.plan().filters.is_empty();
    driver.select_final_response()?;
    driver.begin_accepted_response()?;

    if has_accepted_filters {
        let filter_configs = {
            let phase = accepted_binding.acquire_phase_configs()?;
            let event = accepted_binding.acquire_event_configs()?;
            materialize_filter_configs(
                &accepted_binding.plan().filters,
                Some(connection_configs),
                request_configs,
                Some(attempt_configs),
                &phase,
                &event,
            )?
        };
        filters
            .begin_accepted_response(
                &accepted_binding.plan().filters,
                filter_scope_context(
                    request_id,
                    plan_revision,
                    Some(selected.binding),
                    Some(selected.attempt_id),
                    Some(selected.generation),
                    driver
                        .scope_id(ScopeKind::AcceptedResponse)
                        .ok_or(GatewayExecutionError::MissingFilterScope)?,
                    ScopeKind::AcceptedResponse,
                    deadline,
                    cancellation,
                    telemetry.clone(),
                    budget,
                    filter_configs,
                    executor_pool,
                ),
            )
            .map_err(GatewayExecutionError::Filter)?;
    }

    let mut head = {
        let phase = accepted_binding.acquire_phase_configs()?;
        let configs = PinnedConfigContext {
            ids: &accepted_binding.plan().config_cell_ids,
            connection: connection_configs,
            request: request_configs,
            attempt: attempt_configs,
            phase: &phase,
        };
        provider
            .accepted_response_head(readiness, published, &accepted_binding, &configs)
            .map_err(GatewayExecutionError::Provider)?
    };
    let head_result = if has_accepted_filters {
        await_request_operation(session, cancellation, deadline, telemetry.as_ref(), async {
            filters
                .filter_accepted_head(&mut head)
                .await
                .map_err(GatewayExecutionError::Filter)
        })
        .await?
    } else {
        GatewayFilterResult::headers(None, None)
    };

    let frame = if let Some(reply) = head_result.local_reply {
        head = GatewayResponseHead {
            status: reply.status,
            headers: HeaderMap::new(),
        };
        let provenance = reply.provenance;
        AcceptedBodyFrame {
            output: Some(EncodedOutputUnit {
                bytes: ChargedBytes::copy_from_opaque(
                    budget,
                    MemoryRole::OutputQueue,
                    &reply.body,
                )?,
                provenance,
            }),
            end_stream: true,
            sse_sources: SseTransformSources::default(),
            queue_metadata: BodyMetadataOwner::default(),
        }
    } else {
        let normalized_provenance = normalized_local_reply
            .as_ref()
            .map(|reply| reply.provenance);
        let body = normalized_local_reply
            .as_ref()
            .map(|reply| {
                ChargedBytes::copy_from_opaque(budget, MemoryRole::OutputQueue, &reply.body)
            })
            .transpose()?;
        let phase = accepted_binding.acquire_phase_configs()?;
        let event_configs = accepted_binding.acquire_event_configs()?;
        let configs = PinnedConfigContext {
            ids: &accepted_binding.plan().config_cell_ids,
            connection: connection_configs,
            request: request_configs,
            attempt: attempt_configs,
            phase: &phase,
        };
        let frame = provider
            .encode_accepted_event(
                readiness,
                ProviderAcceptedEvent::Terminal {
                    body,
                    provenance: normalized_provenance.unwrap_or(SemanticProvenance::NonSemantic),
                    upstream_side_effects,
                },
                published,
                &accepted_binding,
                &configs,
                &event_configs,
            )
            .map_err(GatewayExecutionError::Provider)?
            .ok_or(GatewayExecutionError::TerminalEncoderDidNotComplete)?;
        if !frame.end_stream {
            return Err(GatewayExecutionError::TerminalEncoderDidNotComplete);
        }
        if let (Some(expected), Some(output)) = (normalized_provenance, frame.output.as_ref())
            && output.provenance != expected
        {
            return Err(GatewayExecutionError::InvalidLocalReply);
        }
        frame
    };

    let mut body_result = if has_accepted_filters {
        let filter_configs = {
            let phase = accepted_binding.acquire_phase_configs()?;
            let event = accepted_binding.acquire_event_configs()?;
            materialize_filter_configs(
                &accepted_binding.plan().filters,
                Some(connection_configs),
                request_configs,
                Some(attempt_configs),
                &phase,
                &event,
            )?
        };
        await_request_operation(session, cancellation, deadline, telemetry.as_ref(), async {
            filters
                .filter_accepted_body(&mut head, frame, filter_configs)
                .await
                .map_err(GatewayExecutionError::Filter)
        })
        .await?
    } else {
        GatewayFilterResult::forward(frame)
    };
    if body_result.pause.is_some() && body_result.frames.is_empty() {
        body_result =
            await_request_operation(session, cancellation, deadline, telemetry.as_ref(), async {
                filters
                    .wait_accepted_resume(&mut head)
                    .await
                    .map_err(GatewayExecutionError::Filter)
            })
            .await?;
    }
    if let Some(reply) = body_result.local_reply {
        head = GatewayResponseHead {
            status: reply.status,
            headers: HeaderMap::new(),
        };
        body_result = GatewayFilterResult::forward(AcceptedBodyFrame {
            output: Some(EncodedOutputUnit {
                bytes: ChargedBytes::copy_from_opaque(
                    budget,
                    MemoryRole::OutputQueue,
                    &reply.body,
                )?,
                provenance: reply.provenance,
            }),
            end_stream: true,
            sse_sources: SseTransformSources::default(),
            queue_metadata: BodyMetadataOwner::default(),
        });
    }

    write_buffered_final_frames(
        session,
        final_writer,
        head,
        body_result.frames,
        method,
        protocol,
        deadline,
        cancellation,
        telemetry.as_ref(),
    )
    .await?;
    driver.complete();
    Ok(SessionReuse::Reusable)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn write_buffered_accepted_response<R: GatewayRequestFilterPort>(
    session: &mut dyn GatewaySession,
    final_writer: &mut RequestFinalWriter,
    filters: &mut R,
    accepted_binding: &AcceptedResponseExecutionBinding,
    request_configs: &RequestConfigSnapshot,
    mut reply: LocalReply,
    budget: &StreamBudget,
    method: &Method,
    protocol: crate::transport::HttpProtocol,
    apply_filters: bool,
    deadline: Instant,
    cancellation: &CancellationToken,
    telemetry: Option<&RequestTelemetry>,
) -> Result<(), GatewayExecutionError> {
    let mut head = GatewayResponseHead {
        status: reply.status,
        headers: reply.headers.clone(),
    };
    let mut filter_body = apply_filters;
    let head_result = if apply_filters {
        await_request_operation(session, cancellation, deadline, telemetry, async {
            filters
                .filter_accepted_head(&mut head)
                .await
                .map_err(GatewayExecutionError::Filter)
        })
        .await?
    } else {
        GatewayFilterResult::headers(None, None)
    };
    if let Some(replacement) = head_result.local_reply {
        reply = replacement;
        head = GatewayResponseHead {
            status: reply.status,
            headers: reply.headers.clone(),
        };
        filter_body = false;
    }

    let frame = AcceptedBodyFrame {
        output: Some(EncodedOutputUnit {
            bytes: ChargedBytes::copy_from_opaque(budget, MemoryRole::OutputQueue, &reply.body)?,
            provenance: reply.provenance,
        }),
        end_stream: true,
        sse_sources: SseTransformSources::default(),
        queue_metadata: BodyMetadataOwner::default(),
    };
    let mut body_result = if filter_body {
        let filter_configs = {
            let filter_phase = accepted_binding.acquire_phase_configs()?;
            let filter_event = accepted_binding.acquire_event_configs()?;
            materialize_filter_configs(
                &accepted_binding.plan().filters,
                None,
                request_configs,
                None,
                &filter_phase,
                &filter_event,
            )?
        };
        await_request_operation(session, cancellation, deadline, telemetry, async {
            filters
                .filter_accepted_body(&mut head, frame, filter_configs)
                .await
                .map_err(GatewayExecutionError::Filter)
        })
        .await?
    } else {
        GatewayFilterResult::forward(frame)
    };
    if body_result.pause.is_some() && body_result.frames.is_empty() {
        body_result = await_request_operation(session, cancellation, deadline, telemetry, async {
            filters
                .wait_accepted_resume(&mut head)
                .await
                .map_err(GatewayExecutionError::Filter)
        })
        .await?;
    }
    if let Some(replacement) = body_result.local_reply {
        reply = replacement;
        head = GatewayResponseHead {
            status: reply.status,
            headers: HeaderMap::new(),
        };
        body_result = GatewayFilterResult::forward(AcceptedBodyFrame {
            output: Some(EncodedOutputUnit {
                bytes: ChargedBytes::copy_from_opaque(
                    budget,
                    MemoryRole::OutputQueue,
                    &reply.body,
                )?,
                provenance: reply.provenance,
            }),
            end_stream: true,
            sse_sources: SseTransformSources::default(),
            queue_metadata: BodyMetadataOwner::default(),
        });
    }

    write_buffered_final_frames(
        session,
        final_writer,
        head,
        body_result.frames,
        method,
        protocol,
        deadline,
        cancellation,
        telemetry,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn write_buffered_final_frames(
    session: &mut dyn GatewaySession,
    final_writer: &mut RequestFinalWriter,
    mut head: GatewayResponseHead,
    frames: BodyEmitterOutcome<AcceptedBodyFrame>,
    method: &Method,
    protocol: crate::transport::HttpProtocol,
    deadline: Instant,
    cancellation: &CancellationToken,
    telemetry: Option<&RequestTelemetry>,
) -> Result<(), GatewayExecutionError> {
    let body_forbidden = response_body_forbidden(method, head.status);
    let exact = if body_forbidden {
        0
    } else {
        frames
            .iter()
            .try_fold(0_usize, |total, frame| {
                total.checked_add(
                    frame
                        .output
                        .as_ref()
                        .map_or(0, |output| output.bytes.bytes().len()),
                )
            })
            .ok_or(BodyError::BodyLimitExceeded)?
    };
    let mut framing = FramingLedger::default();
    framing.buffered_eos(exact)?;
    framing.finalize(
        &mut head.headers,
        match protocol {
            crate::transport::HttpProtocol::Http1 => HttpFraming::Http1,
            crate::transport::HttpProtocol::Http2 => HttpFraming::Http2,
        },
        Some(method),
        Some(head.status),
    )?;
    final_writer.begin_header_write()?;
    await_session_operation(cancellation, deadline, telemetry, async {
        session
            .write_response_head(head)
            .await
            .map_err(GatewayExecutionError::Transport)
    })
    .await?;
    final_writer.confirm_header_write()?;
    if frames.is_empty() {
        await_session_operation(cancellation, deadline, telemetry, async {
            session
                .write_response_body(Bytes::new(), true)
                .await
                .map_err(GatewayExecutionError::Transport)
        })
        .await?;
    } else {
        let last = frames.len().saturating_sub(1);
        for (index, frame) in frames.into_iter().enumerate() {
            let semantic = !body_forbidden
                && frame.output.as_ref().is_some_and(|output| {
                    output.provenance == SemanticProvenance::ProducesSemantic
                        && !output.bytes.bytes().is_empty()
                });
            if semantic && final_writer.semantic_fence() == CommitFence::Clear {
                final_writer.begin_semantic_write()?;
            }
            let wire_body = if body_forbidden {
                None
            } else {
                frame.output.map(|output| output.bytes)
            };
            await_session_operation(cancellation, deadline, telemetry, async {
                session
                    .write_response_body_charged(wire_body, index == last)
                    .await
                    .map_err(GatewayExecutionError::Transport)
            })
            .await?;
            if semantic
                && final_writer.semantic_fence() == CommitFence::WriteStartedMayHaveCommitted
            {
                final_writer.confirm_semantic_write()?;
            }
        }
    }
    Ok(())
}

pub(super) fn response_body_forbidden(method: &Method, status: StatusCode) -> bool {
    method == Method::HEAD
        || status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
}

pub(super) fn protocol_framing(protocol: HttpProtocol) -> HttpFraming {
    match protocol {
        HttpProtocol::Http1 => HttpFraming::Http1,
        HttpProtocol::Http2 => HttpFraming::Http2,
    }
}

pub(super) fn record_framing_mutations(
    framing: &mut FramingLedger,
    headers: &HeaderMap,
    original_content_length: Option<&http::HeaderValue>,
    original_transfer_encoding: Option<&http::HeaderValue>,
) {
    if headers.get(CONTENT_LENGTH) != original_content_length {
        framing.record_header_mutation(&CONTENT_LENGTH);
    }
    if headers.get(TRANSFER_ENCODING) != original_transfer_encoding {
        framing.record_header_mutation(&TRANSFER_ENCODING);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_final_writer_keeps_partial_io_in_may_have_committed_state() {
        let mut writer = RequestFinalWriter::new(None);
        writer.begin_header_write().expect("begin final head");
        assert_eq!(
            writer.header_fence,
            CommitFence::WriteStartedMayHaveCommitted
        );
        assert_eq!(
            writer.begin_header_write().unwrap_err(),
            AttemptError::FenceAlreadyAdvanced,
            "a possible partial status line forbids a second response head",
        );
        writer.confirm_header_write().expect("confirm final head");
        writer.begin_semantic_write().expect("begin semantic body");
        assert_eq!(
            writer.semantic_fence(),
            CommitFence::WriteStartedMayHaveCommitted,
        );
        assert_eq!(
            writer.begin_semantic_write().unwrap_err(),
            AttemptError::FenceAlreadyAdvanced,
        );
    }
}
