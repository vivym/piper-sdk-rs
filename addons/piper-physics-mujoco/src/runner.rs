//! Reusable real-robot gravity compensation runner

use crate::{
    GravityCompensation, GravityCompensationRunnerError, JointState, JointTorques,
};
use piper_sdk::client::state::{Active, MitMode};
use piper_sdk::client::types::{JointArray, NewtonMeter, Rad, Result as RobotResult};
use piper_sdk::client::{ControlReadPolicy, ControlSnapshot, Piper, RuntimeHealthSnapshot};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Robot interface required by the gravity compensation runner
pub trait GravityCompensationRobot: Send + Sync {
    /// Read a control-safe aligned snapshot
    fn control_snapshot(&self, policy: ControlReadPolicy) -> RobotResult<ControlSnapshot>;

    /// Read current runtime health snapshot
    fn runtime_health(&self) -> RuntimeHealthSnapshot;

    /// Send MIT torque command and wait for TX thread confirmation
    fn command_torques_confirmed(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: &JointArray<f64>,
        kd: &JointArray<f64>,
        torques: &JointArray<NewtonMeter>,
        timeout: Duration,
    ) -> RobotResult<()>;
}

impl GravityCompensationRobot for Piper<Active<MitMode>> {
    fn control_snapshot(&self, policy: ControlReadPolicy) -> RobotResult<ControlSnapshot> {
        self.observer().control_snapshot(policy)
    }

    fn runtime_health(&self) -> RuntimeHealthSnapshot {
        Piper::<Active<MitMode>>::runtime_health(self)
    }

    fn command_torques_confirmed(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: &JointArray<f64>,
        kd: &JointArray<f64>,
        torques: &JointArray<NewtonMeter>,
        timeout: Duration,
    ) -> RobotResult<()> {
        Piper::<Active<MitMode>>::command_torques_confirmed(
            self, positions, velocities, kp, kd, torques, timeout,
        )
    }
}

/// Runner configuration for real-robot gravity compensation
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GravityCompensationRunnerConfig {
    /// Main control loop period
    pub loop_period: Duration,
    /// Maximum time allowed for TX thread delivery confirmation
    pub command_delivery_timeout: Duration,
    /// Safe aligned read policy for control snapshots
    pub read_policy: ControlReadPolicy,
    /// Safety scaling applied to model torques before sending
    pub torque_safety_scale: [f64; 6],
    /// Damping gains used during shutdown
    pub shutdown_kd: [f64; 6],
    /// Duration of the damping shutdown sequence
    pub shutdown_duration: Duration,
}

impl Default for GravityCompensationRunnerConfig {
    fn default() -> Self {
        Self {
            loop_period: Duration::from_millis(5),
            command_delivery_timeout: Duration::from_millis(10),
            read_policy: ControlReadPolicy::default(),
            torque_safety_scale: [0.25, 0.25, 0.25, 1.25, 1.25, 1.25],
            shutdown_kd: [0.4; 6],
            shutdown_duration: Duration::from_secs(5),
        }
    }
}

/// Run statistics for a gravity compensation session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GravityCompensationRunStats {
    /// Completed control iterations
    pub iterations: u64,
    /// Total runtime until the loop exited
    pub runtime: Duration,
}

/// Failure returned after the runner has already attempted damping shutdown
#[derive(Debug, Error)]
#[error(
    "gravity compensation stopped after {iterations} iterations ({stats_runtime_secs:.3}s): {error}"
)]
pub struct GravityCompensationRunFailure {
    /// Accumulated stats before failure
    pub stats: GravityCompensationRunStats,
    /// Terminal error that stopped the main loop
    #[source]
    pub error: GravityCompensationRunnerError,
    iterations: u64,
    stats_runtime_secs: f64,
}

impl GravityCompensationRunFailure {
    fn new(stats: GravityCompensationRunStats, error: GravityCompensationRunnerError) -> Self {
        Self {
            iterations: stats.iterations,
            stats,
            error,
            stats_runtime_secs: stats.runtime.as_secs_f64(),
        }
    }
}

/// Reusable runner for real-robot gravity compensation
pub struct GravityCompensationRunner<'a, R, G> {
    robot: &'a R,
    gravity: &'a mut G,
    config: GravityCompensationRunnerConfig,
}

