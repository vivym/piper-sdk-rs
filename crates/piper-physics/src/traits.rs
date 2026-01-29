//! Core traits for gravity compensation and inverse dynamics

use crate::{types::JointState, types::JointTorques, PhysicsError};

/// Gravity compensation and inverse dynamics trait
///
/// This trait defines the interface for computing dynamic compensation torques
/// required to counteract gravity, Coriolis forces, centrifugal forces, and inertial forces.
///
/// # Modes of Operation
///
/// This trait provides three modes with different levels of dynamic compensation:
///
/// 1. **Pure Gravity Compensation**: `τ = M(q)·g`
///    - For static holding, zero-force teaching
///    - Use: `compute_gravity_compensation()`
///
/// 2. **Partial Inverse Dynamics**: `τ = M(q)·g + C(q,q̇) + F_damping`
///    - For medium-speed trajectory tracking (0.5 - 2 rad/s)
///    - Use: `compute_partial_inverse_dynamics()`
///
/// 3. **Full Inverse Dynamics**: `τ = M(q)·g + C(q,q̇) + M(q)·q̈`
///    - For fast trajectory tracking, force control
///    - Use: `compute_inverse_dynamics()`
///
/// # Selection Guide
///
/// | Scenario | Recommended Mode | API |
/// |----------|-----------------|-----|
/// | Static holding | Pure gravity | `compute_gravity_compensation` |
/// | Zero-force teaching | Pure gravity | `compute_gravity_compensation` |
/// | Slow trajectory (< 0.5 rad/s) | Pure gravity | `compute_gravity_compensation` |
/// | Medium trajectory (0.5-2 rad/s) | Partial ID | `compute_partial_inverse_dynamics` |
/// | Fast trajectory (> 2 rad/s) | Full ID | `compute_inverse_dynamics` |
/// | Force control | Full ID | `compute_inverse_dynamics` |
pub trait GravityCompensation: Send + Sync {
    /// Mode 1: Pure gravity compensation (τ = M(q)·g)
    ///
    /// Computes torques required to counteract gravity only.
    /// Joint velocities are set to zero internally.
    ///
    /// # Use Cases
    ///
    /// - **Static pose holding**: Robot maintains a fixed position
    /// - **Zero-force teaching**: Manual teaching without resistance
    /// - **Low-speed operation**: < 0.5 rad/s where Coriolis forces are negligible
    ///
    /// # Arguments
    ///
    /// * `q` - Joint positions (radians)
    ///
    /// # Returns
    ///
    /// Joint torques for pure gravity compensation
    ///
    /// # Errors
    ///
    /// Returns an error if the calculation fails
    #[must_use = "Gravity compensation result should be used to prevent robot instability"]
    fn compute_gravity_compensation(
        &mut self,
        q: &JointState,
    ) -> Result<JointTorques, PhysicsError>;

    /// Mode 2: Partial inverse dynamics (τ = M(q)·g + C(q,q̇) + F_damping)
    ///
    /// Computes torques to counteract gravity, Coriolis forces, centrifugal forces,
    /// and viscous damping (if defined in model).
    ///
    /// Joint acceleration is set to zero internally, so inertial forces are NOT included.
    ///
    /// # Use Cases
    ///
    /// - **Medium-speed trajectory tracking**: 0.5 - 2 rad/s
    /// - **High-precision tracking**: Automatically compensates joint damping
    /// - **PD+feedforward control**: Augments PD controller with dynamics feedforward
    ///
    /// # Arguments
    ///
    /// * `q` - Joint positions (radians)
    /// * `qvel` - Joint velocities (radians/s)
    ///
    /// # Returns
    ///
    /// Joint torques including gravity and Coriolis/centrifugal forces
    ///
    /// # Errors
    ///
    /// Returns an error if the calculation fails
    ///
    /// # Note
    ///
    /// This does NOT include inertial forces (M·q̈). For fast trajectories,
    /// use `compute_inverse_dynamics()` instead.
    #[must_use = "Partial inverse dynamics result should be used to prevent robot instability"]
    fn compute_partial_inverse_dynamics(
        &mut self,
        q: &JointState,
        qvel: &[f64; 6],
    ) -> Result<JointTorques, PhysicsError>;

    /// Mode 3: Full inverse dynamics (τ = M(q)·g + C(q,q̇) + M(q)·q̈)
    ///
    /// Computes complete inverse dynamics including gravity, Coriolis, centrifugal,
    /// damping, and inertial forces.
    ///
    /// # Use Cases
    ///
    /// - **Fast trajectory tracking**: > 2 rad/s
    /// - **High-dynamic motion**: Rapid accelerations
    /// - **Force control**: Precise force application
    /// - **Impedance control**: Requires inertial matrix
    ///
    /// # Arguments
    ///
    /// * `q` - Joint positions (radians)
    /// * `qvel` - Joint velocities (radians/s)
    /// * `qacc_desired` - Desired joint accelerations (radians/s²)
    ///
    /// # Returns
    ///
    /// Joint torques including all dynamic terms
    ///
    /// # Errors
    ///
    /// Returns an error if the calculation fails
    #[must_use = "Inverse dynamics result should be used to prevent robot instability"]
    fn compute_inverse_dynamics(
        &mut self,
        q: &JointState,
        qvel: &[f64; 6],
        qacc_desired: &[f64; 6],
    ) -> Result<JointTorques, PhysicsError>;

    /// Legacy method: Compute gravity compensation with optional gravity vector
    ///
    /// # Deprecated
    ///
    /// This method is deprecated. Use `compute_gravity_compensation()` instead.
    ///
    /// # Migration Guide
    ///
    /// ```rust,no_run
    /// # use piper_physics::{MujocoGravityCompensation, GravityCompensation};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut gc = MujocoGravityCompensation::from_embedded()?;
    /// # let q = piper_physics::JointState::from_iterator([0.0; 6]);
    /// // Old (deprecated):
    /// let torques = gc.compute_gravity_torques(&q, None)?;
    ///
    /// // New (recommended):
    /// let torques = gc.compute_gravity_compensation(&q)?;
    /// # Ok(())
    /// # }
    /// ```
    #[deprecated(since = "0.0.4", note = "Use compute_gravity_compensation instead")]
    fn compute_gravity_torques(
        &mut self,
        q: &JointState,
        _gravity: Option<&nalgebra::Vector3<f64>>,
    ) -> Result<JointTorques, PhysicsError> {
        // Default implementation forwards to the new method
        self.compute_gravity_compensation(q)
    }

    /// Get the name of this gravity compensation implementation
    fn name(&self) -> &str;

    /// Check if the implementation is properly initialized
    fn is_initialized(&self) -> bool;
}
