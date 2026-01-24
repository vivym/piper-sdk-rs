//! 夹爪控制示例
//!
//! 展示如何使用高层 API 控制夹爪，包括：
//! - 打开/关闭夹爪
//! - 精确位置控制
//! - 力度控制
//! - 读取夹爪状态
//!
//! # 运行
//!
//! ```bash
//! cargo run --example high_level_gripper_control
//! ```

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🤏 Piper SDK - 夹爪控制示例");
    println!("================================\n");

    // 注意：这是演示代码，实际使用需要连接到真实硬件
    println!("⚠️  演示模式：展示 API 使用方法");
    println!("   实际使用时请连接到 Piper 机械臂\n");

    demonstrate_gripper_api();

    Ok(())
}

/// 演示夹爪 API 的使用方法
fn demonstrate_gripper_api() {
    println!("📋 夹爪控制 API 使用方法:\n");

    // 1. 基本控制方法
    println!("1️⃣  基本控制:");
    println!("   ```rust");
    println!("   // 创建 MotionCommander（从已连接的 Piper 获取）");
    println!("   // let piper: Piper<Active<MitMode>> = ...;");
    println!("   // let commander = piper.motion_commander();");
    println!();
    println!("   // 打开夹爪（position = 1.0, effort = 0.3）");
    println!("   commander.open_gripper()?;");
    println!();
    println!("   // 关闭夹爪（position = 0.0, 指定力度）");
    println!("   commander.close_gripper(0.5)?;  // 中等力度");
    println!("   ```\n");

    // 2. 精确位置控制
    println!("2️⃣  精确位置控制:");
    println!("   ```rust");
    println!("   // 设置夹爪到特定位置");
    println!("   // position: 0.0 (完全闭合) -> 1.0 (完全打开)");
    println!("   // effort:   0.0 (最小力度) -> 1.0 (最大力度)");
    println!();
    println!("   // 半开状态，低力度");
    println!("   commander.set_gripper(0.5, 0.3)?;");
    println!();
    println!("   // 夹取小物体，精确位置，中等力度");
    println!("   commander.set_gripper(0.2, 0.5)?;");
    println!();
    println!("   // 夹取大物体，保持打开，高力度");
    println!("   commander.set_gripper(0.8, 0.8)?;");
    println!("   ```\n");

    // 3. 读取夹爪状态
    println!("3️⃣  读取夹爪状态:");
    println!("   ```rust");
    println!("   // 从 Observer 读取夹爪状态");
    println!("   // let observer = piper.observer();");
    println!("   let gripper_state = observer.gripper_state();");
    println!();
    println!("   println!(\"夹爪位置: {{}}\", gripper_state.position);");
    println!("   println!(\"夹爪力度: {{}}\", gripper_state.effort);");
    println!("   println!(\"夹爪使能: {{}}\", gripper_state.enabled);");
    println!("   ```\n");

    // 4. 实际应用场景
    println!("4️⃣  实际应用场景:\n");

    println!("   📦 场景 1: 抓取物体");
    println!("   ```rust");
    println!("   // 1. 打开夹爪准备抓取");
    println!("   commander.open_gripper()?;");
    println!("   thread::sleep(Duration::from_millis(500));");
    println!();
    println!("   // 2. （移动机械臂到物体位置）");
    println!("   // piper.move_to_position(...)?;");
    println!();
    println!("   // 3. 闭合夹爪，中等力度");
    println!("   commander.close_gripper(0.5)?;");
    println!("   thread::sleep(Duration::from_millis(300));");
    println!();
    println!("   // 4. 检查是否抓取成功");
    println!("   let state = observer.gripper_state();");
    println!("   if state.position < 0.1 {{");
    println!("       println!(\"✅ 抓取成功\");");
    println!("   }} else {{");
    println!("       println!(\"❌ 未检测到物体\");");
    println!("   }}");
    println!("   ```\n");

    println!("   🔄 场景 2: 精确夹持");
    println!("   ```rust");
    println!("   // 对于精密操作，逐步闭合");
    println!("   for position in (0..10).rev() {{");
    println!("       let pos = position as f64 / 10.0;");
    println!("       commander.set_gripper(pos, 0.4)?;");
    println!("       thread::sleep(Duration::from_millis(50));");
    println!();
    println!("       // 检查是否接触到物体（位置不再变化）");
    println!("       let current = observer.gripper_state().position;");
    println!("       if (current - pos).abs() > 0.05 {{");
    println!("           println!(\"检测到物体\");");
    println!("           break;");
    println!("       }}");
    println!("   }}");
    println!("   ```\n");

    println!("   🎯 场景 3: 力度感知");
    println!("   ```rust");
    println!("   // 软性物体使用低力度");
    println!("   commander.set_gripper(0.3, 0.2)?;  // 轻柔夹持");
    println!();
    println!("   // 硬性物体使用高力度");
    println!("   commander.set_gripper(0.2, 0.8)?;  // 牢固抓取");
    println!();
    println!("   // 动态调整力度");
    println!("   let state = observer.gripper_state();");
    println!("   if state.position > 0.5 {{  // 物体较大");
    println!("       commander.set_gripper(state.position, 0.6)?;");
    println!("   }}");
    println!("   ```\n");

    // 5. 注意事项
    println!("⚠️  注意事项:\n");
    println!("   1. 参数范围:");
    println!("      - position: 必须在 [0.0, 1.0] 范围内");
    println!("      - effort: 必须在 [0.0, 1.0] 范围内");
    println!("      - 超出范围会返回 RobotError::ConfigError");
    println!();
    println!("   2. 操作间隔:");
    println!("      - 连续操作间建议间隔 50-100ms");
    println!("      - 等待夹爪完全到位需 200-500ms");
    println!();
    println!("   3. 安全考虑:");
    println!("      - 首次使用时从低力度开始测试");
    println!("      - 避免对精密物体使用最大力度");
    println!("      - 定期检查夹爪状态，防止卡死");
    println!();
    println!("   4. 错误处理:");
    println!("      - 夹爪通信失败会返回 CommunicationError");
    println!("      - 状态机 Poisoned 时无法控制");
    println!("      - 记得检查返回的 Result");
    println!();

    // 6. 完整示例
    println!("📝 完整示例代码:\n");
    println!("```rust");
    println!("use piper_sdk::client::{{");
    println!("    state::{{Piper, Active, MitMode}},");
    println!("    types::Result,");
    println!("}};");
    println!("use std::{{thread, time::Duration}};");
    println!();
    println!("fn gripper_demo(piper: Piper<Active<MitMode>>) -> Result<()> {{");
    println!("    let commander = piper.motion_commander();");
    println!("    let observer = piper.observer();");
    println!();
    println!("    // 1. 打开夹爪");
    println!("    println!(\"打开夹爪...\");");
    println!("    commander.open_gripper()?;");
    println!("    thread::sleep(Duration::from_millis(500));");
    println!();
    println!("    // 2. 逐步闭合");
    println!("    println!(\"逐步闭合...\");");
    println!("    for i in (0..=10).rev() {{");
    println!("        let pos = i as f64 / 10.0;");
    println!("        commander.set_gripper(pos, 0.5)?;");
    println!();
    println!("        let state = observer.gripper_state();");
    println!("        println!(\"位置: {{:.2}}, 力度: {{:.2}}\", ");
    println!("                 state.position, state.effort);");
    println!();
    println!("        thread::sleep(Duration::from_millis(100));");
    println!("    }}");
    println!();
    println!("    // 3. 完全闭合");
    println!("    println!(\"完全闭合...\");");
    println!("    commander.close_gripper(0.7)?;");
    println!("    thread::sleep(Duration::from_millis(300));");
    println!();
    println!("    // 4. 再次打开");
    println!("    println!(\"重新打开...\");");
    println!("    commander.open_gripper()?;");
    println!();
    println!("    Ok(())");
    println!("}}");
    println!("```\n");

    println!("✅ 示例说明完成");
    println!("\n💡 提示: 修改上述代码并连接真实硬件即可运行");
}
