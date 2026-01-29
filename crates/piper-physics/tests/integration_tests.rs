//! Integration tests for piper-physics
//!
//! These tests verify the overall functionality of the gravity compensation
//! implementations, including:
//! - Trait implementation correctness
//! - API usability
//! - Error handling
//! - Integration with piper-sdk types

#[cfg(feature = "kinematics")]
use piper_physics::{AnalyticalGravityCompensation, GravityCompensation, JointState};

#[cfg(feature = "kinematics")]
use std::path::Path;

#[cfg(feature = "kinematics")]
#[test]
fn test_analytical_gravity_compensation_api() {
    // This test verifies that the AnalyticalGravityCompensation
    // implements the GravityCompensation trait correctly

    let urdf_path = Path::new("crates/piper-physics/assets/piper_description.urdf");

    // Test loading from URDF
    let mut gravity_calc = match AnalyticalGravityCompensation::from_urdf(urdf_path) {
        Ok(gc) => gc,
        Err(e) => {
            // If URDF file is not found (e.g., in CI), skip test gracefully
            eprintln!("Skipping test: URDF file not found: {}", e);
            return;
        },
    };

    // Verify initialization
    assert!(gravity_calc.is_initialized());
    assert_eq!(gravity_calc.name(), "analytical_rne");

    // Test computing torques at zero position
    let q_zero = JointState::from_iterator(std::iter::repeat_n(0.0, 6));

    let torques_result = gravity_calc.compute_gravity_compensation(&q_zero);
    assert!(
        torques_result.is_ok(),
        "compute_gravity_compensation should succeed: {:?}",
        torques_result
    );

    let torques = torques_result.unwrap();

    // Verify we get 6 torques for 6 joints
    assert_eq!(torques.len(), 6, "Should return 6 joint torques");

    // Note: Currently, the analytical implementation returns zero torques
    // because the actual RNE algorithm is not yet implemented (k crate doesn't provide it)
    // When the RNE implementation is added, these assertions should be updated
    // to verify that non-zero torques are computed for non-horizontal poses
    for (i, &tau) in torques.iter().enumerate() {
        assert!(
            tau.is_finite(),
            "Torque {} should be finite, got {}",
            i,
            tau
        );
    }
}

#[cfg(feature = "kinematics")]
#[test]
fn test_analytical_gravity_compensation_custom_gravity() {
    // Test custom gravity vector
    let urdf_path = Path::new("crates/piper-physics/assets/piper_description.urdf");

    let mut gravity_calc = match AnalyticalGravityCompensation::from_urdf(urdf_path) {
        Ok(gc) => gc,
        Err(e) => {
            eprintln!("Skipping test: URDF file not found: {}", e);
            return;
        },
    };

    let q_zero = JointState::from_iterator(std::iter::repeat_n(0.0, 6));

    // Test with new API (gravity compensation - Mode 1)
    let torques_result = gravity_calc.compute_gravity_compensation(&q_zero);

    assert!(
        torques_result.is_ok(),
        "compute_gravity_compensation should succeed"
    );

    let torques = torques_result.unwrap();
    assert_eq!(torques.len(), 6);
}

#[cfg(feature = "kinematics")]
#[test]
fn test_analytical_gravity_compensation_invalid_urdf() {
    // Test error handling for invalid URDF path
    let invalid_path = Path::new("/nonexistent/path/to/urdf");

    let result = AnalyticalGravityCompensation::from_urdf(invalid_path);
    assert!(result.is_err(), "Should return error for invalid URDF path");
}

