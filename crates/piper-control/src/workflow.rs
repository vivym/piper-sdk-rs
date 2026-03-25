use crate::{ControlProfile, MotionWaitConfig};
use anyhow::{Result, bail};
use piper_client::Observer;
use piper_client::observer::MonitorReadPolicy;
use piper_client::state::{Active, DisableConfig, MotionCapability, Piper, PositionMode, Standby};
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

pub fn active_move_to_joint_target_blocking<Capability>(
    robot: &Piper<Active<PositionMode>, Capability>,
    target: [f64; 6],
    wait: &MotionWaitConfig,
) -> Result<()>
where
    Capability: MotionCapability,
{
    match active_move_to_joint_target_with_cancel(robot, target, wait, || false)? {
        MotionExecutionOutcome::Reached => Ok(()),
        MotionExecutionOutcome::Cancelled => bail!("motion was cancelled before completion"),
    }
}

pub fn active_move_to_joint_target_with_cancel<Capability, ShouldCancel>(
    robot: &Piper<Active<PositionMode>, Capability>,
    target: [f64; 6],
    wait: &MotionWaitConfig,
    should_cancel: ShouldCancel,
) -> Result<MotionExecutionOutcome>
where
    Capability: MotionCapability,
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

pub fn move_to_joint_target_blocking<Capability>(
    standby: Piper<Standby, Capability>,
    profile: &ControlProfile,
    target: [f64; 6],
) -> Result<Piper<Standby, Capability>>
where
    Capability: MotionCapability,
{
    let active = standby.enable_position_mode(profile.position_mode_config())?;
    active_move_to_joint_target_blocking(&active, target, &profile.wait)?;
    active.disable(DisableConfig::default()).map_err(Into::into)
}

pub fn home_zero_blocking<Capability>(
    standby: Piper<Standby, Capability>,
    profile: &ControlProfile,
) -> Result<Piper<Standby, Capability>>
where
    Capability: MotionCapability,
{
    move_to_joint_target_blocking(standby, profile, [0.0; 6])
}

pub fn park_blocking<Capability>(
    standby: Piper<Standby, Capability>,
    profile: &ControlProfile,
) -> Result<Piper<Standby, Capability>>
where
    Capability: MotionCapability,
{
    move_to_joint_target_blocking(standby, profile, profile.park_pose())
}

pub fn set_joint_zero_blocking<Capability>(
    standby: &Piper<Standby, Capability>,
    joints: &[usize],
) -> Result<()>
where
    Capability: MotionCapability,
{
    standby.set_joint_zero_positions(joints).map_err(Into::into)
}

pub fn query_collision_protection_blocking<Capability>(
    standby: &Piper<Standby, Capability>,
    wait: &MotionWaitConfig,
) -> Result<[u8; 6]>
where
    Capability: MotionCapability,
{
    standby.query_collision_protection(wait.timeout).map_err(Into::into)
}

pub fn set_collision_protection_verified<Capability>(
    standby: &Piper<Standby, Capability>,
    levels: [u8; 6],
    wait: &MotionWaitConfig,
) -> Result<()>
where
    Capability: MotionCapability,
{
    for (index, level) in levels.iter().enumerate() {
        if *level > 8 {
            bail!(
                "joint J{} collision protection level {} exceeds 8",
                index + 1,
                level
            );
        }
    }

    verify_collision_protection_after_write(
        levels,
        wait,
        |timeout| standby.query_collision_protection(timeout).map_err(Into::into),
        || standby.set_collision_protection(levels).map_err(Into::into),
    )
}

fn observer_positions<Capability>(
    observer: &Observer<Capability>,
) -> std::result::Result<[f64; 6], RobotError>
where
    Capability: MotionCapability,
{
    let positions = observer.joint_positions_with_policy(MonitorReadPolicy::default())?;
    Ok(std::array::from_fn(|index| positions[index].0))
}

fn joint_array_from_f64(values: [f64; 6]) -> JointArray<Rad> {
    JointArray::from(values.map(Rad))
}

