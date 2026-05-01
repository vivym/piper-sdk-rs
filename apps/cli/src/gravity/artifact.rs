#![allow(dead_code)]

use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const SAMPLE_ARTIFACT_KIND: &str = "quasi-static-samples";
const PATH_ARTIFACT_KIND: &str = "path";
const SAMPLE_ROW_TYPE: &str = "quasi-static-sample";
const PATH_SAMPLE_ROW_TYPE: &str = "path-sample";
const HEADER_ROW_TYPE: &str = "header";
const SCHEMA_VERSION: u32 = 1;
const VALID_JOINT_MASK: u8 = 0x3f;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PassDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplesHeader {
    #[serde(rename = "type")]
    pub row_type: String,
    pub artifact_kind: String,
    pub schema_version: u32,
    pub source_path: String,
    pub source_sha256: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm_id: Option<String>,
    pub target: String,
    pub joint_map: String,
    pub load_profile: String,
    pub torque_convention: String,
    pub frequency_hz: f64,
    pub max_velocity_rad_s: f64,
    pub max_step_rad: f64,
    pub settle_ms: u64,
    pub sample_ms: u64,
    pub stable_velocity_rad_s: f64,
    pub stable_tracking_error_rad: f64,
    pub stable_torque_std_nm: f64,
    pub waypoint_count: usize,
    pub accepted_waypoint_count: usize,
    pub rejected_waypoint_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathHeader {
    #[serde(rename = "type")]
    pub row_type: String,
    pub artifact_kind: String,
    pub schema_version: u32,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm_id: Option<String>,
    pub target: String,
    pub joint_map: String,
    pub load_profile: String,
    pub torque_convention: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuasiStaticSampleRow {
    #[serde(rename = "type")]
    pub row_type: String,
    pub waypoint_id: u64,
    pub segment_id: Option<String>,
    pub pass_direction: PassDirection,
    pub host_mono_us: u64,
    pub raw_timestamp_us: Option<u64>,
    pub q_rad: [f64; 6],
    pub dq_rad_s: [f64; 6],
    pub tau_nm: [f64; 6],
    pub position_valid_mask: u8,
    pub dynamic_valid_mask: u8,
    pub stable_velocity_rad_s: f64,
    pub stable_tracking_error_rad: f64,
    pub stable_torque_std_nm: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathSampleRow {
    #[serde(rename = "type")]
    pub row_type: String,
    pub sample_index: u64,
    pub host_mono_us: u64,
    pub raw_timestamp_us: Option<u64>,
    pub q_rad: [f64; 6],
    pub dq_rad_s: [f64; 6],
    pub tau_nm: [f64; 6],
    pub position_valid_mask: u8,
    pub dynamic_valid_mask: u8,
    pub segment_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LoadedSamples {
    pub header: SamplesHeader,
    pub rows: Vec<QuasiStaticSampleRow>,
}

#[derive(Debug, Clone)]
pub struct LoadedPath {
    pub header: PathHeader,
    pub rows: Vec<PathSampleRow>,
}

#[derive(Debug, Deserialize)]
struct HeaderProbe {
    #[serde(rename = "type")]
    row_type: String,
    artifact_kind: String,
}

#[derive(Debug, Deserialize)]
struct RowTypeProbe {
    #[serde(rename = "type")]
    row_type: String,
}

impl PathHeader {
    pub fn new(
        role: impl Into<String>,
        target: impl Into<String>,
        joint_map: impl Into<String>,
        load_profile: impl Into<String>,
        notes: Option<String>,
    ) -> Self {
        Self {
            row_type: HEADER_ROW_TYPE.to_string(),
            artifact_kind: PATH_ARTIFACT_KIND.to_string(),
            schema_version: SCHEMA_VERSION,
            role: role.into(),
            arm_id: None,
            target: target.into(),
            joint_map: joint_map.into(),
            load_profile: load_profile.into(),
            torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
            notes,
        }
    }
}

pub fn write_jsonl_row<W, T>(writer: &mut W, row: &T) -> Result<()>
where
    W: Write,
    T: Serialize,
{
    serde_json::to_writer(&mut *writer, row)?;
    writer.write_all(b"\n")?;
    Ok(())
}

pub fn read_quasi_static_samples(paths: &[PathBuf]) -> Result<LoadedSamples> {
    if paths.is_empty() {
        bail!("expected at least one quasi-static-samples artifact path");
    }

    let mut loaded_header = None;
    let mut rows = Vec::new();

    for path in paths {
        let (header, mut file_rows) = read_quasi_static_samples_file(path, loaded_header.as_ref())?;
        if loaded_header.is_none() {
            loaded_header = Some(header);
        }
        rows.append(&mut file_rows);
    }

    Ok(LoadedSamples {
        header: loaded_header.expect("paths is non-empty"),
        rows,
    })
}

pub fn read_path(path: &Path) -> Result<LoadedPath> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    let header_line = match lines.next() {
        Some(line) => {
            line.with_context(|| format!("failed to read header from {}", path.display()))?
        },
        None => bail!("{} is empty; expected path header", path.display()),
    };

    let probe: HeaderProbe = serde_json::from_str(&header_line)
        .with_context(|| format!("failed to parse header in {}", path.display()))?;
    if probe.row_type != HEADER_ROW_TYPE {
        bail!(
            "{} first row type must be {HEADER_ROW_TYPE:?}, got {:?}",
            path.display(),
            probe.row_type
        );
    }
    if probe.artifact_kind != PATH_ARTIFACT_KIND {
        bail!(
            "{} artifact_kind must be {PATH_ARTIFACT_KIND:?}, got {:?}",
            path.display(),
            probe.artifact_kind
        );
    }

    let header: PathHeader = serde_json::from_str(&header_line)
        .with_context(|| format!("failed to parse path header in {}", path.display()))?;
    validate_path_header(&header, path)?;

    let mut rows = Vec::new();
    for (index, line) in lines.enumerate() {
        let line_number = index + 2;
        let line = line.with_context(|| {
            format!(
                "failed to read path-sample row {line_number} from {}",
                path.display()
            )
        })?;
        let row_probe: RowTypeProbe = serde_json::from_str(&line).with_context(|| {
            format!(
                "failed to parse path-sample row {line_number} type in {}",
                path.display()
            )
        })?;
        if row_probe.row_type != PATH_SAMPLE_ROW_TYPE {
            bail!(
                "{} row {line_number} type must be {PATH_SAMPLE_ROW_TYPE:?}, got {:?}",
                path.display(),
                row_probe.row_type
            );
        }

        let row: PathSampleRow = serde_json::from_str(&line).with_context(|| {
            format!(
                "failed to parse path-sample row {line_number} in {}; arrays must be finite JSON numbers",
                path.display()
            )
        })?;
        validate_path_row(&row, path, line_number)?;
        rows.push(row);
    }

    if rows.is_empty() {
        bail!("{} contains no path-sample rows", path.display());
    }

    Ok(LoadedPath { header, rows })
}

fn read_quasi_static_samples_file(
    path: &Path,
    loaded_header: Option<&SamplesHeader>,
) -> Result<(SamplesHeader, Vec<QuasiStaticSampleRow>)> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    let header_line = match lines.next() {
        Some(line) => {
            line.with_context(|| format!("failed to read header from {}", path.display()))?
        },
        None => bail!(
            "{} is empty; expected quasi-static-samples header",
            path.display()
        ),
    };

    let probe: HeaderProbe = serde_json::from_str(&header_line)
        .with_context(|| format!("failed to parse header in {}", path.display()))?;
    if probe.row_type != HEADER_ROW_TYPE {
        bail!(
            "{} first row type must be {HEADER_ROW_TYPE:?}, got {:?}",
            path.display(),
            probe.row_type
        );
    }
    if probe.artifact_kind != SAMPLE_ARTIFACT_KIND {
        bail!(
            "{} artifact_kind must be {SAMPLE_ARTIFACT_KIND:?}, got {:?}",
            path.display(),
            probe.artifact_kind
        );
    }

    let header: SamplesHeader = serde_json::from_str(&header_line).with_context(|| {
        format!(
            "failed to parse quasi-static-samples header in {}",
            path.display()
        )
    })?;
    validate_samples_header(&header, path)?;
    validate_header_matches(loaded_header, &header, path)?;

    let mut rows = Vec::new();
    for (index, line) in lines.enumerate() {
        let line_number = index + 2;
        let line = line.with_context(|| {
            format!(
                "failed to read quasi-static-samples row {line_number} from {}",
                path.display()
            )
        })?;
        let row: QuasiStaticSampleRow = serde_json::from_str(&line).with_context(|| {
            format!(
                "failed to parse quasi-static-samples row {line_number} in {}",
                path.display()
            )
        })?;
        validate_sample_row(&row, path, line_number)?;
        rows.push(row);
    }

    Ok((header, rows))
}

fn validate_samples_header(header: &SamplesHeader, path: &Path) -> Result<()> {
    if header.row_type != HEADER_ROW_TYPE {
        bail!(
            "{} header type must be {HEADER_ROW_TYPE:?}, got {:?}",
            path.display(),
            header.row_type
        );
    }
    if header.artifact_kind != SAMPLE_ARTIFACT_KIND {
        bail!(
            "{} artifact_kind must be {SAMPLE_ARTIFACT_KIND:?}, got {:?}",
            path.display(),
            header.artifact_kind
        );
    }
    if header.schema_version != SCHEMA_VERSION {
        bail!(
            "{} schema_version must be {SCHEMA_VERSION}, got {}",
            path.display(),
            header.schema_version
        );
    }
    if header.source_path.trim().is_empty() {
        bail!("{} source_path must not be empty", path.display());
    }
    if header.source_sha256.trim().is_empty() {
        bail!("{} source_sha256 must not be empty", path.display());
    }
    if header.role.trim().is_empty() {
        bail!("{} role must not be empty", path.display());
    }
    validate_optional_non_empty_header_field(path, "arm_id", header.arm_id.as_deref())?;
    if header.target.trim().is_empty() {
        bail!("{} target must not be empty", path.display());
    }
    if header.joint_map.trim().is_empty() {
        bail!("{} joint_map must not be empty", path.display());
    }
    if header.load_profile.trim().is_empty() {
        bail!("{} load_profile must not be empty", path.display());
    }
    if header.torque_convention != crate::gravity::TORQUE_CONVENTION {
        bail!(
            "{} torque_convention must be {:?}, got {:?}",
            path.display(),
            crate::gravity::TORQUE_CONVENTION,
            header.torque_convention
        );
    }
    validate_finite_header_field(path, "frequency_hz", header.frequency_hz)?;
    validate_finite_header_field(path, "max_velocity_rad_s", header.max_velocity_rad_s)?;
    validate_finite_header_field(path, "max_step_rad", header.max_step_rad)?;
    validate_finite_header_field(path, "stable_velocity_rad_s", header.stable_velocity_rad_s)?;
    validate_finite_header_field(
        path,
        "stable_tracking_error_rad",
        header.stable_tracking_error_rad,
    )?;
    validate_finite_header_field(path, "stable_torque_std_nm", header.stable_torque_std_nm)?;
    let total_outcomes = header
        .accepted_waypoint_count
        .checked_add(header.rejected_waypoint_count)
        .with_context(|| format!("{} waypoint outcome counts overflow", path.display()))?;
    if total_outcomes != header.waypoint_count {
        bail!(
            "{} accepted_waypoint_count + rejected_waypoint_count must equal waypoint_count ({} + {} != {})",
            path.display(),
            header.accepted_waypoint_count,
            header.rejected_waypoint_count,
            header.waypoint_count
        );
    }
    Ok(())
}

fn validate_finite_header_field(path: &Path, name: &str, value: f64) -> Result<()> {
    if !value.is_finite() {
        bail!("{} {name} must be finite", path.display());
    }
    Ok(())
}

fn validate_path_header(header: &PathHeader, path: &Path) -> Result<()> {
    if header.row_type != HEADER_ROW_TYPE {
        bail!(
            "{} header type must be {HEADER_ROW_TYPE:?}, got {:?}",
            path.display(),
            header.row_type
        );
    }
    if header.artifact_kind != PATH_ARTIFACT_KIND {
        bail!(
            "{} artifact_kind must be {PATH_ARTIFACT_KIND:?}, got {:?}",
            path.display(),
            header.artifact_kind
        );
    }
    if header.schema_version != SCHEMA_VERSION {
        bail!(
            "{} schema_version must be {SCHEMA_VERSION}, got {}",
            path.display(),
            header.schema_version
        );
    }
    if header.role.trim().is_empty() {
        bail!("{} role must not be empty", path.display());
    }
    validate_optional_non_empty_header_field(path, "arm_id", header.arm_id.as_deref())?;
    if header.target.trim().is_empty() {
        bail!("{} target must not be empty", path.display());
    }
    if header.joint_map.trim().is_empty() {
        bail!("{} joint_map must not be empty", path.display());
    }
    if header.load_profile.trim().is_empty() {
        bail!("{} load_profile must not be empty", path.display());
    }
    if header.torque_convention != crate::gravity::TORQUE_CONVENTION {
        bail!(
            "{} torque_convention must be {:?}, got {:?}",
            path.display(),
            crate::gravity::TORQUE_CONVENTION,
            header.torque_convention
        );
    }
    Ok(())
}

fn validate_optional_non_empty_header_field(
    path: &Path,
    name: &str,
    value: Option<&str>,
) -> Result<()> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        bail!("{} {name} must not be empty when present", path.display());
    }
    Ok(())
}

