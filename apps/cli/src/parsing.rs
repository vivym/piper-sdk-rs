use anyhow::{Result, bail};

pub fn parse_joint_indices_arg(raw: Option<&str>) -> Result<Vec<usize>> {
    let Some(raw) = raw else {
        return Ok((0..6).collect());
    };

    let parsed = raw
        .split(',')
        .map(|part| part.trim().parse::<usize>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| anyhow::anyhow!("解析关节编号失败"))?;

    normalize_joint_indices(parsed.as_slice())
}

pub fn normalize_joint_indices(joints: &[usize]) -> Result<Vec<usize>> {
    if joints.is_empty() {
        bail!("至少需要一个关节编号");
    }

    let mut normalized = Vec::with_capacity(joints.len());
    for joint in joints {
        if !(1..=6).contains(joint) {
            bail!("关节编号必须在 1..=6 范围内，得到 {}", joint);
        }
        let zero_based = joint - 1;
        if !normalized.contains(&zero_based) {
            normalized.push(zero_based);
        }
    }

    Ok(normalized)
}

pub fn parse_collision_levels(level: Option<u8>, levels: Option<&str>) -> Result<[u8; 6]> {
    match (level, levels) {
        (Some(_), Some(_)) => bail!("--level 和 --levels 不能同时指定"),
        (None, None) => bail!("必须指定 --level 或 --levels"),
        (Some(level), None) => {
            if level > 8 {
                bail!("碰撞保护等级必须在 0..=8 范围内");
            }
            Ok([level; 6])
        },
        (None, Some(levels)) => {
            let parsed = levels
                .split(',')
                .map(|part| part.trim().parse::<u8>())
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| anyhow::anyhow!("无效的碰撞保护等级格式"))?;
            if parsed.len() != 6 {
                bail!("--levels 需要 6 个值，得到 {}", parsed.len());
            }
            if parsed.iter().any(|value| *value > 8) {
                bail!("碰撞保护等级必须在 0..=8 范围内");
            }
            Ok([
                parsed[0], parsed[1], parsed[2], parsed[3], parsed[4], parsed[5],
            ])
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_joint_indices_defaults_to_all() {
        assert_eq!(
            parse_joint_indices_arg(None).unwrap(),
            vec![0, 1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn normalize_joint_indices_converts_to_zero_based() {
        assert_eq!(normalize_joint_indices(&[1, 3, 6]).unwrap(), vec![0, 2, 5]);
    }

    #[test]
    fn parse_collision_levels_supports_uniform_and_per_joint() {
        assert_eq!(parse_collision_levels(Some(3), None).unwrap(), [3; 6]);
        assert_eq!(
            parse_collision_levels(None, Some("1,2,3,4,5,6")).unwrap(),
            [1, 2, 3, 4, 5, 6]
        );
    }
}
