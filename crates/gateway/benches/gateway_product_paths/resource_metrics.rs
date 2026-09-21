use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;

pub fn replay_backing_bytes(pid: u32, root: &Path) -> Result<u64, Box<dyn Error>> {
    replay_backing_bytes_platform(pid, root)
}

pub fn resident_kib(pid: u32) -> Result<u64, Box<dyn Error>> {
    resident_kib_platform(pid)
}

pub fn require_linux_procfs() -> Result<(), Box<dyn Error>> {
    require_linux_procfs_platform()
}

fn directory_bytes(path: &Path) -> Result<u64, Box<dyn Error>> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut total = 0_u64;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if file_type.is_dir() {
            total = total
                .checked_add(directory_bytes(&entry.path())?)
                .ok_or("replay directory size overflow")?;
        } else if file_type.is_file() {
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            total = total
                .checked_add(metadata.len())
                .ok_or("replay directory size overflow")?;
        } else {
            return Err("replay directory contained a non-regular entry".into());
        }
    }
    Ok(total)
}

#[cfg(target_os = "linux")]
fn replay_backing_bytes_platform(pid: u32, root: &Path) -> Result<u64, Box<dyn Error>> {
    let mut total = directory_bytes(root)?;
    let prefix = format!("{}/", root.display());
    for entry in fs::read_dir(format!("/proc/{pid}/fd"))? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let target = match fs::read_link(entry.path()) {
            Ok(target) => target,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let target = target.to_string_lossy();
        let Some(deleted) = target.strip_suffix(" (deleted)") else {
            continue;
        };
        if deleted.starts_with(&prefix) {
            let metadata = match fs::metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            total = total
                .checked_add(metadata.len())
                .ok_or("replay backing size overflow")?;
        }
    }
    Ok(total)
}

#[cfg(not(target_os = "linux"))]
fn replay_backing_bytes_platform(_pid: u32, root: &Path) -> Result<u64, Box<dyn Error>> {
    directory_bytes(root)
}

#[cfg(target_os = "linux")]
fn resident_kib_platform(pid: u32) -> Result<u64, Box<dyn Error>> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or("/proc status did not contain VmRSS")?
        .parse::<u64>()
        .map_err(Into::into)
}

#[cfg(not(target_os = "linux"))]
fn resident_kib_platform(_pid: u32) -> Result<u64, Box<dyn Error>> {
    Err("production RSS measurement requires Linux /proc".into())
}

#[cfg(target_os = "linux")]
fn require_linux_procfs_platform() -> Result<(), Box<dyn Error>> {
    if Path::new("/proc/self/status").is_file() {
        Ok(())
    } else {
        Err("dedicated production stability requires Linux /proc".into())
    }
}

#[cfg(not(target_os = "linux"))]
fn require_linux_procfs_platform() -> Result<(), Box<dyn Error>> {
    Err("dedicated production stability requires Linux /proc".into())
}
