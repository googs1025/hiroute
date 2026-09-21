//! Process-exit injection for the real-binary recovery suite. Absent in normal builds.
pub(crate) fn requested(boundary: &str) -> bool {
    #[cfg(feature = "integration-test-hooks")]
    {
        std::env::var("HIROUTE_TEST_PUBLICATION_CRASH_AT").as_deref() == Ok(boundary)
    }
    #[cfg(not(feature = "integration-test-hooks"))]
    {
        let _ = boundary;
        false
    }
}

pub(crate) fn crash(boundary: &str) {
    if requested(boundary) {
        std::process::exit(86);
    }
}
