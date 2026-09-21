use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::{Command, Stdio};

pub(crate) fn create_private_dir(path: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(path)?;
    secure_and_verify(path, true)
}

pub(crate) fn private_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT.
    }
    let mut file = options.open(path)?;
    let before = file.metadata()?;
    reject_unsafe_file_type(&before)?;
    secure_open_file(path, &file)?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    let after = file.metadata()?;
    verify_same_file_identity(&before, &after)?;
    verify_metadata_private(&after, false)?;
    let path_identity = open_read_nofollow(path)?.metadata()?;
    verify_same_file(&after, &path_identity)
}

pub(crate) fn verify_private_dir(path: &Path) -> Result<&'static str, std::io::Error> {
    verify_private(path, true)?;
    Ok("owner_only")
}

pub(crate) fn open_read_nofollow(path: &Path) -> Result<File, std::io::Error> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT.
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    reject_unsafe_file_type(&metadata)?;
    verify_metadata_private(&metadata, false)?;
    Ok(file)
}

pub(crate) fn read_private_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, std::io::Error> {
    Ok(snapshot_private_bounded(path, maximum)?.bytes)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrivateFileSnapshot {
    pub bytes: Vec<u8>,
    pub identity: PrivateFileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct PrivateFileIdentity {
    first: u64,
    second: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct PrivateTreeSeal {
    entries: BTreeMap<PathBuf, (bool, PrivateFileIdentity)>,
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimePrivacyAudit {
    pub private_root_mode: &'static str,
    pub secret_bearing_file_mode: &'static str,
    pub readiness_file_mode: &'static str,
    pub process_artifacts_mode: &'static str,
    pub evidence_artifacts_mode: &'static str,
    pub path_identity_preserved: bool,
}

pub(crate) fn seal_private_tree(root: &Path) -> Result<PrivateTreeSeal, std::io::Error> {
    let mut entries = BTreeMap::new();
    visit_tree(root, &mut |path, directory, metadata| {
        verify_private(path, directory)?;
        entries.insert(
            path.strip_prefix(root).unwrap_or(path).to_path_buf(),
            (directory, file_identity(metadata)?),
        );
        Ok(())
    })?;
    Ok(PrivateTreeSeal { entries })
}

pub(crate) fn audit_private_runtime(
    root: &Path,
    fixture: &Path,
    readiness: &Path,
    seal: &PrivateTreeSeal,
) -> RuntimePrivacyAudit {
    let private_root = verify_private(root, true).is_ok();
    let fixture_private = verify_private(fixture, false).is_ok();
    let readiness_private = verify_private(readiness, false).is_ok();
    let process_private = tree_is_private(&root.join("process"));
    let evidence_private = tree_is_private(&root.join("evidence"));
    let path_identity_preserved = seal
        .entries
        .iter()
        .all(|(relative, (directory, identity))| {
            let path = root.join(relative);
            fs::symlink_metadata(&path).is_ok_and(|metadata| {
                metadata.file_type().is_dir() == *directory
                    && verify_private(&path, *directory).is_ok()
                    && file_identity(&metadata).is_ok_and(|actual| actual == *identity)
            })
        });
    RuntimePrivacyAudit {
        private_root_mode: if private_root {
            "owner_only"
        } else {
            "invalid"
        },
        secret_bearing_file_mode: if fixture_private {
            "owner_read_write"
        } else {
            "invalid"
        },
        readiness_file_mode: if readiness_private {
            "owner_read_write"
        } else {
            "invalid"
        },
        process_artifacts_mode: if process_private {
            "owner_only"
        } else {
            "invalid"
        },
        evidence_artifacts_mode: if evidence_private {
            "owner_only"
        } else {
            "invalid"
        },
        path_identity_preserved,
    }
}

fn tree_is_private(root: &Path) -> bool {
    visit_tree(root, &mut |path, directory, _| {
        verify_private(path, directory)
    })
    .is_ok()
}

fn visit_tree(
    root: &Path,
    visit: &mut dyn FnMut(&Path, bool, &fs::Metadata) -> Result<(), std::io::Error>,
) -> Result<(), std::io::Error> {
    let metadata = fs::symlink_metadata(root)?;
    let directory = metadata.file_type().is_dir();
    if metadata.file_type().is_symlink() || (!directory && !metadata.file_type().is_file()) {
        return Err(std::io::Error::other(
            "private tree has an unsafe filesystem type",
        ));
    }
    visit(root, directory, &metadata)?;
    if directory {
        for entry in fs::read_dir(root)? {
            visit_tree(&entry?.path(), visit)?;
        }
    }
    Ok(())
}

pub(crate) fn snapshot_private_bounded(
    path: &Path,
    maximum: u64,
) -> Result<PrivateFileSnapshot, std::io::Error> {
    let mut file = open_read_nofollow(path)?;
    let before = file.metadata()?;
    verify_metadata_private(&before, false)?;
    if before.len() > maximum {
        return Err(std::io::Error::other("private file exceeds its read bound"));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(std::io::Error::other(
            "private file grew beyond its read bound",
        ));
    }
    let after = file.metadata()?;
    verify_same_file(&before, &after)?;
    if after.len() != bytes.len() as u64 {
        return Err(std::io::Error::other(
            "private file changed during atomic snapshot",
        ));
    }
    let path_identity = open_read_nofollow(path)?.metadata()?;
    verify_same_file(&after, &path_identity)?;
    Ok(PrivateFileSnapshot {
        bytes,
        identity: file_identity(&after)?,
    })
}

fn file_identity(metadata: &fs::Metadata) -> Result<PrivateFileIdentity, std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(PrivateFileIdentity {
            first: metadata.dev(),
            second: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let first = metadata
            .volume_serial_number()
            .map(u64::from)
            .ok_or_else(|| std::io::Error::other("Windows volume identity is unavailable"))?;
        let second = metadata
            .file_index()
            .ok_or_else(|| std::io::Error::other("Windows file identity is unavailable"))?;
        Ok(PrivateFileIdentity { first, second })
    }
    #[cfg(not(any(unix, windows)))]
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private file identity is unsupported on this platform",
    ))
}

fn secure_and_verify(path: &Path, directory: bool) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if directory { 0o700 } else { 0o600 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(windows)]
    set_windows_owner_only(path, directory)?;
    #[cfg(not(any(unix, windows)))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "owner-only permissions are unsupported on this platform",
    ));
    verify_private(path, directory)
}

