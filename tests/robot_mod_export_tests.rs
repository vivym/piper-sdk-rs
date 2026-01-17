//! Robot 模块导出测试
//!
//! 验证所有公共 API 可以从 `piper_sdk::robot` 导入

use piper_sdk::robot::*;

/// 测试所有公共类型和函数都可以导入
#[test]
fn test_module_exports() {
    // 测试错误类型
    let _: RobotError = RobotError::Timeout;

    // 测试状态类型
    let _: CoreMotionState = CoreMotionState::default();
    let _: JointDynamicState = JointDynamicState::default();
    let _: ControlStatusState = ControlStatusState::default();
    let _: DiagnosticState = DiagnosticState::default();
    let _: ConfigState = ConfigState::default();
    let _: PiperContext = PiperContext::new();
    let _: CombinedMotionState = CombinedMotionState {
        core: CoreMotionState::default(),
        joint_dynamic: JointDynamicState::default(),
    };
    let _: AlignedMotionState = AlignedMotionState {
        joint_pos: [0.0; 6],
        joint_vel: [0.0; 6],
        joint_current: [0.0; 6],
        end_pose: [0.0; 6],
        timestamp: 0,
        time_diff_us: 0,
    };
    let _: AlignmentResult = AlignmentResult::Ok(AlignedMotionState {
        joint_pos: [0.0; 6],
        joint_vel: [0.0; 6],
        joint_current: [0.0; 6],
        end_pose: [0.0; 6],
        timestamp: 0,
        time_diff_us: 0,
    });

    // 测试 Pipeline 类型
    let _: PipelineConfig = PipelineConfig::default();

    // 测试 Builder
    let _: PiperBuilder = PiperBuilder::new();

    // 验证导入成功（如果没有编译错误，说明所有类型都可以导入）
}