impl<'a, R, G> GravityCompensationRunner<'a, R, G>
where
    R: GravityCompensationRobot,
    G: GravityCompensation,
{
    /// Create a new runner
    pub fn new(robot: &'a R, gravity: &'a mut G, config: GravityCompensationRunnerConfig) -> Self {
        Self {
            robot,
            gravity,
            config,
        }
    }

    /// Run the compensation loop until cancelled or an error occurs
    pub fn run_until_stopped<ShouldContinue>(
        &mut self,
        mut should_continue: ShouldContinue,
    ) -> Result<GravityCompensationRunStats, GravityCompensationRunFailure>
    where
        ShouldContinue: FnMut() -> bool,
    {
        let start = Instant::now();
        let mut iterations = 0u64;
        let mut stop_error = None;

        while should_continue() {
            let loop_start = Instant::now();

            match self.run_cycle() {
                Ok(()) => iterations += 1,
                Err(error) => {
                    stop_error = Some(error);
                    break;
                },
            }

            sleep_remainder(loop_start, self.config.loop_period);
        }

        self.run_shutdown_sequence();

        let stats = GravityCompensationRunStats {
            iterations,
            runtime: start.elapsed(),
        };

        match stop_error {
            Some(error) => Err(GravityCompensationRunFailure::new(stats, error)),
            None => Ok(stats),
        }
    }

    fn run_cycle(&mut self) -> Result<(), GravityCompensationRunnerError> {
        ensure_runtime_healthy(self.robot.runtime_health())?;
        let snapshot = self.robot.control_snapshot(self.config.read_policy)?;
        let q = joint_state_from_snapshot(snapshot);
        let raw_torques = self.gravity.compute_gravity_compensation(&q)?;
        let commanded_torques = scale_torques(raw_torques, self.config.torque_safety_scale);
        let zero_gains = JointArray::splat(0.0);
        let velocities = snapshot.velocity.map(|velocity| velocity.0);

        self.robot.command_torques_confirmed(
            &snapshot.position,
            &velocities,
            &zero_gains,
            &zero_gains,
            &commanded_torques,
            self.config.command_delivery_timeout,
        )?;

        Ok(())
    }

    fn run_shutdown_sequence(&mut self) {
        if self.config.shutdown_duration.is_zero() {
            return;
        }

        let shutdown_start = Instant::now();
        let zero_kp = JointArray::splat(0.0);
        let zero_velocities = JointArray::splat(0.0);
        let shutdown_kd = JointArray::from(self.config.shutdown_kd);
        let zero_torques = JointArray::splat(NewtonMeter(0.0));

        while shutdown_start.elapsed() < self.config.shutdown_duration {
            let cycle_start = Instant::now();
            let health = self.robot.runtime_health();
            if let Err(error) = ensure_runtime_healthy(health) {
                log::warn!(
                    "Gravity compensation shutdown aborted: runtime health unhealthy: {}",
                    error
                );
                break;
            }
            let snapshot = match self.robot.control_snapshot(self.config.read_policy) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    log::warn!(
                        "Gravity compensation shutdown aborted: failed to read control snapshot: {}",
                        error
                    );
                    break;
                },
            };
            if let Err(error) = self.robot.command_torques_confirmed(
                &snapshot.position,
                &zero_velocities,
                &zero_kp,
                &shutdown_kd,
                &zero_torques,
                self.config.command_delivery_timeout,
            ) {
                log::warn!(
                    "Gravity compensation shutdown aborted: failed to send damping command: {}",
                    error
                );
                break;
            }

            sleep_remainder(cycle_start, self.config.loop_period);
        }
    }
}

fn joint_state_from_snapshot(snapshot: ControlSnapshot) -> JointState {
    let positions = snapshot.position.into_array().map(|position| position.0);
    JointState::from_row_slice(&positions)
}

fn scale_torques(raw_torques: JointTorques, safety_scale: [f64; 6]) -> JointArray<NewtonMeter> {
    JointArray::from(std::array::from_fn(|index| {
        NewtonMeter(raw_torques[index] * safety_scale[index])
    }))
}

fn sleep_remainder(started_at: Instant, period: Duration) {
    if period.is_zero() {
        thread::yield_now();
        return;
    }

    let elapsed = started_at.elapsed();
    if elapsed < period {
        thread::sleep(period - elapsed);
    }
}

