use hiroute_domain::{ObservationQueryError, SessionId, WorkspaceId};
use rusqlite::params;

/// The V1 boolean has no partial cursor. It can consume a ready bounded index,
/// but must fail explicitly while sources are still unindexed.
pub(super) fn session_contains(
    connection: &rusqlite::Connection,
    _authority: &crate::DigestAuthority,
    workspace_id: &WorkspaceId,
    session_id: &SessionId,
    query: &str,
    budget: &mut (usize, usize),
) -> Result<bool, ObservationQueryError> {
    let needle = query.trim().to_lowercase();
    if query.trim().len() > 256 {
        return Err(ObservationQueryError::InvalidQuery);
    }
    if needle.is_empty() {
        return Ok(true);
    }
    let pending:bool=connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM content_instances_v2 c JOIN logical_requests r ON r.workspace_id=c.workspace_id AND r.request_id=c.request_id
         LEFT JOIN observation_text_index_v2 i ON i.workspace=c.workspace_id AND i.digest=c.content_blob_digest
         WHERE c.workspace_id=?1 AND r.session_id=?2 AND c.state='complete' AND (c.canonical_media_type LIKE 'text/%' OR c.canonical_media_type='application/json' OR c.canonical_media_type LIKE 'application/vnd.hiroute.%') AND (i.state IS NULL OR i.state!='ready'))",
        params![workspace_id.as_str(),session_id.as_str()],|row|row.get(0),
    ).map_err(|_|ObservationQueryError::Unavailable)?;
    if pending {
        return Err(ObservationQueryError::Unavailable);
    }
    let mut statement=connection.prepare(
        "SELECT b.folded FROM observation_text_blocks_v2 b JOIN content_instances_v2 c ON c.workspace_id=b.workspace AND c.content_blob_digest=b.digest
         JOIN logical_requests r ON r.workspace_id=c.workspace_id AND r.request_id=c.request_id
         WHERE c.workspace_id=?1 AND r.session_id=?2 AND c.state='complete' LIMIT 201"
    ).map_err(|_|ObservationQueryError::Unavailable)?;
    let mut rows = statement
        .query(params![workspace_id.as_str(), session_id.as_str()])
        .map_err(|_| ObservationQueryError::Unavailable)?;
    while let Some(row) = rows
        .next()
        .map_err(|_| ObservationQueryError::Unavailable)?
    {
        budget.0 += 1;
        let text: &str = row
            .get_ref(0)
            .map_err(|_| ObservationQueryError::Unavailable)?
            .as_str()
            .map_err(|_| ObservationQueryError::Corrupt)?;
        budget.1 += text.len();
        if budget.0 > 200 || budget.1 > 8 * 1024 * 1024 {
            return Err(ObservationQueryError::InvalidQuery);
        }
        if text.contains(&needle) {
            return Ok(true);
        }
    }
    Ok(false)
}
