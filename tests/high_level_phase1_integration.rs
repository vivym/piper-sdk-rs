//! Phase 1 集成测试
//!
//! 验证基础类型系统的整体可用性和协同工作。

#[path = "high_level/common/mod.rs"]
mod common;

use piper_sdk::client::types::*;

#[test]
fn test_full_joint_command() {
    // 模拟完整的关节指令构建
    let target_positions: JointArray<Rad> = JointArray::new([
        Deg(0.0).to_rad(),
        Deg(45.0).to_rad(),
        Deg(90.0).to_rad(),
        Deg(-45.0).to_rad(),
        Deg(0.0).to_rad(),
        Deg(30.0).to_rad(),
    ]);

    // 验证转换正确
    assert!((target_positions[Joint::J2].0 - std::f64::consts::FRAC_PI_4).abs() < 1e-10);
    assert!((target_positions[Joint::J3].0 - std::f64::consts::FRAC_PI_2).abs() < 1e-10);
}

#[test]
fn test_joint_array_map_with_errors() {
    // 模拟限位检查
    let positions: JointArray<Rad> = JointArray::new([Rad(3.5); 6]);
    let limit = Rad(std::f64::consts::PI);

    let mut errors = Vec::new();
    for joint in Joint::ALL {
        if positions[joint].0 > limit.0 {
            errors.push(RobotError::joint_limit(joint, positions[joint].0, limit.0));
        }
    }

    assert_eq!(errors.len(), 6);
    assert!(errors[0].is_limit_error());
    assert!(!errors[0].is_fatal());
}

#[test]
fn test_cartesian_to_joint_simulation() {
    // 模拟笛卡尔空间指令转换为关节空间
    let target_pose = CartesianPose::from_position_euler(
        0.5,
        0.0,
        0.3,
        Rad(0.0),
        Rad(0.0),
        Rad(std::f64::consts::FRAC_PI_2),
    );

    // 验证姿态
    let (_roll, _pitch, yaw) = target_pose.orientation.to_euler();
    assert!((yaw.0 - std::f64::consts::FRAC_PI_2).abs() < 1e-10);
    assert_eq!(target_pose.position.x, 0.5);
}

#[test]
fn test_error_propagation() {
    // 模拟错误传播
    let result: Result<JointArray<Rad>> = Err(RobotError::EmergencyStop);

    assert!(result.is_err());
    match result {
        Ok(_) => panic!("expected Err(RobotError::EmergencyStop)"),
        Err(err) => {
            assert!(err.is_fatal());
            assert!(!err.is_retryable());
        },
    }
}

#[test]
fn test_joint_velocity_calculation() {
    // 模拟速度计算
    let pos_t0: JointArray<Rad> = JointArray::splat(Rad(0.0));
    let pos_t1: JointArray<Rad> = JointArray::splat(Rad(0.1));
    let dt = 0.01; // 10ms

    let velocities = pos_t1.map_with(pos_t0, |p1, p0| Rad((p1.0 - p0.0) / dt));

    for vel in velocities.iter() {
        assert!((vel.0 - 10.0).abs() < 1e-10); // 0.1 / 0.01 = 10 rad/s
    }
}

#[test]
fn test_joint_torque_limits() {
    // 模拟力矩限制检查
    let torques: JointArray<NewtonMeter> = JointArray::new([
        NewtonMeter(5.0),
        NewtonMeter(10.0),
        NewtonMeter(15.0), // 超限
        NewtonMeter(8.0),
        NewtonMeter(6.0),
        NewtonMeter(4.0),
    ]);

    let max_torque = NewtonMeter(12.0);

    let exceeded: Vec<_> = Joint::ALL.iter().filter(|&&j| torques[j].0 > max_torque.0).collect();

    assert_eq!(exceeded.len(), 1);
    assert_eq!(*exceeded[0], Joint::J3);
}

