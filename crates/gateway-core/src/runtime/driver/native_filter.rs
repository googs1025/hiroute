use super::*;

mod owner;
mod port;

pub struct NativeGatewayFilterManager {
    factories: HashMap<Arc<str>, Arc<dyn NativeFilterFactory>>,
    max_retained_bytes_per_scope: usize,
}

impl NativeGatewayFilterManager {
    pub fn new(max_retained_bytes_per_scope: usize) -> Result<Self, GatewayExecutionError> {
        if max_retained_bytes_per_scope == 0 {
            return Err(GatewayExecutionError::InvalidLimits);
        }
        Ok(Self {
            factories: HashMap::new(),
            max_retained_bytes_per_scope,
        })
    }

    pub fn register(
        &mut self,
        id: impl Into<Arc<str>>,
        factory: Arc<dyn NativeFilterFactory>,
    ) -> Result<(), GatewayExecutionError> {
        let id = id.into();
        if id.trim().is_empty() || self.factories.insert(id, factory).is_some() {
            return Err(GatewayExecutionError::InvalidFilterRegistry);
        }
        Ok(())
    }
}

impl GatewayFilterManagerPort for NativeGatewayFilterManager {
    type RequestFilters = NativeGatewayRequestFilters;

    fn instantiate_request(&self) -> Result<Self::RequestFilters, Arc<str>> {
        Ok(NativeGatewayRequestFilters {
            factories: self.factories.clone(),
            max_retained_bytes_per_scope: self.max_retained_bytes_per_scope,
            logical: None,
            logical_pending: VecDeque::new(),
            logical_pending_reservation: None,
            logical_budget: None,
            attempt_request: None,
            attempt_request_pending: VecDeque::new(),
            attempt_request_pending_reservation: None,
            attempt_response: None,
            attempt_response_pending: VecDeque::new(),
            attempt_response_pending_reservation: None,
            attempt_response_sources: None,
            attempt_budget: None,
            accepted: None,
            accepted_pending: VecDeque::new(),
            accepted_pending_reservation: None,
            accepted_sources: None,
            accepted_budget: None,
            next_body_source: 0,
            finalized: false,
        })
    }
}

struct RuntimeBodyRetention {
    max_bytes: usize,
    live_bytes: usize,
    next_id: u64,
    frames: VecDeque<RuntimeRetainedFrame>,
    budget: StreamBudget,
    _metadata_reservation: Reservation,
    read_paused: bool,
}

enum RuntimeRetainedFrame {
    Opaque {
        id: RetainedFrameId,
        bytes: Bytes,
        reservation: Reservation,
    },
    Charged {
        id: RetainedFrameId,
        bytes: usize,
    },
}

impl RuntimeBodyRetention {
    fn new(
        max_bytes: usize,
        max_frames: usize,
        budget: &StreamBudget,
    ) -> Result<Self, FilterError> {
        let metadata_bytes = max_frames
            .checked_mul(std::mem::size_of::<RuntimeRetainedFrame>())
            .ok_or(FilterError::RetentionLimit)?;
        let metadata_reservation = budget
            .reserve(MemoryRole::SemanticState, metadata_bytes)
            .map_err(|_| FilterError::RetentionLimit)?;
        Ok(Self {
            max_bytes,
            live_bytes: 0,
            next_id: 0,
            frames: VecDeque::with_capacity(max_frames),
            budget: budget.clone(),
            _metadata_reservation: metadata_reservation,
            read_paused: false,
        })
    }
}

struct RetainedBytesOwner {
    bytes: Bytes,
    _reservation: Reservation,
}

impl AsRef<[u8]> for RetainedBytesOwner {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl BodyRetentionPort for RuntimeBodyRetention {
    fn retain(&mut self, bytes: Bytes) -> Result<RetainedFrameId, FilterError> {
        let next = self
            .live_bytes
            .checked_add(bytes.len())
            .ok_or(FilterError::RetentionLimit)?;
        if next > self.max_bytes {
            return Err(FilterError::RetentionLimit);
        }
        self.next_id = self.next_id.wrapping_add(1);
        let id = RetainedFrameId(self.next_id);
        let retained_bytes = bytes
            .len()
            .checked_add(std::mem::size_of::<RetainedBytesOwner>())
            .ok_or(FilterError::RetentionLimit)?;
        let reservation = self
            .budget
            .reserve(MemoryRole::SemanticState, retained_bytes)
            .map_err(|_| FilterError::RetentionLimit)?;
        self.live_bytes = next;
        self.frames.push_back(RuntimeRetainedFrame::Opaque {
            id,
            bytes,
            reservation,
        });
        Ok(id)
    }

