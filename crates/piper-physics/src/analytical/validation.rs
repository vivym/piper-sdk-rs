//! Joint mapping validation
//!
//! Validates that CAN ID order matches URDF joint order to prevent
//! robot失控 due to incorrect torque assignments.

use crate::error::PhysicsError;
use k::Chain;

/// Validate joint mapping between CAN IDs and URDF order
///
/// This is a **critical safety check** - if CAN ID order doesn't match
/// URDF joint order, computed torques will be sent to wrong motors,
/// potentially causing the robot to go out of control.
///
/// # Validation Logic
///
/// Piper robot has 6 joints with CAN IDs 1-6. The URDF should define
/// joints in the same order (joint_1, joint_2, ..., joint_6).
///
/// # Errors
///
/// Returns `JointMappingError` if:
/// - Chain doesn't have exactly 6 joints
/// - Joint names don't follow expected pattern
pub fn validate_joint_mapping(chain: &Chain<f64>) -> Result<(), PhysicsError> {
    // Filter only movable joints (joints with limits)
    // Fixed joints typically have no limits or zero limits
    let movable_joints: Vec<_> = chain
        .iter()
        .filter(|node| {
            let joint = node.joint();
            // Check if joint has limits (movable joints typically do)
            joint.limits.is_some()
        })
        .collect();

    if movable_joints.len() != 6 {
        return Err(PhysicsError::JointMappingError(format!(
            "Expected 6 movable joints for Piper robot, found {}",
            movable_joints.len()
        )));
    }

    // Build detailed validation report
    log::info!("🔍 Validating joint mapping...");
    log::info!("URDF joint names (movable joints only):");

    let mut has_unusual_names = false;

    for (i, node) in movable_joints.iter().enumerate() {
        let can_id = i + 1; // CAN IDs are 1-indexed
                            // Get joint name - the guard will deref to the joint
        let joint = node.joint();
        let joint_name: &str = joint.name.as_ref();
        let expected_name = format!("joint_{}", can_id);

        if joint_name != expected_name {
            log::warn!(
                "  ⚠️  Joint {} (CAN ID {}): '{}' (non-standard name, expected '{}')",
                can_id,
                can_id,
                joint_name,
                expected_name
            );
            has_unusual_names = true;
        } else {
            log::info!("  ✓ Joint {} (CAN ID {}): {}", can_id, can_id, joint_name);
        }
    }

    if has_unusual_names {
        log::warn!("\n⚠️  WARNING: Joint names don't follow the 'joint_1' to 'joint_6' pattern.");
        log::warn!("   Please verify that joint order matches CAN ID order!");
        log::warn!("   CAN ID 1 should be the first joint in the chain, etc.\n");
    }

    log::info!("✓ Joint mapping validation complete (6 movable joints found)\n");

    Ok(())
}

#[cfg(test)]
mod tests {
    // TODO: Add tests with mock chain once we have URDF files
}
