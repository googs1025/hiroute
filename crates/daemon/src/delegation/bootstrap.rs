//! Collaboration credential bytes use a bounded inherited pipe/socket, never argv or a
//! context-selector lookup. The settings owner supplies its already prepared credential.
use hiroute_application::delegation::admission::verify_bootstrap;
use hiroute_domain::delegation::{DelegationErrorV1, DelegationGrantAuthorityPort};
use hiroute_domain::{AgentCollaborationCredential, VerifiedCollaborationPrincipal, WorkspaceId};
use std::fs::File;
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const MAX_CREDENTIAL_BYTES: usize = 256;

/// Existing launcher/settings code owns descriptor inheritance and its lifetime. These
/// endpoints reject ordinary files and stdio; no ambient socket/API can ask for a credential.
/// Transport carries only the credential. Nonsecret selectors are checked against current
/// authority by receive, and cannot be used to retrieve material from LocalSecretStore.
pub fn supply(
    mut channel: &File,
    material: &AgentCollaborationCredential,
) -> Result<(), DelegationErrorV1> {
    protected_channel(channel)?;
    let bytes = material.expose();
    if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_BYTES {
        return Err(DelegationErrorV1::InvalidArguments);
    }
    let mut frame = Zeroizing::new((bytes.len() as u16).to_be_bytes().to_vec());
    frame.extend_from_slice(bytes);
    let mut offset = 0;
    transfer(Instant::now() + Duration::from_secs(2), || {
        let count = channel.write(&frame[offset..])?;
        offset += count;
        Ok((count, offset == frame.len()))
    })
}

pub fn receive(
    mut channel: &File,
    authority: &dyn DelegationGrantAuthorityPort,
    workspace: &WorkspaceId,
    grant_id: &str,
    context: &str,
    generation: u64,
) -> Result<VerifiedCollaborationPrincipal, DelegationErrorV1> {
    protected_channel(channel)?;
    let mut prefix = [0; 2];
    let deadline = Instant::now() + Duration::from_secs(2);
    read_frame(&mut channel, &mut prefix, deadline)?;
    let length = u16::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_CREDENTIAL_BYTES {
        return Err(DelegationErrorV1::PermissionDenied);
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    read_frame(&mut channel, &mut bytes, deadline)?;
    let material =
        AgentCollaborationCredential::from_authenticated_storage(std::mem::take(&mut *bytes))
            .map_err(|_| DelegationErrorV1::PermissionDenied)?;
    verify_bootstrap(
        authority, workspace, grant_id, context, generation, &material,
    )
}

fn protected_channel(channel: &File) -> Result<(), DelegationErrorV1> {
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, unix::fs::FileTypeExt};
        let kind = channel
            .metadata()
            .map_err(|_| DelegationErrorV1::CapabilityUnavailable)?
            .file_type();
        if channel.as_raw_fd() > 2 && (kind.is_fifo() || kind.is_socket()) {
            use nix::fcntl::{FcntlArg, OFlag, fcntl};
            let flags = fcntl(channel, FcntlArg::F_GETFL)
                .map_err(|_| DelegationErrorV1::CapabilityUnavailable)?;
            fcntl(
                channel,
                FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
            )
            .map_err(|_| DelegationErrorV1::CapabilityUnavailable)?;
            return Ok(());
        }
    }
    // Existing launcher has no Windows inherited-channel implementation at this baseline.
    // Do not quietly use a regular file or expose a selector-to-secret API as a fallback.
    let _ = channel;
    Err(DelegationErrorV1::CapabilityUnavailable)
}

fn read_frame(
    channel: &mut &File,
    buffer: &mut [u8],
    deadline: Instant,
) -> Result<(), DelegationErrorV1> {
    let mut offset = 0;
    transfer(deadline, || {
        let count = channel.read(&mut buffer[offset..])?;
        offset += count;
        Ok((count, offset == buffer.len()))
    })
}
fn transfer(
    deadline: Instant,
    mut io: impl FnMut() -> std::io::Result<(usize, bool)>,
) -> Result<(), DelegationErrorV1> {
    loop {
        if Instant::now() >= deadline {
            return Err(DelegationErrorV1::DeadlineExceeded);
        }
        match io() {
            Ok((_, true)) => return Ok(()),
            Ok((0, false)) => return Err(DelegationErrorV1::PermissionDenied),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(DelegationErrorV1::PermissionDenied),
        }
    }
}
