use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::sync::{LockResult, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::episode::manifest::{EpisodeStatus, WriterReportJson};
use crate::episode::wire::{self, SvsHeaderV1, SvsStepV1};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum WriterError {
    #[error("writer queue is full")]
    QueueFull,
    #[error("writer capacity must be non-zero")]
    InvalidCapacity,
    #[error("writer worker has stopped")]
    WorkerStopped,
    #[error("writer has already been finished")]
    AlreadyFinished,
    #[error("writer worker panicked")]
    WorkerPanicked,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Wire(#[from] wire::WireError),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeStopReason {
    MaxIterations,
    Cancelled,
    ReadFault,
    ControllerFault,
    CompensationFault,
    SubmissionFault,
    TelemetrySinkFault,
    RuntimeTransportFault,
    RuntimeManualFault,
    WriterFault,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriterStats {
    pub queue_full_stop_events: u64,
    pub queue_full_stop_duration_ms: u64,
    pub queue_full_events: u64,
    pub dropped_step_count: u64,
    pub first_queue_full_host_mono_us: Option<u64>,
    pub latest_queue_full_host_mono_us: Option<u64>,
    pub max_queue_depth: u32,
    pub final_queue_depth: u32,
    pub encoded_step_count: u64,
    pub last_step_index: Option<u64>,
    pub backpressure_threshold_tripped: bool,
    pub flush_failed: bool,
    pub flush_error: Option<String>,
}

struct AtomicWriterStats {
    queue_full_stop_events: AtomicU64,
    queue_full_stop_duration_ms: AtomicU64,
    queue_full_events: AtomicU64,
    dropped_step_count: AtomicU64,
    first_queue_full_host_mono_us: AtomicU64,
    latest_queue_full_host_mono_us: AtomicU64,
    max_queue_depth: AtomicU32,
    final_queue_depth: AtomicU32,
    encoded_step_count: AtomicU64,
    last_step_index: AtomicU64,
    enqueue_terminal_fault: AtomicBool,
    backpressure_threshold_tripped: AtomicBool,
    flush_failed: AtomicBool,
    flush_error: Mutex<Option<String>>,
    #[cfg(test)]
    lock_probe: Mutex<()>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterFlushSummary {
    pub path: PathBuf,
    pub step_count: u64,
    pub last_step_index: Option<u64>,
}

pub struct EpisodeWriter {
    sender: Option<Sender<SvsStepV1>>,
    pause_worker: Arc<AtomicBool>,
    stats: Arc<AtomicWriterStats>,
    worker: Option<JoinHandle<Result<WriterFlushSummary, WriterError>>>,
    #[cfg(test)]
    startup_thread_id: Arc<Mutex<Option<std::thread::ThreadId>>>,
}

#[derive(Debug, Clone)]
pub struct WriterBackpressureMonitor {
    event_threshold: u64,
    duration_threshold: Duration,
    queue_full_events: u64,
    first_queue_full: Option<Instant>,
    latest_queue_full: Option<Instant>,
    tripped: bool,
}

impl EpisodeWriter {
    pub fn new(
        output_dir: impl AsRef<Path>,
        header: SvsHeaderV1,
        capacity: usize,
    ) -> Result<Self, WriterError> {
        Self::start(output_dir.as_ref(), header, capacity, false)
    }

    pub fn for_test_with_capacity(
        output_dir: impl AsRef<Path>,
        capacity: usize,
    ) -> Result<Self, WriterError> {
        Self::start(
            output_dir.as_ref(),
            SvsHeaderV1::for_test("20260428T010203Z-test-000000000000"),
            capacity,
            true,
        )
    }

    pub fn try_enqueue(&self, step: SvsStepV1) -> Result<(), WriterError> {
        let Some(sender) = &self.sender else {
            return Err(WriterError::AlreadyFinished);
        };

        if self.stats.enqueue_terminal_fault.load(Ordering::Relaxed) {
            return Err(WriterError::QueueFull);
        }

        let depth_before = sender.len();
        match sender.try_send(step) {
            Ok(()) => {
                let observed_depth = depth_before.saturating_add(1).max(sender.len());
                self.update_queue_depth(observed_depth);
                Ok(())
            },
            Err(TrySendError::Full(_step)) => {
                self.record_queue_full(sender.len());
                Err(WriterError::QueueFull)
            },
            Err(TrySendError::Disconnected(_step)) => Err(WriterError::WorkerStopped),
        }
    }

    pub fn stats(&self) -> WriterStats {
        self.stats.snapshot()
    }

    pub fn pause_worker_for_test(&mut self) {
        self.pause_worker.store(true, Ordering::Release);
    }

    #[allow(dead_code)]
    pub fn resume_worker_for_test(&mut self) {
        self.pause_worker.store(false, Ordering::Release);
    }

    #[cfg(test)]
    pub fn for_test_with_startup_thread_probe(
        output_dir: impl AsRef<Path>,
        capacity: usize,
    ) -> Result<Self, WriterError> {
        Self::start(
            output_dir.as_ref(),
            SvsHeaderV1::for_test("20260428T010203Z-test-000000000000"),
            capacity,
            true,
        )
    }

    #[cfg(test)]
    pub fn startup_thread_id_for_test(&self) -> Option<std::thread::ThreadId> {
        *self.startup_thread_id.lock().expect("startup thread id lock poisoned")
    }

    pub fn finish(mut self) -> Result<WriterFlushSummary, WriterError> {
        self.pause_worker.store(false, Ordering::Release);
        drop(self.sender.take());
        let Some(worker) = self.worker.take() else {
            return Err(WriterError::AlreadyFinished);
        };
        join_worker(worker, &self.stats)
    }

    fn start(
        output_dir: &Path,
        header: SvsHeaderV1,
        capacity: usize,
        start_paused: bool,
    ) -> Result<Self, WriterError> {
        if capacity == 0 {
            return Err(WriterError::InvalidCapacity);
        }

        let (sender, receiver) = crossbeam_channel::bounded(capacity);
        let pause_worker = Arc::new(AtomicBool::new(start_paused));
        let stats = Arc::new(AtomicWriterStats::default());
        let (startup_tx, startup_rx) = std::sync::mpsc::sync_channel(1);
        let output_dir = output_dir.to_path_buf();
        #[cfg(test)]
        let startup_thread_id = Arc::new(Mutex::new(None));

        let worker_pause = Arc::clone(&pause_worker);
        let worker_stats = Arc::clone(&stats);
        #[cfg(test)]
        let worker_startup_thread_id = Arc::clone(&startup_thread_id);
        let worker = thread::spawn(move || {
            run_worker(
                receiver,
                worker_pause,
                worker_stats,
                header,
                output_dir,
                startup_tx,
                #[cfg(test)]
                worker_startup_thread_id,
            )
        });

        match startup_rx.recv() {
            Ok(Ok(())) => {},
            Ok(Err(err)) => {
                let _ = worker.join();
                return Err(err);
            },
            Err(_closed) => {
                let _ = worker.join();
                return Err(WriterError::WorkerStopped);
            },
        }

        Ok(Self {
            sender: Some(sender),
            pause_worker,
            stats,
            worker: Some(worker),
            #[cfg(test)]
            startup_thread_id,
        })
    }

    fn update_queue_depth(&self, depth: usize) {
        let depth = saturating_u32(depth);
        self.stats.max_queue_depth.fetch_max(depth, Ordering::Relaxed);
        self.stats.final_queue_depth.store(depth, Ordering::Relaxed);
    }

    fn record_queue_full(&self, depth: usize) {
        let now_us = piper_driver::heartbeat::monotonic_micros().max(1);
        let depth = saturating_u32(depth);
        self.stats.enqueue_terminal_fault.store(true, Ordering::Relaxed);
        self.stats.queue_full_events.fetch_add(1, Ordering::Relaxed);
        self.stats.dropped_step_count.fetch_add(1, Ordering::Relaxed);
        let _ = self.stats.first_queue_full_host_mono_us.compare_exchange(
            0,
            now_us,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        self.stats.latest_queue_full_host_mono_us.store(now_us, Ordering::Relaxed);
        self.stats.max_queue_depth.fetch_max(depth, Ordering::Relaxed);
        self.stats.final_queue_depth.store(depth, Ordering::Relaxed);
    }
}

impl Drop for EpisodeWriter {
    fn drop(&mut self) {
        self.pause_worker.store(false, Ordering::Release);
        drop(self.sender.take());
        if let Some(worker) = self.worker.take() {
            let _ = join_worker(worker, &self.stats);
        }
    }
}

impl WriterStats {
    pub fn with_backpressure_thresholds(monitor: &WriterBackpressureMonitor) -> Self {
        let mut stats = Self::default();
        stats.record_backpressure_thresholds(monitor);
        stats
    }

    pub fn record_backpressure_thresholds(&mut self, monitor: &WriterBackpressureMonitor) {
        self.queue_full_stop_events = monitor.queue_full_stop_events();
        self.queue_full_stop_duration_ms = monitor.queue_full_stop_duration_ms();
    }

    pub fn has_fault(&self) -> bool {
        self.queue_full_events > 0
            || self.dropped_step_count > 0
            || self.backpressure_threshold_tripped
            || self.flush_failed
    }

    pub fn record_backpressure_threshold_tripped(&mut self) {
        self.backpressure_threshold_tripped = true;
    }
}

impl Default for AtomicWriterStats {
    fn default() -> Self {
        Self {
            queue_full_stop_events: AtomicU64::new(0),
            queue_full_stop_duration_ms: AtomicU64::new(0),
            queue_full_events: AtomicU64::new(0),
            dropped_step_count: AtomicU64::new(0),
            first_queue_full_host_mono_us: AtomicU64::new(0),
            latest_queue_full_host_mono_us: AtomicU64::new(0),
            max_queue_depth: AtomicU32::new(0),
            final_queue_depth: AtomicU32::new(0),
            encoded_step_count: AtomicU64::new(0),
            last_step_index: AtomicU64::new(u64::MAX),
            enqueue_terminal_fault: AtomicBool::new(false),
            backpressure_threshold_tripped: AtomicBool::new(false),
            flush_failed: AtomicBool::new(false),
            flush_error: Mutex::new(None),
            #[cfg(test)]
            lock_probe: Mutex::new(()),
        }
    }
}

impl AtomicWriterStats {
    fn snapshot(&self) -> WriterStats {
        let first_queue_full = self.first_queue_full_host_mono_us.load(Ordering::Relaxed);
        let latest_queue_full = self.latest_queue_full_host_mono_us.load(Ordering::Relaxed);
        let last_step_index = self.last_step_index.load(Ordering::Relaxed);

        WriterStats {
            queue_full_stop_events: self.queue_full_stop_events.load(Ordering::Relaxed),
            queue_full_stop_duration_ms: self.queue_full_stop_duration_ms.load(Ordering::Relaxed),
            queue_full_events: self.queue_full_events.load(Ordering::Relaxed),
            dropped_step_count: self.dropped_step_count.load(Ordering::Relaxed),
            first_queue_full_host_mono_us: nonzero_to_option(first_queue_full),
            latest_queue_full_host_mono_us: nonzero_to_option(latest_queue_full),
            max_queue_depth: self.max_queue_depth.load(Ordering::Relaxed),
            final_queue_depth: self.final_queue_depth.load(Ordering::Relaxed),
            encoded_step_count: self.encoded_step_count.load(Ordering::Relaxed),
            last_step_index: if last_step_index == u64::MAX {
                None
            } else {
                Some(last_step_index)
            },
            backpressure_threshold_tripped: self
                .backpressure_threshold_tripped
                .load(Ordering::Relaxed),
            flush_failed: self.flush_failed.load(Ordering::Relaxed),
            flush_error: self.flush_error.lock().expect("writer flush error lock poisoned").clone(),
        }
    }

    #[cfg(test)]
    fn lock(&self) -> LockResult<MutexGuard<'_, ()>> {
        self.lock_probe.lock()
    }
}

impl From<&WriterStats> for WriterReportJson {
    fn from(stats: &WriterStats) -> Self {
        Self {
            queue_full_stop_events: stats.queue_full_stop_events,
            queue_full_stop_duration_ms: stats.queue_full_stop_duration_ms,
            queue_full_events: stats.queue_full_events,
            dropped_step_count: stats.dropped_step_count,
            first_queue_full_host_mono_us: stats.first_queue_full_host_mono_us,
            latest_queue_full_host_mono_us: stats.latest_queue_full_host_mono_us,
            max_queue_depth: stats.max_queue_depth,
            final_queue_depth: stats.final_queue_depth,
            backpressure_threshold_tripped: stats.backpressure_threshold_tripped,
            flush_failed: stats.flush_failed,
        }
    }
}

impl WriterBackpressureMonitor {
    pub fn new(event_threshold: u64, duration_threshold: Duration) -> Self {
        Self {
            event_threshold,
            duration_threshold,
            queue_full_events: 0,
            first_queue_full: None,
            latest_queue_full: None,
            tripped: false,
        }
    }

    pub fn record_queue_full(&mut self, now: Instant) -> bool {
        self.queue_full_events = self.queue_full_events.saturating_add(1);
        self.first_queue_full.get_or_insert(now);
        self.latest_queue_full = Some(now);

        if self.event_threshold == 0 || self.queue_full_events >= self.event_threshold {
            self.tripped = true;
        }

        if let Some(first) = self.first_queue_full {
            let elapsed = now.checked_duration_since(first).unwrap_or(Duration::ZERO);
            if elapsed >= self.duration_threshold {
                self.tripped = true;
            }
        }

        self.tripped
    }

    pub fn tripped(&self) -> bool {
        self.tripped
    }

    pub fn queue_full_events(&self) -> u64 {
        self.queue_full_events
    }

    pub fn queue_full_stop_events(&self) -> u64 {
        self.event_threshold
    }

    pub fn queue_full_stop_duration_ms(&self) -> u64 {
        self.duration_threshold.as_millis().min(u128::from(u64::MAX)) as u64
    }

    pub fn first_queue_full(&self) -> Option<Instant> {
        self.first_queue_full
    }

    pub fn latest_queue_full(&self) -> Option<Instant> {
        self.latest_queue_full
    }
}

pub fn final_status_for_writer_stats(
    stop_reason: EpisodeStopReason,
    stats: &WriterStats,
) -> EpisodeStatus {
    if stats.has_fault() {
        return EpisodeStatus::Faulted;
    }

    match stop_reason {
        EpisodeStopReason::MaxIterations => EpisodeStatus::Complete,
        EpisodeStopReason::Cancelled => EpisodeStatus::Cancelled,
        EpisodeStopReason::ReadFault
        | EpisodeStopReason::ControllerFault
        | EpisodeStopReason::CompensationFault
        | EpisodeStopReason::SubmissionFault
        | EpisodeStopReason::TelemetrySinkFault
        | EpisodeStopReason::RuntimeTransportFault
        | EpisodeStopReason::RuntimeManualFault
        | EpisodeStopReason::WriterFault => EpisodeStatus::Faulted,
    }
}

fn run_worker(
    receiver: Receiver<SvsStepV1>,
    pause_worker: Arc<AtomicBool>,
    stats: Arc<AtomicWriterStats>,
    header: SvsHeaderV1,
    output_dir: PathBuf,
    startup_tx: SyncSender<Result<(), WriterError>>,
    #[cfg(test)] startup_thread_id: Arc<Mutex<Option<std::thread::ThreadId>>>,
) -> Result<WriterFlushSummary, WriterError> {
    #[cfg(test)]
    {
        *startup_thread_id.lock().expect("startup thread id lock poisoned") =
            Some(thread::current().id());
    }

    let (file, temp_path, final_path) = match prepare_steps_file(&output_dir, &header) {
        Ok(paths) => {
            if startup_tx.send(Ok(())).is_err() {
                return Err(WriterError::WorkerStopped);
            }
            paths
        },
        Err(err) => {
            if startup_tx.send(Err(err)).is_err() {
                return Err(WriterError::WorkerStopped);
            }
            return Err(WriterError::WorkerStopped);
        },
    };

    let result = run_worker_inner(
        receiver,
        pause_worker,
        Arc::clone(&stats),
        file,
        &temp_path,
        &final_path,
    );
    if let Err(err) = &result {
        mark_flush_failed(&stats, err.to_string());
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn run_worker_inner(
    receiver: Receiver<SvsStepV1>,
    pause_worker: Arc<AtomicBool>,
    stats: Arc<AtomicWriterStats>,
    mut file: File,
    temp_path: &Path,
    final_path: &Path,
) -> Result<WriterFlushSummary, WriterError> {
    let mut step_count = 0_u64;
    let mut last_step_index = None;
    loop {
        if pause_worker.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
            continue;
        }

        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(step) => {
                if step.step_index != step_count {
                    return Err(wire::WireError::NonSequentialStepIndex {
                        expected: step_count,
                        actual: step.step_index,
                    }
                    .into());
                }
                wire::write_step(&mut file, &step)?;
                step_count = step_count.saturating_add(1);
                last_step_index = Some(step.step_index);
                stats.encoded_step_count.store(step_count, Ordering::Relaxed);
                stats.last_step_index.store(step.step_index, Ordering::Relaxed);
                stats.final_queue_depth.store(saturating_u32(receiver.len()), Ordering::Relaxed);
            },
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    file.flush()?;
    file.sync_all()?;
    drop(file);
    persist_temp_no_overwrite(temp_path, final_path)?;
    fsync_parent(final_path)?;

    Ok(WriterFlushSummary {
        path: final_path.to_path_buf(),
        step_count,
        last_step_index,
    })
}

fn join_worker(
    worker: JoinHandle<Result<WriterFlushSummary, WriterError>>,
    stats: &Arc<AtomicWriterStats>,
) -> Result<WriterFlushSummary, WriterError> {
    match worker.join() {
        Ok(result) => result,
        Err(_panic) => {
            mark_flush_failed(stats, "writer worker panicked".to_string());
            Err(WriterError::WorkerPanicked)
        },
    }
}

fn mark_flush_failed(stats: &Arc<AtomicWriterStats>, error: String) {
    stats.flush_failed.store(true, Ordering::Relaxed);
    *stats.flush_error.lock().expect("writer flush error lock poisoned") = Some(error);
}

fn prepare_steps_file(
    output_dir: &Path,
    header: &SvsHeaderV1,
) -> Result<(File, PathBuf, PathBuf), WriterError> {
    header.validate()?;
    std::fs::create_dir_all(output_dir)?;

    let final_path = output_dir.join("steps.bin");
    if final_path.try_exists()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "steps.bin already exists",
        )
        .into());
    }

    let temp_path = output_dir.join(format!(
        ".steps.bin.{}.{}.tmp",
        std::process::id(),
        TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new().write(true).create_new(true).open(&temp_path)?;

    if let Err(err) = wire::write_header(&mut file, header) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err.into());
    }
    file.flush()?;

    Ok((file, temp_path, final_path))
}

fn persist_temp_no_overwrite(temp_path: &Path, final_path: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(temp_path, final_path) {
        Ok(()) => {
            std::fs::remove_file(temp_path)?;
            Ok(())
        },
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn fsync_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let dir = File::open(parent)?;
        dir.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn fsync_parent(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn saturating_u32(value: usize) -> u32 {
    value.min(u32::MAX as usize) as u32
}

fn nonzero_to_option(value: u64) -> Option<u64> {
    if value == 0 { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn queue_full_event_prevents_complete_status() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = EpisodeWriter::for_test_with_capacity(dir.path(), 1).unwrap();
        writer.pause_worker_for_test();

        writer.try_enqueue(SvsStepV1::for_test(0)).unwrap();
        assert!(matches!(
            writer.try_enqueue(SvsStepV1::for_test(1)),
            Err(WriterError::QueueFull)
        ));

        let stats = writer.stats();
        assert_eq!(stats.queue_full_events, 1);
        assert_eq!(stats.dropped_step_count, 1);
        assert_eq!(
            final_status_for_writer_stats(EpisodeStopReason::MaxIterations, &stats),
            EpisodeStatus::Faulted
        );
    }

    #[test]
    fn try_enqueue_does_not_wait_for_stats_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = EpisodeWriter::for_test_with_capacity(dir.path(), 2).unwrap();
        writer.pause_worker_for_test();
        let stats = Arc::clone(&writer.stats);
        let stats_guard = stats.lock().unwrap();
        let (tx, rx) = mpsc::channel();

        let recv_result = thread::scope(|scope| {
            let handle = scope.spawn(|| {
                tx.send(writer.try_enqueue(SvsStepV1::for_test(0))).unwrap();
            });
            let recv_result = rx.recv_timeout(Duration::from_millis(50));
            drop(stats_guard);
            handle.join().unwrap();
            recv_result
        });

        assert!(matches!(recv_result, Ok(Ok(()))));
    }

    #[test]
    fn startup_errors_are_reported_before_returning_writer() {
        let dir = tempfile::tempdir().unwrap();
        let mut header = SvsHeaderV1::for_test("20260428T010203Z-test-000000000000");
        header.reserved = 1;

        assert!(matches!(
            EpisodeWriter::new(dir.path(), header, 1),
            Err(WriterError::Wire(_))
        ));
    }

    #[test]
    fn writer_refuses_to_start_when_steps_file_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("steps.bin"), b"existing").unwrap();

        assert!(matches!(
            EpisodeWriter::for_test_with_capacity(dir.path(), 1),
            Err(WriterError::Io(_))
        ));
    }

    #[test]
    fn finish_does_not_overwrite_steps_file_created_after_start() {
        let dir = tempfile::tempdir().unwrap();
        let writer = EpisodeWriter::for_test_with_capacity(dir.path(), 1).unwrap();
        std::fs::write(dir.path().join("steps.bin"), b"existing").unwrap();

        assert!(matches!(writer.finish(), Err(WriterError::Io(_))));
        assert_eq!(
            std::fs::read(dir.path().join("steps.bin")).unwrap(),
            b"existing"
        );
    }

    #[test]
    fn first_queue_full_is_terminal_but_preserves_prefix_steps() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = EpisodeWriter::for_test_with_capacity(dir.path(), 1).unwrap();
        writer.pause_worker_for_test();

        writer.try_enqueue(SvsStepV1::for_test(0)).unwrap();
        assert!(matches!(
            writer.try_enqueue(SvsStepV1::for_test(1)),
            Err(WriterError::QueueFull)
        ));
        writer.resume_worker_for_test();
        std::thread::sleep(Duration::from_millis(20));
        assert!(matches!(
            writer.try_enqueue(SvsStepV1::for_test(2)),
            Err(WriterError::QueueFull)
        ));

        let writer_stats = writer.stats();
        let summary = writer.finish().unwrap();
        let decoded = crate::episode::wire::read_steps_file(&summary.path).unwrap();
        assert_eq!(summary.step_count, 1);
        assert_eq!(summary.last_step_index, Some(0));
        assert_eq!(decoded.summary.step_count, 1);
        assert_eq!(writer_stats.queue_full_events, 1);
        assert_eq!(writer_stats.dropped_step_count, 1);
    }

    #[test]
    fn startup_file_setup_runs_on_worker_thread() {
        let dir = tempfile::tempdir().unwrap();
        let caller_thread = thread::current().id();
        let writer = EpisodeWriter::for_test_with_startup_thread_probe(dir.path(), 1).unwrap();

        assert_ne!(writer.startup_thread_id_for_test().unwrap(), caller_thread);
    }

    #[test]
    fn finish_rejects_nonsequential_step_indexes_without_final_file() {
        let dir = tempfile::tempdir().unwrap();
        let writer = EpisodeWriter::for_test_with_capacity(dir.path(), 2).unwrap();

        writer.try_enqueue(SvsStepV1::for_test(0)).unwrap();
        writer.try_enqueue(SvsStepV1::for_test(2)).unwrap();

        assert!(matches!(writer.finish(), Err(WriterError::Wire(_))));
        assert!(!dir.path().join("steps.bin").exists());
    }

    #[test]
    fn hard_link_failure_does_not_create_final_file() {
        let dir = tempfile::tempdir().unwrap();
        let temp_dir = dir.path().join("temp-as-directory");
        let final_path = dir.path().join("steps.bin");
        std::fs::create_dir(&temp_dir).unwrap();

        assert!(persist_temp_no_overwrite(&temp_dir, &final_path).is_err());
        assert!(!final_path.exists());
        assert!(temp_dir.exists());
    }

    #[test]
    fn queue_full_timestamps_use_driver_host_monotonic_clock() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = EpisodeWriter::for_test_with_capacity(dir.path(), 1).unwrap();
        writer.pause_worker_for_test();

        writer.try_enqueue(SvsStepV1::for_test(0)).unwrap();
        let before = piper_driver::heartbeat::monotonic_micros();
        assert!(matches!(
            writer.try_enqueue(SvsStepV1::for_test(1)),
            Err(WriterError::QueueFull)
        ));

        let stats = writer.stats();
        let first = stats.first_queue_full_host_mono_us.unwrap();
        let latest = stats.latest_queue_full_host_mono_us.unwrap();
        assert!(first >= before);
        assert!(latest >= before);
        assert!(first > 0);
        assert!(latest > 0);
    }

    #[test]
    fn sustained_queue_full_trips_stop_threshold() {
        let mut monitor = WriterBackpressureMonitor::new(2, Duration::from_millis(100));
        assert!(!monitor.record_queue_full(Instant::now()));
        assert!(monitor.record_queue_full(Instant::now() + Duration::from_millis(50)));

        let mut duration_monitor = WriterBackpressureMonitor::new(10, Duration::from_millis(100));
        let start = Instant::now();
        assert!(!duration_monitor.record_queue_full(start));
        assert!(duration_monitor.record_queue_full(start + Duration::from_millis(101)));
    }

    #[test]
    fn backpressure_thresholds_are_recorded_in_stats_and_report() {
        let monitor = WriterBackpressureMonitor::new(7, Duration::from_millis(250));
        let stats = WriterStats::with_backpressure_thresholds(&monitor);
        let report = WriterReportJson::from(&stats);

        assert_eq!(monitor.queue_full_stop_events(), 7);
        assert_eq!(monitor.queue_full_stop_duration_ms(), 250);
        assert_eq!(stats.queue_full_stop_events, 7);
        assert_eq!(stats.queue_full_stop_duration_ms, 250);
        assert_eq!(report.queue_full_stop_events, 7);
        assert_eq!(report.queue_full_stop_duration_ms, 250);
    }
}