fn is_monitor_warmup_error(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<RobotError>(),
        Some(RobotError::MonitorStateIncomplete { .. } | RobotError::MonitorStateStale { .. })
    )
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
    let max_error = |current: [f64; 6]| {
        current
            .iter()
            .zip(target.iter())
            .map(|(current, target)| (target - current).abs())
            .fold(0.0_f64, f64::max)
    };
    let start = Instant::now();

    if should_cancel() {
        return Ok(MotionExecutionOutcome::Cancelled);
    }

    let initial_error = loop {
        match read_current() {
            Ok(current) => {
                if max_error(current) <= wait.threshold_rad {
                    return Ok(MotionExecutionOutcome::Reached);
                }
                break None;
            },
            Err(error) if is_monitor_warmup_error(&error) => {
                if start.elapsed() >= wait.timeout {
                    break Some(error);
                }

                let remaining = wait.timeout.saturating_sub(start.elapsed());
                let sleep_duration = wait.poll_interval.min(remaining);
                if sleep_duration.is_zero() {
                    break Some(error);
                }
                std::thread::sleep(sleep_duration);
            },
            Err(error) => break Some(error),
        }
    };

    if let Some(error) = initial_error {
        return Err(error);
    }

    publish()?;
    let mut last_publish = Instant::now();

    loop {
        if should_cancel() {
            return Ok(MotionExecutionOutcome::Cancelled);
        }

        let max_error = match read_current() {
            Ok(current) => max_error(current),
            Err(error) if is_monitor_warmup_error(&error) => {
                if start.elapsed() >= wait.timeout {
                    return Err(error);
                }

                let remaining = wait.timeout.saturating_sub(start.elapsed());
                let sleep_duration = wait.poll_interval.min(remaining);
                if sleep_duration.is_zero() {
                    return Err(error);
                }
                std::thread::sleep(sleep_duration);
                continue;
            },
            Err(error) => return Err(error),
        };

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

        let remaining = wait.timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            continue;
        }
        std::thread::sleep(wait.poll_interval.min(remaining));
    }
}

