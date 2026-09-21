//! Branch-local migration intent. The coordinator assigns the integrated schema version.
pub(crate) const RUNTIME: &str = r#"
CREATE TABLE delegation_tasks (
    workspace_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY(workspace_id, task_id)
);
CREATE TABLE delegation_runs (
    run_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    submission_kind TEXT NOT NULL CHECK(submission_kind IN ('start','continue')),
    idempotency_key TEXT NOT NULL,
    occupied INTEGER NOT NULL CHECK(occupied IN (0,1)),
    record_json TEXT NOT NULL,
    UNIQUE(workspace_id, owner_id, submission_kind, idempotency_key)
);
CREATE INDEX delegation_runs_occupied ON delegation_runs(occupied, workspace_id);
CREATE INDEX delegation_runs_task ON delegation_runs(workspace_id, task_id);
CREATE TABLE delegation_run_events (
    run_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    event_json TEXT NOT NULL,
    PRIMARY KEY(run_id, event_id)
);
CREATE TABLE delegation_cancel_receipts (
    run_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY(run_id, operation_id)
);
CREATE TABLE delegation_sequence (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    sequence INTEGER NOT NULL CHECK(sequence>=0)
);
INSERT INTO delegation_sequence(singleton,sequence) VALUES(1,0);
"#;