    fn take(&mut self, id: RetainedFrameId) -> Result<Bytes, FilterError> {
        let position = self
            .frames
            .iter()
            .position(|frame| match frame {
                RuntimeRetainedFrame::Opaque { id: candidate, .. }
                | RuntimeRetainedFrame::Charged { id: candidate, .. } => *candidate == id,
            })
            .ok_or(FilterError::UnknownRetainedFrame)?;
        let frame = self
            .frames
            .remove(position)
            .ok_or(FilterError::UnknownRetainedFrame)?;
        let RuntimeRetainedFrame::Opaque {
            bytes, reservation, ..
        } = frame
        else {
            return Err(FilterError::UnknownRetainedFrame);
        };
        self.live_bytes -= bytes.len();
        Ok(Bytes::from_owner(RetainedBytesOwner {
            bytes,
            _reservation: reservation,
        }))
    }

    fn retain_charged(&mut self, bytes: usize) -> Result<RetainedFrameId, FilterError> {
        let next = self
            .live_bytes
            .checked_add(bytes)
            .ok_or(FilterError::RetentionLimit)?;
        if next > self.max_bytes || self.frames.len() == self.frames.capacity() {
            return Err(FilterError::RetentionLimit);
        }
        self.next_id = self.next_id.wrapping_add(1);
        let id = RetainedFrameId(self.next_id);
        self.live_bytes = next;
        self.frames
            .push_back(RuntimeRetainedFrame::Charged { id, bytes });
        Ok(id)
    }

    fn release_charged(&mut self, id: RetainedFrameId) -> Result<(), FilterError> {
        let position = self
            .frames
            .iter()
            .position(|frame| match frame {
                RuntimeRetainedFrame::Opaque { id: candidate, .. }
                | RuntimeRetainedFrame::Charged { id: candidate, .. } => *candidate == id,
            })
            .ok_or(FilterError::UnknownRetainedFrame)?;
        let frame = self
            .frames
            .remove(position)
            .ok_or(FilterError::UnknownRetainedFrame)?;
        let RuntimeRetainedFrame::Charged { bytes, .. } = frame else {
            return Err(FilterError::UnknownRetainedFrame);
        };
        self.live_bytes -= bytes;
        Ok(())
    }

    fn set_read_paused(&mut self, paused: bool) {
        self.read_paused = paused;
    }

