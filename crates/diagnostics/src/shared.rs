//! State shared by one runtime, its maintenance thread and its writer.
//!
//! The applied level, its source and its revision live in one atomic word so a reader never
//! observes a mixed tuple. The persisted view is the last settings file this process read,
//! cached so a status query never performs file I/O and so a failed read keeps the values
//! that were last known to work.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::context::Counters;
use crate::error::SubsystemReason;
use crate::level::{DiagnosticLevel, LevelSource};
use crate::writer::{WriterCounters, WriterHealth};

#[derive(Debug)]
pub(crate) struct Shared {
    pub counters: Arc<Counters>,
    pub writer_counters: Arc<WriterCounters>,
    pub writer_health: WriterHealth,
    /// One process-wide emit sequence, shared with the writer so every record of this
    /// process (including the writer's own exit record) sorts under one key.
    sequence: Arc<AtomicU64>,
    /// `(revision << 4) | (source << 2) | level`, applied as one store.
    level_state: AtomicU64,
    /// The last settings file this process read; `None` before the first read.
    persisted: Mutex<Option<PersistedSettings>>,
    pub shutdown: AtomicBool,
}

/// The persisted settings view as this process last saw it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PersistedSettings {
    pub revision: u64,
    pub level: DiagnosticLevel,
    /// The finding of the most recent read. A failed read keeps the last known revision
    /// and level and only records the error here.
    pub error: Option<SubsystemReason>,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            counters: Arc::new(Counters::default()),
            writer_counters: Arc::new(WriterCounters::default()),
            writer_health: WriterHealth::new(),
            sequence: Arc::new(AtomicU64::new(0)),
            level_state: AtomicU64::new(pack_level(
                DiagnosticLevel::runtime_default(),
                LevelSource::Default,
                0,
            )),
            persisted: Mutex::new(None),
            shutdown: AtomicBool::new(false),
        }
    }

    pub fn sequence(&self) -> Arc<AtomicU64> {
        self.sequence.clone()
    }

    pub fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }

    pub fn level_state(&self) -> (DiagnosticLevel, LevelSource, u64) {
        unpack_level(self.level_state.load(Ordering::SeqCst))
    }

    pub fn set_level_state(&self, level: DiagnosticLevel, source: LevelSource, revision: u64) {
        self.level_state
            .store(pack_level(level, source, revision), Ordering::SeqCst);
    }

    /// Record one settings read. A successful read replaces the known values; a failed one
    /// keeps the last known revision and level (the safe default before the first success)
    /// and only marks the error, so a transient failure never disguises them as a reset.
    pub fn set_persisted_read(
        &self,
        revision: u64,
        level: DiagnosticLevel,
        error: Option<SubsystemReason>,
    ) {
        let mut view = self.persisted.lock().expect("persisted lock");
        match (&mut *view, error) {
            (Some(current), Some(error)) => current.error = Some(error),
            (Some(current), None) => {
                current.revision = revision;
                current.level = level;
                current.error = None;
            }
            (None, error) => {
                *view = Some(PersistedSettings {
                    revision,
                    level,
                    error,
                })
            }
        }
    }

    pub fn persisted(&self) -> Option<PersistedSettings> {
        *self.persisted.lock().expect("persisted lock")
    }
}

fn pack_level(level: DiagnosticLevel, source: LevelSource, revision: u64) -> u64 {
    let level = match level {
        DiagnosticLevel::Error => 0u64,
        DiagnosticLevel::Warn => 1,
        DiagnosticLevel::Info => 2,
        DiagnosticLevel::Debug => 3,
    };
    let source = match source {
        LevelSource::Default => 0u64,
        LevelSource::Persisted => 1,
        LevelSource::SmokeOverride => 2,
    };
    (revision << 4) | (source << 2) | level
}

fn unpack_level(value: u64) -> (DiagnosticLevel, LevelSource, u64) {
    let level = match value & 0b11 {
        0 => DiagnosticLevel::Error,
        1 => DiagnosticLevel::Warn,
        2 => DiagnosticLevel::Info,
        _ => DiagnosticLevel::Debug,
    };
    let source = match (value >> 2) & 0b11 {
        0 => LevelSource::Default,
        1 => LevelSource::Persisted,
        _ => LevelSource::SmokeOverride,
    };
    (level, source, value >> 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_word_keeps_level_source_and_revision_consistent() {
        let shared = Shared::new();
        assert_eq!(
            shared.level_state(),
            (DiagnosticLevel::runtime_default(), LevelSource::Default, 0)
        );
        shared.set_level_state(DiagnosticLevel::Debug, LevelSource::Persisted, 41);
        assert_eq!(
            shared.level_state(),
            (DiagnosticLevel::Debug, LevelSource::Persisted, 41)
        );
    }

    /// A failed read must not reset the known values; the first read has no known value and
    /// reports the safe default with the error.
    #[test]
    fn a_failed_read_keeps_the_last_known_values_and_only_marks_the_error() {
        let shared = Shared::new();
        assert!(shared.persisted().is_none());
        shared.set_persisted_read(
            0,
            DiagnosticLevel::Info,
            Some(SubsystemReason::SettingsInvalid),
        );
        let first = shared.persisted().expect("first read");
        assert_eq!(first.revision, 0);
        assert_eq!(first.level, DiagnosticLevel::Info);
        assert_eq!(first.error, Some(SubsystemReason::SettingsInvalid));

        shared.set_persisted_read(3, DiagnosticLevel::Debug, None);
        let good = shared.persisted().expect("good read");
        assert_eq!(good.revision, 3);
        assert_eq!(good.level, DiagnosticLevel::Debug);
        assert_eq!(good.error, None);

        shared.set_persisted_read(
            0,
            DiagnosticLevel::Info,
            Some(SubsystemReason::SettingsInvalid),
        );
        let damaged = shared.persisted().expect("damaged read");
        assert_eq!(damaged.revision, 3, "the known revision is kept");
        assert_eq!(
            damaged.level,
            DiagnosticLevel::Debug,
            "the known level is kept"
        );
        assert_eq!(damaged.error, Some(SubsystemReason::SettingsInvalid));

        shared.set_persisted_read(3, DiagnosticLevel::Debug, None);
        let recovered = shared.persisted().expect("recovered read");
        assert_eq!(recovered.error, None);
        assert_eq!(recovered.revision, 3);
    }
}
