use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::timeout;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProcessError {
    #[error("cannot create private work directory for {label}: {source}")]
    WorkDir {
        label: &'static str,
        source: std::io::Error,
    },
    #[error("cannot create E2E process log: {0}")]
    Log(std::io::Error),
    #[error("cannot start {label}: {source}")]
    Spawn {
        label: &'static str,
        source: std::io::Error,
    },
    #[error("{0} exited before becoming ready")]
    EarlyExit(&'static str),
    #[error("{0} exited before harness-initiated termination")]
    ExitedBeforeTermination(&'static str),
    #[error("{0} did not become ready before the deadline")]
    ReadinessTimeout(&'static str),
    #[error("cannot inspect {label}: {source}")]
    Inspect {
        label: &'static str,
        source: std::io::Error,
    },
    #[error("cannot terminate {label}: {source}")]
    Terminate {
        label: &'static str,
        source: std::io::Error,
    },
    #[error("{label} exit status does not prove harness termination: {status}")]
    UnprovenTermination {
        label: &'static str,
        status: ExitStatus,
    },
    #[error("{0} did not terminate within the cleanup bound")]
    CleanupTimeout(&'static str),
}

pub(crate) struct ManagedChild {
    label: &'static str,
    child: Child,
    termination_initiated: bool,
}

pub(crate) struct PreparedChild {
    label: &'static str,
    work_dir: PathBuf,
    stdout: File,
    stderr: File,
}

impl ManagedChild {
    pub fn spawn(
        label: &'static str,
        binary: &Path,
        args: &[String],
        log_dir: &Path,
    ) -> Result<Self, ProcessError> {
        Self::prepare(label, log_dir)?.spawn(binary, args)
    }

    pub fn prepare(label: &'static str, log_dir: &Path) -> Result<PreparedChild, ProcessError> {
        fs::create_dir_all(log_dir).map_err(ProcessError::Log)?;
        private_permissions(log_dir).map_err(ProcessError::Log)?;
        let work_dir = private_empty_work_dir(label, log_dir)?;
        let stdout = private_log(&log_dir.join(format!("{label}.stdout.log")))?;
        let stderr = private_log(&log_dir.join(format!("{label}.stderr.log")))?;
        Ok(PreparedChild {
            label,
            work_dir,
            stdout,
            stderr,
        })
    }
}

impl PreparedChild {
    pub fn spawn(self, binary: &Path, args: &[String]) -> Result<ManagedChild, ProcessError> {
        self.spawn_with_env(binary, args, &[])
    }

    pub fn spawn_with_env(
        self,
        binary: &Path,
        args: &[String],
        environment: &[(String, String)],
    ) -> Result<ManagedChild, ProcessError> {
        let mut command = Command::new(binary);
        command
            .args(args)
            // A developer shell commonly contains real provider credentials.
            // The hermetic topology needs none of them, so child processes
            // start from an explicit minimal environment instead of inheriting
            // the caller's account state.
            .env_clear()
            .current_dir(&self.work_dir)
            .env("HOME", &self.work_dir)
            .env("XDG_CONFIG_HOME", &self.work_dir)
            .env("XDG_CACHE_HOME", &self.work_dir)
            .env("TMPDIR", &self.work_dir)
            // Keep the black-box topology hermetic even when stock CPA starts
            // optional background update checks. Loopback provider traffic is
            // explicitly exempted and therefore still reaches the native mocks.
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .env("http_proxy", "http://127.0.0.1:1")
            .env("https_proxy", "http://127.0.0.1:1")
            .env("all_proxy", "http://127.0.0.1:1")
            .env("NO_PROXY", "127.0.0.1,localhost,::1")
            .env("no_proxy", "127.0.0.1,localhost,::1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(self.stdout))
            .stderr(Stdio::from(self.stderr))
            .kill_on_drop(true);
        command.envs(environment.iter().map(|(key, value)| (key, value)));
        let child = command.spawn().map_err(|source| ProcessError::Spawn {
            label: self.label,
            source,
        })?;
        Ok(ManagedChild {
            label: self.label,
            child,
            termination_initiated: false,
        })
    }
}

impl ManagedChild {
    pub async fn wait_ready(
        &mut self,
        addr: SocketAddr,
        deadline: Duration,
    ) -> Result<(), ProcessError> {
        timeout(deadline, async {
            loop {
                if TcpStream::connect(addr).await.is_ok() {
                    return Ok(());
                }
                if self
                    .child
                    .try_wait()
                    .map_err(|source| ProcessError::Inspect {
                        label: self.label,
                        source,
                    })?
                    .is_some()
                {
                    return Err(ProcessError::EarlyExit(self.label));
                }
                // Readiness is driven by connection attempts and scheduler progress; there is
                // deliberately no fixed sleep that makes startup timing environment-dependent.
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| ProcessError::ReadinessTimeout(self.label))?
    }

    pub fn id(&self) -> Result<u32, ProcessError> {
        self.child.id().ok_or(ProcessError::EarlyExit(self.label))
    }

    pub fn ensure_running(&mut self) -> Result<(), ProcessError> {
        if self
            .child
            .try_wait()
            .map_err(|source| ProcessError::Inspect {
                label: self.label,
                source,
            })?
            .is_some()
        {
            Err(ProcessError::EarlyExit(self.label))
        } else {
            Ok(())
        }
    }

    pub async fn stop(&mut self, bound: Duration) -> Result<(), ProcessError> {
        self.stop_after_liveness_probe(bound, std::future::ready(()))
            .await
    }

    async fn stop_after_liveness_probe<F>(
        &mut self,
        bound: Duration,
        boundary: F,
    ) -> Result<(), ProcessError>
    where
        F: Future<Output = ()>,
    {
        if self.termination_initiated {
            return Ok(());
        }
        if self
            .child
            .try_wait()
            .map_err(|source| ProcessError::Inspect {
                label: self.label,
                source,
            })?
            .is_some()
        {
            return Err(ProcessError::ExitedBeforeTermination(self.label));
        }
        boundary.await;
        self.child
            .start_kill()
            .map_err(|source| ProcessError::Terminate {
                label: self.label,
                source,
            })?;
        self.termination_initiated = true;
        let status = timeout(bound, self.child.wait())
            .await
            .map_err(|_| ProcessError::CleanupTimeout(self.label))?
            .map_err(|source| ProcessError::Terminate {
                label: self.label,
                source,
            })?;
        if exit_status_proves_harness_kill(&status) {
            Ok(())
        } else {
            Err(ProcessError::UnprovenTermination {
                label: self.label,
                status,
            })
        }
    }

    pub async fn shutdown(mut self, bound: Duration) -> Result<(), ProcessError> {
        self.stop(bound).await
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn exit_status_proves_harness_kill(status: &ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        status.signal() == Some(libc::SIGKILL)
    }
    #[cfg(windows)]
    {
        // Rust's Windows Child::kill uses TerminateProcess with exit code 1.
        // A process that won the race and exited first makes TerminateProcess
        // fail; requiring its reserved status also fails closed if that
        // platform behavior ever changes.
        status.code() == Some(1)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = status;
        false
    }
}

fn private_empty_work_dir(label: &'static str, log_dir: &Path) -> Result<PathBuf, ProcessError> {
    let work_root = log_dir.join("work");
    fs::create_dir_all(&work_root).map_err(|source| ProcessError::WorkDir { label, source })?;
    private_permissions(&work_root).map_err(|source| ProcessError::WorkDir { label, source })?;
    let work_dir = work_root.join(label);
    fs::create_dir(&work_dir).map_err(|source| ProcessError::WorkDir { label, source })?;
    private_permissions(&work_dir).map_err(|source| ProcessError::WorkDir { label, source })?;
    Ok(work_dir)
}

fn private_permissions(path: &Path) -> Result<(), std::io::Error> {
    crate::p0::privacy::create_private_dir(path)
}

fn private_log(path: &Path) -> Result<File, ProcessError> {
    crate::p0::privacy::private_write(path, b"").map_err(ProcessError::Log)?;
    let mut options = OpenOptions::new();
    options.truncate(true).write(true);
    options.open(path).map_err(ProcessError::Log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const BOUNDARY_CHILD_ADDR: &str = "HIROUTE_E2E_BOUNDARY_CHILD_ADDR";

    #[test]
    fn every_child_gets_a_distinct_private_empty_work_directory() {
        let temp = tempfile::tempdir().unwrap();
        let first = private_empty_work_dir("first", temp.path()).unwrap();
        let second = private_empty_work_dir("second", temp.path()).unwrap();
        assert_ne!(first, second);
        assert!(fs::read_dir(&first).unwrap().next().is_none());
        assert!(fs::read_dir(&second).unwrap().next().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(first).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(second).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn synchronized_boundary_child() {
        let Ok(addr) = std::env::var(BOUNDARY_CHILD_ADDR) else {
            return;
        };
        let mut control = std::net::TcpStream::connect(addr).unwrap();
        control.write_all(b"R").unwrap();
        let mut request = [0_u8; 1];
        control.read_exact(&mut request).unwrap();
        assert_eq!(request, *b"X");
        // Keep the control socket owned until process teardown. EOF therefore
        // comes from the OS closing the child's handles during natural exit,
        // rather than from a user-space close that could race with exit().
        std::process::exit(0);
    }

    async fn spawn_synchronized_child() -> (ManagedChild, tokio::net::TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .arg("--exact")
            .arg("process::tests::synchronized_boundary_child")
            .env(
                BOUNDARY_CHILD_ADDR,
                listener.local_addr().unwrap().to_string(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = child.spawn().unwrap();
        let (mut control, _) = listener.accept().await.unwrap();
        let mut ready = [0_u8; 1];
        control.read_exact(&mut ready).await.unwrap();
        assert_eq!(ready, *b"R");
        (
            ManagedChild {
                label: "synchronized-boundary-child",
                child,
                termination_initiated: false,
            },
            control,
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn harness_kill_reaped_status_is_accepted() {
        let (mut child, mut control) = spawn_synchronized_child().await;
        child.stop(Duration::from_secs(3)).await.unwrap();
        let mut scratch = [0_u8; 1];
        assert_eq!(control.read(&mut scratch).await.unwrap(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn synchronized_natural_exit_between_probe_and_kill_is_rejected() {
        for _ in 0..8 {
            let (mut child, mut control) = spawn_synchronized_child().await;
            let result = child
                .stop_after_liveness_probe(Duration::from_secs(3), async move {
                    control.write_all(b"X").await.unwrap();
                    let mut scratch = [0_u8; 1];
                    assert_eq!(control.read(&mut scratch).await.unwrap(), 0);
                })
                .await;
            assert!(matches!(
                result,
                Err(ProcessError::UnprovenTermination {
                    label: "synchronized-boundary-child",
                    ..
                }) | Err(ProcessError::Terminate {
                    label: "synchronized-boundary-child",
                    ..
                })
            ));
        }
    }
}
