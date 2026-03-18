# piper-physics

Physics calculations for Piper robot including gravity compensation and inverse dynamics

**Version**: 0.0.3
**Date**: 2025-01-29

---

## Overview

`piper-physics` provides accurate gravity compensation and inverse dynamics calculations for the Piper robot arm using the MuJoCo physics engine.

### Key Features

- ✅ **Production-Ready**: Validated on real robot hardware
- ✅ **High Performance**: < 100μs per calculation
- ✅ **Three Computation Modes**: Pure gravity, partial inverse dynamics, full inverse dynamics
- ✅ **Dynamic Payload Support**: Adjust payload mass at runtime
- ✅ **Type-Safe**: Leverages `nalgebra` for vector/matrix operations
- 🎉 **Auto-Configuration**: MuJoCo is automatically downloaded and configured on first build

---

## 🚀 Quick Start

### Zero-Configuration Installation

`piper-physics` will **automatically** download and configure MuJoCo on the first build.

```bash
# Add dependency
cargo add piper-physics

# Build (MuJoCo is auto-downloaded on first build)
cargo build

# Run your application - no manual configuration needed!
cargo run --example gravity_compensation_mujoco
```

That's it! **No manual environment variables or setup scripts needed** - the executable automatically finds MuJoCo through embedded RPATH (Linux/macOS) or DLL copying (Windows).

---

## Installation

### Automatic Zero-Configuration

The first time you build, `piper-physics` will:

1. **Download** MuJoCo from official GitHub releases
2. **Install** to a standard location:
   - **Linux**: `~/.local/lib/mujoco/`
   - **macOS**: `~/Library/Frameworks/mujoco.framework/`
   - **Windows**: `%LOCALAPPDATA%\mujoco\`
3. **Embed RPATH** (Linux/macOS) or **copy DLLs** (Windows) for zero-configuration

#### What Gets Downloaded

| Platform | File Format | Size |
|----------|-------------|------|
| Linux | tar.gz | ~150 MB |
| macOS | DMG | ~250 MB |
| Windows | ZIP | ~150 MB |

#### How Zero-Configuration Works

- **Linux/macOS**: Library path is embedded in the executable via RPATH - no environment variables needed
- **Windows**: DLL is copied to target directories and project root - no PATH configuration needed

Your application just works after `cargo build`!

### Manual Configuration (Optional)

If you prefer to use a custom MuJoCo installation, set the `MUJOCO_DYNAMIC_LINK_DIR` environment variable:

```bash
export MUJOCO_DYNAMIC_LINK_DIR=/path/to/mujoco/lib
cargo build
```

This skips auto-download and uses your specified library path. RPATH will still be embedded automatically.

---

## Usage Examples

### Mode 1: Pure Gravity Compensation

Ideal for static holding, zero-force teaching, and slow trajectories (< 0.5 rad/s).

```rust
use piper_physics::{MujocoGravityCompensation, GravityCompensation};

let mut gravity_calc = MujocoGravityCompensation::from_embedded()?;

// Zero position
let q = nalgebra::Vector6::zeros();
let torques = gravity_calc.compute_gravity_compensation(&q)?;
```

### Mode 2: Partial Inverse Dynamics

Adds Coriolis, centrifugal forces, and damping. Ideal for medium-speed trajectories (0.5-2 rad/s).

```rust
use piper_physics::{MujocoGravityCompensation, GravityCompensation};

let mut gravity_calc = MujocoGravityCompensation::from_embedded()?;

// Position and velocity
let q = nalgebra::Vector6::zeros();
let qvel = [0.5, 0.5, 0.5, 0.5, 0.5, 0.5];  // rad/s

let torques = gravity_calc.compute_partial_inverse_dynamics(&q, &qvel)?;
```

### Mode 3: Full Inverse Dynamics

Includes inertial forces. Ideal for fast trajectories (> 2 rad/s) and force control.

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

## Physics Modes Explained

### Three Dynamics Compensation Modes

| Mode | Formula | Use Cases | API |
|------|--------|-----------|-----|
| **Pure Gravity Compensation** | τ = M(q)·g | Static holding, zero-force teaching | `compute_gravity_compensation()` |
| **Partial Inverse Dynamics** | τ = M·g + C(q,q̇) + F_damping | Medium-speed tracking (0.5-2 rad/s) | `compute_partial_inverse_dynamics()` |
| **Full Inverse Dynamics** | τ = M·g + C(q,q̇) + M(q)·q̈ | Fast trajectory, force control | `compute_inverse_dynamics()` |

### Numerical Differences

Example: Robot at horizontal position with joint 2 moving at 2 rad/s:

| Mode | Torque | Components | Missing |
|------|--------|------------|---------|
| **Pure gravity** | 5.0 Nm | Gravity: 5.0 | Coriolis, centrifugal, inertia |
| **Partial ID** | 7.8 Nm | Gravity: 5.0<br>Coriolis: 1.5<br>Centrifugal: 0.8<br>Damping: 0.5 | Inertia (28% gap) |
| **Full ID** | 10.6 Nm | All above + Inertia: 2.8 | None (100%) |

**Key insight**: Fast motion without inertial compensation undercompensates by **53%**!

---

## Dynamic Payload Compensation

MuJoCo implementation supports runtime payload adjustment:

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

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
piper-physics = "0.0.3"
```

MuJoCo will be automatically downloaded and configured on the first build (see Quick Start above).

---

## Examples

```bash
# Run gravity compensation example
cargo run --example gravity_compensation_mujoco

# Run real robot control example
cargo run --example gravity_compensation_robot -- can0
```

---

## Technical Details

- **Physics Engine**: MuJoCo 3.3.7
- **Inverse Dynamics**: Recursive Newton-Euler Algorithm (RNEA)
- **Accuracy**: Validated against real robot hardware
- **Performance**: < 100μs per calculation (suitable for 200Hz+ control loops)

---

## Documentation

For detailed analysis and design decisions, see:

- **[docs/v0/mujoco_vs_k_decision_report.md](docs/v0/mujoco_vs_k_decision_report.md)** - Architecture decision: why MuJoCo is the default
- **[docs/v0/rnea_implementation_report.md](docs/v0/rnea_implementation_report.md)** - Analysis of implementing RNEA manually
- **[docs/v0/mit_mode_analysis_report.md](docs/v0/mit_mode_analysis_report.md)** - MIT mode support analysis

---

## License

MIT OR Apache-2.0
