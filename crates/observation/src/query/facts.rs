use hiroute_domain::{ObservationQueryError, SessionId, WorkspaceId};
use rusqlite::params;

/// The legacy boolean can express only proven within-request fallback. Multiple
/// independent requests in a session are not evidence of automatic switching.
/// This safe projection remains available after raw sensitive payload deletion.
pub(super) fn session_model_switch(
    connection: &rusqlite::Connection,
    workspace_id: &WorkspaceId,
    session_id: &SessionId,
) -> Result<Option<bool>, ObservationQueryError> {
    let (count, switched): (u64, bool) = connection
        .query_row(
            "SELECT COUNT(*),COALESCE(MAX(model_count>1),0) FROM (
            SELECT a.request_id,COUNT(DISTINCT a.model_id) AS model_count
            FROM observation_attempt_models_v2 a JOIN logical_requests r
                ON r.workspace_id=a.workspace_id AND r.request_id=a.request_id
            WHERE a.workspace_id=?1 AND r.session_id=?2 AND a.model_id!=''
            GROUP BY a.request_id)",
            params![workspace_id.as_str(), session_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    Ok((count > 0).then_some(switched))
}
