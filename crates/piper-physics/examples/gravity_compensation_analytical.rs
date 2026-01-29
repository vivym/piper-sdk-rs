//! Gravity compensation example using analytical RNE algorithm
//!
//! This example demonstrates how to use the `AnalyticalGravityCompensation`
//! to compute gravitational torques for the Piper robot.
//!
//! **NOTE**: This example requires the `kinematics` feature to be enabled:
//! `cargo run --example gravity_compensation_analytical --features kinematics`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "kinematics")]
    {
        use piper_physics::{AnalyticalGravityCompensation, GravityCompensation};
        use std::{f64::consts::FRAC_PI_2, path::Path};

        println!("🤖 Piper Gravity Compensation Example (Analytical RNE)");
        println!("=====================================================\n");

        // 1. Load URDF file
        let urdf_path = Path::new("crates/piper-physics/assets/piper_description.urdf");
        println!("📄 Loading URDF from: {}", urdf_path.display());

        let mut gravity_calc = AnalyticalGravityCompensation::from_urdf(urdf_path)?;
        println!("✓ URDF loaded successfully\n");

        // 2. Test zero position (Mode 1: Pure gravity compensation)
        println!("📍 Computing gravity compensation torques for zero position...");
        let q_zero = nalgebra::Vector6::zeros();
        let torques_zero = gravity_calc.compute_gravity_compensation(&q_zero)?;
        println!("Torques at zero position:");
        for (i, &tau) in torques_zero.iter().enumerate() {
            println!("  Joint {}: {:.4} Nm", i + 1, tau);
        }
        println!();

        // 3. Test a horizontal pose (all joints at 90 degrees)
        println!("📍 Computing gravity compensation torques for horizontal pose...");
        let q_horizontal = nalgebra::Vector6::from_iterator(std::iter::repeat(FRAC_PI_2)); // π/2
        let torques_horizontal = gravity_calc.compute_gravity_compensation(&q_horizontal)?;
        println!("Torques at horizontal pose:");
        for (i, &tau) in torques_horizontal.iter().enumerate() {
            println!("  Joint {}: {:.4} Nm", i + 1, tau);
        }
        println!();

        // 4. Test Mode 2: Partial inverse dynamics (with velocities)
        println!("🌍 Computing torques with partial inverse dynamics (with velocities)...");
        let qvel = [0.5, 0.5, 0.5, 0.5, 0.5, 0.5]; // rad/s
        let torques_partial = gravity_calc.compute_partial_inverse_dynamics(&q_zero, &qvel)?;
        println!("Torques with partial inverse dynamics:");
        for (i, &tau) in torques_partial.iter().enumerate() {
            println!("  Joint {}: {:.4} Nm", i + 1, tau);
        }
        println!();

        // 5. Test Mode 3: Full inverse dynamics (with velocities and accelerations)
        println!("🚀 Computing torques with full inverse dynamics (with velocities and accelerations)...");
        let qacc = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0]; // rad/s²
        let torques_full = gravity_calc.compute_inverse_dynamics(&q_zero, &qvel, &qacc)?;
        println!("Torques with full inverse dynamics:");
        for (i, &tau) in torques_full.iter().enumerate() {
            println!("  Joint {}: {:.4} Nm", i + 1, tau);
        }
        println!();

        println!("✅ Example completed successfully!");
        println!("ℹ️  Note: The analytical implementation currently returns zero torques");
        println!("   because the k crate does not provide inverse dynamics capabilities.");
        println!("   For actual gravity compensation, use the 'mujoco' feature instead.");

        Ok(())
    }

    #[cfg(not(feature = "kinematics"))]
    {
        println!("⚠️  This example requires the 'kinematics' feature to be enabled.");
        println!("\n   Please run with:");
        println!("   cargo run --example gravity_compensation_analytical --features kinematics");

        Ok(())
    }
}
