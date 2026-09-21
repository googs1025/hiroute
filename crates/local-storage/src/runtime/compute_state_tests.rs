use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use crate::test_tempdir as tempdir;
use hiroute_domain::{
    ComputeRuntimeHealthV1, ComputeRuntimeStateStoreV1, PortErrorCode, RuntimeAvailabilityStateV1,
    RuntimeClockSampleV1, RuntimeProbeAcquireOutcomeV1, RuntimeProbeLeaseRequestV1,
    RuntimeStateIdentityV1, RuntimeStateV1,
};
use rusqlite::{Connection, params};

use super::RuntimeStore;

fn clock(value: i64) -> RuntimeClockSampleV1 {
    RuntimeClockSampleV1::from_unix_millis(value).unwrap()
}

fn acquired(outcome: RuntimeProbeAcquireOutcomeV1) -> RuntimeStateV1 {
    match outcome {
        RuntimeProbeAcquireOutcomeV1::Acquired(state) => state,
        other => panic!("expected acquired probe lease, got {other:?}"),
    }
}

fn credential_identity(
    binding_id: &str,
    credential_generation: u64,
    key_id: &str,
) -> RuntimeStateIdentityV1 {
    RuntimeStateIdentityV1::credential(
        binding_id,
        "credential/ref-a",
        key_id,
        credential_generation,
    )
    .unwrap()
}

fn binding_identity(binding_id: &str) -> RuntimeStateIdentityV1 {
    RuntimeStateIdentityV1::binding(binding_id).unwrap()
}

fn open(database: &Path, backups: &Path) -> RuntimeStore {
    RuntimeStore::open(&crate::test_storage_authority(), database, backups).unwrap()
}

fn seed_credential_cooldown(
    store: &RuntimeStore,
    identity: &RuntimeStateIdentityV1,
    updated_at: i64,
    reset_at: i64,
) -> RuntimeStateV1 {
    let state =
        RuntimeStateV1::cooling_down(identity.clone(), 1, reset_at, None, clock(updated_at))
            .unwrap();
    store.compare_and_set_runtime_state(0, &state).unwrap();
    state
}

#[test]
fn exact_runtime_state_isolated_cas_bound_and_durable() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("data/runtime.db");
    let backups = directory.path().join("data/backups");
    let exact = credential_identity("binding/primary", 1, "opaque key @ slot?#/🔥");
    let rotated = credential_identity("binding/primary", 2, "opaque key @ slot?#/🔥");
    let other_key = credential_identity("binding/primary", 1, "not-a-digest:key-b");
    let other_binding = credential_identity("binding/secondary", 1, "opaque key @ slot?#/🔥");
    let binding = binding_identity("binding/primary");
    let stored;
    {
        let store = open(&database, &backups);
        stored = seed_credential_cooldown(&store, &exact, 1_000, 2_000);
        let stale =
            RuntimeStateV1::cooling_down(exact.clone(), 1, 2_000, None, clock(1_000)).unwrap();
        let error = store.compare_and_set_runtime_state(0, &stale).unwrap_err();
        assert_eq!(error.code, PortErrorCode::Conflict);
        assert!(store.runtime_state(&rotated).unwrap().is_none());
        assert!(store.runtime_state(&other_key).unwrap().is_none());
        assert!(store.runtime_state(&other_binding).unwrap().is_none());
        assert!(store.runtime_state(&binding).unwrap().is_none());

        let binding_state =
            RuntimeStateV1::cooling_down(binding.clone(), 1, 2_100, None, clock(1_100)).unwrap();
        store
            .compare_and_set_runtime_state(0, &binding_state)
            .unwrap();
        assert_eq!(
            store
                .runtime_state(&binding)
                .unwrap()
                .unwrap()
                .cooldown_until_unix_millis(),
            Some(2_100)
        );
    }
    let reopened = open(&database, &backups);
    assert_eq!(reopened.runtime_state(&exact).unwrap(), Some(stored));
    assert!(reopened.runtime_state(&rotated).unwrap().is_none());
}

