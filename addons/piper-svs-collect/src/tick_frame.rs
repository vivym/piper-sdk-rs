use std::sync::Mutex;

use piper_client::dual_arm::{BilateralLoopTelemetry, DualArmSnapshot};
use piper_physics::EndEffectorKinematics;
use thiserror::Error;

use crate::controller::SvsControllerOutput;
use crate::cue::SvsCueOutput;
use crate::stiffness::SvsStiffnessOutput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotKey {
    pub master_dynamic_host_rx_mono_us: u64,
    pub slave_dynamic_host_rx_mono_us: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvsDynamicsFrame {
    pub key: SnapshotKey,
    pub master_model_torque_nm: [f64; 6],
    pub slave_model_torque_nm: [f64; 6],
    pub master_residual_nm: [f64; 6],
    pub slave_residual_nm: [f64; 6],
    pub master_ee: EndEffectorKinematics,
    pub slave_ee: EndEffectorKinematics,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvsPendingTick {
    pub key: SnapshotKey,
    pub dynamics: SvsDynamicsFrame,
    pub cues: SvsCueOutput,
    pub stiffness: SvsStiffnessOutput,
    pub controller_output: SvsControllerOutput,
}

#[derive(Debug, Default)]
pub struct SvsDynamicsSlot {
    slot: Mutex<Option<SvsDynamicsFrame>>,
}

#[derive(Debug, Default)]
pub struct SvsTickStager {
    slot: Mutex<Option<SvsPendingTick>>,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum SvsTickFrameError {
    #[error("SVS dynamics slot is poisoned")]
    DynamicsSlotPoisoned,
    #[error("SVS controller tick slot is poisoned")]
    ControllerTickSlotPoisoned,
    #[error(
        "unconsumed dynamics frame already pending: pending {pending:?}, incoming {incoming:?}"
    )]
    DynamicsAlreadyPending {
        pending: SnapshotKey,
        incoming: SnapshotKey,
    },
    #[error("no dynamics frame pending for requested key {requested:?}")]
    MissingDynamics { requested: SnapshotKey },
    #[error(
        "unconsumed controller tick already pending: pending {pending:?}, incoming {incoming:?}"
    )]
    ControllerTickAlreadyPending {
        pending: SnapshotKey,
        incoming: SnapshotKey,
    },
    #[error("no controller tick pending for telemetry key {requested:?}")]
    MissingControllerTick { requested: SnapshotKey },
    #[error(
        "snapshot key mismatch: expected pending key {expected:?}, got telemetry/requested key {actual:?}"
    )]
    MismatchedSnapshotKey {
        expected: SnapshotKey,
        actual: SnapshotKey,
    },
}

impl SnapshotKey {
    pub fn from_snapshot(snapshot: &DualArmSnapshot) -> Self {
        Self {
            master_dynamic_host_rx_mono_us: snapshot.left.dynamic_host_rx_mono_us,
            slave_dynamic_host_rx_mono_us: snapshot.right.dynamic_host_rx_mono_us,
        }
    }
}

impl SvsDynamicsSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store_dynamics(&self, dynamics: SvsDynamicsFrame) -> Result<(), SvsTickFrameError> {
        let incoming = dynamics.key;
        let mut guard = self.slot.lock().map_err(|_| SvsTickFrameError::DynamicsSlotPoisoned)?;
        if let Some(pending) = guard.as_ref() {
            return Err(SvsTickFrameError::DynamicsAlreadyPending {
                pending: pending.key,
                incoming,
            });
        }
        *guard = Some(dynamics);
        Ok(())
    }

    pub fn take_dynamics(&self, key: SnapshotKey) -> Result<SvsDynamicsFrame, SvsTickFrameError> {
        let mut guard = self.slot.lock().map_err(|_| SvsTickFrameError::DynamicsSlotPoisoned)?;
        match guard.as_ref() {
            Some(frame) if frame.key == key => Ok(guard.take().expect("matched frame exists")),
            Some(frame) => Err(SvsTickFrameError::MismatchedSnapshotKey {
                expected: frame.key,
                actual: key,
            }),
            None => Err(SvsTickFrameError::MissingDynamics { requested: key }),
        }
    }

    pub fn clear(&self) -> Result<(), SvsTickFrameError> {
        let mut guard = self.slot.lock().map_err(|_| SvsTickFrameError::DynamicsSlotPoisoned)?;
        *guard = None;
        Ok(())
    }
}

