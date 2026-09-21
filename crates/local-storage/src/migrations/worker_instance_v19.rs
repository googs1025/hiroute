//! Instance-scoped Worker identity and indexed latest-task metadata.

pub(super) const RUNTIME: &str = r#"
ALTER TABLE delegation_tasks RENAME TO delegation_tasks_v18;
ALTER TABLE delegation_runs RENAME TO delegation_runs_v18;
DROP INDEX delegation_runs_occupied;
DROP INDEX delegation_runs_task;
DROP INDEX IF EXISTS delegation_tasks_latest;
DROP INDEX IF EXISTS delegation_tasks_title_latest;

CREATE TABLE delegation_tasks (
    workspace_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    latest_admission_sequence INTEGER NOT NULL
        CHECK(typeof(latest_admission_sequence) = 'integer' AND latest_admission_sequence >= 0),
    title_lookup_key TEXT,
    record_json TEXT NOT NULL,
    PRIMARY KEY(workspace_id, task_id)
);

CREATE TABLE delegation_runs (
    run_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    submission_kind TEXT NOT NULL CHECK(submission_kind IN ('start','continue')),
    idempotency_key TEXT NOT NULL,
    occupied INTEGER NOT NULL CHECK(occupied IN (0,1)),
    record_json TEXT NOT NULL,
    UNIQUE(workspace_id, submission_kind, idempotency_key)
);

INSERT INTO delegation_tasks(
    workspace_id, task_id, latest_admission_sequence, title_lookup_key, record_json
)
SELECT
    task.workspace_id,
    task.task_id,
    COALESCE((
        SELECT MAX(CAST(json_extract(run.record_json, '$.admission_sequence') AS INTEGER))
        FROM delegation_runs_v18 run
        WHERE run.workspace_id = task.workspace_id AND run.task_id = task.task_id
    ), 0),
    NULL,
    json_set(
        json_remove(task.record_json, '$.owner_id'),
        '$.latest_admission_sequence',
        COALESCE((
            SELECT MAX(CAST(json_extract(run.record_json, '$.admission_sequence') AS INTEGER))
            FROM delegation_runs_v18 run
            WHERE run.workspace_id = task.workspace_id AND run.task_id = task.task_id
        ), 0),
        '$.title',
        json('null')
    )
FROM delegation_tasks_v18 task;

INSERT INTO delegation_runs(
    run_id, workspace_id, task_id, submission_kind, idempotency_key, occupied, record_json
)
SELECT
    run_id,
    workspace_id,
    task_id,
    submission_kind,
    idempotency_key,
    occupied,
    json_remove(record_json, '$.owner_id', '$.grant_id', '$.grant_generation')
FROM delegation_runs_v18;

CREATE INDEX delegation_runs_occupied ON delegation_runs(occupied, workspace_id);
CREATE INDEX delegation_runs_task ON delegation_runs(workspace_id, task_id);
CREATE INDEX delegation_tasks_latest
    ON delegation_tasks(workspace_id, latest_admission_sequence DESC);
CREATE INDEX delegation_tasks_title_latest
    ON delegation_tasks(workspace_id, title_lookup_key, latest_admission_sequence DESC);

DROP TABLE delegation_runs_v18;
DROP TABLE delegation_tasks_v18;
"#;