#[test]
fn binding_transient_backoff_step_survives_store_reopen() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("data/runtime.db");
    let backups = directory.path().join("data/backups");
    let identity = binding_identity("binding/backoff");
    let state = RuntimeStateV1::from_exact_parts_with_backoff(
        identity.clone(),
        1,
        ComputeRuntimeHealthV1::CoolingDown {
            until_unix_millis: 31_000,
        },
        None,
        4,
        clock(15_000),
    )
    .unwrap();
    {
        let store = open(&database, &backups);
        store.compare_and_set_runtime_state(0, &state).unwrap();
    }

    let reopened = open(&database, &backups);
    let persisted = reopened.runtime_state(&identity).unwrap().unwrap();
    assert_eq!(persisted, state);
    assert_eq!(persisted.transient_backoff_step(), 4);
}

#[test]
fn two_contenders_share_one_restart_safe_probe_lease_and_successor_fence() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("data/runtime.db");
    let backups = directory.path().join("data/backups");
    let identity = credential_identity("binding/primary", 1, "opaque key @ slot-A");
    {
        let store = open(&database, &backups);
        seed_credential_cooldown(&store, &identity, 1_000, 2_000);
    }

    let left_store = open(&database, &backups);
    let right_store = open(&database, &backups);
    let barrier = Arc::new(Barrier::new(2));
    let left_request = RuntimeProbeLeaseRequestV1::new(clock(2_000), 1_000).unwrap();
    let right_request = RuntimeProbeLeaseRequestV1::new(clock(2_000), 1_000).unwrap();
    let (left, right) = thread::scope(|scope| {
        let left_barrier = Arc::clone(&barrier);
        let left_identity = identity.clone();
        let left = scope.spawn(move || {
            left_barrier.wait();
            left_store.acquire_runtime_probe(&left_identity, 1, &left_request)
        });
        let right_barrier = Arc::clone(&barrier);
        let right_identity = identity.clone();
        let right = scope.spawn(move || {
            right_barrier.wait();
            right_store.acquire_runtime_probe(&right_identity, 1, &right_request)
        });
        (left.join().unwrap(), right.join().unwrap())
    });
    let winner = match (left, right) {
        (
            Ok(RuntimeProbeAcquireOutcomeV1::Acquired(winner)),
            Ok(RuntimeProbeAcquireOutcomeV1::Conflict),
        )
        | (
            Ok(RuntimeProbeAcquireOutcomeV1::Conflict),
            Ok(RuntimeProbeAcquireOutcomeV1::Acquired(winner)),
        ) => winner,
        (left, right) => panic!("expected one lease winner, got left={left:?}, right={right:?}"),
    };
    assert_eq!(winner.generation(), 2);
    let first_lease = winner.probe_lease().unwrap().clone();

    let store = open(&database, &backups);
    assert_eq!(
        store.runtime_state(&identity).unwrap(),
        Some(winner.clone())
    );
    let active_request = RuntimeProbeLeaseRequestV1::new(clock(2_500), 1_000).unwrap();
    let busy = store
        .acquire_runtime_probe(&identity, 2, &active_request)
        .unwrap();
    assert_eq!(busy, RuntimeProbeAcquireOutcomeV1::Busy);

    drop(store);
    let restarted = open(&database, &backups);
    let replacement_request = RuntimeProbeLeaseRequestV1::new(clock(3_000), 1_000).unwrap();
    let replacement = acquired(
        restarted
            .acquire_runtime_probe(&identity, 2, &replacement_request)
            .unwrap(),
    );
    assert_eq!(replacement.generation(), 3);
    assert_eq!(
        replacement.probe_lease().unwrap().expires_at_unix_millis(),
        4_000
    );

    let stale_completion = RuntimeStateV1::ready(identity.clone(), 3, clock(3_500)).unwrap();
    let error = restarted
        .complete_runtime_probe(&first_lease, &stale_completion)
        .unwrap_err();
    assert_eq!(error.code, PortErrorCode::Conflict);

    let replacement_lease = replacement.probe_lease().unwrap().clone();
    let failed_probe =
        RuntimeStateV1::cooling_down(identity.clone(), 4, 303_500, None, clock(3_500)).unwrap();
    restarted
        .complete_runtime_probe(&replacement_lease, &failed_probe)
        .unwrap();
    assert_eq!(failed_probe.generation(), 4);
    assert_eq!(failed_probe.cooldown_until_unix_millis(), Some(303_500));
    let third_lease = acquired(
        restarted
            .acquire_runtime_probe(
                &identity,
                4,
                &RuntimeProbeLeaseRequestV1::new(clock(303_500), 1_000).unwrap(),
            )
            .unwrap(),
    );
    let third_failure =
        RuntimeStateV1::cooling_down(identity.clone(), 6, 2_104_000, None, clock(304_000)).unwrap();
    restarted
        .complete_runtime_probe(third_lease.probe_lease().unwrap(), &third_failure)
        .unwrap();
    let fourth_lease = acquired(
        restarted
            .acquire_runtime_probe(
                &identity,
                6,
                &RuntimeProbeLeaseRequestV1::new(clock(2_104_000), 1_000).unwrap(),
            )
            .unwrap(),
    );
    let fourth_failure =
        RuntimeStateV1::cooling_down(identity.clone(), 8, 3_904_500, None, clock(2_104_500))
            .unwrap();
    restarted
        .complete_runtime_probe(fourth_lease.probe_lease().unwrap(), &fourth_failure)
        .unwrap();
    assert!(matches!(
        fourth_failure.health(),
        ComputeRuntimeHealthV1::CoolingDown { .. }
    ));

    let fifth_lease = acquired(
        restarted
            .acquire_runtime_probe(
                &identity,
                8,
                &RuntimeProbeLeaseRequestV1::new(clock(3_904_500), 1_000).unwrap(),
            )
            .unwrap(),
    );
    let fifth_failure =
        RuntimeStateV1::cooling_down(identity.clone(), 10, 5_705_000, None, clock(3_905_000))
            .unwrap();
    restarted
        .complete_runtime_probe(fifth_lease.probe_lease().unwrap(), &fifth_failure)
        .unwrap();
    assert!(matches!(
        fifth_failure.health(),
        ComputeRuntimeHealthV1::CoolingDown { .. }
    ));

    let disable_lease = acquired(
        restarted
            .acquire_runtime_probe(
                &identity,
                10,
                &RuntimeProbeLeaseRequestV1::new(clock(5_705_000), 1_000).unwrap(),
            )
            .unwrap(),
    );
    let disabled = RuntimeStateV1::disabled(identity.clone(), 12, clock(5_705_500)).unwrap();
    restarted
        .complete_runtime_probe(disable_lease.probe_lease().unwrap(), &disabled)
        .unwrap();
    assert_eq!(disabled.health(), ComputeRuntimeHealthV1::Disabled);
    drop(restarted);

    let reopened = open(&database, &backups);
    assert_eq!(reopened.runtime_state(&identity).unwrap(), Some(disabled));
}

