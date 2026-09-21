use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledFilterDescriptor {
    pub id: Arc<str>,
    pub max_pending_frames: usize,
    capabilities: FilterCapabilities,
    config_dependencies: Arc<[FilterConfigDependency]>,
}

impl CompiledFilterDescriptor {
    pub fn new(id: impl Into<Arc<str>>, max_pending_frames: usize) -> Result<Self, FilterError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(FilterError::InvalidDescriptor);
        }
        if max_pending_frames == 0 {
            return Err(FilterError::ZeroPendingLimit);
        }
        Ok(Self {
            id,
            max_pending_frames,
            capabilities: FilterCapabilities::observe_only(),
            config_dependencies: Arc::new([]),
        })
    }

    pub fn with_capabilities(mut self, capabilities: FilterCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn capabilities(&self) -> FilterCapabilities {
        self.capabilities
    }

    pub fn with_config_dependencies(
        mut self,
        dependencies: impl Into<Arc<[FilterConfigDependency]>>,
    ) -> Result<Self, FilterError> {
        let dependencies = dependencies.into();
        let mut ids = std::collections::HashSet::new();
        if dependencies
            .iter()
            .any(|dependency| !ids.insert(dependency.id))
        {
            return Err(FilterError::DuplicateConfigDependency);
        }
        self.config_dependencies = dependencies;
        Ok(self)
    }

    pub fn config_dependencies(&self) -> &[FilterConfigDependency] {
        &self.config_dependencies
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilterConfigDependency {
    pub id: ConfigCellId,
    pub policy: ConfigBindingPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterConfigValue {
    pub id: ConfigCellId,
    pub policy: ConfigBindingPolicy,
    pub generation: ConfigGeneration,
    pub value: ImmutableConfig,
}

/// Compact owned callback state. No ArcSwap handle or EventLive guard enters
/// an async native hook; the lifecycle resolves exactly the declared cells
/// before calling the filter owner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilterConfigSnapshot {
    values: Arc<[FilterConfigValue]>,
}

impl FilterConfigSnapshot {
    pub fn new(values: impl Into<Arc<[FilterConfigValue]>>) -> Self {
        Self {
            values: values.into(),
        }
    }

    pub fn value(&self, id: ConfigCellId) -> Option<&ImmutableConfig> {
        self.values
            .iter()
            .find(|value| value.id == id)
            .map(|value| &value.value)
    }

    pub fn generations(&self) -> impl Iterator<Item = (ConfigCellId, ConfigGeneration)> + '_ {
        self.values.iter().map(|value| (value.id, value.generation))
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Compiler-sealed declaration of every body-affecting behavior a native
/// filter may exercise. Runtime factories may implement a strict subset, but
/// cannot widen these flags after publication validation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FilterCapabilities {
    body_mutation: bool,
    body_expansion: bool,
    body_drop: bool,
    local_reply: bool,
    header_mutation_during_body: bool,
    semantic_provenance: bool,
}

impl FilterCapabilities {
    pub const fn observe_only() -> Self {
        Self {
            body_mutation: false,
            body_expansion: false,
            body_drop: false,
            local_reply: false,
            header_mutation_during_body: false,
            semantic_provenance: false,
        }
    }

    pub const fn with_body_mutation(mut self) -> Self {
        self.body_mutation = true;
        self
    }

    pub const fn with_body_expansion(mut self) -> Self {
        self.body_mutation = true;
        self.body_expansion = true;
        self
    }

    pub const fn with_body_drop(mut self) -> Self {
        self.body_drop = true;
        self
    }

    pub const fn with_local_reply(mut self) -> Self {
        self.local_reply = true;
        self
    }

    pub const fn with_header_mutation_during_body(mut self) -> Self {
        self.header_mutation_during_body = true;
        self
    }

    pub const fn with_semantic_provenance(mut self) -> Self {
        self.semantic_provenance = true;
        self
    }

    pub const fn mutates_body(self) -> bool {
        self.body_mutation
    }

    pub const fn drops_body(self) -> bool {
        self.body_drop
    }

    pub const fn expands_body(self) -> bool {
        self.body_expansion
    }

    pub const fn may_local_reply(self) -> bool {
        self.local_reply
    }

    pub const fn mutates_headers_during_body(self) -> bool {
        self.header_mutation_during_body
    }

    pub const fn requires_semantic_provenance(self) -> bool {
        self.semantic_provenance
    }

    pub const fn affects_body_commit(self) -> bool {
        self.body_mutation || self.body_drop || self.local_reply || self.header_mutation_during_body
    }

    pub const fn allows(self, requested: Self) -> bool {
        (!requested.body_mutation || self.body_mutation)
            && (!requested.body_expansion || self.body_expansion)
            && (!requested.body_drop || self.body_drop)
            && (!requested.local_reply || self.local_reply)
            && (!requested.header_mutation_during_body || self.header_mutation_during_body)
            && (!requested.semantic_provenance || self.semantic_provenance)
    }
}
