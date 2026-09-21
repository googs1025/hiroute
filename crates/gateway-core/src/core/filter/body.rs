use super::*;

#[derive(Debug)]
struct FilterBodySourceToken {
    value: u64,
}

/// Drop-tracked identity for a callback input. A promoted input or emitted
/// output keeps this token alive; the runtime keeps only a weak reference to
/// the corresponding provenance record, so ordinary dropped frames do not
/// accumulate in a long-lived stream ledger.
#[derive(Clone, Debug)]
pub(crate) struct FilterBodySourceId(Arc<FilterBodySourceToken>);

impl FilterBodySourceId {
    pub(crate) fn new(value: u64) -> Self {
        Self(Arc::new(FilterBodySourceToken { value }))
    }

    /// Conservative charge for the Arc header and token backing allocated by
    /// `new`. AcceptedResponse reserves this amount for its complete live
    /// source capacity before any token allocation occurs.
    pub(crate) const fn allocation_charge_bytes() -> usize {
        std::mem::size_of::<FilterBodySourceToken>() + 2 * std::mem::size_of::<usize>()
    }

    pub(crate) fn value(&self) -> u64 {
        self.0.value
    }

    pub(crate) fn downgrade(&self) -> FilterBodySourceWeak {
        FilterBodySourceWeak(Arc::downgrade(&self.0))
    }
}

impl PartialEq for FilterBodySourceId {
    fn eq(&self, other: &Self) -> bool {
        self.value() == other.value()
    }
}

impl Eq for FilterBodySourceId {}

impl std::hash::Hash for FilterBodySourceId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value().hash(state);
    }
}

/// Linear identity of the runtime frame whose charged backing is currently
/// being presented to the filter chain. This is deliberately distinct from
/// [`FilterBodySourceSet`]: promotion and merge may widen semantic lineage,
/// but they never widen ownership of the one current runtime backing.
#[derive(Debug)]
pub(crate) struct FilterBodyRuntimeOwner {
    source: FilterBodySourceId,
}

impl FilterBodyRuntimeOwner {
    pub(super) fn new(source: FilterBodySourceId) -> Self {
        Self { source }
    }

    pub(crate) fn source(&self) -> &FilterBodySourceId {
        &self.source
    }
}

#[derive(Debug)]
pub(super) struct FilterBodyOwnership {
    pub(super) runtime_owner: Option<FilterBodyRuntimeOwner>,
    pub(super) sources: FilterBodySourceSet,
}

