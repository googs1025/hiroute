use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Read;

use serde_json::Value;

use super::ManagedLaunchFailure;

const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

pub(super) fn resolve_user_settings(value: &OsStr) -> Result<Value, ManagedLaunchFailure> {
    if value.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(ManagedLaunchFailure::InvalidArguments);
    }
    let document = match value
        .to_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
    {
        Some(document) => document,
        None => {
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                // Opening a caller-supplied FIFO must not wait for a writer before validation.
                options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY);
            }
            let file = options
                .open(value)
                .map_err(|_| ManagedLaunchFailure::InvalidArguments)?;
            let metadata = file
                .metadata()
                .map_err(|_| ManagedLaunchFailure::InvalidArguments)?;
            if !metadata.is_file() || metadata.len() > MAX_SETTINGS_BYTES {
                return Err(ManagedLaunchFailure::InvalidArguments);
            }
            let mut bytes = Vec::new();
            file.take(MAX_SETTINGS_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ManagedLaunchFailure::InvalidArguments)?;
            if bytes.len() as u64 > MAX_SETTINGS_BYTES {
                return Err(ManagedLaunchFailure::InvalidArguments);
            }
            serde_json::from_slice(&bytes).map_err(|_| ManagedLaunchFailure::InvalidArguments)?
        }
    };
    let fields = document
        .as_object()
        .ok_or(ManagedLaunchFailure::InvalidArguments)?;
    if let Some(env) = fields.get("env") {
        let env = env
            .as_object()
            .ok_or(ManagedLaunchFailure::InvalidArguments)?;
        if env.values().any(|value| !value.is_string()) {
            return Err(ManagedLaunchFailure::InvalidArguments);
        }
    }
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_budget_accepts_the_boundary_and_rejects_oversized_input() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut bytes = b"{}".to_vec();
        bytes.resize(MAX_SETTINGS_BYTES as usize, b' ');
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            resolve_user_settings(path.as_os_str()).unwrap(),
            serde_json::json!({})
        );
        assert!(resolve_user_settings(OsStr::new(std::str::from_utf8(&bytes).unwrap())).is_ok());
        bytes.push(b' ');
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            resolve_user_settings(path.as_os_str()),
            Err(ManagedLaunchFailure::InvalidArguments)
        ));
        assert!(matches!(
            resolve_user_settings(OsStr::new(std::str::from_utf8(&bytes).unwrap())),
            Err(ManagedLaunchFailure::InvalidArguments)
        ));
    }

    #[test]
    fn settings_reject_directories_and_invalid_documents() {
        let directory = tempfile::tempdir().unwrap();
        assert!(resolve_user_settings(directory.path().as_os_str()).is_err());
        let path = directory.path().join("settings.json");
        for document in [
            "",
            "not-json",
            "[]",
            "null",
            "{\"env\":[]}",
            "{\"env\":{\"A\":1}}",
        ] {
            std::fs::write(&path, document).unwrap();
            assert!(matches!(
                resolve_user_settings(path.as_os_str()),
                Err(ManagedLaunchFailure::InvalidArguments)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn settings_reject_fifo_without_waiting_for_a_writer() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.fifo");
        nix::unistd::mkfifo(
            &path,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(matches!(
            resolve_user_settings(path.as_os_str()),
            Err(ManagedLaunchFailure::InvalidArguments)
        ));
    }
}
