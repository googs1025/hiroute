use super::*;
use crate::core::execution_plan::{
    AtomicityGroupId, CaPolicy, ConfigBindingPolicy, ConfigBundle, ConfigCellDescriptor,
    ConfigCellGroup, ConfigCellId, ConfigGeneration, ImmutableConfig, PoolEpoch,
};
use crate::test_support::{plain_target, tls_target};
use std::sync::atomic::AtomicUsize;

struct ActiveTask(Arc<AtomicUsize>);

impl Drop for ActiveTask {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn wait_for_active_tasks(active: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_millis(100), async {
        while active.load(Ordering::Acquire) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background task count converged");
}

fn hanging_task(active: Arc<AtomicUsize>) -> AbortOnDropTask<()> {
    AbortOnDropTask::spawn(async move {
        active.fetch_add(1, Ordering::AcqRel);
        let _active = ActiveTask(active);
        std::future::pending::<()>().await;
    })
}

#[tokio::test]
async fn cleanup_future_drop_aborts_owned_task_without_detaching() {
    let active = Arc::new(AtomicUsize::new(0));
    let task = hanging_task(Arc::clone(&active));
    wait_for_active_tasks(&active, 1).await;
    drop(task);
    wait_for_active_tasks(&active, 0).await;
}

#[tokio::test]
async fn accepted_finish_join_completes_and_clears_task_owner() {
    let active = Arc::new(AtomicUsize::new(0));
    let task_active = Arc::clone(&active);
    let mut task = AbortOnDropTask::spawn(async move {
        task_active.fetch_add(1, Ordering::AcqRel);
        let _active = ActiveTask(task_active);
        7_u8
    });
    assert_eq!((&mut task).await.expect("normal reader join"), 7);
    assert_eq!(active.load(Ordering::Acquire), 0);
    drop(task);
}

#[tokio::test]
async fn cleanup_timeout_aborts_then_boundedly_joins_owned_task() {
    let active = Arc::new(AtomicUsize::new(0));
    let mut task = hanging_task(Arc::clone(&active));
    wait_for_active_tasks(&active, 1).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(1), &mut task)
            .await
            .is_err()
    );
    task.abort();
    let joined = tokio::time::timeout(Duration::from_millis(100), &mut task)
        .await
        .expect("aborted task join is bounded");
    assert!(
        joined
            .expect_err("aborted task must not produce a value")
            .is_cancelled()
    );
    assert_eq!(active.load(Ordering::Acquire), 0);
}

#[test]
fn reuse_key_covers_epoch_and_custom_ca_fingerprint() {
    let address: SocketAddr = "127.0.0.1:8443".parse().expect("test address");
    let first = tls_target(address, "upstream.test", "upstream.test", 41);
    let mut next_epoch = first.clone();
    next_epoch.pool_epoch = PoolEpoch(2);
    next_epoch = next_epoch.with_derived_connection_fingerprint();
    assert_ne!(
        first.connection_fingerprint,
        next_epoch.connection_fingerprint
    );
    assert_ne!(
        first.connection_reuse_key(),
        next_epoch.connection_reuse_key()
    );

    let mut custom_ca = first.clone();
    custom_ca.ca = CaPolicy::Pem(Arc::from(&b"different trust root"[..]));
    custom_ca = custom_ca.with_derived_connection_fingerprint();
    assert_ne!(
        first.connection_fingerprint,
        custom_ca.connection_fingerprint
    );
    assert_ne!(
        first.connection_reuse_key(),
        custom_ca.connection_reuse_key()
    );

    let mut different_h2_flow_control = first.clone();
    different_h2_flow_control.h2_stream_window_bytes *= 2;
    different_h2_flow_control.h2_connection_window_bytes *= 2;
    different_h2_flow_control.h2_max_concurrent_streams *= 2;
    different_h2_flow_control = different_h2_flow_control.with_derived_connection_fingerprint();
    assert_ne!(
        first.connection_fingerprint,
        different_h2_flow_control.connection_fingerprint,
    );
    assert_ne!(
        first.connection_reuse_key(),
        different_h2_flow_control.connection_reuse_key(),
    );

    let peer = build_peer(&different_h2_flow_control, address, [0; 32])
        .expect("peer with explicit H2 flow-control settings");
    assert_eq!(
        peer.options.h2_stream_window_size,
        Some(different_h2_flow_control.h2_stream_window_bytes),
    );
    assert_eq!(
        peer.options.h2_connection_window_size,
        Some(different_h2_flow_control.h2_connection_window_bytes),
    );
    assert_eq!(
        peer.options.max_h2_streams,
        different_h2_flow_control.h2_max_concurrent_streams,
    );
}

