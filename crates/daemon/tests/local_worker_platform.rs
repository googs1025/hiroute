#![cfg(unix)]
use hiroute_daemon::delegation::profile::{RunMaterialFile, RunMaterials};
use hiroute_daemon::delegation::{local_worker::*, platform::*};
use hiroute_domain::delegation::DelegationErrorV1;
use std::{
    io::{BufRead, Write},
    path::PathBuf,
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use zeroize::Zeroizing;
mod local_worker_support;
use local_worker_support::{private_tempdir, request, start};

// This is an OS probe, not an ACP or Harness implementation. The launcher under test
// is the real public production module. Child modes run only when explicitly selected.
#[test]
fn local_worker_probe() {
    let Ok(mode) = std::env::var("HIROUTE_PROBE_MODE") else {
        return;
    };
    let cwd = PathBuf::from(std::env::var("HIROUTE_PROBE_CWD").unwrap());
    assert_eq!(
        std::env::current_dir().unwrap(),
        std::fs::canonicalize(&cwd).unwrap()
    );
    assert!(std::env::var("HOME").is_err());
    if mode == "child" {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(cwd.join("heartbeat"))
            .unwrap();
        while !cwd.join("stop").exists() {
            writeln!(file, "tick").unwrap();
            file.flush().unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        return;
    }
    let mut child = if mode == "tree" || mode == "root-first" || mode == "cooperate" {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "local_worker_probe", "--nocapture"])
            .env_clear()
            .env("HIROUTE_PROBE_MODE", "child")
            .env("HIROUTE_PROBE_CWD", &cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write(cwd.join("child.pid"), child.id().to_string()).unwrap();
        for _ in 0..500 {
            if cwd.join("heartbeat").exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(cwd.join("heartbeat").exists());
        Some(child)
    } else {
        None
    };
    if mode == "stderr" {
        std::io::stderr()
            .write_all(&vec![b'x'; 1024 * 1024])
            .unwrap();
    }
    if mode == "literal" {
        assert_eq!(std::env::var("LITERAL").unwrap(), "空 格; $(not-a-command)");
        assert_eq!(std::env::args().next_back().unwrap(), "literal 空 格;$(x)");
    }
    println!("PROBE_READY");
    std::io::stdout().flush().unwrap();
    if mode == "exit" || mode == "root-first" {
        return;
    }
    let mut input = String::new();
    std::io::stdin().lock().read_line(&mut input).unwrap();
    if let Some(ref mut child) = child {
        std::fs::write(cwd.join("stop"), "stop").unwrap();
        child.wait().unwrap();
    }
}

#[tokio::test]
async fn local_worker_spawn_stdio_without_handshake_and_observer_disconnect() {
    let temp = private_tempdir().unwrap();
    let platform = LocalWorkerPlatform::default();
    let (ready, reader) = start(
        &platform,
        &temp.path().join("run"),
        temp.path(),
        "stdio",
        "stderr",
    )
    .await;
    drop(reader);
    assert_eq!(
        platform.observe(&ready.identity).await.unwrap(),
        WorkerObservation::Running
    );
    let stopped = platform.terminate(&ready.identity, 5000).await.unwrap();
    assert!(
        stopped.scope_stopped && !stopped.residual_unknown,
        "{stopped:?}"
    );
    assert!(!temp.path().join("run").exists());
    assert_eq!(
        platform.material_cleanup(&ready.identity),
        Some(MaterialCleanup::Complete)
    );
    assert_eq!(
        platform.terminate(&ready.identity, 100).await.unwrap(),
        stopped
    );
    platform.release(&ready.identity).unwrap();
    assert_eq!(
        platform.observe(&ready.identity).await.unwrap(),
        WorkerObservation::Unknown
    );
}

#[tokio::test]
async fn local_worker_literal_arguments_and_explicit_environment() {
    use tokio::io::AsyncReadExt;
    let temp = private_tempdir().unwrap();
    let platform = LocalWorkerPlatform::default();
    let mut req = request(
        &temp.path().join("run 空 格"),
        temp.path(),
        "literal",
        "literal",
    );
    req.profile.args.push("literal 空 格;$(x)".into());
    // Rust's test harness would parse this as a filter; put it after -- for the probe.
    req.profile.env.insert(
        "LITERAL".into(),
        Zeroizing::new("空 格; $(not-a-command)".into()),
    );
    let mut ready = platform.launch(req).await.unwrap();
    ready.stdin.write_all(b"stop\n").await.unwrap();
    let mut output = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        ready.stdout.read_to_string(&mut output),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.contains("PROBE_READY"), "{output}");
    assert!(
        platform
            .terminate(&ready.identity, 5000)
            .await
            .unwrap()
            .scope_stopped
    );
}

async fn cancel_tree(mode: &str) {
    let temp = private_tempdir().unwrap();
    let platform = LocalWorkerPlatform::default();
    let (mut ready, _reader) =
        start(&platform, &temp.path().join("run"), temp.path(), mode, mode).await;
    if mode == "cooperate" {
        ready.stdin.write_all(b"stop\n").await.unwrap();
    }
    if mode == "root-first" {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !matches!(
                platform.observe(&ready.identity).await.unwrap(),
                WorkerObservation::Exited { .. }
            ) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    let child_pid: i32 = std::fs::read_to_string(temp.path().join("child.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let result = platform.terminate(&ready.identity, 5000).await.unwrap();
    let before = std::fs::metadata(temp.path().join("heartbeat"))
        .unwrap()
        .len();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        before,
        std::fs::metadata(temp.path().join("heartbeat"))
            .unwrap()
            .len(),
        "side effects continued"
    );
    assert!(
        result.scope_stopped && !result.residual_unknown,
        "{mode}: {result:?}"
    );
    assert_eq!(result.scope, WorkerStopScope::ProcessGroup);
    assert!(matches!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(child_pid).unwrap()),
        Err(rustix::io::Errno::SRCH)
    ));
}

#[tokio::test]
async fn local_worker_stops_ordinary_adapter_child_chain() {
    cancel_tree("tree").await;
}
#[tokio::test]
async fn local_worker_root_exit_does_not_lose_child_stop_authority() {
    cancel_tree("root-first").await;
}
#[tokio::test]
async fn local_worker_normal_close_then_stop() {
    cancel_tree("cooperate").await;
}

#[tokio::test]
async fn local_worker_identity_isolation_zero_budget_and_restart_unknown() {
    let temp = private_tempdir().unwrap();
    let p = LocalWorkerPlatform::default();
    let (a, _ar) = start(&p, &temp.path().join("a"), temp.path(), "a", "idle").await;
    let (b, _br) = start(&p, &temp.path().join("b"), temp.path(), "b", "idle").await;
    let zero = p.terminate(&a.identity, 0).await.unwrap();
    assert_eq!(zero.observation, WorkerObservation::Running);
    assert!(!zero.scope_stopped && zero.residual_unknown);
    let mut wrong = a.identity.clone();
    wrong.creation_identity = b.identity.creation_identity.clone();
    assert_eq!(p.observe(&wrong).await.unwrap(), WorkerObservation::Unknown);
    assert!(!p.terminate(&wrong, 500).await.unwrap().scope_stopped);
    assert_eq!(
        LocalWorkerPlatform::default()
            .observe(&a.identity)
            .await
            .unwrap(),
        WorkerObservation::Unknown
    );
    assert!(p.terminate(&a.identity, 5000).await.unwrap().scope_stopped);
    assert_eq!(
        p.observe(&b.identity).await.unwrap(),
        WorkerObservation::Running
    );
    assert!(p.terminate(&b.identity, 5000).await.unwrap().scope_stopped);
}

#[tokio::test]
async fn local_worker_materials_and_sessions_never_share_ownership() {
    use std::os::unix::fs::PermissionsExt;
    let temp = private_tempdir().unwrap();
    let p = LocalWorkerPlatform::default();
    let session = temp.path().join("session");
    std::fs::create_dir(&session).unwrap();
    std::fs::write(session.join("history"), "keep").unwrap();
    let root = temp.path().join("run");
    let mut req = request(&root, temp.path(), "materials", "idle");
    req.profile.session_root = session.clone();
    req.profile.materials = RunMaterials {
        directories: vec!["home".into()],
        files: vec![RunMaterialFile {
            relative_path: "home/opaque".into(),
            contents: Zeroizing::new(b"secret".to_vec()),
        }],
    };
    let ready = p.launch(req).await.unwrap();
    assert_eq!(std::fs::read(root.join("home/opaque")).unwrap(), b"secret");
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(root.join("home/opaque"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        p.terminate(&ready.identity, 5000)
            .await
            .unwrap()
            .scope_stopped
    );
    assert!(!root.exists());
    assert_eq!(std::fs::read(session.join("history")).unwrap(), b"keep");
    for (i, root) in [session.clone(), session.join("nested")]
        .into_iter()
        .enumerate()
    {
        let nonce = format!("overlap{i}");
        let mut req = request(&root, temp.path(), &nonce, "idle");
        req.profile.session_root = session.clone();
        assert!(p.launch(req).await.is_err());
    }
}

#[tokio::test]
async fn local_worker_rejects_paths_existing_root_pin_and_spawn_failure() {
    let temp = private_tempdir().unwrap();
    let p = LocalWorkerPlatform::default();
    for (i, path) in ["../escape", "/absolute", "missing/file"]
        .into_iter()
        .enumerate()
    {
        let nonce = format!("invalid{i}");
        let root = temp.path().join(&nonce);
        let mut req = request(&root, temp.path(), &nonce, "idle");
        req.profile.materials = RunMaterials {
            directories: vec![],
            files: vec![RunMaterialFile {
                relative_path: path.into(),
                contents: Zeroizing::new(vec![]),
            }],
        };
        assert!(p.launch(req).await.is_err());
        assert!(!root.exists());
    }
    let existing = temp.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(existing.join("keep"), "keep").unwrap();
    assert!(
        p.launch(request(&existing, temp.path(), "existing", "idle"))
            .await
            .is_err()
    );
    assert!(existing.join("keep").exists());
    use std::os::unix::fs::PermissionsExt;
    let mut req = request(&temp.path().join("spawn-fail"), temp.path(), "fail", "idle");
    let broken = temp.path().join("missing-interpreter");
    std::fs::write(&broken, b"#!/nonexistent-hiroute-probe-interpreter\n").unwrap();
    std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o700)).unwrap();
    req.profile.executable = broken;
    assert!(matches!(
        p.launch(req).await,
        Err(DelegationErrorV1::CapabilityUnavailable)
    ));
    assert!(!temp.path().join("spawn-fail").exists());
    let mut req = request(&temp.path().join("bad-env"), temp.path(), "bad-env", "idle");
    req.profile.env.insert(
        "INVALID=NAME".into(),
        Zeroizing::new("secret-not-in-error".into()),
    );
    assert!(matches!(
        p.launch(req).await,
        Err(DelegationErrorV1::InvalidArguments)
    ));
    assert!(!temp.path().join("bad-env").exists());
}

