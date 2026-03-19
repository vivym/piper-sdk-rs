//! Dual-arm MuJoCo dynamics compensation bridge.
//!
//! This module keeps MuJoCo-specific logic inside the addon crate while
//! implementing the generic dual-arm compensation hook exposed by `piper-sdk`.

use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use nalgebra::Vector3;
use piper_sdk::client::types::{JointArray, NewtonMeter, RadPerSecond};
use piper_sdk::client::{ControlSnapshotFull, DualArmSnapshot};
use piper_sdk::{BilateralDynamicsCompensation, BilateralDynamicsCompensator};

use crate::{
    GravityCompensation, JointState, JointTorques, MujocoGravityCompensation, PhysicsError,
};

/// Dynamics compensation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DynamicsMode {
    /// Gravity-only compensation.
    #[default]
    PureGravity,
    /// Gravity plus velocity-dependent bias terms.
    PartialInverseDynamics,
    /// Full inverse dynamics with estimated acceleration.
    FullInverseDynamics,
}

impl fmt::Display for DynamicsMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            DynamicsMode::PureGravity => "gravity",
            DynamicsMode::PartialInverseDynamics => "partial",
            DynamicsMode::FullInverseDynamics => "full",
        };
        f.write_str(value)
    }
}

impl FromStr for DynamicsMode {
    type Err = PhysicsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gravity" | "pure-gravity" | "pure_gravity" => Ok(Self::PureGravity),
            "partial" | "partial-id" | "partial_inverse_dynamics" => {
                Ok(Self::PartialInverseDynamics)
            },
            "full" | "full-id" | "full_inverse_dynamics" => Ok(Self::FullInverseDynamics),
            _ => Err(PhysicsError::InvalidInput(format!(
                "unknown dynamics mode: {value}"
            ))),
        }
    }
}

/// Payload description in kilograms and meters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PayloadSpec {
    /// Payload mass in kilograms.
    pub mass_kg: f64,
    /// Payload center of mass in the end-effector local frame.
    pub com_m: [f64; 3],
}

impl Default for PayloadSpec {
    fn default() -> Self {
        Self {
            mass_kg: 0.0,
            com_m: [0.0; 3],
        }
    }
}

impl PayloadSpec {
    /// Create and validate a payload specification.
    pub fn try_new(mass_kg: f64, com_m: [f64; 3]) -> Result<Self, PhysicsError> {
        let spec = Self { mass_kg, com_m };
        spec.validate()?;
        Ok(spec)
    }

    fn validate(self) -> Result<(), PhysicsError> {
        if !self.mass_kg.is_finite() || self.mass_kg < 0.0 {
            return Err(PhysicsError::InvalidInput(format!(
                "payload mass must be finite and >= 0, got {}",
                self.mass_kg
            )));
        }
        if self.com_m.iter().any(|value| !value.is_finite()) {
            return Err(PhysicsError::InvalidInput(
                "payload COM must contain only finite values".to_string(),
            ));
        }
        Ok(())
    }

    fn is_empty(self) -> bool {
        self.mass_kg <= f64::EPSILON
    }

    fn com_vector(self) -> Vector3<f64> {
        Vector3::new(self.com_m[0], self.com_m[1], self.com_m[2])
    }
}

/// Shared payload state used by the runtime console and the compensator.
#[derive(Debug, Clone, Default)]
pub struct SharedPayloadState {
    inner: Arc<RwLock<PayloadSpec>>,
}

impl SharedPayloadState {
    /// Create a new shared payload state.
    pub fn try_new(spec: PayloadSpec) -> Result<Self, PhysicsError> {
        spec.validate()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(spec)),
        })
    }

    /// Read the latest payload value.
    pub fn load(&self) -> PayloadSpec {
        *read_lock(&self.inner)
    }

    /// Overwrite the payload value.
    pub fn store(&self, spec: PayloadSpec) -> Result<(), PhysicsError> {
        spec.validate()?;
        *write_lock(&self.inner) = spec;
        Ok(())
    }
}

/// Shared mode state used by the runtime console and the compensator.
#[derive(Debug, Clone, Default)]
pub struct SharedModeState {
    inner: Arc<RwLock<DynamicsMode>>,
}

impl SharedModeState {
    /// Create a new shared mode state.
    pub fn new(mode: DynamicsMode) -> Self {
        Self {
            inner: Arc::new(RwLock::new(mode)),
        }
    }

