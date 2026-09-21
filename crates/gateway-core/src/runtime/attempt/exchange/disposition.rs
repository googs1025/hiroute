use super::*;

impl<T: AttemptTransport> AttemptExchange<T> {
    pub fn submit_disposition_candidate(
        &mut self,
        disposition: Disposition,
    ) -> Result<(), AttemptError> {
        self.ensure_active()?;
        if self.published.is_some() {
            return Err(AttemptError::DispositionAlreadyPublished);
        }
        self.candidate = Some(disposition);
        self.accept_blocked = false;
        if let Some(telemetry) = &self.telemetry {
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
        Ok(())
    }

    pub async fn wait_writer_gate(
        &mut self,
        deadline: Instant,
    ) -> Result<WriterGate, AttemptError> {
        self.ensure_active()?;
        let candidate = self.candidate.ok_or(AttemptError::NoDispositionCandidate)?;
        self.ensure_publication_fences_clear()?;
        let gate_started = Instant::now();
        match candidate {
            Disposition::Accept => {
                while self.writer_state != WriterState::QuiescedNormalEos {
                    if let Some(reason) = self.accept_progress_blocked_reason(deadline) {
                        self.accept_blocked = true;
                        self.observe_gate(
                            DispositionStage::AcceptBlocked,
                            candidate,
                            None,
                            gate_started.elapsed(),
                        );
                        return Ok(WriterGate::AcceptBlocked { reason });
                    }
                    let remaining = deadline
                        .checked_duration_since(Instant::now())
                        .unwrap_or_default();
                    let cancellation = self.cancellation.clone();
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => {
                            self.accept_blocked = true;
                            self.observe_gate(
                                DispositionStage::AcceptBlocked,
                                candidate,
                                None,
                                gate_started.elapsed(),
                            );
                            return Ok(WriterGate::AcceptBlocked {
                                reason: AcceptBlockedReason::Cancelled,
                            });
                        }
                        _ = tokio::time::sleep(remaining) => {
                            self.accept_blocked = true;
                            self.observe_gate(
                                DispositionStage::AcceptBlocked,
                                candidate,
                                None,
                                gate_started.elapsed(),
                            );
                            return Ok(WriterGate::AcceptBlocked {
                                reason: AcceptBlockedReason::Deadline,
                            });
                        }
                        result = self.drive_writer_once() => result?,
                    }
                }
                if !self.response_live {
                    self.accept_blocked = true;
                    self.observe_gate(
                        DispositionStage::AcceptBlocked,
                        candidate,
                        None,
                        gate_started.elapsed(),
                    );
                    return Ok(WriterGate::AcceptBlocked {
                        reason: AcceptBlockedReason::ResponseNotLive,
                    });
                }
                let permit = self.new_permit(Disposition::Accept);
                self.observe_gate(
                    DispositionStage::GateReady,
                    candidate,
                    Some(RequestCloseMode::NormalEos),
                    gate_started.elapsed(),
                );
                Ok(WriterGate::ReadyToPublishAccept {
                    quiescence: QuiescenceProof {
                        close_mode: RequestCloseMode::NormalEos,
                    },
                    permit,
                })
            }
            Disposition::Continue | Disposition::Terminate => {
                self.cancel_writer().await?;
                let permit = self.new_permit(candidate);
                self.observe_gate(
                    DispositionStage::GateReady,
                    candidate,
                    Some(RequestCloseMode::CancelReset),
                    gate_started.elapsed(),
                );
                Ok(WriterGate::ReadyToPublishNonAccept {
                    quiescence: QuiescenceProof {
                        close_mode: RequestCloseMode::CancelReset,
                    },
                    permit,
                })
            }
        }
    }