#[test]
fn test_unit_type_safety() {
    // 编译期类型安全验证（这些应该无法编译）
    let _rad = Rad(1.0);
    let deg = Deg(180.0);

    // 以下代码应该无法编译（类型不匹配）
    // let _ = rad + deg;  // ❌ 编译错误
    // let _ = rad == deg; // ❌ 编译错误

    // 但转换后可以比较
    let rad2 = deg.to_rad();
    assert!((rad2.0 - std::f64::consts::PI).abs() < 1e-10);
}

#[test]
fn test_cartesian_velocity_composition() {
    // 模拟笛卡尔速度组合
    let linear = Position3D::new(0.1, 0.0, 0.0);
    let angular = Position3D::new(0.0, 0.0, 0.5);

    let vel = CartesianVelocity::new(linear, angular);

    assert_eq!(vel.linear.x, 0.1);
    assert_eq!(vel.angular.z, 0.5);
}

#[test]
fn test_quaternion_rotation_composition() {
    // 模拟旋转组合
    let q1 = Quaternion::from_euler(Rad(0.1), Rad(0.0), Rad(0.0));
    let q2 = Quaternion::from_euler(Rad(0.0), Rad(0.2), Rad(0.0));
    let q_combined = q1.multiply(&q2);

    // 验证组合后的四元数是单位四元数
    let norm_sq = q_combined.w * q_combined.w
        + q_combined.x * q_combined.x
        + q_combined.y * q_combined.y
        + q_combined.z * q_combined.z;
    assert!((norm_sq.sqrt() - 1.0).abs() < 1e-10);
}

#[test]
fn test_joint_array_functional_operations() {
    // 模拟函数式操作
    let positions: JointArray<Rad> =
        JointArray::new([Rad(0.0), Rad(0.5), Rad(1.0), Rad(1.5), Rad(2.0), Rad(2.5)]);

    // 映射：弧度 -> 角度
    let degrees = positions.map(|r| r.to_deg());

    // 过滤：找出超过90度的关节
    let large_angles: Vec<_> = Joint::ALL.iter().filter(|&&j| degrees[j].0 > 90.0).collect();

    assert!(large_angles.len() >= 2);
}

#[test]
fn test_error_context_chaining() {
    // 模拟错误上下文链
    let base_error = RobotError::Unknown("sensor read failed".to_string());
    let with_context = base_error.context("during state update");

    let msg = format!("{}", with_context);
    assert!(msg.contains("during state update"));
    assert!(msg.contains("sensor read failed"));
}

#[test]
fn test_position_vector_operations() {
    // 模拟3D向量运算
    let v1 = Position3D::new(1.0, 0.0, 0.0);
    let v2 = Position3D::new(0.0, 1.0, 0.0);

    // 叉积
    let v3 = v1.cross(&v2);
    assert_eq!(v3.z, 1.0);

    // 点积
    let dot = v1.dot(&v2);
    assert_eq!(dot, 0.0); // 正交向量

    // 归一化
    let v4 = Position3D::new(3.0, 4.0, 0.0);
    let normalized = v4.normalize();
    assert!((normalized.norm() - 1.0).abs() < 1e-10);
}

#[test]
fn test_type_conversion_round_trip() {
    // 测试类型转换往返
    let joint_array = JointArray::new([1, 2, 3, 4, 5, 6]);
    let raw_array: [i32; 6] = joint_array.into();
    let joint_array2 = JointArray::from(raw_array);

    for i in 0..6 {
        assert_eq!(joint_array[i], joint_array2[i]);
    }
}

#[test]
fn test_zero_constants_consistency() {
    // 验证零值常量的一致性
    assert_eq!(Rad::ZERO.0, 0.0);
    assert_eq!(Deg::ZERO.0, 0.0);
    assert_eq!(NewtonMeter::ZERO.0, 0.0);

    assert_eq!(Position3D::ZERO.x, 0.0);
    assert_eq!(Position3D::ZERO.y, 0.0);
    assert_eq!(Position3D::ZERO.z, 0.0);

    assert_eq!(CartesianPose::ZERO.position.x, 0.0);
    assert_eq!(CartesianVelocity::ZERO.linear.x, 0.0);
    assert_eq!(CartesianEffort::ZERO.force.x, 0.0);
}