    /// Read the latest mode value.
    pub fn load(&self) -> DynamicsMode {
        *read_lock(&self.inner)
    }

    /// Overwrite the mode value.
    pub fn store(&self, mode: DynamicsMode) {
        *write_lock(&self.inner) = mode;
    }
}

/// Configuration shared by the dual-arm MuJoCo compensator.
#[derive(Debug, Clone)]
pub struct DualArmMujocoCompensatorConfig {
    /// Runtime-selectable master-arm mode.
    pub master_mode: SharedModeState,
    /// Runtime-selectable slave-arm mode.
    pub slave_mode: SharedModeState,
    /// Runtime-selectable master-arm payload.
    pub master_payload: SharedPayloadState,
    /// Runtime-selectable slave-arm payload.
    pub slave_payload: SharedPayloadState,
    /// Low-pass cutoff for finite-difference acceleration estimation.
    pub qacc_lpf_cutoff_hz: f64,
    /// Per-joint absolute acceleration clamp.
    pub max_abs_qacc: f64,
}

impl Default for DualArmMujocoCompensatorConfig {
    fn default() -> Self {
        Self {
            master_mode: SharedModeState::new(DynamicsMode::PureGravity),
            slave_mode: SharedModeState::new(DynamicsMode::PartialInverseDynamics),
            master_payload: SharedPayloadState::default(),
            slave_payload: SharedPayloadState::default(),
            qacc_lpf_cutoff_hz: 20.0,
            max_abs_qacc: 50.0,
        }
    }
}

impl DualArmMujocoCompensatorConfig {
    fn validate(&self) -> Result<(), PhysicsError> {
        if !self.qacc_lpf_cutoff_hz.is_finite() || self.qacc_lpf_cutoff_hz < 0.0 {
            return Err(PhysicsError::InvalidInput(format!(
                "qacc_lpf_cutoff_hz must be finite and >= 0, got {}",
                self.qacc_lpf_cutoff_hz
            )));
        }
        if !self.max_abs_qacc.is_finite() || self.max_abs_qacc < 0.0 {
            return Err(PhysicsError::InvalidInput(format!(
                "max_abs_qacc must be finite and >= 0, got {}",
                self.max_abs_qacc
            )));
        }
        self.master_payload.load().validate()?;
        self.slave_payload.load().validate()?;
        Ok(())
    }
}

/// Dual-arm MuJoCo compensator backed by two independent calculators.
pub struct MujocoDualArmCompensator {
    core: DualArmCompensatorCore<MujocoGravityCompensation>,
}

impl MujocoDualArmCompensator {
    /// Build two independent calculators from a model directory.
    pub fn from_model_dir_pair(
        dir: &Path,
        config: DualArmMujocoCompensatorConfig,
    ) -> Result<Self, PhysicsError> {
        let master = MujocoGravityCompensation::from_model_dir(dir)?;
        let slave = MujocoGravityCompensation::from_model_dir(dir)?;
        Ok(Self {
            core: DualArmCompensatorCore::new(master, slave, config)?,
        })
    }

    /// Build two independent calculators from the standard search path.
    pub fn from_standard_path_pair(
        config: DualArmMujocoCompensatorConfig,
    ) -> Result<Self, PhysicsError> {
        let master = MujocoGravityCompensation::from_standard_path()?;
        let slave = MujocoGravityCompensation::from_standard_path()?;
        Ok(Self {
            core: DualArmCompensatorCore::new(master, slave, config)?,
        })
    }

    /// Build two independent calculators from the embedded XML.
    pub fn from_embedded_pair(
        config: DualArmMujocoCompensatorConfig,
    ) -> Result<Self, PhysicsError> {
        let master = MujocoGravityCompensation::from_embedded()?;
        let slave = MujocoGravityCompensation::from_embedded()?;
        Ok(Self {
            core: DualArmCompensatorCore::new(master, slave, config)?,
        })
    }

    /// Shared master-arm mode handle.
    pub fn master_mode_state(&self) -> SharedModeState {
        self.core.config.master_mode.clone()
    }

    /// Shared slave-arm mode handle.
    pub fn slave_mode_state(&self) -> SharedModeState {
        self.core.config.slave_mode.clone()
    }

