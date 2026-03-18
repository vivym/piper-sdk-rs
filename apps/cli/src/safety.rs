//! CLI 安全确认辅助

use anyhow::Result;
use piper_control::PreparedMove;

pub fn confirm_prepared_move(prepared: &PreparedMove) -> Result<bool> {
    println!("⚠️  大幅移动检测");
    println!("  最大位移: {:.1}°", prepared.max_delta_deg);
    println!("  当前: {}", format_joint_values(&prepared.current));
    println!(
        "  目标: {}",
        format_joint_values(&prepared.effective_target)
    );

    inquire::Confirm::new("确定要继续吗？")
        .with_default(false)
        .prompt()
        .map_err(|error| anyhow::anyhow!("用户交互失败: {error}"))
}

pub fn confirm_zero_setting(joints: &[usize]) -> Result<bool> {
    let description = if joints.len() == 6 {
        "全部关节".to_string()
    } else {
        joints
            .iter()
            .map(|joint| format!("J{}", joint + 1))
            .collect::<Vec<_>>()
            .join(", ")
    };

    println!("⚠️  即将把 {} 当前位置写入零点标定", description);
    inquire::Confirm::new("确定要继续吗？")
        .with_default(false)
        .prompt()
        .map_err(|error| anyhow::anyhow!("用户交互失败: {error}"))
}

fn format_joint_values(values: &[f64; 6]) -> String {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| format!("J{}={:.3}", index + 1, value))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use piper_control::PreparedMove;

    #[test]
    fn prepared_move_reports_delta_not_absolute_target() {
        let prepared = PreparedMove {
            current: [2.5, 0.0, 0.0, 0.0, 0.0, 0.0],
            effective_target: [0.1, 0.0, 0.0, 0.0, 0.0, 0.0],
            max_delta_rad: 2.4,
            max_delta_deg: 2.4_f64.to_degrees(),
            requires_confirmation: true,
        };

        assert!(prepared.requires_confirmation);
        assert!(prepared.max_delta_deg > 100.0);
    }
}