    pub fn replace_accept_blocked_candidate(
        &mut self,
        disposition: Disposition,
    ) -> Result<(), AttemptError> {
        if !self.accept_blocked || self.candidate != Some(Disposition::Accept) {
            return Err(AttemptError::AcceptWasNotBlocked);
        }
        if disposition == Disposition::Accept {
            return Err(AttemptError::BlockedAcceptMustBecomeNonAccept);
        }
        self.candidate = Some(disposition);
        self.accept_blocked = false;
        if let Some(telemetry) = &self.telemetry {
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
        Ok(())
    }

    pub fn publish_disposition(
        &mut self,
        disposition: Disposition,
        permit: DispositionPublishPermit,
    ) -> Result<PublishedDisposition, AttemptError> {
        self.ensure_active()?;
        if self.published.is_some() {
            return Err(AttemptError::DispositionAlreadyPublished);
        }
        if permit.request_id != self.request_id
            || permit.attempt_id != self.attempt_id
            || permit.generation != self.generation
            || permit.disposition != disposition
            || permit.nonce != self.permit_nonce
            || self.candidate != Some(disposition)
            || permit.downstream_header_fence != self.downstream_header_fence
            || permit.downstream_semantic_fence != self.downstream_semantic_fence
            || permit.accepted_response_scope_created != self.accepted_response_scope_created
        {
            return Err(AttemptError::InvalidDispositionPermit);
        }
        self.ensure_publication_fences_clear()?;
        if disposition == Disposition::Accept
            && (self.writer_state != WriterState::QuiescedNormalEos || !self.response_live)
        {
            self.accept_blocked = true;
            return Err(AttemptError::AcceptLivenessChanged);
        }
        self.permit_nonce = self.permit_nonce.wrapping_add(1);
        self.published = Some(disposition);
        let connection_teardown = self.connection_teardown();
        if let Some(telemetry) = &self.telemetry {
            telemetry.disposition(
                DispositionStage::Published,
                disposition,
                Some(if self.writer_state == WriterState::QuiescedNormalEos {
                    RequestCloseMode::NormalEos
                } else {
                    RequestCloseMode::CancelReset
                }),
                disposition == Disposition::Accept,
                self.reset_count > 0,
                connection_teardown,
                Duration::ZERO,
            );
        }
        Ok(PublishedDisposition {
            request_id: self.request_id,
            attempt_id: self.attempt_id,
            generation: self.generation,
            disposition,
        })
    }

    pub fn begin_accepted_response_scope(&mut self) -> Result<(), AttemptError> {
        self.ensure_disposition_published()?;
        if self.published == Some(Disposition::Continue) {
            return Err(AttemptError::ContinueCannotWriteDownstream);
        }
        if self.accepted_response_scope_created {
            return Err(AttemptError::AcceptedResponseScopeAlreadyCreated);
        }
        self.accepted_response_scope_created = true;
        Ok(())
    }

    pub fn begin_downstream_header_write(&mut self) -> Result<(), AttemptError> {
        self.ensure_downstream_write_allowed()?;
        self.downstream_header_fence.begin_write()?;
        if let Some(telemetry) = &self.telemetry {
            telemetry.commit(
                FenceKind::DownstreamFinalHeaders,
                self.downstream_header_fence,
                self.connection_sub_attempts,
            );
        }
        Ok(())
    }

    pub fn confirm_downstream_header_write(&mut self) -> Result<(), AttemptError> {
        self.ensure_downstream_write_allowed()?;
        self.downstream_header_fence.confirm()?;
        if let Some(telemetry) = &self.telemetry {
            telemetry.commit(
                FenceKind::DownstreamFinalHeaders,
                self.downstream_header_fence,
                self.connection_sub_attempts,
            );
        }
        Ok(())
    }

    pub fn begin_semantic_output_write(&mut self) -> Result<(), AttemptError> {
        self.ensure_downstream_write_allowed()?;
        if self.downstream_header_fence != CommitFence::WriteConfirmed {
            return Err(AttemptError::DownstreamHeaderNotCommitted);
        }
        self.downstream_semantic_fence.begin_write()?;
        if let Some(telemetry) = &self.telemetry {
            telemetry.commit(
                FenceKind::DownstreamSemanticOutput,
                self.downstream_semantic_fence,
                self.connection_sub_attempts,
            );
        }
        Ok(())
    }

    pub fn confirm_semantic_output_write(&mut self) -> Result<(), AttemptError> {
        self.ensure_downstream_write_allowed()?;
        self.downstream_semantic_fence.confirm()?;
        if let Some(telemetry) = &self.telemetry {
            telemetry.commit(
                FenceKind::DownstreamSemanticOutput,
                self.downstream_semantic_fence,
                self.connection_sub_attempts,
            );
        }
        Ok(())
    }

    fn new_permit(&mut self, disposition: Disposition) -> DispositionPublishPermit {
        self.permit_nonce = self.permit_nonce.wrapping_add(1);
        DispositionPublishPermit {
            request_id: self.request_id,
            attempt_id: self.attempt_id,
            generation: self.generation,
            disposition,
            nonce: self.permit_nonce,
            downstream_header_fence: self.downstream_header_fence,
            downstream_semantic_fence: self.downstream_semantic_fence,
            accepted_response_scope_created: self.accepted_response_scope_created,
        }
    }

    fn observe_gate(
        &self,
        stage: DispositionStage,
        disposition: Disposition,
        close_mode: Option<RequestCloseMode>,
        elapsed: Duration,
    ) {
        let connection_teardown = self.connection_teardown();
        if let Some(telemetry) = &self.telemetry {
            telemetry.disposition(
                stage,
                disposition,
                close_mode,
                false,
                self.reset_count > 0,
                connection_teardown,
                elapsed,
            );
        }
    }

    fn ensure_publication_fences_clear(&self) -> Result<(), AttemptError> {
        if self.accepted_response_scope_created
            || !self.downstream_header_fence.is_clear()
            || !self.downstream_semantic_fence.is_clear()
        {
            return Err(AttemptError::DispositionAfterDownstreamCommit);
        }
        Ok(())
    }

    fn accept_progress_blocked_reason(&self, deadline: Instant) -> Option<AcceptBlockedReason> {
        if self.cancellation.is_cancelled() {
            Some(AcceptBlockedReason::Cancelled)
        } else if Instant::now() >= deadline {
            Some(AcceptBlockedReason::Deadline)
        } else if !self.response_live {
            Some(AcceptBlockedReason::ResponseNotLive)
        } else {
            None
        }
    }

    fn ensure_disposition_published(&self) -> Result<(), AttemptError> {
        if self.published.is_none() {
            return Err(AttemptError::DownstreamWriteBeforeDisposition);
        }
        Ok(())
    }

    fn ensure_downstream_write_allowed(&self) -> Result<(), AttemptError> {
        self.ensure_disposition_published()?;
        if !self.accepted_response_scope_created {
            return Err(AttemptError::AcceptedResponseScopeNotCreated);
        }
        Ok(())
    }
}
