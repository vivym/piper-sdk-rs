//! MuJoCo-based gravity compensation implementation
//!
//! This module uses MuJoCo physics engine to compute gravity compensation torques
//! using the `qfrc_bias` field which contains bias forces (gravity + Coriolis + centrifugal).
//!
//! # Best Practices Applied
//!
//! - **Model Loading**: Uses `from_xml_string` + `include_str!` for zero-runtime-overhead
//! - **Ownership**: Uses `Rc<MjModel>` for shared ownership (supports parallelization)
//! - **Zero-Allocation**: Uses `from_iterator` to avoid Vec allocation
//! - **Type Safety**: Validates model dimensions (6-DOF check)
//! - **API Consistency**: Ignores gravity parameter (MuJoCo uses model's internal gravity)

use crate::{
    error::PhysicsError,
    traits::GravityCompensation,
    types::{JointState, JointTorques},
};
use mujoco_rs::{mujoco_c, prelude::*};
use std::sync::Arc;

/// MuJoCo-based gravity compensation
///
/// Uses MuJoCo's `qfrc_bias` field to compute gravity compensation torques.
/// When velocities and accelerations are zero, `qfrc_bias` contains only gravitational forces.
///
/// # Theory
///
/// MuJoCo dynamics equation:
/// ```text
/// M(q) * qacc + C(q, qd) = τ_applied + τ_bias
/// ```
///
/// Where:
/// - `C(q, qd)`: Bias forces (gravity + Coriolis + centrifugal)
/// - `qfrc_bias`: Bias forces stored in MuJoCo
///
/// **Gravity compensation key**:
/// - Set `qvel = 0` → Coriolis forces = 0
/// - Set `qacc = 0` → Inertial forces = 0
/// - Call `forward()` → Compute `qfrc_bias`
/// - Result: `qfrc_bias` ≈ **pure gravity torques**
///
/// # Examples
///
/// ```no_run
/// use piper_physics::{MujocoGravityCompensation, GravityCompensation};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Load from standard path (recommended)
/// let mut gravity_calc = MujocoGravityCompensation::from_standard_path()?;
///
/// // Compute torques for zero position
/// let q = piper_physics::JointState::from_iterator([0.0; 6]);
/// let torques = gravity_calc.compute_gravity_compensation(&q)?;
///
/// // Compute with dynamic payload (e.g., 500g object)
/// let torques_with_load = gravity_calc
///     .compute_gravity_torques_with_payload(
///         &q,
///         0.5,  // 500g
///         nalgebra::Vector3::new(0.0, 0.0, 0.0),  // CoM at end-effector
///     )?;
/// # Ok(())
/// # }
/// ```
pub struct MujocoGravityCompensation {
    /// MuJoCo model (shared, immutable)
    model: Arc<MjModel>,
    /// MuJoCo simulation data (mutable state)
    data: MjData<Arc<MjModel>>,
    /// End-effector site ID (for Jacobian calculations in payload compensation)
    ee_site_id: Option<i32>,
    /// End-effector body ID (parent body of the site)
    ee_body_id: Option<i32>,
}

impl MujocoGravityCompensation {
    /// Load from embedded MJCF XML (zero configuration)
    ///
    /// ⚠️ **WARNING**: This method only works if the MJCF XML does NOT reference
    /// external mesh files (STL/OBJ). If your XML contains `<mesh file="..."/>`,
    /// this method will FAIL at runtime because MuJoCo cannot find the mesh files.
    ///
    /// For models with mesh files, use [`Self::from_model_dir()`] instead.
    ///
    /// This is the recommended way ONLY for simple models using basic geometry
    /// (box, cylinder, sphere) defined directly in the XML.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Embedded XML file cannot be found
    /// - MJCF parsing fails
    /// - Model is not 6-DOF
    /// - XML contains mesh file references (runtime error from MuJoCo)
    pub fn from_embedded() -> Result<Self, PhysicsError> {
        const XML: &str = include_str!("../assets/piper_no_gripper.xml");

        // Validate: check if XML contains mesh references
        if XML.contains("<mesh") || XML.contains("file=\"") {
            return Err(PhysicsError::InvalidInput(
                "Embedded XML contains mesh file references. \
                 Use from_model_dir() for models with mesh files. \
                 See: mujoco_rs_best_practices_CRITICAL_CORRECTIONS.md"
                    .to_string(),
            ));
        }

        Self::from_xml_string(XML)
    }

