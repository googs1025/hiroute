use super::*;

impl DirectionMachine {
    pub async fn resume(
        &mut self,
        token: ContinuationToken,
        action: ResumeAction,
    ) -> Result<MachineOutcome, FilterError> {
        self.validate_token(&token)?;
        match action {
            ResumeAction::LocalReply(reply) => return Ok(self.set_terminal(*reply)),
            ResumeAction::Fail => {
                self.enter_failing_terminal();
                return Err(FilterError::ResumeFailed);
            }
            ResumeAction::Continue(patch) => {
                patch.apply(&mut self.held_headers, self.framing.as_mut())?;
            }
        }

        let pause = self.pauses.pop().expect("validated pause");
        self.pause_receivers.pop();
        self.observe_scope(
            match pause.frame_kind {
                FrameKind::Headers => ScopePhase::Headers,
                FrameKind::Data | FrameKind::Trailers | FrameKind::EndStream => ScopePhase::Body,
            },
            pause.paused_at.elapsed(),
        );
        self.pause_epoch = self.pause_epoch.wrapping_add(1);
        if pause.header_mode == Some(HeaderStopMode::AllWatermark)
            || pause.retention == Some(RetentionMode::Watermark)
        {
            self.retention.set_read_paused(false);
        }

        let mut completed = false;
        if pause.frame_kind == FrameKind::Headers {
            match self.run_headers_from(self.next_header).await? {
                MachineOutcome::Paused(token) => return Ok(MachineOutcome::Paused(token)),
                MachineOutcome::LocalReply(reply) => return Ok(MachineOutcome::LocalReply(reply)),
                MachineOutcome::Complete => completed = true,
                MachineOutcome::Advanced => {}
            }
        }
        // If the header-stopped filter subsequently pauses its own terminal
        // data or trailers callback, the retained frame resumes beyond that
        // filter. Advance headers first, then deliver the same frame from its
        // exact cursor. A newly stopped header replaces the old stop cursor;
        // StopAll keeps the frame queued until its explicit continuation.
        if matches!(pause.frame_kind, FrameKind::Data | FrameKind::Trailers)
            && let Some(old_stop_after) = self.pending_crossing_header_stop()
            && let Some(suspended) = self.pauses.last().copied()
            && suspended.frame_kind == FrameKind::Headers
            && suspended.header_mode == Some(HeaderStopMode::Iteration)
            && old_stop_after == suspended.filter_index
        {
            self.pauses.pop();
            self.pause_receivers.pop();
            self.observe_scope(ScopePhase::Headers, suspended.paused_at.elapsed());
            self.pause_epoch = self.pause_epoch.wrapping_add(1);
            match self.run_headers_from(self.next_header).await? {
                MachineOutcome::Paused(token) => {
                    let next_pause = self
                        .pauses
                        .last()
                        .copied()
                        .ok_or(FilterError::StaleContinuation)?;
                    if next_pause.frame_kind == FrameKind::Headers
                        && next_pause.header_mode == Some(HeaderStopMode::Iteration)
                    {
                        self.rewrite_pending_header_stop(
                            old_stop_after,
                            Some(next_pause.filter_index),
                        );
                    } else {
                        self.rewrite_pending_header_stop(old_stop_after, None);
                        return Ok(MachineOutcome::Paused(token));
                    }
                }
                MachineOutcome::LocalReply(reply) => {
                    return Ok(MachineOutcome::LocalReply(reply));
                }
                MachineOutcome::Complete => {
                    self.rewrite_pending_header_stop(old_stop_after, None);
                    completed = true;
                }
                MachineOutcome::Advanced => {
                    self.rewrite_pending_header_stop(old_stop_after, None);
                }
            }
        }
        let outcome = self.drain_pending().await?;
        if matches!(
            outcome,
            MachineOutcome::Paused(_) | MachineOutcome::LocalReply(_)
        ) {
            return Ok(outcome);
        }
        if let Some(token) = self.reissue_suspended_pause() {
            return Ok(MachineOutcome::Paused(token));
        }
        Ok(
            if completed && matches!(outcome, MachineOutcome::Advanced) {
                MachineOutcome::Complete
            } else {
                outcome
            },
        )
    }

