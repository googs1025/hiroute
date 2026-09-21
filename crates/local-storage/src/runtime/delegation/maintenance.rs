use super::*;
use rusqlite::TransactionBehavior;

pub(super) fn tasks(
    connection: &Connection,
    after: Option<&DelegationTaskMaintenanceCursorV1>,
    limit: u16,
) -> Result<Vec<DelegationTaskV1>> {
    if limit == 0
        || limit > 200
        || after.is_some_and(|cursor| {
            WorkspaceId::parse(cursor.workspace_id.as_str()).is_err()
                || !valid_delegation_id(&cursor.task_id)
        })
    {
        return Err(DelegationErrorV1::InvalidArguments);
    }
    let (after_workspace, after_task) = after.map_or((None, None), |cursor| {
        (
            Some(cursor.workspace_id.as_str()),
            Some(cursor.task_id.as_str()),
        )
    });
    let mut statement = connection
        .prepare(
            "SELECT record_json FROM delegation_tasks
             WHERE (title_lookup_key IS NOT NULL
                    OR json_extract(record_json,'$.title') IS NOT NULL
                    OR json_extract(record_json,'$.resume_until_ms') > 0)
               AND (?1 IS NULL OR workspace_id>?1 OR (workspace_id=?1 AND task_id>?2))
             ORDER BY workspace_id,task_id LIMIT ?3",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![after_workspace, after_task, limit], |row| {
            row.get::<_, String>(0)
        })
        .map_err(storage)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(decode(row.map_err(storage)?)?);
    }
    Ok(result)
}

pub(super) fn clear_title(store: &RuntimeStore, expected: &DelegationTaskV1) -> Result<bool> {
    let mut connection = store.connection.borrow_mut();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let mut current = read_task(&transaction, &expected.workspace_id, &expected.task_id)?
        .ok_or(DelegationErrorV1::Conflict)?;
    if &current != expected {
        return Err(DelegationErrorV1::Conflict);
    }
    if current.title.is_none() {
        return Ok(false);
    }
    current.title = None;
    write_task_with_title_key(&transaction, &current, None)?;
    transaction.commit().map_err(storage)?;
    Ok(true)
}

pub(super) fn claim_continuation_release(
    store: &RuntimeStore,
    expected: &DelegationTaskV1,
) -> Result<DelegationContinuationReleaseV1> {
    let release = DelegationContinuationReleaseV1 {
        workspace_id: expected.workspace_id.clone(),
        task_id: expected.task_id.clone(),
        latest_run_id: expected.latest_run_id.clone(),
        resume_until_ms: expected.resume_until_ms,
        task: expected.clone(),
    };
    release.validate()?;
    let mut connection = store.connection.borrow_mut();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    if let Some(existing) = read_release(
        &transaction,
        &release.workspace_id,
        &release.task_id,
        &release.latest_run_id,
    )? {
        return if existing == release {
            Ok(existing)
        } else {
            Err(DelegationErrorV1::Conflict)
        };
    }
    let another: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM delegation_continuation_releases
             WHERE workspace_id=?1 AND task_id=?2 AND state='pending')",
            params![release.workspace_id.as_str(), release.task_id],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if another {
        return Err(DelegationErrorV1::Conflict);
    }
    let mut current = read_task(&transaction, &expected.workspace_id, &expected.task_id)?
        .ok_or(DelegationErrorV1::Conflict)?;
    if &current != expected {
        return Err(DelegationErrorV1::Conflict);
    }
    current.resume_until_ms = 0;
    write_task(&transaction, &current)?;
    transaction
        .execute(
            "INSERT INTO delegation_continuation_releases(
                workspace_id,task_id,latest_run_id,resume_until_ms,state,task_json
             ) VALUES(?1,?2,?3,?4,'pending',?5)",
            params![
                release.workspace_id.as_str(),
                release.task_id,
                release.latest_run_id,
                release.resume_until_ms,
                encode(&release.task)?,
            ],
        )
        .map_err(storage)?;
    transaction.commit().map_err(storage)?;
    Ok(release)
}

pub(super) fn pending_continuation_releases(
    connection: &Connection,
    limit: u16,
) -> Result<Vec<DelegationContinuationReleaseV1>> {
    if limit == 0 || limit > 200 {
        return Err(DelegationErrorV1::InvalidArguments);
    }
    let mut statement = connection
        .prepare(
            "SELECT workspace_id,task_id,latest_run_id,resume_until_ms,task_json
             FROM delegation_continuation_releases WHERE state='pending'
             ORDER BY workspace_id,task_id,latest_run_id LIMIT ?1",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map([limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(storage)?;
    let mut releases = Vec::new();
    for row in rows {
        let (workspace, task_id, latest_run_id, resume_until_ms, task_json) =
            row.map_err(storage)?;
        let release = DelegationContinuationReleaseV1 {
            workspace_id: WorkspaceId::parse(workspace)
                .map_err(|_| DelegationErrorV1::StorageUnavailable)?,
            task_id,
            latest_run_id,
            resume_until_ms,
            task: decode(task_json)?,
        };
        release
            .validate()
            .map_err(|_| DelegationErrorV1::StorageUnavailable)?;
        releases.push(release);
    }
    Ok(releases)
}

pub(super) fn complete_continuation_release(
    store: &RuntimeStore,
    release: &DelegationContinuationReleaseV1,
) -> Result<()> {
    release.validate()?;
    let connection = store.connection.borrow();
    let current = read_release(
        &connection,
        &release.workspace_id,
        &release.task_id,
        &release.latest_run_id,
    )?
    .ok_or(DelegationErrorV1::Conflict)?;
    if &current != release {
        return Err(DelegationErrorV1::Conflict);
    }
    connection
        .execute(
            "UPDATE delegation_continuation_releases SET state='complete'
             WHERE workspace_id=?1 AND task_id=?2 AND latest_run_id=?3 AND state='pending'",
            params![
                release.workspace_id.as_str(),
                release.task_id,
                release.latest_run_id
            ],
        )
        .map_err(storage)?;
    Ok(())
}

fn read_release(
    connection: &Connection,
    workspace: &WorkspaceId,
    task_id: &str,
    latest_run_id: &str,
) -> Result<Option<DelegationContinuationReleaseV1>> {
    let row: Option<(u64, String)> = connection
        .query_row(
            "SELECT resume_until_ms,task_json FROM delegation_continuation_releases
             WHERE workspace_id=?1 AND task_id=?2 AND latest_run_id=?3",
            params![workspace.as_str(), task_id, latest_run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    let Some((resume_until_ms, task_json)) = row else {
        return Ok(None);
    };
    let release = DelegationContinuationReleaseV1 {
        workspace_id: workspace.clone(),
        task_id: task_id.to_owned(),
        latest_run_id: latest_run_id.to_owned(),
        resume_until_ms,
        task: decode(task_json)?,
    };
    release
        .validate()
        .map_err(|_| DelegationErrorV1::StorageUnavailable)?;
    Ok(Some(release))
}
