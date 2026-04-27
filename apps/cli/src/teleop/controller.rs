#![allow(dead_code)]

use crate::teleop::config::{TeleopControlSettings, TeleopMode};
use anyhow::Result;
use piper_client::dual_arm::{
    BilateralCommand, BilateralControlFrame, BilateralController, DualArmCalibration,
    DualArmSnapshot,
};
use piper_client::types::{JointArray, NewtonMeter};
use std::convert::Infallible;
use std::sync::{Arc, RwLock};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeTeleopSettings {
    pub calibration: DualArmCalibration,
    pub mode: TeleopMode,
    pub track_kp: f64,
    pub track_kd: f64,
    pub master_damping: f64,
    pub reflection_gain: f64,
}

impl RuntimeTeleopSettings {
    pub fn production(calibration: DualArmCalibration) -> Self {
        Self {
            calibration,
            mode: TeleopMode::MasterFollower,
            track_kp: 8.0,
            track_kd: 1.0,
            master_damping: 0.4,
            reflection_gain: 0.25,
        }
    }

    pub fn with_mode(mut self, mode: TeleopMode) -> Result<Self> {
        self.mode = mode;
        self.validate()?;
        Ok(self)
    }

    pub fn with_track_gains(mut self, kp: f64, kd: f64) -> Result<Self> {
        self.track_kp = kp;
        self.track_kd = kd;
        self.validate()?;
        Ok(self)
    }

    pub fn with_master_damping(mut self, damping: f64) -> Result<Self> {
        self.master_damping = damping;
        self.validate()?;
        Ok(self)
    }

