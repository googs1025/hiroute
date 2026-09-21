use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use hiroute_application_api::{
    STANDALONE_PROTECTED_INPUT_MAX_FRAME_BYTES_V1, STANDALONE_PROTECTED_INPUT_REQUEST_SCHEMA_V1,
    STANDALONE_PROTECTED_INPUT_RESPONSE_SCHEMA_V1, StandaloneProtectedInputRequestV1,
    StandaloneProtectedInputResponseV1,
};
use hiroute_domain::ProtectedSecret;
use hiroute_host_runtime::StandaloneLayout;
use zeroize::{Zeroize, Zeroizing};

const IO_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct StandaloneServer {
    listener: UnixListener,
    path: PathBuf,
    shutdown: Receiver<()>,
}

impl StandaloneServer {
    pub(super) fn bind(layout: &StandaloneLayout) -> Result<Self, String> {
        let path = layout.protected_input_socket();
        let parent = path
            .parent()
            .ok_or("protected input socket parent is unavailable")?;
        validate_private_directory(parent)?;
        remove_stale_socket(&path)?;
        let listener =
            UnixListener::bind(&path).map_err(|_| "protected input socket bind failed")?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "protected input socket permissions failed")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "protected input socket setup failed")?;
        Ok(Self {
            listener,
            path,
            shutdown: shutdown_signal()?,
        })
    }

    pub(super) fn run(self, mut role: hiroute_daemon::RoleAllHandle) -> Result<(), String> {
        let result = loop {
            if self.shutdown.try_recv().is_ok() {
                break Ok(());
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // A malformed or unauthorized client is rejected per connection. It cannot
                    // stop the user's service or poison the next bounded request.
                    let _ = handle_connection(&role, stream);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break Err("protected input socket accept failed".into()),
            }
        };
        role.shutdown();
        let joined = role
            .join(Duration::from_secs(30))
            .map_err(|error| error.to_string());
        let _ = std::fs::remove_file(&self.path);
        result.and(joined)
    }
}

impl Drop for StandaloneServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn handle_connection(
    role: &hiroute_daemon::RoleAllHandle,
    mut stream: UnixStream,
) -> Result<(), String> {
    validate_peer_owner(&stream)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|_| "protected input read timeout setup failed")?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|_| "protected input write timeout setup failed")?;
    let mut bytes = Zeroizing::new(Vec::new());
    Read::by_ref(&mut stream)
        .take(STANDALONE_PROTECTED_INPUT_MAX_FRAME_BYTES_V1 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "protected input request read failed")?;
    if bytes.is_empty() || bytes.len() as u64 > STANDALONE_PROTECTED_INPUT_MAX_FRAME_BYTES_V1 {
        return Err("protected input request size is invalid".into());
    }
    let mut request: StandaloneProtectedInputRequestV1 =
        serde_json::from_slice(&bytes).map_err(|_| "protected input request is invalid")?;
    if request.schema != STANDALONE_PROTECTED_INPUT_REQUEST_SCHEMA_V1 {
        return Err("protected input request schema is invalid".into());
    }
    match request.action.as_str() {
        "register" => {
            if request.candidate_revision != Some(1) {
                return Err("protected input request is invalid".into());
            }
            let candidate_ref = request
                .candidate_ref
                .take()
                .filter(|value| valid_candidate_ref(value))
                .ok_or("protected input candidate is invalid")?;
            let mut secret = request.secret.take().ok_or("protected input is missing")?;
            if secret.is_empty() || secret.len() > 32 * 1024 {
                secret.zeroize();
                return Err("protected input is invalid".into());
            }
            let protected = ProtectedSecret::new(secret.as_bytes().to_vec())
                .map_err(|_| "protected input is invalid")?;
            secret.zeroize();
            role.register_manual_protected_input(
                hiroute_application_api::ComputeCandidateRefV2 {
                    candidate_ref: candidate_ref.clone(),
                    candidate_revision: 1,
                },
                protected,
            )?;
            let response = write_response(
                &mut stream,
                &StandaloneProtectedInputResponseV1 {
                    schema: STANDALONE_PROTECTED_INPUT_RESPONSE_SCHEMA_V1.into(),
                    action: "register".into(),
                    registered: true,
                },
            );
            if response.is_err() {
                let _ = role.release_manual_protected_input(&candidate_ref);
            }
            response
        }
        "release" => {
            if request.candidate_revision.is_some() || request.secret.is_some() {
                return Err("protected input release is invalid".into());
            }
            let candidate_ref = request
                .candidate_ref
                .take()
                .filter(|value| valid_candidate_ref(value))
                .ok_or("protected input candidate is invalid")?;
            role.release_manual_protected_input(&candidate_ref)?;
            write_response(
                &mut stream,
                &StandaloneProtectedInputResponseV1 {
                    schema: STANDALONE_PROTECTED_INPUT_RESPONSE_SCHEMA_V1.into(),
                    action: "release".into(),
                    registered: true,
                },
            )
        }
        _ => Err("protected input action is invalid".into()),
    }
}