    /// Shared master-arm payload handle.
    pub fn master_payload_state(&self) -> SharedPayloadState {
        self.core.config.master_payload.clone()
    }

    /// Shared slave-arm payload handle.
    pub fn slave_payload_state(&self) -> SharedPayloadState {
        self.core.config.slave_payload.clone()
    }
}

impl BilateralDynamicsCompensator for MujocoDualArmCompensator {
    type Error = PhysicsError;

    fn compute(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> Result<BilateralDynamicsCompensation, Self::Error> {
        self.core.compute(snapshot, dt)
    }

    fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
        self.core.reset_internal();
        Ok(())
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        self.core.reset_internal();
        Ok(())
    }
}

trait ArmDynamicsEngine {
    fn compute_gravity_compensation(
        &mut self,
        q: &JointState,
    ) -> Result<JointTorques, PhysicsError>;

    fn compute_partial_inverse_dynamics(
        &mut self,
        q: &JointState,
        qvel: &[f64; 6],
    ) -> Result<JointTorques, PhysicsError>;

    fn compute_inverse_dynamics(
        &mut self,
        q: &JointState,
        qvel: &[f64; 6],
        qacc: &[f64; 6],
    ) -> Result<JointTorques, PhysicsError>;

    fn compute_gravity_torques_with_payload(
        &mut self,
        q: &JointState,
        payload_mass: f64,
        payload_com: Vector3<f64>,
    ) -> Result<JointTorques, PhysicsError>;
}

impl ArmDynamicsEngine for MujocoGravityCompensation {
    fn compute_gravity_compensation(
        &mut self,
        q: &JointState,
    ) -> Result<JointTorques, PhysicsError> {
        GravityCompensation::compute_gravity_compensation(self, q)
    }

    fn compute_partial_inverse_dynamics(
        &mut self,
        q: &JointState,
        qvel: &[f64; 6],
    ) -> Result<JointTorques, PhysicsError> {
        GravityCompensation::compute_partial_inverse_dynamics(self, q, qvel)
    }

    fn compute_inverse_dynamics(
        &mut self,
        q: &JointState,
        qvel: &[f64; 6],
        qacc: &[f64; 6],
    ) -> Result<JointTorques, PhysicsError> {
        GravityCompensation::compute_inverse_dynamics(self, q, qvel, qacc)
    }

    fn compute_gravity_torques_with_payload(
        &mut self,
        q: &JointState,
        payload_mass: f64,
        payload_com: Vector3<f64>,
    ) -> Result<JointTorques, PhysicsError> {
        MujocoGravityCompensation::compute_gravity_torques_with_payload(
            self,
            q,
            payload_mass,
            payload_com,
        )
    }
}

struct DualArmCompensatorCore<E> {
    master: E,
    slave: E,
    config: DualArmMujocoCompensatorConfig,
    master_state: ArmAccelerationState,
    slave_state: ArmAccelerationState,
}

#[derive(Clone, Copy)]
struct AccelEstimateConfig {
    qacc_lpf_cutoff_hz: f64,
    max_abs_qacc: f64,
}

struct ArmRuntimeConfig<'a> {
    mode_state: &'a SharedModeState,
    payload_state: &'a SharedPayloadState,
}

