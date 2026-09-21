//! MVP-13 branch-local migration intent; the coordinator assigns the integrated sequence.
pub(crate) const CONTROL: &str = r#"
CREATE TABLE plan_drafts (
    workspace_id TEXT NOT NULL,
    draft_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    draft_json TEXT NOT NULL,
    PRIMARY KEY(workspace_id, draft_id)
);
CREATE TABLE plan_versions (
    workspace_id TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    content_revision INTEGER NOT NULL CHECK(content_revision > 0),
    content_digest TEXT NOT NULL,
    version_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('prepared','published')),
    owner_operation_id TEXT NOT NULL,
    PRIMARY KEY(workspace_id, plan_id, content_revision)
);
CREATE TABLE plan_heads (
    workspace_id TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    head_revision INTEGER NOT NULL CHECK(head_revision > 0),
    head_json TEXT NOT NULL,
    PRIMARY KEY(workspace_id, plan_id)
);
CREATE TABLE plan_version_holds (
    workspace_id TEXT NOT NULL,
    owner_key TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    content_revision INTEGER NOT NULL,
    content_digest TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    confirmed_owner INTEGER NOT NULL CHECK(confirmed_owner IN (0,1)),
    PRIMARY KEY(workspace_id, owner_key),
    FOREIGN KEY(workspace_id, plan_id, content_revision)
      REFERENCES plan_versions(workspace_id, plan_id, content_revision)
);
CREATE TABLE plan_version_recovery (
    workspace_id TEXT PRIMARY KEY,
    ready INTEGER NOT NULL CHECK(ready IN (0,1))
);
CREATE INDEX plan_version_holds_content ON plan_version_holds(workspace_id, plan_id, content_revision);
"#;
