use std::collections::{BTreeSet, VecDeque};
use std::io::Write;
use std::sync::Arc;

use hiroute_domain::{
    ConversationContentEnvelopeV1, ExecutionFactEnvelopeV1, LossNoticeV1, ObservationChannel,
    ObservationStreamV1, SequenceRangeV1, SequencedObservationEnvelope,
};
use parking_lot::Mutex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfferOutcome {
    Accepted,
    DroppedCapacity,
    DroppedOversized,
    RejectedWrongStream,
    RejectedNonMonotonic,
}

#[derive(Clone, Debug)]
pub(crate) struct QueuedEnvelope<T> {
    pub envelope: T,
    pub channel_losses: Vec<LossNoticeV1>,
}

#[derive(Debug)]
struct QueueEntry<T> {
    envelope: T,
    encoded_bytes: usize,
    channel_losses: Vec<LossNoticeV1>,
}

#[derive(Debug)]
struct QueueState<T> {
    entries: VecDeque<QueueEntry<T>>,
    queued_sequences: BTreeSet<u64>,
    used_bytes: usize,
    pending_losses: Vec<LossNoticeV1>,
}

#[derive(Debug)]
struct LossAwareChannel<T> {
    stream: ObservationStreamV1,
    channel: ObservationChannel,
    max_bytes: usize,
    state: Mutex<QueueState<T>>,
}

impl<T> LossAwareChannel<T>
where
    T: SequencedObservationEnvelope,
{
    fn new(stream: ObservationStreamV1, channel: ObservationChannel, max_bytes: usize) -> Self {
        Self {
            stream,
            channel,
            max_bytes: max_bytes.max(1),
            state: Mutex::new(QueueState {
                entries: VecDeque::new(),
                queued_sequences: BTreeSet::new(),
                used_bytes: 0,
                pending_losses: Vec::new(),
            }),
        }
    }

    fn offer(&self, envelope: T) -> OfferOutcome {
        if envelope.channel() != self.channel || envelope.stream() != &self.stream {
            return OfferOutcome::RejectedWrongStream;
        }
        let sequence = envelope.sequence();
        let mut state = self.state.lock();
        if state.queued_sequences.contains(&sequence) {
            return OfferOutcome::RejectedNonMonotonic;
        }
        let encoded_bytes = encoded_size(&envelope)
            .and_then(|bytes| bytes.checked_add(64))
            .unwrap_or(usize::MAX);
        if encoded_bytes > self.max_bytes {
            record_drop(&mut state.pending_losses, &envelope);
            return OfferOutcome::DroppedOversized;
        }
        if state.used_bytes.saturating_add(encoded_bytes) > self.max_bytes {
            record_drop(&mut state.pending_losses, &envelope);
            return OfferOutcome::DroppedCapacity;
        }
        let channel_losses = take_losses_before(&mut state.pending_losses, sequence);
        state.queued_sequences.insert(sequence);
        state.entries.push_back(QueueEntry {
            envelope,
            encoded_bytes,
            channel_losses,
        });
        state.used_bytes += encoded_bytes;
        OfferOutcome::Accepted
    }

    fn pop(&self) -> Option<QueuedEnvelope<T>> {
        let mut state = self.state.lock();
        let entry = state.entries.pop_front()?;
        state.queued_sequences.remove(&entry.envelope.sequence());
        state.used_bytes = state.used_bytes.saturating_sub(entry.encoded_bytes);
        Some(QueuedEnvelope {
            envelope: entry.envelope,
            channel_losses: entry.channel_losses,
        })
    }

    fn take_losses(&self) -> Vec<LossNoticeV1> {
        std::mem::take(&mut self.state.lock().pending_losses)
    }

    fn restore_losses(&self, losses: Vec<LossNoticeV1>) {
        let mut state = self.state.lock();
        for loss in losses {
            merge_loss(&mut state.pending_losses, loss);
        }
    }

    pub(crate) fn stream(&self) -> &ObservationStreamV1 {
        &self.stream
    }
}