impl<E> DualArmCompensatorCore<E>
where
    E: ArmDynamicsEngine,
{
    fn new(
        master: E,
        slave: E,
        config: DualArmMujocoCompensatorConfig,
    ) -> Result<Self, PhysicsError> {
        config.validate()?;
        Ok(Self {
            master,
            slave,
            config,
            master_state: ArmAccelerationState::default(),
            slave_state: ArmAccelerationState::default(),
        })
    }

    fn compute(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> Result<BilateralDynamicsCompensation, PhysicsError> {
        self.config.validate()?;
        let master = Self::compute_arm(
            &mut self.master,
            ArmRuntimeConfig {
                mode_state: &self.config.master_mode,
                payload_state: &self.config.master_payload,
            },
            &mut self.master_state,
            &snapshot.left,
            dt,
            AccelEstimateConfig {
                qacc_lpf_cutoff_hz: self.config.qacc_lpf_cutoff_hz,
                max_abs_qacc: self.config.max_abs_qacc,
            },
        )?;
        let slave = Self::compute_arm(
            &mut self.slave,
            ArmRuntimeConfig {
                mode_state: &self.config.slave_mode,
                payload_state: &self.config.slave_payload,
            },
            &mut self.slave_state,
            &snapshot.right,
            dt,
            AccelEstimateConfig {
                qacc_lpf_cutoff_hz: self.config.qacc_lpf_cutoff_hz,
                max_abs_qacc: self.config.max_abs_qacc,
            },
        )?;

        Ok(BilateralDynamicsCompensation {
            master_model_torque: master.model_torque,
            slave_model_torque: slave.model_torque,
            master_external_torque_est: master.external_torque_est,
            slave_external_torque_est: slave.external_torque_est,
        })
    }

    fn reset_internal(&mut self) {
        self.master_state.reset();
        self.slave_state.reset();
    }

    fn compute_arm(
        engine: &mut E,
        runtime: ArmRuntimeConfig<'_>,
        state: &mut ArmAccelerationState,
        snapshot: &ControlSnapshotFull,
        dt: Duration,
        accel: AccelEstimateConfig,
    ) -> Result<ArmCompensation, PhysicsError> {
        let q = joint_state_from_snapshot(snapshot);
        let qvel = velocity_array(snapshot.state.velocity);
        let payload = runtime.payload_state.load();
        payload.validate()?;

        let model = match runtime.mode_state.load() {
            DynamicsMode::PureGravity => Self::compute_pure_gravity(engine, &q, payload)?,
            DynamicsMode::PartialInverseDynamics => {
                Self::compute_partial_id(engine, &q, &qvel, payload)?
            },
            DynamicsMode::FullInverseDynamics => {
                let qacc =
                    state.estimate_qacc(qvel, dt, accel.qacc_lpf_cutoff_hz, accel.max_abs_qacc);
                Self::compute_full_id(engine, &q, &qvel, &qacc, payload)?
            },
        };

        let model_torque = joint_array_from_torques(model);
        let measured = snapshot.state.torque.into_array();
        let external_torque_est = JointArray::from(std::array::from_fn(|index| {
            NewtonMeter(measured[index].0 - model[index])
        }));

        Ok(ArmCompensation {
            model_torque,
            external_torque_est,
        })
    }

    fn compute_pure_gravity(
        engine: &mut E,
        q: &JointState,
        payload: PayloadSpec,
    ) -> Result<JointTorques, PhysicsError> {
        if payload.is_empty() {
            engine.compute_gravity_compensation(q)
        } else {
            engine.compute_gravity_torques_with_payload(q, payload.mass_kg, payload.com_vector())
        }
    }

    fn compute_partial_id(
        engine: &mut E,
        q: &JointState,
        qvel: &[f64; 6],
        payload: PayloadSpec,
    ) -> Result<JointTorques, PhysicsError> {
        let base = engine.compute_partial_inverse_dynamics(q, qvel)?;
        if payload.is_empty() {
            return Ok(base);
        }

        let gravity_without_payload = engine.compute_gravity_compensation(q)?;
        let gravity_with_payload = engine.compute_gravity_torques_with_payload(
            q,
            payload.mass_kg,
            payload.com_vector(),
        )?;
        Ok(base + (gravity_with_payload - gravity_without_payload))
    }

    fn compute_full_id(
        engine: &mut E,
        q: &JointState,
        qvel: &[f64; 6],
        qacc: &[f64; 6],
        payload: PayloadSpec,
    ) -> Result<JointTorques, PhysicsError> {
        let base = engine.compute_inverse_dynamics(q, qvel, qacc)?;
        if payload.is_empty() {
            return Ok(base);
        }

        let gravity_without_payload = engine.compute_gravity_compensation(q)?;
        let gravity_with_payload = engine.compute_gravity_torques_with_payload(
            q,
            payload.mass_kg,
            payload.com_vector(),
        )?;
        Ok(base + (gravity_with_payload - gravity_without_payload))
    }
}

#[derive(Debug, Default)]
struct ArmAccelerationState {
    prev_velocity: Option<[f64; 6]>,
    filtered_qacc: [f64; 6],
}

impl ArmAccelerationState {
    fn estimate_qacc(
        &mut self,
        qvel: [f64; 6],
        dt: Duration,
        cutoff_hz: f64,
        max_abs_qacc: f64,
    ) -> [f64; 6] {
        let dt_sec = dt.as_secs_f64();
        if dt_sec <= f64::EPSILON {
            self.prev_velocity = Some(qvel);
            self.filtered_qacc = [0.0; 6];
            return self.filtered_qacc;
        }

        let alpha = if cutoff_hz > 0.0 {
            let rc = 1.0 / (2.0 * std::f64::consts::PI * cutoff_hz);
            dt_sec / (rc + dt_sec)
        } else {
            1.0
        };
        let limit = max_abs_qacc.abs();

        let estimated = if let Some(prev_velocity) = self.prev_velocity {
            std::array::from_fn(|index| {
                let raw = (qvel[index] - prev_velocity[index]) / dt_sec;
                let filtered =
                    self.filtered_qacc[index] + alpha * (raw - self.filtered_qacc[index]);
                filtered.clamp(-limit, limit)
            })
        } else {
            [0.0; 6]
        };

        self.prev_velocity = Some(qvel);
        self.filtered_qacc = estimated;
        estimated
    }

    fn reset(&mut self) {
        self.prev_velocity = None;
        self.filtered_qacc = [0.0; 6];
    }
}

struct ArmCompensation {
    model_torque: JointArray<NewtonMeter>,
    external_torque_est: JointArray<NewtonMeter>,
}

fn joint_state_from_snapshot(snapshot: &ControlSnapshotFull) -> JointState {
    let positions = snapshot.state.position.into_array().map(|position| position.0);
    JointState::from_row_slice(&positions)
}

fn velocity_array(velocity: JointArray<RadPerSecond>) -> [f64; 6] {
    velocity.into_array().map(|value| value.0)
}

fn joint_array_from_torques(torques: JointTorques) -> JointArray<NewtonMeter> {
    JointArray::from(std::array::from_fn(|index| NewtonMeter(torques[index])))
}

fn read_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_sdk::client::ControlSnapshot;
    use piper_sdk::client::types::{Rad, RadPerSecond};
    use std::sync::Mutex;
    use std::time::Instant;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum CallKind {
        Gravity,
        Partial,
        Full,
        Payload,
    }

    #[derive(Clone)]
    struct FakeEngine {
        gravity: JointTorques,
        partial: JointTorques,
        payload_gravity: JointTorques,
        calls: Arc<Mutex<Vec<CallKind>>>,
    }

    impl FakeEngine {
        fn new(gravity: f64, partial: f64, payload_gravity: f64) -> Self {
            Self {
                gravity: JointTorques::repeat(gravity),
                partial: JointTorques::repeat(partial),
                payload_gravity: JointTorques::repeat(payload_gravity),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl ArmDynamicsEngine for FakeEngine {
        fn compute_gravity_compensation(
            &mut self,
            _q: &JointState,
        ) -> Result<JointTorques, PhysicsError> {
            self.calls.lock().expect("call log").push(CallKind::Gravity);
            Ok(self.gravity)
        }

        fn compute_partial_inverse_dynamics(
            &mut self,
            _q: &JointState,
            _qvel: &[f64; 6],
        ) -> Result<JointTorques, PhysicsError> {
            self.calls.lock().expect("call log").push(CallKind::Partial);
            Ok(self.partial)
        }

        fn compute_inverse_dynamics(
            &mut self,
            _q: &JointState,
            _qvel: &[f64; 6],
            qacc: &[f64; 6],
        ) -> Result<JointTorques, PhysicsError> {
            self.calls.lock().expect("call log").push(CallKind::Full);
            Ok(JointTorques::from_row_slice(qacc))
        }

        fn compute_gravity_torques_with_payload(
            &mut self,
            _q: &JointState,
            _payload_mass: f64,
            _payload_com: Vector3<f64>,
        ) -> Result<JointTorques, PhysicsError> {
            self.calls.lock().expect("call log").push(CallKind::Payload);
            Ok(self.payload_gravity)
        }
    }

    fn snapshot_with_torque(velocity: f64, measured_torque: f64) -> DualArmSnapshot {
        let full = ControlSnapshotFull {
            state: ControlSnapshot {
                position: JointArray::splat(Rad(0.0)),
                velocity: JointArray::splat(RadPerSecond(velocity)),
                torque: JointArray::splat(NewtonMeter(measured_torque)),
                position_timestamp_us: 1,
                dynamic_timestamp_us: 1,
                skew_us: 0,
            },
            position_system_timestamp_us: 1,
            dynamic_system_timestamp_us: 1,
            feedback_age: Duration::from_millis(1),
        };

        DualArmSnapshot {
            left: full,
            right: full,
            inter_arm_skew: Duration::ZERO,
            host_cycle_timestamp: Instant::now(),
        }
    }

    #[test]
    fn test_modes_dispatch_independently() {
        let master = FakeEngine::new(1.0, 2.0, 4.0);
        let slave = FakeEngine::new(10.0, 20.0, 40.0);
        let config = DualArmMujocoCompensatorConfig {
            master_mode: SharedModeState::new(DynamicsMode::PureGravity),
            slave_mode: SharedModeState::new(DynamicsMode::PartialInverseDynamics),
            master_payload: SharedPayloadState::default(),
            slave_payload: SharedPayloadState::default(),
            qacc_lpf_cutoff_hz: 0.0,
            max_abs_qacc: 100.0,
        };
        let master_calls = master.calls.clone();
        let slave_calls = slave.calls.clone();
        let mut compensator =
            DualArmCompensatorCore::new(master, slave, config).expect("core should build");

        let output = compensator
            .compute(&snapshot_with_torque(0.0, 5.0), Duration::from_millis(5))
            .expect("compute should succeed");

        assert_eq!(
            output.master_model_torque,
            JointArray::splat(NewtonMeter(1.0))
        );
        assert_eq!(
            output.slave_model_torque,
            JointArray::splat(NewtonMeter(20.0))
        );
        assert_eq!(
            output.master_external_torque_est,
            JointArray::splat(NewtonMeter(4.0))
        );
        assert_eq!(
            output.slave_external_torque_est,
            JointArray::splat(NewtonMeter(-15.0))
        );
        assert_eq!(
            master_calls.lock().expect("master calls").as_slice(),
            &[CallKind::Gravity]
        );
        assert_eq!(
            slave_calls.lock().expect("slave calls").as_slice(),
            &[CallKind::Partial]
        );
    }

    #[test]
    fn test_partial_mode_adds_payload_delta() {
        let master = FakeEngine::new(1.0, 10.0, 4.0);
        let slave = FakeEngine::new(1.0, 10.0, 4.0);
        let payload = SharedPayloadState::try_new(
            PayloadSpec::try_new(0.5, [0.0, 0.0, 0.0]).expect("payload should be valid"),
        )
        .expect("shared payload should build");
        let config = DualArmMujocoCompensatorConfig {
            master_mode: SharedModeState::new(DynamicsMode::PartialInverseDynamics),
            slave_mode: SharedModeState::new(DynamicsMode::PartialInverseDynamics),
            master_payload: payload.clone(),
            slave_payload: SharedPayloadState::default(),
            qacc_lpf_cutoff_hz: 0.0,
            max_abs_qacc: 100.0,
        };
        let mut compensator =
            DualArmCompensatorCore::new(master, slave, config).expect("core should build");

        let output = compensator
            .compute(&snapshot_with_torque(0.0, 20.0), Duration::from_millis(5))
            .expect("compute should succeed");

        assert_eq!(
            output.master_model_torque,
            JointArray::splat(NewtonMeter(13.0))
        );
        assert_eq!(
            output.slave_model_torque,
            JointArray::splat(NewtonMeter(10.0))
        );
    }

    #[test]
    fn test_hot_updates_take_effect_without_rebuild() {
        let master = FakeEngine::new(1.0, 10.0, 4.0);
        let slave = FakeEngine::new(1.0, 10.0, 4.0);
        let master_mode = SharedModeState::new(DynamicsMode::PureGravity);
        let master_payload = SharedPayloadState::default();
        let config = DualArmMujocoCompensatorConfig {
            master_mode: master_mode.clone(),
            slave_mode: SharedModeState::new(DynamicsMode::PureGravity),
            master_payload: master_payload.clone(),
            slave_payload: SharedPayloadState::default(),
            qacc_lpf_cutoff_hz: 0.0,
            max_abs_qacc: 100.0,
        };
        let mut compensator =
            DualArmCompensatorCore::new(master, slave, config).expect("core should build");

        let first = compensator
            .compute(&snapshot_with_torque(0.0, 20.0), Duration::from_millis(5))
            .expect("first compute should succeed");
        assert_eq!(
            first.master_model_torque,
            JointArray::splat(NewtonMeter(1.0))
        );

        master_mode.store(DynamicsMode::PartialInverseDynamics);
        master_payload
            .store(PayloadSpec::try_new(0.5, [0.0, 0.0, 0.0]).expect("payload should be valid"))
            .expect("payload update should succeed");

        let second = compensator
            .compute(&snapshot_with_torque(0.0, 20.0), Duration::from_millis(5))
            .expect("second compute should succeed");
        assert_eq!(
            second.master_model_torque,
            JointArray::splat(NewtonMeter(13.0))
        );
    }

    #[test]
    fn test_full_mode_uses_filtered_and_clamped_qacc() {
        let master = FakeEngine::new(1.0, 10.0, 4.0);
        let slave = FakeEngine::new(1.0, 10.0, 4.0);
        let config = DualArmMujocoCompensatorConfig {
            master_mode: SharedModeState::new(DynamicsMode::FullInverseDynamics),
            slave_mode: SharedModeState::new(DynamicsMode::PureGravity),
            master_payload: SharedPayloadState::default(),
            slave_payload: SharedPayloadState::default(),
            qacc_lpf_cutoff_hz: 0.0,
            max_abs_qacc: 3.0,
        };
        let mut compensator =
            DualArmCompensatorCore::new(master, slave, config).expect("core should build");

        let first = compensator
            .compute(
                &snapshot_with_torque(0.0, 0.0),
                Duration::from_secs_f64(0.1),
            )
            .expect("first compute should succeed");
        assert_eq!(
            first.master_model_torque,
            JointArray::splat(NewtonMeter(0.0))
        );

        let second = compensator
            .compute(
                &snapshot_with_torque(10.0, 0.0),
                Duration::from_secs_f64(0.1),
            )
            .expect("second compute should succeed");
        assert_eq!(
            second.master_model_torque,
            JointArray::splat(NewtonMeter(3.0))
        );
    }

    #[test]
    fn test_on_time_jump_resets_acceleration_estimator() {
        let master = FakeEngine::new(1.0, 10.0, 4.0);
        let slave = FakeEngine::new(1.0, 10.0, 4.0);
        let config = DualArmMujocoCompensatorConfig {
            master_mode: SharedModeState::new(DynamicsMode::FullInverseDynamics),
            slave_mode: SharedModeState::new(DynamicsMode::PureGravity),
            master_payload: SharedPayloadState::default(),
            slave_payload: SharedPayloadState::default(),
            qacc_lpf_cutoff_hz: 0.0,
            max_abs_qacc: 100.0,
        };
        let mut compensator =
            DualArmCompensatorCore::new(master, slave, config).expect("core should build");

        let _ = compensator
            .compute(
                &snapshot_with_torque(4.0, 0.0),
                Duration::from_secs_f64(0.1),
            )
            .expect("first compute should succeed");
        compensator.reset_internal();
        let second = compensator
            .compute(
                &snapshot_with_torque(8.0, 0.0),
                Duration::from_secs_f64(0.1),
            )
            .expect("second compute should succeed");

        assert_eq!(
            second.master_model_torque,
            JointArray::splat(NewtonMeter(0.0))
        );
    }

    #[test]
    fn test_dynamics_mode_parser_accepts_cli_strings() {
        assert_eq!(
            "gravity".parse::<DynamicsMode>().expect("gravity should parse"),
            DynamicsMode::PureGravity
        );
        assert_eq!(
            "partial".parse::<DynamicsMode>().expect("partial should parse"),
            DynamicsMode::PartialInverseDynamics
        );
        assert_eq!(
            "full".parse::<DynamicsMode>().expect("full should parse"),
            DynamicsMode::FullInverseDynamics
        );
    }

    #[test]
    fn test_payload_spec_rejects_invalid_mass() {
        let error =
            PayloadSpec::try_new(-1.0, [0.0, 0.0, 0.0]).expect_err("negative mass should fail");
        assert!(matches!(error, PhysicsError::InvalidInput(_)));
    }

    #[test]
    fn test_joint_mirror_map_linked_sdk_builds() {
        let map = piper_sdk::JointMirrorMap::left_right_mirror();
        assert_eq!(map.position_sign[0], -1.0);
    }
}