    fn read_paused(&self) -> bool {
        self.read_paused
    }
}

pub struct NativeGatewayRequestFilters {
    factories: HashMap<Arc<str>, Arc<dyn NativeFilterFactory>>,
    max_retained_bytes_per_scope: usize,
    logical: Option<DirectionMachine>,
    logical_pending: VecDeque<PendingFilterFrame<LogicalRequestBodyFrame>>,
    logical_pending_reservation: Option<Reservation>,
    logical_budget: Option<StreamBudget>,
    attempt_request: Option<DirectionMachine>,
    attempt_request_pending: VecDeque<PendingFilterFrame<AttemptRequestBodyFrame>>,
    attempt_request_pending_reservation: Option<Reservation>,
    attempt_response: Option<DirectionMachine>,
    attempt_response_pending: VecDeque<PendingFilterFrame<PrecommitEvent>>,
    attempt_response_pending_reservation: Option<Reservation>,
    attempt_response_sources: Option<AttemptFilterSourceLedger>,
    attempt_budget: Option<StreamBudget>,
    accepted: Option<DirectionMachine>,
    accepted_pending: VecDeque<PendingFilterFrame<AcceptedBodyFrame>>,
    accepted_pending_reservation: Option<Reservation>,
    accepted_sources: Option<AcceptedFilterSourceLedger>,
    accepted_budget: Option<StreamBudget>,
    next_body_source: u64,
    finalized: bool,
}

#[derive(Debug)]
struct PendingFilterFrame<T> {
    source: Option<FilterBodySourceId>,
    frame: T,
}

#[derive(Debug)]
struct AcceptedFilterSource {
    id: u64,
    live: FilterBodySourceWeak,
    sse_sources: SseTransformSources,
    provenance: SemanticProvenance,
    contains_non_sse_payload: bool,
}

#[derive(Clone, Copy, Debug)]
struct AcceptedSseSourceTally {
    owner_id: u64,
    source: SseTransformSource,
    output_units: usize,
    output_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
struct AcceptedSourceDelta {
    owner_id: u64,
    output_units: usize,
    output_bytes: usize,
}

/// AcceptedResponse-lifetime source registry and cumulative transform ledger.
/// All three backing arrays and every possible source-token allocation are
/// charged before allocation. Entries are reclaimed when the final promoted
/// source owner drops; their fixed-capacity slots remain reusable without
/// growing a long SSE stream's working set.
#[derive(Debug)]
struct AcceptedFilterSourceLedger {
    entries: Vec<AcceptedFilterSource>,
    tallies: Vec<AcceptedSseSourceTally>,
    staged: Vec<AcceptedSourceDelta>,
    max_live_sources: usize,
    max_output_units: usize,
    limits: Option<SseLimits>,
    _metadata_reservation: Reservation,
}

impl AcceptedFilterSourceLedger {
    fn new(
        descriptors: &[CompiledFilterDescriptor],
        budget: &StreamBudget,
        accepted_body_plan: Option<&BodyPlan>,
    ) -> Result<Self, Arc<str>> {
        let max_live_sources = descriptors
            .iter()
            .map(|descriptor| descriptor.max_pending_frames)
            .min()
            .unwrap_or(1)
            .checked_add(1)
            .ok_or_else(|| Arc::from("accepted source limit overflow"))?;
        let max_tallies = max_live_sources
            .checked_mul(max_live_sources)
            .ok_or_else(|| Arc::from("accepted source tally limit overflow"))?;
        let source_token_bytes = max_live_sources
            .checked_mul(FilterBodySourceId::allocation_charge_bytes())
            .ok_or_else(|| Arc::from("accepted source token metadata overflow"))?;
        let entry_bytes = max_live_sources
            .checked_mul(std::mem::size_of::<AcceptedFilterSource>())
            .ok_or_else(|| Arc::from("accepted source metadata overflow"))?;
        let tally_bytes = max_tallies
            .checked_mul(std::mem::size_of::<AcceptedSseSourceTally>())
            .ok_or_else(|| Arc::from("accepted source tally metadata overflow"))?;
        let staged_bytes = max_live_sources
            .checked_mul(std::mem::size_of::<AcceptedSourceDelta>())
            .ok_or_else(|| Arc::from("accepted source staging metadata overflow"))?;
        let metadata_bytes = source_token_bytes
            .checked_add(entry_bytes)
            .and_then(|bytes| bytes.checked_add(tally_bytes))
            .and_then(|bytes| bytes.checked_add(staged_bytes))
            .ok_or_else(|| Arc::from("accepted source ledger metadata overflow"))?;
        let metadata_reservation = budget
            .reserve(MemoryRole::SemanticState, metadata_bytes)
            .map_err(body_filter_error)?;
        let max_output_units = descriptors
            .iter()
            .filter(|descriptor| descriptor.capabilities().expands_body())
            .map(|descriptor| descriptor.max_pending_frames)
            .min()
            .unwrap_or(1);
        let limits = accepted_body_plan.and_then(sse_limits_from_plan);
        Ok(Self {
            entries: Vec::with_capacity(max_live_sources),
            tallies: Vec::with_capacity(max_tallies),
            staged: Vec::with_capacity(max_live_sources),
            max_live_sources,
            max_output_units,
            limits,
            _metadata_reservation: metadata_reservation,
        })
    }

