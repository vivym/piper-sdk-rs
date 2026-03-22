#[test]
fn init_logger_is_noop_when_external_subscriber_exists() {
    tracing_subscriber::fmt()
        .with_test_writer()
        .compact()
        .try_init()
        .expect("external subscriber should install for this test process");

    piper_sdk::init_logger!();

    tracing::info!("logger macro should not panic after external subscriber setup");
    log::info!("log facade should remain usable with external subscriber");
}
