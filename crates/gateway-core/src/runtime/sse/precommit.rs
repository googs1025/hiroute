use super::*;

#[derive(Debug)]
pub struct OwnedSseEvent {
    pub(super) raw: ChargedBytes,
}

impl OwnedSseEvent {
    pub fn raw(&self) -> &Bytes {
        self.raw.bytes()
    }

    pub fn into_raw(self) -> ChargedBytes {
        self.raw
    }
}

#[derive(Debug)]
enum PrecommitRecord<D> {
    Raw {
        event: OwnedSseEvent,
        provenance: SemanticProvenance,
    },
    InFlight {
        source_bytes: usize,
        provenance: SemanticProvenance,
    },
    Decoded {
        decoded: D,
        source_bytes: usize,
        provenance: SemanticProvenance,
    },
}

#[derive(Debug)]
pub struct PrecommitResponseState<D> {
    next_sequence: u64,
    records: VecDeque<(u64, PrecommitRecord<D>)>,
    capacity: usize,
    metadata_reservation: Option<Reservation>,
}

impl<D> Default for PrecommitResponseState<D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D> PrecommitResponseState<D> {
    pub fn new() -> Self {
        Self {
            next_sequence: 0,
            records: VecDeque::new(),
            capacity: usize::MAX,
            metadata_reservation: None,
        }
    }

    pub fn new_budgeted(capacity: usize, budget: &StreamBudget) -> Result<Self, SseError> {
        if capacity == 0 {
            return Err(SseError::InvalidMailboxCapacity);
        }
        let metadata_bytes = capacity
            .checked_mul(size_of::<(u64, PrecommitRecord<D>)>())
            .ok_or(SseError::BudgetExceeded)?;
        let metadata_reservation = budget
            .reserve(MemoryRole::ResponsePrefix, metadata_bytes)
            .map_err(|_| SseError::BudgetExceeded)?;
        Ok(Self {
            next_sequence: 0,
            records: VecDeque::with_capacity(capacity),
            capacity,
            metadata_reservation: Some(metadata_reservation),
        })
    }

    pub fn push_raw(&mut self, event: OwnedSseEvent) -> Result<u64, SseError> {
        self.push_raw_with_provenance(event, SemanticProvenance::ProducesSemantic)
    }

    pub fn push_raw_with_provenance(
        &mut self,
        event: OwnedSseEvent,
        provenance: SemanticProvenance,
    ) -> Result<u64, SseError> {
        if self.records.len() == self.capacity {
            return Err(SseError::HandoffCapacityExceeded);
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.records
            .push_back((sequence, PrecommitRecord::Raw { event, provenance }));
        Ok(sequence)
    }

    /// Transfers the sole raw owner to the provider classifier. The record is
    /// deliberately left in an in-flight state so Accept cannot publish while
    /// a provider callback still owns an unclassified prefix event.
    pub fn take_raw_for_classification(
        &mut self,
        sequence: u64,
    ) -> Result<(OwnedSseEvent, SemanticProvenance), SseError> {
        let (_, record) = self
            .records
            .iter_mut()
            .find(|(candidate, _)| *candidate == sequence)
            .ok_or(SseError::UnknownSequence(sequence))?;
        let source_bytes = match record {
            PrecommitRecord::Raw { event, .. } => event.raw().len(),
            PrecommitRecord::InFlight { .. } | PrecommitRecord::Decoded { .. } => {
                return Err(SseError::SequenceAlreadyDecoded(sequence));
            }
        };
        let previous = std::mem::replace(
            record,
            PrecommitRecord::InFlight {
                source_bytes,
                provenance: SemanticProvenance::NonSemantic,
            },
        );
        let PrecommitRecord::Raw { event, provenance } = previous else {
            unreachable!("raw state checked before linear ownership transfer")
        };
        *record = PrecommitRecord::InFlight {
            source_bytes,
            provenance,
        };
        Ok((event, provenance))
    }

    /// Atomically replaces the classifier's in-flight raw owner with its
    /// bounded decoded representation. Dropping the callback input therefore
    /// releases the raw charge before this method returns.
    pub fn mark_decoded(&mut self, sequence: u64, decoded: D) -> Result<(), SseError> {
        let (_, record) = self
            .records
            .iter_mut()
            .find(|(candidate, _)| *candidate == sequence)
            .ok_or(SseError::UnknownSequence(sequence))?;
        let (source_bytes, provenance) = match record {
            PrecommitRecord::InFlight {
                source_bytes,
                provenance,
            } => (*source_bytes, *provenance),
            PrecommitRecord::Raw { .. } => {
                return Err(SseError::SequenceNotInFlight(sequence));
            }
            PrecommitRecord::Decoded { .. } => {
                return Err(SseError::SequenceAlreadyDecoded(sequence));
            }
        };
        *record = PrecommitRecord::Decoded {
            decoded,
            source_bytes,
            provenance,
        };
        Ok(())
    }

    /// Removes a filter-consumed event whose body emitter produced no
    /// replacement. Only the in-flight owner may perform this transition.
    pub fn drop_inflight(&mut self, sequence: u64) -> Result<(), SseError> {
        let position = self
            .records
            .iter()
            .position(|(candidate, _)| *candidate == sequence)
            .ok_or(SseError::UnknownSequence(sequence))?;
        if !matches!(self.records[position].1, PrecommitRecord::InFlight { .. }) {
            return Err(SseError::SequenceNotInFlight(sequence));
        }
        self.records.remove(position);
        Ok(())
    }

    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub fn publish_accept(self) -> Result<AcceptedHandoff<D>, SseError> {
        if let Some((sequence, _)) = self
            .records
            .iter()
            .find(|(_, record)| matches!(record, PrecommitRecord::InFlight { .. }))
        {
            return Err(SseError::SequenceStillInFlight(*sequence));
        }
        Ok(AcceptedHandoff {
            records: self.records.into_iter(),
            _metadata_reservation: self.metadata_reservation,
        })
    }

    pub fn publish_non_accept(mut self) {
        self.records.clear();
        self.records = VecDeque::new();
        self.metadata_reservation.take();
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

pub struct AcceptedHandoff<D> {
    records: std::collections::vec_deque::IntoIter<(u64, PrecommitRecord<D>)>,
    _metadata_reservation: Option<Reservation>,
}

impl<D> Iterator for AcceptedHandoff<D> {
    type Item = AcceptedEvent<D>;

    fn next(&mut self) -> Option<Self::Item> {
        self.records.next().map(|(sequence, record)| match record {
            PrecommitRecord::Raw { event, provenance } => AcceptedEvent::NeedsDecode {
                sequence,
                raw: event,
                provenance,
            },
            PrecommitRecord::InFlight { .. } => {
                unreachable!("publish_accept rejects in-flight prefix events")
            }
            PrecommitRecord::Decoded {
                decoded,
                source_bytes,
                provenance,
            } => AcceptedEvent::Decoded {
                sequence,
                decoded,
                source_bytes,
                provenance,
            },
        })
    }
}

#[derive(Debug)]
pub enum AcceptedEvent<D> {
    NeedsDecode {
        sequence: u64,
        raw: OwnedSseEvent,
        provenance: SemanticProvenance,
    },
    Decoded {
        sequence: u64,
        decoded: D,
        source_bytes: usize,
        provenance: SemanticProvenance,
    },
}
