use super::*;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum ConfigBindingPolicy {
    ConnectionPinned,
    RequestPinned,
    AttemptPinned,
    PhasePinned,
    EventLive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfigCellDescriptor {
    pub id: ConfigCellId,
    pub compatibility_hash: [u8; 32],
    pub atomicity_group: AtomicityGroupId,
    pub binding_policy: ConfigBindingPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImmutableConfig {
    pub generation: ConfigGeneration,
    pub compatibility_hash: [u8; 32],
    pub bytes: Arc<[u8]>,
}

/// One atomically replaceable configuration bundle. Related values are stored
/// together so a reader cannot observe a half-updated atomicity group.
#[derive(Clone, Debug)]
pub struct ConfigBundle {
    pub group: AtomicityGroupId,
    values: Arc<HashMap<ConfigCellId, ImmutableConfig>>,
    created_at: std::time::Instant,
}

impl ConfigBundle {
    pub fn new(group: AtomicityGroupId, values: HashMap<ConfigCellId, ImmutableConfig>) -> Self {
        Self {
            group,
            values: Arc::new(values),
            created_at: std::time::Instant::now(),
        }
    }

    pub fn get(&self, id: ConfigCellId) -> Option<&ImmutableConfig> {
        self.values.get(&id)
    }
}

impl PartialEq for ConfigBundle {
    fn eq(&self, other: &Self) -> bool {
        self.group == other.group && self.values == other.values
    }
}

impl Eq for ConfigBundle {}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ConfigBundleRuntimeFact {
    pub identity: usize,
    pub bytes: usize,
    pub generation: ConfigGeneration,
    pub external_leases: usize,
    pub age: Duration,
}

#[derive(Clone, Debug)]
pub struct ConfigCellHandle {
    descriptor: ConfigCellDescriptor,
    bundle: Arc<ArcSwap<ConfigBundle>>,
    group_descriptors: Arc<HashMap<ConfigCellId, ConfigCellDescriptor>>,
    publish_lock: Arc<Mutex<()>>,
}

impl ConfigCellHandle {
    pub fn new(
        descriptor: ConfigCellDescriptor,
        initial_bundle: Arc<ConfigBundle>,
    ) -> Result<Self, PlanError> {
        let id = descriptor.id;
        ConfigCellGroup::new([descriptor], initial_bundle)?
            .handle(id)
            .ok_or(PlanError::MissingConfigCell(id))
    }

    pub fn descriptor(&self) -> &ConfigCellDescriptor {
        &self.descriptor
    }

    pub(crate) fn shares_atomic_bundle_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.bundle, &other.bundle)
    }

    pub(crate) fn active_bundle_runtime_fact(&self) -> ConfigBundleRuntimeFact {
        let bundle = self.bundle.load_full();
        let generation = bundle
            .get(self.descriptor.id)
            .expect("validated ConfigBundle membership")
            .generation;
        ConfigBundleRuntimeFact {
            identity: Arc::as_ptr(&bundle) as usize,
            bytes: bundle.values.values().map(|value| value.bytes.len()).sum(),
            generation,
            // ArcSwap owns one strong reference and this inspection owns one.
            // Every additional reference is a real pinned scope lease.
            external_leases: Arc::strong_count(&bundle).saturating_sub(2),
            age: bundle.created_at.elapsed(),
        }
    }

    pub fn publish(&self, bundle: Arc<ConfigBundle>) -> Result<(), PlanError> {
        publish_config_bundle(
            &self.bundle,
            &self.group_descriptors,
            &self.publish_lock,
            bundle,
        )
    }

    pub fn acquire_connection(&self) -> Result<ConfigLease, PlanError> {
        self.acquire_pinned_for(ConfigBindingPolicy::ConnectionPinned)
    }

    pub fn acquire_request(&self) -> Result<ConfigLease, PlanError> {
        self.acquire_pinned_for(ConfigBindingPolicy::RequestPinned)
    }

    pub fn acquire_attempt(&self) -> Result<ConfigLease, PlanError> {
        self.acquire_pinned_for(ConfigBindingPolicy::AttemptPinned)
    }

    pub fn acquire_phase(&self) -> Result<ConfigLease, PlanError> {
        self.acquire_pinned_for(ConfigBindingPolicy::PhasePinned)
    }

    /// The returned guard is intentionally `!Send`; EventLive callers must
    /// copy a bounded owned value before crossing an await or thread boundary.
    pub fn acquire_event(&self) -> Result<ConfigCellGuard, PlanError> {
        self.ensure_policy(ConfigBindingPolicy::EventLive)?;
        Ok(ConfigCellGuard {
            descriptor: self.descriptor.clone(),
            bundle: self.bundle.load_full(),
            _not_send: PhantomData,
        })
    }

    fn acquire_pinned_for(&self, expected: ConfigBindingPolicy) -> Result<ConfigLease, PlanError> {
        self.ensure_policy(expected)?;
        Ok(ConfigLease {
            descriptor: self.descriptor.clone(),
            bundle: self.bundle.load_full(),
        })
    }

    fn ensure_policy(&self, expected: ConfigBindingPolicy) -> Result<(), PlanError> {
        if self.descriptor.binding_policy != expected {
            return Err(PlanError::ConfigBindingPolicyMismatch {
                id: self.descriptor.id,
                expected,
                actual: self.descriptor.binding_policy,
            });
        }
        Ok(())
    }
}

