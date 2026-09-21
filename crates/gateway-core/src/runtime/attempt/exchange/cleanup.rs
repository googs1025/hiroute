use super::*;

impl<T: AttemptTransport> AttemptExchange<T> {
    pub async fn finish_or_abort(&mut self) -> Result<(), AttemptError> {
        if self.finalized {
            return Ok(());
        }
        // This is the destructive terminal path, including cancellation or a
        // response-side failure after normal request EOS. Only the accepted
        // response completion path may preserve/release the upstream session.
        if self.writer_state != WriterState::QuiescedCancelReset {
            self.cancel_writer().await?;
        }
        self.response_window.release_storage();
        self.response_mailbox_reservation.take();
        self.transport_codec_reservation.take();
        self.finalized = true;
        Ok(())
    }

    /// Destructive convergence is itself structured work. A broken transport
    /// cannot keep the request owner alive forever: on timeout the async reset
    /// future is dropped, all core-owned backing is released, and the
    /// transport is left to its synchronous Drop close path.
    pub async fn finish_or_abort_bounded(
        &mut self,
        join_timeout: Duration,
    ) -> Result<(), AttemptError> {
        if join_timeout.is_zero() {
            return Err(AttemptError::InvalidCleanupTimeout);
        }
        let started = Instant::now();
        match tokio::time::timeout(join_timeout, self.finish_or_abort()).await {
            Ok(result) => {
                if let Some(telemetry) = &self.telemetry {
                    telemetry.cleanup(
                        CleanupKind::WriterJoin,
                        started.elapsed(),
                        false,
                        self.connection_teardown(),
                    );
                }
                result
            }
            Err(_) => {
                self.cancellation.cancel();
                // The reset future was dropped before acknowledging teardown.
                // Drop the concrete protocol owner first so codec/socket Bytes
                // clones cannot outlive the accounting released below.
                self.transport.take();
                self.request.body.release();
                self.response_window.release_storage();
                self.response_mailbox_reservation.take();
                self.transport_codec_reservation.take();
                self.response_live = false;
                self.finalized = true;
                if let Some(telemetry) = &self.telemetry {
                    telemetry.cleanup(CleanupKind::WriterJoin, started.elapsed(), true, true);
                }
                Err(AttemptError::CleanupTimeout)
            }
        }
    }

    pub async fn finish_accepted_response(&mut self, reusable: bool) -> Result<(), AttemptError> {
        self.ensure_active()?;
        if self.published != Some(Disposition::Accept)
            || self.writer_state != WriterState::QuiescedNormalEos
        {
            return Err(AttemptError::AcceptedResponseNotReady);
        }
        let started = Instant::now();
        self.transport
            .as_mut()
            .expect("active exchange owns transport")
            .finish_accepted(reusable)
            .await?;
        if let Some(telemetry) = &self.telemetry {
            telemetry.release(ReleasePoint::TransportSendOrReset, Duration::ZERO);
            telemetry.cleanup(
                CleanupKind::AcceptedRelease,
                started.elapsed(),
                false,
                false,
            );
        }
        self.response_window.release_storage();
        self.response_mailbox_reservation.take();
        self.transport_codec_reservation.take();
        self.finalized = true;
        Ok(())
    }

    pub fn snapshot(&self) -> AttemptSnapshot {
        AttemptSnapshot {
            semantic_upstream_calls: self.semantic_upstream_calls,
            connection_sub_attempts: self.connection_sub_attempts,
            writer_state: self.writer_state,
            upstream_request_fence: self.upstream_request_fence,
            downstream_header_fence: self.downstream_header_fence,
            downstream_semantic_fence: self.downstream_semantic_fence,
            accepted_response_scope_created: self.accepted_response_scope_created,
            published: self.published,
            reset_count: self.reset_count,
            finalized: self.finalized,
        }
    }

    pub(super) async fn cancel_writer(&mut self) -> Result<(), AttemptError> {
        if self.writer_state == WriterState::QuiescedCancelReset {
            return Ok(());
        }
        self.writer_state = WriterState::Cancelling;
        let started = Instant::now();
        let connection_teardown = self.connection_teardown_for_reset();
        self.transport
            .as_mut()
            .expect("active exchange owns transport")
            .cancel_reset()
            .await?;
        self.reset_count += 1;
        self.request.body.release();
        self.pending_request_write.take();
        self.response_window.release_storage();
        self.response_mailbox_reservation.take();
        self.transport_codec_reservation.take();
        self.response_live = false;
        self.writer_state = WriterState::QuiescedCancelReset;
        if let Some(telemetry) = &self.telemetry {
            telemetry.release(ReleasePoint::WireSourceConsumedOrCancelled, Duration::ZERO);
            telemetry.release(ReleasePoint::TransportSendOrReset, Duration::ZERO);
            telemetry.release(ReleasePoint::RequestWriterQuiesced, Duration::ZERO);
            telemetry.cleanup(
                CleanupKind::TransportReset,
                started.elapsed(),
                false,
                connection_teardown,
            );
        }
        Ok(())
    }

    pub(super) fn ensure_active(&self) -> Result<(), AttemptError> {
        if self.finalized {
            Err(AttemptError::ExchangeFinalized)
        } else {
            Ok(())
        }
    }

    fn connection_teardown_for_reset(&self) -> bool {
        self.transport
            .as_ref()
            .is_some_and(|transport| transport.protocol() == HttpProtocol::Http1)
    }

    pub(super) fn connection_teardown(&self) -> bool {
        self.reset_count > 0 && self.connection_teardown_for_reset()
    }
}
