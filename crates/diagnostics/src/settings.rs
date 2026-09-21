//! The single persisted diagnostic settings file.
//!
//! `settings.json` holds one level and one revision. Reading never writes; saving requires
//! the caller's expected revision, runs under `settings.lock`, writes a temporary file with
//! `create_new`, syncs, and atomically replaces the target. A damaged or unsafe existing
//! file is refused, never repaired in place.

use std::time::{Duration, Instant};

use serde::de::Error as _;
use serde::{Deserialize, Serialize};

use crate::correlation::{CorrelationKey, KEY_LEN};
use crate::files::{
    CORRELATION_KEY_FILE, FileSafetyError, PrivateDir, SETTINGS_FILE, SETTINGS_LOCK_FILE,
    SETTINGS_TEMP_FILE, VerifiedFile,
};
use crate::level::{DiagnosticLevel, LevelSource};

pub const SETTINGS_SCHEMA_V1: &str = "hiroute.diagnostic-settings/v1";
const SETTINGS_READ_LIMIT: u64 = 4096;
/// Bounded wait for the settings mutex; the caller never blocks a UI or request thread.
const LOCK_WAIT_BUDGET: Duration = Duration::from_millis(250);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsSchema;

impl Serialize for SettingsSchema {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(SETTINGS_SCHEMA_V1)
    }
}

