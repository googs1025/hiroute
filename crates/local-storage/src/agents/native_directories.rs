//! Directory ownership attached to the existing artifact marker, not to user configuration.
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct CreatedNativeDirectory {
    path: PathBuf,
    device: u64,
    inode: u64,
    created_at_nanos: Option<u64>,
}

pub(super) fn validate_parent(parent: &Path) -> Result<(), LocalStorageError> {
    let mut current = parent;
    for _ in 0..32 {
        match fs::symlink_metadata(current) {
            Ok(metadata) => return validate_owner_directory_metadata(&metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = current.parent().ok_or(LocalStorageError::InvalidData)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(LocalStorageError::InvalidData)
}

pub(super) fn prepare_parent(
    target: &Path,
) -> Result<Vec<CreatedNativeDirectory>, LocalStorageError> {
    let mut missing = Vec::new();
    let mut current = target.parent().ok_or(LocalStorageError::InvalidData)?;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) => {
                validate_owner_directory_metadata(&metadata)?;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if missing.len() >= 32 {
                    return Err(LocalStorageError::InvalidData);
                }
                missing.push(current.to_owned());
                current = current.parent().ok_or(LocalStorageError::InvalidData)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut created = Vec::new();
    for path in missing.into_iter().rev() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            fs::DirBuilder::new().mode(0o700).create(&path)?;
            let metadata = fs::symlink_metadata(&path)?;
            validate_owner_directory_metadata(&metadata)?;
            created.push(CreatedNativeDirectory {
                path: path.clone(),
                device: metadata.dev(),
                inode: metadata.ino(),
                created_at_nanos: creation_time(&metadata),
            });
            sync_parent(&path)?;
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            return Err(LocalStorageError::Permission);
        }
    }
    // A crash before marker persistence never licenses claiming an existing directory later.
    // Such uncertain directories are retained; only recorded inode ownership permits cleanup.
    Ok(created)
}

pub(super) fn cleanup(
    target: &Path,
    directories: &[CreatedNativeDirectory],
) -> Result<bool, LocalStorageError> {
    match fs::symlink_metadata(target) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if directories.len() > 32 {
        return Err(LocalStorageError::InvalidData);
    }
    let parent = target.parent().ok_or(LocalStorageError::InvalidData)?;
    for directory in directories.iter().rev() {
        if !directory.path.is_absolute()
            || !parent.starts_with(&directory.path)
            || directory
                .path
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        {
            return Err(LocalStorageError::InvalidData);
        }
        let metadata = match fs::symlink_metadata(&directory.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        validate_owner_directory_metadata(&metadata)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.dev() != directory.device || metadata.ino() != directory.inode {
                return Ok(false);
            }
            // Creation time prevents a quickly recycled inode from identifying a user replacement.
            if directory.created_at_nanos.is_none()
                || creation_time(&metadata) != directory.created_at_nanos
            {
                return Ok(false);
            }
        }
        #[cfg(not(unix))]
        {
            return Err(LocalStorageError::Permission);
        }
        if fs::read_dir(&directory.path)?.next().is_some() {
            return Ok(false);
        }
        // remove_dir never recursively deletes newly added user content.
        match fs::remove_dir(&directory.path) {
            Ok(()) => sync_parent(&directory.path)?,
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

fn creation_time(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .created()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos()
        .try_into()
        .ok()
}

// Freshly created identities replace stale entries at the same path; surviving owned ancestors
// remain tracked across upgrades, removal and reinstall. Cleanup still checks each live identity.
pub(super) fn merge_lineage(
    inherited: Vec<CreatedNativeDirectory>,
    fresh: Vec<CreatedNativeDirectory>,
) -> Result<Vec<CreatedNativeDirectory>, LocalStorageError> {
    let mut by_path = std::collections::BTreeMap::new();
    for directory in inherited.into_iter().chain(fresh) {
        by_path.insert(directory.path.clone(), directory);
    }
    if by_path.len() > 32 {
        return Err(LocalStorageError::InvalidData);
    }
    let mut directories = by_path.into_values().collect::<Vec<_>>();
    directories.sort_by_key(|directory| directory.path.components().count());
    Ok(directories)
}
