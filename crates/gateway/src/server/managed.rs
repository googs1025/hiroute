//! Managed in-process lifetime for the production Pingora listener.

use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;

use super::GatewayLauncher;
#[cfg(unix)]
use super::{GatewayLauncherError, GatewayMode, ListenerRunStyle};
use hiroute_host_runtime::{connect_address, validate_gateway_listen};

#[cfg(unix)]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const STARTUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

/// The externally meaningful phase of a managed Gateway listener.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedGatewayPhase {
    Starting,
    Ready,
    ShutdownRequested,
    Terminated,
    Failed,
}

/// A production Gateway listener owned by an embedding process.
///
/// `shutdown` is idempotent. The embedding owner supplies the bound for
/// `join`, so it can preserve its reverse shutdown order without exposing
/// Pingora runtime details.
pub struct ManagedGatewayHandle {
    listen: SocketAddr,
    phase: Arc<Mutex<ManagedGatewayPhase>>,
    shutdown: Arc<ShutdownRequest>,
    completion: std::sync::mpsc::Receiver<WorkerOutcome>,
    worker: Option<std::thread::JoinHandle<()>>,
    outcome: Option<WorkerOutcome>,
}

impl ManagedGatewayHandle {
    pub fn listen_address(&self) -> SocketAddr {
        self.listen
    }

    pub fn phase(&self) -> ManagedGatewayPhase {
        read_phase(&self.phase)
    }

    pub fn shutdown(&self) {
        self.shutdown.request();
        let mut phase = self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(
            *phase,
            ManagedGatewayPhase::Starting | ManagedGatewayPhase::Ready
        ) {
            *phase = ManagedGatewayPhase::ShutdownRequested;
        }
    }

    pub fn join(&mut self, timeout: Duration) -> Result<(), ManagedGatewayError> {
        if let Some(outcome) = self.outcome {
            return outcome.into_result(self.listen);
        }
        let outcome = match self.completion.recv_timeout(timeout) {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err(ManagedGatewayError::JoinTimeout {
                    listen: self.listen,
                    timeout,
                });
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => WorkerOutcome::Panicked,
        };
        self.finish_worker(outcome)
    }

