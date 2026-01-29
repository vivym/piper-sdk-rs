# Quick Fix Guide - piper-physics Critical Issues

**Status**: 🔴 DO NOT USE IN PRODUCTION until Phase 1 fixes are complete

---

## ✅ Completed Fixes

- ✅ **Issue #5**: Syntax error in `analytical.rs` - FIXED

---

## Phase 1: Critical Fixes (~40 minutes)

### Fix #1: Syntax Error in `analytical.rs`
**Status**: ✅ DONE

### Fix #2: End-Effector Body/Site Mismatch
**File**: `assets/piper_no_gripper.xml`
**Time**: 5 minutes

**Problem**: XML has a `<site>` but code searches for `<body>`

**Solution A** (Recommended): Change code to search for sites

```rust
// In src/mujoco.rs, replace find_end_effector_body_id with:

fn find_end_effector_site_id(model: &MjModel) -> Option<mujoco_rs::sys::mjnSite> {
    let possible_names = vec!["end_effector", "ee", "tool0"];

    for name in possible_names {
        for i in 0..model.ffi().nsite {
            let site_name = unsafe {
                std::ffi::CStr::from_ptr(
                    (*model.ffi()).names.add((*model.ffi()).name_siteadr[i] as usize)
                )
            };
            let site_name_str = site_name.to_string_lossy();

            if site_name_str.contains(name) {
                return Some(i as mujoco_rs::sys::mjnSite);
            }
        }
    }

    None
}

// Update struct field:
pub struct MujocoGravityCompensation {
    model: Rc<MjModel>,
    data: MjData<Rc<MjModel>>,
    ee_site_id: Option<mujoco_rs::sys::mjnSite>,  // Changed from ee_body_id
}

// Update from_xml_string to use site_id:
let ee_site_id = Self::find_end_effector_site_id(&model);
```

**Solution B**: Change XML to add end-effector body

```xml
<!-- In assets/piper_no_gripper.xml -->
<body name="link_6" pos="0 0 0.025">
  <joint name="joint_6" .../>
  <geom name="link_6_geom" .../>
  <site name="end_effector" pos="0 0 0.02" size="0.01" rgba="1 0 0 1"/>

  <!-- ADD THIS: -->
  <body name="end_effector" pos="0 0 0.02">
    <site name="ee_point" pos="0 0 0"/>
  </body>
</body>
```

### Fix #3: Double `forward()` Call
**File**: `src/mujoco.rs`
**Time**: 15 minutes

**Problem**: `compute_gravity_torques_with_payload()` calls `forward()` twice

```rust
// REPLACE compute_payload_torques with:

fn compute_payload_torques(
    &mut self,
    _q: &JointState,  // Already set by caller
    mass: f64,
    com: nalgebra::Vector3<f64>,
    ee_site_id: mujoco_rs::sys::mjnSite,  // Use site_id instead of body_id
) -> Result<JointTorques, PhysicsError> {
    // NOTE: qpos, qvel, qacc already set by compute_gravity_torques_with_payload
    // NOTE: forward() already called by compute_gravity_torques_with_payload

    // 1. Get gravity from model (respects model configuration)
    let model_gravity = self.model.opt().gravity;
    let f_gravity = nalgebra::Vector3::new(
        model_gravity[0] * mass,
        model_gravity[1] * mass,
        model_gravity[2] * mass,
    );

    // 2. Get end-effector Jacobian (linear velocity part)
    let jacp = self.data.jacp(ee_site_id);

    // 3. Jacobian transpose: τ = J^T * F
    let mut jacp_matrix = nalgebra::Matrix3x6::<f64>::zeros();
    for i in 0..3 {
        for j in 0..6 {
            jacp_matrix[(i, j)] = jacp[i * 6 + j];
        }
    }

    let tau_payload = jacp_matrix.transpose() * f_gravity;
    let torques = JointTorques::from_iterator(tau_payload.iter());

    Ok(torques)
}

// UPDATE compute_gravity_torques_with_payload:

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

    // 2. Call forward() ONCE
    self.data.forward();

    // 3. Extract robot body gravity from qfrc_bias
    let tau_robot = JointTorques::from_iterator(
        self.data.qfrc_bias()[0..6].iter().copied()
    );

    // 4. Compute payload (no second forward() call)
    let ee_site_id = self.ee_site_id.ok_or_else(|| {
        PhysicsError::CalculationFailed(
            "End-effector site not found in model. \
             Payload compensation requires a site named 'end_effector'.".to_string()
        )
    })?;

    let tau_payload = self.compute_payload_torques(q, payload_mass, payload_com, ee_site_id)?;

    // 5. Superposition
    Ok(tau_robot + tau_payload)
}
```

