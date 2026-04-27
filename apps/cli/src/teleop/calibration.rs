#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use piper_client::dual_arm::{DualArmCalibration, JointMirrorMap};
use piper_client::types::{Joint, JointArray, Rad};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationFile {
    pub version: u8,
    pub created_at_unix_ms: u64,
    pub note: Option<String>,
    pub map: MirrorMapFile,
    pub zero: CalibrationZeroFile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirrorMapFile {
    pub permutation: [String; 6],
    pub position_sign: [f64; 6],
    pub velocity_sign: [f64; 6],
    pub torque_sign: [f64; 6],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationZeroFile {
    pub master: [f64; 6],
    pub slave: [f64; 6],
}

impl CalibrationFile {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read calibration file '{}'", path.display()))?;
        let file: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse calibration file '{}'", path.display()))?;
        file.validate()
            .with_context(|| format!("invalid calibration file '{}'", path.display()))?;
        Ok(file)
    }

    pub fn save_new(&self, path: &Path) -> Result<()> {
        self.validate().context("invalid calibration file")?;
        let toml = toml::to_string_pretty(self).context("failed to serialize calibration file")?;
        let mut file =
            OpenOptions::new().write(true).create_new(true).open(path).with_context(|| {
                format!(
                    "failed to create calibration file '{}'; refusing to overwrite existing files",
                    path.display()
                )
            })?;
        file.write_all(toml.as_bytes())
            .with_context(|| format!("failed to write calibration file '{}'", path.display()))?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!(
                "unsupported calibration file version {}; expected 1",
                self.version
            );
        }

        validate_permutation(&self.map.permutation)?;
        validate_signs("position_sign", &self.map.position_sign)?;
        validate_signs("velocity_sign", &self.map.velocity_sign)?;
        validate_signs("torque_sign", &self.map.torque_sign)?;
        validate_zero("master", &self.zero.master)?;
        validate_zero("slave", &self.zero.slave)?;

        Ok(())
    }

    pub fn to_calibration(&self) -> Result<DualArmCalibration> {
        self.validate()?;

        Ok(DualArmCalibration {
            master_zero: JointArray::new(self.zero.master.map(Rad)),
            slave_zero: JointArray::new(self.zero.slave.map(Rad)),
            map: JointMirrorMap {
                permutation: std::array::from_fn(|index| {
                    joint_from_name(&self.map.permutation[index])
                        .expect("validated permutation contains only known joints")
                }),
                position_sign: self.map.position_sign,
                velocity_sign: self.map.velocity_sign,
                torque_sign: self.map.torque_sign,
            },
        })
    }

    pub fn from_calibration(
        calibration: &DualArmCalibration,
        note: Option<String>,
        created_at_unix_ms: u64,
    ) -> Self {
        Self {
            version: 1,
            created_at_unix_ms,
            note,
            map: MirrorMapFile {
                permutation: std::array::from_fn(|index| {
                    calibration.map.permutation[index].name().to_string()
                }),
                position_sign: calibration.map.position_sign,
                velocity_sign: calibration.map.velocity_sign,
                torque_sign: calibration.map.torque_sign,
            },
            zero: CalibrationZeroFile {
                master: std::array::from_fn(|index| calibration.master_zero[index].0),
                slave: std::array::from_fn(|index| calibration.slave_zero[index].0),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompatibilityError {
    pub max_error_rad: f64,
    pub joint_errors_rad: [f64; 6],
    pub max_joint: Joint,
    pub threshold_rad: f64,
}

impl std::fmt::Display for CompatibilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "posture mismatch: max error {:.6} rad at {} exceeds threshold {:.6} rad",
            self.max_error_rad, self.max_joint, self.threshold_rad
        )
    }
}

impl std::error::Error for CompatibilityError {}

pub fn check_posture_compatibility(
    calibration: &DualArmCalibration,
    master: JointArray<Rad>,
    slave: JointArray<Rad>,
    max_error_rad: f64,
) -> std::result::Result<(), CompatibilityError> {
    let threshold_rad = max_error_rad;
    if !threshold_rad.is_finite() || threshold_rad <= 0.0 {
        return Err(CompatibilityError {
            max_error_rad: threshold_rad,
            joint_errors_rad: [0.0; 6],
            max_joint: Joint::J1,
            threshold_rad,
        });
    }

    let expected_slave = calibration.master_to_slave_position(master);
    let joint_errors_rad =
        std::array::from_fn(|index| (expected_slave[index].0 - slave[index].0).abs());
    let mut max_index = 0;
    let mut max_error_rad = joint_errors_rad[0];
    for (index, error_rad) in joint_errors_rad.iter().copied().enumerate() {
        if !error_rad.is_finite() {
            return Err(CompatibilityError {
                max_error_rad: f64::INFINITY,
                joint_errors_rad,
                max_joint: Joint::from_index(index).expect("joint error index is in range"),
                threshold_rad,
            });
        }
        if error_rad > max_error_rad {
            max_index = index;
            max_error_rad = error_rad;
        }
    }

    if max_error_rad > threshold_rad {
        return Err(CompatibilityError {
            max_error_rad,
            joint_errors_rad,
            max_joint: Joint::from_index(max_index).expect("joint error index is in range"),
            threshold_rad,
        });
    }

    Ok(())
}

fn validate_permutation(permutation: &[String; 6]) -> Result<()> {
    let mut seen = [false; 6];
    for name in permutation {
        let joint = joint_from_name(name)
            .with_context(|| format!("unknown joint name '{name}'; expected J1 through J6"))?;
        let index = joint.index();
        if seen[index] {
            bail!("mirror permutation contains duplicate joint {joint}");
        }
        seen[index] = true;
    }

    if !seen.iter().all(|seen| *seen) {
        bail!("mirror permutation must contain J1 through J6 exactly once");
    }

    Ok(())
}

fn validate_signs(field: &str, signs: &[f64; 6]) -> Result<()> {
    for (index, sign) in signs.iter().copied().enumerate() {
        if !sign.is_finite() || (sign != 1.0 && sign != -1.0) {
            bail!("{field}[{index}] must be finite and exactly +/-1.0; got {sign}");
        }
    }
    Ok(())
}

fn validate_zero(field: &str, zero: &[f64; 6]) -> Result<()> {
    for (index, value) in zero.iter().copied().enumerate() {
        if !value.is_finite() {
            bail!("zero.{field}[{index}] must be finite; got {value}");
        }
    }
    Ok(())
}

fn joint_from_name(name: &str) -> Option<Joint> {
    match name {
        "J1" => Some(Joint::J1),
        "J2" => Some(Joint::J2),
        "J3" => Some(Joint::J3),
        "J4" => Some(Joint::J4),
        "J5" => Some(Joint::J5),
        "J6" => Some(Joint::J6),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_client::dual_arm::{DualArmCalibration, JointMirrorMap};
    use piper_client::types::{JointArray, Rad};

    fn sample_calibration() -> DualArmCalibration {
        DualArmCalibration {
            master_zero: JointArray::splat(Rad(0.0)),
            slave_zero: JointArray::splat(Rad(0.0)),
            map: JointMirrorMap::left_right_mirror(),
        }
    }

    #[test]
    fn calibration_file_round_trips() {
        let file = CalibrationFile::from_calibration(
            &sample_calibration(),
            Some("bench A".to_string()),
            1_770_000_000_000,
        );
        let toml = toml::to_string(&file).unwrap();
        let decoded: CalibrationFile = toml::from_str(&toml).unwrap();

        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.to_calibration().unwrap(), sample_calibration());
    }

    #[test]
    fn calibration_rejects_invalid_signs() {
        let mut file = CalibrationFile::from_calibration(&sample_calibration(), None, 1);
        file.map.position_sign[0] = 0.0;
        assert!(file.validate().is_err());
    }

    #[test]
    fn compatibility_check_detects_slave_mismatch() {
        let calibration = sample_calibration();
        let master = JointArray::splat(Rad(0.0));
        let slave = JointArray::splat(Rad(1.0));

        let err = check_posture_compatibility(&calibration, master, slave, 0.05)
            .expect_err("mismatch should fail");

        assert!(err.max_error_rad > 0.05);
    }

    #[test]
    fn compatibility_check_rejects_non_finite_master_or_slave_posture_without_panic() {
        let calibration = sample_calibration();
        let mut master = JointArray::splat(Rad(0.0));
        let slave = JointArray::splat(Rad(0.0));
        master[Joint::J1] = Rad(f64::NAN);

        let err = check_posture_compatibility(&calibration, master, slave, 0.05)
            .expect_err("non-finite posture should fail");

        assert!(err.max_error_rad.is_infinite() || !err.max_error_rad.is_finite());
        assert_eq!(err.max_joint, Joint::J1);
        assert_eq!(err.threshold_rad, 0.05);

        let master = JointArray::splat(Rad(0.0));
        let mut slave = JointArray::splat(Rad(0.0));
        slave[Joint::J2] = Rad(f64::NAN);

        let err = check_posture_compatibility(&calibration, master, slave, 0.05)
            .expect_err("non-finite posture should fail");

        assert!(err.max_error_rad.is_infinite() || !err.max_error_rad.is_finite());
        assert_eq!(err.max_joint, Joint::J2);
        assert_eq!(err.threshold_rad, 0.05);
    }

    #[test]
    fn save_new_refuses_to_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("calibration.toml");
        let file = CalibrationFile::from_calibration(&sample_calibration(), None, 1);

        file.save_new(&path).unwrap();

        assert!(file.save_new(&path).is_err());
        assert_eq!(CalibrationFile::load(&path).unwrap(), file);
    }

    #[test]
    fn calibration_rejects_duplicate_joint_names() {
        let mut file = CalibrationFile::from_calibration(&sample_calibration(), None, 1);
        file.map.permutation[1] = "J1".to_string();

        assert!(file.validate().is_err());
    }

    #[test]
    fn calibration_rejects_unknown_joint_names() {
        let mut file = CalibrationFile::from_calibration(&sample_calibration(), None, 1);
        file.map.permutation[0] = "J7".to_string();

        assert!(file.validate().is_err());
    }

    #[test]
    fn calibration_rejects_non_finite_zero_values() {
        let mut file = CalibrationFile::from_calibration(&sample_calibration(), None, 1);
        file.zero.master[0] = f64::NAN;

        assert!(file.validate().is_err());
    }
}
