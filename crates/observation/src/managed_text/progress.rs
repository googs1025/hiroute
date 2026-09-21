use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use super::*;
use crate::LocalObservationStore;

const PROGRESS_RUN_BYTES: u64 = 1024 * 1024;
const PROGRESS_GLOBAL_BYTES: u64 = 64 * 1024 * 1024;
const PROGRESS_BLOCKS: u64 = 4096;
const PROGRESS_READ_BLOCKS: usize = 512;

#[derive(Clone, Copy)]
pub(super) struct ProgressLimits {
    pub run_bytes: u64,
    pub global_bytes: u64,
    pub blocks: u64,
    pub block_bytes: usize,
}

impl ProgressLimits {
    const DEFAULT: Self = Self {
        run_bytes: PROGRESS_RUN_BYTES,
        global_bytes: PROGRESS_GLOBAL_BYTES,
        blocks: PROGRESS_BLOCKS,
        block_bytes: PROGRESS_BATCH_BYTES,
    };

    fn valid(self) -> bool {
        self.run_bytes >= 4
            && self.global_bytes >= self.run_bytes
            && self.blocks > 0
            && (4..=PROGRESS_BATCH_BYTES).contains(&self.block_bytes)
            && self.block_bytes as u64 <= self.run_bytes
    }
}

#[derive(Clone, Copy)]
struct Header {
    created_ms: i64,
    segment: u64,
    head: u64,
    end: u64,
}

enum Snapshot {
    Read(ManagedTextProgressRead),
    Evicted(ManagedTextProgressRecovery),
    Gap(ManagedTextProgressRecovery),
}

impl LocalObservationStore {
    pub fn managed_text_progress_write_batch(
        &self,
        target: &ManagedTextProgressTarget,
        text: &str,
        reset_segment: bool,
        now_ms: i64,
    ) -> Result<ManagedTextProgressWriteOutcome, ManagedTextError> {
        self.progress_write_with_limits(
            target,
            text,
            reset_segment,
            now_ms,
            ProgressLimits::DEFAULT,
        )
    }

    pub(super) fn progress_write_with_limits(
        &self,
        target: &ManagedTextProgressTarget,
        text: &str,
        reset_segment: bool,
        now_ms: i64,
        limits: ProgressLimits,
    ) -> Result<ManagedTextProgressWriteOutcome, ManagedTextError> {
        let (scope, deadline) = checked_target(target, now_ms)?;
        if text.len() > PROGRESS_BATCH_BYTES || !limits.valid() {
            return Err(ManagedTextError::Invalid);
        }
        if now_ms >= deadline {
            return Ok(ManagedTextProgressWriteOutcome::Hidden);
        }
        // The producer's idle check intentionally reaches no SQLite connection at all.
        if text.is_empty() && !reset_segment {
            return Ok(ManagedTextProgressWriteOutcome::Noop);
        }

        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let (generation, cutoff) = barrier_or_default(&transaction, &scope)?;
        if target.created_at_ms <= cutoff {
            return Ok(ManagedTextProgressWriteOutcome::Hidden);
        }
        transaction.execute(
            "INSERT OR IGNORE INTO managed_text_scopes(scope) VALUES(?1)",
            [&scope],
        )?;

        let existing = load_header(&transaction, &scope)?;
        if existing.is_some_and(|header| header.created_ms != target.created_at_ms) {
            return Err(ManagedTextError::Conflict);
        }
        let mut header = match (existing, reset_segment) {
            (Some(previous), true) => {
                transaction.execute(
                    "DELETE FROM managed_text_progress_blocks WHERE scope=?1",
                    [&scope],
                )?;
                Header {
                    created_ms: previous.created_ms,
                    segment: previous
                        .segment
                        .checked_add(1)
                        .ok_or(ManagedTextError::Invalid)?,
                    head: 0,
                    end: 0,
                }
            }
            (None, true) => Header {
                created_ms: target.created_at_ms,
                segment: 1,
                head: 0,
                end: 0,
            },
            (Some(previous), false) => previous,
            (None, false) => Header {
                created_ms: target.created_at_ms,
                segment: 0,
                head: 0,
                end: 0,
            },
        };
        validate_header(header)?;
        upsert_header(&transaction, &scope, header)?;

        if !text.is_empty() {
            append_text(&transaction, &scope, &mut header, text, limits.block_bytes)?;
            trim_scope_to_bytes(&transaction, &scope, limits.run_bytes)?;
            trim_global_to_bytes(&transaction, limits.global_bytes)?;
            trim_global_to_blocks(&transaction, limits.blocks)?;
            header = load_header(&transaction, &scope)?.ok_or(ManagedTextError::Storage)?;
        }
        transaction.commit()?;
        Ok(ManagedTextProgressWriteOutcome::Committed {
            visibility_generation: generation,
            segment: header.segment,
            head: header.head,
            end: header.end,
        })
    }

