use crate::client::LocalControlClientError;
use hiroute_application_api::{ErrorCode, WorkPlanListRequestV1};
use std::io::Read;

pub(crate) fn payload(options: &[String]) -> Result<serde_json::Value, ErrorCode> {
    if options != ["--request-stdin"] {
        return Err(ErrorCode::InvalidArguments);
    }
    let mut input = String::new();
    std::io::stdin()
        .take(4097)
        .read_to_string(&mut input)
        .map_err(|_| ErrorCode::InvalidArguments)?;
    if input.len() > 4096 {
        return Err(ErrorCode::InvalidArguments);
    }
    let query: WorkPlanListRequestV1 =
        serde_json::from_str(&input).map_err(|_| ErrorCode::InvalidArguments)?;
    serde_json::to_value(query).map_err(|_| ErrorCode::InvalidArguments)
}

/// Same protected capability carrier, constrained to an inherited pipe and bounded on both
/// bytes and time. Never obtain material from a context selector, argv, or a regular file.
#[cfg(unix)]
pub(crate) fn credential(fd: u32) -> Result<String, LocalControlClientError> {
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    use std::time::{Duration, Instant};
    let denied = || LocalControlClientError::ProtectedInput;
    if fd < 3 {
        return Err(denied());
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY)
        .open(format!("/dev/fd/{fd}"))
        .map_err(|_| denied())?;
    if !file.metadata().map_err(|_| denied())?.file_type().is_fifo() {
        return Err(denied());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = [0u8; 257];
    let mut len = 0;
    let result = loop {
        if Instant::now() >= deadline || len == bytes.len() {
            break Err(denied());
        }
        match file.read(&mut bytes[len..]) {
            Ok(0) => {
                break std::str::from_utf8(&bytes[..len])
                    .ok()
                    .map(str::trim_end)
                    .filter(|s| !s.is_empty() && !s.contains('\0'))
                    .map(str::to_owned)
                    .ok_or_else(denied);
            }
            Ok(n) => len += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break Err(denied()),
        }
    };
    bytes.fill(0);
    result
}
#[cfg(not(unix))]
pub(crate) fn credential(_: u32) -> Result<String, LocalControlClientError> {
    Err(LocalControlClientError::ProtectedInput)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    #[test]
    fn work_plans_credential_rejects_stdio_regular_file_and_oversize_pipe() {
        assert_eq!(credential(0), Err(LocalControlClientError::ProtectedInput));
        let file = tempfile::tempfile().unwrap();
        assert_eq!(
            credential(file.as_raw_fd() as u32),
            Err(LocalControlClientError::ProtectedInput)
        );
        let (read, write) = nix::unistd::pipe().unwrap();
        nix::unistd::write(&write, &[b'x'; 257]).unwrap();
        drop(write);
        assert_eq!(
            credential(read.as_raw_fd() as u32),
            Err(LocalControlClientError::ProtectedInput)
        );
    }
}
