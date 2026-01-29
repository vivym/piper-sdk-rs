# piper-physics

Physics calculations for Piper robot including gravity compensation and inverse dynamics

**Version**: 0.0.4
**Date**: 2025-01-29

---

## ⚠️ CRITICAL: MuJoCo Feature Currently Broken

**Status**: 🔴 The `mujoco` feature is **currently non-functional** due to breaking API changes in `mujoco-rs` 2.3.

**Impact**: All 45+ compilation errors when building with `--features mujoco`

**Recommended Action**: Use the `kinematics` feature instead (enabled by default)

**For Details**: See [Compilation Error Analysis](../../docs/v0/piper-physics/MUJOCO_COMPILATION_ERRORS_ANALYSIS.md)

**Workaround**:
- The `kinematics` feature provides the trait definitions and types
- Note: Analytical implementation returns zeros pending RNE algorithm implementation
- For production use, you may need to implement your own gravity compensation

---

## ⚠️ Important: MuJoCo Native Dependency (Currently Unavailable)

> **⚠️ NOTE**: The `mujoco` feature is currently broken. The installation instructions below are for reference only once the feature is fixed.

The `mujoco` feature requires MuJoCo native library installation.

### Installation

**macOS**:
```bash
brew install mujoco pkgconf
```

**Linux**:
```bash
sudo apt-get install libmujoco-dev
```

**Environment Variables**:
```bash
export MUJOCO_DIR=/path/to/mujoco
export LD_LIBRARY_PATH=$MUJOCO_DIR/lib:$LD_LIBRARY_PATH
```

### Current Status
- ❌ **Broken**: API incompatibilities with mujoco-rs 2.3
- ✅ **Alternative**: Use `kinematics` feature (default)
- 📋 **Tracking**: See [docs/v0/piper-physics/](../../docs/v0/piper-physics/) for details

---

## Features

### Three Dynamics Compensation Modes

This crate provides three modes with different levels of dynamic compensation:

| Mode | Formula | Use Cases | API |
|------|--------|-----------|-----|
| **Pure Gravity Compensation** | τ = M(q)·g | Static holding, zero-force teaching | `compute_gravity_compensation()` |
| **Partial Inverse Dynamics** | τ = M·g + C(q,q̇) + F_damping | Medium-speed tracking (0.5-2 rad/s) | `compute_partial_inverse_dynamics()` |
| **Full Inverse Dynamics** | τ = M·g + C(q,q̇) + M(q)·q̈ | Fast trajectory, force control | `compute_inverse_dynamics()` |

---

## Selection Guide

| Scenario | Recommended Mode | Reasoning |
|----------|-----------------|-----------|
| **Static pose holding** | Pure gravity | No motion → no Coriolis forces |
| **Zero-force teaching** | Pure gravity | Avoids damping sensation |
| **Slow trajectory (< 0.5 rad/s)** | Pure gravity | Coriolis forces negligible |
| **Medium trajectory (0.5-2 rad/s)** | Partial ID | Compensates Coriolis + damping |
| **High-precision tracking** | Partial ID | Auto-compensates joint damping |
| **Fast trajectory (> 2 rad/s)** | Full ID | Requires inertial forces |
| **Force control** | Full ID | Precise force application |
| **Impedance control** | Full ID | Requires inertial matrix |

---

## Quick Start

### Mode 1: Pure Gravity Compensation

```rust
use piper_physics::{MujocoGravityCompensation, GravityCompensation};

let mut gravity_calc = MujocoGravityCompensation::from_embedded()?;

// Zero position
let q = nalgebra::Vector6::zeros();
let torques = gravity_calc.compute_gravity_compensation(&q)?;
```

### Mode 2: Partial Inverse Dynamics

```rust
use piper_physics::{MujocoGravityCompensation, GravityCompensation};

let mut gravity_calc = MujocoGravityCompensation::from_embedded()?;

// Position and velocity
let q = nalgebra::Vector6::zeros();
let qvel = [0.5, 0.5, 0.5, 0.5, 0.5, 0.5];  // rad/s

let torques = gravity_calc.compute_partial_inverse_dynamics(&q, &qvel)?;
```

### Mode 3: Full Inverse Dynamics

```rust
use piper_physics::{MujocoGravityCompensation, GravityCompensation};

let mut gravity_calc = MujocoGravityCompensation::from_embedded()?;

// Position, velocity, and desired acceleration
let q = nalgebra::Vector6::zeros();
let qvel = [2.0, 2.0, 2.0, 2.0, 2.0, 2.0];     // rad/s
let qacc = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0];     // rad/s²

let torques = gravity_calc.compute_inverse_dynamics(&q, &qvel, &qacc)?;
```

---

## Payload Compensation

MuJoCo implementation supports dynamic payload adjustment:

```rust
// Empty load
let tau_empty = gravity_calc.compute_gravity_compensation(&q)?;

// With 500g object (center of mass at end-effector origin)
let tau_with_load = gravity_calc.compute_gravity_torques_with_payload(
    &q,
    0.5,  // 500g
    nalgebra::Vector3::new(0.0, 0.0, 0.0),
)?;

// With irregular object (center of mass offset)
let tau_irregular = gravity_calc.compute_gravity_torques_with_payload(
    &q,
    0.325,  // 325g
    nalgebra::Vector3::new(0.05, 0.02, 0.1),  // 5cm forward, 2cm right, 1cm up
)?;
```

---

## Numerical Differences

Assume robot at horizontal position with joint 2 moving at 2 rad/s:

| Mode | Torque | Components | Missing |
|------|--------|------------|---------|
| **Pure gravity** | 5.0 Nm | Gravity: 5.0 | Coriolis, centrifugal, inertia |
| **Partial ID** | 7.8 Nm | Gravity: 5.0<br>Coriolis: 1.5<br>Centrifugal: 0.8<br>Damping: 0.5 | Inertia (28% gap) |
| **Full ID** | 10.6 Nm | All above + Inertia: 2.8 | None (100%) |

**Key insight**: Fast motion without inertial compensation undercompensates by **53%**!

---

## Features

- ✅ **Accurate Physics**: MuJoCo's qfrc_bias for pure gravity, qfrc_inverse for full ID
- ✅ **Dynamic Payload**: Adjust payload mass at runtime via Jacobian transpose method
- ✅ **Type-Safe**: Leverages `nalgebra` for vector/matrix operations
- ✅ **Multiple Loading**: Embedded, model directory, or standard path
- ✅ **Joint Mapping Validation**: Prevents robot失控 by validating CAN ID order
- ✅ **Three Modes**: Choose appropriate compensation level for your use case

---

## Feature Flags

```toml
[dependencies]
piper-physics = { version = "0.0.4", features = ["kinematics"] }

# OR (if MuJoCo is installed)
piper-physics = { version = "0.0.4", features = ["mujoco"] }
```

**Features**:
- `kinematics` (default): Basic types and traits (no external deps)
- `mujoco`: MuJoCo-based physics simulation (requires native lib)

---

## Documentation

For detailed analysis of implementation differences and design decisions, see:

- **[GRAVITY_COMPARISON_ANALYSIS_REVISED.md](GRAVITY_COMPARISON_ANALYSIS_REVISED.md) - Technical comparison with reference implementation
- **[REVISION_NOTES_v2.md](REVISION_NOTES_v2.md) - Revision history and technical corrections

---

## License

MIT OR Apache-2.0
