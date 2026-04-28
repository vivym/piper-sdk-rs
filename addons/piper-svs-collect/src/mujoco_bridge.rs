use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use piper_client::dual_arm::{
    BilateralDynamicsCompensation, BilateralDynamicsCompensator, DualArmSnapshot,
};
use piper_physics::{
    EndEffectorSelector, JointState, MujocoDualArmCompensator, MujocoGravityCompensation,
    PhysicsError, loaded_mujoco_library_identity,
};
use thiserror::Error;

use crate::tick_frame::{SnapshotKey, SvsDynamicsFrame, SvsDynamicsSlot, SvsTickFrameError};

#[derive(Debug, Clone)]
pub enum SvsMujocoModelSource {
    ModelDir(PathBuf),
    StandardPath,
    Embedded,
}

#[derive(Debug, Clone)]
pub struct SvsMujocoBridgeConfig {
    pub model_source: SvsMujocoModelSource,
    pub compensator: piper_physics::DualArmMujocoCompensatorConfig,
    pub master_ee_site: String,
    pub slave_ee_site: String,
}

impl Default for SvsMujocoBridgeConfig {
    fn default() -> Self {
        Self {
            model_source: SvsMujocoModelSource::StandardPath,
            compensator: piper_physics::DualArmMujocoCompensatorConfig::default(),
            master_ee_site: "master_tool_center".to_string(),
            slave_ee_site: "slave_tool_center".to_string(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SvsMujocoBridgeError {
    #[error("MuJoCo runtime identity could not be proven: {0}")]
    RuntimeIdentity(PhysicsError),
    #[error("MuJoCo physics failed: {0}")]
    Physics(#[from] PhysicsError),
    #[error("SVS tick staging failed: {0}")]
    TickFrame(#[from] SvsTickFrameError),
}

pub struct SvsMujocoBridge {
    compensator: MujocoDualArmCompensator,
    master_kinematics: MujocoGravityCompensation,
    slave_kinematics: MujocoGravityCompensation,
    master_ee: EndEffectorSelector,
    slave_ee: EndEffectorSelector,
    dynamics_slot: Arc<SvsDynamicsSlot>,
}

impl SvsMujocoBridge {
    pub fn new(
        config: SvsMujocoBridgeConfig,
        dynamics_slot: Arc<SvsDynamicsSlot>,
    ) -> Result<Self, SvsMujocoBridgeError> {
        let master_ee = EndEffectorSelector {
            site_name: config.master_ee_site,
        };
        let slave_ee = EndEffectorSelector {
            site_name: config.slave_ee_site,
        };
        master_ee.validate()?;
        slave_ee.validate()?;
        loaded_mujoco_library_identity().map_err(SvsMujocoBridgeError::RuntimeIdentity)?;

        let compensator = match &config.model_source {
            SvsMujocoModelSource::ModelDir(path) => {
                MujocoDualArmCompensator::from_model_dir_pair(path, config.compensator.clone())?
            },
            SvsMujocoModelSource::StandardPath => {
                MujocoDualArmCompensator::from_standard_path_pair(config.compensator.clone())?
            },
            SvsMujocoModelSource::Embedded => {
                MujocoDualArmCompensator::from_embedded_pair(config.compensator.clone())?
            },
        };
        let (mut master_kinematics, mut slave_kinematics) =
            build_kinematics_pair(&config.model_source)?;
        let zero_q = JointState::from_row_slice(&[0.0; 6]);
        master_kinematics.end_effector_kinematics(&master_ee, &zero_q)?;
        slave_kinematics.end_effector_kinematics(&slave_ee, &zero_q)?;

        Ok(Self {
            compensator,
            master_kinematics,
            slave_kinematics,
            master_ee,
            slave_ee,
            dynamics_slot,
        })
    }

    pub fn dynamics_slot(&self) -> Arc<SvsDynamicsSlot> {
        Arc::clone(&self.dynamics_slot)
    }
}

impl BilateralDynamicsCompensator for SvsMujocoBridge {
    type Error = SvsMujocoBridgeError;

    fn compute(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> Result<BilateralDynamicsCompensation, Self::Error> {
        let compensation = self.compensator.compute(snapshot, dt)?;
        let master_q = joint_state(snapshot.left.state.position.into_array().map(|value| value.0));
        let slave_q = joint_state(snapshot.right.state.position.into_array().map(|value| value.0));
        let master_ee =
            self.master_kinematics.end_effector_kinematics(&self.master_ee, &master_q)?;
        let slave_ee = self.slave_kinematics.end_effector_kinematics(&self.slave_ee, &slave_q)?;
        let key = SnapshotKey::from_snapshot(snapshot);

        self.dynamics_slot.store_dynamics(SvsDynamicsFrame {
            key,
            master_model_torque_nm: torque_array(compensation.master_model_torque),
            slave_model_torque_nm: torque_array(compensation.slave_model_torque),
            master_residual_nm: torque_array(compensation.master_external_torque_est),
            slave_residual_nm: torque_array(compensation.slave_external_torque_est),
            master_ee,
            slave_ee,
        })?;

        Ok(compensation)
    }

    fn on_time_jump(&mut self, dt: Duration) -> Result<(), Self::Error> {
        self.compensator.on_time_jump(dt)?;
        self.dynamics_slot.clear()?;
        Ok(())
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        self.compensator.reset()?;
        self.dynamics_slot.clear()?;
        Ok(())
    }
}

fn build_kinematics_pair(
    source: &SvsMujocoModelSource,
) -> Result<(MujocoGravityCompensation, MujocoGravityCompensation), PhysicsError> {
    match source {
        SvsMujocoModelSource::ModelDir(path) => Ok((
            MujocoGravityCompensation::from_model_dir(path)?,
            MujocoGravityCompensation::from_model_dir(path)?,
        )),
        SvsMujocoModelSource::StandardPath => Ok((
            MujocoGravityCompensation::from_standard_path()?,
            MujocoGravityCompensation::from_standard_path()?,
        )),
        SvsMujocoModelSource::Embedded => Ok((
            MujocoGravityCompensation::from_embedded()?,
            MujocoGravityCompensation::from_embedded()?,
        )),
    }
}

fn joint_state(values: [f64; 6]) -> JointState {
    JointState::from_row_slice(&values)
}

fn torque_array(
    values: piper_client::types::JointArray<piper_client::types::NewtonMeter>,
) -> [f64; 6] {
    values.into_array().map(|value| value.0)
}
