//! Provider-owner fakes used to verify the #9 integration boundary.
//!
//! These fakes deliberately exchange only compiled envelopes, opaque model
//! values, technical attempt identities, and published dispositions. They do
//! not implement a configuration backend, selection policy, or provider wire
//! protocol.

use std::collections::VecDeque;

use bytes::Bytes;
use http::StatusCode;
use thiserror::Error;

use crate::core::execution_plan::ResolvedTargetBindingId;
use crate::core::publication::CompiledGatewayPublicationEnvelope;
use crate::runtime::attempt::{
    AttemptGeneration, AttemptId, Disposition, PublishedDisposition, RequestId,
};

/// The notification mechanism is provenance only. Every variant delivers the
/// same already-compiled envelope contract to #9.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceDeliveryKind {
    InMemory,
    Poll,
    MessageHint { sequence: u64 },
    WatchHint { sequence: u64 },
    PeriodicReconciliation,
}

#[derive(Clone, Debug)]
pub struct CompiledSourceDelivery {
    pub kind: SourceDeliveryKind,
    pub envelope: CompiledGatewayPublicationEnvelope,
}

#[derive(Debug, Default)]
pub struct CompiledSourceFake {
    pending: VecDeque<CompiledSourceDelivery>,
}

impl CompiledSourceFake {
    pub fn enqueue(
        &mut self,
        kind: SourceDeliveryKind,
        envelope: CompiledGatewayPublicationEnvelope,
    ) {
        self.pending
            .push_back(CompiledSourceDelivery { kind, envelope });
    }

    pub fn next_compiled(&mut self) -> Option<CompiledSourceDelivery> {
        self.pending.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// A technical selection result. The fake chooses outside #9; an
/// `AttemptExchange` receives exactly one of these bindings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectedAttempt {
    pub request_id: RequestId,
    pub attempt_id: AttemptId,
    pub generation: AttemptGeneration,
    pub binding: ResolvedTargetBindingId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishedDispositionFact {
    pub request_id: RequestId,
    pub attempt_id: AttemptId,
    pub generation: AttemptGeneration,
    pub disposition: Disposition,
}

#[derive(Debug, Default)]
pub struct SelectorPublisherFake {
    candidates: VecDeque<ResolvedTargetBindingId>,
    next_attempt_id: u64,
    selected: Vec<SelectedAttempt>,
    published: Vec<PublishedDispositionFact>,
}

impl SelectorPublisherFake {
    pub fn new(candidates: impl IntoIterator<Item = ResolvedTargetBindingId>) -> Self {
        Self {
            candidates: candidates.into_iter().collect(),
            next_attempt_id: 1,
            selected: Vec::new(),
            published: Vec::new(),
        }
    }

    /// Each call is an explicit #13 decision and creates one new attempt
    /// identity; #9 cannot pull a fallback candidate from inside an exchange.
    pub fn select_next(
        &mut self,
        request_id: RequestId,
        generation: AttemptGeneration,
    ) -> Option<SelectedAttempt> {
        let binding = self.candidates.pop_front()?;
        let selected = SelectedAttempt {
            request_id,
            attempt_id: AttemptId(self.next_attempt_id),
            generation,
            binding,
        };
        self.next_attempt_id = self.next_attempt_id.wrapping_add(1);
        self.selected.push(selected);
        Some(selected)
    }

    pub fn observe_published(
        &mut self,
        disposition: &PublishedDisposition,
    ) -> Result<(), ContractError> {
        let selected = self
            .selected
            .iter()
            .find(|selected| {
                selected.request_id == disposition.request_id
                    && selected.attempt_id == disposition.attempt_id
                    && selected.generation == disposition.generation
            })
            .ok_or(ContractError::UnknownAttempt)?;
        if self.published.iter().any(|fact| {
            fact.request_id == selected.request_id
                && fact.attempt_id == selected.attempt_id
                && fact.generation == selected.generation
        }) {
            return Err(ContractError::DuplicatePublication);
        }
        self.published.push(PublishedDispositionFact {
            request_id: disposition.request_id,
            attempt_id: disposition.attempt_id,
            generation: disposition.generation,
            disposition: disposition.disposition,
        });
        Ok(())
    }

    pub fn selected(&self) -> &[SelectedAttempt] {
        &self.selected
    }

    pub fn published(&self) -> &[PublishedDispositionFact] {
        &self.published
    }
}

/// Opaque #15-owned model IR used only to prove consuming boundaries.
#[derive(Debug)]
pub struct FakeModelRequestIr {
    request_bytes: Option<Bytes>,
}

#[derive(Debug)]
pub struct FakePreparedProviderRequest {
    pub wire_bytes: Bytes,
}

#[derive(Debug)]
pub struct FakeDecodedProviderEvent {
    raw_event: Option<Bytes>,
}

#[derive(Debug)]
pub struct FakeReadiness {
    disposition: Disposition,
    decoded: FakeDecodedProviderEvent,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProviderContractCounts {
    pub parsed: usize,
    pub materialized: usize,
    pub classified: usize,
    pub readiness: usize,
    pub encoded: usize,
}

/// A consuming parse/materialize/classify/readiness/post-disposition encoder
/// fake. Byte interpretation remains entirely inside this #15 stand-in.
#[derive(Debug, Default)]
pub struct ConsumingProviderFake {
    counts: ProviderContractCounts,
}

impl ConsumingProviderFake {
    pub fn parse_consuming(&mut self, request_bytes: Bytes) -> FakeModelRequestIr {
        self.counts.parsed += 1;
        FakeModelRequestIr {
            request_bytes: Some(request_bytes),
        }
    }

    pub fn materialize_consuming(
        &mut self,
        ir: &mut FakeModelRequestIr,
    ) -> Result<FakePreparedProviderRequest, ContractError> {
        let wire_bytes = ir
            .request_bytes
            .take()
            .ok_or(ContractError::AlreadyConsumed)?;
        self.counts.materialized += 1;
        Ok(FakePreparedProviderRequest { wire_bytes })
    }

    pub fn classify_consuming(
        &mut self,
        raw_event: Bytes,
        status: StatusCode,
    ) -> (FakeDecodedProviderEvent, Disposition) {
        self.counts.classified += 1;
        let disposition = if status == StatusCode::TOO_MANY_REQUESTS {
            Disposition::Continue
        } else {
            Disposition::Accept
        };
        (
            FakeDecodedProviderEvent {
                raw_event: Some(raw_event),
            },
            disposition,
        )
    }

    pub fn establish_readiness(
        &mut self,
        decoded: FakeDecodedProviderEvent,
        disposition: Disposition,
    ) -> FakeReadiness {
        self.counts.readiness += 1;
        FakeReadiness {
            disposition,
            decoded,
        }
    }

    pub fn encode_after_publication(
        &mut self,
        readiness: FakeReadiness,
        published: &PublishedDisposition,
    ) -> Result<Bytes, ContractError> {
        if readiness.disposition != published.disposition {
            return Err(ContractError::DispositionMismatch);
        }
        let raw = readiness
            .decoded
            .raw_event
            .ok_or(ContractError::AlreadyConsumed)?;
        self.counts.encoded += 1;
        Ok(raw)
    }

    pub fn counts(&self) -> ProviderContractCounts {
        self.counts
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ContractError {
    #[error("contract value was already consumed")]
    AlreadyConsumed,
    #[error("published disposition does not match readiness")]
    DispositionMismatch,
    #[error("publication references an unknown selected attempt")]
    UnknownAttempt,
    #[error("the selected attempt was already published")]
    DuplicatePublication,
}