    /// Read one immutable SQLite snapshot, then recheck only the privacy boundary before release.
    pub fn managed_text_progress_read(
        &self,
        target: &ManagedTextProgressTarget,
        cursor: Option<ManagedTextProgressCursor>,
        max_bytes: usize,
    ) -> Result<ManagedTextProgressRead, ManagedTextProgressReadError> {
        let snapshot_now = system_now_ms()?;
        self.progress_read_with_privacy_now(target, cursor, max_bytes, snapshot_now, None)
    }

    #[cfg(test)]
    pub(super) fn progress_read_at(
        &self,
        target: &ManagedTextProgressTarget,
        cursor: Option<ManagedTextProgressCursor>,
        max_bytes: usize,
        snapshot_now_ms: i64,
        privacy_now_ms: i64,
    ) -> Result<ManagedTextProgressRead, ManagedTextProgressReadError> {
        self.progress_read_with_privacy_now(
            target,
            cursor,
            max_bytes,
            snapshot_now_ms,
            Some(privacy_now_ms),
        )
    }

    fn progress_read_with_privacy_now(
        &self,
        target: &ManagedTextProgressTarget,
        cursor: Option<ManagedTextProgressCursor>,
        max_bytes: usize,
        snapshot_now_ms: i64,
        privacy_now_override_ms: Option<i64>,
    ) -> Result<ManagedTextProgressRead, ManagedTextProgressReadError> {
        if max_bytes == 0 || max_bytes > PROGRESS_READ_MAX_BYTES {
            return Err(ManagedTextProgressReadError::InvalidCursor);
        }
        let (scope, deadline) = checked_target(target, snapshot_now_ms)
            .map_err(|_| ManagedTextProgressReadError::Storage)?;
        if privacy_now_override_ms.is_some_and(|value| value < snapshot_now_ms) {
            return Err(ManagedTextProgressReadError::InvalidCursor);
        }
        let mut connection =
            Connection::open_with_flags(&self.activity_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| ManagedTextProgressReadError::Storage)?;
        let transaction = connection
            .transaction()
            .map_err(|_| ManagedTextProgressReadError::Storage)?;
        let (generation, cutoff) = barrier_or_default(&transaction, &scope)
            .map_err(|_| ManagedTextProgressReadError::Storage)?;
        let hidden = visibility(target.created_at_ms, deadline, cutoff, snapshot_now_ms);
        if let Some(state) = hidden {
            return if cursor.is_some() {
                Err(ManagedTextProgressReadError::Stale)
            } else {
                Ok(state)
            };
        }
        if cursor.is_some_and(|value| value.visibility_generation != generation) {
            return Err(ManagedTextProgressReadError::Stale);
        }

        let header =
            load_header(&transaction, &scope).map_err(|_| ManagedTextProgressReadError::Storage)?;
        let snapshot = match header {
            None => {
                if cursor.is_some_and(|value| value.segment != 0 || value.position != 0) {
                    return Err(ManagedTextProgressReadError::InvalidCursor);
                }
                Snapshot::Read(ManagedTextProgressRead::Missing {
                    visibility_generation: generation,
                })
            }
            Some(header) => {
                validate_header(header).map_err(|_| ManagedTextProgressReadError::Storage)?;
                if header.created_ms != target.created_at_ms {
                    return Err(ManagedTextProgressReadError::Storage);
                }
                if let Some(value) = cursor
                    && (value.segment > header.segment
                        || (value.segment == header.segment && value.position > header.end))
                {
                    return Err(ManagedTextProgressReadError::InvalidCursor);
                }
                match cursor {
                    Some(value) if value.segment < header.segment => {
                        Snapshot::Gap(recovery(generation, header))
                    }
                    Some(value) if value.position < header.head => {
                        Snapshot::Evicted(recovery(generation, header))
                    }
                    _ => {
                        let position = cursor.map_or(header.head, |value| value.position);
                        let page = read_page(
                            &transaction,
                            &scope,
                            header,
                            generation,
                            position,
                            max_bytes,
                        )?;
                        Snapshot::Read(ManagedTextProgressRead::Available(page))
                    }
                }
            }
        };
        transaction
            .commit()
            .map_err(|_| ManagedTextProgressReadError::Storage)?;
        let privacy_now_ms = privacy_now_override_ms.map_or_else(system_now_ms, Ok)?;
        self.finish_progress_snapshot(
            &connection,
            target,
            cursor,
            snapshot,
            generation,
            deadline,
            privacy_now_ms,
            &scope,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_progress_snapshot(
        &self,
        connection: &Connection,
        target: &ManagedTextProgressTarget,
        cursor: Option<ManagedTextProgressCursor>,
        mut snapshot: Snapshot,
        snapshot_generation: u64,
        deadline: i64,
        privacy_now_ms: i64,
        scope: &str,
    ) -> Result<ManagedTextProgressRead, ManagedTextProgressReadError> {
        let (fresh_generation, fresh_cutoff) = barrier_or_default(connection, scope)
            .map_err(|_| ManagedTextProgressReadError::Storage)?;
        if let Some(hidden) =
            visibility(target.created_at_ms, deadline, fresh_cutoff, privacy_now_ms)
        {
            return if cursor.is_some() {
                Err(ManagedTextProgressReadError::Stale)
            } else {
                Ok(hidden)
            };
        }
        if cursor.is_some() && fresh_generation != snapshot_generation {
            return Err(ManagedTextProgressReadError::Stale);
        }
        if fresh_generation != snapshot_generation {
            match &mut snapshot {
                Snapshot::Read(ManagedTextProgressRead::Missing {
                    visibility_generation,
                }) => *visibility_generation = fresh_generation,
                Snapshot::Read(ManagedTextProgressRead::Available(page)) => {
                    page.visibility_generation = fresh_generation;
                }
                Snapshot::Read(
                    ManagedTextProgressRead::Deleted | ManagedTextProgressRead::Expired,
                )
                | Snapshot::Evicted(_)
                | Snapshot::Gap(_) => return Err(ManagedTextProgressReadError::Storage),
            }
        }
        match snapshot {
            Snapshot::Read(read) => Ok(read),
            Snapshot::Evicted(recovery) => Err(ManagedTextProgressReadError::Evicted(recovery)),
            Snapshot::Gap(recovery) => Err(ManagedTextProgressReadError::Gap(recovery)),
        }
    }

    /// Independently remove expired or deleted progress BLOBs. Headers and scope barriers stay so
    /// delayed writers and old cursors cannot revive hidden text.
    pub fn managed_text_progress_gc(
        &self,
        now_ms: i64,
        limit: usize,
    ) -> Result<usize, ManagedTextError> {
        if now_ms < 0 || limit == 0 || limit > 200 {
            return Err(ManagedTextError::Invalid);
        }
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let blocks = {
            let mut statement = transaction.prepare(
                "SELECT b.scope,b.segment,b.start,b.end
                 FROM managed_text_progress_blocks b INDEXED BY managed_text_progress_oldest
                 JOIN managed_text_progress p ON p.scope=b.scope
                 JOIN managed_text_scopes s ON s.scope=b.scope
                 WHERE p.created_ms+?1<=?2 OR p.created_ms<=s.deleted_through_ms
                 ORDER BY b.order_seq LIMIT ?3",
            )?;
            statement
                .query_map(params![RETENTION_MS, now_ms, limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, u64>(2)?,
                        row.get::<_, u64>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (scope, segment, start, end) in &blocks {
            delete_full_block(&transaction, scope, *segment, *start, *end)?;
        }
        transaction.commit()?;
        Ok(blocks.len())
    }
}

fn checked_target(
    target: &ManagedTextProgressTarget,
    now_ms: i64,
) -> Result<(String, i64), ManagedTextError> {
    if now_ms < 0 || target.created_at_ms < 0 || target.created_at_ms > now_ms {
        return Err(ManagedTextError::Invalid);
    }
    let deadline = target
        .created_at_ms
        .checked_add(RETENTION_MS)
        .ok_or(ManagedTextError::Invalid)?;
    Ok((target.scope.key()?, deadline))
}

fn system_now_ms() -> Result<i64, ManagedTextProgressReadError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|time| i64::try_from(time.as_millis()).ok())
        .ok_or(ManagedTextProgressReadError::Storage)
}

fn visibility(
    created_ms: i64,
    deadline: i64,
    cutoff: i64,
    now_ms: i64,
) -> Option<ManagedTextProgressRead> {
    if now_ms >= deadline {
        Some(ManagedTextProgressRead::Expired)
    } else if created_ms <= cutoff {
        Some(ManagedTextProgressRead::Deleted)
    } else {
        None
    }
}

fn barrier_or_default(
    connection: &Connection,
    scope: &str,
) -> Result<(u64, i64), ManagedTextError> {
    Ok(connection
        .query_row(
            "SELECT generation,deleted_through_ms FROM managed_text_scopes WHERE scope=?1",
            [scope],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, -1)))
}

fn load_header(connection: &Connection, scope: &str) -> Result<Option<Header>, ManagedTextError> {
    connection
        .query_row(
            "SELECT created_ms,segment,head,end FROM managed_text_progress WHERE scope=?1",
            [scope],
            |row| {
                Ok(Header {
                    created_ms: row.get(0)?,
                    segment: row.get(1)?,
                    head: row.get(2)?,
                    end: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn validate_header(header: Header) -> Result<(), ManagedTextError> {
    if header.created_ms < 0
        || header.segment > i64::MAX as u64
        || header.head > i64::MAX as u64
        || header.end > i64::MAX as u64
        || header.head > header.end
    {
        return Err(ManagedTextError::Storage);
    }
    Ok(())
}

fn upsert_header(
    transaction: &Transaction<'_>,
    scope: &str,
    header: Header,
) -> Result<(), ManagedTextError> {
    transaction.execute(
        "INSERT INTO managed_text_progress(scope,created_ms,segment,head,end)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(scope) DO UPDATE SET
            created_ms=excluded.created_ms,segment=excluded.segment,
            head=excluded.head,end=excluded.end",
        params![
            scope,
            header.created_ms,
            header.segment,
            header.head,
            header.end
        ],
    )?;
    Ok(())
}

fn append_text(
    transaction: &Transaction<'_>,
    scope: &str,
    header: &mut Header,
    text: &str,
    block_bytes: usize,
) -> Result<(), ManagedTextError> {
    let mut remaining = text;
    let tail: Option<(u64, u64, Vec<u8>, String)> = transaction
        .query_row(
            "SELECT start,end,bytes,digest FROM managed_text_progress_blocks
             WHERE scope=?1 AND segment=?2 ORDER BY start DESC LIMIT 1",
            params![scope, header.segment],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((start, end, mut bytes, expected_digest)) = tail {
        validate_block(start, end, &bytes, &expected_digest, block_bytes)?;
        if end != header.end {
            return Err(ManagedTextError::Storage);
        }
        let room = block_bytes.saturating_sub(bytes.len());
        let take = prefix_boundary(remaining, room);
        if take > 0 {
            bytes.extend_from_slice(&remaining.as_bytes()[..take]);
            let new_end = end
                .checked_add(take as u64)
                .filter(|value| *value <= i64::MAX as u64)
                .ok_or(ManagedTextError::Invalid)?;
            transaction.execute(
                "UPDATE managed_text_progress_blocks SET end=?4,bytes=?5,digest=?6
                 WHERE scope=?1 AND segment=?2 AND start=?3",
                params![scope, header.segment, start, new_end, bytes, digest(&bytes)],
            )?;
            header.end = new_end;
            remaining = &remaining[take..];
        }
    } else if header.head != header.end {
        return Err(ManagedTextError::Storage);
    }

    while !remaining.is_empty() {
        let take = prefix_boundary(remaining, block_bytes);
        if take == 0 {
            return Err(ManagedTextError::Invalid);
        }
        let bytes = &remaining.as_bytes()[..take];
        let start = header.end;
        let end = start
            .checked_add(take as u64)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or(ManagedTextError::Invalid)?;
        let order_seq = allocate_order_seq(transaction)?;
        transaction.execute(
            "INSERT INTO managed_text_progress_blocks(
                scope,segment,start,end,bytes,digest,order_seq)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                scope,
                header.segment,
                start,
                end,
                bytes,
                digest(bytes),
                order_seq
            ],
        )?;
        header.end = end;
        remaining = &remaining[take..];
    }
    transaction.execute(
        "UPDATE managed_text_progress SET end=?2 WHERE scope=?1",
        params![scope, header.end],
    )?;
    Ok(())
}

fn prefix_boundary(text: &str, limit: usize) -> usize {
    let mut boundary = text.len().min(limit);
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn allocate_order_seq(transaction: &Transaction<'_>) -> Result<u64, ManagedTextError> {
    let current: u64 = transaction.query_row(
        "SELECT next_order_seq FROM managed_text_progress_meta WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    let next = current
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or(ManagedTextError::Storage)?;
    let changed = transaction.execute(
        "UPDATE managed_text_progress_meta SET next_order_seq=?1
         WHERE singleton=1 AND next_order_seq=?2",
        params![next, current],
    )?;
    if changed != 1 {
        return Err(ManagedTextError::Storage);
    }
    Ok(current)
}

fn trim_scope_to_bytes(
    transaction: &Transaction<'_>,
    scope: &str,
    limit: u64,
) -> Result<(), ManagedTextError> {
    loop {
        let total: u64 = transaction.query_row(
            "SELECT COALESCE(SUM(length(bytes)),0)
             FROM managed_text_progress_blocks WHERE scope=?1",
            [scope],
            |row| row.get(0),
        )?;
        if total <= limit {
            return Ok(());
        }
        let block = oldest_scope_block(transaction, scope)?.ok_or(ManagedTextError::Storage)?;
        trim_block(transaction, block, total - limit)?;
    }
}

fn trim_global_to_bytes(transaction: &Transaction<'_>, limit: u64) -> Result<(), ManagedTextError> {
    loop {
        let total: u64 = transaction.query_row(
            "SELECT COALESCE(SUM(length(bytes)),0) FROM managed_text_progress_blocks",
            [],
            |row| row.get(0),
        )?;
        if total <= limit {
            return Ok(());
        }
        let block = oldest_global_block(transaction)?.ok_or(ManagedTextError::Storage)?;
        trim_block(transaction, block, total - limit)?;
    }
}

fn trim_global_to_blocks(
    transaction: &Transaction<'_>,
    limit: u64,
) -> Result<(), ManagedTextError> {
    loop {
        let count: u64 = transaction.query_row(
            "SELECT COUNT(*) FROM managed_text_progress_blocks",
            [],
            |row| row.get(0),
        )?;
        if count <= limit {
            return Ok(());
        }
        let block = oldest_global_block(transaction)?.ok_or(ManagedTextError::Storage)?;
        delete_full_block(
            transaction,
            &block.scope,
            block.segment,
            block.start,
            block.end,
        )?;
    }
}

struct Block {
    scope: String,
    segment: u64,
    start: u64,
    end: u64,
    bytes: Vec<u8>,
    digest: String,
}

fn oldest_scope_block(
    connection: &Connection,
    scope: &str,
) -> Result<Option<Block>, ManagedTextError> {
    connection
        .query_row(
            "SELECT scope,segment,start,end,bytes,digest
             FROM managed_text_progress_blocks WHERE scope=?1 ORDER BY start LIMIT 1",
            [scope],
            block_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn oldest_global_block(connection: &Connection) -> Result<Option<Block>, ManagedTextError> {
    connection
        .query_row(
            "SELECT scope,segment,start,end,bytes,digest
             FROM managed_text_progress_blocks ORDER BY order_seq LIMIT 1",
            [],
            block_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn block_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Block> {
    Ok(Block {
        scope: row.get(0)?,
        segment: row.get(1)?,
        start: row.get(2)?,
        end: row.get(3)?,
        bytes: row.get(4)?,
        digest: row.get(5)?,
    })
}

fn trim_block(
    transaction: &Transaction<'_>,
    block: Block,
    remove_at_least: u64,
) -> Result<(), ManagedTextError> {
    validate_block(
        block.start,
        block.end,
        &block.bytes,
        &block.digest,
        PROGRESS_BATCH_BYTES,
    )?;
    let header = load_header(transaction, &block.scope)?.ok_or(ManagedTextError::Storage)?;
    if header.segment != block.segment || header.head != block.start {
        return Err(ManagedTextError::Storage);
    }
    if remove_at_least >= block.bytes.len() as u64 {
        return delete_full_block(
            transaction,
            &block.scope,
            block.segment,
            block.start,
            block.end,
        );
    }
    let text = std::str::from_utf8(&block.bytes).map_err(|_| ManagedTextError::Storage)?;
    let mut cut = usize::try_from(remove_at_least).map_err(|_| ManagedTextError::Storage)?;
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    if cut == text.len() {
        return delete_full_block(
            transaction,
            &block.scope,
            block.segment,
            block.start,
            block.end,
        );
    }
    let new_start = block
        .start
        .checked_add(cut as u64)
        .ok_or(ManagedTextError::Storage)?;
    let bytes = &block.bytes[cut..];
    transaction.execute(
        "UPDATE managed_text_progress_blocks SET start=?4,bytes=?5,digest=?6
         WHERE scope=?1 AND segment=?2 AND start=?3",
        params![
            block.scope,
            block.segment,
            block.start,
            new_start,
            bytes,
            digest(bytes)
        ],
    )?;
    transaction.execute(
        "UPDATE managed_text_progress SET head=?2 WHERE scope=?1",
        params![block.scope, new_start],
    )?;
    Ok(())
}

fn delete_full_block(
    transaction: &Transaction<'_>,
    scope: &str,
    segment: u64,
    start: u64,
    end: u64,
) -> Result<(), ManagedTextError> {
    let header = load_header(transaction, scope)?.ok_or(ManagedTextError::Storage)?;
    if header.segment != segment || header.head != start || end > header.end {
        return Err(ManagedTextError::Storage);
    }
    let changed = transaction.execute(
        "DELETE FROM managed_text_progress_blocks
         WHERE scope=?1 AND segment=?2 AND start=?3 AND end=?4",
        params![scope, segment, start, end],
    )?;
    if changed != 1 {
        return Err(ManagedTextError::Storage);
    }
    transaction.execute(
        "UPDATE managed_text_progress SET head=?2 WHERE scope=?1",
        params![scope, end],
    )?;
    Ok(())
}

fn validate_block(
    start: u64,
    end: u64,
    bytes: &[u8],
    expected_digest: &str,
    max_bytes: usize,
) -> Result<(), ManagedTextError> {
    if bytes.is_empty()
        || bytes.len() > max_bytes
        || end.checked_sub(start) != Some(bytes.len() as u64)
        || std::str::from_utf8(bytes).is_err()
        || digest(bytes) != expected_digest
    {
        return Err(ManagedTextError::Storage);
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn recovery(generation: u64, header: Header) -> ManagedTextProgressRecovery {
    ManagedTextProgressRecovery {
        visibility_generation: generation,
        segment: header.segment,
        position: header.head,
    }
}

fn read_page(
    transaction: &Transaction<'_>,
    scope: &str,
    header: Header,
    generation: u64,
    position: u64,
    max_bytes: usize,
) -> Result<ManagedTextProgressPage, ManagedTextProgressReadError> {
    if position == header.end {
        return Ok(ManagedTextProgressPage {
            visibility_generation: generation,
            segment: header.segment,
            window_start: header.head,
            window_end: header.end,
            text: String::new(),
            next_position: position,
            has_more: false,
            truncated: header.head > 0,
        });
    }
    let mut statement = transaction
        .prepare(
            "SELECT start,end,bytes,digest FROM managed_text_progress_blocks
             WHERE scope=?1 AND segment=?2 AND end>?3 ORDER BY start LIMIT ?4",
        )
        .map_err(|_| ManagedTextProgressReadError::Storage)?;
    let mut rows = statement
        .query(params![
            scope,
            header.segment,
            position,
            PROGRESS_READ_BLOCKS + 1
        ])
        .map_err(|_| ManagedTextProgressReadError::Storage)?;
    let mut available = Vec::with_capacity(max_bytes + 4);
    let mut expected_start = None;
    let mut observed_end = position;
    let mut count = 0usize;
    let mut continuation_proven = false;
    while let Some(row) = rows
        .next()
        .map_err(|_| ManagedTextProgressReadError::Storage)?
    {
        count += 1;
        if count > PROGRESS_READ_BLOCKS {
            let start: u64 = row
                .get(0)
                .map_err(|_| ManagedTextProgressReadError::Storage)?;
            let end: u64 = row
                .get(1)
                .map_err(|_| ManagedTextProgressReadError::Storage)?;
            if Some(start) != expected_start || start >= end || end > header.end {
                return Err(ManagedTextProgressReadError::Storage);
            }
            continuation_proven = true;
            break;
        }
        let start: u64 = row
            .get(0)
            .map_err(|_| ManagedTextProgressReadError::Storage)?;
        let end: u64 = row
            .get(1)
            .map_err(|_| ManagedTextProgressReadError::Storage)?;
        let bytes: Vec<u8> = row
            .get(2)
            .map_err(|_| ManagedTextProgressReadError::Storage)?;
        let expected_digest: String = row
            .get(3)
            .map_err(|_| ManagedTextProgressReadError::Storage)?;
        validate_block(start, end, &bytes, &expected_digest, PROGRESS_BATCH_BYTES)
            .map_err(|_| ManagedTextProgressReadError::Storage)?;
        let block_text =
            std::str::from_utf8(&bytes).map_err(|_| ManagedTextProgressReadError::Storage)?;
        let from = if expected_start.is_none() {
            if !(start <= position && position < end) {
                return Err(ManagedTextProgressReadError::Storage);
            }
            let from = usize::try_from(position - start)
                .map_err(|_| ManagedTextProgressReadError::Storage)?;
            if !block_text.is_char_boundary(from) {
                return Err(ManagedTextProgressReadError::InvalidCursor);
            }
            from
        } else {
            if Some(start) != expected_start {
                return Err(ManagedTextProgressReadError::Storage);
            }
            0
        };
        available.extend_from_slice(&bytes[from..]);
        observed_end = end;
        expected_start = Some(end);
        if available.len() > max_bytes || observed_end == header.end {
            continuation_proven = true;
            break;
        }
    }
    if available.is_empty()
        || observed_end > header.end
        || (!continuation_proven && observed_end < header.end)
    {
        return Err(ManagedTextProgressReadError::Storage);
    }
    let available_text =
        std::str::from_utf8(&available).map_err(|_| ManagedTextProgressReadError::Storage)?;
    let take = prefix_boundary(available_text, max_bytes);
    if take == 0 {
        return Err(ManagedTextProgressReadError::PageTooSmall);
    }
    let next_position = position
        .checked_add(take as u64)
        .ok_or(ManagedTextProgressReadError::Storage)?;
    Ok(ManagedTextProgressPage {
        visibility_generation: generation,
        segment: header.segment,
        window_start: header.head,
        window_end: header.end,
        text: available_text[..take].to_owned(),
        next_position,
        has_more: next_position < header.end,
        truncated: header.head > 0,
    })
}