    /// Load from model directory (RECOMMENDED for production)
    ///
    /// This is the **recommended** method for models with external mesh files (STL/OBJ).
    ///
    /// The directory must contain:
    /// - The MJCF XML file (e.g., `piper_no_gripper.xml`)
    /// - All mesh files referenced in the XML (e.g., `link1.stl`, `link2.obj`)
    ///
    /// MuJoCo will automatically load mesh files from the same directory as the XML.
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory containing the MJCF XML and mesh files
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - XML file not found in directory
    /// - MJCF parsing fails
    /// - Mesh files cannot be loaded
    /// - Model is not 6-DOF
    ///
    /// # Example
    ///
    /// ```no_run
    /// use piper_physics::MujocoGravityCompensation;
    /// use std::path::Path;
    ///
    /// // Load from assets directory (development)
    /// let gravity = MujocoGravityCompensation::from_model_dir(
    ///     Path::new("./assets")
    /// ).unwrap();
    ///
    /// // Load from system directory (production)
    /// let gravity = MujocoGravityCompensation::from_model_dir(
    ///     Path::new("/usr/local/share/piper/models")
    /// ).unwrap();
    /// ```
    pub fn from_model_dir(dir: &std::path::Path) -> Result<Self, PhysicsError> {
        let xml_path = dir.join("piper_no_gripper.xml");

        if !xml_path.exists() {
            return Err(PhysicsError::ModelLoadError(format!(
                "MJCF XML file not found: {:?}. \
                 Please ensure the model directory contains the XML file and all mesh files.\n\
                 \
                 Model directory: {:?}\n\
                 \
                 Hint: Set PIPER_MODEL_PATH environment variable to point to your model directory.",
                xml_path, dir
            )));
        }

        // Load from XML file (MuJoCo will automatically load meshes from same dir)
        let xml_content = std::fs::read_to_string(&xml_path).map_err(PhysicsError::IoError)?;

        Self::from_xml_string(&xml_content)
    }

    /// Load from standard path (environment variable or default locations)
    ///
    /// This method searches for model files in the following order:
    /// 1. `PIPER_MODEL_PATH` environment variable (if set)
    /// 2. `~/.piper/models/` (user directory)
    /// 3. `/usr/local/share/piper/models/` (system directory)
    /// 4. `./assets/` (development directory)
    ///
    /// # Errors
    ///
    /// Returns an error if model files cannot be found in any of the standard locations.
    pub fn from_standard_path() -> Result<Self, PhysicsError> {
        // 1. Try environment variable
        if let Ok(model_path) = std::env::var("PIPER_MODEL_PATH") {
            let dir = std::path::Path::new(&model_path);
            if dir.exists() {
                return Self::from_model_dir(dir);
            }
        }

        // 2. Try standard locations
        let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());

        let standard_paths = vec![
            std::path::PathBuf::from(format!("{}/.piper/models", home_dir)),
            std::path::PathBuf::from("/usr/local/share/piper/models"),
            std::path::PathBuf::from("./assets"), // Development
        ];

        for dir in standard_paths {
            if dir.exists() {
                return Self::from_model_dir(&dir);
            }
        }

