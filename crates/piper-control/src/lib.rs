//! High-level workflow helpers for Piper control applications.

mod profile;
mod target;
mod workflow;

pub use profile::{ControlProfile, DEFAULT_PARK_SPEED_PERCENT, MotionWaitConfig, ParkOrientation};
pub use target::{TargetSpec, client_builder_for_target, driver_builder_for_target};
pub use workflow::{
    MotionExecutionOutcome, PreparedMove, active_move_to_joint_target_blocking,
    active_move_to_joint_target_with_cancel, home_zero_blocking, move_to_joint_target_blocking,
    park_blocking, prepare_move, query_collision_protection_blocking,
    set_collision_protection_verified, set_joint_zero_blocking,
};
