use super::progress::ProgressLimits;
use super::*;
use crate::{DigestAuthority, LocalObservationStore};

const NOW: i64 = 10_000;

fn store(root: &std::path::Path) -> LocalObservationStore {
    LocalObservationStore::open(root, DigestAuthority::new([19; 32])).unwrap()
}

fn target(run_id: &str, created_at_ms: i64) -> ManagedTextProgressTarget {
    ManagedTextProgressTarget {
        scope: ManagedTextScope {
            workspace_id: WorkspaceId::default(),
            task_id: format!("task-{run_id}"),
            run_id: run_id.to_owned(),
        },
        created_at_ms,
    }
}

fn read_at(
    store: &LocalObservationStore,
    target: &ManagedTextProgressTarget,
    cursor: Option<ManagedTextProgressCursor>,
    max_bytes: usize,
    now_ms: i64,
) -> Result<ManagedTextProgressRead, ManagedTextProgressReadError> {
    store.progress_read_at(target, cursor, max_bytes, now_ms, now_ms)
}

fn available(read: ManagedTextProgressRead) -> ManagedTextProgressPage {
    match read {
        ManagedTextProgressRead::Available(page) => page,
        other => panic!("expected available progress, got {other:?}"),
    }
}

#[test]
fn tail_batches_merge_and_idle_checks_do_not_create_or_touch_progress() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let target = target("tail", 100);
    assert_eq!(
        store
            .managed_text_progress_write_batch(&target, "", false, NOW)
            .unwrap(),
        ManagedTextProgressWriteOutcome::Noop
    );
    let header_count: u64 = store
        .connection
        .lock()
        .query_row("SELECT COUNT(*) FROM managed_text_progress", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(header_count, 0);

    for _ in 0..720 {
        store
            .managed_text_progress_write_batch(&target, "0123456789abcdef", false, NOW)
            .unwrap();
    }
    let connection = store.connection.lock();
    let (blocks, bytes, next_order): (u64, u64, u64) = connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM managed_text_progress_blocks),
                (SELECT SUM(length(bytes)) FROM managed_text_progress_blocks),
                (SELECT next_order_seq FROM managed_text_progress_meta WHERE singleton=1)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((blocks, bytes, next_order), (1, 720 * 16, 1));
    drop(connection);
    let page = available(read_at(&store, &target, None, 32, NOW).unwrap());
    assert_eq!(page.window_start, 0);
    assert_eq!(page.window_end, 720 * 16);
    assert_eq!(page.text, "0123456789abcdef0123456789abcdef");
    assert!(page.has_more);
}

#[test]
fn reset_switches_segment_once_and_old_cursor_reports_exact_gap_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let target = target("gap", 100);
    store
        .managed_text_progress_write_batch(&target, "old", false, NOW)
        .unwrap();
    let old_cursor = ManagedTextProgressCursor {
        visibility_generation: 0,
        segment: 0,
        position: 3,
    };
    store
        .managed_text_progress_write_batch(&target, "new", true, NOW)
        .unwrap();
    assert_eq!(
        read_at(&store, &target, Some(old_cursor), 32, NOW),
        Err(ManagedTextProgressReadError::Gap(
            ManagedTextProgressRecovery {
                visibility_generation: 0,
                segment: 1,
                position: 0,
            }
        ))
    );
    let page = available(read_at(&store, &target, None, 32, NOW).unwrap());
    assert_eq!((page.segment, page.text.as_str()), (1, "new"));

    store
        .managed_text_progress_write_batch(&target, "", true, NOW)
        .unwrap();
    assert_eq!(
        store
            .managed_text_progress_write_batch(&target, "", false, NOW)
            .unwrap(),
        ManagedTextProgressWriteOutcome::Noop
    );
    let page = available(read_at(&store, &target, None, 32, NOW).unwrap());
    assert_eq!(
        (page.segment, page.window_start, page.window_end),
        (2, 0, 0)
    );
}

#[test]
fn per_run_and_global_eviction_preserve_utf8_suffix_and_cursor_truth() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let first = target("first", 100);
    let second = target("second", 100);
    let limits = ProgressLimits {
        run_bytes: 8,
        global_bytes: 12,
        blocks: 8,
        block_bytes: 8,
    };
    store
        .progress_write_with_limits(&first, "123456好x", false, NOW, limits)
        .unwrap();
    let page = available(read_at(&store, &first, None, 32, NOW).unwrap());
    assert_eq!((page.window_start, page.window_end), (2, 10));
    assert_eq!(page.text, "3456好x");
    assert!(page.truncated);
    assert_eq!(
        read_at(
            &store,
            &first,
            Some(ManagedTextProgressCursor {
                visibility_generation: 0,
                segment: 0,
                position: 0,
            }),
            32,
            NOW,
        ),
        Err(ManagedTextProgressReadError::Evicted(
            ManagedTextProgressRecovery {
                visibility_generation: 0,
                segment: 0,
                position: 2,
            }
        ))
    );

    store
        .progress_write_with_limits(&second, "abcdefgh", false, NOW, limits)
        .unwrap();
    let first_page = available(read_at(&store, &first, None, 32, NOW).unwrap());
    assert_eq!(first_page.text, "好x");
    assert_eq!(first_page.window_start, 6);
    let second_page = available(read_at(&store, &second, None, 32, NOW).unwrap());
    assert_eq!(second_page.text, "abcdefgh");
    let total: u64 = store
        .connection
        .lock()
        .query_row(
            "SELECT SUM(length(bytes)) FROM managed_text_progress_blocks",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(total, 12);
}