#[test]
fn connection_pinned_config_generation_segregates_pingora_pool_and_peer_key() {
    let id = ConfigCellId(44);
    let bundle = |generation| {
        Arc::new(ConfigBundle::new(
            AtomicityGroupId(44),
            HashMap::from([(
                id,
                ImmutableConfig {
                    generation: ConfigGeneration(generation),
                    compatibility_hash: [4; 32],
                    bytes: Arc::from([generation as u8]),
                },
            )]),
        ))
    };
    let group = ConfigCellGroup::new(
        [ConfigCellDescriptor {
            id,
            compatibility_hash: [4; 32],
            atomicity_group: AtomicityGroupId(44),
            binding_policy: ConfigBindingPolicy::ConnectionPinned,
        }],
        bundle(1),
    )
    .expect("connection config group");
    let first = group
        .acquire_connection_snapshot()
        .expect("first connection snapshot");
    group.publish(bundle(2)).expect("rotate connection config");
    let second = group
        .acquire_connection_snapshot()
        .expect("second connection snapshot");
    let adapter = PingoraConnectorAdapter::new();
    let first_transport = adapter.create_transport(&first);
    let second_transport = adapter.create_transport(&second);
    assert_ne!(
        first_transport.connection_config_fingerprint,
        second_transport.connection_config_fingerprint
    );

    let target = plain_target("127.0.0.1:8080".parse().expect("target"), 44);
    let first_peer = build_peer(
        &target,
        target.addresses[0],
        first_transport.connection_config_fingerprint,
    )
    .expect("first peer");
    let second_peer = build_peer(
        &target,
        target.addresses[0],
        second_transport.connection_config_fingerprint,
    )
    .expect("second peer");
    assert_ne!(first_peer.group_key, second_peer.group_key);
}

#[test]
fn connector_registry_rotates_forward_and_never_rolls_back() {
    let registry = PingoraConnectorRegistry::default();
    let first_target = plain_target("127.0.0.1:8080".parse().expect("test address"), 42);
    let first = registry
        .connector_for(&first_target, [0; 32])
        .expect("first connector");
    let same = registry
        .connector_for(&first_target, [0; 32])
        .expect("same connector");
    assert!(Arc::ptr_eq(&first, &same));

    let mut next_target = first_target.clone();
    next_target.pool_epoch = PoolEpoch(2);
    next_target = next_target.with_derived_connection_fingerprint();
    let next = registry
        .connector_for(&next_target, [0; 32])
        .expect("next connector");
    assert!(!Arc::ptr_eq(&first, &next));

    let stale = registry
        .connector_for(&first_target, [0; 32])
        .expect("isolated stale connector");
    assert!(!Arc::ptr_eq(&first, &stale));
    assert!(!Arc::ptr_eq(&next, &stale));
    let still_next = registry
        .connector_for(&next_target, [0; 32])
        .expect("current connector");
    assert!(Arc::ptr_eq(&next, &still_next));
}

#[tokio::test]
async fn unresolved_authority_never_reaches_the_native_connector() {
    let provider = std::net::TcpListener::bind("127.0.0.1:0").expect("provider sentinel");
    provider
        .set_nonblocking(true)
        .expect("nonblocking sentinel");
    let address = provider.local_addr().expect("provider address");
    let mut target = plain_target(address, 77);
    target.authority =
        TransportTarget::mark_resolution_required(format!("localhost:{}", address.port()));
    target.addresses = Arc::from([]);
    target = target.with_derived_connection_fingerprint();
    target.validate().expect("unresolved target shape");

    let error = match PingoraConnectorAdapter::new()
        .connect(&target, address)
        .await
    {
        Ok(_) => panic!("unresolved authority must be non-executable"),
        Err(error) => error,
    };
    assert!(matches!(error, AttemptError::InvalidTarget(_)));
    assert_eq!(
        provider.accept().expect_err("no connect syscall").kind(),
        std::io::ErrorKind::WouldBlock
    );
}