    pub fn with_reflection_gain(mut self, gain: f64) -> Result<Self> {
        self.reflection_gain = gain;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<()> {
        self.as_control_settings().validate()
    }

    fn as_control_settings(&self) -> TeleopControlSettings {
        TeleopControlSettings {
            mode: self.mode,
            track_kp: self.track_kp,
            track_kd: self.track_kd,
            master_damping: self.master_damping,
            reflection_gain: self.reflection_gain,
            ..TeleopControlSettings::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeTeleopSettingsHandle {
    inner: Arc<RwLock<RuntimeTeleopSettings>>,
}

impl RuntimeTeleopSettingsHandle {
    pub fn new(settings: RuntimeTeleopSettings) -> Self {
        settings.validate().expect("runtime teleop settings must satisfy hard caps");
        Self {
            inner: Arc::new(RwLock::new(settings)),
        }
    }

    pub fn snapshot(&self) -> RuntimeTeleopSettings {
        match self.inner.read() {
            Ok(settings) => settings.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn update_mode(&self, mode: TeleopMode) -> Result<()> {
        self.update(|settings| settings.mode = mode)
    }

    pub fn update_track_gains(&self, kp: f64, kd: f64) -> Result<()> {
        self.update(|settings| {
            settings.track_kp = kp;
            settings.track_kd = kd;
        })
    }

    pub fn update_master_damping(&self, damping: f64) -> Result<()> {
        self.update(|settings| settings.master_damping = damping)
    }

    pub fn update_reflection_gain(&self, gain: f64) -> Result<()> {
        self.update(|settings| settings.reflection_gain = gain)
    }

    fn update(&self, mutate: impl FnOnce(&mut RuntimeTeleopSettings)) -> Result<()> {
        let mut guard = match self.inner.write() {
            Ok(settings) => settings,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut next = guard.clone();
        mutate(&mut next);
        next.validate()?;
        *guard = next;
        Ok(())
    }
}

pub struct RuntimeTeleopController {
    settings: RuntimeTeleopSettingsHandle,
}

impl RuntimeTeleopController {
    pub fn new(settings: RuntimeTeleopSettingsHandle) -> Self {
        Self { settings }
    }
}

impl BilateralController for RuntimeTeleopController {
    type Error = Infallible;

    fn tick(
        &mut self,
        snapshot: &DualArmSnapshot,
        _dt: Duration,
    ) -> std::result::Result<BilateralCommand, Self::Error> {
        Ok(command_from_settings(
            &self.settings.snapshot(),
            snapshot,
            snapshot.right.state.torque,
        ))
    }

    fn tick_with_compensation(
        &mut self,
        frame: &BilateralControlFrame,
        _dt: Duration,
    ) -> std::result::Result<BilateralCommand, Self::Error> {
        let settings = self.settings.snapshot();
        let torque_source = if settings.mode == TeleopMode::Bilateral {
            frame
                .compensation
                .map(|compensation| compensation.slave_external_torque_est)
                .unwrap_or(frame.snapshot.right.state.torque)
        } else {
            frame.snapshot.right.state.torque
        };

        Ok(command_from_settings(
            &settings,
            &frame.snapshot,
            torque_source,
        ))
    }
}

fn command_from_settings(
    settings: &RuntimeTeleopSettings,
    snapshot: &DualArmSnapshot,
    torque_source: JointArray<NewtonMeter>,
) -> BilateralCommand {
    let master_interaction_torque = match settings.mode {
        TeleopMode::MasterFollower => JointArray::splat(NewtonMeter::ZERO),
        TeleopMode::Bilateral => settings
            .calibration
            .slave_to_master_torque(torque_source)
            .map_with(JointArray::splat(settings.reflection_gain), |tau, gain| {
                NewtonMeter(-tau.0 * gain)
            }),
    };

    BilateralCommand {
        slave_position: settings.calibration.master_to_slave_position(snapshot.left.state.position),
        slave_velocity: settings.calibration.master_to_slave_velocity(snapshot.left.state.velocity),
        slave_kp: JointArray::splat(settings.track_kp),
        slave_kd: JointArray::splat(settings.track_kd),
        slave_feedforward_torque: JointArray::splat(NewtonMeter::ZERO),
        master_position: snapshot.left.state.position,
        master_velocity: JointArray::splat(0.0),
        master_kp: JointArray::splat(0.0),
        master_kd: JointArray::splat(settings.master_damping),
        master_interaction_torque,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teleop::config::TeleopMode;
    use piper_client::dual_arm::{
        BilateralControlFrame, BilateralController, BilateralDynamicsCompensation,
        DualArmCalibration, DualArmSnapshot, JointMirrorMap, JointSpaceBilateralController,
        MasterFollowerController,
    };
    use piper_client::observer::{ControlSnapshot, ControlSnapshotFull};
    use piper_client::types::{Joint, JointArray, NewtonMeter, Rad, RadPerSecond};
    use std::time::{Duration, Instant};

    #[test]
    fn runtime_controller_matches_master_follower_controller() {
        let calibration = sample_calibration();
        let settings = RuntimeTeleopSettings::production(calibration.clone())
            .with_mode(TeleopMode::MasterFollower)
            .unwrap()
            .with_track_gains(8.0, 1.0)
            .unwrap()
            .with_master_damping(0.4)
            .unwrap();
        let handle = RuntimeTeleopSettingsHandle::new(settings);
        let mut runtime = RuntimeTeleopController::new(handle);
        let mut reference = MasterFollowerController::new(calibration)
            .with_track_gains(JointArray::splat(8.0), JointArray::splat(1.0))
            .with_master_damping(JointArray::splat(0.4));
        let snapshot = sample_snapshot();

        assert_eq!(
            runtime.tick(&snapshot, Duration::from_millis(5)).unwrap(),
            reference.tick(&snapshot, Duration::from_millis(5)).unwrap()
        );
    }

    #[test]
    fn mode_update_affects_next_tick() {
        let handle = RuntimeTeleopSettingsHandle::new(RuntimeTeleopSettings::production(
            sample_calibration(),
        ));
        let mut controller = RuntimeTeleopController::new(handle.clone());

        handle.update_mode(TeleopMode::Bilateral).unwrap();
        let command = controller.tick(&sample_snapshot(), Duration::from_millis(5)).unwrap();

        assert!(command.master_interaction_torque.iter().any(|torque| torque.0 != 0.0));
    }

    #[test]
    fn runtime_controller_matches_joint_space_bilateral_controller() {
        let calibration = sample_calibration();
        let settings = RuntimeTeleopSettings::production(calibration.clone())
            .with_mode(TeleopMode::Bilateral)
            .unwrap()
            .with_track_gains(8.0, 1.0)
            .unwrap()
            .with_master_damping(0.4)
            .unwrap()
            .with_reflection_gain(0.25)
            .unwrap();
        let handle = RuntimeTeleopSettingsHandle::new(settings);
        let mut runtime = RuntimeTeleopController::new(handle);
        let mut reference = JointSpaceBilateralController::new(calibration)
            .with_track_gains(JointArray::splat(8.0), JointArray::splat(1.0))
            .with_master_damping(JointArray::splat(0.4))
            .with_reflection_gain(JointArray::splat(0.25));
        let snapshot = sample_snapshot();

        assert_eq!(
            runtime.tick(&snapshot, Duration::from_millis(5)).unwrap(),
            reference.tick(&snapshot, Duration::from_millis(5)).unwrap()
        );
    }

    #[test]
    fn tick_with_compensation_uses_external_slave_torque_in_bilateral_mode() {
        let handle = RuntimeTeleopSettingsHandle::new(RuntimeTeleopSettings::production(
            sample_calibration(),
        ));
        handle.update_mode(TeleopMode::Bilateral).unwrap();
        let mut controller = RuntimeTeleopController::new(handle);
        let snapshot = sample_snapshot();
        let compensation = BilateralDynamicsCompensation {
            slave_external_torque_est: JointArray::new([
                NewtonMeter(0.5),
                NewtonMeter(-0.6),
                NewtonMeter(0.7),
                NewtonMeter(-0.8),
                NewtonMeter(0.9),
                NewtonMeter(-1.0),
            ]),
            ..BilateralDynamicsCompensation::default()
        };

        let command = controller
            .tick_with_compensation(
                &BilateralControlFrame {
                    snapshot,
                    compensation: Some(compensation),
                },
                Duration::from_millis(5),
            )
            .unwrap();

        assert_eq!(
            command.master_interaction_torque,
            sample_calibration()
                .slave_to_master_torque(compensation.slave_external_torque_est)
                .map_with(JointArray::splat(0.25), |torque, gain| {
                    NewtonMeter(-torque.0 * gain)
                })
        );
    }

    #[test]
    fn invalid_update_returns_error_and_preserves_settings() {
        let handle = RuntimeTeleopSettingsHandle::new(RuntimeTeleopSettings::production(
            sample_calibration(),
        ));
        let before = handle.snapshot();

        assert!(handle.update_track_gains(21.0, 1.0).is_err());
        assert!(handle.update_master_damping(2.1).is_err());
        assert!(handle.update_reflection_gain(0.6).is_err());

        assert_eq!(handle.snapshot(), before);
    }

    fn sample_calibration() -> DualArmCalibration {
        DualArmCalibration {
            master_zero: JointArray::new([
                Rad(0.1),
                Rad(-0.2),
                Rad(0.3),
                Rad(-0.4),
                Rad(0.5),
                Rad(-0.6),
            ]),
            slave_zero: JointArray::new([
                Rad(-0.1),
                Rad(0.2),
                Rad(-0.3),
                Rad(0.4),
                Rad(-0.5),
                Rad(0.6),
            ]),
            map: JointMirrorMap {
                permutation: [
                    Joint::J2,
                    Joint::J1,
                    Joint::J4,
                    Joint::J3,
                    Joint::J6,
                    Joint::J5,
                ],
                position_sign: [-1.0, 1.0, -1.0, 1.0, -1.0, 1.0],
                velocity_sign: [-1.0, 1.0, -1.0, 1.0, -1.0, 1.0],
                torque_sign: [-1.0, 1.0, -1.0, 1.0, -1.0, 1.0],
            },
        }
    }

    fn sample_snapshot() -> DualArmSnapshot {
        snapshot(
            JointArray::new([
                Rad(0.7),
                Rad(-0.8),
                Rad(0.9),
                Rad(-1.0),
                Rad(1.1),
                Rad(-1.2),
            ]),
            JointArray::new([
                RadPerSecond(0.11),
                RadPerSecond(-0.12),
                RadPerSecond(0.13),
                RadPerSecond(-0.14),
                RadPerSecond(0.15),
                RadPerSecond(-0.16),
            ]),
            JointArray::new([
                NewtonMeter(1.0),
                NewtonMeter(-1.1),
                NewtonMeter(1.2),
                NewtonMeter(-1.3),
                NewtonMeter(1.4),
                NewtonMeter(-1.5),
            ]),
        )
    }

    fn snapshot(
        master_position: JointArray<Rad>,
        master_velocity: JointArray<RadPerSecond>,
        slave_torque: JointArray<NewtonMeter>,
    ) -> DualArmSnapshot {
        DualArmSnapshot {
            left: ControlSnapshotFull {
                state: ControlSnapshot {
                    position: master_position,
                    velocity: master_velocity,
                    torque: JointArray::splat(NewtonMeter::ZERO),
                    position_timestamp_us: 1,
                    dynamic_timestamp_us: 1,
                    skew_us: 0,
                },
                position_host_rx_mono_us: 1,
                dynamic_host_rx_mono_us: 1,
                feedback_age: Duration::from_millis(1),
            },
            right: ControlSnapshotFull {
                state: ControlSnapshot {
                    position: JointArray::splat(Rad::ZERO),
                    velocity: JointArray::splat(RadPerSecond::ZERO),
                    torque: slave_torque,
                    position_timestamp_us: 1,
                    dynamic_timestamp_us: 1,
                    skew_us: 0,
                },
                position_host_rx_mono_us: 1,
                dynamic_host_rx_mono_us: 1,
                feedback_age: Duration::from_millis(1),
            },
            inter_arm_skew: Duration::ZERO,
            host_cycle_timestamp: Instant::now(),
        }
    }
}
