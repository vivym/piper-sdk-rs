//! End-effector pose, Jacobian, and runtime identity helpers.

use crate::error::PhysicsError;
use mujoco_rs::mujoco_c;
use sha2::{Digest, Sha256};
use std::ffi::CStr;
use std::fs;
#[cfg(unix)]
use std::mem;
use std::path::{Path, PathBuf};

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
pub fn loaded_mujoco_library_identity() -> Result<MujocoRuntimeIdentity, PhysicsError> {
    let runtime_version = mujoco_runtime_version_string();
    let library_path = find_mujoco_library_path(&runtime_version)?;
    let bytes = fs::read(&library_path).map_err(PhysicsError::IoError)?;

    Ok(MujocoRuntimeIdentity {
        runtime_version,
        rust_binding_version: None,
        native_library_sha256: Some(sha256_hex(&bytes)),
        static_build_identity: None,
    })
}

fn find_mujoco_library_path(runtime_version: &str) -> Result<PathBuf, PhysicsError> {
    #[cfg(unix)]
    if let Some(path) = find_mujoco_library_from_dladdr()? {
        return Ok(path);
    }

    Err(PhysicsError::CalculationFailed(format!(
        "could not prove loaded MuJoCo native shared-library path for version {runtime_version}"
    )))
}

#[cfg(unix)]
fn find_mujoco_library_from_dladdr() -> Result<Option<PathBuf>, PhysicsError> {
    let mut info = mem::MaybeUninit::<libc::Dl_info>::zeroed();
    let symbol = mujoco_c::mj_versionString as *const libc::c_void;
    let found = unsafe { libc::dladdr(symbol, info.as_mut_ptr()) };
    if found == 0 {
        return Ok(None);
    }

    let info = unsafe { info.assume_init() };
    if info.dli_fname.is_null() {
        return Ok(None);
    }
    let path = unsafe { CStr::from_ptr(info.dli_fname) };
    let path = PathBuf::from(path.to_string_lossy().as_ref());
    if !is_mujoco_shared_library_path(&path) {
        return Err(PhysicsError::CalculationFailed(format!(
            "MuJoCo mj_versionString resolved to non-MuJoCo module {}",
            path.display()
        )));
    }
    Ok(Some(path))
}

fn is_mujoco_shared_library_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name.contains("mujoco")
        && (file_name.ends_with(".so")
            || file_name.contains(".so.")
            || file_name.ends_with(".dylib")
            || file_name.ends_with(".dll"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(hex_nibble(byte >> 4));
        out.push(hex_nibble(byte & 0x0f));
    }
    out
}

fn hex_nibble(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + (nibble - 10)) as char,
        _ => unreachable!("nibble is always <= 15"),
    }
}

pub(crate) fn condition_number_from_singular_values(values: [f64; 3]) -> f64 {
    let mut min = f64::INFINITY;
    let mut max = 0.0;

    for value in values {
        if !value.is_finite() || value < 0.0 {
            return f64::INFINITY;
        }

        max = f64::max(max, value);
        min = f64::min(min, value);
    }

    if min <= f64::EPSILON {
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

    #[test]
    fn singular_condition_number_is_infinite() {
        let values = [10.0, 2.0, 0.0];
        assert_eq!(condition_number_from_singular_values(values), f64::INFINITY);
    }

    #[test]
    fn loaded_library_identity_returns_native_hash() {
        let identity = loaded_mujoco_library_identity().expect("runtime identity should be proven");
        let hash = identity.native_library_sha256.expect("dynamic MuJoCo runtime should be hashed");

        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(identity.static_build_identity.is_none());
    }
}
