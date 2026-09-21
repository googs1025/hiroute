//! Legacy content DTO assembly; V2 request pagination lives in query_v2.
use super::*;
use std::fs;
use std::io::Read;

pub(super) fn messages(
    connection: &rusqlite::Connection,
    authority: &crate::DigestAuthority,
    workspace_id: &WorkspaceId,
    turn_id: &TurnId,
    content_mode: ContentMode,
    remaining_bytes: &mut usize,
) -> Result<Vec<StoredMessageV1>, ObservationQueryError> {
    let mut statement = connection
        .prepare(
            "SELECT c.message_instance_id, c.content_id, c.content_blob_digest,
                    s.result_transcript_root, s.parent_transcript_root, c.direction,
                    c.content_kind, m.message_role, c.canonical_media_type,
                    c.created_at_unix_nanos, b.object_path, c.request_id, c.fork_id,
                    m.message_ordinal, c.part_ordinal, s.attempt_id, c.has_content_ref,
                    c.expected_byte_count, c.transport_frame_id, c.downstream_delivery
             FROM content_instances_v2 c
             JOIN content_message_instances_v2 m
               ON m.workspace_id=c.workspace_id AND m.message_instance_id=c.message_instance_id
             JOIN content_blobs_v2 b
               ON b.workspace_id=c.workspace_id AND b.blob_digest=c.content_blob_digest
             LEFT JOIN conversation_content_streams_v2 s
               ON s.workspace_id=c.workspace_id AND s.request_id=c.request_id
              AND s.direction=c.direction AND s.fork_id=c.fork_id
             JOIN logical_requests r
               ON r.workspace_id=c.workspace_id AND r.request_id=c.request_id
             WHERE c.workspace_id=?1 AND r.turn_id=?2 AND c.state='complete'
             ORDER BY m.message_ordinal, c.part_ordinal, c.content_id LIMIT 201",
        )
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let rows = statement
        .query_map(params![workspace_id.as_str(), turn_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, i64>(13)?,
                row.get::<_, i64>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, bool>(16)?,
                row.get::<_, Option<i64>>(17)?,
                row.get::<_, Option<String>>(18)?,
                row.get::<_, Option<String>>(19)?,
            ))
        })
        .map_err(|_| ObservationQueryError::Unavailable)?;
    let mut messages = Vec::new();
    for row in rows {
        let row = row.map_err(|_| ObservationQueryError::Corrupt)?;
        if content_mode == ContentMode::Messages && is_tool_kind(&row.6) {
            continue;
        }
        let direction: ConversationContentDirectionV2 = parse_enum(&row.5)?;
        let blob_digest =
            ContentBlobDigest::parse(row.2).map_err(|_| ObservationQueryError::Corrupt)?;
        let content_id =
            ContentId::parse(row.1.clone()).map_err(|_| ObservationQueryError::Corrupt)?;
        *remaining_bytes = remaining_bytes
            .checked_sub(4096)
            .ok_or(ObservationQueryError::InvalidQuery)?;
        let mut bytes = Vec::new();
        fs::File::open(&row.10)
            .map_err(|_| ObservationQueryError::Unavailable)?
            .take((*remaining_bytes as u64) + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ObservationQueryError::Unavailable)?;
        *remaining_bytes = remaining_bytes
            .checked_sub(bytes.len())
            .ok_or(ObservationQueryError::InvalidQuery)?;
        let mut accumulator = authority.content_accumulator(&row.8);
        accumulator.update(&bytes);
        if accumulator.finish().0 != blob_digest {
            return Err(ObservationQueryError::Corrupt);
        }
        let content_ref = if row.16 {
            Some(ContentRefV2 {
                content_id: content_id.clone(),
                digest: blob_digest.clone(),
                byte_count: row
                    .17
                    .ok_or(ObservationQueryError::Corrupt)?
                    .try_into()
                    .map_err(|_| ObservationQueryError::Corrupt)?,
                media_type: row.8.clone(),
            })
        } else {
            None
        };
        messages.push(StoredMessageV1 {
            request_id: hiroute_domain::LogicalRequestId::parse(row.11)
                .map_err(|_| ObservationQueryError::Corrupt)?,
            attempt_id: row
                .15
                .map(AttemptId::parse)
                .transpose()
                .map_err(|_| ObservationQueryError::Corrupt)?,
            fork_id: row.12,
            message_instance_id: MessageInstanceId::parse(row.0)
                .map_err(|_| ObservationQueryError::Corrupt)?,
            content_id,
            blob_digest: blob_digest.clone(),
            transcript_root: row
                .3
                .map(TranscriptRoot::parse)
                .transpose()
                .map_err(|_| ObservationQueryError::Corrupt)?,
            parent_transcript_root: row
                .4
                .map(TranscriptRoot::parse)
                .transpose()
                .map_err(|_| ObservationQueryError::Corrupt)?,
            direction,
            message_ordinal: row
                .13
                .try_into()
                .map_err(|_| ObservationQueryError::Corrupt)?,
            part_ordinal: row
                .14
                .try_into()
                .map_err(|_| ObservationQueryError::Corrupt)?,
            kind: row.6,
            role: row.7,
            media_type: row.8,
            transport_frame_id: row.18,
            content_ref,
            downstream_delivery: row.19.as_deref().map(parse_enum).transpose()?,
            occurred_at_unix_nanos: row
                .9
                .try_into()
                .map_err(|_| ObservationQueryError::Corrupt)?,
            bytes,
        });
    }
    Ok(messages)
}

fn is_tool_kind(kind: &str) -> bool {
    kind.starts_with("tool_") || kind.contains("tool_call") || kind.contains("tool_result")
}