### Fix #4: Incorrect Gravity Handling
**File**: `src/mujoco.rs:431-436`
**Time**: 5 minutes

**Problem**: Hardcoded `9.81` instead of using model's gravity

**Included in Fix #3 above** - this is already corrected in the new code.

---

## Phase 2: Important Fixes (~55 minutes)

### Fix #5: COM Offset Parameter
**File**: `src/mujoco.rs`
**Time**: 30 minutes

**Problem**: `_com` parameter is ignored

**Fix**: Use MuJoCo's `mj_jacBody` to compute Jacobian at offset point

```rust
// In compute_payload_torques, add:

fn compute_payload_torques(
    &mut self,
    _q: &JointState,
    mass: f64,
    com: nalgebra::Vector3<f64>,  // NOW USED!
    ee_site_id: mujoco_rs::sys::mjnSite,
) -> Result<JointTorques, PhysicsError> {
    // ... state setup ...

    // Get model gravity
    let model_gravity = self.model.opt().gravity;
    let f_gravity = nalgebra::Vector3::new(
        model_gravity[0] * mass,
        model_gravity[1] * mass,
        model_gravity[2] * mass,
    );

    // Compute Jacobian at offset point
    let mut jacp = [0.0f64; 18];  // 3 x 6
    let mut jacr = [0.0f64; 18];  // 3 x 6 (not used but required)

    unsafe {
        mujoco_rs::sys::mj_jacSite(
            self.model.ffi(),
            self.data.ffi(),
            jacp.as_mut_ptr(),
            jacr.as_mut_ptr(),
            ee_site_id as i32,
            com[0], com[1], com[2],  // Offset point
        );
    }

    // Convert to nalgebra matrix
    let mut jacp_matrix = nalgebra::Matrix3x6::<f64>::zeros();
    for i in 0..3 {
        for j in 0..6 {
            jacp_matrix[(i, j)] = jacp[i * 6 + j];
        }
    }

    let tau_payload = jacp_matrix.transpose() * f_gravity;
    let torques = JointTorques::from_iterator(tau_payload.iter());

    Ok(torques)
}
```

### Fix #6: Joint Name Validation
**File**: `src/analytical/validation.rs`
**Time**: 15 minutes

**Problem**: Only validates count, not names

```rust
pub fn validate_joint_mapping(chain: &Chain<f64>) -> Result<(), PhysicsError> {
    let movable_joints: Vec<_> = chain
        .iter()
        .filter(|node| node.joint().limits.is_some())
        .collect();

    if movable_joints.len() != 6 {
        return Err(PhysicsError::JointMappingError(format!(
            "Expected 6 movable joints for Piper robot, found {}",
            movable_joints.len()
        )));
    }

    println!("🔍 Validating joint mapping...");
    println!("URDF joint names (movable joints only):");

    // VALIDATE each joint name
    for (i, node) in movable_joints.iter().enumerate() {
        let can_id = i + 1;
        let joint = node.joint();
        let joint_name: &str = joint.name.as_ref();
        let expected_name = format!("joint_{}", can_id);

        if joint_name != expected_name {
            return Err(PhysicsError::JointMappingError(format!(
                "❌ CRITICAL: Joint name mismatch!\n   Position {}: Expected '{}', found '{}'\n   \
                 This would send torques to the wrong motors and cause robot失控!",
                can_id, expected_name, joint_name
            )));
        }

        println!("  ✓ Joint {} (CAN ID {}): {}", can_id, can_id, joint_name);
    }

    println!();
    println!("✓ Joint mapping validation complete");
    Ok(())
}
```

