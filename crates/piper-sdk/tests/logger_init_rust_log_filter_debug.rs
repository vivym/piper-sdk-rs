use tracing::Level;

#[test]
fn init_logger_tightens_log_max_level_for_debug_filter() {
    // SAFETY: This integration test runs in its own process and sets RUST_LOG
    // before any logger/subscriber initialization happens in this binary.
    unsafe {
        std::env::set_var("RUST_LOG", "debug");
    }

    piper_sdk::init_logger!();

    assert_eq!(
        log::max_level(),
        log::LevelFilter::Debug,
        "RUST_LOG=debug should keep the log facade fast path at DEBUG",
    );
    assert!(
        tracing::enabled!(Level::DEBUG),
        "RUST_LOG=debug should keep debug events enabled",
    );
    assert!(
        !tracing::enabled!(Level::TRACE),
        "RUST_LOG=debug should not widen filtering to TRACE",
    );
}
