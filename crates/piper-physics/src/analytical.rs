//! Analytical gravity compensation using RNE algorithm
//!
//! This module provides gravity compensation calculations using the
//! Recursive Newton-Euler (RNE) algorithm via the `k` crate.

mod validation;

use crate::{
    error::PhysicsError,
    traits::GravityCompensation,
    types::{JointState, JointTorques},
};
use k::Chain;
use nalgebra::Vector3;
use std::path::Path;

/// Analytical gravity compensation using RNE algorithm
///
/// # Examples
///
/// ```no_run
/// use piper_physics::AnalyticalGravityCompensation;
///
/// // Load from URDF file (includes validation)
/// let mut gravity_calc = AnalyticalGravityCompensation::from_urdf(
///     std::path::Path::new("/path/to/piper.urdf")
/// ).expect("Failed to load URDF");
///
/// // Compute torques for zero position
/// let q = nalgebra::Vector6::zeros();
/// let torques = gravity_calc.compute_gravity_torques(&q, None)
///     .expect("Failed to compute torques");
/// ```
#[derive(Default)]
pub struct AnalyticalGravityCompensation {
    /// Kinematic chain loaded from URDF
    chain: Option<Chain<f64>>,
}

impl AnalyticalGravityCompensation {
    /// Create from custom URDF file with validation
    ///
    /// # Arguments
    ///
    /// * `urdf_path` - Path to URDF file
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - URDF file cannot be read
    /// - URDF parsing fails
    /// - Joint mapping validation fails
    pub fn from_urdf(urdf_path: &Path) -> Result<Self, PhysicsError> {
        let chain = Chain::from_urdf_file(urdf_path).map_err(|e| PhysicsError::UrdfParseError {
            path: urdf_path.to_path_buf(),
            error: e.to_string(),
        })?;

        // Validate joint mapping
        validation::validate_joint_mapping(&chain)?;

        Ok(Self { chain: Some(chain) })
    }

    /// Get reference to the kinematic chain
    pub fn chain(&self) -> Option<&Chain<f64>> {
        self.chain.as_ref()
    }
}

impl GravityCompensation for AnalyticalGravityCompensation {
    /// Mode 1: Pure gravity compensation (placeholder)
    ///
    /// **Note**: The k crate does NOT provide inverse dynamics capabilities.
    /// This method returns zero torques as a placeholder.
    ///
    /// For actual gravity compensation, use the `mujoco` feature instead.
    fn compute_gravity_compensation(
        &mut self,
        q: &JointState,
    ) -> Result<JointTorques, PhysicsError> {
        let chain = self.chain.as_mut().ok_or(PhysicsError::NotInitialized)?;

        // Set joint positions (use as_slice() for k crate API)
        chain.set_joint_positions(q.as_slice()).map_err(|e| {
            PhysicsError::CalculationFailed(format!("Failed to set joint positions: {}", e))
        })?;

        // TODO: Implement RNE algorithm
        // The k crate provides FK/IK/Jacobian but NOT inverse dynamics
        // Options:
        // 1. Use a different crate (e.g., nalgebra-linalg for custom RNE)
        // 2. Implement RNE algorithm from scratch
        // 3. Use MuJoCo's mujoco feature instead

        // For now, return zero torques as placeholder
        let torques_vec = vec![0.0f64; 6];
        let torques = JointTorques::from_iterator(torques_vec);

        log::warn!("Analytical gravity compensation is not implemented yet.");
        log::warn!("The k crate does not provide inverse dynamics (RNE algorithm).");
        log::warn!("Use the 'mujoco' feature for actual gravity compensation.");

        Ok(torques)
    }

    /// Mode 2: Partial inverse dynamics (placeholder)
    ///
    /// **Note**: The k crate does NOT provide inverse dynamics capabilities.
    /// This method returns zero torques as a placeholder.
    fn compute_partial_inverse_dynamics(
        &mut self,
        q: &JointState,
        _qvel: &[f64; 6],
    ) -> Result<JointTorques, PhysicsError> {
        // TODO: Implement RNE algorithm with Coriolis/centrifugal forces
        // Forward to pure gravity compensation for now
        self.compute_gravity_compensation(q)
    }

    /// Mode 3: Full inverse dynamics (placeholder)
    ///
    /// **Note**: The k crate does NOT provide inverse dynamics capabilities.
    /// This method returns zero torques as a placeholder.
    fn compute_inverse_dynamics(
        &mut self,
        q: &JointState,
        _qvel: &[f64; 6],
        _qacc_desired: &[f64; 6],
    ) -> Result<JointTorques, PhysicsError> {
        // TODO: Implement complete RNE algorithm
        // Forward to pure gravity compensation for now
        self.compute_gravity_compensation(q)
    }

    /// Legacy method: Compute gravity compensation (deprecated)
    fn compute_gravity_torques(
        &mut self,
        q: &JointState,
        _gravity: Option<&Vector3<f64>>,
    ) -> Result<JointTorques, PhysicsError> {
        // Forward to the new method
        self.compute_gravity_compensation(q)
    }

    fn name(&self) -> &str {
        "analytical_rne"
    }

    fn is_initialized(&self) -> bool {
        self.chain.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_not_initialized() {
        let gc = AnalyticalGravityCompensation::default();
        assert!(!gc.is_initialized());
    }

    #[test]
    fn test_compute_without_initialization() {
        let mut gc = AnalyticalGravityCompensation::default();
        let q = JointState::zeros();
        let result = gc.compute_gravity_compensation(&q);
        assert!(matches!(result, Err(PhysicsError::NotInitialized)));
    }

    // TODO: Add tests with actual URDF file in assets/
}