### Fix #7: Unsafe Code Safety
**File**: `src/mujoco.rs:264-266`
**Time**: 10 minutes

```rust
for i in 0..model.ffi().nbody {
    // SAFETY: MuJoCo guarantees name_bodyadr[i] is within bounds
    let body_name = unsafe {
        let name_offset = (*model.ffi()).name_bodyadr[i] as usize;
        let base_ptr = (*model.ffi()).names;

        // Defensive check
        if base_ptr.is_null() {
            continue;
        }

        std::ffi::CStr::from_ptr(base_ptr.add(name_offset))
    };

    let body_name_str = body_name.to_string_lossy();

    if body_name_str.contains(name) {
        return Some(i as mujoco_rs::sys::mjnBody);
    }
}
```

---

## Phase 3: Quality Fixes (~20 minutes)

### Fix #8: Document Thread Safety

```rust
/// Compute gravity compensation torques
///
/// # Thread Safety
///
/// ⚠️ **NOT thread-safe**: This method mutates internal state (`self.data`).
/// Do not call this method concurrently from multiple threads on the same instance.
///
/// For concurrent use:
/// - Clone the instance (cheap due to `Rc<MjModel>`), or
/// - Wrap in a `Mutex<MujocoGravityCompensation>`
fn compute_gravity_torques(...) -> Result<JointTorques, PhysicsError> {
    // ...
}
```

### Fix #9: Remove Unimplemented Method

```rust
// In src/analytical.rs, remove or comment out:

/*
/// Create from default Piper URDF (embedded)
///
/// This loads the default URDF file embedded at compile time,
/// which represents the standard Piper robot configuration.
///
/// # Errors
///
/// Returns an error if URDF parsing or joint mapping validation fails.
#[doc(hidden)]
pub fn from_piper_urdf() -> Result<Self, PhysicsError> {
    // TODO: Embed default URDF using include_str!
    // Requires k crate to support loading from string (currently requires file path)
    Err(PhysicsError::NotInitialized)
}
*/
```

### Fix #10: Better Mesh Validation

```rust
use regex::Regex;

// In from_embedded():

// Check for mesh file references using regex
lazy_static::lazy_static! {
    static ref MESH_FILE_PATTERN: Regex = Regex::new(
        r#"<mesh[^>]+file\s*=\s*["'][^"']+["']"#
    ).unwrap();
}

if MESH_FILE_PATTERN.is_match(XML) {
    return Err(PhysicsError::InvalidInput(
        "Embedded XML contains mesh file references. \
         Use from_model_dir() for models with mesh files. \
         \n\n\
         Found: <mesh file=\"...\" />\n\
         Solution: Use from_model_dir() or from_standard_path() instead.".to_string()
    ));
}
```

Add to `Cargo.toml`:
```toml
[dependencies]
lazy_static = "1.4"
regex = "1"
```

---

## Testing After Fixes

```bash
# 1. Verify compilation
cargo check -p piper-physics --all-features

# 2. Run kinematics tests
cargo test -p piper-physics --no-default-features --features kinematics

# 3. Run example (if MuJoCo installed)
cargo run -p piper-physics --example gravity_compensation_analytical \
  --no-default-features --features kinematics
```

---

## Verification Checklist

- [x] Syntax error fixed
- [ ] End-effector body/site mismatch fixed
- [ ] Double `forward()` call eliminated
- [ ] Gravity vector uses model's value
- [ ] COM offset parameter is used
- [ ] Joint names are validated
- [ ] Unsafe code has defensive checks
- [ ] Thread safety documented
- [ ] Unimplemented method removed
- [ ] Mesh validation improved

---

## Summary

**Before Phase 1**: 🔴 CRITICAL - Do not use
**After Phase 1**: 🟡 USABLE - Basic gravity compensation works
**After Phase 2**: 🟢 PRODUCTION-READY - All critical issues resolved

**Estimated total time**: ~2 hours for all fixes