    fn wait_until_ready(&mut self, timeout: Duration) -> Result<(), ManagedGatewayError> {
        let started = std::time::Instant::now();
        loop {
            if production_ready(self.listen) {
                set_phase(&self.phase, ManagedGatewayPhase::Ready);
                return Ok(());
            }
            match self.completion.try_recv() {
                Ok(WorkerOutcome::Terminated) => {
                    self.finish_worker(WorkerOutcome::Terminated)?;
                    return Err(ManagedGatewayError::StoppedBeforeReady(self.listen));
                }
                Ok(WorkerOutcome::Panicked) => {
                    return self.finish_worker(WorkerOutcome::Panicked);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return self.finish_worker(WorkerOutcome::Panicked);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            if started.elapsed() >= timeout {
                return Err(ManagedGatewayError::ReadyTimeout {
                    listen: self.listen,
                    timeout,
                });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn finish_worker(&mut self, mut outcome: WorkerOutcome) -> Result<(), ManagedGatewayError> {
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            outcome = WorkerOutcome::Panicked;
            set_phase(&self.phase, ManagedGatewayPhase::Failed);
        }
        self.outcome = Some(outcome);
        outcome.into_result(self.listen)
    }
}

impl Drop for ManagedGatewayHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone, Copy)]
enum WorkerOutcome {
    Terminated,
    Panicked,
}

impl WorkerOutcome {
    fn into_result(self, listen: SocketAddr) -> Result<(), ManagedGatewayError> {
        match self {
            Self::Terminated => Ok(()),
            Self::Panicked => Err(ManagedGatewayError::WorkerPanicked(listen)),
        }
    }
}

#[derive(Default)]
struct ShutdownRequest {
    requested: std::sync::atomic::AtomicBool,
    wake: tokio::sync::Notify,
}

impl ShutdownRequest {
    fn request(&self) {
        if !self
            .requested
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            self.wake.notify_one();
        }
    }

    #[cfg(unix)]
    async fn wait(&self) {
        loop {
            if self.requested.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            self.wake.notified().await;
        }
    }
}

#[cfg(unix)]
struct ManagedShutdownSignal {
    request: Arc<ShutdownRequest>,
}

#[cfg(unix)]
#[async_trait::async_trait]
impl pingora_core::server::ShutdownSignalWatch for ManagedShutdownSignal {
    async fn recv(&self) -> pingora_core::server::ShutdownSignal {
        self.request.wait().await;
        pingora_core::server::ShutdownSignal::GracefulTerminate
    }
}

#[cfg(unix)]
pub(super) fn start(
    launcher: GatewayLauncher,
) -> Result<ManagedGatewayHandle, ManagedGatewayError> {
    if !matches!(&launcher.mode, GatewayMode::Production { .. }) {
        return Err(ManagedGatewayError::ProductionOnly);
    }
    let listen = launcher.listen;
    if validate_gateway_listen(listen).is_err() {
        return Err(GatewayLauncherError::ListenerMustUseReservedAddress(listen).into());
    }
    let reservation = std::net::TcpListener::bind(listen)
        .map_err(|source| ManagedGatewayError::ListenerUnavailable { listen, source })?;
    drop(reservation);

    let server = launcher.into_server(ListenerRunStyle::Managed)?;
    let phase = Arc::new(Mutex::new(ManagedGatewayPhase::Starting));
    let shutdown = Arc::new(ShutdownRequest::default());
    let (completion_tx, completion) = std::sync::mpsc::channel();
    let worker_phase = Arc::clone(&phase);
    let worker_shutdown = Arc::clone(&shutdown);
    let worker = std::thread::Builder::new()
        .name("hiroute-managed-gateway".to_owned())
        .spawn(move || {
            let outcome = if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                server.run(pingora_core::server::RunArgs {
                    shutdown_signal: Box::new(ManagedShutdownSignal {
                        request: worker_shutdown,
                    }),
                });
            }))
            .is_ok()
            {
                set_phase(&worker_phase, ManagedGatewayPhase::Terminated);
                WorkerOutcome::Terminated
            } else {
                set_phase(&worker_phase, ManagedGatewayPhase::Failed);
                WorkerOutcome::Panicked
            };
            let _ = completion_tx.send(outcome);
        })
        .map_err(ManagedGatewayError::WorkerSpawn)?;
    let mut handle = ManagedGatewayHandle {
        listen,
        phase,
        shutdown,
        completion,
        worker: Some(worker),
        outcome: None,
    };
    if let Err(error) = handle.wait_until_ready(STARTUP_TIMEOUT) {
        handle.shutdown();
        if handle.worker.is_some() {
            handle.join(STARTUP_CLEANUP_TIMEOUT)?;
        }
        return Err(error);
    }
    Ok(handle)
}

#[cfg(not(unix))]
pub(super) fn start(
    _launcher: GatewayLauncher,
) -> Result<ManagedGatewayHandle, ManagedGatewayError> {
    Err(ManagedGatewayError::UnsupportedPlatform)
}

