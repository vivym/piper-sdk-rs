#[test]
fn init_logger_is_idempotent() {
    piper_sdk::init_logger!();
    piper_sdk::init_logger!();

    tracing::info!("logger macro should tolerate repeated initialization");
    log::info!("log facade should remain usable after repeated initialization");
}