fn verify_collision_protection_after_write<Query, Publish>(
    expected: [u8; 6],
    wait: &MotionWaitConfig,
    mut request_query: Query,
    mut publish: Publish,
) -> Result<()>
where
    Query: FnMut(std::time::Duration) -> Result<[u8; 6]>,
    Publish: FnMut() -> Result<()>,
{
    publish()?;
    let start = Instant::now();
    let mut last_query_at: Option<Instant> = None;

    loop {
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
                Ok(levels) if levels == expected => return Ok(()),
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
    use piper_client::MonitorStateSource;
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
    fn blocking_motion_loop_skips_publish_when_target_is_already_reached() {
        let publishes = Arc::new(Mutex::new(0usize));
        let wait = MotionWaitConfig {
            threshold_rad: 0.01,
            poll_interval: Duration::from_millis(1),
            republish_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(50),
        };

        let outcome = blocking_motion_loop_with_cancel(
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            &wait,
            || Ok([1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            {
                let publishes = Arc::clone(&publishes);
                move || {
                    *publishes.lock().unwrap() += 1;
                    Ok(())
                }
            },
            || false,
        )
        .unwrap();

        assert_eq!(outcome, MotionExecutionOutcome::Reached);
        assert_eq!(*publishes.lock().unwrap(), 0);
    }

    #[test]
    fn blocking_motion_loop_retries_monitor_warmup_errors_before_deciding_target_is_reached() {
        let publishes = Arc::new(Mutex::new(0usize));
        let attempts = Arc::new(Mutex::new(0usize));
        let wait = MotionWaitConfig {
            threshold_rad: 0.01,
            poll_interval: Duration::from_millis(1),
            republish_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(50),
        };

        let outcome = blocking_motion_loop_with_cancel(
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            &wait,
            {
                let attempts = Arc::clone(&attempts);
                move || {
                    let mut attempts = attempts.lock().unwrap();
                    *attempts += 1;
                    if *attempts < 3 {
                        Err(RobotError::monitor_state_incomplete(
                            MonitorStateSource::JointPosition,
                            0b001,
                            0b111,
                        )
                        .into())
                    } else {
                        Ok([1.0, 0.0, 0.0, 0.0, 0.0, 0.0])
                    }
                }
            },
            {
                let publishes = Arc::clone(&publishes);
                move || {
                    *publishes.lock().unwrap() += 1;
                    Ok(())
                }
            },
            || false,
        )
        .expect("warmup errors should be retried until a valid snapshot arrives");

        assert_eq!(outcome, MotionExecutionOutcome::Reached);
        assert_eq!(*attempts.lock().unwrap(), 3);
        assert_eq!(
            *publishes.lock().unwrap(),
            0,
            "warmup retries should not publish when the validated snapshot already matches target",
        );
    }

    #[test]
    fn verify_collision_protection_after_write_accepts_query_match() {
        let wait = MotionWaitConfig {
            threshold_rad: 0.02,
            poll_interval: Duration::from_millis(1),
            republish_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(20),
        };
        let query_attempts = Arc::new(Mutex::new(0usize));

        verify_collision_protection_after_write(
            [4_u8; 6],
            &wait,
            {
                let query_attempts = Arc::clone(&query_attempts);
                move |_| {
                    *query_attempts.lock().unwrap() += 1;
                    Ok([4_u8; 6])
                }
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(*query_attempts.lock().unwrap(), 1);
    }

    #[test]
    fn blocking_motion_loop_does_not_oversleep_past_timeout_budget() {
        let wait = MotionWaitConfig {
            threshold_rad: 0.01,
            poll_interval: Duration::from_millis(100),
            republish_interval: Duration::from_millis(100),
            timeout: Duration::from_millis(20),
        };

        let started_at = Instant::now();
        let error = blocking_motion_loop_with_cancel(
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            &wait,
            || Ok([0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            || Ok(()),
            || false,
        )
        .expect_err("unreached target should time out");

        let elapsed = started_at.elapsed();
        assert!(
            elapsed < Duration::from_millis(80),
            "timeout loop overslept too far past its 20ms budget: {elapsed:?}",
        );
        assert!(
            error.to_string().contains("motion did not reach target within 0.02s"),
            "unexpected timeout error: {error}",
        );
    }

    #[test]
    fn verify_collision_protection_after_write_retries_after_query_timeout() {
        let wait = MotionWaitConfig {
            threshold_rad: 0.02,
            poll_interval: Duration::from_millis(1),
            republish_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(20),
        };
        let query_attempts = Arc::new(Mutex::new(0usize));

        verify_collision_protection_after_write(
            [5_u8; 6],
            &wait,
            {
                let query_attempts = Arc::clone(&query_attempts);
                move |_| {
                    let mut attempts = query_attempts.lock().unwrap();
                    *attempts += 1;
                    if *attempts < 3 {
                        Err(RobotError::Timeout { timeout_ms: 1 }.into())
                    } else {
                        Ok([5_u8; 6])
                    }
                }
            },
            || Ok(()),
        )
        .unwrap();

        assert_eq!(*query_attempts.lock().unwrap(), 3);
    }

    #[test]
    fn verify_collision_protection_after_write_rejects_mismatched_query_result() {
        let wait = MotionWaitConfig {
            threshold_rad: 0.02,
            poll_interval: Duration::from_millis(1),
            republish_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(10),
        };
        let query_attempts = Arc::new(Mutex::new(0usize));

        let error = verify_collision_protection_after_write(
            [6_u8; 6],
            &wait,
            {
                let query_attempts = Arc::clone(&query_attempts);
                move |_| {
                    *query_attempts.lock().unwrap() += 1;
                    Ok([5_u8; 6])
                }
            },
            || Ok(()),
        )
        .expect_err("mismatched query response must not verify the write");

        assert!(error.to_string().contains("collision protection verification timed out"));
        assert!(*query_attempts.lock().unwrap() >= 1);
    }
}
