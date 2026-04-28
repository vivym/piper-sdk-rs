use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use piper_sdk::JointMirrorMap;
use piper_sdk::client::types::Joint;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum CalibrationError {
    #[error("{0}")]
    Invalid(String),
    #[error("{kind} file is not in canonical TOML form")]
    NonCanonical { kind: &'static str },
    #[error(
        "calibration mirror map mismatch at {field}[{joint_index}]: expected {expected}, got {actual}"
    )]
    MirrorMapMismatch {
        field: &'static str,
        joint_index: usize,
        expected: String,
        actual: String,
    },
    #[error("no calibration was supplied or captured for this episode")]
    MissingCalibration,
    #[error(
        "{arm} joint {joint_index} posture differs from calibration zero: current {current_rad} rad, zero {zero_rad} rad, max error {max_error_rad} rad"
    )]
    PostureMismatch {
        arm: &'static str,
        joint_index: usize,
        current_rad: f64,
        zero_rad: f64,
        max_error_rad: f64,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirrorMapFile {
    pub schema_version: u32,
    pub permutation: [usize; 6],
    pub position_sign: [f64; 6],
    pub velocity_sign: [f64; 6],
    pub torque_sign: [f64; 6],
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationFile {
    pub schema_version: u32,
    pub created_unix_ms: u64,
    pub master_zero_rad: [f64; 6],
    pub slave_zero_rad: [f64; 6],
    pub mirror_map: MirrorMapFile,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedMirrorMap {
    pub mirror_map: MirrorMapFile,
    pub runtime_map: JointMirrorMap,
    pub canonical_bytes: Vec<u8>,
    pub sha256_hex: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCalibration {
    pub calibration: CalibrationFile,
    pub canonical_bytes: Vec<u8>,
    pub sha256_hex: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MirrorMapStandaloneToml {
    schema_version: u32,
    permutation: [usize; 6],
    position_sign: [f64; 6],
    velocity_sign: [f64; 6],
    torque_sign: [f64; 6],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MirrorMapBodyToml {
    permutation: [usize; 6],
    position_sign: [f64; 6],
    velocity_sign: [f64; 6],
    torque_sign: [f64; 6],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalibrationToml {
    schema_version: u32,
    created_unix_ms: u64,
    master_zero_rad: [f64; 6],
    slave_zero_rad: [f64; 6],
    mirror_map: MirrorMapBodyToml,
}

impl MirrorMapFile {
    pub fn left_right_for_tests() -> Self {
        Self::from_runtime_map(JointMirrorMap::left_right_mirror())
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, CalibrationError> {
        let text = std::str::from_utf8(bytes)?;
        let raw: MirrorMapStandaloneToml = parse_toml("mirror map", text)?;
        let mirror_map = Self {
            schema_version: raw.schema_version,
            permutation: raw.permutation,
            position_sign: raw.position_sign,
            velocity_sign: raw.velocity_sign,
            torque_sign: raw.torque_sign,
        };
        let canonical = mirror_map.to_canonical_toml_bytes()?;
        if canonical.as_slice() != bytes {
            return Err(CalibrationError::NonCanonical { kind: "mirror map" });
        }
        Ok(mirror_map)
    }

    pub fn to_canonical_toml_bytes(&self) -> Result<Vec<u8>, CalibrationError> {
        self.validate()?;

        let mut out = String::new();
        push_u32_line(&mut out, "schema_version", self.schema_version);
        push_mirror_map_body(&mut out, self);
        Ok(out.into_bytes())
    }

    pub fn to_runtime_map(&self) -> Result<JointMirrorMap, CalibrationError> {
        self.validate()?;
        Ok(JointMirrorMap {
            permutation: std::array::from_fn(|index| {
                Joint::from_index(self.permutation[index]).expect("validated permutation index")
            }),
            position_sign: self.position_sign,
            velocity_sign: self.velocity_sign,
            torque_sign: self.torque_sign,
        })
    }

    fn from_runtime_map(map: JointMirrorMap) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            permutation: map.permutation.map(Joint::index),
            position_sign: map.position_sign,
            velocity_sign: map.velocity_sign,
            torque_sign: map.torque_sign,
        }
    }

    fn from_body(body: MirrorMapBodyToml) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            permutation: body.permutation,
            position_sign: body.position_sign,
            velocity_sign: body.velocity_sign,
            torque_sign: body.torque_sign,
        }
    }

    fn validate(&self) -> Result<(), CalibrationError> {
        validate_schema_version(self.schema_version)?;
        validate_permutation("mirror_map.permutation", &self.permutation)?;
        validate_sign_array("mirror_map.position_sign", &self.position_sign)?;
        validate_sign_array("mirror_map.velocity_sign", &self.velocity_sign)?;
        validate_sign_array("mirror_map.torque_sign", &self.torque_sign)?;
        Ok(())
    }
}

impl CalibrationFile {
    pub fn identity_for_tests() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            created_unix_ms: 0,
            master_zero_rad: [0.0; 6],
            slave_zero_rad: [0.0; 6],
            mirror_map: MirrorMapFile::left_right_for_tests(),
        }
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, CalibrationError> {
        let text = std::str::from_utf8(bytes)?;
        let raw: CalibrationToml = parse_toml("calibration", text)?;
        let calibration = Self {
            schema_version: raw.schema_version,
            created_unix_ms: raw.created_unix_ms,
            master_zero_rad: raw.master_zero_rad,
            slave_zero_rad: raw.slave_zero_rad,
            mirror_map: MirrorMapFile::from_body(raw.mirror_map),
        };
        let canonical = calibration.to_canonical_toml_bytes()?;
        if canonical.as_slice() != bytes {
            return Err(CalibrationError::NonCanonical {
                kind: "calibration",
            });
        }
        Ok(calibration)
    }

    pub fn to_canonical_toml_bytes(&self) -> Result<Vec<u8>, CalibrationError> {
        self.validate()?;

        let mut out = String::new();
        push_u32_line(&mut out, "schema_version", self.schema_version);
        push_u64_line(&mut out, "created_unix_ms", self.created_unix_ms);
        push_f64_array_line(&mut out, "master_zero_rad", &self.master_zero_rad);
        push_f64_array_line(&mut out, "slave_zero_rad", &self.slave_zero_rad);
        out.push('\n');
        out.push_str("[mirror_map]\n");
        push_mirror_map_body(&mut out, &self.mirror_map);
        Ok(out.into_bytes())
    }

    pub fn sha256_hex(&self) -> Result<String, CalibrationError> {
        Ok(sha256_hex(&self.to_canonical_toml_bytes()?))
    }

    fn validate(&self) -> Result<(), CalibrationError> {
        validate_schema_version(self.schema_version)?;
        validate_finite_array("master_zero_rad", &self.master_zero_rad)?;
        validate_finite_array("slave_zero_rad", &self.slave_zero_rad)?;
        self.mirror_map.validate()
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(hex_nibble(byte >> 4));
        out.push(hex_nibble(byte & 0x0f));
    }
    out
}

pub fn persist_calibration_no_overwrite(
    path: impl AsRef<Path>,
    bytes: &[u8],
) -> Result<(), CalibrationError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

pub fn load_file_backed_mirror_map_bytes(
    bytes: &[u8],
) -> Result<LoadedMirrorMap, CalibrationError> {
    let mirror_map = MirrorMapFile::from_canonical_bytes(bytes)?;
    let runtime_map = mirror_map.to_runtime_map()?;
    Ok(LoadedMirrorMap {
        mirror_map,
        runtime_map,
        canonical_bytes: bytes.to_vec(),
        sha256_hex: sha256_hex(bytes),
    })
}

/// Resolve the calibration that will be persisted for an episode.
///
/// A loaded calibration takes precedence over a captured calibration. If no
/// loaded calibration is supplied, the captured calibration is used. If neither
/// is available, this returns [`CalibrationError::MissingCalibration`]. The
/// selected calibration's mirror map must match the already-resolved effective
/// runtime mirror map exactly before the episode can proceed.
pub fn resolve_episode_calibration(
    loaded: Option<CalibrationFile>,
    effective_map: JointMirrorMap,
    captured: Option<CalibrationFile>,
) -> Result<ResolvedCalibration, CalibrationError> {
    let calibration = loaded.or(captured).ok_or(CalibrationError::MissingCalibration)?;
    calibration.validate()?;
    validate_runtime_map_matches_file(&calibration.mirror_map, effective_map)?;
    let canonical_bytes = calibration.to_canonical_toml_bytes()?;
    let sha256_hex = sha256_hex(&canonical_bytes);
    Ok(ResolvedCalibration {
        calibration,
        canonical_bytes,
        sha256_hex,
    })
}

pub fn validate_current_posture(
    calibration: &CalibrationFile,
    current_master: [f64; 6],
    current_slave: [f64; 6],
    max_error_rad: f64,
) -> Result<(), CalibrationError> {
    calibration.validate()?;
    validate_positive("calibration_max_error_rad", max_error_rad)?;
    validate_posture_arm(
        "master",
        &current_master,
        &calibration.master_zero_rad,
        max_error_rad,
    )?;
    validate_posture_arm(
        "slave",
        &current_slave,
        &calibration.slave_zero_rad,
        max_error_rad,
    )
}

fn parse_toml<T>(kind: &str, text: &str) -> Result<T, CalibrationError>
where
    T: for<'de> Deserialize<'de>,
{
    toml::from_str(text)
        .map_err(|error| CalibrationError::Invalid(format!("{kind} TOML parse failed: {error}")))
}

fn validate_schema_version(schema_version: u32) -> Result<(), CalibrationError> {
    if schema_version == SCHEMA_VERSION {
        Ok(())
    } else {
        invalid(format!(
            "schema_version must be {SCHEMA_VERSION}, got {schema_version}"
        ))
    }
}

fn validate_permutation(name: &str, permutation: &[usize; 6]) -> Result<(), CalibrationError> {
    let mut seen = [false; 6];
    for (index, joint) in permutation.iter().copied().enumerate() {
        if joint >= seen.len() {
            return invalid(format!("{name}[{index}] must be in 0..6"));
        }
        if seen[joint] {
            return invalid(format!("{name} must contain each joint exactly once"));
        }
        seen[joint] = true;
    }
    Ok(())
}

fn validate_sign_array(name: &str, values: &[f64; 6]) -> Result<(), CalibrationError> {
    for (index, value) in values.iter().copied().enumerate() {
        validate_finite(&format!("{name}[{index}]"), value)?;
        if !is_unit_sign(value) {
            return invalid(format!("{name}[{index}] must be exactly -1.0 or 1.0"));
        }
    }
    Ok(())
}

fn validate_finite_array(name: &str, values: &[f64; 6]) -> Result<(), CalibrationError> {
    for (index, value) in values.iter().copied().enumerate() {
        validate_finite(&format!("{name}[{index}]"), value)?;
    }
    Ok(())
}

fn validate_finite(name: &str, value: f64) -> Result<(), CalibrationError> {
    if value.is_finite() {
        Ok(())
    } else {
        invalid(format!("{name} must be finite"))
    }
}

fn validate_positive(name: &str, value: f64) -> Result<(), CalibrationError> {
    validate_finite(name, value)?;
    if value > 0.0 {
        Ok(())
    } else {
        invalid(format!("{name} must be positive"))
    }
}

fn validate_posture_arm(
    arm: &'static str,
    current: &[f64; 6],
    zero: &[f64; 6],
    max_error_rad: f64,
) -> Result<(), CalibrationError> {
    for (joint_index, (current_rad, zero_rad)) in
        current.iter().copied().zip(zero.iter().copied()).enumerate()
    {
        validate_finite(&format!("{arm}[{joint_index}]"), current_rad)?;
        if (current_rad - zero_rad).abs() > max_error_rad {
            return Err(CalibrationError::PostureMismatch {
                arm,
                joint_index,
                current_rad,
                zero_rad,
                max_error_rad,
            });
        }
    }
    Ok(())
}

fn validate_runtime_map_matches_file(
    file: &MirrorMapFile,
    effective_map: JointMirrorMap,
) -> Result<(), CalibrationError> {
    let runtime_map = file.to_runtime_map()?;
    for index in 0..6 {
        let actual = runtime_map.permutation[index].index();
        let expected = effective_map.permutation[index].index();
        if actual != expected {
            return Err(CalibrationError::MirrorMapMismatch {
                field: "permutation",
                joint_index: index,
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
    }
    compare_signs(
        "position_sign",
        runtime_map.position_sign,
        effective_map.position_sign,
    )?;
    compare_signs(
        "velocity_sign",
        runtime_map.velocity_sign,
        effective_map.velocity_sign,
    )?;
    compare_signs(
        "torque_sign",
        runtime_map.torque_sign,
        effective_map.torque_sign,
    )
}

fn compare_signs(
    field: &'static str,
    actual_values: [f64; 6],
    expected_values: [f64; 6],
) -> Result<(), CalibrationError> {
    for (joint_index, (actual, expected)) in
        actual_values.iter().copied().zip(expected_values.iter().copied()).enumerate()
    {
        if actual.to_bits() != expected.to_bits() {
            return Err(CalibrationError::MirrorMapMismatch {
                field,
                joint_index,
                expected: format_f64(expected),
                actual: format_f64(actual),
            });
        }
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T, CalibrationError> {
    Err(CalibrationError::Invalid(message.into()))
}

fn is_unit_sign(value: f64) -> bool {
    matches!(
        value.to_bits(),
        bits if bits == 1.0f64.to_bits() || bits == (-1.0f64).to_bits()
    )
}

fn hex_nibble(value: u8) -> char {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    char::from(HEX[usize::from(value)])
}

fn push_mirror_map_body(out: &mut String, mirror_map: &MirrorMapFile) {
    push_usize_array_line(out, "permutation", &mirror_map.permutation);
    push_f64_array_line(out, "position_sign", &mirror_map.position_sign);
    push_f64_array_line(out, "velocity_sign", &mirror_map.velocity_sign);
    push_f64_array_line(out, "torque_sign", &mirror_map.torque_sign);
}

fn push_u32_line(out: &mut String, key: &str, value: u32) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(&value.to_string());
    out.push('\n');
}

fn push_u64_line(out: &mut String, key: &str, value: u64) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(&value.to_string());
    out.push('\n');
}

fn push_f64(out: &mut String, value: f64) {
    let mut buf = ryu::Buffer::new();
    out.push_str(buf.format_finite(value));
}

fn format_f64(value: f64) -> String {
    let mut buf = ryu::Buffer::new();
    buf.format_finite(value).to_string()
}

fn push_f64_array_line(out: &mut String, key: &str, values: &[f64; 6]) {
    out.push_str(key);
    out.push_str(" = [");
    for (index, value) in values.iter().copied().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        push_f64(out, value);
    }
    out.push_str("]\n");
}

fn push_usize_array_line(out: &mut String, key: &str, values: &[usize; 6]) {
    out.push_str(key);
    out.push_str(" = [");
    for (index, value) in values.iter().copied().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&value.to_string());
    }
    out.push_str("]\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_bytes_are_canonical_and_hashable() {
        let calibration = CalibrationFile::identity_for_tests();
        let bytes = calibration.to_canonical_toml_bytes().unwrap();
        assert_eq!(sha256_hex(&bytes), calibration.sha256_hex().unwrap());
        assert!(std::str::from_utf8(&bytes).unwrap().contains("[mirror_map]\n"));
    }

    #[test]
    fn loading_rejects_noncanonical_bytes() {
        let text = "schema_version=1\ncreated_unix_ms=0\n";
        assert!(CalibrationFile::from_canonical_bytes(text.as_bytes()).is_err());
    }

    #[test]
    fn save_calibration_refuses_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("calibration.toml");
        std::fs::write(&path, b"existing").unwrap();
        let calibration = CalibrationFile::identity_for_tests();
        assert!(
            persist_calibration_no_overwrite(
                &path,
                &calibration.to_canonical_toml_bytes().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn loaded_calibration_mirror_map_must_match_effective_profile() {
        let mut calibration = CalibrationFile::identity_for_tests();
        calibration.mirror_map.torque_sign[0] *= -1.0;
        let effective = JointMirrorMap::left_right_mirror();

        assert!(matches!(
            resolve_episode_calibration(Some(calibration), effective, None),
            Err(CalibrationError::MirrorMapMismatch { .. })
        ));
    }

    #[test]
    fn episode_calibration_bytes_match_manifest_hash() {
        let calibration = CalibrationFile::identity_for_tests();
        let runtime_map = calibration.mirror_map.to_runtime_map().unwrap();
        let resolved = resolve_episode_calibration(Some(calibration), runtime_map, None).unwrap();

        assert_eq!(sha256_hex(&resolved.canonical_bytes), resolved.sha256_hex);
        assert!(
            std::str::from_utf8(&resolved.canonical_bytes)
                .unwrap()
                .contains("schema_version = 1\n")
        );
    }

    #[test]
    fn file_backed_mirror_map_rejects_noncanonical_bytes() {
        let text = "schema_version=1\npermutation=[0,1,2,3,4,5]\n";

        assert!(MirrorMapFile::from_canonical_bytes(text.as_bytes()).is_err());
    }

    #[test]
    fn file_backed_mirror_map_hashes_exact_loaded_bytes() {
        let mirror = MirrorMapFile::left_right_for_tests();
        let bytes = mirror.to_canonical_toml_bytes().unwrap();
        let loaded = load_file_backed_mirror_map_bytes(&bytes).unwrap();

        assert_eq!(loaded.sha256_hex, sha256_hex(&bytes));
        assert_eq!(loaded.runtime_map, mirror.to_runtime_map().unwrap());
    }

    #[test]
    fn current_posture_must_match_calibration_zeroes() {
        let calibration = CalibrationFile::identity_for_tests();

        assert!(validate_current_posture(&calibration, [0.01; 6], [-0.01; 6], 0.05).is_ok());
        assert!(validate_current_posture(&calibration, [0.06; 6], [0.0; 6], 0.05).is_err());
    }

    #[test]
    fn current_posture_rejects_nonfinite_values() {
        let calibration = CalibrationFile::identity_for_tests();

        assert!(validate_current_posture(&calibration, [f64::NAN; 6], [0.0; 6], 0.05).is_err());
        assert!(validate_current_posture(&calibration, [0.0; 6], [0.0; 6], 0.0).is_err());
    }
}
