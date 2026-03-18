//! Core type definitions for physics calculations

use nalgebra::*;

/// Joint state vector (6-DOF manipulator)
pub type JointState = Vector6<f64>;

/// Joint torques vector
pub type JointTorques = Vector6<f64>;

/// Gravity vector (3D)
pub type GravityVector = Vector3<f64>;

/// Jacobian matrix (3x6 for 6-DOF manipulator)
pub type Jacobian3x6 = Matrix3x6<f64>;

/// Time step (seconds)
pub type TimeStep = f64;