impl SvsTickStager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store_controller_tick(
        &self,
        pending_tick: SvsPendingTick,
    ) -> Result<(), SvsTickFrameError> {
        let incoming = pending_tick.key;
        let mut guard =
            self.slot.lock().map_err(|_| SvsTickFrameError::ControllerTickSlotPoisoned)?;
        if let Some(pending) = guard.as_ref() {
            return Err(SvsTickFrameError::ControllerTickAlreadyPending {
                pending: pending.key,
                incoming,
            });
        }
        *guard = Some(pending_tick);
        Ok(())
    }

    pub fn take_for_telemetry(
        &self,
        telemetry: &BilateralLoopTelemetry,
    ) -> Result<SvsPendingTick, SvsTickFrameError> {
        let key = SnapshotKey::from_snapshot(&telemetry.control_frame.snapshot);
        let mut guard =
            self.slot.lock().map_err(|_| SvsTickFrameError::ControllerTickSlotPoisoned)?;
        match guard.as_ref() {
            Some(pending) if pending.key == key => Ok(guard.take().expect("matched tick exists")),
            Some(pending) => Err(SvsTickFrameError::MismatchedSnapshotKey {
                expected: pending.key,
                actual: key,
            }),
            None => Err(SvsTickFrameError::MissingControllerTick { requested: key }),
        }
    }

    pub fn clear(&self) -> Result<(), SvsTickFrameError> {
        let mut guard =
            self.slot.lock().map_err(|_| SvsTickFrameError::ControllerTickSlotPoisoned)?;
        *guard = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use piper_client::dual_arm::{
        BilateralCommand, BilateralControlFrame, BilateralDynamicsCompensation,
        BilateralFinalTorques, BilateralGripperCommandStatus, BilateralLoopGripperTelemetry,
        BilateralLoopTelemetry, BilateralLoopTimingTelemetry, DualArmSnapshot,
    };
    use piper_client::observer::{ControlSnapshot, ControlSnapshotFull};
    use piper_client::types::{JointArray, NewtonMeter, Rad, RadPerSecond};
    use piper_physics::EndEffectorKinematics;

    use crate::controller::SvsControllerOutput;
    use crate::cue::SvsCueOutput;
    use crate::stiffness::{SvsContactState, SvsStiffnessOutput};

    use super::*;

    #[test]
    fn tick_stager_pairs_controller_output_with_matching_telemetry_snapshot() {
        let key = SnapshotKey {
            master_dynamic_host_rx_mono_us: 10,
            slave_dynamic_host_rx_mono_us: 20,
        };
        let stager = SvsTickStager::default();
        let pending = pending_tick(key);

        stager.store_controller_tick(pending.clone()).unwrap();
        let taken = stager.take_for_telemetry(&telemetry_for_key(key)).unwrap();

        assert_eq!(taken.key, key);
        assert_eq!(taken.controller_output, pending.controller_output);
        assert!(matches!(
            stager.take_for_telemetry(&telemetry_for_key(key)),
            Err(SvsTickFrameError::MissingControllerTick { .. })
        ));
    }

    #[test]
    fn tick_stager_rejects_mismatched_or_stale_telemetry() {
        let pending_key = SnapshotKey {
            master_dynamic_host_rx_mono_us: 10,
            slave_dynamic_host_rx_mono_us: 20,
        };
        let telemetry_key = SnapshotKey {
            master_dynamic_host_rx_mono_us: 10,
            slave_dynamic_host_rx_mono_us: 21,
        };
        let stager = SvsTickStager::default();

        stager.store_controller_tick(pending_tick(pending_key)).unwrap();

        assert!(matches!(
            stager.take_for_telemetry(&telemetry_for_key(telemetry_key)),
            Err(SvsTickFrameError::MismatchedSnapshotKey { expected, actual })
                if expected == pending_key && actual == telemetry_key
        ));

        let taken = stager.take_for_telemetry(&telemetry_for_key(pending_key)).unwrap();
        assert_eq!(taken.key, pending_key);
    }

    #[test]
    fn dynamics_slot_rejects_duplicate_and_preserves_mismatch() {
        let pending_key = SnapshotKey {
            master_dynamic_host_rx_mono_us: 10,
            slave_dynamic_host_rx_mono_us: 20,
        };
        let requested_key = SnapshotKey {
            master_dynamic_host_rx_mono_us: 11,
            slave_dynamic_host_rx_mono_us: 20,
        };
        let slot = SvsDynamicsSlot::default();

        slot.store_dynamics(dynamics_frame(pending_key)).unwrap();
        assert!(matches!(
            slot.store_dynamics(dynamics_frame(requested_key)),
            Err(SvsTickFrameError::DynamicsAlreadyPending { pending, incoming })
                if pending == pending_key && incoming == requested_key
        ));
        assert!(matches!(
            slot.take_dynamics(requested_key),
            Err(SvsTickFrameError::MismatchedSnapshotKey { expected, actual })
                if expected == pending_key && actual == requested_key
        ));

        assert_eq!(slot.take_dynamics(pending_key).unwrap().key, pending_key);
        assert!(matches!(
            slot.take_dynamics(pending_key),
            Err(SvsTickFrameError::MissingDynamics { requested }) if requested == pending_key
        ));
    }

    #[test]
    fn clear_drops_pending_slots() {
        let key = SnapshotKey {
            master_dynamic_host_rx_mono_us: 10,
            slave_dynamic_host_rx_mono_us: 20,
        };
        let dynamics_slot = SvsDynamicsSlot::default();
        let tick_stager = SvsTickStager::default();

        dynamics_slot.store_dynamics(dynamics_frame(key)).unwrap();
        tick_stager.store_controller_tick(pending_tick(key)).unwrap();

        dynamics_slot.clear().unwrap();
        tick_stager.clear().unwrap();

        assert!(matches!(
            dynamics_slot.take_dynamics(key),
            Err(SvsTickFrameError::MissingDynamics { .. })
        ));
        assert!(matches!(
            tick_stager.take_for_telemetry(&telemetry_for_key(key)),
            Err(SvsTickFrameError::MissingControllerTick { .. })
        ));
    }

    fn pending_tick(key: SnapshotKey) -> SvsPendingTick {
        let dynamics = dynamics_frame(key);
        SvsPendingTick {
            key,
            dynamics,
            cues: SvsCueOutput {
                tau_master_effort_residual_nm: [0.0; 6],
                tau_master_feedback_subtracted_nm: [0.0; 6],
                tau_slave_residual_nm: [0.0; 6],
                u_ee_raw: [0.0; 3],
                r_ee_raw: [0.0; 3],
                u_ee: [0.0; 3],
                r_ee: [0.0; 3],
            },
            stiffness: SvsStiffnessOutput {
                contact_state: SvsContactState::Free,
                k_state_raw_n_per_m: [50.0; 3],
                k_state_clipped_n_per_m: [50.0; 3],
                k_tele_n_per_m: [50.0; 3],
            },
            controller_output: SvsControllerOutput::for_tests(),
        }
    }

    fn dynamics_frame(key: SnapshotKey) -> SvsDynamicsFrame {
        SvsDynamicsFrame {
            key,
            master_model_torque_nm: [0.0; 6],
            slave_model_torque_nm: [0.0; 6],
            master_residual_nm: [0.0; 6],
            slave_residual_nm: [0.0; 6],
            master_ee: ee_with_jacobian(identity_translational_jacobian()),
            slave_ee: ee_with_jacobian(identity_translational_jacobian()),
        }
    }

    fn telemetry_for_key(key: SnapshotKey) -> BilateralLoopTelemetry {
        let snapshot = sample_snapshot(
            key.master_dynamic_host_rx_mono_us,
            key.slave_dynamic_host_rx_mono_us,
        );
        let command = zero_command();
        BilateralLoopTelemetry {
            control_frame: BilateralControlFrame {
                snapshot,
                compensation: Some(BilateralDynamicsCompensation::default()),
            },
            controller_command: command.clone(),
            shaped_command: command,
            compensation: Some(BilateralDynamicsCompensation::default()),
            gripper: BilateralLoopGripperTelemetry {
                mirror_enabled: false,
                master_available: false,
                slave_available: false,
                master_hw_timestamp_us: 0,
                slave_hw_timestamp_us: 0,
                master_host_rx_mono_us: 0,
                slave_host_rx_mono_us: 0,
                master_age_us: 0,
                slave_age_us: 0,
                master_position: 0.0,
                master_effort: 0.0,
                slave_position: 0.0,
                slave_effort: 0.0,
                command_status: BilateralGripperCommandStatus::None,
                command_position: 0.0,
                command_effort: 0.0,
            },
            final_torques: BilateralFinalTorques {
                master: JointArray::splat(NewtonMeter::ZERO),
                slave: JointArray::splat(NewtonMeter::ZERO),
            },
            master_t_ref_nm: None,
            slave_t_ref_nm: None,
            master_tx_finished_host_mono_us: Some(1),
            slave_tx_finished_host_mono_us: Some(2),
            timing: BilateralLoopTimingTelemetry {
                scheduler_tick_start_host_mono_us: 1,
                control_frame_host_mono_us: 2,
                previous_control_frame_host_mono_us: None,
                raw_dt_us: 5_000,
                clamped_dt_us: 5_000,
                nominal_period_us: 5_000,
                submission_deadline_mono_us: 7_000,
                deadline_missed: false,
            },
        }
    }

    fn zero_command() -> BilateralCommand {
        BilateralCommand {
            slave_position: JointArray::splat(Rad::ZERO),
            slave_velocity: JointArray::splat(0.0),
            slave_kp: JointArray::splat(0.0),
            slave_kd: JointArray::splat(0.0),
            slave_feedforward_torque: JointArray::splat(NewtonMeter::ZERO),
            master_position: JointArray::splat(Rad::ZERO),
            master_velocity: JointArray::splat(0.0),
            master_kp: JointArray::splat(0.0),
            master_kd: JointArray::splat(0.0),
            master_interaction_torque: JointArray::splat(NewtonMeter::ZERO),
        }
    }

    fn sample_snapshot(
        master_dynamic_host_rx_mono_us: u64,
        slave_dynamic_host_rx_mono_us: u64,
    ) -> DualArmSnapshot {
        DualArmSnapshot {
            left: control_snapshot_full(master_dynamic_host_rx_mono_us),
            right: control_snapshot_full(slave_dynamic_host_rx_mono_us),
            inter_arm_skew: Duration::from_micros(
                master_dynamic_host_rx_mono_us.abs_diff(slave_dynamic_host_rx_mono_us),
            ),
            host_cycle_timestamp: Instant::now(),
        }
    }

    fn control_snapshot_full(dynamic_host_rx_mono_us: u64) -> ControlSnapshotFull {
        ControlSnapshotFull {
            state: ControlSnapshot {
                position: JointArray::splat(Rad::ZERO),
                velocity: JointArray::splat(RadPerSecond::ZERO),
                torque: JointArray::splat(NewtonMeter::ZERO),
                position_timestamp_us: dynamic_host_rx_mono_us - 1,
                dynamic_timestamp_us: dynamic_host_rx_mono_us,
                skew_us: 1,
            },
            position_host_rx_mono_us: dynamic_host_rx_mono_us - 1,
            dynamic_host_rx_mono_us,
            feedback_age: Duration::from_micros(500),
        }
    }

    fn ee_with_jacobian(translational_jacobian_base: [[f64; 6]; 3]) -> EndEffectorKinematics {
        EndEffectorKinematics {
            position_base_m: [0.0; 3],
            rotation_base_from_ee: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            translational_jacobian_base,
            jacobian_condition: 1.0,
        }
    }

    fn identity_translational_jacobian() -> [[f64; 6]; 3] {
        [
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
        ]
    }
}
