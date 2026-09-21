//! Bounded legacy status projection.
use super::*;

pub(super) fn read_gaps(
    connection: &rusqlite::Connection,
    workspace_id: &WorkspaceId,
) -> Result<Vec<ObservationGapV1>, ObservationQueryError> {
    let mut statement = connection
        .prepare(
            "SELECT channel, producer_id, producer_epoch, stream_id, first_sequence,
                    last_sequence, known_loss, session_id, reason FROM observation_gaps
             WHERE workspace_id=?1 ORDER BY gap_id LIMIT 201",
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let gaps: Vec<ObservationGapV1> = statement
        .query_map([workspace_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })
        .map_err(|_| ObservationQueryError::Unavailable)?
        .map(|row| {
            let row = row.map_err(|_| ObservationQueryError::Corrupt)?;
            Ok(ObservationGapV1 {
                channel: match row.0.as_str() {
                    "fact" => ObservationChannel::Fact,
                    "content" => ObservationChannel::Content,
                    _ => return Err(ObservationQueryError::Corrupt),
                },
                stream: ObservationStreamV1 {
                    producer_id: hiroute_domain::ProducerId::parse(row.1)
                        .map_err(|_| ObservationQueryError::Corrupt)?,
                    producer_epoch: hiroute_domain::ProducerEpoch::parse(row.2)
                        .map_err(|_| ObservationQueryError::Corrupt)?,
                    stream_id: hiroute_domain::StreamId::parse(row.3)
                        .map_err(|_| ObservationQueryError::Corrupt)?,
                },
                first_sequence: row
                    .4
                    .try_into()
                    .map_err(|_| ObservationQueryError::Corrupt)?,
                last_sequence: row
                    .5
                    .try_into()
                    .map_err(|_| ObservationQueryError::Corrupt)?,
                known_loss: row.6,
                reason: row.8,
                session_id: row
                    .7
                    .map(SessionId::parse)
                    .transpose()
                    .map_err(|_| ObservationQueryError::Corrupt)?,
            })
        })
        .collect::<Result<_, _>>()?;
    if gaps.len() > 200 {
        return Err(ObservationQueryError::InvalidQuery);
    }
    Ok(gaps)
}

pub(super) fn aggregate_content_completeness(
    connection: &rusqlite::Connection,
    workspace_id: &WorkspaceId,
    gaps: &[ObservationGapV1],
) -> Result<ContentCompleteness, ObservationQueryError> {
    if gaps
        .iter()
        .any(|gap| gap.channel == ObservationChannel::Content)
    {
        return Ok(ContentCompleteness::Partial);
    }
    let values: String = connection
        .query_row(
            "SELECT COALESCE(GROUP_CONCAT(DISTINCT content_completeness), '')
             FROM sessions WHERE workspace_id=?1",
            [workspace_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    if values.split(',').any(|value| value == "partial") {
        Ok(ContentCompleteness::Partial)
    } else if values.split(',').any(|value| value == "unknown") {
        Ok(ContentCompleteness::Unknown)
    } else if values.split(',').any(|value| value == "complete") {
        Ok(ContentCompleteness::Complete)
    } else {
        Ok(ContentCompleteness::Unknown)
    }
}

pub(super) fn aggregate_facts_completeness(
    connection: &rusqlite::Connection,
    workspace_id: &WorkspaceId,
    gaps: &[ObservationGapV1],
) -> Result<FactsCompleteness, ObservationQueryError> {
    if gaps
        .iter()
        .any(|gap| gap.channel == ObservationChannel::Fact)
    {
        return Ok(FactsCompleteness::Partial);
    }
    let values: String = connection
        .query_row(
            "SELECT COALESCE(GROUP_CONCAT(DISTINCT facts_completeness), '')
             FROM sessions WHERE workspace_id=?1",
            [workspace_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    if values.split(',').any(|value| value == "partial") {
        Ok(FactsCompleteness::Partial)
    } else if values.split(',').any(|value| value == "unknown") {
        Ok(FactsCompleteness::Unknown)
    } else if values.split(',').any(|value| value == "complete") {
        Ok(FactsCompleteness::Complete)
    } else {
        Ok(FactsCompleteness::Unknown)
    }
}
