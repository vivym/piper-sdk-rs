//! Phase 2 性能基准测试
//!
//! 测试重构后的 High Level API 性能指标：
//! - Observer 零拷贝读取性能
//! - Observer 并发读取性能
//! - 强类型单位开销
//! - 完整控制循环场景

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use piper_sdk::can::{CanAdapter, CanError, PiperFrame, SplittableAdapter};
use piper_sdk::high_level::client::observer::Observer;
use piper_sdk::high_level::types::{Joint, JointArray, Rad};
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
    Observer::new(robot)
}

/// Observer 读取性能测试
fn bench_observer(c: &mut Criterion) {
    let mut group = c.benchmark_group("Observer");

    let observer = setup_observer();

    // 单项读取
    group.bench_function("read_joint_positions", |b| {
        b.iter(|| black_box(observer.joint_positions()))
    });

    group.bench_function("read_joint_velocities", |b| {
        b.iter(|| black_box(observer.joint_velocities()))
    });

    group.bench_function("read_joint_torques", |b| {
        b.iter(|| black_box(observer.joint_torques()))
    });

    group.bench_function("read_gripper_state", |b| {
        b.iter(|| black_box(observer.gripper_state()))
    });

    // 完整状态快照（时间一致）
    group.bench_function("read_snapshot", |b| {
        b.iter(|| black_box(observer.snapshot()))
    });

    group.finish();
}

/// Observer 并发读取测试
fn bench_observer_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("ObserverConcurrent");

    let observer = Arc::new(setup_observer());

    // 并发读取（不同线程数）
    for num_threads in [1, 2, 4, 8] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}threads", num_threads)),
            &num_threads,
            |b, &threads| {
                b.iter(|| {
                    let mut handles = vec![];
                    for _ in 0..threads {
                        let obs = observer.clone();
                        handles.push(std::thread::spawn(move || {
                            for _ in 0..100 {
                                black_box(obs.joint_positions());
                            }
                        }));
                    }
                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Observer 并发读取测试（多个 Observer 实例）
fn bench_observer_multiple_instances(c: &mut Criterion) {
    let mut group = c.benchmark_group("ObserverMultipleInstances");

    let observer = Arc::new(setup_observer());

    group.bench_function("multiple_readers", |b| {
        b.iter(|| {
            let obs1 = observer.clone();
            let obs2 = observer.clone();
            let obs3 = observer.clone();

            // 多个线程同时读取
            let t1 = std::thread::spawn(move || {
                for _ in 0..50 {
                    black_box(obs1.joint_positions());
                }
            });
            let t2 = std::thread::spawn(move || {
                for _ in 0..50 {
                    black_box(obs2.joint_velocities());
                }
            });
            let t3 = std::thread::spawn(move || {
                for _ in 0..50 {
                    black_box(obs3.snapshot());
                }
            });

            t1.join().unwrap();
            t2.join().unwrap();
            t3.join().unwrap();
        });
    });

    group.finish();
}

/// 强类型单位开销测试
fn bench_typed_units(c: &mut Criterion) {
    let mut group = c.benchmark_group("TypedUnits");

    // Rad 创建和访问
    group.bench_function("rad_creation", |b| b.iter(|| black_box(Rad(1.5))));

    group.bench_function("rad_to_deg", |b| {
        let rad = Rad(1.5);
        b.iter(|| black_box(rad.to_deg()))
    });

    // JointArray 创建
    group.bench_function("joint_array_splat", |b| {
        b.iter(|| black_box(JointArray::splat(Rad(1.0))))
    });

    group.bench_function("joint_array_index", |b| {
        let array = JointArray::splat(Rad(1.0));
        b.iter(|| black_box(array[Joint::J3]))
    });

    group.finish();
}

/// 完整的控制循环场景测试
fn bench_full_scenario(c: &mut Criterion) {
    let mut group = c.benchmark_group("FullScenario");

    let observer = setup_observer();

    group.bench_function("control_loop_iteration", |b| {
        b.iter(|| {
            // 1. 读取当前状态（使用 snapshot 保证时间一致性）
            let snapshot = black_box(observer.snapshot());

            // 2. 计算控制量（模拟 PD 控制）
            let target = Rad(0.1);
            let position_error = target.0 - snapshot.position[Joint::J1].0;
            let velocity_error = 0.0 - snapshot.velocity[Joint::J1].value();

            // 3. 计算控制输出（模拟）
            let kp = 10.0;
            let kd = 0.8;
            black_box(kp * position_error + kd * velocity_error)
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_observer,
    bench_observer_concurrent,
    bench_observer_multiple_instances,
    bench_typed_units,
    bench_full_scenario,
);
criterion_main!(benches);
