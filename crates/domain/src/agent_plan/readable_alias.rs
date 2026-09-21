//! Deterministic offline readable names using pinned local pinyin, never hashes or translation.
use super::*;
use pinyin::ToPinyin;

impl AliasRegistryV1 {
    /// A suggestion reserves nothing. Preview seals this exact value and registry revision;
    /// Apply uses `allocate_custom` on that value, never silently reruns disambiguation.
    pub fn suggest_alias(&self, display_name: &str) -> Result<ModelAlias, AgentPlanIdentityError> {
        self.validate()?;
        if display_name.chars().count() > 128 {
            return Err(AgentPlanIdentityError::InvalidDisplayName);
        }
        let mut slug = String::new();
        for ch in display_name.chars() {
            if ch.is_ascii_alphanumeric() {
                slug.push(ch.to_ascii_lowercase());
            } else if let Some(pinyin) = ch.to_pinyin() {
                for letter in pinyin.plain().chars() {
                    if letter.is_ascii_alphanumeric() {
                        slug.push(letter.to_ascii_lowercase());
                    } else if letter == 'ü' {
                        slug.push('v');
                    }
                }
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        let slug = slug.trim_end_matches('-');
        let base = if slug.is_empty() {
            "hiroute-plan".to_owned()
        } else if slug.starts_with("hiroute-") {
            slug.to_owned()
        } else {
            format!("hiroute-{slug}")
        };
        let occupied = self.active.len().saturating_add(self.tombstones.len());
        // At most N names can be occupied, so N+1 distinct suggestions suffice.
        for index in 0..=occupied {
            let suffix = if slug.is_empty() {
                format!("-{}", index + 1)
            } else if index == 0 {
                String::new()
            } else {
                format!("-{}", index + 1)
            };
            let stem = &base[..base.len().min(64 - suffix.len())];
            let alias =
                ModelAlias::parse_custom(format!("{}{suffix}", stem.trim_end_matches('-')))?;
            if !self.tombstones.contains(&alias) && !self.active.values().any(|a| a == &alias) {
                return Ok(alias);
            }
        }
        Err(AgentPlanIdentityError::SequenceExhausted)
    }

    pub fn allocate_named(
        &mut self,
        plan_id: AgentPlanId,
        display_name: &str,
    ) -> Result<ModelAlias, AgentPlanIdentityError> {
        self.validate()?;
        AgentPlanId::parse(plan_id.as_str())?;
        if self.retired_plan_ids.contains(&plan_id) {
            return Err(AgentPlanIdentityError::RetiredPlanReused);
        }
        if let Some(alias) = self.active.get(&plan_id) {
            return Ok(alias.clone());
        }
        let alias = self.suggest_alias(display_name)?;
        let next = self
            .next_sequence
            .checked_add(1)
            .ok_or(AgentPlanIdentityError::SequenceExhausted)?;
        self.allocate_custom(plan_id, alias.clone())?;
        self.next_sequence = next;
        Ok(alias)
    }
}