#[test]
fn expired_probe_completion_succeeds_after_restart_when_generation_is_unchanged() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("data/runtime.db");
    let backups = directory.path().join("data/backups");
    let identity = credential_identity("binding/primary", 1, "slow-probe-key");
    let lease = {
        let store = open(&database, &backups);
        seed_credential_cooldown(&store, &identity, 1_000, 2_000);
        acquired(
            store
                .acquire_runtime_probe(
                    &identity,
                    1,
                    &RuntimeProbeLeaseRequestV1::new(clock(2_000), 1_000).unwrap(),
                )
                .unwrap(),
        )
        .probe_lease()
        .unwrap()
        .clone()
    };

    let restarted = open(&database, &backups);
    let completed = RuntimeStateV1::ready(identity.clone(), 3, clock(3_500)).unwrap();
    restarted
        .complete_runtime_probe(&lease, &completed)
        .unwrap();
    assert_eq!(restarted.runtime_state(&identity).unwrap(), Some(completed));
}

#[test]
fn expiry_equality_completion_and_reacquire_serialize_to_one_generation_path() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("data/runtime.db");
    let backups = directory.path().join("data/backups");
    let identity = credential_identity("binding/primary", 1, "expiry-race-key");
    let lease = {
        let store = open(&database, &backups);
        seed_credential_cooldown(&store, &identity, 1_000, 2_000);
        acquired(
            store
                .acquire_runtime_probe(
                    &identity,
                    1,
                    &RuntimeProbeLeaseRequestV1::new(clock(2_000), 1_000).unwrap(),
                )
                .unwrap(),
        )
        .probe_lease()
        .unwrap()
        .clone()
    };

    let completion_store = open(&database, &backups);
    let reacquire_store = open(&database, &backups);
    let barrier = Arc::new(Barrier::new(2));
    let completed = RuntimeStateV1::ready(identity.clone(), 3, clock(3_000)).unwrap();
    let completion_result_state = completed.clone();
    let completion_lease = lease.clone();
    let completion_barrier = Arc::clone(&barrier);
    let reacquire_identity = identity.clone();
    let reacquire_barrier = Arc::clone(&barrier);
    let (completion, reacquire) = thread::scope(|scope| {
        let completion = scope.spawn(move || {
            completion_barrier.wait();
            completion_store.complete_runtime_probe(&completion_lease, &completion_result_state)
        });
        let reacquire = scope.spawn(move || {
            reacquire_barrier.wait();
            reacquire_store.acquire_runtime_probe(
                &reacquire_identity,
                2,
                &RuntimeProbeLeaseRequestV1::new(clock(3_000), 1_000).unwrap(),
            )
        });
        (completion.join().unwrap(), reacquire.join().unwrap())
    });

    let winner = match (completion, reacquire) {
        (Ok(()), Ok(RuntimeProbeAcquireOutcomeV1::Conflict)) => completed,
        (Err(error), Ok(RuntimeProbeAcquireOutcomeV1::Acquired(replacement))) => {
            assert_eq!(error.code, PortErrorCode::Conflict);
            replacement
        }
        (completion, reacquire) => {
            panic!(
                "expected one serialized generation winner, got completion={completion:?}, reacquire={reacquire:?}"
            )
        }
    };
    assert_eq!(winner.generation(), 3);
    let verifier = open(&database, &backups);
    assert_eq!(verifier.runtime_state(&identity).unwrap(), Some(winner));
}