impl FilterBodyOwnership {
    pub(super) fn new(
        runtime_owner: Option<FilterBodyRuntimeOwner>,
        sources: FilterBodySourceSet,
    ) -> Self {
        Self {
            runtime_owner,
            sources,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FilterBodySourceWeak(std::sync::Weak<FilterBodySourceToken>);

impl FilterBodySourceWeak {
    pub(crate) fn is_live(&self) -> bool {
        self.0.strong_count() != 0
    }
}

#[derive(Debug)]
pub(crate) struct ChargedFilterBodySources {
    ids: Box<[FilterBodySourceId]>,
    _reservation: Reservation,
}

/// Exact callback-input lineage carried through every filter emission. The
/// common one-source case is allocation free; an explicit merge reserves its
/// complete source-set metadata before allocating it.
#[derive(Clone, Debug, Default)]
pub(crate) enum FilterBodySourceSet {
    #[default]
    Anonymous,
    One(FilterBodySourceId),
    Many(Arc<ChargedFilterBodySources>),
}

impl FilterBodySourceSet {
    pub(crate) fn one(source: FilterBodySourceId) -> Self {
        Self::One(source)
    }

    pub(crate) fn as_slice(&self) -> &[FilterBodySourceId] {
        match self {
            Self::Anonymous => &[],
            Self::One(source) => std::slice::from_ref(source),
            Self::Many(sources) => &sources.ids,
        }
    }

    fn merge_for_output(
        budget: &StreamBudget,
        inherited: &Self,
        promoted: &[&PromotedBody],
    ) -> Result<Self, FilterError> {
        let max_sources = promoted
            .iter()
            .try_fold(inherited.as_slice().len(), |total, input| {
                total.checked_add(input.sources.as_slice().len())
            })
            .ok_or(FilterError::BodyOutputBudget)?;
        if max_sources == 0 {
            return Ok(Self::Anonymous);
        }
        let metadata_bytes = max_sources
            .checked_mul(std::mem::size_of::<FilterBodySourceId>())
            .ok_or(FilterError::BodyOutputBudget)?;
        let reservation = budget
            .reserve(MemoryRole::SemanticState, metadata_bytes)
            .map_err(|_| FilterError::BodyOutputBudget)?;
        let mut ids = Vec::with_capacity(max_sources);
        for source in inherited
            .as_slice()
            .iter()
            .chain(promoted.iter().flat_map(|input| input.sources.as_slice()))
        {
            if !ids.contains(source) {
                ids.push(source.clone());
            }
        }
        match ids.as_slice() {
            [] => Ok(Self::Anonymous),
            [source] => Ok(Self::One(source.clone())),
            _ => Ok(Self::Many(Arc::new(ChargedFilterBodySources {
                ids: ids.into_boxed_slice(),
                _reservation: reservation,
            }))),
        }
    }
}

impl PartialEq for FilterBodySourceSet {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for FilterBodySourceSet {}

#[derive(Debug)]
pub enum FilterBodyEmission {
    Forward,
    Drop,
    Replace(FilterBodyOutput),
}

/// A retained callback input whose reservation follows every `Bytes` clone
/// embedded in this owner. Only borrowed access is exposed.
#[derive(Clone, Debug)]
pub struct PromotedBody {
    pub(super) bytes: Bytes,
    pub(super) sources: FilterBodySourceSet,
}

impl PromotedBody {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl AsRef<[u8]> for PromotedBody {
    fn as_ref(&self) -> &[u8] {
        self.bytes()
    }
}

/// Linear, already-charged replacement units. The metadata reservation stays
/// alive through iteration; each byte unit embeds its own drop-tracked charge.
#[derive(Debug)]
pub struct FilterBodyOutput {
    units: Vec<FilterBodyOutputData>,
    metadata_reservation: Option<Arc<Reservation>>,
}

#[derive(Debug)]
pub(super) struct FilterBodyOutputData {
    pub(super) bytes: ChargedBytes,
    pub(super) sources: FilterBodySourceSet,
}

impl FilterBodyOutput {
    pub fn empty() -> Self {
        Self {
            units: Vec::new(),
            metadata_reservation: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    pub fn len(&self) -> usize {
        self.units.len()
    }
}

pub struct FilterBodyOutputIntoIter {
    units: std::vec::IntoIter<FilterBodyOutputData>,
    metadata_reservation: Option<Arc<Reservation>>,
}

#[derive(Debug)]
pub struct FilterBodyOutputUnit {
    pub(crate) bytes: ChargedBytes,
    pub(crate) sources: FilterBodySourceSet,
    pub(crate) queue_metadata: BodyMetadataOwner,
}

impl Iterator for FilterBodyOutputIntoIter {
    type Item = FilterBodyOutputUnit;

    fn next(&mut self) -> Option<Self::Item> {
        self.units.next().map(|unit| FilterBodyOutputUnit {
            bytes: unit.bytes,
            sources: unit.sources,
            queue_metadata: BodyMetadataOwner::from_reservation(self.metadata_reservation.clone()),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.units.size_hint()
    }
}

impl ExactSizeIterator for FilterBodyOutputIntoIter {}

impl IntoIterator for FilterBodyOutput {
    type Item = FilterBodyOutputUnit;
    type IntoIter = FilterBodyOutputIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        FilterBodyOutputIntoIter {
            units: self.units.into_iter(),
            metadata_reservation: self.metadata_reservation,
        }
    }
}

/// Reserve-before-allocate builder for native body replacement output.
pub struct FilterBodyEmitter {
    pub(super) budget: StreamBudget,
    pub(super) role: MemoryRole,
    pub(super) max_units: usize,
    pub(super) units: Vec<FilterBodyOutputData>,
    pub(super) metadata_reservation: Option<Arc<Reservation>>,
    pub(super) inherited_sources: FilterBodySourceSet,
}

impl FilterBodyEmitter {
    pub fn emit_copy(&mut self, bytes: &[u8]) -> Result<(), FilterError> {
        if self.units.len() >= self.max_units {
            return Err(FilterError::BodyOutputUnitLimit);
        }
        let charged = ChargedBytes::copy_from_opaque(&self.budget, self.role, bytes)
            .map_err(|_| FilterError::BodyOutputBudget)?;
        self.units.push(FilterBodyOutputData {
            bytes: charged,
            sources: self.inherited_sources.clone(),
        });
        Ok(())
    }

    /// Emits one unit that explicitly derives from the current callback and
    /// previously promoted inputs. This is the only merge path that carries
    /// every source identity into the downstream transform ledger.
    pub fn emit_copy_from_promoted(
        &mut self,
        promoted: &[&PromotedBody],
        bytes: &[u8],
    ) -> Result<(), FilterError> {
        if self.units.len() >= self.max_units {
            return Err(FilterError::BodyOutputUnitLimit);
        }
        let sources =
            FilterBodySourceSet::merge_for_output(&self.budget, &self.inherited_sources, promoted)?;
        let charged = ChargedBytes::copy_from_opaque(&self.budget, self.role, bytes)
            .map_err(|_| FilterError::BodyOutputBudget)?;
        self.units.push(FilterBodyOutputData {
            bytes: charged,
            sources,
        });
        Ok(())
    }

    pub fn finish(self) -> FilterBodyOutput {
        FilterBodyOutput {
            units: self.units,
            metadata_reservation: self.metadata_reservation,
        }
    }
}

#[derive(Debug)]
pub(crate) enum EmittedBodyBacking {
    /// Borrowed callback input. The runtime's pending source frame remains the
    /// unique charged owner and is moved by source identity at resolution.
    Forwarded(Bytes),
    /// Replacement allocation with its reservation intact from emitter to
    /// the next framework owner.
    Replacement(ChargedBytes),
    /// Typed terminal control. This carries no payload backing and therefore
    /// cannot be mistaken by the runtime resolver for a forwarded empty view
    /// of the original source owner.
    EndStreamControl,
}

impl EmittedBodyBacking {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Forwarded(bytes) => bytes.as_ref(),
            Self::Replacement(bytes) => bytes.bytes().as_ref(),
            Self::EndStreamControl => &[],
        }
    }
}

#[derive(Debug)]
pub(crate) struct EmittedBodyFrame {
    pub(crate) backing: EmittedBodyBacking,
    pub(crate) end_stream: bool,
    pub(crate) runtime_owner: Option<FilterBodyRuntimeOwner>,
    pub(crate) sources: FilterBodySourceSet,
    pub(crate) queue_metadata: BodyMetadataOwner,
}

impl EmittedBodyFrame {
    pub(crate) fn bytes(&self) -> &[u8] {
        self.backing.bytes()
    }
}
