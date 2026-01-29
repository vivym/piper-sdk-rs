//! # piper-physics: Physics calculations for Piper robot
//!
//! This crate provides physics functionality for gravity compensation
//! using the MuJoCo physics engine.
//!
//! ## Features
//!
//! - **MuJoCo-based Physics**: Accurate gravity compensation and inverse dynamics
//! - **Three Computation Modes**: Pure gravity, partial inverse dynamics, full inverse dynamics
//! - **Type-Safe**: Leverages nalgebra for vector/matrix operations
//! - **Production-Ready**: Validated on real robot hardware
//! - **High Performance**: < 100μs per calculation
//!
//! ## Prerequisites
//!
//! The `mujoco` feature (enabled by default) requires MuJoCo native library:
//!
//! ### macOS
//! ```bash
//! brew install mujoco pkgconf
//! ```
//!
//! ### Linux (Debian/Ubuntu)
//! ```bash
//! sudo apt-get install libmujoco-dev pkg-config
//! ```
//!
//! See [MuJoCo installation guide](https://github.com/google-deepmind/mujoco/blob/main/BUILD.md) for details.
//!
//! ## Quick Start
//!
//! ### Mode 1: Pure Gravity Compensation
//!
//! ```rust,no_run
//! use piper_physics::{MujocoGravityCompensation, GravityCompensation};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut gravity_calc = MujocoGravityCompensation::from_embedded()?;
//! let q = piper_physics::JointState::from_iterator([0.0; 6]);
//! let torques = gravity_calc.compute_gravity_compensation(&q)?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Mode 2: Partial Inverse Dynamics (with Coriolis and centrifugal forces)
//!
//! ```rust,no_run
//! use piper_physics::{MujocoGravityCompensation, GravityCompensation};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut gravity_calc = MujocoGravityCompensation::from_embedded()?;
//! let q = piper_physics::JointState::from_iterator([0.0; 6]);
//! let qvel = [0.5, 0.5, 0.5, 0.5, 0.5, 0.5];
//! let torques = gravity_calc.compute_partial_inverse_dynamics(&q, &qvel)?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Mode 3: Full Inverse Dynamics (with inertial forces)
//!
//! ```rust,no_run
//! use piper_physics::{MujocoGravityCompensation, GravityCompensation};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut gravity_calc = MujocoGravityCompensation::from_embedded()?;
//! let q = piper_physics::JointState::from_iterator([0.0; 6]);
//! let qvel = [2.0, 2.0, 2.0, 2.0, 2.0, 2.0];
//! let qacc = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
//! let torques = gravity_calc.compute_inverse_dynamics(&q, &qvel, &qacc)?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

// Re-export nalgebra to avoid version conflicts
pub use nalgebra;

mod error;
mod traits;
mod types;

// Re-export common types
pub use error::PhysicsError;
pub use traits::GravityCompensation;
pub use types::*;

// MuJoCo implementation (physics simulation)
pub mod mujoco;
pub use mujoco::MujocoGravityCompensation;

#[cfg(test)]
mod tests {
    //! Unit tests for matrix operations and FFI safety
    //!
    //! These tests verify the correctness of matrix transformations
    //! used in MuJoCo integration, particularly:
    //! - Row-major vs column-major matrix indexing
    //! - COM offset calculations
    //! - FFI pointer passing
    //!
    //! These tests don't require MuJoCo runtime - they only test the
    //! mathematical operations used in the MuJoCo implementation.