#[test]
fn failed_lease_write_rolls_back_and_never_grants_an_in_memory_owner() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("data/runtime.db");
    let backups = directory.path().join("data/backups");
    let identity = credential_identity("binding/primary", 1, "key-a");
    let store = open(&database, &backups);
    let cooling = seed_credential_cooldown(&store, &identity, 1_000, 2_000);
    store.with_connection(|connection| {
        connection
            .execute_batch(
                "CREATE TRIGGER compute_runtime_state_v1_test_write_failure
                 BEFORE UPDATE ON compute_runtime_state_v1
                 BEGIN
                    SELECT RAISE(ABORT, 'injected runtime-state write failure');
                 END;",
            )
            .unwrap();
    });
    let request = RuntimeProbeLeaseRequestV1::new(clock(2_000), 1_000).unwrap();
    let error = store
        .acquire_runtime_probe(&identity, 1, &request)
        .unwrap_err();
    assert_eq!(error.code, PortErrorCode::Unavailable);
    assert_eq!(store.runtime_state(&identity).unwrap(), Some(cooling));
    assert!(
        store
            .runtime_state(&identity)
            .unwrap()
            .unwrap()
            .probe_lease()
            .is_none()
    );
    store.with_connection(|connection| {
        connection
            .execute_batch("DROP TRIGGER compute_runtime_state_v1_test_write_failure;")
            .unwrap();
    });
    let acquired = acquired(store.acquire_runtime_probe(&identity, 1, &request).unwrap());
    let lease = acquired.probe_lease().unwrap().clone();
    let canceled = acquired.cancel_probe(&lease, clock(2_500)).unwrap();
    store.complete_runtime_probe(&lease, &canceled).unwrap();
    assert!(canceled.probe_lease().is_none());
    assert_eq!(store.runtime_state(&identity).unwrap(), Some(canceled));
}

