use crate::gravity::model::{JOINT_COUNT, QuasiStaticTorqueModel};
use piper_client::dual_arm::{
    BilateralDynamicsCompensation, BilateralDynamicsCompensator, DualArmSnapshot,
};
use piper_client::types::{JointArray, NewtonMeter, Rad};
use std::error::Error;
use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

const CONFIDENCE_TOLERANCE_RAD: f64 = 0.05;
const MASTER_ASSIST_TORQUE_LIMIT_NM: f64 = 1.5;
const SLAVE_ASSIST_TORQUE_LIMIT_NM: f64 = 4.0;
const ASSIST_TORQUE_SLEW_LIMIT_NM_PER_S: f64 = 25.0;

#[derive(Debug, Clone)]
pub struct GravityCompensator {
    master_model: Option<QuasiStaticTorqueModel>,
    slave_model: Option<QuasiStaticTorqueModel>,
    settings: GravityCompensationSettings,
    telemetry: GravityCompensationTelemetry,
    master_assist_torque_nm: [f64; JOINT_COUNT],
    slave_assist_torque_nm: [f64; JOINT_COUNT],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GravityCompensationSettings {
    pub reflection_compensation: bool,
    pub master_assist_ratio: f64,
    pub slave_assist_ratio: f64,
}

#[derive(Debug, Clone, Default)]
pub struct GravityCompensationTelemetry {
    range_violations: Arc<AtomicU64>,
}

impl GravityCompensationTelemetry {
    pub fn range_violations(&self) -> u64 {
        self.range_violations.load(Ordering::Relaxed)
    }