fn validate_header_matches(
    loaded_header: Option<&SamplesHeader>,
    header: &SamplesHeader,
    path: &Path,
) -> Result<()> {
    let Some(first_header) = loaded_header else {
        return Ok(());
    };

    if first_header.role != header.role {
        bail!(
            "{} role {:?} does not match first artifact role {:?}",
            path.display(),
            header.role,
            first_header.role
        );
    }
    if first_header.arm_id != header.arm_id {
        bail!(
            "{} arm_id {:?} does not match first artifact arm_id {:?}",
            path.display(),
            header.arm_id,
            first_header.arm_id
        );
    }
    if first_header.joint_map != header.joint_map {
        bail!(
            "{} joint_map {:?} does not match first artifact joint_map {:?}",
            path.display(),
            header.joint_map,
            first_header.joint_map
        );
    }
    if first_header.load_profile != header.load_profile {
        bail!(
            "{} load_profile {:?} does not match first artifact load_profile {:?}",
            path.display(),
            header.load_profile,
            first_header.load_profile
        );
    }
    if first_header.torque_convention != header.torque_convention {
        bail!(
            "{} torque_convention {:?} does not match first artifact torque_convention {:?}",
            path.display(),
            header.torque_convention,
            first_header.torque_convention
        );
    }

    Ok(())
}