#[test]
fn unknown_runtime_state_schema_fails_closed_as_corrupt() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("data/runtime.db");
    let backups = directory.path().join("data/backups");
    let identity = credential_identity("binding/primary", 1, "key-a");
    let store = open(&database, &backups);
    let state = seed_credential_cooldown(&store, &identity, 1_000, 2_000);
    let mut encoded = serde_json::to_value(state).unwrap();
    encoded["schema"] = serde_json::json!("hiroute.compute-runtime-state/v2");
    store.with_connection(|connection| {
        connection
            .execute(
                "UPDATE compute_runtime_state_v1 SET state_json=?2 WHERE identity_key=?1",
                params![identity.canonical_key().unwrap(), encoded.to_string()],
            )
            .unwrap();
    });
    let error = store.runtime_state(&identity).unwrap_err();
    assert_eq!(error.code, PortErrorCode::Corrupt);
}

#[test]
fn version_six_migration_quarantines_collapsed_rows_without_fabricating_identity() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("data/runtime.db");
    let backups = directory.path().join("data/backups");
    create_legacy_v5_database(&database);
    let identity = credential_identity("binding/primary", 1, "key-a");
    {
        let store = open(&database, &backups);
        assert!(store.runtime_state(&identity).unwrap().is_none());
        store.with_connection(|connection| {
            let version: u32 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, crate::migrations::LATEST_SCHEMA_VERSION);
            let legacy_rows: u32 = connection
                .query_row(
                    "SELECT count(*) FROM legacy_compute_runtime_state_v4",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(legacy_rows, 2);
            let old_tables: u32 = connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master
                     WHERE type='table' AND name IN (
                        'credential_runtime_state','binding_runtime_state'
                     )",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(old_tables, 0);
            assert!(
                connection
                    .execute(
                        "DELETE FROM legacy_compute_runtime_state_v4
                         WHERE legacy_kind='credential'",
                        [],
                    )
                    .is_err()
            );
        });
    }
    let reopened = open(&database, &backups);
    assert!(reopened.runtime_state(&identity).unwrap().is_none());
}

fn create_legacy_v5_database(path: &Path) {
    let parent = path.parent().unwrap();
    crate::test_create_dir_all(parent).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE schema_migrations(
                version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL,
                binary_version TEXT NOT NULL
             );
             CREATE TABLE storage_meta(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1), store_uuid TEXT NOT NULL,
                key_id TEXT, created_at INTEGER NOT NULL
             );
             INSERT INTO storage_meta(singleton,store_uuid,key_id,created_at)
                VALUES(1,'legacy-runtime-store',NULL,1);
             CREATE TABLE runtime_state(
                state_key TEXT PRIMARY KEY, value_json TEXT NOT NULL, value_digest TEXT NOT NULL,
                generation INTEGER NOT NULL, owner_operation_id TEXT, updated_at INTEGER NOT NULL
             );
             CREATE TABLE credential_runtime_state(
                credential_id TEXT PRIMARY KEY, state_json TEXT NOT NULL,
                generation INTEGER NOT NULL, updated_at INTEGER NOT NULL
             );
             CREATE TABLE binding_runtime_state(
                binding_id TEXT PRIMARY KEY, state_json TEXT NOT NULL,
                generation INTEGER NOT NULL, updated_at INTEGER NOT NULL
             );
             INSERT INTO schema_migrations(version,applied_at,binary_version)
                VALUES(5,1,'legacy');
             PRAGMA user_version=5;",
        )
        .unwrap();
    let credential = RuntimeAvailabilityStateV1::ready("credential/key-a")
        .unwrap()
        .credential_quota(0, 10, Some(20))
        .unwrap();
    let binding = RuntimeAvailabilityStateV1::ready("binding/primary")
        .unwrap()
        .binding_overload(0, 10, Some(20))
        .unwrap();
    connection
        .execute(
            "INSERT INTO credential_runtime_state(
                credential_id,state_json,generation,updated_at
             ) VALUES(?1,?2,?3,10)",
            params![
                credential.subject_id,
                serde_json::to_string(&credential).unwrap(),
                credential.generation
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO binding_runtime_state(binding_id,state_json,generation,updated_at)
             VALUES(?1,?2,?3,10)",
            params![
                binding.subject_id,
                serde_json::to_string(&binding).unwrap(),
                binding.generation
            ],
        )
        .unwrap();
    drop(connection);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