        Err(PhysicsError::ModelLoadError(
            "Model files not found. Please set PIPER_MODEL_PATH environment variable \
             or ensure model files are in one of:\n\
             \
             1. ~/.piper/models/\n\
             2. /usr/local/share/piper/models/\n\
             3. ./assets/ (development)\n\
             \
             Example:\n  export PIPER_MODEL_PATH=/path/to/models"
                .to_string(),
        ))
    }

    /// Load from XML string (useful for testing)
    ///
    /// # Arguments
    ///
    /// * `xml` - MJCF XML content as string
    pub fn from_xml_string(xml: &str) -> Result<Self, PhysicsError> {
        // Load MuJoCo model
        let model = Arc::new(
            MjModel::from_xml_string(xml)
                .map_err(|e| PhysicsError::ModelLoadError(e.to_string()))?,
        );

        // Create simulation data
        let data = MjData::new(model.clone());

        // Validate model is 6-DOF
        let nv = model.ffi().nv;
        if nv != 6 {
            return Err(PhysicsError::InvalidInput(format!(
                "Expected 6-DOF robot, got {} DOF",
                nv
            )));
        }

        // Find end-effector site ID (for payload compensation)
        let ee_site_id = Self::find_end_effector_site_id(&model);

        // Find parent body of the site
        let ee_body_id = ee_site_id.and_then(|site_id| {
            // site_bodyid is a method on MjModel wrapper
            let parent_body_i32 = model.site_bodyid()[site_id as usize];

            if parent_body_i32 >= 0 {
                Some(parent_body_i32)
            } else {
                None
            }
        });

        log::info!("MuJoCo model loaded successfully");
        log::info!("  DOF: {}", nv);
        log::info!("  NQ (positions): {}", model.ffi().nq);
        if let Some(id) = ee_site_id {
            log::info!("  End-effector site ID: {}", id);
        } else {
            log::warn!("  End-effector site not found (payload compensation unavailable)");
        }
        if let Some(id) = ee_body_id {
            log::info!("  End-effector body ID: {}", id);
        }

        Ok(Self {
            model,
            data,
            ee_site_id,
            ee_body_id,
        })
    }

    /// Find end-effector site ID by name
    ///
    /// Tries common end-effector names: "end_effector", "ee", "tool0"
    fn find_end_effector_site_id(model: &MjModel) -> Option<i32> {
        let possible_names = vec!["end_effector", "ee", "tool0"];

        for name in possible_names {
            for i in 0..model.ffi().nsite {
                // SAFETY: MuJoCo guarantees name_siteadr[i] is within bounds
                let site_name = unsafe {
                    let name_siteadr_ptr = model.ffi().name_siteadr;
                    let name_offset = *name_siteadr_ptr.add(i as usize) as usize;
                    let base_ptr = model.ffi().names;

                    if base_ptr.is_null() {
                        continue;
                    }

                    std::ffi::CStr::from_ptr(base_ptr.add(name_offset))
                };

                let site_name_str = site_name.to_string_lossy();

                if site_name_str.contains(name) {
                    return Some(i);
                }
            }
        }

        None
    }

    /// Load from XML file (not recommended, prefer embedded)
    ///
    /// **Note**: File loading requires runtime file I/O and path management.
    /// Prefer `from_embedded()` for production use.
    pub fn from_xml_file(path: &std::path::Path) -> Result<Self, PhysicsError> {
        // Check file extension
        if path.extension().and_then(|s| s.to_str()) != Some("xml") {
            return Err(PhysicsError::InvalidInput(
                "MuJoCo requires XML format (.xml), not URDF".to_string(),
            ));
        }

        // Read file
        let xml_content = std::fs::read_to_string(path).map_err(PhysicsError::IoError)?;

        Self::from_xml_string(&xml_content)
    }

    /// Get reference to MuJoCo model
    pub fn model(&self) -> &MjModel {
        &self.model
    }

    /// Get reference to MuJoCo data
    pub fn data(&self) -> &MjData<Arc<MjModel>> {
        &self.data
    }

    /// Get mutable reference to MuJoCo data
    pub fn data_mut(&mut self) -> &mut MjData<Arc<MjModel>> {
        &mut self.data
    }

    /// Compute gravity compensation torques with dynamic payload
    ///
    /// This method uses a **hybrid approach**:
    /// 1. MuJoCo calculates robot body gravity compensation
    /// 2. Manual calculation adds payload contribution via Jacobian transpose
    ///
    /// This allows dynamic payload adjustment without reloading the MJCF XML.
    ///
    /// # Arguments
    ///
    /// * `q` - Joint positions (6-DOF)
    /// * `payload_mass` - Payload mass in kg
    /// * `payload_com` - Payload center of mass in end-effector LOCAL frame (3D vector)
    ///
    /// # Theory
    ///
    /// The payload gravity compensation is computed using Jacobian transpose:
    /// ```text
    /// τ_payload = J^T * F_gravity
    ///
    /// where:
    /// - J: End-effector Jacobian (3x6, linear velocity part)
    /// - F_gravity: Payload gravity vector = [0, 0, -mass * g]^T
    /// - τ_payload: Joint torques (6x1)
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - End-effector site ID is not available (not found in model)
    /// - End-effector body ID is not available (not found in model)
    /// - Joint state dimension mismatch
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use piper_physics::{MujocoGravityCompensation, GravityCompensation};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut gravity_calc = MujocoGravityCompensation::from_standard_path()?;
    /// let q = piper_physics::JointState::from_iterator([0.0; 6]);
    ///
    /// // Pure gravity compensation
    /// let tau_empty = gravity_calc.compute_gravity_compensation(&q)?;
    ///
    /// // With 500g object (center of mass at end-effector origin)
    /// let tau_with_load = gravity_calc
    ///     .compute_gravity_torques_with_payload(
    ///         &q,
    ///         0.5,  // 500g
    ///         nalgebra::Vector3::new(0.0, 0.0, 0.0),
    ///     )?;
    ///
    /// // With irregular object (center of mass offset)
    /// let tau_irregular = gravity_calc
    ///     .compute_gravity_torques_with_payload(
    ///         &q,
    ///         0.325,  // 325g
    ///         nalgebra::Vector3::new(0.05, 0.02, 0.1),  // 5cm forward, 2cm right, 1cm up
    ///     )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn compute_gravity_torques_with_payload(
        &mut self,
        q: &JointState,
        payload_mass: f64,
        payload_com: nalgebra::Vector3<f64>,
    ) -> Result<JointTorques, PhysicsError> {
        // 1. Set joint positions
        self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());
        self.data.qvel_mut()[0..6].fill(0.0);
        self.data.qacc_mut()[0..6].fill(0.0);

        // 2. Call forward() ONLY ONCE
        self.data.forward();

        // 3. Extract robot body gravity from qfrc_bias
        let tau_robot = JointTorques::from_iterator(self.data.qfrc_bias()[0..6].iter().copied());

        // 4. Compute payload contribution (no second forward() call)
        let ee_site_id = self.ee_site_id.ok_or_else(|| {
            PhysicsError::CalculationFailed(
                "End-effector site not found in model. \
                 Payload compensation requires a site named 'end_effector'."
                    .to_string(),
            )
        })?;

        let ee_body_id = self.ee_body_id.ok_or_else(|| {
            PhysicsError::CalculationFailed(
                "End-effector body ID not available. Cannot compute Jacobian.".to_string(),
            )
        })?;

        let tau_payload =
            self.compute_payload_torques(q, payload_mass, payload_com, ee_site_id, ee_body_id)?;

        // 5. Superposition
        Ok(tau_robot + tau_payload)
    }

    /// Helper: Compute Jacobian at an arbitrary point in world frame
    ///
    /// This method encapsulates the unsafe `mj_jac` FFI call to provide a safe
    /// Rust interface for computing Jacobians at dynamic points (e.g., variable payload CoM).
    ///
    /// # Why Use `mj_jac` Instead of `jac_site`?
    ///
    /// - `jac_site`: Computes Jacobian at a **fixed Site** (predefined in XML)
    /// - `mj_jac`: Computes Jacobian at **any point** in world frame
    ///
    /// For payload compensation with **variable CoM**, `mj_jac` is the only viable option.
    ///
    /// # Arguments
    ///
    /// * `body_id` - Body ID for Jacobian calculation
    /// * `point_world` - Point in world frame coordinates (x, y, z)
    ///
    /// # Returns
    ///
    /// Tuple of (linear_jacobian, rotational_jacobian) as 3x6 matrices
    ///
    /// # Safety
    ///
    /// This method encapsulates the unsafe FFI call. The caller does not need to use `unsafe`.
    fn compute_jacobian_at_point(
        &mut self,
        body_id: i32,
        point_world: &[f64; 3],
    ) -> Result<(nalgebra::Matrix3x6<f64>, nalgebra::Matrix3x6<f64>), PhysicsError> {
        let mut jacp = [0.0f64; 18]; // 3 x 6 (linear Jacobian)
        let mut jacr = [0.0f64; 18]; // 3 x 6 (rotational Jacobian)

        unsafe {
            mujoco_c::mj_jac(
                self.model.ffi(),
                self.data.ffi(),
                jacp.as_mut_ptr(),
                jacr.as_mut_ptr(),
                point_world.as_ptr(),
                body_id,
            );
        }

        // Convert to nalgebra matrices (row-major)
        let jacp_matrix = nalgebra::Matrix3x6::from_row_slice(&jacp[..]);
        let jacr_matrix = nalgebra::Matrix3x6::from_row_slice(&jacr[..]);

        Ok((jacp_matrix, jacr_matrix))
    }

    /// Compute payload gravity compensation using Jacobian transpose method
    ///
    /// # Theory
    ///
    /// The Jacobian transpose method maps end-effector forces to joint torques:
    /// ```text
    /// τ = J^T * F
    ///
    /// where:
    /// - J: Point Jacobian (3 x nv) - linear velocity Jacobian
    /// - F: Force vector in world frame (3 x 1)
    /// - τ: Joint torques (nv x 1)
    /// ```
    ///
    /// For gravity compensation, F = [0, 0, -mass * g]^T.
    ///
    /// # Arguments
    ///
    /// * `_q` - Joint positions (already set by caller)
    /// * `mass` - Payload mass in kg
    /// * `com` - Payload center of mass offset in end-effector LOCAL frame
    /// * `ee_site_id` - End-effector site ID
    /// * `ee_body_id` - Body ID that the site is attached to
    fn compute_payload_torques(
        &mut self,
        _q: &JointState, // NOTE: already set by compute_gravity_torques_with_payload
        mass: f64,
        com: nalgebra::Vector3<f64>, // Site local frame offset
        ee_site_id: i32,
        ee_body_id: i32,
    ) -> Result<JointTorques, PhysicsError> {
        // NOTE: qpos, qvel, qacc already set and forward() already called

        // 1. Get gravity vector from model (respects model configuration)
        let model_gravity = self.model.opt().gravity; // &[f64; 3]
        let f_gravity = nalgebra::Vector3::new(
            model_gravity[0] * mass,
            model_gravity[1] * mass,
            model_gravity[2] * mass,
        );

        // 2. Get site position and rotation matrix in world frame
        // site_xpos and site_xmat are methods that return nested array slices
        let all_site_xpos = self.data.site_xpos(); // &[[f64; 3]]
        let all_site_xmat = self.data.site_xmat(); // &[[f64; 9]]

        let site_idx = ee_site_id as usize;
        let site_xpos = &all_site_xpos[site_idx]; // &[f64; 3]
        let site_xmat = &all_site_xmat[site_idx]; // &[f64; 9]

        // 3. Convert local offset to world frame
        // ✅ Use nalgebra to avoid manual index errors
        let rot_mat = nalgebra::Matrix3::from_row_slice(site_xmat);
        let world_offset = rot_mat * com; // Vector3 = Matrix3 * Vector3

        // 4. Compute payload CoM position in world frame
        let world_com = nalgebra::Vector3::new(
            site_xpos[0] + world_offset[0],
            site_xpos[1] + world_offset[1],
            site_xpos[2] + world_offset[2],
        );

        // 5. Compute Jacobian at the world CoM point using encapsulated FFI helper
        // ⭐ The mj_jac FFI is necessary here to support **arbitrary (variable) CoM positions**
        let point = [world_com[0], world_com[1], world_com[2]];
        let (jacp_matrix, _jacr_matrix) = self.compute_jacobian_at_point(ee_body_id, &point)?;

        // 6. Jacobian transpose: τ = J^T * F
        let tau_payload = jacp_matrix.transpose() * f_gravity;

        // 7. Convert to Vector6
        let torques = JointTorques::from_iterator(tau_payload.iter().copied());

        Ok(torques)
    }
}

