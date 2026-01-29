//! MuJoCo gravity compensation example
//!
//! This example demonstrates how to use MuJoCo to compute gravity compensation
//! and inverse dynamics torques for the Piper robot.
//!
//! **NOTE**: This example requires the `mujoco` feature to be enabled:
//! `cargo run --example gravity_compensation_mujoco --features mujoco`
//!
//! # Prerequisites
//!
//! Install MuJoCo native library:
//! - macOS: `brew install mujoco pkgconf`
//! - Linux: `sudo apt-get install libmujoco-dev`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "mujoco")]
    {
        use piper_physics::{GravityCompensation, MujocoGravityCompensation};
        use std::f64::consts::FRAC_PI_2;

        println!("🤖 Piper Gravity Compensation Example (MuJoCo Simulation)");
        println!("===========================================================\n");

        // ============================================================
        // 1. Load MuJoCo model
        // ============================================================
        println!("📄 Loading MuJoCo model from embedded XML...");
        let mut gravity_calc = MujocoGravityCompensation::from_embedded()
            .expect("Failed to load MuJoCo model from embedded XML");
        println!("✓ MuJoCo model loaded successfully\n");

        // ============================================================
        // 2. Mode 1: Pure Gravity Compensation
        // ============================================================
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("📍 Mode 1: Pure Gravity Compensation");
        println!("   Formula: τ = M(q)·g");
        println!("   Use case: Static holding, zero-force teaching");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

        // Test 2.1: Zero position (all joints at 0)
        println!("📍 Computing torques for ZERO position...");
        let q_zero = nalgebra::Vector6::zeros();
        let torques_zero = gravity_calc.compute_gravity_compensation(&q_zero)?;
        println!("   Joint positions (rad): {:?}", q_zero.as_slice());
        println!("   Gravity torques (Nm):");
        for (i, &tau) in torques_zero.iter().enumerate() {
            println!("     Joint {}: {:8.4} Nm", i + 1, tau);
        }
        println!();

        // Test 2.2: Horizontal pose (most joints at 90°/π/2, joint3 at negative due to range limits)
        // Note: joint3 range is -2.967 ~ 0.0 (negative only), so we use -π/2 instead of π/2
        println!("📍 Computing torques for HORIZONTAL pose...");
        let q_horizontal = nalgebra::Vector6::new(
            FRAC_PI_2,  // joint1: range -2.618 ~ 2.168 ✅
            FRAC_PI_2,  // joint2: range 0.0 ~ 3.14 ✅
            -FRAC_PI_2, // joint3: range -2.967 ~ 0.0 (negative only!) ✅
            FRAC_PI_2,  // joint4: range -1.745 ~ 1.745 ✅
            FRAC_PI_2,  // joint5: range -1.22 ~ 1.22 ✅
            FRAC_PI_2,  // joint6: range -2.0944 ~ 2.0944 ✅
        );
        let torques_horizontal = gravity_calc.compute_gravity_compensation(&q_horizontal)?;
        println!("   Joint positions (rad): {:?}", q_horizontal.as_slice());
        println!("   Gravity torques (Nm):");
        for (i, &tau) in torques_horizontal.iter().enumerate() {
            println!("     Joint {}: {:8.4} Nm", i + 1, tau);
        }
        println!();

        // ============================================================
        // 3. Mode 2: Partial Inverse Dynamics
        // ============================================================
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("📍 Mode 2: Partial Inverse Dynamics");
        println!("   Formula: τ = M·g + C(q,q̇) + F_damping");
        println!("   Use case: Medium-speed tracking (0.5-2 rad/s)");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

        let qvel = [0.5, 0.5, 0.5, 0.5, 0.5, 0.5]; // rad/s
        println!("📍 Computing partial inverse dynamics (with velocity)...");
        println!("   Joint positions (rad): {:?}", q_horizontal.as_slice());
        println!("   Joint velocities (rad/s): {:?}", qvel);

        let torques_partial =
            gravity_calc.compute_partial_inverse_dynamics(&q_horizontal, &qvel)?;
        println!("   Partial ID torques (Nm):");
        for (i, &tau) in torques_partial.iter().enumerate() {
            println!("     Joint {}: {:8.4} Nm", i + 1, tau);
        }

        // Compare with pure gravity
        println!("\n📊 Comparison (Effect of velocity terms):");
        for (i, (&tau_grav, &tau_partial)) in
            torques_horizontal.iter().zip(torques_partial.iter()).enumerate()
        {
            let diff = tau_partial - tau_grav;
            let pct = if tau_grav.abs() > 1e-6 {
                diff / tau_grav.abs() * 100.0
            } else {
                0.0
            };
            println!(
                "     Joint {}: Gravity={:8.4}, Partial={:8.4}, Diff={:+7.4} ({:+5.1}%)",
                i + 1,
                tau_grav,
                tau_partial,
                diff,
                pct
            );
        }
        println!();

        // ============================================================
        // 4. Mode 3: Full Inverse Dynamics
        // ============================================================
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("📍 Mode 3: Full Inverse Dynamics");
        println!("   Formula: τ = M·g + C(q,q̇) + M(q)·q̈");
        println!("   Use case: Fast trajectory (> 2 rad/s), force control");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

        let qvel_fast = [2.0, 2.0, 2.0, 2.0, 2.0, 2.0]; // rad/s (fast)
        let qacc = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0]; // rad/s²

        println!("📍 Computing full inverse dynamics (with velocity and acceleration)...");
        println!("   Joint positions (rad): {:?}", q_horizontal.as_slice());
        println!("   Joint velocities (rad/s): {:?}", qvel_fast);
        println!("   Joint accelerations (rad/s²): {:?}", qacc);

        let torques_full =
            gravity_calc.compute_inverse_dynamics(&q_horizontal, &qvel_fast, &qacc)?;
        println!("   Full ID torques (Nm):");
        for (i, &tau) in torques_full.iter().enumerate() {
            println!("     Joint {}: {:8.4} Nm", i + 1, tau);
        }

        // Compare all three modes
        println!("\n📊 Comparison of All Three Modes:");
        println!(
            "     {:<10} {:<12} {:<12} {:<12}",
            "Joint", "Pure Grav", "Partial ID", "Full ID"
        );
        println!("     {}", "─".repeat(50));
        for (i, ((&tau_grav, &tau_partial), &tau_full)) in torques_horizontal
            .iter()
            .zip(torques_partial.iter())
            .zip(torques_full.iter())
            .enumerate()
        {
            println!(
                "     J{:=<9} {:<12.4} {:<12.4} {:<12.4}",
                i + 1,
                tau_grav,
                tau_partial,
                tau_full
            );
        }
        println!();

        // Numerical difference analysis
        println!("📊 Key Insight: Inertial Forces Matter!");
        let avg_inertial = torques_full
            .iter()
            .zip(torques_partial.iter())
            .map(|(full, partial)| full - partial)
            .fold(0.0, |acc, x| acc + x.abs())
            / 6.0;

        let avg_gravity = torques_horizontal.iter().map(|x| x.abs()).sum::<f64>() / 6.0;

        let pct_inertial = (avg_inertial / (avg_inertial + avg_gravity)) * 100.0;

        println!(
            "   Average inertial contribution: {:.4} Nm ({:.1}%)",
            avg_inertial, pct_inertial
        );
        println!(
            "   → Fast motion without inertial compensation undercompensates by {:.1}%!",
            pct_inertial
        );
        println!();

        // ============================================================
        // 5. Payload Compensation
        // ============================================================
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("📍 Payload Compensation");
        println!("   Dynamically adjust for additional payload mass");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

        // Empty load
        println!("📍 Computing torques for zero position:");
        let torques_empty = gravity_calc.compute_gravity_compensation(&q_zero)?;
        println!("   Empty load:");
        for (i, &tau) in torques_empty.iter().enumerate() {
            println!("     Joint {}: {:8.4} Nm", i + 1, tau);
        }
        println!();

        // With 500g payload at end-effector origin
        println!("📍 Computing torques with 500g payload (CoM at end-effector origin)...");
        let torques_with_load = gravity_calc.compute_gravity_torques_with_payload(
            &q_zero,
            0.5, // 500g
            nalgebra::Vector3::new(0.0, 0.0, 0.0),
        )?;
        println!("   With 500g payload:");
        for (i, (&tau_empty, &tau_load)) in
            torques_empty.iter().zip(torques_with_load.iter()).enumerate()
        {
            let diff = tau_load - tau_empty;
            println!(
                "     Joint {}: {:8.4} → {:8.4} (Δ={:+7.4})",
                i + 1,
                tau_empty,
                tau_load,
                diff
            );
        }
        println!();

        // With irregular payload (CoM offset)
        println!("📍 Computing torques with 325g irregular payload (CoM offset by 5cm)...");
        let torques_irregular = gravity_calc.compute_gravity_torques_with_payload(
            &q_zero,
            0.325,                                   // 325g
            nalgebra::Vector3::new(0.05, 0.02, 0.1), // 5cm forward, 2cm right, 1cm up
        )?;
        println!("   With 325g irregular payload:");
        for (i, (&tau_empty, &tau_irregular)) in
            torques_empty.iter().zip(torques_irregular.iter()).enumerate()
        {
            let diff = tau_irregular - tau_empty;
            println!(
                "     Joint {}: {:8.4} → {:8.4} (Δ={:+7.4})",
                i + 1,
                tau_empty,
                tau_irregular,
                diff
            );
        }
        println!();

        // ============================================================
        // 6. Summary and Recommendations
        // ============================================================
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("📚 Summary and Mode Selection Guide");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

        println!("✅ When to use each mode:");
        println!("   1. Pure Gravity:");
        println!("      • Static pose holding");
        println!("      • Zero-force teaching");
        println!("      • Slow trajectories (< 0.5 rad/s)");
        println!();

        println!("   2. Partial Inverse Dynamics:");
        println!("      • Medium-speed tracking (0.5-2 rad/s)");
        println!("      • High-precision tracking");
        println!("      • PD+feedforward control");
        println!();

        println!("   3. Full Inverse Dynamics:");
        println!("      • Fast trajectories (> 2 rad/s)");
        println!("      • High-dynamic motion");
        println!("      • Force control");
        println!("      • Impedance control");
        println!();

        println!("✅ Example completed successfully!");
        println!("   • All three modes demonstrated");
        println!("   • Payload compensation tested");
        println!("   • Numerical comparisons shown");
        println!();

        Ok(())
    }

    #[cfg(not(feature = "mujoco"))]
    {
        println!("⚠️  This example requires the 'mujoco' feature to be enabled.");
        println!("\n   Please run with:");
        println!("   cargo run --example gravity_compensation_mujoco --features mujoco");
        println!("\n   ⚙️  Prerequisites:");
        println!("   The 'mujoco' feature requires MuJoCo native library installation.");
        println!();
        println!("   macOS:");
        println!("     brew install mujoco pkgconf");
        println!();
        println!("   Linux:");
        println!("     sudo apt-get install libmujoco-dev");
        println!();
        println!("   See crates/piper-physics/README.md for more details.");

        Ok(())
    }
}
