//! End-effector pose, Jacobian, and runtime identity helpers.

use crate::error::PhysicsError;
use mujoco_rs::mujoco_c;
use std::ffi::CStr;

/// Explicit MuJoCo site selector for end-effector kinematics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndEffectorSelector {
    /// Exact MuJoCo site name to resolve.
    pub site_name: String,
}

/// End-effector pose and translational Jacobian in the robot base frame.
#[derive(Debug, Clone, PartialEq)]
pub struct EndEffectorKinematics {
    /// End-effector site position in the robot base frame, in meters.
    pub position_base_m: [f64; 3],
    /// Rotation matrix from end-effector frame to robot base frame.
    pub rotation_base_from_ee: [[f64; 3]; 3],
    /// Translational site Jacobian in the robot base frame.
    pub translational_jacobian_base: [[f64; 6]; 3],
    /// Singular-value condition number of the translational Jacobian.
    pub jacobian_condition: f64,
}

/// Identity metadata for the loaded MuJoCo runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MujocoRuntimeIdentity {
    /// Version string reported by the native MuJoCo library.
    pub runtime_version: String,
    /// Rust binding crate version when it can be determined.
    pub rust_binding_version: Option<String>,
    /// SHA-256 hash of the loaded native shared library bytes.
    pub native_library_sha256: Option<String>,
    /// Explicit identity string for a statically linked native MuJoCo build.
    pub static_build_identity: Option<String>,
}

impl EndEffectorSelector {
    /// Validates that an explicit end-effector site name was configured.
    pub fn validate(&self) -> Result<(), PhysicsError> {
        if self.site_name.trim().is_empty() {
            return Err(PhysicsError::InvalidInput(
                "end-effector site name is required".to_string(),
            ));
        }

        Ok(())
    }
}

/// Returns the native MuJoCo runtime version string.
pub fn mujoco_runtime_version_string() -> String {
    let version = unsafe { mujoco_c::mj_versionString() };
    if version.is_null() {
        return "unknown".to_string();
    }

    unsafe { CStr::from_ptr(version) }.to_string_lossy().into_owned()
}

/// Returns reproducibility identity for the loaded MuJoCo native library.
///
/// This first implementation intentionally fails rather than guessing a library
/// path or hash that may be wrong on some platforms. The collector can use this
/// error to fail before enabling MIT mode.
pub fn loaded_mujoco_library_identity() -> Result<MujocoRuntimeIdentity, PhysicsError> {
    Err(PhysicsError::CalculationFailed(format!(
        "MuJoCo native shared-library identity hashing is not implemented; \
         cannot prove reproducible runtime identity for MuJoCo {}",
        mujoco_runtime_version_string()
    )))
}

pub(crate) fn condition_number_from_singular_values(values: [f64; 3]) -> f64 {
    let mut min = f64::INFINITY;
    let mut max = 0.0;

    for value in values {
        if !value.is_finite() {
            return f64::INFINITY;
        }

        max = f64::max(max, value);
        if value > 0.0 {
            min = f64::min(min, value);
        }
    }

    if min == f64::INFINITY || min <= f64::EPSILON {
        return f64::INFINITY;
    }

    max / min
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_end_effector_site_name() {
        let selector = EndEffectorSelector {
            site_name: String::new(),
        };
        assert!(selector.validate().is_err());
    }

    #[test]
    fn computes_condition_number_from_singular_values() {
        let values = [10.0, 2.0, 0.5];
        assert_eq!(condition_number_from_singular_values(values), 20.0);
    }
}