    pub(super) async fn drain_pending(&mut self) -> Result<MachineOutcome, FilterError> {
        let mut final_outcome = MachineOutcome::Advanced;
        while let Some(frame) = self.pending.pop_front() {
            let outcome = match frame {
                PendingFrame::Data {
                    backing,
                    end_stream,
                    cursor,
                    stop_after,
                    runtime_owner,
                    sources,
                    queue_metadata,
                } => {
                    let backing = match backing {
                        PendingBodyBacking::Retained(retained) => {
                            EmittedBodyBacking::Forwarded(self.retention.take(retained)?)
                        }
                        PendingBodyBacking::Charged { retention, bytes } => {
                            self.retention.release_charged(retention)?;
                            EmittedBodyBacking::Replacement(bytes)
                        }
                        PendingBodyBacking::EndStreamControl => {
                            EmittedBodyBacking::EndStreamControl
                        }
                    };
                    self.deliver_data_range(
                        backing,
                        end_stream,
                        cursor,
                        stop_after,
                        FilterBodyOwnership::new(runtime_owner, sources),
                        queue_metadata,
                    )
                    .await?
                }
                PendingFrame::DroppedDataControl { .. } => MachineOutcome::Advanced,
                PendingFrame::DroppedEndStreamControl {
                    cursor,
                    stop_after,
                    sources,
                } => {
                    self.deliver_data_range(
                        EmittedBodyBacking::EndStreamControl,
                        true,
                        cursor,
                        stop_after,
                        FilterBodyOwnership::new(None, sources),
                        BodyMetadataOwner::default(),
                    )
                    .await?
                }
                PendingFrame::Trailers {
                    trailers,
                    cursor,
                    stop_after,
                } => {
                    self.deliver_trailers_from(trailers, cursor, stop_after)
                        .await?
                }
            };
            if !matches!(outcome, MachineOutcome::Advanced | MachineOutcome::Complete) {
                return Ok(outcome);
            }
            if matches!(outcome, MachineOutcome::Complete) {
                final_outcome = MachineOutcome::Complete;
            }
        }
        Ok(final_outcome)
    }

    pub(super) fn queue_data(
        &mut self,
        backing: EmittedBodyBacking,
        end_stream: bool,
        cursor: usize,
        stop_after: Option<usize>,
        ownership: FilterBodyOwnership,
        queue_metadata: BodyMetadataOwner,
    ) -> Result<(), FilterError> {
        if self.pending.len() >= self.max_pending_frames {
            return Err(FilterError::PendingFrameLimit);
        }
        let backing = match backing {
            EmittedBodyBacking::Forwarded(bytes) => {
                PendingBodyBacking::Retained(self.retention.retain(bytes)?)
            }
            EmittedBodyBacking::Replacement(bytes) => PendingBodyBacking::Charged {
                retention: self.retention.retain_charged(bytes.bytes().len())?,
                bytes,
            },
            EmittedBodyBacking::EndStreamControl => PendingBodyBacking::EndStreamControl,
        };
        self.pending.push_back(PendingFrame::Data {
            backing,
            end_stream,
            cursor,
            stop_after,
            runtime_owner: ownership.runtime_owner,
            sources: ownership.sources,
            queue_metadata,
        });
        Ok(())
    }

    pub(super) fn queue_dropped_data_control(
        &mut self,
        end_stream: bool,
        cursor: usize,
        stop_after: Option<usize>,
        runtime_owner: Option<FilterBodyRuntimeOwner>,
        sources: FilterBodySourceSet,
    ) -> Result<(), FilterError> {
        if self.pending.len() >= self.max_pending_frames
            || (runtime_owner.is_some()
                && self.dropped_runtime_owners.len() >= self.max_pending_frames)
        {
            return Err(FilterError::PendingFrameLimit);
        }
        if let Some(runtime_owner) = runtime_owner {
            self.dropped_runtime_owners.push_back(runtime_owner);
        }
        if end_stream {
            self.pending
                .push_back(PendingFrame::DroppedEndStreamControl {
                    cursor,
                    stop_after,
                    sources,
                });
        } else {
            self.pending
                .push_back(PendingFrame::DroppedDataControl { cursor, stop_after });
        }
        Ok(())
    }

    pub(super) fn record_dropped_runtime_owner(
        &mut self,
        runtime_owner: Option<FilterBodyRuntimeOwner>,
    ) -> Result<(), FilterError> {
        let Some(runtime_owner) = runtime_owner else {
            return Ok(());
        };
        if self.dropped_runtime_owners.len() >= self.max_pending_frames {
            return Err(FilterError::PendingFrameLimit);
        }
        self.dropped_runtime_owners.push_back(runtime_owner);
        Ok(())
    }