fn write_response(
    stream: &mut UnixStream,
    response: &StandaloneProtectedInputResponseV1,
) -> Result<(), String> {
    let mut bytes = Zeroizing::new(
        serde_json::to_vec(response).map_err(|_| "protected input response is invalid")?,
    );
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .and_then(|_| stream.flush())
        .map_err(|_| "protected input response write failed".into())
}

fn valid_candidate_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':')
        })
}

fn validate_private_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "protected input socket directory is unavailable")?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err("protected input socket directory is unsafe".into());
    }
    Ok(())
}

fn remove_stale_socket(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("protected input socket state is unavailable".into()),
        Ok(metadata)
            if metadata.file_type().is_socket()
                && metadata.uid() == nix::unistd::geteuid().as_raw() =>
        {
            std::fs::remove_file(path)
                .map_err(|_| "protected input stale socket cannot be removed".into())
        }
        Ok(_) => Err("protected input socket path conflicts with another entry".into()),
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn validate_peer_owner(stream: &UnixStream) -> Result<(), String> {
    let credentials =
        nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
            .map_err(|_| "protected input peer ownership unavailable")?;
    if credentials.uid() != nix::unistd::geteuid().as_raw() {
        return Err("protected input peer ownership mismatch".into());
    }
    Ok(())
}

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "ios",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
))]
fn validate_peer_owner(stream: &UnixStream) -> Result<(), String> {
    let (uid, _) = nix::unistd::getpeereid(stream)
        .map_err(|_| "protected input peer ownership unavailable")?;
    if uid != nix::unistd::geteuid() {
        return Err("protected input peer ownership mismatch".into());
    }
    Ok(())
}

#[cfg(not(any(
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "ios",
    target_os = "linux",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos"
)))]
fn validate_peer_owner(_stream: &UnixStream) -> Result<(), String> {
    Err("protected input peer ownership unavailable".into())
}

fn shutdown_signal() -> Result<Receiver<()>, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("hiroute-standalone-signals".into())
        .spawn(move || {
            use tokio::signal::unix::{SignalKind, signal};
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let (Ok(mut terminate), Ok(mut interrupt)) = (
                    signal(SignalKind::terminate()),
                    signal(SignalKind::interrupt()),
                ) else {
                    return;
                };
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = interrupt.recv() => {}
                }
                let _ = sender.try_send(());
            });
        })
        .map_err(|_| "standalone signal handler unavailable")?;
    Ok(receiver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_rejects_authority_fields_and_invalid_candidate_shapes() {
        assert!(valid_candidate_ref("candidate/manual:one"));
        assert!(!valid_candidate_ref("candidate with spaces"));
        let request: Result<StandaloneProtectedInputRequestV1, _> = serde_json::from_slice(
            br#"{"schema":"hiroute.standalone-protected-input-request/v1","action":"register","candidate_ref":"candidate/manual","candidate_revision":1,"secret":"x","operation_kind":"ApplyAgentPlanChange"}"#,
        );
        assert!(request.is_err());
    }

    #[test]
    fn stale_socket_cleanup_refuses_regular_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("protected-input.sock");
        std::fs::write(&path, b"unrelated").unwrap();
        assert!(remove_stale_socket(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unrelated");
    }
}