fn validate_sample_row(row: &QuasiStaticSampleRow, path: &Path, line_number: usize) -> Result<()> {
    if row.row_type != SAMPLE_ROW_TYPE {
        bail!(
            "{} row {line_number} type must be {SAMPLE_ROW_TYPE:?}, got {:?}",
            path.display(),
            row.row_type
        );
    }
    if row.q_rad.iter().any(|value| !value.is_finite()) {
        bail!("{} row {line_number} q_rad must be finite", path.display());
    }
    if row.dq_rad_s.iter().any(|value| !value.is_finite()) {
        bail!(
            "{} row {line_number} dq_rad_s must be finite",
            path.display()
        );
    }
    if row.tau_nm.iter().any(|value| !value.is_finite()) {
        bail!("{} row {line_number} tau_nm must be finite", path.display());
    }
    validate_joint_mask(
        "position_valid_mask",
        row.position_valid_mask,
        path,
        line_number,
    )?;
    if row.position_valid_mask != VALID_JOINT_MASK {
        bail!(
            "{} row {line_number} position_valid_mask must be {VALID_JOINT_MASK:#04x}, got {:#04x}",
            path.display(),
            row.position_valid_mask
        );
    }
    validate_joint_mask(
        "dynamic_valid_mask",
        row.dynamic_valid_mask,
        path,
        line_number,
    )?;
    if row.dynamic_valid_mask != VALID_JOINT_MASK {
        bail!(
            "{} row {line_number} dynamic_valid_mask must be {VALID_JOINT_MASK:#04x}, got {:#04x}",
            path.display(),
            row.dynamic_valid_mask
        );
    }
    if !row.stable_velocity_rad_s.is_finite() {
        bail!(
            "{} row {line_number} stable_velocity_rad_s must be finite",
            path.display()
        );
    }
    if !row.stable_tracking_error_rad.is_finite() {
        bail!(
            "{} row {line_number} stable_tracking_error_rad must be finite",
            path.display()
        );
    }
    if !row.stable_torque_std_nm.is_finite() {
        bail!(
            "{} row {line_number} stable_torque_std_nm must be finite",
            path.display()
        );
    }
    Ok(())
}

