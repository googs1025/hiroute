//! One current trusted live-check result per settings context and client surface.

pub(super) const CONTROL: &str = r#"
CREATE TABLE agent_surface_checks (
    workspace_id TEXT NOT NULL,
    context_id TEXT NOT NULL,
    surface TEXT NOT NULL
        CHECK(surface IN ('codex_cli','codex_desktop','claude_cli')),
    applied_revision INTEGER NOT NULL
        CHECK(typeof(applied_revision) = 'integer' AND applied_revision > 0),
    record_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(workspace_id, context_id, surface)
);
"#;
