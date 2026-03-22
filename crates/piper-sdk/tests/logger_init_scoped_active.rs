fn current_dispatch_is_no_subscriber() -> bool {
    tracing::dispatcher::get_default(|dispatch| dispatch.is::<tracing::subscriber::NoSubscriber>())
}

#[test]
fn init_logger_is_noop_while_scoped_subscriber_is_active() {
    let guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt().with_test_writer().compact().finish(),
    );

    assert!(
        !current_dispatch_is_no_subscriber(),
        "scoped subscriber should be active for this test thread",
    );

    piper_sdk::init_logger!();

    assert!(
        !current_dispatch_is_no_subscriber(),
        "SDK logger should not replace an active scoped subscriber",
    );
    assert_eq!(
        log::max_level(),
        log::LevelFilter::Off,
        "SDK logger should not install a log bridge while scoped tracing is active",
    );

    drop(guard);

    assert!(
        current_dispatch_is_no_subscriber(),
        "dropping the scoped subscriber should restore the no-subscriber default",
    );
    assert_eq!(
        log::max_level(),
        log::LevelFilter::Off,
        "SDK logger no-op should not leave a residual global log bridge behind",
    );
}