/// Owns the one atomic pointer shared by all cells in an atomicity group.
/// Publishing through either the group or any derived handle validates the
/// complete descriptor set before a single pointer swap.
#[derive(Clone, Debug)]
pub struct ConfigCellGroup {
    descriptors: Arc<HashMap<ConfigCellId, ConfigCellDescriptor>>,
    bundle: Arc<ArcSwap<ConfigBundle>>,
    publish_lock: Arc<Mutex<()>>,
}

impl ConfigCellGroup {
    pub fn new(
        descriptors: impl IntoIterator<Item = ConfigCellDescriptor>,
        initial_bundle: Arc<ConfigBundle>,
    ) -> Result<Self, PlanError> {
        let mut by_id = HashMap::new();
        for descriptor in descriptors {
            let id = descriptor.id;
            if by_id.insert(id, descriptor).is_some() {
                return Err(PlanError::DuplicateConfigCell(id));
            }
        }
        if by_id.is_empty() {
            return Err(PlanError::EmptyConfigAtomicityGroup);
        }
        validate_bundle_values(&by_id, &initial_bundle)?;
        Ok(Self {
            descriptors: Arc::new(by_id),
            bundle: Arc::new(ArcSwap::from(initial_bundle)),
            publish_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn handle(&self, id: ConfigCellId) -> Option<ConfigCellHandle> {
        self.descriptors
            .get(&id)
            .cloned()
            .map(|descriptor| ConfigCellHandle {
                descriptor,
                bundle: Arc::clone(&self.bundle),
                group_descriptors: Arc::clone(&self.descriptors),
                publish_lock: Arc::clone(&self.publish_lock),
            })
    }

    pub fn handles(&self) -> HashMap<ConfigCellId, ConfigCellHandle> {
        self.descriptors
            .keys()
            .copied()
            .filter_map(|id| self.handle(id).map(|handle| (id, handle)))
            .collect()
    }

    pub fn publish(&self, bundle: Arc<ConfigBundle>) -> Result<(), PlanError> {
        publish_config_bundle(&self.bundle, &self.descriptors, &self.publish_lock, bundle)
    }

    pub fn acquire_connection_snapshot(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(
            &self.handles().into_values().collect::<Vec<_>>(),
            ConfigBindingPolicy::ConnectionPinned,
        )
    }

    pub fn acquire_request_snapshot(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(
            &self.handles().into_values().collect::<Vec<_>>(),
            ConfigBindingPolicy::RequestPinned,
        )
    }

    pub fn acquire_attempt_snapshot(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(
            &self.handles().into_values().collect::<Vec<_>>(),
            ConfigBindingPolicy::AttemptPinned,
        )
    }

    pub fn acquire_phase_snapshot(&self) -> Result<ConfigScopeSnapshot, PlanError> {
        ConfigScopeSnapshot::acquire(
            &self.handles().into_values().collect::<Vec<_>>(),
            ConfigBindingPolicy::PhasePinned,
        )
    }

    pub fn acquire_event_snapshot(&self) -> Result<ConfigEventSnapshot, PlanError> {
        Ok(ConfigEventSnapshot {
            snapshot: ConfigScopeSnapshot::acquire(
                &self.handles().into_values().collect::<Vec<_>>(),
                ConfigBindingPolicy::EventLive,
            )?,
            _not_send: PhantomData,
        })
    }
}

fn publish_config_bundle(
    active: &ArcSwap<ConfigBundle>,
    descriptors: &HashMap<ConfigCellId, ConfigCellDescriptor>,
    publish_lock: &Mutex<()>,
    candidate: Arc<ConfigBundle>,
) -> Result<(), PlanError> {
    let candidate_generation = validate_bundle_values(descriptors, &candidate)?;
    let _guard = publish_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current = active.load_full();
    let current_generation = validate_bundle_values(descriptors, &current)?;
    if candidate_generation < current_generation {
        return Err(PlanError::StaleConfigGeneration {
            active: current_generation,
            candidate: candidate_generation,
        });
    }
    if candidate_generation == current_generation {
        return if *candidate == *current {
            Ok(())
        } else {
            Err(PlanError::ConfigGenerationConflict(candidate_generation))
        };
    }
    active.store(candidate);
    Ok(())
}

fn validate_bundle_values(
    descriptors: &HashMap<ConfigCellId, ConfigCellDescriptor>,
    bundle: &ConfigBundle,
) -> Result<ConfigGeneration, PlanError> {
    if let Some(unknown) = bundle
        .values
        .keys()
        .find(|id| !descriptors.contains_key(id))
    {
        return Err(PlanError::UnknownConfigCell(*unknown));
    }
    let mut generation = None;
    for descriptor in descriptors.values() {
        if bundle.group != descriptor.atomicity_group {
            return Err(PlanError::AtomicityGroupMismatch);
        }
        let value = bundle
            .get(descriptor.id)
            .ok_or(PlanError::MissingConfigCell(descriptor.id))?;
        if value.compatibility_hash != descriptor.compatibility_hash {
            return Err(PlanError::CompatibilityMismatch(descriptor.id));
        }
        match generation {
            Some(expected) if expected != value.generation => {
                return Err(PlanError::MixedConfigGeneration);
            }
            None => generation = Some(value.generation),
            _ => {}
        }
    }
    generation.ok_or(PlanError::EmptyConfigAtomicityGroup)
}

#[derive(Debug)]
pub struct ConfigCellGuard {
    descriptor: ConfigCellDescriptor,
    bundle: Arc<ConfigBundle>,
    _not_send: PhantomData<Rc<()>>,
}

impl ConfigCellGuard {
    pub fn value(&self) -> &ImmutableConfig {
        // Construction and publication validate membership.
        self.bundle
            .get(self.descriptor.id)
            .expect("validated ConfigBundle membership")
    }
}

#[derive(Debug)]
pub struct ConfigLease {
    descriptor: ConfigCellDescriptor,
    bundle: Arc<ConfigBundle>,
}

impl ConfigLease {
    pub fn value(&self) -> &ImmutableConfig {
        self.bundle
            .get(self.descriptor.id)
            .expect("validated ConfigBundle membership")
    }
}

/// One scope-consistent view of every referenced config cell. Each
/// atomicity group is loaded exactly once; all values in that group therefore
/// come from the same immutable bundle generation even if a publisher swaps
/// the group while the caller resolves individual cell ids.
#[derive(Debug)]
pub struct ConfigScopeSnapshot {
    policy: ConfigBindingPolicy,
    descriptors: HashMap<ConfigCellId, ConfigCellDescriptor>,
    bundles: HashMap<AtomicityGroupId, Arc<ConfigBundle>>,
    bundle_sources: HashMap<AtomicityGroupId, usize>,
}

impl ConfigScopeSnapshot {
    fn empty(policy: ConfigBindingPolicy) -> Self {
        Self {
            policy,
            descriptors: HashMap::new(),
            bundles: HashMap::new(),
            bundle_sources: HashMap::new(),
        }
    }

    pub(super) fn acquire(
        handles: &[ConfigCellHandle],
        policy: ConfigBindingPolicy,
    ) -> Result<Self, PlanError> {
        let mut snapshot = Self::empty(policy);
        snapshot.extend(handles)?;
        Ok(snapshot)
    }

    fn extend(&mut self, handles: &[ConfigCellHandle]) -> Result<(), PlanError> {
        for handle in handles
            .iter()
            .filter(|handle| handle.descriptor.binding_policy == self.policy)
        {
            handle.ensure_policy(self.policy)?;
            let descriptor = handle.descriptor.clone();
            let group = descriptor.atomicity_group;
            let source = Arc::as_ptr(&handle.bundle) as usize;
            if self
                .bundle_sources
                .get(&group)
                .is_some_and(|active| *active != source)
            {
                return Err(PlanError::SplitConfigAtomicityGroup(group));
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = self.bundles.entry(group) {
                entry.insert(handle.bundle.load_full());
                self.bundle_sources.insert(group, source);
            }
            self.descriptors.insert(descriptor.id, descriptor);
        }
        Ok(())
    }

    pub fn policy(&self) -> ConfigBindingPolicy {
        self.policy
    }

    pub fn value(&self, id: ConfigCellId) -> Option<&ImmutableConfig> {
        let descriptor = self.descriptors.get(&id)?;
        self.bundles
            .get(&descriptor.atomicity_group)?
            .get(descriptor.id)
    }

    pub fn generations(&self) -> impl Iterator<Item = (ConfigCellId, ConfigGeneration)> + '_ {
        self.descriptors
            .keys()
            .copied()
            .filter_map(|id| self.value(id).map(|value| (id, value.generation)))
    }

    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }

    /// Stable, non-secret identity of the exact generation/value set bound to
    /// one connection scope. Transport pools use this in addition to the
    /// compiler-sealed target fingerprint so a live config rotation cannot
    /// reuse a socket created with an older trust/auth generation.
    pub fn stable_fingerprint(&self) -> [u8; 32] {
        let mut ids: Vec<_> = self.descriptors.keys().copied().collect();
        ids.sort_unstable_by_key(|id| id.0);
        let mut hasher = Sha256::new();
        hasher.update(b"hiroute-config-scope-v1");
        hasher.update([self.policy as u8]);
        for id in ids {
            let descriptor = self
                .descriptors
                .get(&id)
                .expect("id originated from descriptor map");
            let value = self.value(id).expect("validated snapshot membership");
            hasher.update(id.0.to_le_bytes());
            hasher.update(descriptor.atomicity_group.0.to_le_bytes());
            hasher.update(value.generation.0.to_le_bytes());
            hasher.update(value.compatibility_hash);
            hasher.update((value.bytes.len() as u64).to_le_bytes());
            hasher.update(value.bytes.as_ref());
        }
        hasher.finalize().into()
    }
}

#[derive(Debug)]
pub struct RequestConfigSnapshot {
    snapshot: ConfigScopeSnapshot,
}

impl Default for RequestConfigSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestConfigSnapshot {
    pub fn new() -> Self {
        Self {
            snapshot: ConfigScopeSnapshot::empty(ConfigBindingPolicy::RequestPinned),
        }
    }

    pub(super) fn acquire(handles: &[ConfigCellHandle]) -> Result<Self, PlanError> {
        Ok(Self {
            snapshot: ConfigScopeSnapshot::acquire(handles, ConfigBindingPolicy::RequestPinned)?,
        })
    }

    pub fn value(&self, id: ConfigCellId) -> Option<&ImmutableConfig> {
        self.snapshot.value(id)
    }

    pub fn generations(&self) -> impl Iterator<Item = (ConfigCellId, ConfigGeneration)> + '_ {
        self.snapshot.generations()
    }

    /// Release every request-pinned atomicity group that the accepted
    /// response can no longer reference. Groups shared with a retained cell
    /// remain pinned as one indivisible bundle.
    pub(crate) fn retain_only(&mut self, ids: &[ConfigCellId]) {
        let retained: std::collections::HashSet<_> = ids.iter().copied().collect();
        self.snapshot
            .descriptors
            .retain(|id, _| retained.contains(id));
        let retained_groups: std::collections::HashSet<_> = self
            .snapshot
            .descriptors
            .values()
            .map(|descriptor| descriptor.atomicity_group)
            .collect();
        self.snapshot
            .bundles
            .retain(|group, _| retained_groups.contains(group));
        self.snapshot
            .bundle_sources
            .retain(|group, _| retained_groups.contains(group));
    }
}

/// EventLive group guard. `!Send`/`!Sync` prevents a provider from retaining
/// a live view across an await; it may only inspect or copy bounded values in
/// the synchronous classify/encode callback that received it.
#[derive(Debug)]
pub struct ConfigEventSnapshot {
    pub(super) snapshot: ConfigScopeSnapshot,
    pub(super) _not_send: PhantomData<Rc<()>>,
}

impl ConfigEventSnapshot {
    pub fn value(&self, id: ConfigCellId) -> Option<&ImmutableConfig> {
        self.snapshot.value(id)
    }

    pub fn generations(&self) -> impl Iterator<Item = (ConfigCellId, ConfigGeneration)> + '_ {
        self.snapshot.generations()
    }
}
