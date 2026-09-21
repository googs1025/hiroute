#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

pub fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
        .expect("chmod temp dir");
    directory
}