    pub(super) fn queue_trailers(
        &mut self,
        trailers: HeaderMap,
        cursor: usize,
        stop_after: Option<usize>,
    ) -> Result<(), FilterError> {
        if self.pending.len() >= self.max_pending_frames {
            return Err(FilterError::PendingFrameLimit);
        }
        self.pending.push_back(PendingFrame::Trailers {
            trailers,
            cursor,
            stop_after,
        });
        Ok(())
    }

    pub(super) fn pending_crossing_header_stop(&self) -> Option<usize> {
        match self.pending.front() {
            Some(PendingFrame::Data {
                cursor,
                stop_after: Some(stop_after),
                ..
            })
            | Some(PendingFrame::DroppedDataControl {
                cursor,
                stop_after: Some(stop_after),
            })
            | Some(PendingFrame::DroppedEndStreamControl {
                cursor,
                stop_after: Some(stop_after),
                ..
            })
            | Some(PendingFrame::Trailers {
                cursor,
                stop_after: Some(stop_after),
                ..
            }) if cursor > stop_after => Some(*stop_after),
            _ => None,
        }
    }

    pub(super) fn rewrite_pending_header_stop(&mut self, from: usize, to: Option<usize>) {
        for frame in &mut self.pending {
            let stop_after = match frame {
                PendingFrame::Data { stop_after, .. }
                | PendingFrame::DroppedDataControl { stop_after, .. }
                | PendingFrame::DroppedEndStreamControl { stop_after, .. }
                | PendingFrame::Trailers { stop_after, .. } => stop_after,
            };
            if *stop_after == Some(from) {
                *stop_after = to;
            }
        }
    }

    pub(super) fn pause_at(
        &mut self,
        filter_index: usize,
        frame_kind: FrameKind,
        header_mode: Option<HeaderStopMode>,
        retention: Option<RetentionMode>,
        receiver: oneshot::Receiver<ResumeAction>,
    ) -> Result<MachineOutcome, FilterError> {
        self.pause_epoch = self.pause_epoch.wrapping_add(1);
        let pause = PauseState {
            filter_index,
            frame_kind,
            epoch: self.pause_epoch,
            header_mode,
            retention,
            paused_at: Instant::now(),
        };
        self.pauses.push(pause);
        self.pause_receivers.push(Some(receiver));
        self.observe_scope(ScopePhase::Paused, Duration::ZERO);
        Ok(MachineOutcome::Paused(self.token_for(pause)))
    }

    pub(super) fn token_for(&self, pause: PauseState) -> ContinuationToken {
        ContinuationToken {
            stream_id: self.stream_id,
            scope_id: self.scope_id,
            direction: self.direction,
            filter_index: pause.filter_index,
            frame_kind: pause.frame_kind,
            epoch: pause.epoch,
        }
    }

    pub(super) fn validate_token(&self, token: &ContinuationToken) -> Result<(), FilterError> {
        if token.stream_id != self.stream_id {
            return Err(FilterError::CrossStreamContinuation);
        }
        if token.scope_id != self.scope_id {
            return Err(FilterError::CrossScopeContinuation);
        }
        if token.direction != self.direction {
            return Err(FilterError::WrongDirectionContinuation);
        }
        if self.finalized || self.terminal {
            return Err(FilterError::ResumeAfterFinalize);
        }
        let Some(pause) = self.pauses.last().copied() else {
            return Err(FilterError::StaleContinuation);
        };
        if pause.filter_index != token.filter_index
            || pause.frame_kind != token.frame_kind
            || pause.epoch != token.epoch
        {
            return Err(FilterError::StaleContinuation);
        }
        Ok(())
    }

    pub(super) fn suspended_header_stop_after(&self) -> Option<usize> {
        self.pauses.iter().rev().find_map(|pause| {
            (pause.frame_kind == FrameKind::Headers
                && pause.header_mode == Some(HeaderStopMode::Iteration))
            .then_some(pause.filter_index)
        })
    }

    pub(super) fn reissue_suspended_pause(&mut self) -> Option<ContinuationToken> {
        let pause = self.pauses.last_mut()?;
        self.pause_epoch = self.pause_epoch.wrapping_add(1);
        pause.epoch = self.pause_epoch;
        pause.paused_at = Instant::now();
        let pause = *pause;
        self.observe_scope(ScopePhase::Paused, Duration::ZERO);
        Some(self.token_for(pause))
    }
}