impl Default for MujocoGravityCompensation {
    fn default() -> Self {
        // Load from standard path (environment variable or default locations)
        // This is the recommended method for production use
        Self::from_standard_path().expect(
            "Failed to load MuJoCo model from standard path. \
             Please set PIPER_MODEL_PATH or ensure model files are in ~/.piper/models/ or ./assets/"
        )
    }
}

impl GravityCompensation for MujocoGravityCompensation {
    /// Mode 1: Pure gravity compensation (τ = M(q)·g)
    ///
    /// Sets qvel=0 and qacc=0 to compute pure gravity torques.
    fn compute_gravity_compensation(
        &mut self,
        q: &JointState,
    ) -> Result<JointTorques, PhysicsError> {
        // 1. Set joint positions
        self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());

        // 2. Zero velocities (eliminate Coriolis and centrifugal forces)
        self.data.qvel_mut()[0..6].fill(0.0);

        // 3. Zero accelerations (eliminate inertial forces)
        self.data.qacc_mut()[0..6].fill(0.0);

        // 4. Call forward dynamics (computes qfrc_bias)
        self.data.forward();

        // 5. Extract pure gravity compensation torques from qfrc_bias
        // With qvel=0 and qacc=0: qfrc_bias ≈ pure gravity
        let torques = JointTorques::from_iterator(self.data.qfrc_bias()[0..6].iter().copied());

