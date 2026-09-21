use super::*;

pub(super) fn sse_limits_from_plan(plan: &BodyPlan) -> Option<SseLimits> {
    let BodyPlan::SseFramedStreaming {
        max_event_bytes,
        max_pending_bytes,
        max_output_event_bytes,
        expansion_ratio_numerator,
        expansion_ratio_denominator,
        expansion_slack_bytes,
    } = plan
    else {
        return None;
    };
    Some(SseLimits {
        max_event_bytes: *max_event_bytes,
        max_pending_bytes: *max_pending_bytes,
        max_output_event_bytes: *max_output_event_bytes,
        expansion_ratio_numerator: *expansion_ratio_numerator,
        expansion_ratio_denominator: *expansion_ratio_denominator,
        expansion_slack_bytes: *expansion_slack_bytes,
        retained_capacity_threshold: *max_pending_bytes,
        eof_policy: EofPolicy::Strict,
    })
}

pub(super) fn sse_event_provenance(event: &SseEventView<'_>) -> SemanticProvenance {
    if event
        .fields()
        .any(|field| !field.comment && field.name == b"data")
    {
        SemanticProvenance::ProducesSemantic
    } else {
        SemanticProvenance::NonSemantic
    }
}

struct DiscardSseOutput;

impl BoundedOutputSink for DiscardSseOutput {
    fn emit_borrowed(&mut self, _bytes: &[u8]) -> Result<(), SseError> {
        Ok(())
    }

    fn emit_owned(&mut self, _bytes: ChargedBytes) -> Result<(), SseError> {
        Ok(())
    }
}

struct PrecommitSseVisitor<'a, D> {
    budget: &'a StreamBudget,
    handoff: &'a mut PrecommitResponseState<D>,
    mailbox: &'a mut BudgetedResponseMailbox<u64>,
}

impl<D> SseVisitor for PrecommitSseVisitor<'_, D> {
    fn on_event(
        &mut self,
        event: SseEventView<'_>,
        emitter: &mut BoundedEventEmitter<'_, '_>,
    ) -> Result<(), SseError> {
        if self.mailbox.is_full() {
            return Err(SseError::NeedDrain);
        }
        let provenance = sse_event_provenance(&event);
        let sequence = self
            .handoff
            .push_raw_with_provenance(event.promote(self.budget)?, provenance)?;
        self.mailbox
            .try_push(sequence)
            .map_err(|_| SseError::NeedDrain)?;
        emitter.drop_event()
    }
}

struct AcceptedSseVisitor<'a> {
    budget: &'a StreamBudget,
    next_sequence: &'a mut u64,
    mailbox: &'a mut BudgetedResponseMailbox<PrecommitEvent>,
}

impl SseVisitor for AcceptedSseVisitor<'_> {
    fn on_event(
        &mut self,
        event: SseEventView<'_>,
        emitter: &mut BoundedEventEmitter<'_, '_>,
    ) -> Result<(), SseError> {
        if self.mailbox.is_full() {
            return Err(SseError::NeedDrain);
        }
        let provenance = sse_event_provenance(&event);
        let bytes =
            ChargedBytes::copy_from_opaque(self.budget, MemoryRole::ResponsePrefix, event.raw())
                .map_err(|_| SseError::BudgetExceeded)?;
        let sequence = *self.next_sequence;
        *self.next_sequence = self.next_sequence.wrapping_add(1);
        self.mailbox
            .try_push(PrecommitEvent::SseEvent {
                sequence,
                bytes,
                provenance,
            })
            .map_err(|_| SseError::NeedDrain)?;
        emitter.drop_event()
    }
}

pub(super) fn feed_precommit_sse<D>(
    framer: &mut SseFramer,
    input: ChargedBytes,
    end_stream: bool,
    budget: &StreamBudget,
    handoff: &mut PrecommitResponseState<D>,
    mailbox: &mut BudgetedResponseMailbox<u64>,
) -> Result<SseFeedOutcome, SseError> {
    let mut visitor = PrecommitSseVisitor {
        budget,
        handoff,
        mailbox,
    };
    framer.feed(input, end_stream, &mut visitor, &mut DiscardSseOutput)
}

