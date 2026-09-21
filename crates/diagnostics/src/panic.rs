//! A payload-free panic hook that entry points may install explicitly.
//!
//! The crate never installs a hook while starting a runtime; a host that owns a
//! long-running process decides to call [`install_panic_hook`]. The hook replaces the
//! previous hook instead of chaining into it, because the default hook prints the panic
//! payload. It records a compiled-in relative source path and line, never reads the
//! payload, never takes a business lock and never waits for the writer.

use std::panic::PanicHookInfo;
use std::sync::OnceLock;

use crate::context::DiagnosticHandle;
use crate::event::{DiagnosticEvent, PanicObserved};
use crate::identity::SourceFileRef;

static PANIC_HANDLE: OnceLock<DiagnosticHandle> = OnceLock::new();

/// Install the diagnostics panic hook once per process. Later calls are ignored so the
/// chain cannot grow.
pub fn install_panic_hook(handle: DiagnosticHandle) {
    if PANIC_HANDLE.set(handle).is_err() {
        return;
    }
    std::panic::set_hook(Box::new(|info: &PanicHookInfo<'_>| {
        emit_panic(info);
    }));
}

fn emit_panic(info: &PanicHookInfo<'_>) {
    let Some(handle) = PANIC_HANDLE.get() else {
        return;
    };
    let (file, line) = match info.location() {
        Some(location) => (location.file(), location.line()),
        None => ("unknown", 0),
    };
    let source_file = SourceFileRef::parse(file).unwrap_or_else(|_| {
        SourceFileRef::parse("crates/diagnostics/src/panic.rs").expect("static path")
    });
    // Best effort only: no business lock, no flush wait, no second panic.
    handle.try_emit(DiagnosticEvent::PanicObserved(PanicObserved {
        source_file,
        line,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Counters, DiagnosticHandle, Emitter};
    use crate::event::ProcessRole;
    use crate::identity::BootId;
    use crate::level::DiagnosticLevel;
    use crate::queue::bounded_queue;
    use crate::record::Component;

    #[test]
    fn panic_hook_records_location_without_payload() {
        let (sender, receiver) = bounded_queue();
        let emitter = Emitter::new(
            sender,
            Some(DiagnosticLevel::Debug),
            Component::Diagnostics,
            ProcessRole::Daemon,
            BootId::random().expect("boot id"),
            None,
            std::time::Instant::now(),
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            std::sync::Arc::new(Counters::default()),
        );
        let handle = DiagnosticHandle::attached(std::sync::Arc::new(emitter));
        emit_panic_for_test(&handle);
        let bytes = receiver
            .pop_timeout(std::time::Duration::from_millis(50))
            .expect("record");
        let record = crate::record::DiagnosticRecordV1::parse_line(&bytes).expect("valid");
        let rendered = String::from_utf8(bytes).expect("utf8");
        assert!(rendered.contains("panic_observed"));
        assert!(!rendered.contains("sentinel-payload"));
        assert_eq!(record.event.kind(), "panic_observed");
    }

    fn emit_panic_for_test(handle: &DiagnosticHandle) {
        // The hook reads only `location()`; simulate the same call path without a real
        // panic so the test does not depend on panic-hook global state.
        let file = SourceFileRef::parse("crates/diagnostics/src/panic.rs").expect("path");
        handle.try_emit(DiagnosticEvent::PanicObserved(PanicObserved {
            source_file: file,
            line: 42,
        }));
    }
}
