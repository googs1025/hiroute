use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use super::{Result, SmokeError, require};
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};

pub(super) fn private_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

pub(super) struct Process {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
    cancel: Arc<AtomicBool>,
    reaped: bool,
}

impl Process {
    pub(super) fn spawn(
        command: &mut Command,
        logs: &Path,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self> {
        Self::spawn_with_stdin(command, logs, cancel, Stdio::null())
    }
    pub(super) fn spawn_with_input(
        command: &mut Command,
        logs: &Path,
        cancel: Arc<AtomicBool>,
        input: File,
    ) -> Result<Self> {
        Self::spawn_with_stdin(command, logs, cancel, Stdio::from(input))
    }
    fn spawn_with_stdin(
        command: &mut Command,
        logs: &Path,
        cancel: Arc<AtomicBool>,
        stdin: Stdio,
    ) -> Result<Self> {
        require(!cancel.load(Ordering::SeqCst), "cancelled")?;
        let stdout = logs.with_extension("stdout");
        let stderr = logs.with_extension("stderr");
        let child = command
            .process_group(0)
            .stdin(stdin)
            .stdout(private_file(&stdout)?)
            .stderr(private_file(&stderr)?)
            .spawn()?;
        // Establish the cleanup guard before any fallible bookkeeping after spawn.
        let process = Self {
            child,
            stdout,
            stderr,
            cancel,
            reaped: false,
        };
        let mut identity = private_file(&logs.with_extension("process.json"))?;
        identity.write_all(&serde_json::to_vec(&serde_json::json!({
            "pid":process.id(), "process_group":process.id(), "owner_pid":std::process::id()
        }))?)?;
        identity.sync_all()?;
        Ok(process)
    }
    pub(super) fn id(&self) -> u32 {
        self.child.id()
    }
    pub(super) fn check(&mut self) -> Result<()> {
        require(!self.cancel.load(Ordering::SeqCst), "cancelled")?;
        require(
            self.child.try_wait()?.is_none(),
            "child_exited_before_ready",
        )
    }
    pub(super) fn wait(&mut self, deadline: Instant, limit: u64) -> Result<ExitStatus> {
        loop {
            require(!self.cancel.load(Ordering::SeqCst), "cancelled")?;
            require(Instant::now() < deadline, "process_timeout")?;
            require(
                std::fs::metadata(&self.stdout)?.len() <= limit
                    && std::fs::metadata(&self.stderr)?.len() <= limit,
                "output_limit",
            )?;
            if let Some(status) = self.child.try_wait()? {
                // Reap/terminate owned descendants even when the leader exited normally.
                self.stop()?;
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pub(super) fn output(&self, limit: u64) -> Result<(Vec<u8>, Vec<u8>)> {
        let read = |path: &Path| -> Result<Vec<u8>> {
            let mut bytes = Vec::new();
            File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
            require(bytes.len() as u64 <= limit, "output_limit")?;
            Ok(bytes)
        };
        Ok((read(&self.stdout)?, read(&self.stderr)?))
    }
    pub(super) fn stop(&mut self) -> Result<()> {
        if self.reaped {
            return Ok(());
        }
        let group = Pid::from_raw(self.child.id() as i32);
        let signal = |value| match killpg(group, value) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(_) => Err(SmokeError("process_cleanup_failed")),
        };
        signal(Signal::SIGTERM)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.child.try_wait()?.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        signal(Signal::SIGKILL)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.child.try_wait()?.is_none() {
            require(Instant::now() < deadline, "process_cleanup_timeout")?;
            std::thread::sleep(Duration::from_millis(20));
        }
        self.reaped = true;
        Ok(())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_and_cancel_reap_owned_child_without_stopping_another_task() {
        let temp = tempfile::tempdir().unwrap();
        let mut daily_command = Command::new("/bin/sh");
        daily_command.args(["-c", "sleep 60"]);
        let mut daily = Process::spawn(
            &mut daily_command,
            &temp.path().join("daily"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 60 & wait"]);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut child =
            Process::spawn(&mut command, &temp.path().join("timeout"), cancel.clone()).unwrap();
        assert_eq!(
            child
                .wait(Instant::now() + Duration::from_millis(25), 1024)
                .unwrap_err()
                .0,
            "process_timeout"
        );
        child.stop().unwrap();
        assert!(child.child.try_wait().unwrap().is_some());
        daily.check().unwrap();
        let mut child =
            Process::spawn(&mut command, &temp.path().join("cancel"), cancel.clone()).unwrap();
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            child
                .wait(Instant::now() + Duration::from_secs(1), 1024)
                .unwrap_err()
                .0,
            "cancelled"
        );
        child.stop().unwrap();
        daily.check().unwrap();
        daily.stop().unwrap();
    }

    #[test]
    fn oversized_output_is_a_bounded_failure() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "while :; do printf 'sentinel-secret'; done"]);
        let mut child = Process::spawn(
            &mut command,
            &temp.path().join("limit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let error = child
            .wait(Instant::now() + Duration::from_secs(2), 64)
            .unwrap_err();
        assert_eq!(error.0, "output_limit");
        child.stop().unwrap();
        assert!(!error.to_string().contains("sentinel-secret"));
    }
}
