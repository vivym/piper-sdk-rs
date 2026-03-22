fn current_dispatch_is_no_subscriber() -> bool {
    tracing::dispatcher::get_default(|dispatch| dispatch.is::<tracing::subscriber::NoSubscriber>())
}

#[test]
fn init_logger_installs_after_scoped_subscriber_has_dropped() {
    // SAFETY: This integration test runs in its own process and clears RUST_LOG
    // before any logger/subscriber initialization happens in this binary.
    unsafe {
        std::env::remove_var("RUST_LOG");
    }

    let guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt().with_test_writer().compact().finish(),
    );

    assert!(
        !current_dispatch_is_no_subscriber(),
        "scoped subscriber should be active before dropping the guard",
    );
    drop(guard);

    assert!(
        current_dispatch_is_no_subscriber(),
        "after dropping the guard, the thread should be back to the no-subscriber default",
    );

    piper_sdk::init_logger!();

    assert!(
        !current_dispatch_is_no_subscriber(),
        "SDK logger should install once the scoped subscriber is no longer active",
    );
    assert_eq!(
        log::max_level(),
        log::LevelFilter::Info,
        "default SDK logger init should install the INFO-level log bridge after scoped tracing is gone",
    );
}