fn encoded_size(value: &impl serde::Serialize) -> Option<usize> {
    #[derive(Default)]
    struct Counter(usize);

    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("serialized observation size overflow"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.0)
}

fn record_drop<T: SequencedObservationEnvelope>(losses: &mut Vec<LossNoticeV1>, envelope: &T) {
    let sequence = envelope.sequence();
    let scope = envelope.scope();
    merge_loss(
        losses,
        LossNoticeV1 {
            range: SequenceRangeV1 {
                first: sequence,
                last: sequence,
            },
            scope,
        },
    );
}

fn merge_loss(losses: &mut Vec<LossNoticeV1>, loss: LossNoticeV1) {
    losses.push(loss);
    losses.sort_by(|left, right| {
        left.scope
            .workspace_id
            .cmp(&right.scope.workspace_id)
            .then_with(|| left.scope.session_id.cmp(&right.scope.session_id))
            .then_with(|| left.range.first.cmp(&right.range.first))
    });
    let mut merged: Vec<LossNoticeV1> = Vec::with_capacity(losses.len());
    for loss in losses.drain(..) {
        if let Some(last) = merged.last_mut()
            && last.scope == loss.scope
            && loss.range.first <= last.range.last.saturating_add(1)
        {
            last.range.last = last.range.last.max(loss.range.last);
        } else {
            merged.push(loss);
        }
    }
    *losses = merged;
}

fn take_losses_before(losses: &mut Vec<LossNoticeV1>, sequence: u64) -> Vec<LossNoticeV1> {
    let mut ready = Vec::new();
    let mut future = Vec::new();
    for loss in losses.drain(..) {
        if loss.range.last < sequence {
            ready.push(loss);
        } else {
            future.push(loss);
        }
    }
    *losses = future;
    ready
}

#[derive(Clone, Debug)]
pub struct FactChannel(Arc<LossAwareChannel<ExecutionFactEnvelopeV1>>);

impl FactChannel {
    pub fn new(stream: ObservationStreamV1, max_bytes: usize) -> Self {
        Self(Arc::new(LossAwareChannel::new(
            stream,
            ObservationChannel::Fact,
            max_bytes,
        )))
    }

    pub fn offer(&self, envelope: ExecutionFactEnvelopeV1) -> OfferOutcome {
        self.0.offer(envelope)
    }

    pub(crate) fn pop(&self) -> Option<QueuedEnvelope<ExecutionFactEnvelopeV1>> {
        self.0.pop()
    }

    pub(crate) fn take_losses(&self) -> Vec<LossNoticeV1> {
        self.0.take_losses()
    }

    pub(crate) fn restore_losses(&self, losses: Vec<LossNoticeV1>) {
        self.0.restore_losses(losses);
    }

    pub(crate) fn stream(&self) -> &ObservationStreamV1 {
        self.0.stream()
    }
}

#[derive(Clone, Debug)]
pub struct ConversationContentChannel(Arc<LossAwareChannel<ConversationContentEnvelopeV1>>);

impl ConversationContentChannel {
    pub fn new(stream: ObservationStreamV1, max_bytes: usize) -> Self {
        Self(Arc::new(LossAwareChannel::new(
            stream,
            ObservationChannel::Content,
            max_bytes,
        )))
    }

    pub fn offer(&self, envelope: ConversationContentEnvelopeV1) -> OfferOutcome {
        self.0.offer(envelope)
    }

    pub(crate) fn pop(&self) -> Option<QueuedEnvelope<ConversationContentEnvelopeV1>> {
        self.0.pop()
    }

    pub(crate) fn take_losses(&self) -> Vec<LossNoticeV1> {
        self.0.take_losses()
    }

    pub(crate) fn restore_losses(&self, losses: Vec<LossNoticeV1>) {
        self.0.restore_losses(losses);
    }

    pub(crate) fn stream(&self) -> &ObservationStreamV1 {
        self.0.stream()
    }
}