fn ensure_runtime_healthy(health: RuntimeHealthSnapshot) -> RobotResult<()> {
    if health.rx_alive && health.tx_alive && health.fault.is_none() {
        return Ok(());
    }

    Err(piper_sdk::client::types::RobotError::runtime_health_unhealthy(
        health.rx_alive,
        health.tx_alive,
        health.fault,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_sdk::client::types::RadPerSecond;
    use piper_sdk::client::types::RobotError;
    use piper_sdk::client::RuntimeFaultKind;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::PhysicsError;

    #[derive(Debug, Clone, Copy, PartialEq)]
    struct RecordedCommand {
        velocities: [f64; 6],
        kd: [f64; 6],
        torques: [f64; 6],
    }

    struct FakeRobot {
        snapshots: Mutex<VecDeque<RobotResult<ControlSnapshot>>>,
        healths: Mutex<VecDeque<RuntimeHealthSnapshot>>,
        command_results: Mutex<VecDeque<RobotResult<()>>>,
        commands: Mutex<Vec<RecordedCommand>>,
    }

    impl FakeRobot {
        fn new(
            snapshots: impl Into<VecDeque<RobotResult<ControlSnapshot>>>,
            healths: impl Into<VecDeque<RuntimeHealthSnapshot>>,
            command_results: impl Into<VecDeque<RobotResult<()>>>,
        ) -> Self {
            Self {
                snapshots: Mutex::new(snapshots.into()),
                healths: Mutex::new(healths.into()),
                command_results: Mutex::new(command_results.into()),
                commands: Mutex::new(Vec::new()),
            }
        }

        fn commands(&self) -> Vec<RecordedCommand> {
            self.commands.lock().expect("commands lock").clone()
        }
    }

    impl GravityCompensationRobot for FakeRobot {
        fn control_snapshot(&self, _policy: ControlReadPolicy) -> RobotResult<ControlSnapshot> {
            self.snapshots.lock().expect("snapshots lock").pop_front().unwrap_or_else(|| {
                Err(RobotError::feedback_stale(
                    Duration::from_millis(100),
                    Duration::from_millis(50),
                ))
            })
        }

        fn runtime_health(&self) -> RuntimeHealthSnapshot {
            self.healths
                .lock()
                .expect("healths lock")
                .pop_front()
                .unwrap_or_else(healthy_runtime)
        }

        fn command_torques_confirmed(
            &self,
            _positions: &JointArray<Rad>,
            velocities: &JointArray<f64>,
            _kp: &JointArray<f64>,
            kd: &JointArray<f64>,
            torques: &JointArray<NewtonMeter>,
            _timeout: Duration,
        ) -> RobotResult<()> {
            self.commands.lock().expect("commands lock").push(RecordedCommand {
                velocities: velocities.into_array(),
                kd: kd.into_array(),
                torques: torques.into_array().map(|torque| torque.0),
            });

            self.command_results
                .lock()
                .expect("command results lock")
                .pop_front()
                .unwrap_or(Ok(()))
        }
    }

    struct FakeGravity {
        results: VecDeque<Result<JointTorques, PhysicsError>>,
        calls: AtomicUsize,
    }

    impl FakeGravity {
        fn new(results: impl Into<VecDeque<Result<JointTorques, PhysicsError>>>) -> Self {
            Self {
                results: results.into(),
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl GravityCompensation for FakeGravity {
        fn compute_gravity_compensation(
            &mut self,
            _q: &JointState,
        ) -> Result<JointTorques, PhysicsError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.results.pop_front().unwrap_or_else(|| Ok(JointState::repeat(0.0)))
        }

        fn compute_partial_inverse_dynamics(
            &mut self,
            q: &JointState,
            _qvel: &[f64; 6],
        ) -> Result<JointTorques, PhysicsError> {
            self.compute_gravity_compensation(q)
        }

        fn compute_inverse_dynamics(
            &mut self,
            q: &JointState,
            _qvel: &[f64; 6],
            _qacc_desired: &[f64; 6],
        ) -> Result<JointTorques, PhysicsError> {
            self.compute_gravity_compensation(q)
        }

        fn name(&self) -> &str {
            "fake"
        }

        fn is_initialized(&self) -> bool {
            true
        }
    }

    fn snapshot_with_velocity(velocity: f64) -> ControlSnapshot {
        ControlSnapshot {
            position: JointArray::splat(Rad(0.0)),
            velocity: JointArray::splat(RadPerSecond(velocity)),
            torque: JointArray::splat(NewtonMeter(0.0)),
            position_timestamp_us: 100,
            dynamic_timestamp_us: 100,
            skew_us: 0,
        }
    }

    fn healthy_runtime() -> RuntimeHealthSnapshot {
        RuntimeHealthSnapshot {
            connected: true,
            last_feedback_age: Duration::from_millis(1),
            rx_alive: true,
            tx_alive: true,
            fault: None,
        }
    }

    #[test]
    fn runner_stops_cleanly_when_cancelled() {
        let robot = FakeRobot::new(
            vec![Ok(snapshot_with_velocity(0.0)), Ok(snapshot_with_velocity(0.0))],
            vec![healthy_runtime()],
            Vec::<RobotResult<()>>::new(),
        );
        let mut gravity = FakeGravity::new(vec![Ok(JointState::repeat(1.0))]);
        let mut runner = GravityCompensationRunner::new(
            &robot,
            &mut gravity,
            GravityCompensationRunnerConfig {
                loop_period: Duration::ZERO,
                shutdown_duration: Duration::ZERO,
                ..Default::default()
            },
        );
        let mut polls = 0;

        let stats = runner
            .run_until_stopped(|| {
                polls += 1;
                polls <= 1
            })
            .expect("runner should stop cleanly");

        assert_eq!(stats.iterations, 1);
        assert_eq!(robot.commands().len(), 1);
    }

    #[test]
    fn runner_reports_stale_feedback() {
        let robot = FakeRobot::new(
            vec![Err(RobotError::feedback_stale(
                Duration::from_millis(80),
                Duration::from_millis(50),
            ))],
            vec![healthy_runtime()],
            Vec::<RobotResult<()>>::new(),
        );
        let mut gravity = FakeGravity::new(Vec::<Result<JointTorques, PhysicsError>>::new());
        let mut runner = GravityCompensationRunner::new(
            &robot,
            &mut gravity,
            GravityCompensationRunnerConfig {
                shutdown_duration: Duration::ZERO,
                ..Default::default()
            },
        );

        let error = runner
            .run_until_stopped(|| true)
            .expect_err("stale feedback should stop the runner");

        assert!(matches!(
            error.error,
            GravityCompensationRunnerError::Robot(RobotError::FeedbackStale { .. })
        ));
        assert_eq!(error.stats.iterations, 0);
    }

    #[test]
    fn runner_reports_misaligned_feedback() {
        let robot = FakeRobot::new(
            vec![Err(RobotError::state_misaligned(9_000, 5_000))],
            vec![healthy_runtime()],
            Vec::<RobotResult<()>>::new(),
        );
        let mut gravity = FakeGravity::new(Vec::<Result<JointTorques, PhysicsError>>::new());
        let mut runner = GravityCompensationRunner::new(
            &robot,
            &mut gravity,
            GravityCompensationRunnerConfig {
                shutdown_duration: Duration::ZERO,
                ..Default::default()
            },
        );

        let error = runner
            .run_until_stopped(|| true)
            .expect_err("misaligned feedback should stop the runner");

        assert!(matches!(
            error.error,
            GravityCompensationRunnerError::Robot(RobotError::StateMisaligned { .. })
        ));
    }

    #[test]
    fn runner_reports_solver_error() {
        let robot = FakeRobot::new(
            vec![Ok(snapshot_with_velocity(0.0))],
            vec![healthy_runtime()],
            Vec::<RobotResult<()>>::new(),
        );
        let mut gravity = FakeGravity::new(vec![Err(PhysicsError::CalculationFailed(
            "solver failed".to_string(),
        ))]);
        let mut runner = GravityCompensationRunner::new(
            &robot,
            &mut gravity,
            GravityCompensationRunnerConfig {
                shutdown_duration: Duration::ZERO,
                ..Default::default()
            },
        );

        let error = runner
            .run_until_stopped(|| true)
            .expect_err("solver error should stop the runner");

        assert!(matches!(
            error.error,
            GravityCompensationRunnerError::Physics(PhysicsError::CalculationFailed(_))
        ));
    }

    #[test]
    fn runner_reports_send_error() {
        let robot = FakeRobot::new(
            vec![Ok(snapshot_with_velocity(0.0))],
            vec![healthy_runtime()],
            vec![Err(RobotError::CanIoError("send failed".to_string()))],
        );
        let mut gravity = FakeGravity::new(vec![Ok(JointState::repeat(1.0))]);
        let mut runner = GravityCompensationRunner::new(
            &robot,
            &mut gravity,
            GravityCompensationRunnerConfig {
                shutdown_duration: Duration::ZERO,
                ..Default::default()
            },
        );

        let error = runner
            .run_until_stopped(|| true)
            .expect_err("send error should stop the runner");

        assert!(matches!(
            error.error,
            GravityCompensationRunnerError::Robot(RobotError::CanIoError(_))
        ));
    }

    #[test]
    fn runner_applies_damping_shutdown() {
        let robot = FakeRobot::new(
            vec![
                Ok(snapshot_with_velocity(0.0)),
                Ok(snapshot_with_velocity(1.5)),
            ],
            vec![healthy_runtime(), healthy_runtime()],
            Vec::<RobotResult<()>>::new(),
        );
        let mut gravity = FakeGravity::new(vec![Ok(JointState::repeat(2.0))]);
        let config = GravityCompensationRunnerConfig {
            loop_period: Duration::ZERO,
            shutdown_duration: Duration::from_millis(10),
            shutdown_kd: [0.4, 0.4, 0.4, 0.6, 0.6, 0.6],
            ..Default::default()
        };
        let mut runner = GravityCompensationRunner::new(&robot, &mut gravity, config);
        let mut polls = 0;

        let stats = runner
            .run_until_stopped(|| {
                polls += 1;
                polls <= 1
            })
            .expect("runner should stop cleanly");

        let commands = robot.commands();
        assert_eq!(stats.iterations, 1);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[1].torques, [0.0; 6]);
        assert_eq!(commands[1].velocities, [0.0; 6]);
        assert_eq!(commands[1].kd, config.shutdown_kd);
    }

    #[test]
    fn runner_stops_before_solver_when_runtime_health_is_unhealthy() {
        let robot = FakeRobot::new(
            vec![Ok(snapshot_with_velocity(0.0))],
            vec![RuntimeHealthSnapshot {
                connected: true,
                last_feedback_age: Duration::from_millis(1),
                rx_alive: true,
                tx_alive: false,
                fault: Some(RuntimeFaultKind::TxExited),
            }],
            Vec::<RobotResult<()>>::new(),
        );
        let mut gravity = FakeGravity::new(vec![Ok(JointState::repeat(1.0))]);
        let mut runner = GravityCompensationRunner::new(
            &robot,
            &mut gravity,
            GravityCompensationRunnerConfig {
                shutdown_duration: Duration::ZERO,
                ..Default::default()
            },
        );

        let error = runner
            .run_until_stopped(|| true)
            .expect_err("unhealthy runtime should stop the runner");

        assert!(matches!(
            error.error,
            GravityCompensationRunnerError::Robot(RobotError::RuntimeHealthUnhealthy { .. })
        ));
        assert_eq!(gravity.call_count(), 0);
        assert!(robot.commands().is_empty());
    }

    #[test]
    fn runner_stops_shutdown_when_confirmed_send_fails() {
        let robot = FakeRobot::new(
            vec![
                Ok(snapshot_with_velocity(0.0)),
                Ok(snapshot_with_velocity(1.5)),
                Ok(snapshot_with_velocity(1.5)),
            ],
            vec![healthy_runtime(), healthy_runtime(), healthy_runtime()],
            vec![Ok(()), Err(RobotError::CanIoError("shutdown failed".to_string()))],
        );
        let mut gravity = FakeGravity::new(vec![Ok(JointState::repeat(2.0))]);
        let mut runner = GravityCompensationRunner::new(
            &robot,
            &mut gravity,
            GravityCompensationRunnerConfig {
                loop_period: Duration::ZERO,
                shutdown_duration: Duration::from_millis(10),
                ..Default::default()
            },
        );
        let mut polls = 0;

        let _stats = runner
            .run_until_stopped(|| {
                polls += 1;
                polls <= 1
            })
            .expect("runner should stop cleanly");

        let commands = robot.commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[1].torques, [0.0; 6]);
    }
}
