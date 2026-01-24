//! Observer 性能基准测试
//!
//! 测试 Observer 的零拷贝访问性能，验证重构后的性能优化效果。

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use piper_sdk::can::{CanAdapter, CanError, PiperFrame, SplittableAdapter};
use piper_sdk::high_level::client::observer::Observer;
use piper_sdk::robot::Piper as RobotPiper;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// MockCanAdapter 用于基准测试
struct MockCanAdapter {
    receive_queue: Arc<Mutex<VecDeque<PiperFrame>>>,
    sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
}

impl MockCanAdapter {
    fn new() -> Self {
        Self {
            receive_queue: Arc::new(Mutex::new(VecDeque::new())),
            sent_frames: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl CanAdapter for MockCanAdapter {
    fn send(&mut self, frame: PiperFrame) -> std::result::Result<(), CanError> {
        self.sent_frames.lock().unwrap().push(frame);
        Ok(())
    }

    fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
        self.receive_queue.lock().unwrap().pop_front().ok_or(CanError::Timeout)
    }
}

struct MockRxAdapter {
    receive_queue: Arc<Mutex<VecDeque<PiperFrame>>>,
}

impl piper_sdk::can::RxAdapter for MockRxAdapter {
    fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
        self.receive_queue.lock().unwrap().pop_front().ok_or(CanError::Timeout)
    }
}

struct MockTxAdapter {
    sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
}

impl piper_sdk::can::TxAdapter for MockTxAdapter {
    fn send(&mut self, frame: PiperFrame) -> std::result::Result<(), CanError> {
        self.sent_frames.lock().unwrap().push(frame);
        Ok(())
    }
}

impl SplittableAdapter for MockCanAdapter {
    type RxAdapter = MockRxAdapter;
    type TxAdapter = MockTxAdapter;

    fn split(self) -> std::result::Result<(Self::RxAdapter, Self::TxAdapter), CanError> {
        Ok((
            MockRxAdapter {
                receive_queue: self.receive_queue.clone(),
            },
            MockTxAdapter {
                sent_frames: self.sent_frames.clone(),
            },
        ))
    }
}

fn setup_observer() -> Observer {
    let adapter = MockCanAdapter::new();
    let robot = Arc::new(RobotPiper::new_dual_thread(adapter, None).unwrap());
    // Observer::new 是 pub(crate)，可以在 benches 中使用（benches 是 crate 的一部分）
    Observer::new(robot)
}

fn bench_joint_positions(c: &mut Criterion) {
    let observer = setup_observer();

    c.bench_function("observer_joint_positions", |b| {
        b.iter(|| {
            black_box(observer.joint_positions());
        });
    });
}

fn bench_joint_velocities(c: &mut Criterion) {
    let observer = setup_observer();

    c.bench_function("observer_joint_velocities", |b| {
        b.iter(|| {
            black_box(observer.joint_velocities());
        });
    });
}

fn bench_joint_torques(c: &mut Criterion) {
    let observer = setup_observer();

    c.bench_function("observer_joint_torques", |b| {
        b.iter(|| {
            black_box(observer.joint_torques());
        });
    });
}

fn bench_snapshot(c: &mut Criterion) {
    let observer = setup_observer();

    c.bench_function("observer_snapshot", |b| {
        b.iter(|| {
            black_box(observer.snapshot());
        });
    });
}

fn bench_gripper_state(c: &mut Criterion) {
    let observer = setup_observer();

    c.bench_function("observer_gripper_state", |b| {
        b.iter(|| {
            black_box(observer.gripper_state());
        });
    });
}

fn bench_is_arm_enabled(c: &mut Criterion) {
    let observer = setup_observer();

    c.bench_function("observer_is_arm_enabled", |b| {
        b.iter(|| {
            black_box(observer.is_arm_enabled());
        });
    });
}

criterion_group!(
    benches,
    bench_joint_positions,
    bench_joint_velocities,
    bench_joint_torques,
    bench_snapshot,
    bench_gripper_state,
    bench_is_arm_enabled
);
criterion_main!(benches);
