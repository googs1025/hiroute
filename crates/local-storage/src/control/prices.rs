//! Price projection writes participate in the existing control effect activation transaction.
use super::{ControlStore, port};
use hiroute_domain::{
    CanonicalDigest, ComputeManagementSourceV2, ComputeSourceV1, PortErrorCode, PortResult,
    PriceModelIdentityV1, PriceTargetV1, SourceBindingV1, SourcePriceChangeV2,
    SourcePriceOverrideV1, WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

impl ControlStore {
    pub fn source_price_override(
        &self,
        target: &PriceTargetV1,
    ) -> PortResult<Option<SourcePriceOverrideV1>> {
        read_override(&self.connection.borrow(), target)
    }
    pub fn source_price_overrides(
        &self,
        workspace: &WorkspaceId,
    ) -> PortResult<Vec<SourcePriceOverrideV1>> {
        let c = self.connection.borrow();
        let mut statement = c.prepare("SELECT override_json, override_digest FROM source_price_overrides WHERE workspace_id=?1 ORDER BY target_digest").map_err(|_| port(PortErrorCode::Unavailable, "prices.list"))?;
        let rows = statement
            .query_map(params![workspace.as_str()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|_| port(PortErrorCode::Unavailable, "prices.rows"))?;
        rows.map(|row| {
            let (json, digest) = row.map_err(|_| port(PortErrorCode::Unavailable, "prices.row"))?;
            decode_override(&json, &digest)
        })
        .collect()
    }
    /// Consumed by the Application target resolver; no user-supplied digest enters this join.
    pub fn price_source_bindings(&self) -> PortResult<Vec<(ComputeSourceV1, SourceBindingV1)>> {
        let c = self.connection.borrow();
        let mut q = c.prepare("SELECT s.source_json,b.binding_json FROM compute_sources s JOIN source_bindings b ON s.source_id=b.source_id WHERE b.active=1 ORDER BY b.binding_id").map_err(|_| port(PortErrorCode::Unavailable, "prices.targets"))?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|_| port(PortErrorCode::Unavailable, "prices.targets"))?;
        rows.map(|row| {
            let (s, b) = row.map_err(|_| port(PortErrorCode::Unavailable, "prices.target"))?;
            let source: ComputeSourceV1 = serde_json::from_str(&s)
                .map_err(|_| port(PortErrorCode::Corrupt, "prices.source"))?;
            let binding: SourceBindingV1 = serde_json::from_str(&b)
                .map_err(|_| port(PortErrorCode::Corrupt, "prices.binding"))?;
            source
                .validate_shape()
                .map_err(|_| port(PortErrorCode::Corrupt, "prices.source"))?;
            binding
                .validate_shape()
                .map_err(|_| port(PortErrorCode::Corrupt, "prices.binding"))?;
            Ok((source, binding))
        })
        .collect()
    }
}
fn decode_override(json: &str, digest: &str) -> PortResult<SourcePriceOverrideV1> {
    let value: SourcePriceOverrideV1 =
        serde_json::from_str(json).map_err(|_| port(PortErrorCode::Corrupt, "prices.decode"))?;
    value
        .validate()
        .map_err(|_| port(PortErrorCode::Corrupt, "prices.validate"))?;
    if CanonicalDigest::of(&value)
        .map_err(|_| port(PortErrorCode::Corrupt, "prices.digest"))?
        .as_str()
        != digest
    {
        return Err(port(PortErrorCode::Corrupt, "prices.digest"));
    }
    Ok(value)
}
fn read_override(
    c: &Connection,
    target: &PriceTargetV1,
) -> PortResult<Option<SourcePriceOverrideV1>> {
    let key = target
        .digest()
        .map_err(|_| port(PortErrorCode::InvalidData, "prices.target"))?;
    let row = c.query_row("SELECT override_json,override_digest FROM source_price_overrides WHERE target_digest=?1", params![key.as_str()], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?))).optional().map_err(|_| port(PortErrorCode::Unavailable, "prices.read"))?;
    row.map(|(json, digest)| {
        let value = decode_override(&json, &digest)?;
        if value.target != *target {
            return Err(port(PortErrorCode::Corrupt, "prices.target_digest"));
        }
        Ok(value)
    })
    .transpose()
}
fn staged_change(staged: &str) -> PortResult<Option<SourcePriceChangeV2>> {
    let root: Value =
        serde_json::from_str(staged).map_err(|_| port(PortErrorCode::Corrupt, "prices.stage"))?;
    let Some(raw) = root
        .get("value")
        .and_then(|v| v.get("source_price_change_v2"))
    else {
        return Ok(None);
    };
    let change: SourcePriceChangeV2 = serde_json::from_value(raw.clone())
        .map_err(|_| port(PortErrorCode::Corrupt, "prices.stage"))?;
    change
        .validate()
        .map_err(|_| port(PortErrorCode::Corrupt, "prices.stage"))?;
    Ok(Some(change))
}
pub(super) fn activate(c: &Connection, workspace: &WorkspaceId, staged: &str) -> PortResult<()> {
    let Some(change) = staged_change(staged)? else {
        return Ok(());
    };
    if change.target.workspace_id != *workspace
        || read_override(c, &change.target)? != change.before
    {
        return Err(port(PortErrorCode::Conflict, "prices.override_cas"));
    }
    let legacy_source: Option<String> = c
        .query_row(
            "SELECT source_json FROM compute_sources WHERE source_id=?1",
            params![change.target.source_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| port(PortErrorCode::Unavailable, "prices.source"))?;
    let model = match &change.target.model_identity {
        PriceModelIdentityV1::CatalogModel(id) | PriceModelIdentityV1::LocalModel(id) => id,
    };
    let legacy_matches = if let Some(source) = legacy_source.as_ref() {
        let source: ComputeSourceV1 = serde_json::from_str(source)
            .map_err(|_| port(PortErrorCode::Corrupt, "prices.source"))?;
        let binding: Option<String> = c
            .query_row(
                "SELECT binding_json FROM source_bindings WHERE binding_id=?1 AND active=1",
                params![change.binding_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| port(PortErrorCode::Unavailable, "prices.binding"))?;
        if let Some(binding) = binding {
            let binding: SourceBindingV1 = serde_json::from_str(&binding)
                .map_err(|_| port(PortErrorCode::Corrupt, "prices.binding"))?;
            source.revision == change.expected_source_revision
                && source.identity_digest == change.target.source_identity_digest
                && binding.source_id == source.source_id
                && binding.source_identity_digest == source.identity_digest
                && binding.revision == change.expected_binding_revision
                && binding.model_configuration_id == *model
        } else {
            false
        }
    } else {
        false
    };
    let management_source: Option<String> = c
        .query_row(
            "SELECT source_json FROM compute_management_sources
             WHERE workspace_id=?1 AND source_id=?2",
            params![workspace.as_str(), change.target.source_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| port(PortErrorCode::Unavailable, "prices.management_source"))?;
    let management_matches = if let Some(source) = management_source.as_ref() {
        let source: ComputeManagementSourceV2 = serde_json::from_str(source)
            .map_err(|_| port(PortErrorCode::Corrupt, "prices.management_source"))?;
        source
            .validate()
            .map_err(|_| port(PortErrorCode::Corrupt, "prices.management_source"))?;
        source
            .models
            .iter()
            .find(|candidate| candidate.binding_id == change.binding_id)
            .is_some_and(|binding| {
                let stored_model = binding
                    .catalog_configuration_id
                    .as_ref()
                    .unwrap_or(&binding.model_ref);
                source.revision == change.expected_source_revision
                    && source.lineage_digest == change.target.source_identity_digest
                    && binding.revision == change.expected_binding_revision
                    && stored_model == model
            })
    } else {
        false
    };
    if !legacy_matches && !management_matches {
        if legacy_source.is_none() && management_source.is_none() {
            return Err(port(PortErrorCode::NotFound, "prices.source"));
        }
        return Err(port(PortErrorCode::Conflict, "prices.identity_cas"));
    }
    write_override(c, &change.after)
}
fn write_override(c: &Connection, value: &SourcePriceOverrideV1) -> PortResult<()> {
    let key = value
        .target
        .digest()
        .map_err(|_| port(PortErrorCode::InvalidData, "prices.target"))?;
    let digest = CanonicalDigest::of(value)
        .map_err(|_| port(PortErrorCode::InvalidData, "prices.digest"))?;
    let json = serde_json::to_string(value)
        .map_err(|_| port(PortErrorCode::InvalidData, "prices.encode"))?;
    // SQLite INTEGER cannot represent arbitrary u64 revisions; reject before binding.
    if value.revision > i64::MAX as u64 {
        return Err(port(PortErrorCode::InvalidData, "prices.revision"));
    }
    c.execute("INSERT INTO source_price_overrides(target_digest,workspace_id,source_id,revision,override_json,override_digest) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(target_digest) DO UPDATE SET revision=excluded.revision,override_json=excluded.override_json,override_digest=excluded.override_digest",params![key.as_str(),value.target.workspace_id.as_str(),value.target.source_id,value.revision,json,digest.as_str()]).map_err(|_|port(PortErrorCode::Unavailable,"prices.write"))?;
    Ok(())
}
pub(super) fn compensate(c: &Connection, operation_id: &str) -> PortResult<bool> {
    let staged: Option<String> = c
        .query_row(
            "SELECT staged_json FROM control_effects WHERE operation_id=?1",
            params![operation_id],
            |r| r.get(0),
        )
        .map_err(|_| port(PortErrorCode::Unavailable, "prices.compensate.read"))?;
    let Some(change) = staged.as_deref().map(staged_change).transpose()?.flatten() else {
        return Ok(true);
    };
    if read_override(c, &change.target)? != Some(change.after) {
        return Ok(false);
    }
    match change.before {
        Some(value) => write_override(c, &value)?,
        None => {
            c.execute(
                "DELETE FROM source_price_overrides WHERE target_digest=?1",
                params![
                    change
                        .target
                        .digest()
                        .map_err(|_| port(PortErrorCode::Corrupt, "prices.target"))?
                        .as_str()
                ],
            )
            .map_err(|_| port(PortErrorCode::Unavailable, "prices.compensate.delete"))?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