fn production_ready(listen: SocketAddr) -> bool {
    use std::io::{Read, Write};

    let io_timeout = Duration::from_millis(200);
    let Ok(target) = connect_address(listen) else {
        return false;
    };
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&target, io_timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(io_timeout));
    let _ = stream.set_write_timeout(Some(io_timeout));
    let request =
        format!("GET /_hiroute/ready HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = Vec::new();
    if stream.read_to_end(&mut response).is_err() {
        return false;
    }
    let Some(body_start) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    if !response.starts_with(b"HTTP/1.1 200 ") && !response.starts_with(b"HTTP/1.0 200 ") {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(&response[body_start + 4..]).is_ok_and(|body| {
        body["schema_version"] == "hiroute.gateway.ready/v2" && body["status"] == "ready"
    })
}

fn read_phase(phase: &Mutex<ManagedGatewayPhase>) -> ManagedGatewayPhase {
    *phase
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn set_phase(phase: &Mutex<ManagedGatewayPhase>, next: ManagedGatewayPhase) {
    *phase
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
}

#[derive(Debug, Error)]
pub enum ManagedGatewayError {
    #[error(transparent)]
    Launcher(#[from] GatewayLauncherError),
    #[error("managed Gateway startup requires a production launcher")]
    ProductionOnly,
    #[error("managed Gateway lifecycle is currently supported only on Unix")]
    UnsupportedPlatform,
    #[error("cannot spawn managed Gateway worker: {0}")]
    WorkerSpawn(#[source] io::Error),
    #[error("managed Gateway listener {listen} is unavailable: {source}")]
    ListenerUnavailable {
        listen: SocketAddr,
        #[source]
        source: io::Error,
    },
    #[error("managed Gateway at {0} stopped before readiness")]
    StoppedBeforeReady(SocketAddr),
    #[error("managed Gateway at {listen} did not become ready within {timeout:?}")]
    ReadyTimeout {
        listen: SocketAddr,
        timeout: Duration,
    },
    #[error("managed Gateway at {listen} did not join within {timeout:?}")]
    JoinTimeout {
        listen: SocketAddr,
        timeout: Duration,
    },
    #[error("managed Gateway worker at {0} panicked")]
    WorkerPanicked(SocketAddr),
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn reserved_loopback_address() -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    }

    #[test]
    fn production_listener_is_ready_and_releases_port_after_idempotent_shutdown() {
        let listen = reserved_loopback_address();
        let lkg_path = std::env::temp_dir().join(format!(
            "hiroute-managed-gateway-missing-lkg-{}-{listen}.json",
            std::process::id()
        ));
        let launcher = GatewayLauncher::production(listen, lkg_path).unwrap();

        let mut handle = launcher.start_managed().unwrap();
        assert_eq!(handle.listen_address(), listen);
        assert_eq!(handle.phase(), ManagedGatewayPhase::Ready);
        assert!(production_ready(listen));

        handle.shutdown();
        handle.shutdown();
        handle.join(Duration::from_secs(5)).unwrap();
        assert_eq!(handle.phase(), ManagedGatewayPhase::Terminated);

        std::net::TcpListener::bind(listen).expect("managed join must release the listener port");
    }

    #[test]
    fn wildcard_listener_uses_loopback_for_readiness_and_releases_the_bound_port() {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let listen = SocketAddr::from(([0, 0, 0, 0], port));
        let lkg_path = std::env::temp_dir().join(format!(
            "hiroute-managed-gateway-wildcard-missing-lkg-{}-{port}.json",
            std::process::id()
        ));
        let launcher = GatewayLauncher::production(listen, lkg_path).unwrap();
        let mut handle = launcher.start_managed().unwrap();
        assert_eq!(handle.listen_address(), listen);
        assert!(production_ready(listen));
        handle.shutdown();
        handle.join(Duration::from_secs(5)).unwrap();
        std::net::TcpListener::bind(listen).expect("wildcard listener must be released");
    }

    #[test]
    fn ipv6_listener_is_rejected_before_startup() {
        let listen = "[::1]:5837".parse().unwrap();
        let lkg_path = std::env::temp_dir().join(format!(
            "hiroute-managed-gateway-ipv6-missing-lkg-{}.json",
            std::process::id()
        ));
        let launcher = GatewayLauncher::production(listen, lkg_path).unwrap();
        assert!(matches!(
            launcher.start_managed(),
            Err(ManagedGatewayError::Launcher(
                GatewayLauncherError::ListenerMustUseReservedAddress(value)
            )) if value == listen
        ));
    }
}