    fn insert(
        &mut self,
        id: u64,
        frame: &AcceptedBodyFrame,
    ) -> Result<FilterBodySourceId, Arc<str>> {
        self.reclaim_dead();
        if self.entries.len() == self.max_live_sources {
            return Err(Arc::from("accepted live source hard limit exceeded"));
        }
        let source_components = frame.sse_sources.as_slice();
        if self
            .tallies
            .len()
            .checked_add(source_components.len())
            .is_none_or(|required| required > self.tallies.capacity())
        {
            return Err(Arc::from("accepted SSE source tally hard limit exceeded"));
        }
        if self.entries.iter().any(|entry| entry.id == id) {
            return Err(Arc::from("accepted source identity was reused"));
        }

        // The ledger's source-token reservation is already live here, before
        // the Arc allocation performed by FilterBodySourceId::new.
        let source = FilterBodySourceId::new(id);
        self.tallies.extend(
            source_components
                .iter()
                .map(|source| AcceptedSseSourceTally {
                    owner_id: id,
                    source: *source,
                    output_units: 0,
                    output_bytes: 0,
                }),
        );
        self.entries.push(AcceptedFilterSource {
            id,
            live: source.downgrade(),
            sse_sources: frame.sse_sources.clone(),
            provenance: frame
                .output
                .as_ref()
                .map_or(SemanticProvenance::NonSemantic, |output| output.provenance),
            contains_non_sse_payload: frame.output.is_some()
                && frame.sse_sources.as_slice().is_empty(),
        });
        Ok(source)
    }

    fn get(&self, id: u64) -> Option<&AcceptedFilterSource> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    fn begin_batch(&mut self) {
        self.staged.clear();
    }

    fn stage_output(
        &mut self,
        sources: &[FilterBodySourceId],
        output_bytes: usize,
    ) -> Result<(), Arc<str>> {
        let Some(limits) = self.limits.as_ref() else {
            return Ok(());
        };
        for source in sources {
            let owner_id = source.value();
            let entry = self
                .get(owner_id)
                .ok_or_else(|| Arc::from("accepted filter emitted an unknown source identity"))?;
            if entry.sse_sources.as_slice().is_empty() {
                continue;
            }
            if output_bytes > limits.max_output_event_bytes {
                return Err(Arc::from(SseError::OutputLimit.to_string()));
            }
            if let Some(delta) = self
                .staged
                .iter_mut()
                .find(|delta| delta.owner_id == owner_id)
            {
                delta.output_units = delta
                    .output_units
                    .checked_add(1)
                    .ok_or_else(|| Arc::from(SseError::OutputLimit.to_string()))?;
                delta.output_bytes = delta
                    .output_bytes
                    .checked_add(output_bytes)
                    .ok_or_else(|| Arc::from(SseError::OutputLimit.to_string()))?;
            } else {
                if self.staged.len() == self.staged.capacity() {
                    return Err(Arc::from("accepted source staging hard limit exceeded"));
                }
                self.staged.push(AcceptedSourceDelta {
                    owner_id,
                    output_units: 1,
                    output_bytes,
                });
            }
        }
        Ok(())
    }

    fn commit_batch(&mut self) -> Result<(), Arc<str>> {
        let Some(limits) = self.limits.as_ref() else {
            self.staged.clear();
            return Ok(());
        };
        for delta in &self.staged {
            for tally in self
                .tallies
                .iter()
                .filter(|tally| tally.owner_id == delta.owner_id)
            {
                let output_units = tally
                    .output_units
                    .checked_add(delta.output_units)
                    .ok_or_else(|| Arc::from(SseError::OutputLimit.to_string()))?;
                let output_bytes = tally
                    .output_bytes
                    .checked_add(delta.output_bytes)
                    .ok_or_else(|| Arc::from(SseError::OutputLimit.to_string()))?;
                validate_sse_source_totals(
                    limits,
                    tally.source,
                    output_units,
                    output_bytes,
                    self.max_output_units,
                )
                .map_err(|error| Arc::from(error.to_string()))?;
            }
        }
        for delta in self.staged.drain(..) {
            for tally in self
                .tallies
                .iter_mut()
                .filter(|tally| tally.owner_id == delta.owner_id)
            {
                tally.output_units += delta.output_units;
                tally.output_bytes += delta.output_bytes;
            }
        }
        Ok(())
    }