impl<'de> Deserialize<'de> for SettingsSchema {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value == SETTINGS_SCHEMA_V1 {
            Ok(SettingsSchema)
        } else {
            Err(D::Error::custom("unsupported diagnostic settings schema"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticSettingsV1 {
    pub schema: SettingsSchema,
    pub revision: u64,
    pub level: DiagnosticLevel,
}

impl Default for DiagnosticSettingsV1 {
    fn default() -> Self {
        Self {
            schema: SettingsSchema,
            revision: 0,
            level: DiagnosticLevel::runtime_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("diagnostic settings path is unsafe")]
    UnsafePath,
    #[error("diagnostic settings file is unsafe")]
    UnsafeFile,
    #[error("diagnostic settings file is invalid")]
    Invalid,
    #[error("diagnostic settings revision does not match the caller's expectation")]
    Conflict,
    #[error("diagnostic settings revision is exhausted")]
    RevisionExhausted,
    #[error("diagnostic settings are being modified")]
    Busy,
    #[error("diagnostic settings could not be written")]
    Unwritable,
    #[error("diagnostic settings I/O failed")]
    Io,
    #[error("diagnostic settings are not supported on this platform")]
    UnsupportedPlatform,
}

impl From<FileSafetyError> for SettingsError {
    fn from(error: FileSafetyError) -> Self {
        match error {
            FileSafetyError::UnsafeDirectory => SettingsError::UnsafePath,
            FileSafetyError::UnsafeFile | FileSafetyError::IdentityChanged => {
                SettingsError::UnsafeFile
            }
            FileSafetyError::InvalidData => SettingsError::Invalid,
            FileSafetyError::UnsupportedPlatform => SettingsError::UnsupportedPlatform,
            _ => SettingsError::Io,
        }
    }
}

/// Result of reading the settings file: the effective values plus whether the file exists
/// and whether an error made the values fall back to defaults.
#[derive(Debug, Clone)]
pub struct SettingsSnapshot {
    pub settings: DiagnosticSettingsV1,
    pub present: bool,
    pub error: Option<SettingsError>,
}

impl SettingsSnapshot {
    pub fn level_source(&self) -> LevelSource {
        if self.present && self.error.is_none() {
            LevelSource::Persisted
        } else {
            LevelSource::Default
        }
    }
}

/// Reads and saves the one settings file under a verified diagnostics root.
pub struct SettingsStore {
    root: PrivateDir,
}

impl SettingsStore {
    pub fn new(root: PrivateDir) -> Self {
        Self { root }
    }

    /// Read the settings file. A missing file reads as revision 0 / Info without writing
    /// anything. A damaged file is reported as an error and read as the safe default; the
    /// file itself is left untouched.
    pub fn load(&self) -> SettingsSnapshot {
        match self.root.open_read(SETTINGS_FILE) {
            Ok(None) => SettingsSnapshot {
                settings: DiagnosticSettingsV1::default(),
                present: false,
                error: None,
            },
            Ok(Some(mut file)) => match read_settings(&mut file) {
                Ok(settings) => SettingsSnapshot {
                    settings,
                    present: true,
                    error: None,
                },
                Err(error) => SettingsSnapshot {
                    settings: DiagnosticSettingsV1::default(),
                    present: true,
                    error: Some(error),
                },
            },
            Err(error) => SettingsSnapshot {
                settings: DiagnosticSettingsV1::default(),
                present: false,
                error: Some(error.into()),
            },
        }
    }

    /// Save a new level. Requires the current revision, refuses damaged files, and never
    /// wraps the revision counter.
    pub fn save(
        &self,
        expected_revision: u64,
        level: DiagnosticLevel,
    ) -> Result<DiagnosticSettingsV1, SettingsError> {
        let _lock = self.acquire_lock()?;
        let current = match self.root.open_read(SETTINGS_FILE) {
            Ok(None) => DiagnosticSettingsV1::default(),
            Ok(Some(mut file)) => read_settings(&mut file)?,
            Err(error) => return Err(error.into()),
        };
        if current.revision != expected_revision {
            return Err(SettingsError::Conflict);
        }
        let next_revision = current
            .revision
            .checked_add(1)
            .ok_or(SettingsError::RevisionExhausted)?;
        let next = DiagnosticSettingsV1 {
            schema: SettingsSchema,
            revision: next_revision,
            level,
        };
        let encoded = serde_json::to_vec(&next).map_err(|_| SettingsError::Unwritable)?;
        let mut temp = match self.root.create_new(SETTINGS_TEMP_FILE) {
            Ok(file) => file,
            Err(FileSafetyError::AlreadyExists) => {
                // A leftover temp file from a crashed save is ours; remove only if it is a
                // verified private regular file, then create anew.
                let stale = self
                    .root
                    .open_read(SETTINGS_TEMP_FILE)
                    .map_err(SettingsError::from)?;
                if let Some(stale) = stale {
                    self.root
                        .remove_verified(SETTINGS_TEMP_FILE, stale.identity())
                        .map_err(SettingsError::from)?;
                }
                self.root
                    .create_new(SETTINGS_TEMP_FILE)
                    .map_err(SettingsError::from)?
            }
            Err(error) => return Err(error.into()),
        };
        temp.append(&encoded)
            .map_err(|_| SettingsError::Unwritable)?;
        temp.sync().map_err(|_| SettingsError::Unwritable)?;
        let identity = temp.identity();
        self.root
            .rename_verified(SETTINGS_TEMP_FILE, SETTINGS_FILE, identity)
            .map_err(SettingsError::from)?;
        self.root.sync().map_err(SettingsError::from)?;
        Ok(next)
    }

    /// Create or read the 32-byte correlation key while holding the settings mutex. An
    /// existing key is never replaced, even when it fails validation.
    pub fn ensure_correlation_key(&self) -> Result<CorrelationKey, SettingsError> {
        let _lock = self.acquire_lock()?;
        match self.root.open_read(CORRELATION_KEY_FILE) {
            Ok(Some(mut file)) => {
                if file.len() != KEY_LEN as u64 {
                    return Err(SettingsError::Invalid);
                }
                let bytes = file
                    .read_prefix(KEY_LEN as u64)
                    .map_err(SettingsError::from)?;
                let mut key = [0u8; KEY_LEN];
                key.copy_from_slice(&bytes);
                Ok(CorrelationKey::from_bytes(key))
            }
            Ok(None) => {
                let key = CorrelationKey::generate().map_err(|_| SettingsError::Io)?;
                let mut file = match self.root.create_new(CORRELATION_KEY_FILE) {
                    Ok(file) => file,
                    Err(FileSafetyError::AlreadyExists) => {
                        // Lost a creation race with the other role: read the winner's key.
                        return self.ensure_correlation_key_present();
                    }
                    Err(error) => return Err(error.into()),
                };
                file.append(key_bytes(&key))
                    .map_err(|_| SettingsError::Unwritable)?;
                file.sync().map_err(|_| SettingsError::Unwritable)?;
                Ok(key)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn ensure_correlation_key_present(&self) -> Result<CorrelationKey, SettingsError> {
        let mut file = self
            .root
            .open_read(CORRELATION_KEY_FILE)
            .map_err(SettingsError::from)?
            .ok_or(SettingsError::Invalid)?;
        if file.len() != KEY_LEN as u64 {
            return Err(SettingsError::Invalid);
        }
        let bytes = file
            .read_prefix(KEY_LEN as u64)
            .map_err(SettingsError::from)?;
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        Ok(CorrelationKey::from_bytes(key))
    }

    fn acquire_lock(&self) -> Result<VerifiedFile, SettingsError> {
        let lock = self
            .root
            .open_lock(SETTINGS_LOCK_FILE)
            .map_err(SettingsError::from)?;
        let deadline = Instant::now() + LOCK_WAIT_BUDGET;
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => return Ok(lock),
                Err(FileSafetyError::Locked) => {
                    if Instant::now() >= deadline {
                        return Err(SettingsError::Busy);
                    }
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

fn read_settings(file: &mut VerifiedFile) -> Result<DiagnosticSettingsV1, SettingsError> {
    if file.len() > SETTINGS_READ_LIMIT {
        return Err(SettingsError::Invalid);
    }
    let bytes = file
        .read_prefix(SETTINGS_READ_LIMIT)
        .map_err(SettingsError::from)?;
    serde_json::from_slice::<DiagnosticSettingsV1>(&bytes).map_err(|_| SettingsError::Invalid)
}

fn key_bytes(key: &CorrelationKey) -> &[u8] {
    key.as_bytes()
}

/// Map a settings failure to the fixed native error vocabulary.
pub fn stable_code(error: &SettingsError) -> crate::error::StableErrorCode {
    use crate::error::StableErrorCode as E;
    match error {
        SettingsError::UnsafePath | SettingsError::UnsafeFile => E::PathUnsafe,
        SettingsError::Invalid | SettingsError::RevisionExhausted => E::SettingsInvalid,
        SettingsError::Conflict => E::SettingsConflict,
        SettingsError::Busy => E::SettingsBusy,
        SettingsError::Unwritable | SettingsError::Io => E::SettingsUnwritable,
        SettingsError::UnsupportedPlatform => E::UnsupportedPlatform,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &PrivateDir) -> SettingsStore {
        SettingsStore::new(PrivateDir::open_existing(dir.path()).expect("open existing dir"))
    }

    fn temp_dir() -> PrivateDir {
        let path = crate::private_tempdir();
        let root = path.path().join("diagnostics");
        PrivateDir::open_or_create(&root).expect("create diagnostics root");
        std::mem::forget(path);
        PrivateDir::open_existing(&root).expect("reopen")
    }

    #[test]
    fn missing_file_reads_as_default_without_writing() {
        let dir = temp_dir();
        let store = store(&dir);
        let snapshot = store.load();
        assert!(!snapshot.present);
        assert_eq!(snapshot.settings.revision, 0);
        assert_eq!(snapshot.settings.level, DiagnosticLevel::runtime_default());
        assert!(snapshot.error.is_none());
        assert!(dir.list_names().expect("names").is_empty());
    }

    #[test]
    fn save_requires_expected_revision_and_increments() {
        let dir = temp_dir();
        let store = store(&dir);
        let saved = store.save(0, DiagnosticLevel::Debug).expect("save");
        assert_eq!(saved.revision, 1);
        assert_eq!(store.load().settings.level, DiagnosticLevel::Debug);
        assert_eq!(
            store.save(0, DiagnosticLevel::Warn),
            Err(SettingsError::Conflict)
        );
        assert_eq!(
            store.save(1, DiagnosticLevel::Warn).expect("save").revision,
            2
        );
        assert_eq!(store.load().settings.level, DiagnosticLevel::Warn);
    }

    #[test]
    fn damaged_file_is_reported_and_never_overwritten() {
        let dir = temp_dir();
        let mut file = dir.create_new(SETTINGS_FILE).expect("create");
        file.append(b"{\"schema\":\"hiroute.diagnostic-settings/v1\",\"revision\":1,")
            .expect("append");
        drop(file);
        let store = store(&dir);
        let snapshot = store.load();
        assert!(snapshot.present);
        assert_eq!(snapshot.error, Some(SettingsError::Invalid));
        assert_eq!(snapshot.settings.level, DiagnosticLevel::runtime_default());
        assert_eq!(
            store.save(0, DiagnosticLevel::Debug),
            Err(SettingsError::Invalid)
        );
        let mut reread = dir.open_read(SETTINGS_FILE).expect("open").expect("file");
        assert_eq!(
            reread.read_prefix(1024).expect("read"),
            b"{\"schema\":\"hiroute.diagnostic-settings/v1\",\"revision\":1,"
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let dir = temp_dir();
        let mut file = dir.create_new(SETTINGS_FILE).expect("create");
        file.append(
            br#"{"schema":"hiroute.diagnostic-settings/v1","revision":3,"level":"info","extra":true}"#,
        )
        .expect("append");
        drop(file);
        let store = store(&dir);
        assert_eq!(store.load().error, Some(SettingsError::Invalid));
    }

    #[test]
    fn correlation_key_is_created_once_and_kept() {
        let dir = temp_dir();
        let store = store(&dir);
        let first = store.ensure_correlation_key().expect("create key");
        let second = store.ensure_correlation_key().expect("read key");
        let probe = store.save(0, DiagnosticLevel::Info).expect("save");
        assert_eq!(probe.revision, 1);
        let token_first = first.token(crate::correlation::CorrelationDomain::Run, "id");
        let token_second = second.token(crate::correlation::CorrelationDomain::Run, "id");
        assert_eq!(token_first, token_second);
    }

    #[test]
    fn wrong_length_key_is_rejected_without_replacement() {
        let dir = temp_dir();
        let mut file = dir.create_new(CORRELATION_KEY_FILE).expect("create");
        file.append(b"short").expect("append");
        drop(file);
        let store = store(&dir);
        assert!(matches!(
            store.ensure_correlation_key(),
            Err(SettingsError::Invalid)
        ));
        let mut reread = dir
            .open_read(CORRELATION_KEY_FILE)
            .expect("open")
            .expect("file");
        assert_eq!(reread.read_prefix(64).expect("read"), b"short");
    }

    #[test]
    fn concurrent_settings_save_is_busy_not_blocking() {
        let dir = temp_dir();
        let store = store(&dir);
        let held = dir.open_lock(SETTINGS_LOCK_FILE).expect("lock file");
        held.try_lock_exclusive().expect("hold lock");
        let started = Instant::now();
        assert_eq!(
            store.save(0, DiagnosticLevel::Debug),
            Err(SettingsError::Busy)
        );
        assert!(started.elapsed() < Duration::from_millis(1000));
    }
}
