#[test]
fn init_logger_uses_env_filter_hint_for_target_directives() {
    // SAFETY: This integration test runs in its own process and sets RUST_LOG
    // before any logger/subscriber initialization happens in this binary.
    unsafe {
        std::env::set_var("RUST_LOG", "piper_sdk=debug");
    }

    piper_sdk::init_logger!();

    assert_eq!(
        log::max_level(),
        log::LevelFilter::Debug,
        "targeted RUST_LOG directives should still tighten the global log fast path",
    );
}