    fn reclaim_dead(&mut self) {
        self.entries.retain(|entry| entry.live.is_live());
        self.tallies
            .retain(|tally| self.entries.iter().any(|entry| entry.id == tally.owner_id));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Copy, Debug)]
enum AttemptFilterSource {
    Body,
    Sse {
        sequence: u64,
        provenance: SemanticProvenance,
    },
    EndStream,
}

#[derive(Debug)]
struct AttemptFilterSourceRecord {
    id: u64,
    live: FilterBodySourceWeak,
    metadata: AttemptFilterSource,
}

/// RouteAttempt response metadata outlives the payload owner when a filter
/// promotes, replaces, or drops the current backing. Weak liveness keeps the
/// registry bounded by actual semantic lineage rather than by stream length.
#[derive(Debug)]
struct AttemptFilterSourceLedger {
    entries: Vec<AttemptFilterSourceRecord>,
    max_live_sources: usize,
    _metadata_reservation: Reservation,
}

impl AttemptFilterSourceLedger {
    fn new(
        descriptors: &[CompiledFilterDescriptor],
        budget: &StreamBudget,
    ) -> Result<Self, Arc<str>> {
        let max_live_sources = descriptors
            .iter()
            .map(|descriptor| descriptor.max_pending_frames)
            .min()
            .ok_or_else(|| Arc::<str>::from("empty attempt response filter chain"))?
            .checked_add(1)
            .ok_or_else(|| Arc::<str>::from("attempt source limit overflow"))?;
        let source_token_bytes = max_live_sources
            .checked_mul(FilterBodySourceId::allocation_charge_bytes())
            .ok_or_else(|| Arc::<str>::from("attempt source token metadata overflow"))?;
        let record_bytes = max_live_sources
            .checked_mul(std::mem::size_of::<AttemptFilterSourceRecord>())
            .ok_or_else(|| Arc::<str>::from("attempt source metadata overflow"))?;
        let metadata_reservation = budget
            .reserve(
                MemoryRole::SemanticState,
                source_token_bytes
                    .checked_add(record_bytes)
                    .ok_or_else(|| Arc::<str>::from("attempt source ledger metadata overflow"))?,
            )
            .map_err(body_filter_error)?;
        Ok(Self {
            entries: Vec::with_capacity(max_live_sources),
            max_live_sources,
            _metadata_reservation: metadata_reservation,
        })
    }

    fn insert(&mut self, id: u64, event: &PrecommitEvent) -> Result<FilterBodySourceId, Arc<str>> {
        self.reclaim_dead();
        if self.entries.len() == self.max_live_sources {
            return Err(Arc::from("attempt live source hard limit exceeded"));
        }
        let metadata = match event {
            PrecommitEvent::ResponseHead(_) => {
                return Err(Arc::from("response head cannot own filter body lineage"));
            }
            PrecommitEvent::Body(_) => AttemptFilterSource::Body,
            PrecommitEvent::SseEvent {
                sequence,
                provenance,
                ..
            } => AttemptFilterSource::Sse {
                sequence: *sequence,
                provenance: *provenance,
            },
            PrecommitEvent::EndStream => AttemptFilterSource::EndStream,
        };
        if self.entries.iter().any(|entry| entry.id == id) {
            return Err(Arc::from("attempt source identity was reused"));
        }
        let source = FilterBodySourceId::new(id);
        self.entries.push(AttemptFilterSourceRecord {
            id,
            live: source.downgrade(),
            metadata,
        });
        Ok(source)
    }

    fn get(&self, id: u64) -> Option<AttemptFilterSource> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.metadata)
    }

    fn reclaim_dead(&mut self) {
        self.entries.retain(|entry| entry.live.is_live());
    }
}

impl Drop for NativeGatewayRequestFilters {
    fn drop(&mut self) {
        self.finalize();
    }
}

fn filter_error(error: FilterError) -> Arc<str> {
    Arc::from(error.to_string())
}

#[cfg(test)]
mod tests;
