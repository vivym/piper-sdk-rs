use std::sync::atomic::{AtomicUsize, Ordering};

static HOST_LOGGER: HostLogger = HostLogger;
static HOST_INFO_COUNT: AtomicUsize = AtomicUsize::new(0);

struct HostLogger;

impl log::Log for HostLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) && record.level() == log::Level::Info {
            HOST_INFO_COUNT.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn flush(&self) {}
}

#[test]
fn init_logger_is_noop_when_host_only_installed_log_logger() {
    log::set_logger(&HOST_LOGGER)
        .map(|()| log::set_max_level(log::LevelFilter::Info))
        .expect("host log logger should install for this test process");

    assert!(
        tracing::dispatcher::get_default(|dispatch| {
            dispatch.is::<tracing::subscriber::NoSubscriber>()
        }),
        "test process should start without a tracing subscriber",
    );

    piper_sdk::init_logger!();

    assert!(
        tracing::dispatcher::get_default(|dispatch| {
            dispatch.is::<tracing::subscriber::NoSubscriber>()
        }),
        "SDK logger should not install a tracing subscriber when host already owns the log path",
    );
    assert_eq!(
        log::max_level(),
        log::LevelFilter::Info,
        "SDK logger should not disturb the host logger max level",
    );

    log::info!("host logger should still receive logs after SDK init");
    assert_eq!(
        HOST_INFO_COUNT.load(Ordering::SeqCst),
        1,
        "host logger should remain active after SDK init_logger no-op",
    );
}