#[tokio::test]
async fn local_worker_cleanup_failure_does_not_delete_replacement_or_session() {
    let temp = private_tempdir().unwrap();
    let p = LocalWorkerPlatform::default();
    let root = temp.path().join("run");
    let (ready, _reader) = start(&p, &root, temp.path(), "cleanup", "idle").await;
    let owned = temp.path().join("moved");
    std::fs::rename(&root, &owned).unwrap();
    let session = temp.path().join("session");
    std::fs::create_dir(&session).unwrap();
    std::fs::write(session.join("keep"), "history").unwrap();
    std::os::unix::fs::symlink(&session, &root).unwrap();
    let failed = p.terminate(&ready.identity, 5000).await.unwrap();
    assert!(failed.scope_stopped && failed.residual_unknown);
    assert_eq!(
        p.material_cleanup(&ready.identity),
        Some(MaterialCleanup::Failed)
    );
    assert!(session.join("keep").exists());
    assert!(p.release(&ready.identity).is_err());
    std::fs::remove_file(&root).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("unrelated"), "keep").unwrap();
    assert!(
        p.terminate(&ready.identity, 5000)
            .await
            .unwrap()
            .residual_unknown
    );
    assert!(root.join("unrelated").exists());
    std::fs::rename(&root, temp.path().join("replacement")).unwrap();
    std::fs::rename(&owned, &root).unwrap();
    assert!(
        !p.terminate(&ready.identity, 5000)
            .await
            .unwrap()
            .residual_unknown
    );
    assert!(session.join("keep").exists());
}