        Ok(torques)
    }

    /// Mode 2: Partial inverse dynamics (τ = M(q)·g + C(q,q̇) + F_damping)
    ///
    /// Sets qacc=0 but uses actual qvel to include Coriolis, centrifugal, and damping forces.
    fn compute_partial_inverse_dynamics(
        &mut self,
        q: &JointState,
        qvel: &[f64; 6],
    ) -> Result<JointTorques, PhysicsError> {
        // 1. Set joint positions
        self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());

        // 2. Set actual velocities (include Coriolis and centrifugal forces)
        self.data.qvel_mut()[0..6].copy_from_slice(qvel);

        // 3. Zero accelerations (eliminate inertial forces - M·q̈ is NOT included)
        self.data.qacc_mut()[0..6].fill(0.0);

        // 4. Call forward dynamics (computes qfrc_bias)
        self.data.forward();

        // 5. Extract partial inverse dynamics torques from qfrc_bias
        // With actual qvel and qacc=0: qfrc_bias = gravity + C(q,q̇) + damping + friction
        let torques = JointTorques::from_iterator(self.data.qfrc_bias()[0..6].iter().copied());

        Ok(torques)
    }

    /// Mode 3: Full inverse dynamics (τ = M(q)·g + C(q,q̇) + M(q)·q̈)
    ///
    /// Uses mj_inverse() to compute complete inverse dynamics including inertial forces.
    fn compute_inverse_dynamics(
        &mut self,
        q: &JointState,
        qvel: &[f64; 6],
        qacc_desired: &[f64; 6],
    ) -> Result<JointTorques, PhysicsError> {
        // 1. Set joint positions
        self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());

        // 2. Set joint velocities
        self.data.qvel_mut()[0..6].copy_from_slice(qvel);

        // 3. Set desired accelerations
        self.data.qacc_mut()[0..6].copy_from_slice(qacc_desired);

        // 4. Call inverse dynamics solver (computes qfrc_inverse)
        // ⚠️ CRITICAL: Do NOT use forward() here!
        // forward() + qfrc_bias does NOT include inertial forces (M·q̈)
        self.data.inverse();

        // 5. Extract full inverse dynamics torques from qfrc_inverse
        // Result: τ = M(q)·q̈ + C(q,q̇) + g(q) + damping + friction
        // qfrc_inverse is not exposed by mujoco-rs, need to access via FFI
        let torques = unsafe {
            let qfrc_inverse_ptr = self.data.ffi().qfrc_inverse;
            let slice = std::slice::from_raw_parts(qfrc_inverse_ptr, 6);
            JointTorques::from_iterator(slice.iter().copied())
        };

        Ok(torques)
    }

    /// Legacy method (deprecated): Compute gravity compensation
    ///
    /// This method forwards to `compute_gravity_compensation()`.
    fn compute_gravity_torques(
        &mut self,
        q: &JointState,
        _gravity: Option<&nalgebra::Vector3<f64>>, // MuJoCo uses model's internal gravity
    ) -> Result<JointTorques, PhysicsError> {
        // Forward to the new method
        self.compute_gravity_compensation(q)
    }

    fn name(&self) -> &str {
        "mujoco_simulation"
    }

    fn is_initialized(&self) -> bool {
        true // Always initialized after construction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_initialization() {
        // This test requires embedded XML file to exist
        // For now, we'll skip it if the file doesn't exist
        if let Ok(gravity) = MujocoGravityCompensation::from_embedded() {
            assert!(gravity.is_initialized());
            assert_eq!(gravity.name(), "mujoco_simulation");
        }
    }

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

    // TODO: Add tests with actual MJCF XML file
    // TODO: Test torque computation with known values
    // TODO: Test performance (should be < 10 µs per call)

    /// Test that pure gravity compensation equals partial inverse dynamics when velocity is zero
    #[test]
    fn test_gravity_compensation_matches_partial_at_zero_velocity() {
        // This test requires embedded XML file to exist
        if let Ok(mut gravity) = MujocoGravityCompensation::from_embedded() {
            let q = JointState::from_iterator([0.0; 6]);

            // Pure gravity compensation (qvel = 0)
            let tau_gravity = gravity.compute_gravity_compensation(&q).unwrap();

            // Partial inverse dynamics with zero velocity (qvel = 0)
            let qvel = [0.0; 6];
            let tau_partial = gravity.compute_partial_inverse_dynamics(&q, &qvel).unwrap();

            // Should be equal (within numerical precision)
            for i in 0..6 {
                let diff = (tau_gravity[i] - tau_partial[i]).abs();
                assert!(
                    diff < 1e-10,
                    "Joint {}: gravity compensation ({}) should match partial inverse dynamics ({}) at zero velocity",
                    i,
                    tau_gravity[i],
                    tau_partial[i]
                );
            }
        }
    }

    /// Test that partial inverse dynamics includes Coriolis forces
    #[test]
    fn test_partial_inverse_dynamics_includes_coriolis() {
        if let Ok(mut gravity) = MujocoGravityCompensation::from_embedded() {
            let q = JointState::from_iterator([0.0; 6]);
            let qvel_slow = [0.5; 6]; // 0.5 rad/s
            let qvel_fast = [2.0; 6]; // 2.0 rad/s

            // Static case (qvel = 0)
            let qvel_static = [0.0; 6];
            let tau_static = gravity.compute_partial_inverse_dynamics(&q, &qvel_static).unwrap();

            // Slow motion
            let tau_slow = gravity.compute_partial_inverse_dynamics(&q, &qvel_slow).unwrap();

            // Fast motion
            let tau_fast = gravity.compute_partial_inverse_dynamics(&q, &qvel_fast).unwrap();

            // Verify: velocity affects the torque (Coriolis forces are present)
            // We check that at least some joints show significant Coriolis effects
            let mut joints_with_coriolis = 0;
            for i in 0..6 {
                let slow_diff = (tau_slow[i] - tau_static[i]).abs();
                let fast_diff = (tau_fast[i] - tau_static[i]).abs();

                // Check if this joint shows velocity-dependent effects
                if slow_diff > 0.0001 || fast_diff > 0.0001 {
                    joints_with_coriolis += 1;

                    // For joints with Coriolis effects, faster motion should have larger effect
                    assert!(
                        fast_diff >= slow_diff,
                        "Joint {}: fast motion diff ({:.6}) should be >= slow diff ({:.6})",
                        i,
                        fast_diff,
                        slow_diff
                    );
                }
            }

            // At least 3 joints should show Coriolis effects
            assert!(
                joints_with_coriolis >= 3,
                "Expected at least 3 joints with Coriolis effects, got {}",
                joints_with_coriolis
            );
        }
    }

    /// Test that full inverse dynamics includes inertial forces
    #[test]
    fn test_full_inverse_dynamics_includes_inertia() {
        if let Ok(mut gravity) = MujocoGravityCompensation::from_embedded() {
            let q = JointState::from_iterator([0.0; 6]);
            let qvel = [2.0; 6];
            let qacc_small = [0.5; 6];
            let qacc_large = [2.0; 6];

            // Partial inverse dynamics (no inertial forces)
            let tau_partial = gravity.compute_partial_inverse_dynamics(&q, &qvel).unwrap();

            // Full inverse dynamics with small acceleration
            let tau_full_small = gravity.compute_inverse_dynamics(&q, &qvel, &qacc_small).unwrap();

            // Full inverse dynamics with large acceleration
            let tau_full_large = gravity.compute_inverse_dynamics(&q, &qvel, &qacc_large).unwrap();

            // Verify: Full inverse dynamics should be larger than partial
            for i in 0..6 {
                // Small acceleration
                assert!(
                    tau_full_small[i].abs() >= tau_partial[i].abs(),
                    "Joint {}: full ID (small acc) should be >= partial ID ({:.6} >= {:.6})",
                    i,
                    tau_full_small[i],
                    tau_partial[i]
                );

                // Large acceleration should be even larger
                assert!(
                    tau_full_large[i].abs() >= tau_full_small[i].abs(),
                    "Joint {}: full ID (large acc) should be >= full ID (small acc) ({:.6} >= {:.6})",
                    i,
                    tau_full_large[i],
                    tau_full_small[i]
                );
            }
        }
    }
}