#[test]
fn utf8_cursor_and_page_boundaries_are_exact() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let target = target("utf8", 100);
    store
        .managed_text_progress_write_batch(&target, "é🙂x", false, NOW)
        .unwrap();
    assert_eq!(
        read_at(&store, &target, None, 1, NOW),
        Err(ManagedTextProgressReadError::PageTooSmall)
    );
    let first = available(read_at(&store, &target, None, 2, NOW).unwrap());
    assert_eq!((first.text.as_str(), first.next_position), ("é", 2));
    assert!(first.has_more);
    let second = available(
        read_at(
            &store,
            &target,
            Some(ManagedTextProgressCursor {
                visibility_generation: 0,
                segment: 0,
                position: 2,
            }),
            4,
            NOW,
        )
        .unwrap(),
    );
    assert_eq!((second.text.as_str(), second.next_position), ("🙂", 6));
    assert_eq!(
        read_at(
            &store,
            &target,
            Some(ManagedTextProgressCursor {
                visibility_generation: 0,
                segment: 0,
                position: 3,
            }),
            4,
            NOW,
        ),
        Err(ManagedTextProgressReadError::InvalidCursor)
    );
}

#[test]
fn missing_deleted_expired_and_fresh_privacy_recheck_are_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let missing = target("missing", 100);
    assert_eq!(
        read_at(&store, &missing, None, 32, NOW).unwrap(),
        ManagedTextProgressRead::Missing {
            visibility_generation: 0
        }
    );

    let deleted = target("deleted", 100);
    store
        .managed_text_progress_write_batch(&deleted, "private", false, NOW)
        .unwrap();
    let preview = store
        .managed_text_delete_preview(&deleted.scope, 100, NOW)
        .unwrap();
    store.managed_text_delete_apply(&preview).unwrap();
    assert_eq!(
        read_at(&store, &deleted, None, 32, NOW).unwrap(),
        ManagedTextProgressRead::Deleted
    );
    assert_eq!(
        read_at(
            &store,
            &deleted,
            Some(ManagedTextProgressCursor {
                visibility_generation: 0,
                segment: 0,
                position: 0,
            }),
            32,
            NOW,
        ),
        Err(ManagedTextProgressReadError::Stale)
    );
    assert_eq!(
        store
            .managed_text_progress_write_batch(&deleted, "late", false, NOW)
            .unwrap(),
        ManagedTextProgressWriteOutcome::Hidden
    );

    let expiring = target("expiry", 100);
    store
        .managed_text_progress_write_batch(&expiring, "old", false, NOW)
        .unwrap();
    assert_eq!(
        store
            .progress_read_at(
                &expiring,
                None,
                32,
                100 + RETENTION_MS - 1,
                100 + RETENTION_MS,
            )
            .unwrap(),
        ManagedTextProgressRead::Expired
    );
}

#[test]
fn restart_preserves_atomic_tail_and_corruption_fails_closed() {
    let directory = tempfile::tempdir().unwrap();
    let target = target("restart", 100);
    {
        let store = store(directory.path());
        store
            .managed_text_progress_write_batch(&target, "first", false, NOW)
            .unwrap();
        store
            .managed_text_progress_write_batch(&target, "第二", false, NOW)
            .unwrap();
    }
    let restarted = store(directory.path());
    let page = available(read_at(&restarted, &target, None, 32, NOW).unwrap());
    assert_eq!(page.text, "first第二");
    restarted
        .connection
        .lock()
        .execute(
            "UPDATE managed_text_progress_blocks SET digest='bad' WHERE scope=?1",
            [target.scope.key().unwrap()],
        )
        .unwrap();
    assert_eq!(
        read_at(&restarted, &target, None, 32, NOW),
        Err(ManagedTextProgressReadError::Storage)
    );
}

#[test]
fn a_missing_tail_block_is_storage_corruption_not_a_short_success_page() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let target = target("missing-tail", 100);
    let limits = ProgressLimits {
        run_bytes: 64,
        global_bytes: 64,
        blocks: 16,
        block_bytes: 4,
    };
    store
        .progress_write_with_limits(&target, "abcdefgh", false, NOW, limits)
        .unwrap();
    store
        .connection
        .lock()
        .execute(
            "DELETE FROM managed_text_progress_blocks WHERE scope=?1 AND start=4",
            [target.scope.key().unwrap()],
        )
        .unwrap();
    assert_eq!(
        read_at(&store, &target, None, 4, NOW),
        Err(ManagedTextProgressReadError::Storage)
    );
}

#[test]
fn progress_gc_is_bounded_and_retains_hidden_header() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let first = target("gc-first", 10);
    let second = target("gc-second", 20);
    store
        .managed_text_progress_write_batch(&first, "first", false, NOW)
        .unwrap();
    store
        .managed_text_progress_write_batch(&second, "second", false, NOW)
        .unwrap();
    let now = RETENTION_MS + 10;
    assert_eq!(store.managed_text_progress_gc(now, 1).unwrap(), 1);
    assert_eq!(store.managed_text_progress_gc(now, 1).unwrap(), 0);
    let (headers, blocks): (u64, u64) = store
        .connection
        .lock()
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM managed_text_progress),
                (SELECT COUNT(*) FROM managed_text_progress_blocks)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((headers, blocks), (2, 1));
    assert_eq!(
        read_at(&store, &first, None, 32, now).unwrap(),
        ManagedTextProgressRead::Expired
    );
}
