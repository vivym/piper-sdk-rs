use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Default)]
pub struct CollectorCancelToken {
    flag: Arc<AtomicBool>,
}

impl CollectorCancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn signal(&self) {
        self.flag.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    pub fn loop_signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}

pub fn install_ctrlc_handler(token: CollectorCancelToken) -> anyhow::Result<()> {
    ctrlc::set_handler(move || token.signal())?;
    Ok(())
}