fn validate_path_row(row: &PathSampleRow, path: &Path, line_number: usize) -> Result<()> {
    if row.row_type != PATH_SAMPLE_ROW_TYPE {
        bail!(
            "{} row {line_number} type must be {PATH_SAMPLE_ROW_TYPE:?}, got {:?}",
            path.display(),
            row.row_type
        );
    }
    if row.q_rad.iter().any(|value| !value.is_finite()) {
        bail!("{} row {line_number} q_rad must be finite", path.display());
    }
    if row.dq_rad_s.iter().any(|value| !value.is_finite()) {
        bail!(
            "{} row {line_number} dq_rad_s must be finite",
            path.display()
        );
    }
    if row.tau_nm.iter().any(|value| !value.is_finite()) {
        bail!("{} row {line_number} tau_nm must be finite", path.display());
    }
    validate_joint_mask(
        "position_valid_mask",
        row.position_valid_mask,
        path,
        line_number,
    )?;
    if row.position_valid_mask != VALID_JOINT_MASK {
        bail!(
            "{} row {line_number} position_valid_mask must be {VALID_JOINT_MASK:#04x}, got {:#04x}",
            path.display(),
            row.position_valid_mask
        );
    }
    validate_joint_mask(
        "dynamic_valid_mask",
        row.dynamic_valid_mask,
        path,
        line_number,
    )?;
    Ok(())
}

