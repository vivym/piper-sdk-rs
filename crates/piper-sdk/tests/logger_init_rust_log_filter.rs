use tracing::Level;

#[test]
fn init_logger_applies_rust_log_filter() {
    // SAFETY: This integration test runs in its own process and sets RUST_LOG
    // before any logger/subscriber initialization happens in this binary.
    unsafe {
        std::env::set_var("RUST_LOG", "error");
    }

    piper_sdk::init_logger!();

    assert!(
        tracing::dispatcher::has_been_set(),
        "SDK logger should install a tracing subscriber when it owns initialization",
    );
    assert_eq!(
        log::max_level(),
        log::LevelFilter::Trace,
        "RUST_LOG branch should leave log filtering to the EnvFilter-backed subscriber",
    );
    assert!(
        !tracing::enabled!(Level::INFO),
        "RUST_LOG=error should disable info events",
    );
    assert!(
        !tracing::enabled!(Level::WARN),
        "RUST_LOG=error should disable warn events",
    );
    assert!(
        tracing::enabled!(Level::ERROR),
        "RUST_LOG=error should keep error events enabled",
    );
}
