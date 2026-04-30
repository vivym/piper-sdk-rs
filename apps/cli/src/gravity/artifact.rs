#![allow(dead_code)]

use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const SAMPLE_ARTIFACT_KIND: &str = "quasi-static-samples";
const SAMPLE_ROW_TYPE: &str = "quasi-static-sample";
const HEADER_ROW_TYPE: &str = "header";

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

#[derive(Debug, Clone)]
pub struct LoadedSamples {
    pub header: SamplesHeader,
    pub rows: Vec<QuasiStaticSampleRow>,
}

#[derive(Debug, Deserialize)]
struct HeaderProbe {
    #[serde(rename = "type")]
    row_type: String,
    artifact_kind: String,
}

pub fn read_quasi_static_samples(paths: &[PathBuf]) -> Result<LoadedSamples> {
    if paths.is_empty() {
        bail!("expected at least one quasi-static-samples artifact path");
    }

    let mut loaded_header = None;
    let mut rows = Vec::new();

    for path in paths {
        let (header, mut file_rows) = read_quasi_static_samples_file(path)?;
        validate_header_matches(&loaded_header, &header, path)?;
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

fn read_quasi_static_samples_file(
    path: &Path,
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
    if header.artifact_kind != SAMPLE_ARTIFACT_KIND {
        bail!(
            "{} artifact_kind must be {SAMPLE_ARTIFACT_KIND:?}, got {:?}",
            path.display(),
            header.artifact_kind
        );
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

fn validate_header_matches(
    loaded_header: &Option<SamplesHeader>,
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
}