fn validate_joint_mask(name: &str, mask: u8, path: &Path, line_number: usize) -> Result<()> {
    if (mask & !VALID_JOINT_MASK) != 0 {
        bail!(
            "{} row {line_number} {name} has bits outside {VALID_JOINT_MASK:#04x}: {mask:#04x}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn path_header_jsonl_row_uses_path_schema_and_notes() {
        let header = PathHeader {
            row_type: "header".to_string(),
            artifact_kind: "path".to_string(),
            schema_version: 1,
            role: "slave".to_string(),
            arm_id: None,
            target: "socketcan:can0".to_string(),
            joint_map: "identity".to_string(),
            load_profile: "normal-gripper-d405".to_string(),
            torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
            notes: Some("operator note".to_string()),
        };

        let mut encoded = Vec::new();
        write_jsonl_row(&mut encoded, &header).unwrap();

        let line = String::from_utf8(encoded).unwrap();
        assert!(line.ends_with('\n'));

        let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["type"], "header");
        assert_eq!(value["artifact_kind"], "path");
        assert_eq!(
            value["torque_convention"],
            crate::gravity::TORQUE_CONVENTION
        );
        assert_eq!(value["notes"], "operator note");
        assert!(value.get("arm_id").is_none());
    }

    #[test]
    fn path_sample_jsonl_row_preserves_state_vectors_and_masks() {
        let row = PathSampleRow {
            row_type: "path-sample".to_string(),
            sample_index: 7,
            host_mono_us: 123_456,
            raw_timestamp_us: Some(654_321),
            q_rad: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            dq_rad_s: [1.1, 1.2, 1.3, 1.4, 1.5, 1.6],
            tau_nm: [2.1, 2.2, 2.3, 2.4, 2.5, 2.6],
            position_valid_mask: 0b0000_0111,
            dynamic_valid_mask: 0b0011_1111,
            segment_id: None,
        };

        let mut encoded = Vec::new();
        write_jsonl_row(&mut encoded, &row).unwrap();

        let value: serde_json::Value =
            serde_json::from_slice(&encoded[..encoded.len() - 1]).unwrap();
        assert_eq!(value["type"], "path-sample");
        assert_eq!(value["sample_index"], 7);
        assert_eq!(value["q_rad"].as_array().unwrap().len(), 6);
        assert_eq!(value["dynamic_valid_mask"], 0b0011_1111);
    }

    #[test]
    fn path_reader_loads_header_and_rows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("path.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"path\",\"schema_version\":1,\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\"}\n",
                "{\"type\":\"path-sample\",\"sample_index\":0,\"host_mono_us\":1,\"raw_timestamp_us\":2,\"q_rad\":[0,0.1,0.2,0.3,0.4,0.5],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":63,\"dynamic_valid_mask\":63,\"segment_id\":\"seg-a\"}\n"
            ),
        )
        .unwrap();

        let loaded = read_path(&path).unwrap();

        assert_eq!(loaded.header.role, "slave");
        assert_eq!(loaded.header.target, "socketcan:can0");
        assert_eq!(loaded.rows.len(), 1);
        assert_eq!(loaded.rows[0].sample_index, 0);
        assert_eq!(loaded.rows[0].segment_id.as_deref(), Some("seg-a"));
    }

    #[test]
    fn path_reader_rejects_schema_and_torque_convention_mismatch() {
        let dir = tempdir().unwrap();
        let schema_path = dir.path().join("schema.jsonl");
        std::fs::write(
            &schema_path,
            "{\"type\":\"header\",\"artifact_kind\":\"path\",\"schema_version\":2,\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\"}\n",
        )
        .unwrap();

        let schema_err = read_path(&schema_path).unwrap_err();
        assert!(schema_err.to_string().contains("schema_version"));

        let torque_path = dir.path().join("torque.jsonl");
        std::fs::write(
            &torque_path,
            "{\"type\":\"header\",\"artifact_kind\":\"path\",\"schema_version\":1,\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"legacy\"}\n",
        )
        .unwrap();

        let torque_err = read_path(&torque_path).unwrap_err();
        assert!(torque_err.to_string().contains("torque_convention"));
    }

    #[test]
    fn path_reader_rejects_bad_row_type_nonfinite_arrays_and_invalid_masks() {
        let dir = tempdir().unwrap();
        let bad_type_path = dir.path().join("bad-type.jsonl");
        std::fs::write(
            &bad_type_path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"path\",\"schema_version\":1,\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\"}\n",
                "{\"type\":\"quasi-static-sample\",\"sample_index\":0,\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,0],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":63,\"dynamic_valid_mask\":63}\n"
            ),
        )
        .unwrap();
        assert!(read_path(&bad_type_path).unwrap_err().to_string().contains("path-sample"));

        let nonfinite_path = dir.path().join("nonfinite.jsonl");
        std::fs::write(
            &nonfinite_path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"path\",\"schema_version\":1,\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\"}\n",
                "{\"type\":\"path-sample\",\"sample_index\":0,\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,1e999],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":63,\"dynamic_valid_mask\":63}\n"
            ),
        )
        .unwrap();
        assert!(read_path(&nonfinite_path).unwrap_err().to_string().contains("finite"));

        let mask_path = dir.path().join("mask.jsonl");
        std::fs::write(
            &mask_path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"path\",\"schema_version\":1,\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\"}\n",
                "{\"type\":\"path-sample\",\"sample_index\":0,\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,0],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":64,\"dynamic_valid_mask\":63}\n"
            ),
        )
        .unwrap();
        assert!(read_path(&mask_path).unwrap_err().to_string().contains("mask"));
    }

    #[test]
    fn path_reader_rejects_incomplete_position_masks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("incomplete-position-mask.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"path\",\"schema_version\":1,\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\"}\n",
                "{\"type\":\"path-sample\",\"sample_index\":0,\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,0],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":31,\"dynamic_valid_mask\":0}\n"
            ),
        )
        .unwrap();

        let err = read_path(&path).unwrap_err();

        assert!(err.to_string().contains("position_valid_mask"), "{err:#}");
    }

    #[test]
    fn fit_reader_rejects_manual_path_artifact() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("path.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"header","artifact_kind":"path","schema_version":1,"role":"slave","target":"socketcan:can0","joint_map":"identity","load_profile":"load","torque_convention":"piper-sdk-normalized-nm-v1"}"#,
        )
        .unwrap();

        let err = read_quasi_static_samples(&[path]).unwrap_err();
        assert!(err.to_string().contains("quasi-static-samples"));
    }

    #[test]
    fn sample_reader_rejects_unsupported_schema_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("unsupported-schema.samples.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"quasi-static-samples\",\"schema_version\":2,\"source_path\":\"p.jsonl\",\"source_sha256\":\"abc\",\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\",\"frequency_hz\":100.0,\"max_velocity_rad_s\":0.08,\"max_step_rad\":0.02,\"settle_ms\":500,\"sample_ms\":300,\"stable_velocity_rad_s\":0.01,\"stable_tracking_error_rad\":0.03,\"stable_torque_std_nm\":0.08,\"waypoint_count\":1,\"accepted_waypoint_count\":1,\"rejected_waypoint_count\":0}\n",
                "{\"type\":\"quasi-static-sample\",\"waypoint_id\":7,\"segment_id\":\"seg-a\",\"pass_direction\":\"forward\",\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,0],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":63,\"dynamic_valid_mask\":63,\"stable_velocity_rad_s\":0.0,\"stable_tracking_error_rad\":0.0,\"stable_torque_std_nm\":0.0}\n"
            ),
        )
        .unwrap();

        let err = read_quasi_static_samples(&[path]).unwrap_err();

        assert!(err.to_string().contains("schema_version"), "{err:#}");
    }

    #[test]
    fn sample_reader_rejects_incomplete_sample_masks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("incomplete-mask.samples.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"quasi-static-samples\",\"schema_version\":1,\"source_path\":\"p.jsonl\",\"source_sha256\":\"abc\",\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\",\"frequency_hz\":100.0,\"max_velocity_rad_s\":0.08,\"max_step_rad\":0.02,\"settle_ms\":500,\"sample_ms\":300,\"stable_velocity_rad_s\":0.01,\"stable_tracking_error_rad\":0.03,\"stable_torque_std_nm\":0.08,\"waypoint_count\":1,\"accepted_waypoint_count\":1,\"rejected_waypoint_count\":0}\n",
                "{\"type\":\"quasi-static-sample\",\"waypoint_id\":7,\"segment_id\":\"seg-a\",\"pass_direction\":\"forward\",\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,0],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":31,\"dynamic_valid_mask\":63,\"stable_velocity_rad_s\":0.0,\"stable_tracking_error_rad\":0.0,\"stable_torque_std_nm\":0.0}\n"
            ),
        )
        .unwrap();

        let err = read_quasi_static_samples(&[path]).unwrap_err();

        assert!(err.to_string().contains("position_valid_mask"), "{err:#}");
    }

    #[test]
    fn sample_reader_preserves_waypoint_segment_and_direction() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("samples.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"quasi-static-samples\",\"schema_version\":1,\"source_path\":\"p.jsonl\",\"source_sha256\":\"abc\",\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\",\"frequency_hz\":100.0,\"max_velocity_rad_s\":0.08,\"max_step_rad\":0.02,\"settle_ms\":500,\"sample_ms\":300,\"stable_velocity_rad_s\":0.01,\"stable_tracking_error_rad\":0.03,\"stable_torque_std_nm\":0.08,\"waypoint_count\":1,\"accepted_waypoint_count\":1,\"rejected_waypoint_count\":0}\n",
                "{\"type\":\"quasi-static-sample\",\"waypoint_id\":7,\"segment_id\":\"seg-a\",\"pass_direction\":\"forward\",\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,0],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":63,\"dynamic_valid_mask\":63,\"stable_velocity_rad_s\":0.0,\"stable_tracking_error_rad\":0.0,\"stable_torque_std_nm\":0.0}\n"
            ),
        )
        .unwrap();

        let loaded = read_quasi_static_samples(&[path]).unwrap();
        assert_eq!(loaded.rows[0].waypoint_id, 7);
        assert_eq!(loaded.rows[0].segment_id.as_deref(), Some("seg-a"));
        assert_eq!(loaded.rows[0].pass_direction, PassDirection::Forward);
    }

    #[test]
    fn sample_reader_accepts_legacy_missing_arm_id_and_preserves_present_arm_id() {
        let dir = tempdir().unwrap();
        let legacy_path = dir.path().join("legacy.samples.jsonl");
        std::fs::write(
            &legacy_path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"quasi-static-samples\",\"schema_version\":1,\"source_path\":\"p.jsonl\",\"source_sha256\":\"abc\",\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\",\"frequency_hz\":100.0,\"max_velocity_rad_s\":0.08,\"max_step_rad\":0.02,\"settle_ms\":500,\"sample_ms\":300,\"stable_velocity_rad_s\":0.01,\"stable_tracking_error_rad\":0.03,\"stable_torque_std_nm\":0.08,\"waypoint_count\":1,\"accepted_waypoint_count\":1,\"rejected_waypoint_count\":0}\n",
                "{\"type\":\"quasi-static-sample\",\"waypoint_id\":7,\"segment_id\":\"seg-a\",\"pass_direction\":\"forward\",\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,0],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":63,\"dynamic_valid_mask\":63,\"stable_velocity_rad_s\":0.0,\"stable_tracking_error_rad\":0.0,\"stable_torque_std_nm\":0.0}\n"
            ),
        )
        .unwrap();
        let arm_path = dir.path().join("arm.samples.jsonl");
        std::fs::write(
            &arm_path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"quasi-static-samples\",\"schema_version\":1,\"source_path\":\"p.jsonl\",\"source_sha256\":\"abc\",\"role\":\"slave\",\"arm_id\":\"piper-left\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\",\"frequency_hz\":100.0,\"max_velocity_rad_s\":0.08,\"max_step_rad\":0.02,\"settle_ms\":500,\"sample_ms\":300,\"stable_velocity_rad_s\":0.01,\"stable_tracking_error_rad\":0.03,\"stable_torque_std_nm\":0.08,\"waypoint_count\":1,\"accepted_waypoint_count\":1,\"rejected_waypoint_count\":0}\n",
                "{\"type\":\"quasi-static-sample\",\"waypoint_id\":7,\"segment_id\":\"seg-a\",\"pass_direction\":\"forward\",\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,0],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":63,\"dynamic_valid_mask\":63,\"stable_velocity_rad_s\":0.0,\"stable_tracking_error_rad\":0.0,\"stable_torque_std_nm\":0.0}\n"
            ),
        )
        .unwrap();

        let legacy = read_quasi_static_samples(&[legacy_path]).unwrap();
        let with_arm = read_quasi_static_samples(&[arm_path]).unwrap();

        assert_eq!(legacy.header.arm_id, None);
        assert_eq!(with_arm.header.arm_id.as_deref(), Some("piper-left"));
    }

    #[test]
    fn multi_file_arm_id_mismatch_is_reported_before_row_errors() {
        let dir = tempdir().unwrap();
        let first_path = dir.path().join("first.jsonl");
        let second_path = dir.path().join("second.jsonl");
        std::fs::write(
            &first_path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"quasi-static-samples\",\"schema_version\":1,\"source_path\":\"p.jsonl\",\"source_sha256\":\"abc\",\"role\":\"slave\",\"arm_id\":\"piper-left\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\",\"frequency_hz\":100.0,\"max_velocity_rad_s\":0.08,\"max_step_rad\":0.02,\"settle_ms\":500,\"sample_ms\":300,\"stable_velocity_rad_s\":0.01,\"stable_tracking_error_rad\":0.03,\"stable_torque_std_nm\":0.08,\"waypoint_count\":1,\"accepted_waypoint_count\":1,\"rejected_waypoint_count\":0}\n",
                "{\"type\":\"quasi-static-sample\",\"waypoint_id\":1,\"segment_id\":\"seg-a\",\"pass_direction\":\"forward\",\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,0],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":63,\"dynamic_valid_mask\":63,\"stable_velocity_rad_s\":0.0,\"stable_tracking_error_rad\":0.0,\"stable_torque_std_nm\":0.0}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &second_path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"quasi-static-samples\",\"schema_version\":1,\"source_path\":\"p.jsonl\",\"source_sha256\":\"abc\",\"role\":\"slave\",\"arm_id\":\"piper-right\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\",\"frequency_hz\":100.0,\"max_velocity_rad_s\":0.08,\"max_step_rad\":0.02,\"settle_ms\":500,\"sample_ms\":300,\"stable_velocity_rad_s\":0.01,\"stable_tracking_error_rad\":0.03,\"stable_torque_std_nm\":0.08,\"waypoint_count\":1,\"accepted_waypoint_count\":1,\"rejected_waypoint_count\":0}\n",
                "{\"type\":\"quasi-static-sample\"\n"
            ),
        )
        .unwrap();

        let err = read_quasi_static_samples(&[first_path, second_path]).unwrap_err();

        assert!(err.to_string().contains("arm_id"), "{err:#}");
    }

    #[test]
    fn multi_file_role_mismatch_is_reported_before_row_errors() {
        let dir = tempdir().unwrap();
        let first_path = dir.path().join("first.jsonl");
        let second_path = dir.path().join("second.jsonl");
        std::fs::write(
            &first_path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"quasi-static-samples\",\"schema_version\":1,\"source_path\":\"p.jsonl\",\"source_sha256\":\"abc\",\"role\":\"slave\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\",\"frequency_hz\":100.0,\"max_velocity_rad_s\":0.08,\"max_step_rad\":0.02,\"settle_ms\":500,\"sample_ms\":300,\"stable_velocity_rad_s\":0.01,\"stable_tracking_error_rad\":0.03,\"stable_torque_std_nm\":0.08,\"waypoint_count\":1,\"accepted_waypoint_count\":1,\"rejected_waypoint_count\":0}\n",
                "{\"type\":\"quasi-static-sample\",\"waypoint_id\":1,\"segment_id\":\"seg-a\",\"pass_direction\":\"forward\",\"host_mono_us\":1,\"q_rad\":[0,0,0,0,0,0],\"dq_rad_s\":[0,0,0,0,0,0],\"tau_nm\":[1,2,3,4,5,6],\"position_valid_mask\":63,\"dynamic_valid_mask\":63,\"stable_velocity_rad_s\":0.0,\"stable_tracking_error_rad\":0.0,\"stable_torque_std_nm\":0.0}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &second_path,
            concat!(
                "{\"type\":\"header\",\"artifact_kind\":\"quasi-static-samples\",\"schema_version\":1,\"source_path\":\"p.jsonl\",\"source_sha256\":\"abc\",\"role\":\"master\",\"target\":\"socketcan:can0\",\"joint_map\":\"identity\",\"load_profile\":\"load\",\"torque_convention\":\"piper-sdk-normalized-nm-v1\",\"frequency_hz\":100.0,\"max_velocity_rad_s\":0.08,\"max_step_rad\":0.02,\"settle_ms\":500,\"sample_ms\":300,\"stable_velocity_rad_s\":0.01,\"stable_tracking_error_rad\":0.03,\"stable_torque_std_nm\":0.08,\"waypoint_count\":1,\"accepted_waypoint_count\":1,\"rejected_waypoint_count\":0}\n",
                "{\"type\":\"quasi-static-sample\"\n"
            ),
        )
        .unwrap();

        let err = read_quasi_static_samples(&[first_path, second_path]).unwrap_err();

        assert!(err.to_string().contains("role"), "{err:#}");
    }
}