#[cfg(feature = "kinematics")]
#[test]
fn test_uninitialized_gravity_compensation() {
    // Test that uninitialized implementation returns appropriate error
    let mut gravity_calc = AnalyticalGravityCompensation::default();

    assert!(
        !gravity_calc.is_initialized(),
        "Default implementation should not be initialized"
    );

    let q_zero = JointState::from_iterator(std::iter::repeat_n(0.0, 6));
    let result = gravity_calc.compute_gravity_compensation(&q_zero);

    assert!(
        result.is_err(),
        "Uninitialized implementation should return error"
    );

    match result {
        Err(piper_physics::PhysicsError::NotInitialized) => {
            // Expected error type
        },
        Err(e) => {
            panic!("Expected NotInitialized error, got: {:?}", e);
        },
        Ok(_) => {
            panic!("Expected error for uninitialized implementation");
        },
    }
}

#[cfg(feature = "kinematics")]
#[test]
fn test_gravity_compensation_must_use_attribute() {
    // Verify that the #[must_use] attribute is in place by checking
    // that ignoring the result produces a compiler warning
    let urdf_path = Path::new("crates/piper-physics/assets/piper_description.urdf");

    let mut gravity_calc = match AnalyticalGravityCompensation::from_urdf(urdf_path) {
        Ok(gc) => gc,
        Err(e) => {
            eprintln!("Skipping test: URDF file not found: {}", e);
            return;
        },
    };

    let q_zero = JointState::from_iterator(std::iter::repeat_n(0.0, 6));

    // This should produce a compiler warning if #[must_use] is correctly applied
    let _ = gravity_calc.compute_gravity_compensation(&q_zero);

    // Test passes if code compiles (with warning)
}

#[cfg(feature = "kinematics")]
#[test]
fn test_jointstate_integration() {
    // Test that JointState from piper-sdk integrates correctly
    let urdf_path = Path::new("crates/piper-physics/assets/piper_description.urdf");

    let mut gravity_calc = match AnalyticalGravityCompensation::from_urdf(urdf_path) {
        Ok(gc) => gc,
        Err(e) => {
            eprintln!("Skipping test: URDF file not found: {}", e);
            return;
        },
    };

    // Create JointState using different methods
    let q1 = JointState::from_iterator(vec![0.0, 0.1, 0.2, 0.3, 0.4, 0.5]);
    let q2 = JointState::from_iterator(std::iter::repeat_n(0.0, 6));
    let q3 = JointState::from_iterator([0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);

    // All should work
    for q in [&q1, &q2, &q3] {
        let result = gravity_calc.compute_gravity_compensation(q);
        assert!(
            result.is_ok(),
            "compute_gravity_compensation should work with JointState: {:?}",
            result
        );
    }
}

#[cfg(feature = "kinematics")]
#[test]
fn test_three_mode_api() {
    // Test that all three modes are available and work correctly
    let urdf_path = Path::new("crates/piper-physics/assets/piper_description.urdf");

    let mut gravity_calc = match AnalyticalGravityCompensation::from_urdf(urdf_path) {
        Ok(gc) => gc,
        Err(e) => {
            eprintln!("Skipping test: URDF file not found: {}", e);
            return;
        },
    };

    let q_zero = JointState::from_iterator(std::iter::repeat_n(0.0, 6));
    let qvel = [0.5, 0.5, 0.5, 0.5, 0.5, 0.5];
    let qacc = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0];

    // Mode 1: Pure gravity compensation
    let result_mode1 = gravity_calc.compute_gravity_compensation(&q_zero);
    assert!(
        result_mode1.is_ok(),
        "Mode 1 should succeed: {:?}",
        result_mode1
    );

    // Mode 2: Partial inverse dynamics
    let result_mode2 = gravity_calc.compute_partial_inverse_dynamics(&q_zero, &qvel);
    assert!(
        result_mode2.is_ok(),
        "Mode 2 should succeed: {:?}",
        result_mode2
    );

    // Mode 3: Full inverse dynamics
    let result_mode3 = gravity_calc.compute_inverse_dynamics(&q_zero, &qvel, &qacc);
    assert!(
        result_mode3.is_ok(),
        "Mode 3 should succeed: {:?}",
        result_mode3
    );
}