    fn record_range_violation(&self) {
        self.range_violations.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub struct GravityCompensationError {
    message: String,
}

impl fmt::Display for GravityCompensationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for GravityCompensationError {}

impl GravityCompensationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl GravityCompensator {
    pub fn new_with_telemetry(
        master_model: Option<QuasiStaticTorqueModel>,
        slave_model: Option<QuasiStaticTorqueModel>,
        settings: GravityCompensationSettings,
        telemetry: GravityCompensationTelemetry,
    ) -> Self {
        Self {
            master_model,
            slave_model,
            settings,
            telemetry,
            master_assist_torque_nm: [0.0; JOINT_COUNT],
            slave_assist_torque_nm: [0.0; JOINT_COUNT],
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(
        master_model: Option<QuasiStaticTorqueModel>,
        slave_model: Option<QuasiStaticTorqueModel>,
        settings: GravityCompensationSettings,
    ) -> Self {
        Self::new_with_telemetry(
            master_model,
            slave_model,
            settings,
            GravityCompensationTelemetry::default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn for_tests_with_telemetry(
        master_model: Option<QuasiStaticTorqueModel>,
        slave_model: Option<QuasiStaticTorqueModel>,
        settings: GravityCompensationSettings,
        telemetry: GravityCompensationTelemetry,
    ) -> Self {
        Self::new_with_telemetry(master_model, slave_model, settings, telemetry)
    }
}

impl BilateralDynamicsCompensator for GravityCompensator {
    type Error = GravityCompensationError;

    fn compute(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> Result<BilateralDynamicsCompensation, Self::Error> {
        let q_master = rad_array(snapshot.left.state.position);
        let q_slave = rad_array(snapshot.right.state.position);
        let slave_measured_tau = nm_array(snapshot.right.state.torque);

        let master_hat = eval_or_zero(self.master_model.as_ref(), q_master, "master")?;
        let slave_hat = eval_or_zero(self.slave_model.as_ref(), q_slave, "slave")?;
        let master_confidence = confidence_or_zero(self.master_model.as_ref(), q_master);
        let slave_confidence = confidence_or_zero(self.slave_model.as_ref(), q_slave);
        if self.compute_has_range_violation(master_confidence, slave_confidence) {
            self.telemetry.record_range_violation();
        }

        let mut master_assist_target_nm = [0.0; JOINT_COUNT];
        let mut slave_assist_target_nm = [0.0; JOINT_COUNT];
        let mut slave_external_torque_est = [NewtonMeter::ZERO; JOINT_COUNT];

        for joint in 0..JOINT_COUNT {
            let reflected_model = if self.settings.reflection_compensation {
                slave_hat[joint] * slave_confidence
            } else {
                0.0
            };
            slave_external_torque_est[joint] =
                NewtonMeter(slave_measured_tau[joint] - reflected_model);
            master_assist_target_nm[joint] = clamp_assist_target(
                master_hat[joint] * self.settings.master_assist_ratio * master_confidence,
                MASTER_ASSIST_TORQUE_LIMIT_NM,
            );
            slave_assist_target_nm[joint] = clamp_assist_target(
                slave_hat[joint] * self.settings.slave_assist_ratio * slave_confidence,
                SLAVE_ASSIST_TORQUE_LIMIT_NM,
            );
        }
        let (master_model_torque, slave_model_torque) =
            self.shape_assist_torque(master_assist_target_nm, slave_assist_target_nm, dt);

        Ok(BilateralDynamicsCompensation {
            master_model_torque: JointArray::new(master_model_torque),
            slave_model_torque: JointArray::new(slave_model_torque),
            master_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
            slave_external_torque_est: JointArray::new(slave_external_torque_est),
        })
    }

    fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
        self.reset_assist_ramps();
        Ok(())
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        self.reset_assist_ramps();
        Ok(())
    }
}

impl GravityCompensator {
    fn compute_has_range_violation(&self, master_confidence: f64, slave_confidence: f64) -> bool {
        let master_assist_uses_model =
            self.settings.master_assist_ratio > 0.0 && self.master_model.is_some();
        let slave_assist_uses_model =
            self.settings.slave_assist_ratio > 0.0 && self.slave_model.is_some();
        let reflection_uses_model =
            self.settings.reflection_compensation && self.slave_model.is_some();

        (master_assist_uses_model && confidence_below_one(master_confidence))
            || ((slave_assist_uses_model || reflection_uses_model)
                && confidence_below_one(slave_confidence))
    }

    fn shape_assist_torque(
        &mut self,
        master_target_nm: [f64; JOINT_COUNT],
        slave_target_nm: [f64; JOINT_COUNT],
        dt: Duration,
    ) -> ([NewtonMeter; JOINT_COUNT], [NewtonMeter; JOINT_COUNT]) {
        let max_delta_nm = ASSIST_TORQUE_SLEW_LIMIT_NM_PER_S * dt.as_secs_f64();
        let max_delta_nm = if max_delta_nm.is_finite() && max_delta_nm > 0.0 {
            max_delta_nm
        } else {
            0.0
        };

        let mut master = [NewtonMeter::ZERO; JOINT_COUNT];
        let mut slave = [NewtonMeter::ZERO; JOINT_COUNT];
        for joint in 0..JOINT_COUNT {
            self.master_assist_torque_nm[joint] = slew_toward(
                self.master_assist_torque_nm[joint],
                master_target_nm[joint],
                max_delta_nm,
            );
            self.slave_assist_torque_nm[joint] = slew_toward(
                self.slave_assist_torque_nm[joint],
                slave_target_nm[joint],
                max_delta_nm,
            );
            master[joint] = NewtonMeter(self.master_assist_torque_nm[joint]);
            slave[joint] = NewtonMeter(self.slave_assist_torque_nm[joint]);
        }
        (master, slave)
    }

    fn reset_assist_ramps(&mut self) {
        self.master_assist_torque_nm = [0.0; JOINT_COUNT];
        self.slave_assist_torque_nm = [0.0; JOINT_COUNT];
    }
}

fn eval_or_zero(
    model: Option<&QuasiStaticTorqueModel>,
    q: [f64; JOINT_COUNT],
    role: &str,
) -> Result<[f64; JOINT_COUNT], GravityCompensationError> {
    match model {
        Some(model) => model.eval(q).map_err(|error| {
            GravityCompensationError::new(format!("{role} gravity eval failed: {error}"))
        }),
        None => Ok([0.0; JOINT_COUNT]),
    }
}

fn confidence_or_zero(model: Option<&QuasiStaticTorqueModel>, q: [f64; JOINT_COUNT]) -> f64 {
    model.map(|model| confidence_for_training_range(model, q)).unwrap_or(0.0)
}

fn confidence_for_training_range(model: &QuasiStaticTorqueModel, q: [f64; JOINT_COUNT]) -> f64 {
    let mut confidence = 1.0;
    for (joint, q_joint) in q.iter().copied().enumerate() {
        let min = model.training_range.q_min_rad[joint];
        let max = model.training_range.q_max_rad[joint];
        confidence = f64::min(confidence, confidence_for_joint(q_joint, min, max));
    }
    confidence
}

fn confidence_for_joint(q: f64, min: f64, max: f64) -> f64 {
    if !q.is_finite() || !min.is_finite() || !max.is_finite() || min > max {
        return 0.0;
    }
    let outside = if q < min {
        min - q
    } else if q > max {
        q - max
    } else {
        0.0
    };
    if outside <= 0.0 {
        1.0
    } else if outside >= CONFIDENCE_TOLERANCE_RAD {
        0.0
    } else {
        1.0 - outside / CONFIDENCE_TOLERANCE_RAD
    }
}

fn confidence_below_one(confidence: f64) -> bool {
    confidence < 1.0
}

fn clamp_assist_target(value: f64, limit: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(-limit, limit)
    }
}

fn slew_toward(current: f64, target: f64, max_delta: f64) -> f64 {
    let delta = target - current;
    if delta.abs() <= max_delta {
        target
    } else {
        current + delta.signum() * max_delta
    }
}

fn rad_array(values: JointArray<Rad>) -> [f64; JOINT_COUNT] {
    values.map(|value| value.0).into_array()
}

fn nm_array(values: JointArray<NewtonMeter>) -> [f64; JOINT_COUNT] {
    values.map(|value| value.0).into_array()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity::model::QuasiStaticTorqueModel;
    use piper_client::dual_arm::{BilateralDynamicsCompensator, DualArmSnapshot};
    use piper_client::observer::{ControlSnapshot, ControlSnapshotFull};
    use piper_client::types::{JointArray, NewtonMeter, Rad, RadPerSecond};
    use std::time::{Duration, Instant};

    #[test]
    fn compensation_subtracts_slave_model_only_for_reflection() {
        let compensator = GravityCompensator::for_tests(
            None,
            Some(QuasiStaticTorqueModel::for_tests_with_constant_output(
                [1.0; 6],
            )),
            GravityCompensationSettings {
                reflection_compensation: true,
                master_assist_ratio: 0.0,
                slave_assist_ratio: 0.0,
            },
        );

        let out = compute_once(compensator, &dual_arm_snapshot_with_slave_torque([3.0; 6]));

        assert_eq!(
            out.slave_external_torque_est.map(|nm| nm.0).into_array(),
            [2.0; 6]
        );
        assert_eq!(
            out.master_model_torque.map(|nm| nm.0).into_array(),
            [0.0; 6]
        );
    }

    #[test]
    fn master_assist_scales_master_model_torque() {
        let compensator = GravityCompensator::for_tests(
            Some(QuasiStaticTorqueModel::for_tests_with_constant_output(
                [2.0; 6],
            )),
            None,
            GravityCompensationSettings {
                reflection_compensation: false,
                master_assist_ratio: 0.25,
                slave_assist_ratio: 0.0,
            },
        );

        let out = compute_once_with_dt(
            compensator,
            &dual_arm_snapshot_for_tests(),
            Duration::from_secs(1),
        );

        assert_eq!(
            out.master_model_torque.map(|nm| nm.0).into_array(),
            [0.5; 6]
        );
    }

    #[test]
    fn assist_model_torque_slew_limits_from_zero_and_clamps() {
        let mut compensator = GravityCompensator::for_tests(
            Some(QuasiStaticTorqueModel::for_tests_with_constant_output(
                [10.0; 6],
            )),
            Some(QuasiStaticTorqueModel::for_tests_with_constant_output(
                [10.0; 6],
            )),
            GravityCompensationSettings {
                reflection_compensation: false,
                master_assist_ratio: 1.0,
                slave_assist_ratio: 1.0,
            },
        );
        let snapshot = dual_arm_snapshot_for_tests();
        let dt = Duration::from_millis(10);

        let first = compute_with_dt(&mut compensator, &snapshot, dt);
        let second = compute_with_dt(&mut compensator, &snapshot, dt);
        let mut last = second;
        for _ in 0..20 {
            last = compute_with_dt(&mut compensator, &snapshot, dt);
        }

        assert_eq!(
            first.master_model_torque.map(|nm| nm.0).into_array(),
            [0.25; 6]
        );
        assert_eq!(
            first.slave_model_torque.map(|nm| nm.0).into_array(),
            [0.25; 6]
        );
        assert_eq!(
            second.master_model_torque.map(|nm| nm.0).into_array(),
            [0.5; 6]
        );
        assert_eq!(
            second.slave_model_torque.map(|nm| nm.0).into_array(),
            [0.5; 6]
        );
        assert_eq!(
            last.master_model_torque.map(|nm| nm.0).into_array(),
            [1.5; 6]
        );
        assert_eq!(
            last.slave_model_torque.map(|nm| nm.0).into_array(),
            [4.0; 6]
        );
    }

    #[test]
    fn reset_and_time_jump_clear_assist_ramp_state() {
        let mut compensator = GravityCompensator::for_tests(
            Some(QuasiStaticTorqueModel::for_tests_with_constant_output(
                [10.0; 6],
            )),
            Some(QuasiStaticTorqueModel::for_tests_with_constant_output(
                [10.0; 6],
            )),
            GravityCompensationSettings {
                reflection_compensation: false,
                master_assist_ratio: 1.0,
                slave_assist_ratio: 1.0,
            },
        );
        let snapshot = dual_arm_snapshot_for_tests();

        let _ = compute_with_dt(&mut compensator, &snapshot, Duration::from_secs(1));
        compensator.reset().expect("reset should succeed");
        let after_reset = compute_with_dt(&mut compensator, &snapshot, Duration::from_millis(10));

        let _ = compute_with_dt(&mut compensator, &snapshot, Duration::from_secs(1));
        compensator
            .on_time_jump(Duration::from_millis(500))
            .expect("time jump reset should succeed");
        let after_time_jump =
            compute_with_dt(&mut compensator, &snapshot, Duration::from_millis(10));

        assert_eq!(
            after_reset.master_model_torque.map(|nm| nm.0).into_array(),
            [0.25; 6]
        );
        assert_eq!(
            after_reset.slave_model_torque.map(|nm| nm.0).into_array(),
            [0.25; 6]
        );
        assert_eq!(
            after_time_jump.master_model_torque.map(|nm| nm.0).into_array(),
            [0.25; 6]
        );
        assert_eq!(
            after_time_jump.slave_model_torque.map(|nm| nm.0).into_array(),
            [0.25; 6]
        );
    }

    #[test]
    fn one_joint_outside_range_attenuates_entire_slave_model() {
        let mut model = QuasiStaticTorqueModel::for_tests_with_constant_output([2.0; 6]);
        model.training_range.q_min_rad = [-0.1; 6];
        model.training_range.q_max_rad = [0.1; 6];
        let compensator = GravityCompensator::for_tests(
            None,
            Some(model),
            GravityCompensationSettings {
                reflection_compensation: true,
                master_assist_ratio: 0.0,
                slave_assist_ratio: 1.0,
            },
        );
        let mut snapshot = dual_arm_snapshot_with_slave_torque([10.0; 6]);
        snapshot.right.state.position =
            JointArray::new([Rad(0.125), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)]);

        let out = compute_once_with_dt(compensator, &snapshot, Duration::from_secs(1));

        assert_eq!(
            out.slave_external_torque_est.map(|nm| nm.0).into_array(),
            [9.0; 6]
        );
        assert_array_close(out.slave_model_torque.map(|nm| nm.0).into_array(), [1.0; 6]);
    }

    #[test]
    fn confidence_blends_reflection_to_raw_torque_outside_training_range() {
        let mut model = QuasiStaticTorqueModel::for_tests_with_constant_output([1.0; 6]);
        model.training_range.q_min_rad = [-0.1; 6];
        model.training_range.q_max_rad = [0.1; 6];
        let compensator = GravityCompensator::for_tests(
            None,
            Some(model),
            GravityCompensationSettings {
                reflection_compensation: true,
                master_assist_ratio: 0.0,
                slave_assist_ratio: 0.5,
            },
        );
        let mut snapshot = dual_arm_snapshot_with_slave_torque([3.0; 6]);
        snapshot.right.state.position = JointArray::splat(Rad(0.2));

        let out = compute_once(compensator, &snapshot);

        assert_eq!(
            out.slave_external_torque_est.map(|nm| nm.0).into_array(),
            [3.0; 6]
        );
        assert_eq!(out.slave_model_torque.map(|nm| nm.0).into_array(), [0.0; 6]);
    }

    #[test]
    fn telemetry_counts_one_range_violation_per_out_of_range_compute() {
        let telemetry = GravityCompensationTelemetry::default();
        let mut model = QuasiStaticTorqueModel::for_tests_with_constant_output([1.0; 6]);
        model.training_range.q_min_rad = [-0.1; 6];
        model.training_range.q_max_rad = [0.1; 6];
        let compensator = GravityCompensator::for_tests_with_telemetry(
            None,
            Some(model),
            GravityCompensationSettings {
                reflection_compensation: true,
                master_assist_ratio: 0.0,
                slave_assist_ratio: 0.5,
            },
            telemetry.clone(),
        );
        let mut snapshot = dual_arm_snapshot_with_slave_torque([3.0; 6]);
        snapshot.right.state.position = JointArray::splat(Rad(0.2));

        let _ = compute_once(compensator, &snapshot);

        assert_eq!(telemetry.range_violations(), 1);
    }

    #[test]
    fn telemetry_does_not_count_in_range_compute() {
        let telemetry = GravityCompensationTelemetry::default();
        let mut model = QuasiStaticTorqueModel::for_tests_with_constant_output([1.0; 6]);
        model.training_range.q_min_rad = [-0.1; 6];
        model.training_range.q_max_rad = [0.1; 6];
        let compensator = GravityCompensator::for_tests_with_telemetry(
            None,
            Some(model),
            GravityCompensationSettings {
                reflection_compensation: true,
                master_assist_ratio: 0.0,
                slave_assist_ratio: 0.5,
            },
            telemetry.clone(),
        );
        let snapshot = dual_arm_snapshot_with_slave_torque([3.0; 6]);

        let _ = compute_once(compensator, &snapshot);

        assert_eq!(telemetry.range_violations(), 0);
    }

    fn compute_once(
        mut compensator: GravityCompensator,
        snapshot: &DualArmSnapshot,
    ) -> piper_client::dual_arm::BilateralDynamicsCompensation {
        compute_with_dt(&mut compensator, snapshot, Duration::from_millis(10))
    }

    fn compute_once_with_dt(
        mut compensator: GravityCompensator,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> piper_client::dual_arm::BilateralDynamicsCompensation {
        compute_with_dt(&mut compensator, snapshot, dt)
    }

    fn compute_with_dt(
        compensator: &mut GravityCompensator,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> piper_client::dual_arm::BilateralDynamicsCompensation {
        compensator.compute(snapshot, dt).expect("compensation should compute")
    }

    fn assert_array_close(actual: [f64; 6], expected: [f64; 6]) {
        for joint in 0..6 {
            assert!(
                (actual[joint] - expected[joint]).abs() < 1e-12,
                "joint {joint}: expected {}, got {}",
                expected[joint],
                actual[joint]
            );
        }
    }

    fn dual_arm_snapshot_for_tests() -> DualArmSnapshot {
        dual_arm_snapshot_with_slave_torque([0.0; 6])
    }

    fn dual_arm_snapshot_with_slave_torque(torque_nm: [f64; 6]) -> DualArmSnapshot {
        DualArmSnapshot {
            left: control_snapshot(JointArray::splat(Rad(0.0)), [0.0; 6]),
            right: control_snapshot(JointArray::splat(Rad(0.0)), torque_nm),
            inter_arm_skew: Duration::ZERO,
            host_cycle_timestamp: Instant::now(),
        }
    }

    fn control_snapshot(position: JointArray<Rad>, torque_nm: [f64; 6]) -> ControlSnapshotFull {
        ControlSnapshotFull {
            state: ControlSnapshot {
                position,
                velocity: JointArray::splat(RadPerSecond(0.0)),
                torque: JointArray::new(torque_nm.map(NewtonMeter)),
                position_timestamp_us: 0,
                dynamic_timestamp_us: 0,
                skew_us: 0,
            },
            position_host_rx_mono_us: 0,
            dynamic_host_rx_mono_us: 0,
            feedback_age: Duration::ZERO,
        }
    }
}