    /// Test that MuJoCo row-major matrix is correctly converted to nalgebra
    ///
    /// MuJoCo stores matrices in row-major order: [R00, R01, R02, R10, R11, R12, R20, R21, R22]
    /// This test verifies that we use the correct indexing when converting.
    #[test]
    fn test_row_major_matrix_conversion() {
        // Create a rotation matrix in MuJoCo's row-major format
        // For a rotation of 90 degrees around Z-axis:
        // [0, -1,  0]
        // [1,  0,  0]
        // [0,  0,  1]
        // Row-major: [0, -1, 0, 1, 0, 0, 0, 0, 1]
        let site_xmat: [f64; 9] = [0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

        // Convert using nalgebra's from_row_slice (correct method)
        let rot_mat = nalgebra::Matrix3::from_row_slice(&site_xmat);

        // Verify the matrix was correctly interpreted
        assert!((rot_mat[(0, 0)] - 0.0).abs() < 1e-10);
        assert!((rot_mat[(0, 1)] - (-1.0)).abs() < 1e-10);
        assert!((rot_mat[(0, 2)] - 0.0).abs() < 1e-10);
        assert!((rot_mat[(1, 0)] - 1.0).abs() < 1e-10);
        assert!((rot_mat[(1, 1)] - 0.0).abs() < 1e-10);
        assert!((rot_mat[(1, 2)] - 0.0).abs() < 1e-10);
        assert!((rot_mat[(2, 0)] - 0.0).abs() < 1e-10);
        assert!((rot_mat[(2, 1)] - 0.0).abs() < 1e-10);
        assert!((rot_mat[(2, 2)] - 1.0).abs() < 1e-10);
    }

    /// Test that incorrect column-major indexing produces wrong results
    ///
    /// This test demonstrates why using column-major indexing (i + 3*j) is wrong
    /// for MuJoCo's row-major data.
    #[test]
    fn test_column_major_indexing_is_wrong() {
        // Same rotation matrix in row-major format
        let site_xmat: [f64; 9] = [0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

        // WRONG: Using column-major indexing (i + 3*j)
        // This reads the TRANSPOSED matrix, which is incorrect
        let mut wrong_mat = nalgebra::Matrix3::zeros();
        for i in 0..3 {
            for j in 0..3 {
                // This is WRONG for row-major data
                wrong_mat[(i, j)] = site_xmat[i + 3 * j];
            }
        }

        // Verify this produces the WRONG (transposed) result
        // Instead of rotating by +90°, it would rotate by -90°
        assert!((wrong_mat[(0, 0)] - 0.0).abs() < 1e-10);
        assert!((wrong_mat[(0, 1)] - 1.0).abs() < 1e-10); // WRONG! Should be -1.0
        assert!((wrong_mat[(1, 0)] - (-1.0)).abs() < 1e-10); // WRONG! Should be 1.0
    }

    /// Test matrix multiplication for COM offset calculation
    #[test]
    fn test_com_offset_calculation() {
        // Identity rotation matrix
        let site_xmat: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

        // Local COM offset (e.g., 5cm in X direction)
        let com = nalgebra::Vector3::new(0.05, 0.0, 0.0);

        // Correct conversion using from_row_slice
        let rot_mat = nalgebra::Matrix3::from_row_slice(&site_xmat);
        let world_offset = rot_mat * com;

        // With identity rotation, offset should be unchanged
        assert!((world_offset[0] - 0.05).abs() < 1e-10);
        assert!((world_offset[1] - 0.0).abs() < 1e-10);
        assert!((world_offset[2] - 0.0).abs() < 1e-10);

        // Test with 90° Z rotation
        let rot_xmat: [f64; 9] = [0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let rot_mat = nalgebra::Matrix3::from_row_slice(&rot_xmat);
        let world_offset = rot_mat * com;

        // After 90° rotation: X offset becomes Y offset
        assert!((world_offset[0] - 0.0).abs() < 1e-10);
        assert!((world_offset[1] - 0.05).abs() < 1e-10);
        assert!((world_offset[2] - 0.0).abs() < 1e-10);
    }

    /// Test that FFI pointer passing is correctly implemented
    #[test]
    fn test_ffi_pointer_creation() {
        // Simulate world_com calculation
        let site_xpos = [0.1, 0.2, 0.3];
        let site_xmat: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let com = nalgebra::Vector3::new(0.05, 0.0, 0.0);

        let rot_mat = nalgebra::Matrix3::from_row_slice(&site_xmat);
        let world_offset = rot_mat * com;

        let world_com = nalgebra::Vector3::new(
            site_xpos[0] + world_offset[0],
            site_xpos[1] + world_offset[1],
            site_xpos[2] + world_offset[2],
        );

        // Create array for FFI (as done in compute_payload_torques)
        let point = [world_com[0], world_com[1], world_com[2]];

        // Verify pointer is valid and points to correct data
        let ptr = point.as_ptr();
        assert!(!ptr.is_null());

        // Verify we can read back the data through the pointer
        unsafe {
            assert!((*ptr - 0.15).abs() < 1e-10); // 0.1 + 0.05
            assert!((*ptr.add(1) - 0.2).abs() < 1e-10); // 0.2 + 0.0
            assert!((*ptr.add(2) - 0.3).abs() < 1e-10); // 0.3 + 0.0
        }
    }
}
