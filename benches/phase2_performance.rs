//! Phase 2 性能基准测试
//!
//! 测试读写分离架构的性能指标：
//! - StateTracker 快速检查延迟
//! - RawCommander 命令发送吞吐量
//! - Observer 并发读取性能
//! - MotionCommander API 延迟

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use parking_lot::RwLock;
use piper_sdk::high_level::client::observer::{Observer, RobotState};
use piper_sdk::high_level::client::state_tracker::StateTracker;
use piper_sdk::high_level::types::joint::{Joint, JointArray};
use piper_sdk::high_level::types::units::Rad;
use std::sync::Arc;

/// StateTracker 性能测试
fn bench_state_tracker(c: &mut Criterion) {
    let mut group = c.benchmark_group("StateTracker");

    // 快速路径 - 有效状态
    let tracker = StateTracker::new();
    group.bench_function("fast_path_valid", |b| {
        b.iter(|| black_box(tracker.is_valid()))
    });

    // 快速路径 - 带 Result
    group.bench_function("fast_path_with_result", |b| {
        b.iter(|| black_box(tracker.check_valid_fast()))
    });

    // 慢路径 - 损坏状态
    tracker.mark_poisoned("Test");
    group.bench_function("slow_path_poisoned", |b| {
        b.iter(|| black_box(tracker.check_valid_fast()))
    });

    group.finish();
}

/// Observer 读取性能测试
fn bench_observer(c: &mut Criterion) {
    let mut group = c.benchmark_group("Observer");

    let state = Arc::new(RwLock::new(RobotState::default()));
    let observer = Observer::new(state);

    // 单项读取
    group.bench_function("read_joint_positions", |b| {
        b.iter(|| black_box(observer.joint_positions()))
    });

    group.bench_function("read_joint_velocities", |b| {
        b.iter(|| black_box(observer.joint_velocities()))
    });

    group.bench_function("read_gripper_state", |b| {
        b.iter(|| black_box(observer.gripper_state()))
    });

    // 完整状态快照
    group.bench_function("read_full_state", |b| {
        b.iter(|| black_box(observer.state()))
    });

    group.finish();
}

/// Observer 并发读取测试
fn bench_observer_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("ObserverConcurrent");

    let state = Arc::new(RwLock::new(RobotState::default()));
    let observer = Arc::new(Observer::new(state));

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

/// Observer 读写混合测试
fn bench_observer_read_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("ObserverReadWrite");

    let state = Arc::new(RwLock::new(RobotState::default()));
    let observer = Arc::new(Observer::new(state));

    group.bench_function("mixed_read_write", |b| {
        b.iter(|| {
            let obs_reader = observer.clone();
            let obs_writer = observer.clone();

            // 写线程
            let writer = std::thread::spawn(move || {
                for i in 0..10 {
                    obs_writer.update_joint_positions(JointArray::splat(Rad(i as f64)));
                }
            });

            // 读线程
            let reader = std::thread::spawn(move || {
                for _ in 0..100 {
                    black_box(obs_reader.joint_positions());
                }
            });

            writer.join().unwrap();
            reader.join().unwrap();
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

/// 完整的读写分离场景测试
fn bench_full_scenario(c: &mut Criterion) {
    let mut group = c.benchmark_group("FullScenario");

    let tracker = Arc::new(StateTracker::new());
    let state = Arc::new(RwLock::new(RobotState::default()));
    let observer = Arc::new(Observer::new(state));

    group.bench_function("control_loop_iteration", |b| {
        b.iter(|| {
            // 1. 状态检查（快速路径）
            black_box(tracker.check_valid_fast()).ok();

            // 2. 读取当前状态
            let positions = black_box(observer.joint_positions());

            // 3. 计算控制量（模拟）
            let target = JointArray::splat(Rad(0.1));
            let error = positions[Joint::J1].0 - target[Joint::J1].0;

            // 4. 准备命令（模拟）
            black_box(error * 10.0);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_state_tracker,
    bench_observer,
    bench_observer_concurrent,
    bench_observer_read_write,
    bench_typed_units,
    bench_full_scenario,
);
criterion_main!(benches);
