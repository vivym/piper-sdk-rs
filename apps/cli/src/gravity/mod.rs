pub mod artifact;
pub mod compensation;
pub mod eval;
pub mod fit;
pub mod model;
pub mod record_path;
pub mod replay_sample;

#[allow(dead_code)]
pub const TORQUE_CONVENTION: &str = "piper-sdk-normalized-nm-v1";
#[allow(dead_code)]
pub const MODEL_KIND: &str = "joint-space-quasi-static-torque";
pub const BASIS_TRIG_V1: &str = "trig-v1";