#[tokio::test]
async fn local_worker_consumes_final_profile_materials_preserving_native_root() {
    let temp = private_tempdir().unwrap();
    let platform = LocalWorkerPlatform::default();
    let root = temp.path().join("run-final-profile");
    let req = request(&root, temp.path(), "final-profile", "idle");
    let session = req.profile.session_root.clone();
    std::fs::write(session.join("native-history"), b"keep-native-history").unwrap();
    let ready = platform.launch(req).await.unwrap();
    assert!(root.join("home").is_dir() && root.join("tmp").is_dir());
    assert!(
        platform
            .terminate(&ready.identity, 5000)
            .await
            .unwrap()
            .scope_stopped
    );
    assert!(!root.exists());
    assert_eq!(
        std::fs::read(session.join("native-history")).unwrap(),
        b"keep-native-history"
    );
}

#[tokio::test]
async fn local_worker_already_exited_root_is_reaped_without_false_residual() {
    let temp = private_tempdir().unwrap();
    let platform = LocalWorkerPlatform::default();
    let (ready, _reader) = start(
        &platform,
        &temp.path().join("exited"),
        temp.path(),
        "exited",
        "exit",
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !matches!(
            platform.observe(&ready.identity).await.unwrap(),
            WorkerObservation::Exited { .. }
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let result = platform.terminate(&ready.identity, 5000).await.unwrap();
    assert!(
        result.scope_stopped && !result.residual_unknown,
        "{result:?}"
    );
}
