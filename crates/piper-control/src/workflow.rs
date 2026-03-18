use crate::{ControlProfile, MotionWaitConfig};
use anyhow::{Result, bail};
use piper_client::Observer;
use piper_client::observer::{CollisionProtectionSnapshot, ControlReadPolicy};
use piper_client::state::{Active, DisableConfig, Piper, PositionMode, Standby};
use piper_client::types::RobotError;
use piper_client::types::{JointArray, Rad};
use piper_tools::SafetyConfig;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedMove {
    pub current: [f64; 6],
    pub effective_target: [f64; 6],
    pub max_delta_rad: f64,
    pub max_delta_deg: f64,
    pub requires_confirmation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionExecutionOutcome {
    Reached,
    Cancelled,
}

pub fn prepare_move(
    current: [f64; 6],
    requested_joints: &[f64],
    safety: &SafetyConfig,
    force: bool,
) -> Result<PreparedMove> {
    if requested_joints.is_empty() {
        bail!("at least one joint value is required");
    }
    if requested_joints.len() > 6 {
        bail!("at most six joint values are supported");
    }

    let mut effective_target = current;
    for (index, position) in requested_joints.iter().copied().enumerate() {
        if !position.is_finite() {
            bail!("joint J{} target is not finite", index + 1);
        }
        if !safety.check_joint_position(index, position) {
            bail!(
                "joint J{} target {:.3} rad exceeds configured limits",
                index + 1,
                position
            );
        }
        effective_target[index] = position;
    }

    let max_delta_rad = effective_target
        .iter()
        .zip(current.iter())
        .map(|(target, current)| (target - current).abs())
        .fold(0.0_f64, f64::max);
    let max_delta_deg = max_delta_rad.to_degrees();

    Ok(PreparedMove {
        current,
        effective_target,
        max_delta_rad,
        max_delta_deg,
        requires_confirmation: !force
            && safety.confirmation.enabled
            && safety.requires_confirmation(max_delta_deg),
    })
}

pub fn active_move_to_joint_target_blocking(
    robot: &Piper<Active<PositionMode>>,
    target: [f64; 6],
    wait: &MotionWaitConfig,
) -> Result<()> {
    match active_move_to_joint_target_with_cancel(robot, target, wait, || false)? {
        MotionExecutionOutcome::Reached => Ok(()),
        MotionExecutionOutcome::Cancelled => bail!("motion was cancelled before completion"),
    }
}

pub fn active_move_to_joint_target_with_cancel<ShouldCancel>(
    robot: &Piper<Active<PositionMode>>,
    target: [f64; 6],
    wait: &MotionWaitConfig,
    should_cancel: ShouldCancel,
) -> Result<MotionExecutionOutcome>
where
    ShouldCancel: Fn() -> bool,
{
    let target_positions = joint_array_from_f64(target);
    blocking_motion_loop_with_cancel(
        target,
        wait,
        || observer_positions(robot.observer()).map_err(Into::into),
        || robot.send_position_command(&target_positions).map_err(Into::into),
        should_cancel,
    )
}

pub fn move_to_joint_target_blocking(
    standby: Piper<Standby>,
    profile: &ControlProfile,
    target: [f64; 6],
) -> Result<Piper<Standby>> {
    let active = standby.enable_position_mode(profile.position_mode_config())?;
    active_move_to_joint_target_blocking(&active, target, &profile.wait)?;
    active.disable(DisableConfig::default()).map_err(Into::into)
}

pub fn home_zero_blocking(
    standby: Piper<Standby>,
    profile: &ControlProfile,
) -> Result<Piper<Standby>> {
    move_to_joint_target_blocking(standby, profile, [0.0; 6])
}

pub fn park_blocking(standby: Piper<Standby>, profile: &ControlProfile) -> Result<Piper<Standby>> {
    move_to_joint_target_blocking(standby, profile, profile.park_pose())
}

pub fn set_joint_zero_blocking(standby: &Piper<Standby>, joints: &[usize]) -> Result<()> {
    standby.set_joint_zero_positions(joints).map_err(Into::into)
}

pub fn query_collision_protection_blocking(
    standby: &Piper<Standby>,
    wait: &MotionWaitConfig,
) -> Result<[u8; 6]> {
    standby.query_collision_protection(wait.timeout).map_err(Into::into)
}

pub fn set_collision_protection_verified(
    standby: &Piper<Standby>,
    levels: [u8; 6],
    wait: &MotionWaitConfig,
) -> Result<()> {
    for (index, level) in levels.iter().enumerate() {
        if *level > 8 {
            bail!(
                "joint J{} collision protection level {} exceeds 8",
                index + 1,
                level
            );
        }
    }

    let baseline = standby.collision_protection_cached().ok();
    let baseline_hw = baseline.as_ref().map_or(0, |state| state.hardware_timestamp_us);
    let baseline_sys = baseline.as_ref().map_or(0, |state| state.system_timestamp_us);

    verify_collision_protection_after_write(
        baseline_hw,
        baseline_sys,
        levels,
        wait,
        || standby.collision_protection_cached().map_err(Into::into),
        |timeout| standby.query_collision_protection(timeout).map_err(Into::into),
        || standby.set_collision_protection(levels).map_err(Into::into),
    )
}

fn observer_positions(observer: &Observer) -> std::result::Result<[f64; 6], RobotError> {
    let snapshot = observer.control_snapshot(ControlReadPolicy::default())?;
    Ok(std::array::from_fn(|index| snapshot.position[index].0))
}

fn joint_array_from_f64(values: [f64; 6]) -> JointArray<Rad> {
    JointArray::from(values.map(Rad))
}

fn blocking_motion_loop_with_cancel<ReadCurrent, Publish, ShouldCancel>(
    target: [f64; 6],
    wait: &MotionWaitConfig,
    mut read_current: ReadCurrent,
    mut publish: Publish,
    should_cancel: ShouldCancel,
) -> Result<MotionExecutionOutcome>
where
    ReadCurrent: FnMut() -> Result<[f64; 6]>,
    Publish: FnMut() -> Result<()>,
    ShouldCancel: Fn() -> bool,
{
    if should_cancel() {
        return Ok(MotionExecutionOutcome::Cancelled);
    }
    publish()?;
    let start = Instant::now();
    let mut last_publish = Instant::now();

    loop {
        if should_cancel() {
            return Ok(MotionExecutionOutcome::Cancelled);
        }

        let current = read_current()?;
        let max_error = current
            .iter()
            .zip(target.iter())
            .map(|(current, target)| (target - current).abs())
            .fold(0.0_f64, f64::max);

        if max_error <= wait.threshold_rad {
            return Ok(MotionExecutionOutcome::Reached);
        }

        let now = Instant::now();
        if now.duration_since(start) >= wait.timeout {
            bail!(
                "motion did not reach target within {:.2}s (remaining max error {:.4} rad)",
                wait.timeout.as_secs_f64(),
                max_error
            );
        }

        if now.duration_since(last_publish) >= wait.republish_interval {
            if should_cancel() {
                return Ok(MotionExecutionOutcome::Cancelled);
            }
            publish()?;
            last_publish = now;
        }

        std::thread::sleep(wait.poll_interval);
    }
}

fn verify_collision_protection_after_write<ReadCached, Query, Publish>(
    baseline_hw: u64,
    baseline_sys: u64,
    expected: [u8; 6],
    wait: &MotionWaitConfig,
    mut read_cached: ReadCached,
    mut request_query: Query,
    mut publish: Publish,
) -> Result<()>
where
    ReadCached: FnMut() -> Result<CollisionProtectionSnapshot>,
    Query: FnMut(std::time::Duration) -> Result<[u8; 6]>,
    Publish: FnMut() -> Result<()>,
{
    publish()?;
    let start = Instant::now();
    let mut last_query_at: Option<Instant> = None;

    loop {
        let cached = read_cached()?;
        if cached.is_newer_than(baseline_hw, baseline_sys) && cached.levels == expected {
            return Ok(());
        }

        let now = Instant::now();
        if now.duration_since(start) >= wait.timeout {
            bail!(
                "collision protection verification timed out after {:.2}s",
                wait.timeout.as_secs_f64()
            );
        }

        let should_query = last_query_at
            .map(|last_query| now.duration_since(last_query) >= wait.republish_interval)
            .unwrap_or(true);
        if should_query {
            let remaining = wait.timeout.saturating_sub(now.duration_since(start));
            let query_timeout = wait.poll_interval.min(remaining);
            if query_timeout.is_zero() {
                bail!(
                    "collision protection verification timed out after {:.2}s",
                    wait.timeout.as_secs_f64()
                );
            }
            match request_query(query_timeout) {
                Ok(_) => {},
                Err(error)
                    if matches!(
                        error.downcast_ref::<RobotError>(),
                        Some(RobotError::Timeout { .. })
                    ) => {},
                Err(error) => return Err(error),
            }
            last_query_at = Some(Instant::now());
            continue;
        }

        std::thread::sleep(wait.poll_interval.min(wait.timeout.saturating_sub(start.elapsed())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_tools::SafetyConfig;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn prepare_move_uses_delta_from_current_state() {
        let current = [2.5, 0.0, 0.0, 0.0, 0.0, 0.0];
        let prepared =
            prepare_move(current, &[0.1], &SafetyConfig::default_config(), false).unwrap();
        assert!(prepared.requires_confirmation);
        assert!(prepared.max_delta_deg > 100.0);
    }

    #[test]
    fn blocking_motion_loop_republishes_until_target_is_reached() {
        let state = Arc::new(Mutex::new((0.0_f64, 0_usize)));
        let target = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let wait = MotionWaitConfig {
            threshold_rad: 0.01,
            poll_interval: Duration::from_millis(1),
            republish_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(50),
        };

        let read_state = {
            let state = Arc::clone(&state);
            move || {
                let mut guard = state.lock().unwrap();
                if guard.0 < 1.0 {
                    guard.0 += 0.25;
                }
                Ok([guard.0, 0.0, 0.0, 0.0, 0.0, 0.0])
            }
        };
        let publish = {
            let state = Arc::clone(&state);
            move || {
                state.lock().unwrap().1 += 1;
                Ok(())
            }
        };

        let outcome =
            blocking_motion_loop_with_cancel(target, &wait, read_state, publish, || false).unwrap();

        assert_eq!(outcome, MotionExecutionOutcome::Reached);
        let (_, publishes) = *state.lock().unwrap();
        assert!(publishes >= 1);
    }

    #[test]
    fn blocking_motion_loop_returns_cancelled_when_requested() {
        let wait = MotionWaitConfig {
            threshold_rad: 0.01,
            poll_interval: Duration::from_millis(1),
            republish_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(50),
        };

        let outcome = blocking_motion_loop_with_cancel(
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            &wait,
            || Ok([0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            || Ok(()),
            || true,
        )
        .unwrap();

        assert_eq!(outcome, MotionExecutionOutcome::Cancelled);
    }

    #[test]
    fn verify_collision_protection_after_write_accepts_post_write_cached_match() {
        let wait = MotionWaitConfig {
            threshold_rad: 0.02,
            poll_interval: Duration::from_millis(1),
            republish_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(20),
        };
        let current = Arc::new(Mutex::new(CollisionProtectionSnapshot {
            hardware_timestamp_us: 10,
            system_timestamp_us: 10,
            levels: [0; 6],
        }));
        let query_attempts = Arc::new(Mutex::new(0usize));

        verify_collision_protection_after_write(
            10,
            10,
            [4_u8; 6],
            &wait,
            {
                let current = Arc::clone(&current);
                move || Ok(*current.lock().unwrap())
            },
            {
                let current = Arc::clone(&current);
                let query_attempts = Arc::clone(&query_attempts);
                move |_| {
                    let mut attempts = query_attempts.lock().unwrap();
                    *attempts += 1;
                    if *attempts == 1 {
                        *current.lock().unwrap() = CollisionProtectionSnapshot {
                            hardware_timestamp_us: 11,
                            system_timestamp_us: 11,
                            levels: [4_u8; 6],
                        };
                        Err(RobotError::Timeout { timeout_ms: 1 }.into())
                    } else {
                        Ok([4_u8; 6])
                    }
                }
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(*query_attempts.lock().unwrap(), 1);
    }

    #[test]
    fn verify_collision_protection_after_write_rejects_stale_matching_cache() {
        let wait = MotionWaitConfig {
            threshold_rad: 0.02,
            poll_interval: Duration::from_millis(1),
            republish_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(20),
        };
        let reads = Arc::new(Mutex::new(0usize));

        verify_collision_protection_after_write(
            20,
            20,
            [5_u8; 6],
            &wait,
            {
                let reads = Arc::clone(&reads);
                move || {
                    let mut read_count = reads.lock().unwrap();
                    *read_count += 1;
                    if *read_count < 3 {
                        Ok(CollisionProtectionSnapshot {
                            hardware_timestamp_us: 20,
                            system_timestamp_us: 20,
                            levels: [5_u8; 6],
                        })
                    } else {
                        Ok(CollisionProtectionSnapshot {
                            hardware_timestamp_us: 21,
                            system_timestamp_us: 21,
                            levels: [5_u8; 6],
                        })
                    }
                }
            },
            |_| Err(RobotError::Timeout { timeout_ms: 1 }.into()),
            || Ok(()),
        )
        .unwrap();

        assert!(*reads.lock().unwrap() >= 3);
    }
}
