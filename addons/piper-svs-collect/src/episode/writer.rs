use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterFlushSummary {
    pub path: PathBuf,
    pub step_count: u64,
    pub last_step_index: Option<u64>,
}

pub struct EpisodeWriter {
    sender: Option<Sender<SvsStepV1>>,
    pause_worker: Arc<AtomicBool>,
    stats: Arc<Mutex<WriterStats>>,
    worker: Option<JoinHandle<Result<WriterFlushSummary, WriterError>>>,
    monotonic_origin: Instant,
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
        self.stats.lock().expect("writer stats lock poisoned").clone()
    }

    pub fn pause_worker_for_test(&mut self) {
        self.pause_worker.store(true, Ordering::Release);
    }

    #[allow(dead_code)]
    pub fn resume_worker_for_test(&mut self) {
        self.pause_worker.store(false, Ordering::Release);
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

        std::fs::create_dir_all(output_dir)?;
        let final_path = output_dir.join("steps.bin");
        let temp_path = output_dir.join(format!(
            ".steps.bin.{}.{}.tmp",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let (sender, receiver) = crossbeam_channel::bounded(capacity);
        let pause_worker = Arc::new(AtomicBool::new(start_paused));
        let stats = Arc::new(Mutex::new(WriterStats::default()));

        let worker_pause = Arc::clone(&pause_worker);
        let worker_stats = Arc::clone(&stats);
        let worker = thread::spawn(move || {
            run_worker(
                receiver,
                worker_pause,
                worker_stats,
                header,
                temp_path,
                final_path,
            )
        });

        Ok(Self {
            sender: Some(sender),
            pause_worker,
            stats,
            worker: Some(worker),
            monotonic_origin: Instant::now(),
        })
    }

    fn update_queue_depth(&self, depth: usize) {
        let depth = saturating_u32(depth);
        let mut stats = self.stats.lock().expect("writer stats lock poisoned");
        stats.max_queue_depth = stats.max_queue_depth.max(depth);
        stats.final_queue_depth = depth;
    }

    fn record_queue_full(&self, depth: usize) {
        let now_us = saturating_u64(self.monotonic_origin.elapsed().as_micros());
        let depth = saturating_u32(depth);
        let mut stats = self.stats.lock().expect("writer stats lock poisoned");
        stats.queue_full_events = stats.queue_full_events.saturating_add(1);
        stats.dropped_step_count = stats.dropped_step_count.saturating_add(1);
        stats.first_queue_full_host_mono_us.get_or_insert(now_us);
        stats.latest_queue_full_host_mono_us = Some(now_us);
        stats.max_queue_depth = stats.max_queue_depth.max(depth);
        stats.final_queue_depth = depth;
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

impl From<&WriterStats> for WriterReportJson {
    fn from(stats: &WriterStats) -> Self {
        Self {
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
    stats: Arc<Mutex<WriterStats>>,
    header: SvsHeaderV1,
    temp_path: PathBuf,
    final_path: PathBuf,
) -> Result<WriterFlushSummary, WriterError> {
    let result = run_worker_inner(
        receiver,
        pause_worker,
        Arc::clone(&stats),
        header,
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
    stats: Arc<Mutex<WriterStats>>,
    header: SvsHeaderV1,
    temp_path: &Path,
    final_path: &Path,
) -> Result<WriterFlushSummary, WriterError> {
    if let Some(parent) = temp_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new().write(true).create_new(true).open(temp_path)?;
    wire::write_header(&mut file, &header)?;

    let mut step_count = 0_u64;
    let mut last_step_index = None;
    loop {
        if pause_worker.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
            continue;
        }

        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(step) => {
                wire::write_step(&mut file, &step)?;
                step_count = step_count.saturating_add(1);
                last_step_index = Some(step.step_index);
                let mut stats = stats.lock().expect("writer stats lock poisoned");
                stats.encoded_step_count = step_count;
                stats.last_step_index = last_step_index;
                stats.final_queue_depth = saturating_u32(receiver.len());
            },
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    file.flush()?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp_path, final_path)?;
    fsync_parent(final_path)?;

    Ok(WriterFlushSummary {
        path: final_path.to_path_buf(),
        step_count,
        last_step_index,
    })
}

fn join_worker(
    worker: JoinHandle<Result<WriterFlushSummary, WriterError>>,
    stats: &Arc<Mutex<WriterStats>>,
) -> Result<WriterFlushSummary, WriterError> {
    match worker.join() {
        Ok(result) => result,
        Err(_panic) => {
            mark_flush_failed(stats, "writer worker panicked".to_string());
            Err(WriterError::WorkerPanicked)
        },
    }
}

fn mark_flush_failed(stats: &Arc<Mutex<WriterStats>>, error: String) {
    let mut stats = stats.lock().expect("writer stats lock poisoned");
    stats.flush_failed = true;
    stats.flush_error = Some(error);
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

fn saturating_u64(value: u128) -> u64 {
    value.min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
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
    fn sustained_queue_full_trips_stop_threshold() {
        let mut monitor = WriterBackpressureMonitor::new(2, Duration::from_millis(100));
        assert!(!monitor.record_queue_full(Instant::now()));
        assert!(monitor.record_queue_full(Instant::now() + Duration::from_millis(50)));

        let mut duration_monitor = WriterBackpressureMonitor::new(10, Duration::from_millis(100));
        let start = Instant::now();
        assert!(!duration_monitor.record_queue_full(start));
        assert!(duration_monitor.record_queue_full(start + Duration::from_millis(101)));
    }
}
