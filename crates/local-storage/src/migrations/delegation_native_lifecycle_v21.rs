//! Durable task-native root ownership, per-run uses, cleanup claims and continuation releases.

pub(super) const RUNTIME: &str = r#"
CREATE TABLE delegation_native_roots (
    workspace_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    root_generation INTEGER NOT NULL
        CHECK(typeof(root_generation) = 'integer' AND root_generation > 0),
    state TEXT NOT NULL
        CHECK(state IN ('creating','ready','no_native','unknown','deleting','removed')),
    use_revision INTEGER NOT NULL
        CHECK(typeof(use_revision) = 'integer' AND use_revision > 0),
    record_json TEXT NOT NULL,
    PRIMARY KEY(workspace_id, task_id)
);

CREATE TABLE delegation_native_uses (
    workspace_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    root_generation INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK(state IN ('accepted','may_have_spawned','stopped','never_spawned')),
    record_json TEXT NOT NULL,
    PRIMARY KEY(workspace_id, run_id)
);

CREATE INDEX delegation_native_uses_root
    ON delegation_native_uses(workspace_id, task_id, root_generation, run_id);
CREATE INDEX delegation_native_uses_scope
    ON delegation_native_uses(workspace_id, task_id, run_id);

CREATE TABLE delegation_continuation_releases (
    workspace_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    latest_run_id TEXT NOT NULL,
    resume_until_ms INTEGER NOT NULL
        CHECK(typeof(resume_until_ms) = 'integer' AND resume_until_ms > 0),
    state TEXT NOT NULL CHECK(state IN ('pending','complete')),
    task_json TEXT NOT NULL,
    PRIMARY KEY(workspace_id, task_id, latest_run_id)
);
CREATE INDEX delegation_continuation_releases_pending
    ON delegation_continuation_releases(state, workspace_id, task_id, latest_run_id);
"#;
