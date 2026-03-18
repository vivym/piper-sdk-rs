//! Error types for physics calculations

use piper_sdk::client::types::RobotError;
use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur in physics calculations
#[derive(Debug, Error)]
pub enum PhysicsError {
    /// Calculation failed
    #[error("Calculation failed: {0}")]
    CalculationFailed(String),

    /// Chain not initialized
    #[error("Chain not initialized")]
    NotInitialized,

    /// Invalid input parameter
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// URDF parsing error
    #[error("URDF parse error (path: {path}): {error}")]
    UrdfParseError {
        /// File path that failed to parse
        path: PathBuf,
        /// Underlying error message
        error: String,
    },

    /// Joint mapping validation failed
    #[error("Joint mapping validation failed: {0}")]
    JointMappingError(String),

    /// MuJoCo model loading failed
    #[error("MuJoCo model loading failed: {0}")]
    ModelLoadError(String),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Errors that can occur while running real-robot gravity compensation
#[derive(Debug, Error)]
pub enum GravityCompensationRunnerError {
    /// Physics calculation failed
    #[error("Physics calculation failed: {0}")]
    Physics(#[from] PhysicsError),

    /// Robot read/write failed
    #[error("Robot control failed: {0}")]
    Robot(#[from] RobotError),
}
