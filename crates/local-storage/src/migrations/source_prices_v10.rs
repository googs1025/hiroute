// Branch-local migration intent; the project convergence owner assigns the integrated number.
pub(super) const CONTROL_V10: &str = "
CREATE TABLE source_price_overrides (
    target_digest TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    override_json TEXT NOT NULL,
    override_digest TEXT NOT NULL
);
CREATE INDEX source_price_workspace ON source_price_overrides(workspace_id, source_id);
";
