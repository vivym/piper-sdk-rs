use std::sync::{Arc, Barrier};

fn current_dispatch_is_no_subscriber() -> bool {
    tracing::dispatcher::get_default(|dispatch| dispatch.is::<tracing::subscriber::NoSubscriber>())
}

#[test]
fn init_logger_serializes_concurrent_sdk_initialization() {
    // SAFETY: This integration test runs in its own process and clears RUST_LOG
    // before any logger/subscriber initialization happens in this binary.
    unsafe {
        std::env::remove_var("RUST_LOG");
    }

    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();

    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            piper_sdk::init_logger!();
        }));
    }

    barrier.wait();

    for handle in handles {
        handle.join().expect("concurrent SDK logger initialization should not panic");
    }

    assert!(
        !current_dispatch_is_no_subscriber(),
        "concurrent SDK init should leave a live tracing subscriber installed",
    );
    assert_eq!(
        log::max_level(),
        log::LevelFilter::Info,
        "default concurrent init should converge to the SDK INFO-level log bridge",
    );

    tracing::info!("concurrent SDK logger init should leave tracing usable");
    log::info!("concurrent SDK logger init should leave log bridge usable");
}