pub(super) fn feed_accepted_sse(
    framer: &mut SseFramer,
    input: ChargedBytes,
    end_stream: bool,
    budget: &StreamBudget,
    next_sequence: &mut u64,
    mailbox: &mut BudgetedResponseMailbox<PrecommitEvent>,
) -> Result<SseFeedOutcome, SseError> {
    let mut visitor = AcceptedSseVisitor {
        budget,
        next_sequence,
        mailbox,
    };
    framer.feed(input, end_stream, &mut visitor, &mut DiscardSseOutput)
}

pub(super) fn resume_precommit_sse<D>(
    framer: &mut SseFramer,
    budget: &StreamBudget,
    handoff: &mut PrecommitResponseState<D>,
    mailbox: &mut BudgetedResponseMailbox<u64>,
) -> Result<SseFeedOutcome, SseError> {
    let mut visitor = PrecommitSseVisitor {
        budget,
        handoff,
        mailbox,
    };
    framer.resume(&mut visitor, &mut DiscardSseOutput)
}

pub(super) fn resume_accepted_sse(
    framer: &mut SseFramer,
    budget: &StreamBudget,
    next_sequence: &mut u64,
    mailbox: &mut BudgetedResponseMailbox<PrecommitEvent>,
) -> Result<SseFeedOutcome, SseError> {
    let mut visitor = AcceptedSseVisitor {
        budget,
        next_sequence,
        mailbox,
    };
    framer.resume(&mut visitor, &mut DiscardSseOutput)
}

pub(super) fn validate_sse_transform_batch(
    limits: &SseLimits,
    frames: &BodyEmitterOutcome<AcceptedBodyFrame>,
    max_output_units: usize,
    semantic_replacement_authorized: bool,
) -> Result<(), SseError> {
    let mut totals: HashMap<u64, (SseTransformSource, usize, usize)> = HashMap::new();
    for frame in frames.iter() {
        let expected_provenance = frame
            .sse_sources
            .as_slice()
            .iter()
            .fold(SemanticProvenance::NonSemantic, |combined, source| {
                combined.merge(source.provenance)
            });
        if let Some(output) = frame.output.as_ref()
            && !frame.sse_sources.as_slice().is_empty()
        {
            if output.provenance != expected_provenance && !semantic_replacement_authorized {
                return Err(SseError::SemanticReplacementNotAuthorized);
            }
            if output.bytes.bytes().len() > limits.max_output_event_bytes {
                return Err(SseError::OutputLimit);
            }
        }
        for source in frame.sse_sources.as_slice() {
            let entry = totals.entry(source.sequence).or_insert((*source, 0, 0));
            if entry.0 != *source {
                return Err(SseError::TransformSequenceMismatch);
            }
            if let Some(output) = frame.output.as_ref() {
                let output_bytes = output.bytes.bytes().len();
                entry.1 = entry.1.saturating_add(1);
                entry.2 = entry.2.saturating_add(output_bytes);
            }
        }
    }
    for (_, (source, count, bytes)) in totals {
        validate_sse_source_totals(limits, source, count, bytes, max_output_units)?;
    }
    Ok(())
}

pub(super) fn validate_sse_source_totals(
    limits: &SseLimits,
    source: SseTransformSource,
    output_units: usize,
    output_bytes: usize,
    max_output_units: usize,
) -> Result<(), SseError> {
    if output_units > max_output_units {
        return Err(SseError::OutputLimit);
    }
    let ratio_limit = source
        .source_bytes
        .saturating_mul(limits.expansion_ratio_numerator)
        / limits.expansion_ratio_denominator;
    let expanded_limit = ratio_limit.saturating_add(limits.expansion_slack_bytes);
    if output_bytes > expanded_limit {
        return Err(SseError::OutputLimit);
    }
    Ok(())
}