fn verify_private(path: &Path, directory: bool) -> Result<(), std::io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || (directory && !metadata.file_type().is_dir())
        || (!directory && !metadata.file_type().is_file())
    {
        return Err(std::io::Error::other(
            "private path has an unsafe filesystem type",
        ));
    }
    if !directory {
        reject_unsafe_file_type(&metadata)?;
    }
    verify_metadata_private(&metadata, directory)?;
    #[cfg(windows)]
    verify_windows_owner_only(path, directory)?;
    Ok(())
}

fn verify_metadata_private(metadata: &fs::Metadata, directory: bool) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let expected = if directory { 0o700 } else { 0o600 };
        if metadata.permissions().mode() & 0o777 != expected {
            return Err(std::io::Error::other(
                "private path permissions are too broad",
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x0000_0400 != 0 {
            return Err(std::io::Error::other(
                "private path is a Windows reparse point",
            ));
        }
    }
    Ok(())
}

fn reject_unsafe_file_type(metadata: &fs::Metadata) -> Result<(), std::io::Error> {
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::other("evidence is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(std::io::Error::other(
                "private file must not have additional hard links",
            ));
        }
    }
    #[cfg(windows)]
    verify_metadata_private(metadata, false)?;
    Ok(())
}

fn verify_same_file(before: &fs::Metadata, after: &fs::Metadata) -> Result<(), std::io::Error> {
    verify_same_file_identity(before, after)?;
    if before.len() != after.len() {
        return Err(std::io::Error::other(
            "private file length changed during snapshot",
        ));
    }
    Ok(())
}

fn verify_same_file_identity(
    before: &fs::Metadata,
    after: &fs::Metadata,
) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(std::io::Error::other(
                "private file identity changed during snapshot",
            ));
        }
    }
    Ok(())
}

fn secure_open_file(path: &Path, file: &File) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    set_windows_owner_only(path, false)?;
    #[cfg(not(any(unix, windows)))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "owner-only permissions are unsupported on this platform",
    ));
    let handle_identity = file.metadata()?;
    verify_metadata_private(&handle_identity, false)?;
    let path_identity = open_read_nofollow(path)?.metadata()?;
    verify_same_file_identity(&handle_identity, &path_identity)?;
    #[cfg(windows)]
    verify_windows_owner_only(path, false)?;
    Ok(())
}

#[cfg(windows)]
fn set_windows_owner_only(path: &Path, directory: bool) -> Result<(), std::io::Error> {
    run_windows_acl(path, directory, true)
}

#[cfg(windows)]
fn verify_windows_owner_only(path: &Path, directory: bool) -> Result<(), std::io::Error> {
    run_windows_acl(path, directory, false)
}

#[cfg(windows)]
fn run_windows_acl(path: &Path, directory: bool, set: bool) -> Result<(), std::io::Error> {
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$target = $args[0]
$isDirectory = $args[1] -eq 'directory'
$shouldSet = $args[2] -eq 'set'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
if ($shouldSet) {
  if ($isDirectory) {
    $acl = New-Object System.Security.AccessControl.DirectorySecurity
    $inherit = [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [System.Security.AccessControl.InheritanceFlags]::ObjectInherit
  } else {
    $acl = New-Object System.Security.AccessControl.FileSecurity
    $inherit = [System.Security.AccessControl.InheritanceFlags]::None
  }
  $acl.SetOwner($sid)
  $acl.SetAccessRuleProtection($true, $false)
  $rule = New-Object System.Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', $inherit, 'None', 'Allow')
  $acl.SetAccessRule($rule)
  Set-Acl -LiteralPath $target -AclObject $acl
}
$check = Get-Acl -LiteralPath $target
$rules = @($check.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
$ownerSid = $check.GetOwner([System.Security.Principal.SecurityIdentifier]).Value
if ($ownerSid -ne $sid.Value -or $rules.Count -ne 1) { exit 41 }
$only = $rules[0]
if ($only.IdentityReference.Value -ne $sid.Value -or $only.AccessControlType -ne 'Allow' -or $only.IsInherited -or (($only.FileSystemRights -band [System.Security.AccessControl.FileSystemRights]::FullControl) -ne [System.Security.AccessControl.FileSystemRights]::FullControl)) { exit 42 }
"#;
    let status = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ])
        .arg(path)
        .arg(if directory { "directory" } else { "file" })
        .arg(if set { "set" } else { "verify" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Windows owner-only DACL verification failed",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn nofollow_private_write_does_not_modify_a_symlink_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.json");
        fs::write(&target, b"original").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = temp.path().join("result.json");
        symlink(&target, &link).unwrap();

        assert!(private_write(&link, b"replacement").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn bounded_reader_rejects_a_hard_linked_evidence_file() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("evidence.json");
        fs::write(&target, b"{}").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&target, temp.path().join("alias.json")).unwrap();

        assert!(read_private_bounded(&target, 16).is_err());
    }
}
